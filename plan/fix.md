# Stock IPO Reminder 代码审查报告(修复计划)

- 审查日期:2026-08-28(v4,经四轮独立交叉复核修订)
- 审查对象:main 分支 @ 58c8df7,版本 0.3.1(约 19,300 行 Rust + Slint UI;依赖以 Cargo.lock 锁定)
- 审查方式:模块化并行深度审查 → 主审逐条复核(第 1 轮)→ 独立交叉审查修正(第 2 轮)→ 第三方交叉审查(第 3 轮,ChatGPT)→ 修复方案可实施性复核(第 4 轮)。v4 关键变化:**M5 再修正**(逐次 read 超时不构成整段响应体或退出总时限)、M8 改为缺失 total 时继续探测分页、M11 拆分「结果文件原子性」与「上传后状态落盘的不确定窗口」、M13 明确状态迁移、M14 收紧清理边界,L5/L6/L11 修法细化,并补充逐项回归验收矩阵。

## 结论总览

| 级别 | 编号 | 摘要 |
|---|---|---|
| 中(6) | M2, M3, M4, M8, M9, M13 | 提醒重试无退避、DEFERRED 读后写升级、UI 线程同步读写、公告截断静默/警告丢失、runtime 查询 churn、Postponed 仍规划提醒 |
| 低(19) | M5, M6, M7, M10, M11, M12, M14, L1, L2, L4–L12, L14 | 体验、防御性、数据模型限制、日志与容错 |
| 维持现状(2) | L3, L13 | 属有意设计(at-least-once / 保守配额),仅可做诊断改进 |
| 推翻(4) | 推翻 1–4 | 初判为 bug,复核后确认不是(含两轮各一条高优先级误报) |

整体评价:代码质量高于同类桌面应用平均水平。SQL 全参数化、URL 白名单 + 体积上限、更新链四重校验、runtime 线程带退避自恢复、崩溃上报默认关闭且脱敏限流。未发现内存安全问题、SQL 注入、数据竞争死锁或线程静默死亡。

---

## 中优先级

### M2. 提醒窗口显示失败时,outbox 进入无退避的 2 分钟循环重试【已核实】

- 位置:`src/main.rs:492-529`(`if shown { ... }` 无 else 分支)、`src/storage.rs:541`(过期租约每轮重置回 Pending)、`src/storage.rs:568`(claim 无 attempt_count 上限)。
- 事实边界:显示失败本身有 ERROR 日志(main.rs:769),声音/闪烁/Toast 先于 `if shown` 执行(main.rs:478-491),用户并非无感知。
- 核心问题:`shown == false` 时既不 `complete_delivery` 也不 `fail_delivery`,无 `last_error`、无退避(退避只在 `fail_delivery` 生效)、claim 无次数上限——以约 2 分钟一次无限重试。
- 修复:补 else 分支调用 `fail_delivery`(带失败原因),让退避与 `last_error` 生效。小。

### M3. 写事务 DEFERRED:「事务内先读、后写」路径存在 SQLITE_BUSY 升级失败【已核实,范围修正】

- 位置:`src/storage.rs` 共 15 处 `.transaction()`(DEFERRED)、4 处 `Immediate`(`:262/:307/:1863/:1897`)。
- **范围修正(第 3 轮)**:只有「事务内先读、后写」的路径才会触发读锁升级失败(读快照被并发写者作废时立即 `SQLITE_BUSY`,`busy_timeout` 不生效)。已核实的此类路径:`save_settings` / `save_settings_and_replan`(storage.rs:100/110,事务内先 `settings_from_connection` 读)、`fail_delivery`(:639,事务内先 SELECT attempt_count)、`upsert_event`、`reserve_secondary_notification_test`、`try_claim_source_probe`。而 claim_due_at(:540)等事务首条语句就是 UPDATE,直接取写锁,不属于此场景。
- **修法修正**:不建议把全部 15 处机械替换为 IMMEDIATE(会更早更久占用写锁,增加不必要的写者竞争)。只改上述读-改-写与配额/claim 类需要原子性的路径。双连接并发测试必须用 barrier/test hook 精确控制「连接 A 完成读 → 连接 B 提交写 → 连接 A 尝试写」的顺序,先证明旧实现可稳定触发 BUSY/BUSY_SNAPSHOT,再验证定向 IMMEDIATE 后行为稳定;不能依靠线程调度碰运气。
- 批次 2。工作量:小-中(含测试)。

### M4. UI 线程同步执行数据库读写与文件操作【已核实,机理修正】

- **机理修正(第 3 轮)**:WAL 模式下普通后台写事务**不会阻塞 UI 读**(读不等待写),初稿「后台写事务会让 UI 查询等待 10 秒」夸大。真正的风险:
  1. UI 线程的**写**操作(保存设置并重算全部提醒 main.rs:852-940、确认/撤销确认、人工覆盖)与 runtime 写事务构成写-写竞争,可被 `busy_timeout` 阻塞至 10 秒;M3 的 BUSY 升级错误也会直接把错误抛到 UI;
  2. UI 线程的**重 CPU/文件操作**:诊断包(integrity_check + 打 ZIP,main.rs:1081-1095)秒级卡顿;`show_event_details` 串行 5 组独立查询(main.rs:2209);`refresh_ui` 每次通知都跑 `settings()` + 两个事件列表查询(main.rs:1959+)。
- 修复(按收益):①诊断包、设置保存、确认、人工覆盖移到工作线程 + `invoke_from_event_loop` 回填;②`show_event_details` 异步加载;③`refresh_ui` 读缓存快照。
- 批次 3。工作量:中。

### M8. 公告分页 total 缺失时静默截断;镜像失败分支丢失截断警告【已核实,范围扩展】

- 位置一(初稿):total 回退 `rows.len()` → 不翻页且 `truncated=false`、无「不完整」警告(`announcement.rs:405-410` 巨潮、`:252-261` 上交所)。
- **位置二(第 3 轮新增)**:`(Ok(official), Err(error))` 分支(`announcement.rs:118-124`)只报「巨潮镜像不可用」,**丢失 official.truncated 信息**——上交所结果已截断 + 巨潮同时失败时,审计只看到镜像故障,看不到官方结果不完整。对称的 `(Err, Ok)` 分支反而正确组合了两个信号,属不对称处理。
- **修法再修正(第 4 轮)**:不能只把「total 缺失」改成 `truncated=true` 后仍停止翻页——这虽然不再静默,却仍会漏取后续页,并且对不足一页的完整结果产生不必要警告。解析层应保留 `total: Option<usize>`(或等价的完整性枚举),不要用 `rows.len()` 提前抹掉「计数缺失」状态:
  1. total 合法时按 total 翻页;
  2. total 缺失时,以**原始响应行数**判断:等于 page size 就继续探测下一页,短页/空页结束;
  3. 无 total 且到达安全页数上限时才标记 `truncated=true`;
  4. total 字段存在但格式非法时,明确报 schema 错误(或标记「完整性未知」),不要与字段缺失混为一类;
  5. `(Ok(official), Err(mirror))` 的警告同时组合 `official.truncated` 与镜像错误。
- 批次 1。工作量:小-中(含分页 fixture 测试)。

### M9. runtime 主循环连接/查询 churn【已核实,修法修正】

- 每轮循环:`touch_heartbeat` ×2(runtime.rs:557-558);`next_lifecycle/local/secondary_delivery_at` **各 ×2**(559-567 due 判定 + 696-704 deadline 计算);`settings()` ×3(576/633/681);`automatic_sync_schedule` ×2(其内部还多次开连接)。
- **修法修正(第 3 轮)**:不建议把 `Database` 改为单一 `Mutex<Connection>`——那会连读也串行化,抵消 WAL 并发读优势,且 UI 会在 Mutex 上等后台长事务,反而加剧 M4。只做:单轮循环内复用一份 settings 与三个 next_* 结果(save_settings 后失效)、心跳合并为一条 UPDATE。
- 批次 3。工作量:小。

### M13.【第 3 轮新增】Postponed 状态仍会规划申购提醒【已核实】

- 位置:`model.rs:89-95` `is_terminal()` 只含 `Terminated | Suspended`(状态)与 `SuspendedOrCancelled`(生命周期);`storage.rs:2989` 人工覆盖把「延期发行/暂缓发行」映射为 `IssueStatus::Postponed`;`plan_reminders`(core.rs:163-186)只检查 `is_terminal()` 与 apply_date,**不检查 issue_status**。
- 后果:用户/数据把某任务标为「延期发行」后,若 apply_date 仍是今天或未来,晨间、逐时、截止前提醒照常触发。另注意语义不一致:网络采集把「暂缓发行」映射为 `Suspended`(network.rs:499,终态),而人工覆盖把「暂缓发行」映射为 `Postponed`(storage.rs:2989,非终态)——同一词语两种结果。
- **修法再修正(第 4 轮)**:现有模型只有一个 `apply_date`,无法判断它是延期前遗留的旧日期,还是延期后正式公布的新日期,因此不能可靠实现「Postponed 且无未来新申购日期」这一条件。明确采用以下状态机:
  1. 「暂缓发行/暂停发行」统一映射为 `Suspended`;
  2. 「延期发行」映射为 `Postponed`;
  3. `Postponed` 状态一律停止申购提醒规划;
  4. 可信来源发布新的申购日时,同步把状态恢复为 `Upcoming`/`Active`,再重新规划提醒。
- 批次 1。工作量:小-中(需覆盖旧日期残留、公布新日期后的状态恢复及人工覆盖映射测试)。

---

## 低优先级

### M5. 退出时事件循环线程等待 runtime 收尾【已核实,降级】

- `stop_requested` 在每个数据源/分页/公告检索边界都有检查(runtime.rs 多处、announcement.rs `ensure_not_cancelled`),因此不会无条件跑完整个同步;但当前响应体通过 `take(...).read_to_end(...)` 读取(network.rs:753-758),**读取循环内部没有取消检查**。结合「推翻 4」确认的逐次 read 超时语义,只要服务端持续慢速发送且每次 read 间隔不超过 45 秒,整段响应体读取就没有固定总时限;响应体字节上限不等于退出时间上限。因此旧表述「最坏等待≈连接+头+单次读」仍不成立。
- **修法再修正(第 4 轮)**:继续否决「超时后 detach」——它会在进程退出时硬中断数据库/同步工作。把网络响应体改为显式分块读取,每次成功 read 后检查 `stop_requested`,取消时尽快返回;这样退出最多等待当前阻塞中的 connect/read 操作,不会继续读取整个响应体。UI 可同时显示「正在收尾」。批次 5,仍定为低(可信白名单源下主要是退出体验与异常网络问题)。

### M6. `effective_cutoff` 取 `sessions.last()` 依赖存储顺序【已核实,降级】

- 触发面很窄:网络采集器不产出时段(network.rs 各 collector `sessions: vec![]`,已核实);默认时段天然有序;人工覆盖按时段号顺序编号(storage.rs:2956-2960)——无序只可能来自历史数据/手工改库/未来数据源。且 `plan_reminders` 与 UI 共用同一函数(「一起错」而非互相矛盾)。
- 修法:取 `max(official_end)`(语义即「最后结束的时段」)优于排序后取尾,对任意乱序稳健。批次 5。小。

### M7. `map_event_offset` 硬编码列偏移【已核实,修法修正】

- 偏移当前与列序完全一致;ADD COLUMN 追加表尾不破坏;真实风险是重建表式迁移(本库 v10 即 DROP+RENAME 先例)、SELECT 前缀列数变化、`DROP COLUMN`。
- **修法修正(第 3 轮)**:不能笼统「按列名读取」——查询前缀已有 `o.id`/`s.id`,事件表又有 `e.id`,裸 `row.get("id")` 会取到投递行的 ID。正确做法是显式列出事件列并加别名(`e.id AS event_id` …)。批次 5,随下次 schema 迁移一并做。小。

### M10. `upsert_event` 双层「None 即保留」【改判:数据模型限制,非现行 bug】

- `Candidate` 的 `Option<T>` 无法区分「本次未采到」与「上游显式置空」,且 `replace_field_sources` 对值为 None 的字段不生成来源记录(storage.rs:1264-1300)——初稿建议的「利用 field_sources 判断显式为空」**现有模型实现不了**。
- 处置:引入三态(Missing / Value / ExplicitNull)前不动;且在未确认数据源确实提供显式撤回语义前放开日期清空,可能误取消提醒。记录为已知限制,批次 6(暂缓)。

### M11. 崩溃上报结果落盘非原子 + save_state 失败重复上传【已核实,降级】

- **M11a(可直接修复)**:`crash_upload.rs:256-262` 的 `write_last_result` 裸 `fs::write`;改为项目已有的 `atomic_replace_file`,避免结果 JSON 被部分写入。注意 `save_state` 本身(`:225-239`)已经使用临时文件 + 原子替换,不能笼统表述为「状态文件非原子」。批次 4。
- **M11b(结果不确定窗口)**:`:146-194` 中 HTTP 已成功,但最终 `save_state` 失败时,下次仍可能上传同一报告。单纯「原子替换 + 记日志」不能消除此窗口:远端已经接收而本地未能持久化,客户端无法单方面实现 exactly-once。优先让服务端以 payload 中已有的 `reportSha256` 作为幂等键去重;若服务端不支持,则明确保留 at-least-once 语义并依赖 24h ≤3 次限流,不要用「发送前标成功」换取可能漏报。批次 6(需确认服务端契约),整体仍为低。

### M12. 更新助手 TOCTOU 窗口;Authenticode 禁用吊销检查【已核实,定级:可选加固】

- 缓解已较强:UUID 文件名 + 签名者证书指纹固定(`TRUSTED_UPDATE_SIGNER_SHA256`)+ helper 二次复核。
- **修正(第 3 轮)**:「spawn 前校验 helper」不能消除 TOCTOU(校验与执行之间仍可替换);且威胁模型是同用户已有恶意进程——该进程本就能操作用户数据与应用配置,此加固收益有限。吊销策略(`WTD_REVOKE_NONE`)可在文档记录取舍。批次 6,可选。

### M14.【第 3 轮新增】更新失败路径残留无法及时、安全地回收【已核实,表述修正】

- 更新文件写入 `data_root/temp/updates/` 子目录(updater.rs:140-166),而 maintenance 只遍历 `data_root/temp` **第一层**且仅 `remove_file`(storage.rs:1968-1981,目录被跳过)——下载失败的 `.part`(上限可达 INSTALLER_LIMIT=200MiB)、改名后 Authenticode 校验失败/安装失败的 MSI 会长期残留。helper 在系统 `%TEMP%` 的副本已调用 `MOVEFILE_DELAY_UNTIL_REBOOT`,所以不能称为「永不清理」,但它无法即时回收,且调度删除失败被忽略。
- **修法再修正(第 4 轮)**:
  1. 用 RAII 清理守卫同时覆盖 `.part` 和改名后的 `.msi`,直到 helper 成功启动才解除;失败路径立即尽力清理;
  2. maintenance 直接扫描已知的平面目录 `temp/updates`,无需递归;只删除严格匹配应用更新命名、超过年龄阈值的普通文件;
  3. 使用 `symlink_metadata`/Windows reparse-point 检查,不跟随符号链接或 junction,防止清理越过数据目录;
  4. `%TEMP%` helper 仅清理超过年龄阈值、严格匹配名称且确认不是活动进程映像的普通文件;保留 `delete_after_reboot`,并记录其失败。
- 批次 4。工作量:小-中(含清理边界与 reparse-point 测试)。

### L1. 每条日志重新编译 5 个正则【已核实】operations.rs:280-299 → `OnceLock` 缓存。批次 4。

### L2. `LOG_GATE` 中毒静默丢日志【已核实】operations.rs:201 → `unwrap_or_else(|p| p.into_inner())`。批次 4。

### L4. 二级通知按字符截断 3500,超企业微信字节上限【已核实】secondary_notification.rs:225 → 按服务商设 UTF-8 字节上限。批次 4。

### L5. 远端响应严格 UTF-8,无声明编码兼容路径【已核实】network.rs:749-751。修复时不要对所有非法 UTF-8 响应盲目尝试 GBK,否则损坏或恶意字节也可能被解码为貌似有效的数据;仅根据 `Content-Type charset` 或已知数据源契约选择 GB18030/GBK,无声明且 UTF-8 失败时保持报错,附有限长度十六进制诊断。批次 4。

### L6. `xml_escape` 不滤 XML 非法字符【已核实】windows_integration.rs:405-418。不能直接过滤全部 `char::is_control()`,因为制表符、换行和回车在 XML 1.0 中合法;按 XML 1.0 `Char` 合法范围过滤后再转义,并补包含 `\t`/`\n`/`\r` 与非法 C0 字符的测试。批次 4。

### L7. 读设置失败回退「有声/闪烁/Toast」默认值【已核实】model.rs:192-194 三个默认均 true;main.rs:470-491。改读失败时跳过提醒副作用仅记日志。批次 1。

### L8. startup 失败时 revision 已标记未刷新【已核实】main.rs:355-374 → revision 更新移到刷新成功后。批次 1。

### L9. deployment 旗标不互斥【已核实】deployment.rs:64-97 → 启动互斥校验。批次 4。

### L10. 枚举未知值静默映射 Unknown【已核实】model.rs:14-16 → 反序列化路径记日志。批次 4。

### L11. maintenance 容错不一致 + 备份目标名可能碰撞【部分核实,拆分修法】

- `storage.rs:1969-1981` 属 maintenance:单个条目的 `metadata()?` 可让整轮维护失败,而 `remove_file` 失败又被静默忽略。改为逐条容错并统一记录受脱敏保护的 WARN;与 M14 同批处理。
- `storage.rs:2098` 位于数据库备份提交路径,**不在 maintenance 函数中**;毫秒时间戳目标已存在时 `rename` 会失败。仅记日志不能修复碰撞,应给最终备份名加入随机后缀/UUID,或碰撞后重新生成目标名并有限重试。
- 批次 4。

### L12. 公告搜索每页重复 dedup+排序【已核实】announcement.rs:706-716 → 只在外层做一次。批次 4。

### L14.【第 3 轮新增】更新 URL 凭据校验不一致【已核实】

- `crash_upload.rs:200-210` 检查了 `username` 非空 **与** `password().is_some()`;`updater.rs` 的 `validated_https_url`(`:228-236` 附近)只查 username——`https://:secret@host/...` 可通过更新校验。修复:对齐两项检查(顺带拒绝 fragment)。批次 1。小。

---

## 维持现状(有意设计,不改行为)

### L3. 二级通知哨兵记录(success=-1)计入小时配额【维持现状】

- 请求发出**前**写 `success=-1`(storage.rs:742):即使进程崩溃,请求也可能已到达服务商;让它计入滚动 1 小时配额(storage.rs:705)可防止重启后重复轰炸。这是保守而正确的取向。可选改进:超租约期(>10 分钟)仍为 -1 的记录改标「结果未知」以改善诊断,但**继续计入配额**。

### L13. 150ms 可见性确认 Timer 与快速退出竞态【维持现状】

- 提醒窗口已显示但 150ms 确认未完成即退出 → 行停留 Leased → 下次启动重投。这是 **at-least-once 语义**:在无法确认用户是否真的看到时,重复投递优于静默标记成功。「退出时收尾 Leased」可能造成真实漏报,「同步等待 150ms」会冻结 UI,均不建议;除非引入持久化的「呈现确认」状态(不值得)。保持现状。

---

## 复核后推翻的初判

### 推翻 1:「确认后提醒被复活」——不是 bug

`acknowledge_at`(storage.rs:262-298)先取消所有 Pending/Leased/Failed 行,再在同一事务内重新规划;`plan_reminders` 对 Acknowledged 生命周期有显式守卫(core.rs:186),只重建受 `post_apply_reminders_enabled` 控制的「申购后提示」(预期行为);dedupe_key 含 event_version(core.rs:287);`ON CONFLICT` 保留 2/3 正是 revoke 恢复提醒的依赖机制。残留:`acknowledged_at` 未随重置清空,纯一致性瑕疵,无功能影响。

### 推翻 2:「request_sync 积压触发连续同步」——不成立

runtime.rs:719-728 消费 Sync 后用 `try_recv` 循环合并队列中积压的同类命令。

### 推翻 3:「runtime 主循环 `?` 静默杀死后台线程」——不成立

`run_loop` 的 Err 由线程闭包重试包装(runtime.rs:470-520)接住:记 ERROR、写健康 Failed、UI 状态显式标红,按 `[1s, 5s, 15s, 30s]`(runtime.rs:41-46)退避重启,稳定 10 分钟后计数重置。初始化失败路径同样显式呈现。残留影响仅为当轮作废 + 短暂重启(M3 修好后触发率大降)。

### 推翻 4(M1):「自动更新下载被 45 秒总超时切断」——前提错误,整条推翻【第 3 轮证实】

- **本项目两个版本的审查(含 v2 的降级和带宽推导)都建立在错误前提上**:把 reqwest blocking client 的 `.timeout()` 当作「从连接到响应体读完的总超时」。源码核实(Cargo.lock 锁定 reqwest 0.12.28):
  1. `blocking/client.rs:383-392`:`timeout` 的文档语义是 **"Set a timeout for connect, read and write operations of a Client"**——连接、单次读、单次写各自适用,并非全程总预算;
  2. `blocking/response.rs` 的 `impl Read for Response`:每次 `read()` 都通过 `wait::timeout(self.body_mut().read(buf), timeout)` **重新套用完整超时**(`wait.rs:9-15` 每次调用重算 deadline);
  3. blocking `ClientBuilder` **没有** `read_timeout` 方法(只有 `timeout`/`connect_timeout`)——v2 建议的修法在 blocking 上不存在。
- 因此 updater 的下载循环(`read_limited` → `read_to_end`)只要**每次 read 间隔不超过 45 秒**(数据持续到达),总时长不限。6.1 MiB 实际包体在 1 Mbps 下需 ~50 秒也能正常完成。「基本必现」「慢速网络必失败」均不成立。而「单次读卡死 45 秒即失败」恰是合理的 stall 保护。
- 连带修正:M5 原文「单请求受 45s 总超时约束」同为此语义误读,已改。
- **处置:M1 撤销,不进入任何修复批次。**此前两轮对安装包大小的核查(6.1 MiB)结论仍正确,但已无需要修复的问题。

---

## 做得好的方面(复核确认)

SQL 全参数化;请求前后双重 URL 白名单 + `Policy::none()` + 响应体积上限;更新链固定签名者证书 + 分离签名 + SHA-256 + Authenticode + helper 复核;runtime 线程日志 + 健康写库 + UI 呈现 + 退避重启的完整自恢复;outbox 租约/退避状态机自洽;崩溃上报默认关闭、脱敏、限流;统一东八区时间处理。

## 修复顺序(v4)

| 批次 | 内容 |
|---|---|
| 1 | M2(else→fail_delivery)、M8(保留 total 缺失状态 + 满页继续探测 + 组合警告)、M13(明确状态迁移并停止 Postponed 规划)、L7、L8、L14(URL 凭据对齐) |
| 2 | M3(仅读-改-写路径改 IMMEDIATE + barrier 控制的双连接并发测试) |
| 3 | M4(UI 重操作异步化:诊断、设置保存、确认、覆盖)、M9(仅单轮缓存/合并) |
| 4 | M11a(`write_last_result` 原子替换)、M14(双阶段清理守卫 + 平面定向清理 + reparse 防护)、L1、L2、L4、L5、L6、L9、L10、L11、L12 |
| 5 | M5(取消感知的分块读取 + 收尾提示)、M6(`max(official_end)`)、M7(显式别名化) |
| 6(暂缓/维持) | M10(等三态模型与数据源语义确认)、M11b(确认服务端幂等契约,否则保留 at-least-once)、L3、L13(维持现状)、M12(可选加固,或文档记录取舍) |

每批完成后按 AGENTS.md:`rtk cargo fmt` → `rtk cargo test` → `rtk cmd /c build.bat --package` → `scripts/validate-build-layout.ps1`。

## 回归验收矩阵(v4 新增)

通用命令只能证明整体未回归,不能替代问题对应的定向测试。至少补齐以下验收:

| 条目 | 必须覆盖的回归场景 |
|---|---|
| M2 | 专用提醒窗口显示失败后进入 Failed、写入 `last_error`、按既有公式退避;显示成功路径不受影响 |
| M3 | barrier 精确制造 DEFERRED 读后写升级竞争;修复前稳定复现 BUSY/BUSY_SNAPSHOT,修复后两连接结果原子且无偶现测试 |
| M5 | 自定义慢速 `Read` 在多次成功读取后触发取消;确认读取循环及时返回,且不通过 detach 中断数据库工作 |
| M8 | total 合法多页、total 缺失短页、total 缺失满页继续探测、达到页数上限、total 格式非法、official truncated + mirror error 组合警告 |
| M13 | Postponed + 遗留未来日期不规划;「暂缓发行」映射一致;可信新日期把状态恢复为 Upcoming/Active 后重新规划 |
| M14/L11 | `.part`、验签失败 MSI、安装失败 MSI、未过期文件、无关文件、符号链接/junction/reparse point、单条 metadata/remove 失败、备份同毫秒命名碰撞 |
| L4 | 各服务商分别按 UTF-8 字节上限截断,多字节中文不被切坏,JSON payload 仍有效 |
| L5 | UTF-8、明确声明 GB18030/GBK、无声明非法 UTF-8、超长错误诊断截断 |
| L6 | XML 转义字符、合法 `\t`/`\n`/`\r` 保留、非法 C0 字符过滤 |
| L8 | 首次刷新失败时 revision 不前移;同一 revision 后续可重试并成功 |
| L14 | username、password-only userinfo、fragment、非 HTTPS、重定向后的最终 URL 均按统一策略校验 |

## 附:四轮交叉复核记录

| # | 修订主张 | 来源 | 结论 | 关键依据 |
|---|---|---|---|---|
| 1 | H1 降级(实际包体 6.1 MiB) | 第 2 轮 | 采纳 | 实测 MSI = 6,373,376 字节 |
| 2 | M1(提醒重试)「无任何日志」不准 | 第 2 轮 | 采纳 | main.rs:769 有 ERROR;副作用先于 `if shown` |
| 3 | DEFERRED 计数 15+4 | 第 2 轮 | 采纳 | 逐行重数 |
| 4 | effective_cutoff「一起错」表述 | 第 2 轮 | 采纳 | plan 也调 effective_cutoff |
| 5 | ADD COLUMN 不移位 | 第 2 轮 | 采纳 | SQLite 追加语义;真实风险=重建/前缀/DROP |
| 6 | L3 非永久占用 | 第 2 轮 | 采纳 | 滚动 1h 窗口 |
| 7 | L7 含 toast | 第 2 轮 | 采纳 | model.rs:194 |
| 8 | M9 加重(next_* ×2) | 第 2 轮 | 采纳 | runtime.rs:559-567/696-704 |
| 9 | 新增 150ms Timer 竞态 | 第 2 轮 | 采纳→v3 改判维持现状 | at-least-once 更安全 |
| 10 | S1 runtime 线程静默死亡 | 第 2 轮 | **驳回** | runtime.rs:470-520 重试包装 |
| 11 | **M1(45s 总超时)推翻** | 第 3 轮 | **采纳** | reqwest blocking 源码:client.rs:383 文档语义 + response.rs Read 每次重套超时 + 无 read_timeout |
| 12 | M3 只改读后写路径 | 第 3 轮 | 采纳 | save_settings/fail_delivery 等事务内先读已核实;claim 首条即 UPDATE |
| 13 | M4 机理改为 UI 写竞争 + CPU/文件 I/O | 第 3 轮 | 采纳 | WAL 读不阻塞写 |
| 14 | M5 降级(取消检查密集)+ 否决 detach | 第 3 轮 | 采纳 | 每源/分页边界均有取消检查 |
| 15 | M6 降级 + max(official_end) | 第 3 轮 | 采纳 | 网络不产出 sessions;覆盖按输入顺序编号 |
| 16 | M7 需显式别名 | 第 3 轮 | 采纳 | 前缀列与 e.id 同名冲突 |
| 17 | M9 否决 Mutex\<Connection\> | 第 3 轮 | 采纳 | 串行化读、加剧 M4 |
| 18 | M10 改判数据模型限制 | 第 3 轮 | 采纳 | replace_field_sources 对 None 不产记录,需三态模型 |
| 19 | L3 维持现状 | 第 3 轮 | 采纳 | 请求先落账防重启轰炸,保守配额是特性 |
| 20 | L13 维持现状 | 第 3 轮 | 采纳 | 同 #9 |
| 21 | M12 不能视为完整修复 | 第 3 轮 | 采纳 | 前置校验仍有窗口;威胁模型即同用户进程 |
| 22 | 新增:更新临时文件无清理 | 第 3 轮 | **采纳(属实)** | updater 写 temp/updates 子目录;maintenance 只扫第一层文件 |
| 23 | 新增:Postponed 仍规划提醒 | 第 3 轮 | **采纳(属实)** | is_terminal 不含 Postponed;plan 不查 status;「暂缓发行」两处映射不一致 |
| 24 | 新增:镜像失败丢失截断警告 | 第 3 轮 | **采纳(属实)** | announcement.rs (Ok,Err) 分支未组合 official.truncated |
| 25 | 新增:更新 URL 未查 password | 第 3 轮 | **采纳(属实)** | updater 与 crash_upload 校验不对齐 |
| 26 | M5 退出等待仍无固定总上界 | 第 4 轮 | **采纳** | `read_to_end` 内无取消检查;逐次 read 超时会在慢速持续数据下反复重置 |
| 27 | M8 缺失 total 不应只标截断 | 第 4 轮 | **采纳** | 只告警仍漏页;应保留 Option 并用原始满页继续探测 |
| 28 | M13「未来新日期」现有模型不可判定 | 第 4 轮 | **采纳** | 单一 apply_date 无法区分延期前旧值和重新公布值;改为显式状态迁移 |
| 29 | M11 原子性与重复上传需拆分 | 第 4 轮 | **采纳** | save_state 已原子;write_last_result 未原子;HTTP 成功后落盘失败需服务端幂等或接受 at-least-once |
| 30 | M14 清理不能盲目递归/全量扫描 %TEMP% | 第 4 轮 | **采纳** | 需平面定向扫描、年龄阈值、普通文件/reparse 防护和活动 helper 排除 |
| 31 | L5 不能盲目 GBK 兜底 | 第 4 轮 | **采纳** | 编码选择需依据 Content-Type 或来源契约,否则损坏字节可能被误解码 |
| 32 | L6 `char::is_control` 过度过滤 | 第 4 轮 | **采纳** | XML 1.0 合法保留 TAB/LF/CR,应按 Char 范围过滤 |
| 33 | L11 混合 maintenance 与 backup 两条路径 | 第 4 轮 | **采纳** | `:2098` 不在 maintenance;目标碰撞需唯一命名/重试而非仅记日志 |
| 34 | 批次编号与定向验收缺失 | 第 4 轮 | **采纳** | 统一 M11/M12 批次并新增逐项回归矩阵 |

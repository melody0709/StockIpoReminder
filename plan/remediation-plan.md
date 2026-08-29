# Stock IPO Reminder 审查修复执行计划

## 1. 计划元数据

- 制定日期:2026-08-28
- 源码基线:`main` @ `58c8df7`
- 应用版本:`Cargo.toml` 中的 `0.3.1`;版本号始终以 `Cargo.toml` 为唯一来源
- 审查依据:`plan/fix.md` v4
- 目标:按风险和依赖关系修复已确认问题,补齐可重复的回归测试,生成并验证当前版本的 Windows 发布产物
- 本文是执行计划。问题论证、误报推翻过程和历次交叉复核记录保留在 `plan/fix.md`,执行时不重复争论已定结论

## 2. 执行规则

1. 开始前阅读仓库根目录 `AGENTS.md` 和其引用的 `C:\Users\kawae\.codex\RTK.md`。
2. Windows 命令只使用 PowerShell 7 (`pwsh`),所有 shell 命令以 `rtk` 开头。
3. 修改文件使用 `apply_patch`;保留工作区已有用户改动,不得清理、覆盖或顺带重构无关代码。
4. 开始每个批次前运行 `rtk git status --short`。若目标文件存在来源不明且与本批重叠的修改,停止该子项并报告,不要覆盖。
5. 行号仅用于定位基线。若源码已变化,以函数名、类型名和行为为准,不得按行号机械替换。
6. 每完成一个 Rust/Slint/设置/存储/同步/打包相关批次:
   - 运行该批次的定向测试;
   - 运行 `rtk cargo fmt`;
   - 运行 `rtk cargo test`;
   - 运行 `rtk cargo clippy --all-targets`,记录并区分既有警告与本批新增警告;
   - 检查并更新与用户行为变化对应的用户文档和 `RELEASE_NOTES.md`。
7. 所有实施批次完成后运行:

   ```text
   rtk cmd /c build.bat --package
   rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1
   ```

   如果批次被分别交付,则每次交付都执行打包和布局验证。仅在用户明确要求 source-only 时跳过发布产物。
8. 不主动提升版本号;如用户要求发布新版本,只修改 `Cargo.toml`,UI 继续使用 `CARGO_PKG_VERSION`。
9. 任一修复若需要改变本文明确的业务语义、引入新的外部服务契约或扩大数据删除范围,先停止并请求用户决定。

## 3. 全局完成标准

计划完成必须同时满足:

- 批次 1–5 的所有任务完成,或明确记录无法实施的外部阻塞;
- 第 9 节的暂缓项没有被误改;
- 所有新增回归测试稳定通过,不得依赖线程调度或真实公网偶然行为;
- `rtk cargo test` 全量通过;
- `rtk cargo clippy --all-targets` 完成且没有本计划引入的新警告;
- 无新增未解释的编译警告;已有警告如仍存在,在完成报告中列明;
- 当前版本以下文件存在且由当前源码重新生成:

  ```text
  build\run\x64-release\StockIpoReminder.exe
  build\packages\<version>\StockIpoReminder-<version>-win-x64-portable.zip
  build\packages\<version>\StockIpoReminder-<version>-win-x64.msi
  build\packages\<version>\release-manifest.json
  build\packages\<version>\SHA256SUMS.txt
  ```

- 构建布局验证通过;
- 最终报告提供可点击的 EXE、MSI、portable ZIP、`SHA256SUMS.txt` 路径,并分别报告测试、release build、安装包、便携包、布局验证结果。

执行状态表(执行 AI在完成批次后更新状态和证据链接):

| 批次 | 初始状态 | 依赖 | 交付重点 |
|---|---|---|---|
| 1 | 待处理 | 无 | 提醒失败退避、公告完整性、Postponed 语义、URL/设置/UI revision |
| 2 | 待处理 | 建议在批次 1 后 | SQLite 定向 IMMEDIATE 与确定性并发测试 |
| 3 | 待处理 | 批次 2 | UI 异步化、runtime 单轮缓存 |
| 4 | 待处理 | 可在批次 2 后实施;合并时仍按本文顺序验证 | 文件原子性、清理安全、日志/编码/XML/部署/枚举/维护 |
| 5 | 待处理 | 批次 1–3 | 取消感知读取、截止时间、显式 SQL 别名 |
| 暂缓项 | 不实施 | 外部契约或单独设计决定 | M10、M11b、M12、L3、L13 |

## 4. 批次 1:直接影响提醒正确性和安全校验

### 1.1 M2:提醒窗口显示失败进入正常失败退避

涉及位置:

- `src/main.rs`: `drain_runtime_ui`,调用 `show_dedicated_reminder` 后的 `if shown` 分支
- `src/storage.rs`: `fail_delivery` / `fail_delivery_at`

修改要求:

1. `shown == false` 时,对本批每个 delivery 调用 `fail_delivery`,写入清晰但不包含敏感信息的错误原因。
2. 保留既有 `fail_delivery_at` 的退避公式和 `last_error` 记录,不要另建第二套重试策略。
3. 单条状态更新失败只记录 ERROR,继续处理同批其他 delivery。
4. 显示成功路径继续走 150ms 可见性确认,不得改变现有 at-least-once 语义。

禁止方案:

- 不得在显示失败时直接 `complete_delivery` 或取消提醒;
- 不得新增固定 2 分钟重试;
- 不得修改 L13 的 150ms 可见性确认策略。

定向测试:

- `fail_delivery_at` 后状态为 Failed,`last_error` 已写入,`lease_until` 使用既有退避公式;
- 显示失败处理不会遗漏批次中的后续 delivery;
- 显示成功路径不进入失败处理。

### 1.2 M8:公告分页完整性和组合警告

涉及位置:

- `src/announcement.rs`: `search_sse`, `search_sse_official`, `parse_sse_reference_page`
- `src/announcement.rs`: `search_cninfo_market`, `parse_cninfo_reference_page_for_event`

修改要求:

1. 页解析结果必须保留以下三项,可使用小型结构体表达:
   - 解析后的 references;
   - 原始响应数组行数;
   - `total: Option<usize>` 或等价的完整性状态。
2. total 合法时按 total 计算页数。
3. total 缺失时:
   - 原始行数等于 page size:继续请求下一页;
   - 原始行数小于 page size 或为空:结束;
   - 到达 `MAX_ANNOUNCEMENT_PAGES` 且最后一页仍满:标记 `truncated=true`。
4. total 字段存在但类型/数值非法时返回带来源上下文的 schema 错误,不得静默退回 `rows.len()`。
5. `(Ok(official), Err(mirror))` 同时保留镜像错误和 `official.truncated` 警告;不得让其中一个信号覆盖另一个。
6. 本批不要顺带实施 L12 的全局 dedup 优化,除非修改数据结构时不可分割;如同时实施,必须单独验证结果顺序和去重行为未变。

定向测试全部使用 fixture/本地假响应,不得访问真实网络:

- total 合法且有多页;
- total 缺失且第一页为短页;
- total 缺失且第一页满页、第二页短页;
- total 缺失并连续满页直到安全上限;
- total 字段存在但格式非法;
- official 截断且 mirror 失败时警告包含两个事实;
- official 和 mirror 都成功时仍正确合并及去重。

### 1.3 M13:统一 Postponed/Suspended 状态语义

涉及位置:

- `src/core.rs`: `plan_reminders`
- `src/network.rs`:网络状态映射
- `src/storage.rs`: `parse_issue_status_override`,人工覆盖应用/撤销路径
- 与候选合并、事件重规划有关的存储逻辑

业务语义:

- 「暂缓发行」「暂停发行」=`Suspended`;
- 「延期发行」=`Postponed`;
- `Postponed` 和 `Suspended` 均不从 `plan_reminders` 生成提醒;恢复正常状态后再基于最新日期整体重规划;
- `Postponed` 不是永久终态:后续可信来源发布**与当前遗留值不同的新申购日**且候选状态为 `Upcoming`/`Active` 后可以重新规划;
- 活跃的人工 `IssueStatus` 覆盖优先于网络数据。若用户手工设为 Postponed,网络不得擅自恢复;用户必须修改或撤销该状态覆盖。

修改要求:

1. 拆分人工覆盖映射,不得继续把「延期发行」和「暂缓发行」映射为同一状态。
2. `plan_reminders` 对 `IssueStatus::Postponed` 显式返回空计划。不要仅为复用守卫就把 Postponed 加入全局 `is_terminal()`,除非先审计所有调用方;它在业务上仍是可恢复状态。
3. 非人工覆盖事件仅在高可信候选同时满足以下条件时恢复为 `Upcoming`/`Active`:候选状态正常、候选 `apply_date` 存在、且候选日期与当前 Postponed 事件保存的日期不同。若来源能提供明确的「恢复发行」状态,可另加该显式信号,但必须有 fixture 和测试依据。
4. 同一个遗留日期即使仍在未来也不能自动解封;不能只运行 `status_from_dates` 就把 Postponed 恢复为 Upcoming。
5. 多个来源对新日期冲突时沿用现有优先级/冲突机制并保持 Postponed,不得选一个日期擅自恢复;待冲突消除后再恢复。
6. Postponed 期间通过现有重规划/取消机制取消当前 event version 的既有 Pending/Leased/Failed 计划,防止旧计划继续投递;恢复 Upcoming/Active 时按新的 event version 和日期重建。不得把已经 Delivered 的历史记录改写。

定向测试:

- Postponed + 遗留未来 apply_date 不产生申购提醒;
- 人工输入「暂缓发行」得到 Suspended,「延期发行」得到 Postponed;
- 非人工 Postponed 事件收到可信、不同的新日期及 Upcoming/Active 后恢复规划;
- 同一遗留日期配合派生出的 Upcoming 状态不会自动恢复;
- 新日期存在来源冲突时保持 Postponed 且不规划;
- 人工 Postponed 覆盖不会被网络候选覆盖;
- 撤销人工状态覆盖后按当前可信数据重新计算状态和提醒。

### 1.4 L7:读取设置失败时通知副作用 fail-closed

涉及位置:`src/main.rs` 提醒呈现后读取 `runtime.settings()` 的路径。

修改要求:

- 设置读取失败时记录 ERROR,不再回退到 `AppSettings::default()` 后自动播放声音、闪烁任务栏或发送 Toast;
- 专用提醒窗口已成功显示时仍可完成 delivery,不得把设置读取失败误判为窗口显示失败;
- 不得改变用户设置读取成功时的现有行为。

定向测试:把“设置读取结果 → 是否执行 sound/flash/toast”的决策提取为可测试逻辑,覆盖成功、全部关闭、读取失败三种情况。

### 1.5 L8:UI revision 只在刷新成功后提交

涉及位置:`src/main.rs`: `drain_runtime_ui`, `RuntimeUiBridgeState.last_ui_revision`, `refresh_ui`。

修改要求:

1. 发现 revision 变化时先记录待刷新值,不要立即写入 `last_ui_revision`。
2. `refresh_ui` 成功后才提交该 revision。
3. 刷新失败时保留旧 revision,确保下一次 notifier/事件循环仍会重试。
4. 如果把 `refresh_ui` 改为异步,只允许最新 generation 的结果提交 revision,防止旧请求覆盖新状态。

定向测试:首次刷新失败、同一 revision 第二次成功;连续两个 revision 的旧异步结果不得覆盖新结果。

### 1.6 L14:统一 HTTPS URL 凭据与 fragment 校验

涉及位置:

- `src/updater.rs`: `validated_https_url`
- `src/crash_upload.rs`:同名校验函数

修改要求:

- 两处统一拒绝非 HTTPS、缺少 host、非空 username、任何 password、fragment;
- 初始 URL和重定向后的最终 URL都执行相同校验;
- 不改变既有 redirect policy、可信签名和体积限制;
- 可提取共享小函数,但不要为此进行无关网络层重构。

定向测试:

- 合法 HTTPS URL;
- username userinfo;
- `https://:secret@host/...` 这类 password-only userinfo;
- fragment;
- 非 HTTPS和缺少 host;
- 最终重定向 URL同样被检查。

批次 1 完成门槛:上述定向测试、`rtk cargo fmt`、`rtk cargo test` 全部通过,并更新与 Postponed 行为有关的用户文档/`RELEASE_NOTES.md`。

## 5. 批次 2:SQLite 读后写升级竞争

### 2.1 M3:只对真实读-改-写路径使用 IMMEDIATE

涉及位置:`src/storage.rs`。

已确认目标:

- `save_settings`
- `save_settings_and_replan`
- `upsert_event`
- `fail_delivery_at`
- `reserve_secondary_notification_test`
- `try_claim_source_probe`

修改要求:

1. 上述事务若保持“事务内先读后写”结构,改用 `TransactionBehavior::Immediate`。
2. 可把 `try_claim_source_probe` 等路径改成单条条件 UPDATE 以消除读后写,但必须保持原子 claim 语义和返回值;选择该方案时无需再强行使用 IMMEDIATE。
3. 不得机械修改全部 `.transaction()`。事务首条语句就是 UPDATE/INSERT 的路径保持现状,除非有独立证据。
4. 保留 `busy_timeout`;不要用无限重试掩盖锁竞争。

并发测试要求:

1. 使用两个独立 SQLite 连接和 barrier/test hook 精确控制顺序。
2. 对旧模式证明以下顺序可稳定产生 BUSY/BUSY_SNAPSHOT:
   - A 开始 DEFERRED 并完成读取;
   - B 写入并提交;
   - A 尝试写入。
3. 对修复后路径验证:
   - A 获得 IMMEDIATE 写权后读取;
   - B 的写等待或按 busy timeout 明确失败;
   - A 更新并提交,不存在丢失更新或偶现测试。
4. 至少覆盖设置保存、delivery fail 和一个配额/claim 路径。
5. 测试不得依赖 `sleep` 猜测线程顺序。

批次 2 完成门槛:定向并发测试连续运行稳定,`rtk cargo fmt`、`rtk cargo test` 通过。

## 6. 批次 3:UI 异步化与 runtime 单轮缓存

### 3.1 M4:UI 线程不执行重数据库/文件操作

优先异步化范围:

- 创建诊断包(`integrity_check` + ZIP);
- 保存设置并重规划;
- 确认、撤销确认;
- 应用、撤销人工覆盖;
- `show_event_details` 的多组查询;
- `refresh_ui` 中可造成可见卡顿的数据库列表读取。

统一实现要求:

1. UI 回调只收集并校验输入、设置 busy 状态、启动工作线程。
2. 工作线程不得持有或访问 Slint UI 对象;只传递 owned 数据和可安全克隆的 runtime/database handle。
3. 完成后通过 `slint::invoke_from_event_loop` 回填 UI。
4. 成功和失败路径都必须清除 busy 状态;线程创建失败也要恢复 UI。
5. 同一操作防止重复提交。详情和刷新请求使用 generation/token,旧结果不得覆盖用户后来选择的事件或更高 revision。
6. 保持错误日志脱敏,同时给 UI 可理解的失败信息。
7. 不引入全局线程池或异步 runtime,除非现有简单工作线程确实无法满足;优先复用项目已有 update/crash-upload 后台操作模式。

验收:

- 人为让数据库操作或 ZIP 操作阻塞时,Slint 事件循环仍能重绘和响应窗口操作;
- 连续点击同一写操作不会产生两个并行提交;
- 快速切换详情时旧结果不会覆盖新选择;
- 异步错误后按钮/busy 状态恢复;
- 应用退出时不得因 detached 数据库写导致事务被硬中断。需要跟踪的工作线程应安全收尾或在操作生命周期内禁止退出并明确提示。

### 3.2 M9:只做单轮缓存,保持 WAL 并发优势

涉及位置:`src/runtime.rs` 主循环及相应 storage 查询。

修改要求:

1. 每轮只读取一次 settings,在本轮健康摘要、同步退避和 deadline 计算中复用。
2. `next_lifecycle_transition_at`、`next_local_delivery_at`、`next_secondary_delivery_at` 在无相关写入时复用第一次结果。
3. 若本轮执行了 lifecycle 更新、delivery、同步或其他会改变 deadline 的写操作,只重新计算受影响的值,不要无条件重复全部查询。
4. `automatic_sync_schedule` 重构为接收已取得的 settings/必要数据,避免函数内部再次打开多条连接。
5. 两次 heartbeat 合并为一次连接/一条批量写入操作,同时更新 scheduler 和 delivery。
6. UI 保存设置后发出的 Wake 必须使下一轮重新读取 settings。

禁止方案:

- 不得把 `Database` 改成单一 `Mutex<Connection>`;
- 不得跨多轮永久缓存 settings 或 deadline;
- 不得为减少查询而复用已经被本轮写操作作废的 deadline。

定向测试:

- 无到期工作时,计数器/测试连接证明 settings 和三个 next 查询不会重复;
- 本轮执行 delivery/lifecycle 更新后 deadline 被正确重算;
- 保存设置 + Wake 后下一轮使用新设置;
- heartbeat 两个名称都被更新。

批次 3 完成门槛:定向测试和 UI 手工响应性验证通过,`rtk cargo fmt`、`rtk cargo test` 通过。

## 7. 批次 4:低风险健壮性与维护

### 4.1 M11a:崩溃上报结果文件原子提交

涉及位置:`src/crash_upload.rs`: `write_last_result`。

- 使用同目录 UUID 临时文件 + `operations::atomic_replace_file`;
- 失败时尽力删除临时文件并返回带上下文错误;
- 不修改已经原子化的 `save_state`;
- 不在本批尝试客户端 exactly-once;M11b 见暂缓项。

测试:正常替换、已有目标替换、提交失败后的临时文件清理;测试不得破坏真实 diagnostics 目录。

### 4.2 M14:更新残留的边界安全清理

涉及位置:

- `src/updater.rs`:下载、改名、Authenticode 校验、helper dispatch
- `src/storage.rs`: `maintenance`
- `src/windows_integration.rs`:延迟删除结果处理

修改要求:

1. RAII 守卫跟踪 `.part` 和改名后的 `.msi`;helper 成功启动前任何错误都尽力删除当前文件。
2. helper 成功启动后解除即时守卫,安装包由 helper 成功路径删除;安装失败残留交给后续维护。
3. maintenance 只扫描 `data_root/temp/updates` 的**当前一层**,不递归。
4. 只删除超过年龄阈值且严格匹配以下应用生成格式的普通文件:
   - `.StockIpoReminder-<version>-win-x64-<uuid>.msi.part`
   - `StockIpoReminder-<version>-win-x64-<uuid>.msi`
5. 使用 `symlink_metadata` 并拒绝 symlink/junction/reparse point;不得跟随目录项离开 `data_root`。
6. 单个 metadata/remove 失败记录 WARN 并继续,不能中止整轮 maintenance。
7. `%TEMP%` helper 只处理严格匹配 `StockIpoReminder-Update-<uuid>.exe` 且超过保守年龄阈值的普通文件。无需为清理而引入复杂进程枚举;删除遇到 sharing violation 时记录并跳过,保留 `delete_after_reboot`。
8. `delete_after_reboot` 失败不得静默。

禁止方案:

- 不得递归删除整个 `temp` 或 `updates`;
- 不得调用 `remove_dir_all`;
- 不得删除不匹配命名规则或未达到年龄阈值的文件;
- 不得跟随 reparse point。

### 4.3 L1/L2:日志热路径

涉及位置:`src/operations.rs`。

- L1:使用 `OnceLock`/`LazyLock` 缓存 `redact` 使用的 5 个 Regex,每条日志不再重新编译;
- L2:`LOG_GATE` 中毒时使用 `poisoned.into_inner()` 恢复串行化,不得静默丢弃后续日志;
- 保持当前脱敏规则、替换顺序和日志轮转行为。

测试:既有脱敏 fixture 全部保持一致;新增中毒恢复测试或把锁恢复逻辑提取为可验证的小函数。

### 4.4 L4:按服务商 UTF-8 字节限制截断

涉及位置:`src/secondary_notification.rs`。

修改要求:

- 为每个 provider 定义独立限制,数值必须依据服务商正式文档或项目已确认契约,并在常量旁注明依据;
- 按 UTF-8 字节数截断且只能在字符边界结束;
- 标题和正文按各 provider 的实际字段分别处理,不得用一个 3500 字符常量覆盖全部服务商;
- 保持 JSON 有效;若添加省略号,其字节数计入限制。

测试:ASCII、中文、emoji、刚好到边界、超过边界、极小限制,并验证序列化后的目标文本字段不超限。

### 4.5 L5:受控的响应编码兼容

涉及位置:`src/network.rs`: `response_text` 及调用方。

修改要求:

1. 读取 body 前保存/解析 `Content-Type charset`。
2. UTF-8 正常解码保持原路径。
3. 仅在 charset 明确声明 GB18030/GBK/GB2312,或调用方明确标识为已确认使用该编码的数据源时使用相应解码器。
4. 不得对无声明的任意非法 UTF-8 自动做 GBK 猜测。
5. 不支持或解码失败时返回来源、charset 和有限长度 hex preview;preview 不超过固定小上限并经过日志脱敏。
6. 保持现有响应体积上限。

测试:UTF-8、明确 GB18030/GBK、大小写/空格 charset、未知 charset、无声明非法 UTF-8、hex preview 长度上限。

### 4.6 L6:过滤 XML 1.0 非法字符

涉及位置:`src/windows_integration.rs`: `xml_escape`。

- 先按 XML 1.0 `Char` 合法范围过滤,再转义 `& < > " '`;
- 必须保留合法 TAB、LF、CR;
- 不得使用笼统的 `char::is_control()` 删除所有控制类字符。

测试:五个转义字符、TAB/LF/CR、非法 C0 字符、普通中文和 emoji。

### 4.7 L9:部署模式互斥

涉及位置:`src/deployment.rs`: `try_handle`。

修改要求:

- 先解析为单一模式枚举:Install、Uninstall、LegacyUninstallHelper、MsiUninstallHelper;
- 显式旗标或隐式文件名推导出两个以上模式时立即报错,不得依赖当前 `if/else` 优先级;
- 每种模式只接受与其相关的必需参数;互相冲突的 helper 参数应报错;
- 保持合法 setup/uninstaller 隐式模式兼容。

测试:四种合法模式、所有两两冲突、隐式模式与显式冲突、普通启动返回 None。

### 4.8 L10:未知枚举值可诊断

涉及位置:`src/model.rs` 的 `numeric_enum!` 和 `src/storage.rs` 各 DB 映射边界。

修改要求:

- 不要在通用 `from_i32` 内无上下文地每次写日志;
- 增加 checked 转换或在 storage row mapping 处检测未知原始值,记录 enum/字段名和整数值;
- 为避免 UI 高频读取刷屏,同一 enum + 原始值应在进程内去重或限频;
- 保持当前可恢复策略:除非该字段对安全状态机至关重要,未知值仍映射到 `Unknown`;若选择返回错误,必须逐字段说明理由并补兼容测试。

测试:每个代表性 enum 的合法值、未知值映射、诊断去重。

### 4.9 L11:maintenance 容错和备份名碰撞

涉及位置:`src/storage.rs`。

- maintenance:每个目录项的 read/metadata/remove 独立容错并记录 WARN,一个占用文件不能中止整轮维护;
- backup:最终备份名加入 UUID/随机后缀,或在目标碰撞时有限重试;不能只记录 rename 失败;
- 保持同目录临时文件、完整性检查、sync 和最终原子 rename 顺序;
- 不覆盖已有备份。

测试:单条 metadata/remove 失败后继续、同一毫秒创建两个备份、已有同名目标不被覆盖、临时文件失败清理。

### 4.10 L12:公告只在外层统一去重排序

涉及位置:`src/announcement.rs`。

- 页解析函数只解析和校验,不在每页执行全量 dedup/sort;
- 搜索函数汇总全部已读取页面后执行一次 `deduplicate`;
- 测试辅助 API若承诺返回去重结果,在辅助 API边界调用一次,不要改变既有测试语义;
- 保持最终稳定顺序、冲突处理和 URL 校验行为。

测试:跨页重复、同页重复、不同 provider 相同 ID、输入顺序变化后的稳定输出。

批次 4 完成门槛:所有定向文件系统测试必须使用临时目录;不得操作真实 `%TEMP%` 中不属于测试的文件。随后运行 `rtk cargo fmt`、`rtk cargo test`。

## 8. 批次 5:退出体验和结构性加固

### 5.1 M5:取消感知的分块响应读取

涉及位置:

- `src/network.rs`: `response_text`, `read_limited`
- `src/announcement.rs` 及采集调用链中的 `cancelled` 回调
- `src/runtime.rs` 停止流程

修改要求:

1. 将 runtime 数据同步使用的 `read_to_end` 改为显式固定大小 buffer 循环。
2. 每次成功 read 后检查取消标志;取消时返回可识别的取消错误,上层不得把它记录成来源故障或增加失败退避。
3. 继续保留体积上限和逐次 read stall timeout。
4. `send()`/当前阻塞 read 可能仍需等待一次操作超时;UI 显示「正在收尾」,但不得承诺绝对秒数。
5. 不修改自动更新下载的超时语义,除非为共享读取帮助函数所必需;更新下载没有 runtime stop token。

禁止方案:

- 不得超时后 detach runtime 线程;
- 不得强杀可能正在写数据库的线程;
- 不得删除体积上限或把 45 秒误改为总下载预算。

测试:自定义 `Read` 连续返回多个小块,中途切换取消标志后立即停止;取消不计为源失败;超限仍报错。

### 5.2 M6:时段截止取最大 official_end

涉及位置:`src/core.rs`: `effective_cutoff`。

- 对事件时段使用 `max(official_end)`,再与全局 safety cutoff 取既有的 min;
- 空时段继续使用默认截止时间;
- 不依赖 session_number 或 Vec 存储顺序。

测试:顺序、逆序、session_number 与时间顺序不一致、空时段。

### 5.3 M7:joined event 查询使用显式别名

涉及位置:`src/storage.rs`: `map_event_offset` 及所有带前缀列后拼接 `e.*` 的查询。

修改要求:

1. 为事件列建立单一、显式的 SELECT 投影,例如 `e.id AS event_id`,其余字段使用唯一 `event_*` 别名。
2. 新 mapper 按别名读取,不得使用裸 `row.get("id")`,因为查询中存在 `o.id`/`s.id`/`e.id` 重名。
3. 所有 joined 查询改用相同投影;普通 `SELECT * FROM ipo_events` 的 `map_event` 可保留。
4. 删除或停止使用 `map_event_offset`;不得同时保留两个易漂移实现。

测试:

- joined 查询前缀列顺序变化时事件映射仍正确;
- delivery id 与 event id 故意不同,确认没有串列;
- 所有 event 字段、sessions JSON、枚举和可空字段完整映射;
- 既有 delivery/secondary delivery 查询结果不变。

批次 5 完成门槛:定向测试、`rtk cargo fmt`、`rtk cargo test` 通过。

## 9. 暂缓项和明确维持现状

以下内容不是实施批次。执行 AI不得为了“全部清零”擅自修改。

### 9.1 M10:显式空值需要三态模型

- 暂不修改 `Candidate` 的 `Option<T>` 合并行为;
- 只有在确认数据源提供明确撤回语义并设计 `Missing / Value / ExplicitNull` 后另立任务;
- 不得利用现有 `field_sources` 猜测 None 是显式清空。

### 9.2 M11b:崩溃上传 exactly-once

- 先确认服务端是否按 `reportSha256` 幂等去重;
- 若支持,补充契约测试/文档;
- 若不支持,保留受 24h 三次限流约束的 at-least-once,不要发送前标成功造成漏报;
- 客户端原子文件替换不能消除 HTTP 成功后本地落盘失败的不确定窗口。

### 9.3 M12:更新 helper TOCTOU/吊销策略

- 可选加固,本计划不默认实施;
- spawn 前重复验签不能消除校验与执行之间的全部窗口;
- `WTD_REVOKE_NONE` 的取舍可写入安全文档,改变吊销策略前需评估离线环境和更新可用性。

### 9.4 L3:未知结果计入二级通知小时配额

- 维持现状;
- 请求发送前记录的 `success=-1` 继续计入滚动配额,防止崩溃重启后重复轰炸;
- 只允许增加「结果未知」诊断展示,不得放宽配额。

### 9.5 L13:150ms 可见性确认竞态

- 维持现有 at-least-once 语义;
- 不在退出时把所有 Leased 强制标记成功;
- 不同步冻结 UI 等待 150ms。

## 10. 已否决方案

执行过程中不得重新引入:

- M1:把 reqwest blocking `.timeout(45s)` 当作整段请求/下载总时限;
- 把 blocking `ClientBuilder` 改成不存在的 `read_timeout` API;
- 将全部 SQLite 事务机械改为 IMMEDIATE;
- 用单一 `Mutex<Connection>` 包裹整个 Database;
- 退出超时后 detach 或强杀 runtime;
- M14 对 `temp`、`updates` 或系统 `%TEMP%` 做无边界递归删除;
- 用 `char::is_control()` 过滤全部 XML 控制字符;
- 对任意非法 UTF-8 响应盲目尝试 GBK;
- 用发送前标成功换取崩溃上报 at-most-once;
- 修改 L3/L13 的既定保守语义。

## 11. 执行记录模板

每完成一个批次,在最终报告或单独执行记录中填写:

```text
批次:
完成任务:
修改文件:
定向测试:
cargo fmt:
cargo test:
文档/RELEASE_NOTES:
release build:
EXE:
MSI:
portable ZIP:
release-manifest.json:
SHA256SUMS.txt:
layout validation:
已有/新增警告:
跳过项及原因:
剩余风险:
```

全部批次结束时再运行 `rtk git status --short` 和只读 diff 审查,确认没有无关修改、临时测试文件或遗漏的生成产物。

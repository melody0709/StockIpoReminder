# stock_new 深度审查修复计划

- 状态：已完成
- 基线：`abb40e1` / `0.3.1`
- 日期：2026-08-27
- 范围：当前 Rust、Slint、SQLite、Windows 集成与发布链路
- 原则：保留现有用户改动；不改变“只提醒、不自动申购”的产品边界；所有网络输入、数据库写入和后台线程都必须可失败而不静默丢任务或终止进程。

## 1. 修复目标

### P0：远端输入不得终止进程

1. JSONP 括号顺序异常时返回解析错误，不执行越界切片。
2. `Retry-After` 只接受有界的非负延迟，使用 checked 时间运算。

### P1：后台服务与通知必须可恢复

1. 后台运行循环将单次数据库、同步、投递或健康摘要错误隔离到本轮，记录健康状态并按有界退避继续；只有显式 Stop/通道断开才退出。
2. 健康模型使用实际运行时心跳，并把心跳过期纳入 Warning/Failed，避免后台线程死亡后仍显示健康。
3. Watchdog 写崩溃报告失败时记录警告并继续执行重启策略。
4. 第二通知通道 Outbox 的身份包含 provider；切换服务商后取消旧 provider，并为新 provider 建立独立待发记录，不再被 CANCELLED 行占用唯一键。
5. SSE 官方公告和巨潮镜像结果取并集并稳定去重，不丢任一成功来源。

### P2：边界、原子性和数据可信度

1. 普通数据/公告响应流式读取并设置解压后大小上限；所有自动重定向逐跳验证 HTTPS 与白名单。
2. `MultiSourceVerified` 按不同来源计数；扩展关键业务字段冲突检测。
3. SSE/CNINFO 公告按有界页数翻页；持久化不再固定截断 5 条，并对上游截断产生健康警告。
4. 健康摘要只在文本生成并成功放入 UI 事件队列后标记已发送；失败允许当天重试。
5. 确认、撤销确认和人工覆盖在同一 SQLite 事务内完成版本校验与写入。
6. 设置保存按可回滚顺序执行；SQLite 设置和提醒重算在同一事务内，注册表失败时恢复数据库设置；凭据更新采用提交/回滚保护。
7. 更新检查和安装各自增加并发门闩；更新下载使用本次操作唯一临时文件。
8. 更新/卸载 helper 无法打开父进程时 fail closed，不继续覆盖或删除正在运行的程序。
9. 日志脱敏覆盖 UNC、Windows 正斜杠绝对路径和 `--data-root` 值。
10. 设置与会话 JSON 损坏时返回可诊断错误，不静默切换整套默认配置。

### P3：交互和调度完整性

1. 提醒批处理使用显式展示优先级，最终截止提醒不被 DataChanged 文本覆盖。
2. 原生 Toast 携带事件激活参数；多股票批次明确只打开任务列表，托盘气泡不伪装成某只股票的深链。
3. 定点补做遵守自动同步窗口；退出时使用可中断的网络请求/同步边界，避免逐来源和逐公告串行等待数分钟。
4. 时钟探测与业务数据白名单拆分，职责与重定向规则一致。
5. 移除确认的死代码并为窗口恢复路径保留 revision 驱动刷新。

## 2. 数据库迁移

- schema 从 v9 升至 v10。
- 将 `secondary_notification_outbox` 的唯一约束从 `reminder_outbox_id` 调整为 `(reminder_outbox_id, provider)`。
- 迁移在事务内创建新表、复制兼容数据、重建索引并替换旧表；保留历史 DELIVERED/EXHAUSTED/CANCELLED 记录。
- 新安装的基础 schema 直接使用 v10 结构。

## 3. 回归测试

必须新增或扩展以下自动测试：

- 反序 JSONP 与巨大 `Retry-After` 不 panic。
- 白名单重定向逐跳拒绝、解压后超限响应拒绝。
- 后台循环一次 SQLite/发送错误后继续运行并更新心跳。
- provider A → provider B 后同一提醒可以为 B 重新入队。
- SSE 官方/镜像并集和多页公告去重。
- 单来源重复候选不会标为多源验证；新增关键字段冲突会标为冲突。
- 健康摘要投递失败不会抢先写 sent 标记。
- stale runtime heartbeat 进入 Warning/Failed。
- 设置、确认和人工覆盖的版本/失败回滚。
- 更新安装门闩与唯一临时文件。
- UNC、正斜杠和 `--data-root` 脱敏。
- Final 与 DataChanged 同批时 Final 优先展示。

## 4. 验收门禁

1. `rtk cargo fmt`
2. `rtk cargo test --locked`
3. `rtk cargo clippy --locked --all-targets`；既有非本次引入 lint 单独记录，不以批量无关重构掩盖功能修复。
4. 更新 `README.md`、`RELEASE_NOTES.md` 和涉及的运维文档。
5. `rtk cmd /c build.bat --package`
6. `rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1`
7. 核对 EXE、MSI、便携 ZIP、manifest 和 SHA256SUMS 的时间与哈希均来自当前源码。

## 5. 完成记录

### 实际修改

- P0/P1 项已全部修复：JSONP 和 `Retry-After` 不再 panic；后台运行循环、健康心跳和 Watchdog 可恢复；第二通知服务商切换不再丢提醒；`43xxxx` 正确识别为北交所；SSE 官方与巨潮镜像公告取并集；健康摘要仅在成功入队后标记。
- P2/P3 项已完整落地：网络响应有界读取、逐跳 HTTPS/白名单重定向、时钟探针域名隔离、公告 10 页保护、不同来源计数和扩展冲突检测、设置/确认/人工覆盖原子性、SQLite/DPAPI/Run 跨存储回滚、更新并发门闩与 UUID 安装路径、helper fail closed、路径脱敏、损坏 JSON 报错、Toast 事件路由、提醒优先级、窗口恢复刷新、自动同步窗口和退出取消边界。
- SQLite schema 从 v9 升至 v10；迁移和新建库均使用 `(reminder_outbox_id, provider)` 唯一身份。
- 用户文档和发布说明已同步到最终行为；应用版本保持 `0.3.1`。

### 偏离与边界

- 没有未完成的计划项。
- Toast 激活处理覆盖应用仍驻留托盘时的进程内点击路由；本次没有声称或新增进程退出后的 COM 激活重启。
- 未做与审查无关的大规模 lint 重构，以避免扩大本次可靠性修复的回归面。

### 验证结果

- `rtk cargo fmt`：通过。
- `rtk cargo fmt -- --check`：通过。
- `rtk cargo test --locked`：115 passed，0 failed，0 ignored。
- `rtk cargo clippy --locked --all-targets`：0 errors，41 warnings。警告均为非阻断风格/复杂度项，主要包括 Windows 条件编译分支中的多余 `return`、可折叠 `if`、参数数量、枚举体积和引用写法；未发现安全或正确性错误。
- `rtk git diff --check`：通过。
- `rtk cmd /c build.bat --package`：通过；release 构建成功，MSI 构建 0 warnings / 0 errors，打包过程中再次执行 115 项测试并全部通过。
- `rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1`：通过，`runtimeFiles=4`。
- 独立 SHA-256 复算：便携 ZIP、MSI 和 `release-manifest.json` 均与 `SHA256SUMS.txt` 一致。

### 发布产物

- 生成时间：`2026-08-27T14:05:59Z`。
- 可运行 EXE：`build\run\x64-release\StockIpoReminder.exe`，15,491,072 bytes，SHA-256 `0cf410100f1bcca23251ba6bfdd9acdb40dea984af670747597b1b170deceff1`。
- 便携 ZIP：`build\packages\0.3.1\StockIpoReminder-0.3.1-win-x64-portable.zip`，7,718,040 bytes，SHA-256 `1e6701b1edb377e6fb1124a71bc262c2c60b1609cf0cf7547d8bde458a4f5910`。
- MSI：`build\packages\0.3.1\StockIpoReminder-0.3.1-win-x64.msi`，6,369,280 bytes，SHA-256 `727b4f31fd97abea8dd2a356ea45a5358ca40ab5c26c98be7126aefa24c900ef`。
- Manifest：`build\packages\0.3.1\release-manifest.json`，SHA-256 `8bccd8b110edb1a632b65a206c6b9116dc6eb69f400a7e7d0138cabd7f5e2294`。
- 校验和：`build\packages\0.3.1\SHA256SUMS.txt`。
- 当前本地发布未配置正式签名证书、更新源或崩溃上报端点，manifest 明确记录 `signed: false`；这是既有发布配置，不影响本次未签名包的构建与布局验收。

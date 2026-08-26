# A 股新股申购提醒

一个常驻 Windows 系统托盘的 A 股新股申购提醒程序。它从公开渠道发现沪市、深市和北交所申购任务，并持续提醒，直到你对每只股票分别完成“确认已申购”的二次确认。

当前版本：`0.3.1`

正式运行版本已经完全迁移到 Rust：界面使用 Slint，Windows 托盘、原生 Toast、单实例、声音和当前用户开机自启动注册使用 `windows-rs`，数据层使用内嵌 SQLite。MSI 安装包和便携包都不依赖 .NET Runtime，仓库也已移除旧 C#/.NET 工程。

程序启动时自动同步一次；06:00–22:00 内普通日期默认每 30 分钟同步，当日存在未确认或待复核申购任务时默认每 10 分钟同步，并在申购日前一日 20:00、申购日 08:00 做定点核验。普通轮询和申购日轮询分别加入受控抖动，错过定点后会立即补做。PDF 由同一 Rust EXE 的短生命周期 Worker 顺序解析；Worker 使用面向文字提取的对象过滤、20 页和 256,000 字符上限，并由 Windows Job Object 限制为最多 512 MiB 进程内存。Worker 超时、崩溃或触发内存上限只会使对应公告受控降级，完成后退出，因此同步期的响应、正文和 PDF 解析内存不会留在常驻主进程中。

## 能做什么

- 获取沪市、深市、创业板、科创板和北交所公开发行信息。
- 每只新股独立维护提醒和人工确认状态。
- 使用独立置顶提醒小窗，不强制恢复主窗口；同一股票的多个到期提醒会合并，多股票同时到期时仍要求逐只打开和确认。
- 程序恢复或长时间离线后，同一股票已过期的申购提醒会折叠到最新到期层级；提醒窗口持续无法显示时按 1、5、15、30 分钟退避，并在健康页显示错误。
- 系统通知优先使用 Windows Toast；安装版通过开始菜单快捷方式注册稳定 AUMID，系统权限、安装注册或当前呈现状态异常时自动回退托盘气泡。
- 可选把同一批到期提醒发送到企业微信、钉钉、飞书或 PushPlus；凭据由 Windows 当前用户加密并使用独立持久化重试队列。
- 再次启动同一数据目录的程序会唤醒已有实例并显示主窗口，不会只静默退出。
- 默认白天每小时提醒；临近安全截止时间升级为每 15、5、2 分钟提醒。
- 用户确认已申购后，可按公开数据中的中签结果日和缴款日继续收到查询/资金提醒；程序不会读取券商账户、推断是否中签或判断是否完成缴款。
- 用户确认已申购且公开数据给出上市日后，可在上市日 08:30 收到本地提醒；提醒不读取持仓、不跟踪收益，也不保证证券当日一定可正常交易。
- 今日任务与未来 60 天列表支持按简称/代码、市场和状态组合筛选，并使用虚拟化列表避免大量任务时一次创建全部卡片。
- 关闭窗口、Toast 消失、锁屏、休眠或重启程序都不会自动完成任务。
- 申购代码、日期、价格、上限、单位、市值/资金要求、发行状态或官方申购时段/资金规则变化后，旧确认进入“待复核”，恢复提醒并要求重新确认。
- 简称、历史代码、中签/缴款/上市日期或公告链接等非关键字段变化会发送一次普通变更提示，不会误使旧确认失效。
- 使用 SQLite、持久化提醒队列和来源健康状态；只有启用市场的必要来源本轮全部成功时才会显示“今日无新股”，否则明确提示来源覆盖不完整并保留已有任务。
- 保存正式公告、字段来源和解析证据；自动解析失败时明确标记为“待人工核验”。

## 明确不做什么

- 不登录券商。
- 不保存券商账号、密码或交易凭据。
- 不读取持仓、市值或可申购额度。
- 不调用自动申购或下单接口。
- 不验证券商是否已受理委托。

“确认已申购”只代表你在本程序中的人工确认，不代表券商订单成功。该操作仅在任务所列申购日当天可用；未来 60 天列表中的待申购任务不能提前确认。升级后若检测到旧版本遗留的未来确认记录，程序会自动撤销该无效确认并恢复提醒。中签/缴款提醒也只是提示你自行查询券商和正式公告，具体资金到账规则始终以券商和发行公告为准。

## 数据渠道

程序采用分层数据链路：

1. 东方财富公开接口用于全市场候选发现。
2. 上交所、巨潮资讯/深交所披露域名、北交所公开接口用于市场核验。
3. 本次正式发行公告用于裁决申购日期、代码、价格、上限、时段和发行状态。

沪市公告保留上交所官方检索，并在上交所 PDF 直链触发 JavaScript 验证时优先使用巨潮资讯的沪市公告镜像；镜像文档仍归入 `sse-announcement`，不会把备用通道误显示成另一个市场来源。

运行时不会把任何单一第三方项目、AKShare 或 Tushare 作为唯一数据依赖。公开网页接口可能改版、限流或返回 WAF 页面，因此程序具有来源级退避、缓存、契约检查和人工核验降级；HTTP 429/503 的 `Retry-After` 会优先于本地退避，本地 1/2/4/8/15/30 分钟指数退避带有最多 10% 抖动。退避持续较久时会低频探测站点可达性，但不会因探测成功而提前绕过 API 退避。

### 受控降级

- 某个来源失败不会清空其他来源已经发现的任务。
- 公告 URL 即使以 `.pdf` 结尾，也必须通过正文类型和 `%PDF-` 文件签名检查。
- HTML/WAF 错误页不会保存成公告 PDF。
- 公告检索、下载和重定向后的最终地址都必须位于公告域名白名单；上游 JavaScript 验证页会被明确拒绝。
- 公告结果优先处理正式发行公告，并排除律师意见、核查报告、保荐书、批复和招股书附录；取得可用正式证据后不再下载冗余大附件。
- 同一公告来源按整轮任务汇总健康状态：部分事件或备用路径异常显示“警告”，全部检索或正文处理失败才显示“失败”。
- 采集器同时审计上游声明数量、返回明细数量和通过身份校验的数量；数量不一致的来源不能参与“完整覆盖”结论。
- 来源过期阈值会随用户配置的自动同步间隔调整；没有相关近期任务的公告来源不会仅因长期未调用而误报失败。
- 临近申购日但没有可用正式公告时，任务进入“待人工核验”，保留官方链接，不会假装已经核验。
- 所有来源异常时，不会错误显示成“今日无新股”。

## 系统要求

- x64 Windows。
- 当前目标最低版本为 Windows 10 2004（Build 19041）。
- 应用声明 Per-Monitor V2 DPI 感知；在不同缩放比例的显示器之间移动窗口时会重新缩放，并在小于 1000×650 逻辑像素时自动使用紧凑布局。最低窗口客户区为 760×460。
- Windows 11 Build 26200 已作为当前开发和发布验证环境；正式声明 Windows 10 支持前仍需补对应系统 smoke 证据。

## 安装版

运行：

```text
StockIpoReminder-0.3.1-win-x64.msi
```

默认目录：

- 程序：`%ProgramFiles%\StockIpoReminder`（安装时可点击“更改目录”选择其他位置）
- 数据：`%LocalAppData%\StockIpoReminder`

MSI 是 64 位按计算机安装，写入 Program Files 时会请求管理员权限；升级会记住上次选择的安装目录。开始菜单快捷方式写入 `StockIpoReminder.Desktop` AUMID，供 Windows Toast 识别。程序启动或保存设置时，会按“登录 Windows 后自动启动”选项写入当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，并清理旧版本遗留的同名计划任务。

MSI 使用 Major Upgrade 完成升级，并由 Windows Installer 提供程序文件事务回滚。用户数据始终保存在 `%LocalAppData%\StockIpoReminder`，不会随程序目录升级而移动；新版本第一次打开旧数据库时，会在任何 schema 迁移前创建并校验 SQLite 备份，备份失败则停止启动而不继续迁移。

普通 MSI 卸载只删除程序文件、开始菜单入口和安装目录记录，默认保留数据库、设置、公告缓存和备份。设置页在检测到本产品的 Windows Installer 注册后会提供“卸载程序”入口；默认仍保留数据，也可明确选择在 MSI 成功卸载后删除当前用户数据。删除数据必须准确输入 `删除当前用户数据`，卸载助手只接受当前用户默认的 `%LocalAppData%\StockIpoReminder`，不会枚举或删除其他 Windows 用户目录；MSI 卸载失败时不会删除数据。

## 便携版

解压 `StockIpoReminder-0.3.1-win-x64-portable.zip` 后直接运行 `StockIpoReminder.exe`。便携版没有 Windows Installer 注册，因此设置页不会启用 MSI 卸载和自动更新入口；退出程序后直接删除便携文件即可，用户数据仍按下述规则独立保留。

便携版如果启用“登录 Windows 后自动启动”，会按当前可执行文件位置写入当前用户 Run 注册项。默认数据仍保存在 `%LocalAppData%\StockIpoReminder`，因此移动或删除便携程序后，应先关闭自启动或重新保存设置以更新路径。便携运行通常没有 MSI 创建的开始菜单 AUMID 注册；设置页会明确显示 Toast 诊断结果，Toast 无法提交时仍使用托盘气泡回退。

## 数据、备份和诊断

默认数据目录包含：

```text
stock-ipo-reminder.db     SQLite 主数据库
announcements\            公告文件
backups\                  自动备份和升级前备份
logs\                     轮转日志
diagnostics\              用户导出的脱敏诊断包
diagnostics\crashes\      Watchdog 异常退出与重启报告
diagnostics\crash-upload-state.json       本地发送去重与限流状态
diagnostics\crash-upload-last-result.json 最近一次发送结果
secrets\secondary-notification.dpapi.json 当前 Windows 用户加密的第二通道凭据
application-version.txt   已成功打开数据库的应用版本标记
```

程序每小时独立检查每日数据库备份，默认保留最近 7 份；备份使用唯一临时文件、SQLite 完整性检查、写盘刷新和最终提交，失败时清理临时文件并保留既有备份。日志按日期保存、单日按大小分段并保留 14 天。诊断包 schema v4 默认不包含数据库、公告全文、原始接口响应或第二通知通道凭据，会脱敏 URL 查询参数、Cookie、Authorization、临时目录和工作区绝对路径，并附带近期同步、来源/运维健康、本地与第二通知 Outbox、提醒日志、Windows Toast/AUMID 状态和最近的 Watchdog 崩溃报告。

正常启动会由同一 EXE 的轻量 Watchdog 监督主程序。主程序异常退出时最多在 10 分钟窗口内重启 3 次，并按 2、10、30 秒退避；正常“安全退出”不会重启。它不是 Windows Service，关机、退出登录、同时结束 Watchdog 与主程序或 Watchdog 自身被结束时无法继续拉起。

用于隔离测试或故障排查时，可以显式指定数据目录：

```text
StockIpoReminder.exe --data-root "D:\Temp\StockIpoReminder-Test"
```

也可以使用环境变量 `STOCK_IPO_REMINDER_DATA_ROOT`。命令行参数优先于环境变量。不同数据目录使用不同的单实例互斥量和 Run 注册值名称，避免测试污染正式数据。

`--skip-startup-sync`、`--skip-auto-start-registration`、`--skip-update-check`、`--skip-crash-upload`、`--no-watchdog`、`--self-test-report <path>` 和 `--exit-after-seconds <n>` 是发布 smoke 使用的参数，不建议日常使用。

## 默认提醒规则

- 安全截止时间：14:55，可在首次设置或设置页修改，但不能晚于正式公告的官方结束时间。
- 上午开盘前、11:20、12:55 有边界提醒。
- 截止前 60–30 分钟：每 15 分钟。
- 截止前 30–10 分钟：每 5 分钟。
- 最后 10 分钟：每 2 分钟。
- 到达安全截止时间后记录“截止时仍未确认”，不整夜持续弹窗。
- 确认已申购且公开数据给出日期后：中签结果日 18:00 提醒查询；缴款日 08:30 和 14:00 提醒核对中签与资金。这些时间是本地提示点，不是对交易所或券商截止时间的推断。

北交所任务会额外提示全额缴付申购资金和早盘优先处理，不套用沪深市值申购文案。

## 签名与安全自动更新

仓库已提供 Authenticode、RFC 3161 时间戳、detached CMS 更新清单、证书指纹固定和 CI 手动签名发布主路径。正式签名构建必须同时配置具有 Code Signing EKU 的证书和 HTTPS 稳定版更新清单地址；应用只对已安装的 MSI 版本开放自动更新，便携版继续手动更新。

客户端会依次验证 HTTPS、CMS 清单签名、固定证书 SHA-256、产品和 stable 通道、递增版本、最低 Windows Build、安装包大小与 SHA-256，以及 MSI Authenticode 信任和相同证书指纹。只有全部通过并由用户明确点击后才调用 Windows Installer；下载或验证失败不会启动安装。

当前本机生成的 `0.3.1` 基线没有接入正式 CA 证书，因此发布清单仍明确记录 `signed: false`，设置页会显示自动更新未配置。Windows SmartScreen 或安全软件仍可能显示“未知发布者”；请核对 `SHA256SUMS.txt`。正式签名和更新源部署见 `docs/release-signing-and-updates.md`。

## 可选崩溃报告共享

崩溃报告共享默认关闭。只有发布构建同时嵌入无凭据 HTTPS 接收地址和 HTTPS 隐私政策地址时，设置页才允许用户启用“异常退出后自动发送”或明确点击“发送最近报告”。没有配置服务时，Watchdog JSON 只保留在本机。

客户端只读取 `diagnostics\crashes` 顶层、名称符合 `crash-recovery-*.json` 且不超过 128 KiB 的文件。发送前会再次递归移除命令行、路径、目录、用户名、主机、设备、账户、持仓、Cookie、Authorization、token、密码和凭据字段，并对剩余文本执行脱敏；不会附带数据库、日志、公告正文或稳定设备标识。过去 24 小时最多尝试 3 次，同一报告成功后不会重复发送。部署要求和服务端仍需落实的保留/删除策略见 `docs/crash-reporting.md`。

## 可选第二通知通道

设置页可以选择企业微信机器人、钉钉机器人、飞书机器人或 PushPlus。企业微信、钉钉和飞书只接受对应官方域名及固定路径格式的无凭据 HTTPS Webhook；PushPlus token 只发送到固定的 `www.pushplus.plus` HTTPS 接口。所有请求禁止重定向、总超时 15 秒，并要求成功 HTTP 状态和服务商 JSON 成功代码同时满足。

Webhook 或 token 不写入 SQLite、日志、诊断 ZIP、命令行或发布清单，而是通过 Windows DPAPI 绑定当前用户加密保存在 `secrets\secondary-notification.dpapi.json`。切换 Windows 用户或复制到其他电脑后不能解密，需要重新填写。普通卸载默认保留数据，因此也会保留该加密文件；可以先在设置页清除凭据，或使用明确确认的数据清理卸载。

第二通道使用独立于桌面提醒的持久化队列。失败后按 1、5、15、30 分钟退避，单条最多尝试 5 次；自动批次和用户测试合计每小时最多 20 个请求。请求尝试记录保留 30 天且最多 2,000 条，已完成、取消或耗尽的远程队列项保留 90 天。发送内容只包含公开的新股简称、申购代码、提醒类型、到期时间和脱敏提示，不读取账户、持仓或券商委托状态。详细安全与隐私边界见 `docs/secondary-notifications.md`。

## 开发与验证

正式版使用 Rust/Cargo。所有生成内容统一写入 `build/`，日常构建、清理和打包入口为根目录 `build.bat`：

```powershell
rtk cmd /c build.bat
rtk cmd /c build.bat --rebuild
rtk cmd /c build.bat --package
rtk cmd /c build.bat --package --sign
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-release.ps1
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-signing-update.ps1
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/audit-release.ps1
```

唯一可直接运行的开发版本位于 `build\run\x64-release\StockIpoReminder.exe`。Cargo 缓存和测试二进制位于 `build\cargo`；MSI 与便携包位于 `build\packages\<version>`。

日常测试使用四来源和正式公告的固定真实响应裁剪，不把随机网络访问混入单元测试。联网端到端验收必须使用独立 `--data-root`，不得污染正式数据。

当前 Rust 固定 fixture、SQLite 迁移、字段来源、公告关联、确认与 Outbox 恢复、同步调度、来源覆盖结论、退避/探测、备份、诊断、版本升级保护、安全卸载、更新清单、崩溃报告隐私约束和第二通知通道安全边界回归测试共 74 项。

## 仓库结构

```text
Cargo.toml / Cargo.lock   Rust 正式项目与锁定依赖
.cargo/config.toml        将 Cargo 生成目录固定到 build/cargo
build.bat                 构建、清理和 MSI/便携包统一入口
build/                    全部本地生成输出；仅 README.txt 纳入版本控制
src/                      应用、同步、存储、PDF、部署和 Windows 集成
ui/                       Slint 界面
assets/                   Windows 应用图标
tests/fixtures/           四来源与正式公告的离线固定响应样本
scripts/                  构建、smoke、发布审计和内存测量
packaging/windows/        WiX MSI 项目、可选目录界面和稳定升级标识
```

完整迁移、PDF Worker、内存结果和发布闸门见 `.plan/feat/memory-footprint-pdf-rust.md`；产品设计基线见 `.plan/feat/windows-ipo-reminder.md`。

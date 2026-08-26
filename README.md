# A 股新股申购提醒

一个常驻 Windows 系统托盘的 A 股新股申购提醒程序。它从公开渠道发现沪市、深市和北交所申购任务，并持续提醒，直到你对每只股票分别完成“确认已申购”的二次确认。

当前版本：`0.2.2`

正式运行版本已经完全迁移到 Rust：界面使用 Slint，Windows 托盘、通知、单实例、声音和当前用户开机自启动注册使用 `windows-rs`，数据层使用内嵌 SQLite。MSI 安装包和便携包都不依赖 .NET Runtime，仓库也已移除旧 C#/.NET 工程。

程序启动时自动同步一次，之后按设置中的自定义间隔自动同步（默认 24 小时），并保留“立即同步”。PDF 由同一 Rust EXE 的短生命周期 Worker 解析；Worker 完成后退出，因此同步期的响应、正文和 PDF 解析内存不会留在常驻主进程中。

## 能做什么

- 获取沪市、深市、创业板、科创板和北交所公开发行信息。
- 每只新股独立维护提醒和人工确认状态。
- 默认白天每小时提醒；临近安全截止时间升级为每 15、5、2 分钟提醒。
- 关闭窗口、Toast 消失、锁屏、休眠或重启程序都不会自动完成任务。
- 关键申购字段变化后撤销旧确认，恢复提醒并要求重新确认。
- 使用 SQLite、持久化提醒队列和来源健康状态，区分“今天没有新股”与“数据源没有正常工作”。
- 保存正式公告、字段来源和解析证据；自动解析失败时明确标记为“待人工核验”。

## 明确不做什么

- 不登录券商。
- 不保存券商账号、密码或交易凭据。
- 不读取持仓、市值或可申购额度。
- 不调用自动申购或下单接口。
- 不验证券商是否已受理委托。

“确认已申购”只代表你在本程序中的人工确认，不代表券商订单成功。

## 数据渠道

程序采用分层数据链路：

1. 东方财富公开接口用于全市场候选发现。
2. 上交所、巨潮资讯/深交所披露域名、北交所公开接口用于市场核验。
3. 本次正式发行公告用于裁决申购日期、代码、价格、上限、时段和发行状态。

沪市公告保留上交所官方检索，并在上交所 PDF 直链触发 JavaScript 验证时优先使用巨潮资讯的沪市公告镜像；镜像文档仍归入 `sse-announcement`，不会把备用通道误显示成另一个市场来源。

运行时不会把任何单一第三方项目、AKShare 或 Tushare 作为唯一数据依赖。公开网页接口可能改版、限流或返回 WAF 页面，因此程序具有来源级退避、缓存、契约检查和人工核验降级。

### 受控降级

- 某个来源失败不会清空其他来源已经发现的任务。
- 公告 URL 即使以 `.pdf` 结尾，也必须通过正文类型和 `%PDF-` 文件签名检查。
- HTML/WAF 错误页不会保存成公告 PDF。
- 公告检索、下载和重定向后的最终地址都必须位于公告域名白名单；上游 JavaScript 验证页会被明确拒绝。
- 公告结果优先处理正式发行公告，并排除律师意见、核查报告、保荐书、批复和招股书附录；取得可用正式证据后不再下载冗余大附件。
- 同一公告来源按整轮任务汇总健康状态：部分事件或备用路径异常显示“警告”，全部检索或正文处理失败才显示“失败”。
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
StockIpoReminder-0.2.2-win-x64.msi
```

默认目录：

- 程序：`%ProgramFiles%\StockIpoReminder`（安装时可点击“更改目录”选择其他位置）
- 数据：`%LocalAppData%\StockIpoReminder`

MSI 是 64 位按计算机安装，写入 Program Files 时会请求管理员权限；升级会记住上次选择的安装目录。程序启动或保存设置时，会按“登录 Windows 后自动启动”选项写入当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`，并清理旧版本遗留的同名计划任务。

MSI 使用 Major Upgrade 完成升级，并由 Windows Installer 提供事务回滚。用户数据始终保存在 `%LocalAppData%\StockIpoReminder`，不会随程序目录升级而移动。

普通 MSI 卸载只删除程序文件、开始菜单入口和安装目录记录，默认保留数据库、设置、公告缓存和备份。

## 便携版

解压 `StockIpoReminder-0.2.2-win-x64-portable.zip` 后直接运行 `StockIpoReminder.exe`。

便携版如果启用“登录 Windows 后自动启动”，会按当前可执行文件位置写入当前用户 Run 注册项。默认数据仍保存在 `%LocalAppData%\StockIpoReminder`，因此移动或删除便携程序后，应先关闭自启动或重新保存设置以更新路径。

## 数据、备份和诊断

默认数据目录包含：

```text
stock-ipo-reminder.db     SQLite 主数据库
announcements\            公告文件
backups\                  自动备份和升级前备份
logs\                     轮转日志
diagnostics\              用户导出的脱敏诊断包
```

程序默认保留最近 7 份自动数据库备份。诊断包默认不包含公告全文和原始接口响应，并会脱敏 URL 查询参数、Cookie、Authorization、临时目录和工作区绝对路径。

用于隔离测试或故障排查时，可以显式指定数据目录：

```text
StockIpoReminder.exe --data-root "D:\Temp\StockIpoReminder-Test"
```

也可以使用环境变量 `STOCK_IPO_REMINDER_DATA_ROOT`。命令行参数优先于环境变量。不同数据目录使用不同的单实例互斥量和 Run 注册值名称，避免测试污染正式数据。

`--skip-startup-sync`、`--skip-auto-start-registration`、`--self-test-report <path>` 和 `--exit-after-seconds <n>` 是发布 smoke 使用的参数，不建议日常使用。

## 默认提醒规则

- 安全截止时间：14:55，可在首次设置或设置页修改，但不能晚于正式公告的官方结束时间。
- 上午开盘前、11:20、12:55 有边界提醒。
- 截止前 60–30 分钟：每 15 分钟。
- 截止前 30–10 分钟：每 5 分钟。
- 最后 10 分钟：每 2 分钟。
- 到达安全截止时间后记录“截止时仍未确认”，不整夜持续弹窗。

北交所任务会额外提示全额缴付申购资金和早盘优先处理，不套用沪深市值申购文案。

## 未签名风险

`0.2.2` 发布物尚未进行 Authenticode 代码签名。Windows SmartScreen 或安全软件可能显示“未知发布者”或要求额外确认。请只使用本项目发布目录中的文件，并在运行前核对 `SHA256SUMS.txt`。

发布清单会明确记录 `signed: false`。代码签名属于后续增强，不会通过隐藏警告来伪装成已签名版本。

## 开发与验证

正式版使用 Rust/Cargo。所有生成内容统一写入 `build/`，日常构建、清理和打包入口为根目录 `build.bat`：

```powershell
rtk build.bat
rtk build.bat --rebuild
rtk build.bat --package
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-release.ps1 -Version 0.2.2
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/audit-release.ps1 -Version 0.2.2
```

唯一可直接运行的开发版本位于 `build\run\x64-release\StockIpoReminder.exe`。Cargo 缓存和测试二进制位于 `build\cargo`；MSI 与便携包位于 `build\packages\<version>`。

日常测试使用四来源和正式公告的固定真实响应裁剪，不把随机网络访问混入单元测试。联网端到端验收必须使用独立 `--data-root`，不得污染正式数据。

当前 Rust 固定 fixture、SQLite 状态迁移、字段来源、公告关联、确认撤销和人工覆盖回归测试共 23 项。

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

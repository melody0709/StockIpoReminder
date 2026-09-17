# 变更日志

本文件按版本倒序记录本仓库的变更，用于回答「改了什么、什么时候改的、影响面在哪」。

## 维护约定

- 每次源码、UI、脚本、打包或发布流程变更后，先在 `[未发布]` 小节登记，说明变更类型（新增 / 变更 / 修复 / 移除 / 安全）和影响面。
- 版本定稿并跑完 `rtk cargo fmt`、`rtk cargo test`、`build.bat --package`、布局校验后，把 `[未发布]` 小节改名为 `## <版本> — <日期>`，再另起一个空的 `[未发布]` 小节。
- 版本号只以 `Cargo.toml` 为准，本文件不重复维护第二个版本来源。
- `RELEASE_NOTES.md` 面向用户，记录「用户能感知到什么」；本文件面向维护者，额外记录内部重构、脚本和测试等工程变更。
- 只追加、不改写历史条目；历史版本如确需更正，追加说明而不是覆盖原文。

---

## [未发布] — 0.3.8（草稿）

> 状态：源码、UI、脚本、文档与打包产物已提交（`9da808d`）并推送到 `main`；**GitHub Release `v0.3.8` 已发布为 stable/latest**，公开 feed 的 `update-manifest.json` 与 `.minisig` 已复核可用。
> 未完成的定义：`.plan/feat/certificate-free-minisign-auto-update.md` 第 12 节要求最后再走一次「两版真实 MSI 升级闭环」——从已安装的 0.3.8 升级到更高版本（含 UAC 取消与 `3010` 路径）。

### 变更

- **自动更新信任根迁移**：从「SignPath / CA 代码签名证书 + detached CMS + Authenticode 指纹」改为「内置 Minisign 公钥 + 预哈希签名」，`Cargo.toml` 升至 `0.3.8`，新增依赖 `minisign-verify = "0.2"`，移除三个不再使用的 `Win32_Security_Cryptography_*` feature。
- **更新清单升级为 schema v2**（`src/updater.rs` 重写）：Minisign 预哈希签名覆盖清单原始字节，拒绝 legacy 非预哈希签名；随后依次校验产品、stable 通道、严格递增版本、RFC 3339 发布时间、最低 Windows Build、安装包文件名/大小/SHA-256；下载时执行响应上限、声明长度和增量哈希三重校验。
- **下载与安装拆分为两步**：下载验证完成后落到受控 `pending` 目录并可跨进程恢复；只有用户点击「重启并更新」才退出升级。安装 helper 只接收数据目录与父进程 PID，自行重验全部材料，并以只读句柄锁定 MSI 直到 `msiexec` 结束。
- **新增 `UpdateController`**（`src/ui/background_operations.rs`）：收编原先分散的 `available_update` / `update_check_busy` / `update_install_busy` 三类共享状态，统一负责检查、下载、安装与每小时周期重查，`main.rs` 与各回调层改为持有 `Arc<UpdateController>`。
- **设置项新增**：`AppSettings::automatic_update_download_enabled`（默认 `false`），与原有「启动时及每日自动检查」解耦；自动检查按 24 小时节流，手动检查不受限，同一版本只主动提醒一次。
- **UI**：更新区域重排为两行——第一行「自动检查开关 + 检查更新 + 下载/重试 + 重启并更新」，第二行「发现更新后自动下载（默认关闭）」；新增 `update-ready` / `update-downloading` / `update-download-failed` / `update-download-progress` 属性，区域高度 148px → 216px。
- **托盘通知路由**：新增 `updater::is_update_activation`，更新类 Toast 点击后直达设置页更新分区，不再误入股票详情。
- **发布流水线**：`build.bat` 与各脚本改用 `pwsh`；`build-release.ps1` 移除 `UpdateFeedUrl` 参数与 `Write-DetachedCmsSignature`，`--sign` 退化为可选的纯 Authenticode 能力；CI 新增下载安装 minisign 0.12 的步骤。
- **CI 作用域说明**：`signed-release.yml` 增加头注释与提示步骤，明确它只做构建、可选 Authenticode 与校验，**不产出 Minisign 清单也不发布 Release**；没有代码签名证书时可跳过该工作流，改用本地 `build.bat --package`。

### 新增

- `assets/update-signing/stock-ipo-update.pub`：受信任的正式公钥（编译进 EXE，仓库外私钥签名）。
- `scripts/generate-update-signing-key.ps1`、`scripts/sign-update-manifest.ps1`：密钥生成与清单签名。
- `scripts/test-update-helper-recovery.ps1`：helper 故障恢复测试。
- `tests/fixtures/minisign/`：Minisign 测试密钥与清单 fixture（仅测试用，不被客户端信任）。
- `.gitattributes`、`docs/release-signing-and-updates.md` 重写（无证书更新边界、密钥保管、发版步骤）。

- **发布说明入口**：`updater::release_notes_url` 把签名清单中的 `releaseNotesUrl` 解析为与更新源同目录的安全 HTTPS 地址；只接受不带协议、主机和路径分隔符的相对文件名，绝对 URL、其他主机和 `../` 穿越一律拒绝。设置页据此显示「发布说明」按钮。
- **更新区域信息补全**：显示当前版本与可用版本，卡片高度 216px → 240px，新增 `update-release-notes-url` 属性与 `open-release-notes` 回调。

### 移除

- `build-release.ps1` 中的 detached CMS 签名与 `--sign` 与更新信任的绑定关系。
- `.plan/feat/zero-cost-github-msi-signpath-updates.md` 记录的 SignPath 路线作废（申请未通过），替代方案为 `.plan/feat/certificate-free-minisign-auto-update.md`。

### 验证

- `rtk cargo fmt` / `rtk cargo test`：169 项通过。
- `scripts/test-update-helper-recovery.ps1`：通过（`msiexec` 对无效包返回 1620 → helper 退出码 2、pending 与清单保留、结果文件写入）。
- `validate-build-layout.ps1`、`smoke-release.ps1`、`test-signing-update.ps1`、`audit-release.ps1`：全部通过（0.3.8 最新一轮报告时间戳 `20260917-030603`）。
- 已创建对象：`StockIpoReminder-0.3.8-win-x64.msi`、便携 ZIP、`update-manifest.json` 与其 `.minisig`、`release-manifest.json`、`SHA256SUMS.txt`、`README.md`、`RELEASE_NOTES.md`；发布清单为 `signed: false`（无 Authenticode，属预期）。
- GitHub Release `v0.3.8` 已发布为 stable/latest：上传上述八项资产 → 以重新下载的副本复核（`SHA256SUMS.txt` 全项 OK、`minisign -V` 通过、EXE `--update-bundle-self-test` 返回 `success`）→ 发布后从公开 `releases/latest/download/` 再取一次清单与签名，字节与本地签名副本一致且验签通过。
- 已知未修：`SHA256SUMS.txt` 使用 CRLF 换行，`sha256sum -c` 需先 `tr -d '\r'`；这是历史格式，本版未变更。

---

## 0.3.7 — 2026-09-02

### 移除

- 移除「任务栏按钮提示」开关、运行时闪烁调用与测试按钮：托盘常驻时无可见效果，旧设置的 `flashTaskbar` / `notificationFlashTestPassed` 被安全忽略。
- 通知测试收敛为置顶提醒窗口、Windows Toast、托盘气泡回退和声音；Toast 或气泡任一通过即满足系统通知测试。

## 0.3.6 — 2026-09-02

### 变更

- 设置页改为「常用 / 通知与测试 / 同步 / 高级与维护」四分区，分区切换不丢未保存修改。
- 「恢复默认」「保存设置」「安全退出」固定在设置页底部；通知开关改为等宽布局，通道测试改为双列（窄窗口降为单列）。
- 首次使用提示增加「去完成测试」入口，直达通知与测试分区。

## 0.3.5 — 2026-09-02

### 新增 / 变更

- 设置页底部新增「恢复默认」按钮：先回填、确认后再写入；会清除已完成的通知测试状态。
- 默认「当日未确认任务核验间隔」10 → 20 分钟；第二通知通道的操作控件改为分行排列，避免窄屏重叠。

## 0.3.4 — 2026-09-01

### 新增

- 主窗口尺寸记忆：拖拽后防抖保存（逻辑像素），托盘隐藏 / 关窗 / 安全退出时再同步；最大化与全屏不覆盖普通尺寸，损坏的 `window-state.json` 回退默认。

## 0.3.3 — 2026-09-01

### 变更

- 默认托盘启动：普通启动与开机自启都直接驻留托盘；双击托盘、托盘菜单、通知和重复启动仍显式显示主窗口。
- 主窗口工作区适配与首次重绘延后到用户第一次显式打开；smoke 新增「普通启动保持托盘隐藏」门禁。

## 0.3.2 — 2026-09-01

### 变更

- 提醒降噪：未来任务字段变化静默更新，取消预告、早间、预受理、开盘前和午休结束提醒；打新提醒只落在 09:30–11:30 与 13:00 至安全截止时间内。
- 投递层新增交易时段二次校验，已错过的提醒直接取消；健康摘要仅在确有当日申购任务时进入交易时段提醒。

### 内部

- SQLite 持久化（原 5,500+ 行）按领域拆分；应用入口与 UI、后台运行时按职责拆分，Windows 集成模块分离；行为不变的重构，无新迁移。

## 0.3.1 — 2026-08-27

### 变更

- 宽高紧凑模式解耦：宽度 < 1000、高度 < 650 逻辑像素分别触发各自的压缩策略；最低客户区 760×460 → 800×500。

### 修复 / 内部

- 修复畸形 JSONP 反序括号 panic；`Retry-After` 限 24 小时；有界流式读取（业务 16 MiB / 公告 8 MiB）；HTTPS 重定向逐跳白名单。
- 后台循环改为有界退避（1/5/15/30 秒）而非退出；调度改为单一绝对截止时间调度器，移除每秒 UI 轮询。
- 数据库维护改为每工作日一次；自动备份改为业务指纹触发（保留 7 份 / 512 MiB）；同步无字段变化时不重写表。
- 停止自动下载解析公告 PDF，移除 `lopdf` 与 PDF Worker；东方财富、上交所、北交所查询加时间窗与有界分页。
- 启动提速：数据库准备后台化并加门禁；修复 SQLite 在线备份的等待参数；schema 升至 v10，清除无用的接口正文。
- 回归测试增至 115 项。

## 0.3.0 — 2026-08-26

### 新增

- 加密的第二通知通道：企业微信 / 钉钉 / 飞书机器人与 PushPlus，默认关闭；官方域名与无凭据 HTTPS 白名单、15 秒超时、64 KiB 响应上限。
- Webhook / token 用 Windows DPAPI 绑定当前用户加密，单独存放 `secrets\secondary-notification.dpapi.json`，不进 SQLite、日志与诊断包。
- 独立 Outbox、1/5/15/30 分钟退避（单条最多 5 次）、滚动 1 小时 20 次配额。

### 验证

- SQLite schema v8；新增 7 项定向测试，回归测试增至 74 项；smoke schema v10 增加 DPAPI 与配额门禁。

## 0.2.9 — 2026-08-26

### 新增

- 用户同意的脱敏崩溃报告共享：默认关闭，需同时配置 HTTPS 接收端与隐私政策；上传前二次脱敏（移除命令行、路径、用户名、token 等），24 小时最多 3 次、成功后去重。
- 回归测试增至 67 项；smoke schema v9 增加同意、脱敏、无重定向与限流门禁。

## 0.2.8 — 2026-08-26

### 新增

- Authenticode 与签名发布链：`build.bat --package --sign` 要求 Code Signing EKU 证书、RFC 3161 时间戳与 HTTPS 更新源，签后由 `signtool verify /pa /all` 复核；PFX 密码只从环境变量读取。
- 安全自动更新：detached CMS 清单、证书指纹固定、HTTPS stable 通道、严格递增版本、大小上限与流式 SHA-256、安装前 Authenticode 校验。

## 0.2.7 — 2026-08-26

### 新增

- 安全卸载与数据保留：卸载入口保留用户数据；可选清理要求输入「删除当前用户数据」，MSI 卸载成功后才删除 `%LocalAppData%\StockIpoReminder`。
- 上市日提醒、任务列表性能与筛选、Watchdog 恢复与发布证据。

## 0.2.6 — 2026-08-26

### 变更

- Windows 通知与交互可靠性改进；中签查询与缴款资金提醒；恢复流程与发布验证门禁。

## 0.2.5 — 2026-08-26

### 新增

- 独立非模态置顶提醒窗（不抢焦点）；同一轮按股票聚合到期提醒。
- 自动同步限制在 06:00–22:00 并加定点核验；同步结论改为四态；长退避增加低频健康探测。
- 同 EXE Watchdog（2/10/30 秒退避，10 分钟内最多 3 次）；系统时间诊断纳入 W32Time 状态；新版本首次打开旧库先创建校验备份。

### 验证

- 回归测试 51 项通过。

## 0.2.4 — 2026-08-26

### 变更

- 扩大关键变化判定并引入「已确认但需复核」；同步结论增加必要来源覆盖检查。
- 同步间隔拆分为普通日 / 申购日两档（默认 30 / 10 分钟）；`Retry-After` 解析秒数与 HTTP 日期，上限 24 小时。

### 验证

- 回归测试增至 35 项。

## 0.2.3 — 2026-08-26

### 变更

- 申购确认限制在申购日当天，存储层同步校验，旧版遗留的未来确认会被自动撤销。
- 标题栏版本标识、Per-Monitor V2 DPI 清单与紧凑布局；同步间隔可自定义 5 分钟至 7 天。

### 内部

- 公告源原子写入来源健康；沪市公告增加巨潮镜像；伪 PDF（HTML / WAF / JS 挑战页）落盘前拒绝。
- 开机自启改为 HKCU Run 注册项并清理旧计划任务；发布改为标准 x64 MSI（Program Files、可改目录、Major Upgrade、事务回滚）。
- 生成目录统一收口到 `build/`，新增 `build.bat` 与布局校验。

## 0.2.2 — 2026-08-25

### 变更

- 统一 Cargo、User-Agent、脚本与文档版本为 0.2.2 并重打发布包；发布审计保留 `source.rust-only` 门禁。
- 修复联网同步内存脚本的完成状态判定。

## 0.2.1 — 2026-08-25

### 修复

- 修复 Slint 软件渲染窗口从托盘恢复时只重绘局部、残留桌面背景的问题；所有恢复入口统一整窗重绘。

### 内部

- Rust 实现从 `prototypes/stock-ipo-reminder-rust` 迁移到仓库根目录的标准 Cargo 布局；fixture 迁到 `tests/fixtures`，图标迁到 `assets`。
- 删除旧 C#/.NET/WPF 工程与本地 SDK，正式链路只保留 Rust。

## 0.2.0 — 2026-08-25

### 变更

- 正式运行版本完全迁移到 Rust：Slint 界面 + `windows-rs` 原生集成，四来源同步与健康、诊断、备份全部迁移，单个 EXE 承载 PDF Worker 与安装 / 升级 / 便携 / 审计。

### 验证

- 修复新事件首次同步的外键失败与「撤销确认后状态回写」；修复后台启动与关窗生命周期。
- 16/16 固定 fixture 与存储回归通过；真实联网同步 45 条候选 / 24 个事件 / 7 份公告，四来源成功。

### 已知限制

- 不连接券商、不自动下单；发布物尚未进行 Authenticode 签名。

## 0.1.1 — 2026-08-24

### 变更

- 更换为「市场哨兵」新版图标（16–256 px 多尺寸）；托盘右键菜单显示版本号并固定从底部任务栏上方弹出。

## 0.1.0 — 2026-08-24

首个 Windows 预发布候选版本。

### 新增

- 沪市 / 深市 / 北交所新股申购发现与多源核验；常驻托盘、逐只确认与撤销、关键字段变化重新确认。
- 分级提醒（每小时至每 2 分钟）、午间恢复与安全截止；SQLite 持久化、提醒 Outbox、休眠恢复、来源健康与每日摘要。
- 正式公告下载与哈希校验、字段证据与人工覆盖；数据清理、在线备份、日志轮转、脱敏诊断包与系统时间检查。
- 自包含 `win-x64` 便携 ZIP 与免管理员安装器；发布清单与 SHA-256 校验文件。

### 已知限制

- 不连接券商、不自动下单；首版发布物未进行代码签名；Windows 10 2004+ 的支持声明仍需 smoke 证据。

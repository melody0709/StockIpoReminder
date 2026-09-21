# 无 CA 证书的 Minisign 自动更新与一键安装方案

- 状态：客户端已按本文实施，第 10 节自动化门槛全部通过；无证书引导版 `0.3.8` 已提交并发布为 GitHub stable/latest。
- 写作时的剩余项：第 12 节的第 3～7 项——修复显示问题后的 `0.3.9` 已发布为 stable/latest，等待从已安装的 `0.3.8` 完成一次真实的应用内升级（检查 → 下载 → 重启并更新，含 UAC 取消与失败恢复观察）。
- 记录日期：2026-09-09；写作时的应用版本为 `0.3.7`，以 `Cargo.toml` 为唯一版本事实来源。
- **版本基线更新（2026-09-21 复核）：当前工作区版本为 `0.4.3`。** 正文中的旧版本号、旧节流参数和历史结论按原样保留，取代关系只在此登记：

  - **交互**：`.plan/feat/home-update-banner-and-silent-notifications.md`（0.4.0 起实施）把 §7 的两步交互改成标题行绿色胶囊的**一次点击**：按钮标签由「重启并更新」变为「更新到 x.y.z」，一次点击即授权「下载 + 安装 + 重启」，不再有第二次确认。§5.3 第 10 条「全部通过且用户明确点击后才退出主程序并安装」仍然成立，只是「点击」指这一次。
  - **检查节流**：§10 与 §6.3 的「自动检查按 24 小时节流」已失效。0.4.0 改为周期 6 小时、成功结果进程内缓存 6 小时；0.4.1 起**每次启动都重新检查**（唯一例外是距上次检查不足 10 分钟，用于兜住 Watchdog 崩溃重启循环），周期路径仍为 6 小时，自动检查开关默认开启，手动检查不受节流限制。
  - **通知**：§10 中关于「更新 Toast 不进入股票详情」的部分已失效——0.4.0 起更新流程不再发送 Windows Toast 或托盘气泡，只保留主窗口内的绿色胶囊与按需细行；打新提醒、健康摘要和崩溃报告通知不受影响。
  - **发布链**：`CHANGELOG.md` 记录 `0.4.1` 的首次发布缺少启动即检查修复、已删除并替换为重新构建重新签名的产物，随后发布 `0.4.2` 作为验证一键升级闭环的升级目标版本；`0.4.0` 的 GitHub 发布状态未在仓库文档中记录，需以 Release 页面为准。
- 目标平台：Windows x64，MSI 安装版
- 更新源：GitHub Releases stable/latest
- 信任根：应用内置的 Minisign 公钥
- 不再依赖：SignPath、CA 代码签名证书、PFX、付费云签名服务

## 1. 已确认结论

本项目可以在没有 Authenticode/CA 证书的情况下实现可信自动更新：应用使用内置 Minisign 公钥验证更新清单，清单签署 MSI 的版本、大小和 SHA-256；Windows Installer 继续负责实际升级。

边界如下：

1. 保留 MSI + Portable ZIP，不增加 `x64-setup.exe`。
2. 只有 MSI 安装版支持应用内更新；便携版继续人工下载和替换。
3. 不迁移 Tauri/WebView，不引入独立更新服务器。
4. 允许 Windows 显示 UAC、SmartScreen 和“未知发布者”；不得绕过系统安全交互。
5. 首个支持本协议的引导版仍需用户手动安装一次，后续版本才能自动更新。

Minisign 解决的是“应用确认更新确实由项目维护者发布”，不是“Windows 显示已认证发布者”。两者不能混为一谈。

## 2. 目标体验

1. 用户可启用启动及每日自动检查稳定版更新。
2. 发现新版本后通过 Windows Toast 提醒；Toast 不可用时回退托盘气泡。
3. 用户可另外启用“发现更新后自动下载”；该选项默认关闭。
4. MSI 下载完成并重新验证后显示“重启并更新”。
5. 用户点击一次后，应用正常退出、显示必要的 Windows 安全提示、完成 MSI 升级并自动启动新版。
6. 应用关闭或 Windows 重启后，已验证的待安装更新仍可恢复为 `ready` 状态。

“一键更新”指应用内只需点击一次；Windows 自己要求的 UAC 或安全确认仍可能增加一次系统交互。

## 3. 当前可复用基线

- `src/updater.rs`：HTTPS 更新源、版本和 Windows Build 检查、大小限制、流式 SHA-256、受控下载目录、更新 helper、`msiexec /passive`。
- `src/ui/runtime_bridge.rs`、`src/ui/background_operations.rs`：启动延迟检查和后台任务门禁。
- `src/ui/update_callbacks.rs`、`ui/main.slint`：手动检查和安装入口。
- `src/windows_integration`、`src/native_tray.rs`：Toast、托盘气泡、单实例和 Watchdog 互斥量。
- `packaging/windows/Package.wxs`：固定 UpgradeCode、Major Upgrade 和 `InstallFolder` 注册表值。
- `scripts/build-release.ps1`：MSI、Portable ZIP、发布清单和校验和生成。

当前实现仍要求 CMS 清单签名和受 Windows 信任的 Authenticode MSI。`0.3.7` 的 `release-manifest.json` 为 `signed=false`，也没有内置更新 feed，因此当前发布版不会自动更新。

## 4. 最小信任模型

采用一对免费自建的 Minisign 密钥：

| 内容 | 存放位置 | 用途 |
| --- | --- | --- |
| 私钥 | `C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key` | 本地签署 `update-manifest.json` |
| 公钥原件 | 同目录的 `stock-ipo-update.pub` | 私钥恢复与人工复核 |
| 公钥副本 | `assets/update-signing/stock-ipo-update.pub` | 提交仓库并编译进 EXE |

要求：

- 私钥必须设置强密码；不得使用 Minisign 的 `-W` 无密码模式生成正式私钥。
- 私钥不得进入 Git、构建目录、诊断包、日志、命令行正文或 GitHub Release。
- OneDrive 中保存的是已加密私钥，但 OneDrive 不算离线备份；另保留两份离线加密备份。
- 公钥可以公开。编译时直接包含仓库内公钥和固定 GitHub feed，避免因漏设环境变量构建出“更新功能未配置”的正式包。
- 私钥丢失或泄露时，旧客户端不能安全接受一把未经旧私钥授权的新钥匙；必须停止自动更新，并发布需要人工安装的新引导版。

首次生成命令使用 PowerShell 单行语法，并让 Minisign 交互式读取密码：

```powershell
rtk minisign -G -p 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.pub' -s 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key'
```

暂不实现多密钥、在线密钥服务或自动轮换。确需轮换时，先用旧私钥签署一个同时内置新公钥的过渡版本。

## 5. 更新协议

### 5.1 发布资产

每个 stable GitHub Release 至少包含：

```text
StockIpoReminder-<version>-win-x64.msi
StockIpoReminder-<version>-win-x64-portable.zip
release-manifest.json
SHA256SUMS.txt
README.md
RELEASE_NOTES.md
update-manifest.json
update-manifest.json.minisig
```

稳定版 feed 固定为：

```text
https://github.com/melody0709/StockIpoReminder/releases/latest/download/update-manifest.json
```

### 5.2 清单 schema v2

`update-manifest.json`：

```json
{
  "schemaVersion": 2,
  "product": "StockIpoReminder",
  "channel": "stable",
  "version": "0.3.8",
  "publishedAtUtc": "2026-09-09T00:00:00Z",
  "minimumWindowsBuild": 19041,
  "releaseNotesUrl": "RELEASE_NOTES.md",
  "installer": {
    "url": "StockIpoReminder-0.3.8-win-x64.msi",
    "sha256": "<64 位小写十六进制>",
    "sizeBytes": 0
  }
}
```

不增加冗余的 `signatureAlgorithm` 或 `signerSha256` 字段：schema v2 已固定使用 Minisign，签名文件自身包含算法和 key ID。

签名对象是清单写盘后的原始 UTF-8 字节。生成签名后不得重新格式化或改写清单。正式签名使用 Minisign 默认的预哈希格式；客户端必须拒绝 legacy 非预哈希签名。

签名命令：

```powershell
rtk minisign -S -m '.\update-manifest.json' -s 'C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key'
```

### 5.3 客户端验证顺序

顺序固定为：

1. 只从不含用户名、密码和 URL fragment 的 HTTPS feed 下载清单和 `.minisig`。
2. 对清单原始字节验证 Minisign 预哈希签名；成功前不得解析或使用其中任何字段。
3. 精确验证 `schemaVersion=2`、产品、stable 通道、严格的 `x.y.z` 版本、RFC 3339 发布时间和最低 Windows Build。
4. 新版本必须严格高于 `CARGO_PKG_VERSION`；拒绝同版本和降级。
5. MSI 资产名必须与清单版本匹配；URL 必须为无凭据、无 fragment 的 HTTPS URL。
6. MSI 大小必须在允许范围内，SHA-256 必须为 64 位小写十六进制。
7. 下载 MSI 时同时执行响应大小上限、声明长度和增量 SHA-256 校验。
8. helper 安装前重新验证原始清单签名、版本、MSI 大小和 SHA-256。
9. helper 复核 MSI 时持有禁止写入和删除共享的文件句柄，直到 `msiexec` 结束，关闭“验完后被替换”的 TOCTOU 窗口。
10. 全部通过且用户明确点击后才退出主程序并安装。

不得降级为“只相信 HTTPS”或“相信同一未签名清单中的哈希”。GitHub 账号或服务器被接管时，这两种做法不能阻止安装包与哈希同时被替换。

## 6. 客户端最小改造

### 6.1 Minisign 验证

在 `Cargo.toml` 增加 `minisign-verify = "0.2"`。该库支持标准 Minisign 文件格式、预哈希验证且没有额外传递依赖；不引入完整 Tauri 更新器。

修改 `src/updater.rs`：

- 以 `include_str!` 编译仓库内公钥，并固定 stable feed。
- 将 `.p7s` CMS 验证替换为 `.minisig` 验证，调用验证 API 时 `allow_legacy=false`。
- 从 `UpdateInstaller` 移除 `signerSha256`。
- 删除 MSI Authenticode/证书指纹作为应用内更新门禁；保留可选 Authenticode 发布能力。
- 保留现有 HTTPS、重定向、版本、Windows Build、大小、SHA-256、独立 UUID 临时文件和路径约束。
- 将当前 `download_and_request_install` 拆为：
  - `download_and_verify_update`：下载并提交待安装文件，不退出应用。
  - `request_install`：确认 pending 完整后启动 helper。
- helper 不再相信主进程通过命令行传入的哈希或任意 MSI 路径；它只接收 `data_root` 和父进程 PID，从受控 pending 目录重新读取并验证全部材料。

### 6.2 设置与运行状态

保留现有字段语义：

```text
automatic_updates_enabled = 启动及每日自动检查
```

新增独立设置：

```text
automatic_update_download_enabled = 发现更新后自动下载，默认 false
```

不能把已有“启动时自动检查”静默扩展为“自动下载”。自动检查关闭时，自动下载不运行，但用户仍可手动检查和下载。

以下内容是运行状态，不写入 `AppSettings` 或 SQLite：

```text
data_root/update-state.json
data_root/updates/pending/update-manifest.json
data_root/updates/pending/update-manifest.json.minisig
data_root/updates/pending/StockIpoReminder-<version>-win-x64.msi
```

`update-state.json` 最少保存：

```json
{
  "schemaVersion": 1,
  "lastAutomaticCheckAtUtc": null,
  "lastNotifiedVersion": null,
  "pendingVersion": null
}
```

状态文件使用现有原子替换能力写入。pending 目录和文件名必须由程序生成并再次校验，不能接受状态文件中的任意绝对路径。

未完成下载继续放在 `data_root/temp/updates`，由现有 24 小时维护任务清理；已验证 pending 放在 `data_root/updates/pending`，不会被临时文件清理误删。安装成功、版本不再高于当前版本或重新验证失败时，删除对应受控 pending 文件。

### 6.3 检查、下载与提醒

- 启动后延迟 3 秒，若距上次自动检查已满 24 小时则检查。
- 长期驻留时复用现有运行时周期调度，到期后再次检查；不新增长驻线程或可配置间隔。
- 自动检查开始时记录本次尝试时间，失败写脱敏日志，等待下一个 24 小时周期；手动检查永不受节流限制。
- 自动下载关闭：发现新版本后保存可用状态并提醒一次。
- 自动下载开启：发现新版本后直接后台下载；验证完成后提醒“已准备好”，避免同一版本连续弹出“发现”和“准备好”两次通知。
- 同一版本只主动提醒一次；手动检查始终更新界面。
- 应用启动时若存在 pending，必须先重新验证，再恢复 `ready`；失败则安全清理并回到 `available` 或 `idle`。
- 检查或下载并发继续复用现有 `OperationGate`，不引入任务框架。

状态机：

```text
idle -> checking -> available -> downloading -> ready -> installing
                    ^               |            |
                    +----- retry <--+-- error ---+
```

### 6.4 Toast 与界面

复用现有 Windows Toast 和托盘气泡，不新增通知框架。激活参数改为带类型的值：

```text
event:<股票事件 ID>
update:available
update:ready
```

更新通知点击后只显示主窗口并切换到设置页更新区域，不得把 `update:*` 当成股票事件 ID。

设置页更新卡片提供：

- 当前版本、可用版本和下载进度。
- “启动时及每日自动检查”开关。
- “发现更新后自动下载”开关，默认关闭。
- `下载更新`、`重启并更新`、`稍后`、`重试`。

不实现复杂发布说明渲染器；提供简短状态和打开 Release/发布说明的安全 HTTPS 链接即可。

## 7. 一键安装、Watchdog 与重启

用户点击“重启并更新”后（0.4.x 已把按钮标签改为「更新到 x.y.z」并合并为一次点击，见文首版本基线更新）：

1. 主进程启动当前 EXE 的临时 helper 副本。
2. helper 从受控 pending 目录重验清单签名、版本、大小和 MSI SHA-256，并保持 MSI 只读锁定句柄。
3. 主程序正常退出。
4. helper 等待主进程退出，再轮询取得现有 Watchdog supervisor mutex；取得即证明 Watchdog 已观察到正常退出并释放旧 EXE。超时则取消安装，不再依赖固定 1.2 秒睡眠。
5. helper 持有 supervisor mutex 和 MSI 锁，执行 `msiexec /i ... /passive /norestart`。
6. MSI 返回 `0`：清理 pending，原子写入成功结果。
7. MSI 返回 `3010`：清理 pending，写入“安装成功、需要重启 Windows”，不立即启动应用。
8. MSI 失败或用户取消 UAC：保留可诊断结果和可重试 pending；如果已安装 EXE 仍存在，则在释放 supervisor mutex 后自动重启当前版本。
9. 普通成功时，从 `HKLM\Software\StockIpoReminder\InstallFolder` 读取安装目录，验证为绝对目录且存在 `StockIpoReminder.exe`，释放 supervisor mutex 后启动 `StockIpoReminder.exe --background`。
10. 新版正常启动后读取并显示 helper 已写好的结果；最终只能存在一个新 Watchdog/主程序组合。

真实 MSI 闭环必须验证 supervisor mutex 等待、UAC 取消、安装失败、返回 `3010` 和正常成功路径。不能只依赖单元测试证明进程释放时序。

## 8. 构建与发布流程

### 8.1 参数解耦

构建职责拆开：

- `build.bat --package`：普通测试、构建、MSI/ZIP 打包；始终编译公开 feed 和公钥。
- 独立 `scripts/sign-update-manifest.ps1`：根据最终 MSI 生成/验证 schema v2 清单并调用仓库外 Minisign 私钥签名。
- `build.bat --package --sign`：如未来有 Authenticode 证书，仅给 EXE/MSI 做可选 Authenticode；不再决定客户端是否包含更新能力，也不再生成 CMS。

所有本项目 PowerShell 调用使用 PowerShell 7 `pwsh`，同步替换 `build.bat` 和工作流中的 `powershell.exe`/Windows PowerShell 5.1 调用。

`release-manifest.json` 必须分别表达：

- `signed`/`signerSha256`：仅代表可选 Authenticode。
- `updateManifest`、`updateManifestSignature`、`updateSignatureAlgorithm=minisign` 和公钥 key ID：代表应用内更新签名。

`signed=false` 与“Minisign 更新签名有效”可以同时成立。`SHA256SUMS.txt` 在全部清单和签名完成后最后重新生成。

### 8.2 本地发版顺序

1. 修改 `Cargo.toml` 版本并更新 `RELEASE_NOTES.md`。
2. 运行测试和 `build.bat --package`，得到最终 MSI 和 Portable ZIP。
3. 根据最终 MSI 生成 `update-manifest.json`。
4. 使用仓库外有密码私钥生成 `.minisig`；不得把密码作为命令行参数。
5. 用编译进客户端的同一公钥复核签名、版本、MSI 名称、大小和 SHA-256。
6. 更新 `release-manifest.json`，最后生成 `SHA256SUMS.txt`。
7. 创建 GitHub Draft Release，一次上传全部资产。
8. 通过经过身份验证的 Draft 下载或 GitHub API 重新下载全部资产，再次验证名称、大小、哈希和 Minisign 签名。
9. 验证全部通过后，一次性将 Draft 发布为 stable/latest；不得发布后再补传或覆盖更新清单和 MSI。
10. 发布后从公开的 `releases/latest/download/update-manifest.json` 和 `.minisig` 再做一次只读验证。

Draft 中资产的上传先后顺序不构成安全边界；真正的发布边界是“完整 Draft 一次性公开”。

## 9. 预期改动范围

| 区域 | 文件 | 最小改动 |
| --- | --- | --- |
| 协议与验证 | `src/updater.rs`、更新测试 | Minisign、pending 恢复、下载/安装拆分、锁定 MSI 后二次校验 |
| 依赖和公钥 | `Cargo.toml`、`Cargo.lock`、`assets/update-signing/stock-ipo-update.pub` | 加入零传递依赖验证库和公开信任根；无需为此修改 `build.rs` |
| 设置 | `src/model.rs`、设置 UI/测试 | 仅增加独立自动下载同意，默认关闭 |
| 运行状态 | 更新模块内的小型状态读写 | 原子 `update-state.json` 和受控 pending 目录，不进入 SQLite |
| 后台流程 | `src/ui/runtime_bridge.rs`、`src/ui/background_operations.rs` | 24 小时节流、自动下载、pending 恢复、现有任务门禁 |
| 通知和操作 | `src/ui/update_callbacks.rs`、`src/native_tray.rs` | 带类型的激活参数、下载、重启安装 |
| UI | `ui/main.slint` | 独立下载开关、进度和状态按钮 |
| 进程闭环 | `src/updater.rs`、现有单实例/Watchdog API | supervisor mutex 等待、安装结果、失败恢复、启动新版 |
| 打包 | `scripts/build-release.ps1`、`build.bat`、新签名脚本 | 解耦 Authenticode 与 Minisign；使用 `pwsh` |
| 验证 | `scripts/test-signing-update.ps1`、`smoke-release.ps1`、`audit-release.ps1`、`validate-build-layout.ps1` | 移除 `.p7s`/CMS 硬编码并覆盖 Minisign/pending/发布资产 |
| 文档 | `README.md`、`RELEASE_NOTES.md`、`docs/release-signing-and-updates.md` | 无证书更新边界、未知发布者说明、发版步骤 |

不新建更新服务、通用状态框架、抽象签名接口或第二套安装器。

## 10. 验收门槛

自动化至少覆盖：

- 正确的 Minisign 预哈希签名被接受；legacy 或错误 key ID 被拒绝。
- 清单、签名或 MSI 任一字节被修改均拒绝安装。
- 错误 schema、产品、通道、版本、发布时间、Windows Build、资产名、大小、哈希或非 HTTPS URL 被拒绝。
- 降级和同版本覆盖被拒绝。
- helper 不信任命令行哈希或任意路径，并会完整重验 pending。
- helper 校验到 `msiexec` 结束期间，MSI 不能被写入、重命名或删除。
- 自动检查按 24 小时节流，手动检查不受限制。→ 0.4.x 已取代为「启动 10 分钟地板 + 驻留周期 6 小时 + 成功结果进程内缓存 6 小时」，见文首版本基线更新。
- 原有自动检查设置不会自动授权下载；新下载开关默认关闭。
- 同一版本只主动提醒一次，更新 Toast 不会进入股票详情。→ 0.4.0 起更新流程不再发送 Toast 或托盘气泡，「更新 Toast 路由」部分已失效；不再打扰用户的方式改为「同版本隐藏 + 无更新时整块不渲染」，见文首版本基线更新。
- 退出或重启应用后，有效 pending 恢复为 `ready`；损坏或过期 pending 被拒绝并清理。
- 下载失败、UAC 取消和安装失败不会损坏当前安装；应用退出后能重新启动当前版本。
- MSI Major Upgrade 保留用户数据；安装成功后新版以托盘模式启动，且只有一个 Watchdog/主进程组合。
- 发布目录同时包含 MSI、ZIP、Minisign 清单和签名、发布清单、发布说明和校验和。

实施完成后运行：

```text
rtk cargo fmt
rtk cargo test
rtk cmd /c build.bat --package
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1
```

同时执行更新篡改测试、Windows smoke、release audit，以及一次从首个引导版升级到更高测试版本的真实 MSI 闭环。人工记录 UAC/“未知发布者”表现；没有 Authenticode 时不得把 SmartScreen 信誉视为通过条件。

## 11. 明确不做

- 不申请 SignPath 或其他 CA 代码签名证书。
- 不增加 `x64-setup.exe`。
- 不迁移 Tauri/WebView。
- 不支持便携 ZIP 自动覆盖。
- 不做无提示强制安装。
- 不绕过 UAC、SmartScreen 或 Windows 安全提示。
- 不实现差分更新、多发布通道、自动回滚、在线密钥服务或自动密钥轮换。
- 不为了一个实现增加通用更新器抽象或任务框架。

## 12. 引导版与完成定义

现有 `0.3.7` 没有内置 Minisign 公钥和 stable feed，不能通过旧协议自动获得新版本。因此完成需要两版验证：

1. 人工安装首个无证书自动更新引导版；它内置正式公钥和 feed。
2. 发布更高版本的无 Authenticode MSI、签名清单和完整 Release 资产。
3. 引导版能按设置自动检查、提醒，并只在独立授权后自动下载。
4. 用户点击“重启并更新”后完成 MSI 升级并自动启动新版。（0.4.x 的按钮标签为「更新到 x.y.z」，同一次点击即完成下载、安装与重启，见文首版本基线更新）
5. 篡改清单、签名或 MSI 时明确拒绝安装。
6. 下载、退出、Watchdog 等待、UAC 取消或 MSI 安装失败时，当前安装和用户数据保持可用。
7. 所有自动化门槛、发布审计和真实 MSI 闭环通过。

满足以上七项后，本功能才算完成。

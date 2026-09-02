# 零费用 GitHub MSI + SignPath 一键发布与安全更新方案

- 状态：已选定路线，尚未开始实施
- 记录日期：2026-09-02
- 产品版本事实来源：`Cargo.toml`（当前工作区为 `0.3.7`）
- 已确认路线：零费用 + GitHub Release + MSI + SignPath
- 不在本方案内：Microsoft Store / MSIX 迁移、便携版自动更新、无交互静默安装、自动下单或任何券商能力

## 1. 目标与准确边界

本方案要做到两件事：

1. 发布者在 GitHub 上触发一次稳定版发布流程，构建、测试、远程签名、生成更新元数据、创建 GitHub Release、上传资产和最终公开发布都由工作流完成。
2. 已通过 MSI 安装的用户，应用启动后在后台检查新版本；发现更新时收到托盘或系统通知，点击一次“立即更新”即可下载、校验、调用 MSI 升级并重启。

“一键”不等于绕过安全边界：

- SignPath 的人工签名批准仍由项目维护者完成。这是对每次正式签名的外部授权，不应试图自动绕过。
- Windows UAC 仍由用户确认；应用只在用户点击安装后启动更新助手，绝不静默替换程序。
- 每个新版本仍必须先修改 `Cargo.toml` 的版本号并提交/打标签。工作流不在发布时擅自修改源码版本。
- 便携 ZIP 继续手动更新，不能被应用内更新流程改造成 MSI 安装版。

## 2. 当前基线与需要解决的问题

当前仓库已经具备一条“本地持有代码签名私钥”的安全更新链：

- `src/updater.rs` 从编译时环境变量读取更新清单 URL 和固定的签名证书 SHA-256；它下载 `update-manifest.json` 和同名 `.p7s`，验证 CMS/PKCS#7 分离签名、MSI 哈希和 Authenticode。
- `scripts/build-release.ps1 -Sign` 假定构建机可以访问 PFX 或当前用户证书私钥，并在本地生成 `.p7s` 更新清单签名。
- `.github/workflows/signed-release.yml` 当前只构建、签名、验证并上传 GitHub Actions artifact，不会创建 GitHub Release，也不适合直接接入无私钥导出的远程签名服务。
- `src/ui/runtime_bridge.rs` 已有启动后检查更新入口；`AppSettings` 中的启动检查默认关闭，尚未有“每天最多一次”的持久化节流。
- 仓库当前没有 `LICENSE` 文件；申请开源免费签名服务之前必须由项目所有者明确选择许可证。

SignPath 的签名私钥不应被导出到仓库、GitHub Secret 或本机构建脚本。并且不能把长期更新可用性建立在“远程签名服务一定能生成当前 CMS 文件，且其叶证书永不变化”的假设上。因此，实施时需要把“更新清单可信性”和“Windows 安装包可信性”分开。

## 3. 推荐的最终信任架构

### 3.1 GitHub Release 作为稳定更新源

稳定通道只使用非草稿、非预发布的 GitHub Release，并把下面地址编译进应用：

```text
https://github.com/melody0709/StockIpoReminder/releases/latest/download/update-manifest.json
```

清单中的 MSI 使用相对文件名；客户端以清单 URL 为基准解析，因此会下载同一条 `latest` Release 中的 MSI。预发布必须标记为 prerelease，不能污染 stable 的 `latest` 指针。

### 3.2 将更新清单签名与 Authenticode 分离

采用两个互补的信任层：

| 层级 | 用途 | 密钥/证书位置 | 客户端验证 |
| --- | --- | --- | --- |
| 更新清单签名 | 证明版本、下载 URL、SHA-256、允许的 MSI 签名者没有被篡改 | Ed25519 发布私钥仅存于受保护的 GitHub Environment Secret；恢复私钥离线保存 | 内置公钥、`keyId`、分离签名 |
| Windows Authenticode | 让 Windows 和用户识别 MSI/EXE 的签名发布者 | SignPath Foundation 远程签名服务 | Windows 信任验证 + 清单中已签名的证书 SHA-256 |

这比继续把更新清单固定到单张 Authenticode 叶证书更适合 SignPath：签名证书续期或轮换时，只需在已认证的新清单中声明新的 MSI 签名证书 SHA-256；旧客户端不会因叶证书变化而永久失去更新能力。

### 3.3 Ed25519 更新签名与密钥轮换

首个可自动更新的 MSI 内置两个非秘密公钥：

- `release-<年份>`：日常发布私钥对应的公钥。私钥只作为 GitHub `release-signing` Environment Secret 使用。
- `recovery-<年份>`：离线恢复私钥对应的公钥。私钥不上传 GitHub、不参与日常发布；仅在日常私钥泄露、丢失或轮换时使用。

每个 `update-manifest.json` 增加 `signatureAlgorithm: "ed25519"` 和 `keyId`，并配套发布 `update-manifest.json.sig`。客户端只接受内置公钥集合中相应 `keyId` 的有效签名。

日常密钥需要轮换时，使用离线 recovery 私钥签署一个更高版本的清单；该版本的 EXE 内置新的日常公钥。这样已安装的旧客户端可以安全过渡到新的日常密钥。若两个私钥同时不可用或均泄露，则停止自动更新并通过 GitHub Release 说明要求用户手动安装修复版。

### 3.4 MSI 验证规则

更新客户端必须按以下顺序拒绝不可信更新：

1. 清单 URL、重定向最终地址和 MSI URL 均为无凭据、无片段的 HTTPS 地址。
2. Ed25519 分离签名、产品名、stable 通道、版本格式、最低 Windows Build、文件大小和 SHA-256 全部通过。
3. 新版本必须严格大于当前 `CARGO_PKG_VERSION`，禁止回滚或同版本覆盖。
4. 下载完成后再次校验长度和 SHA-256。
5. 以 Windows 标准信任策略验证 MSI Authenticode，并读取其签名证书 SHA-256；该值必须等于清单中、且已被 Ed25519 签名保护的 `installer.signerSha256`。
6. 只有以上全部通过且用户明确点击安装时，才复制更新助手、等待主程序退出并调用 `msiexec`。

这保留现有的大小边界、哈希双重校验、HTTPS 限制、版本单调性和更新助手保护，同时不再把 SignPath 的具体叶证书指纹硬编码为唯一更新根。

## 4. 发布资产与清单格式

每个 stable GitHub Release 必须包含下列资产；所有资产先上传至 Draft Release，验证成功后再公开：

```text
StockIpoReminder-<version>-win-x64.msi
StockIpoReminder-<version>-win-x64-portable.zip
release-manifest.json
SHA256SUMS.txt
RELEASE_NOTES.md
update-manifest.json
update-manifest.json.sig
```

建议将更新清单升级为 schema v2，核心结构如下：

```json
{
  "schemaVersion": 2,
  "product": "StockIpoReminder",
  "channel": "stable",
  "version": "0.3.8",
  "publishedAtUtc": "2026-09-02T00:00:00Z",
  "minimumWindowsBuild": 19041,
  "releaseNotesUrl": "RELEASE_NOTES.md",
  "signatureAlgorithm": "ed25519",
  "keyId": "release-2026",
  "installer": {
    "url": "StockIpoReminder-0.3.8-win-x64.msi",
    "sha256": "<64 位小写十六进制>",
    "sizeBytes": 0,
    "signerSha256": "<SignPath 签名证书 SHA-256>"
  }
}
```

`update-manifest.json.sig` 是清单原始 UTF-8 字节的 Ed25519 分离签名。实现时固定 JSON 写入方式和 UTF-8 无 BOM，签名前后不得重新格式化，以免改变待验证字节。

`release-manifest.json` 和 `SHA256SUMS.txt` 继续用于人工审计与发布验证；应用内更新只以已签名的 `update-manifest.json` 为授权来源。

## 5. 一键发布流水线

### 5.1 工作流触发与保护

新增或重构稳定版工作流，使其仅接受：

- 已推送的 `v<version>` tag，且 tag 去掉 `v` 后必须严格等于 `Cargo.toml` 版本；或
- 手动 `workflow_dispatch` 指定 tag，工作流先执行同样的版本一致性检查。

工作流必须设置：

- `concurrency`：stable 发布同一时间只允许一个运行，防止两个 Draft Release 竞争 `latest`。
- 最小 GitHub permissions：构建 job 只读；创建/发布 Release 的 job 才有 `contents: write`；只有确有需要时才授予 `id-token: write`。
- `release-signing` GitHub Environment：受保护分支/tag、需要审批者、只在发布 job 注入更新签名私钥。
- 所有第三方 Action 固定到完整 commit SHA，不以浮动 tag 运行。
- 日志绝不打印更新私钥、GitHub Secret、SignPath 令牌、PFX 或任何带凭据 URL。

### 5.2 按签名依赖排序的构建步骤

EXE 必须先被 SignPath 签名，再进入 MSI；MSI 完整生成后才能再次由 SignPath 签名。推荐步骤如下：

1. 检出已验证的 tag，读取并校验 `Cargo.toml` 版本、工作区干净状态和 Release Notes 条目。
2. 用内置的更新公钥集合编译 unsigned EXE，执行 Rust 格式化、测试和未签名构建布局验证。
3. 将 EXE 提交给 SignPath；维护者在 SignPath 侧批准后取回签名后的 EXE，并验证其 Authenticode 状态。
4. 用已签名 EXE 生成便携 ZIP 和 unsigned MSI。
5. 将完整 MSI 提交给 SignPath；批准后取回签名后的 MSI，并验证 Authenticode 状态、签名证书 SHA-256 和 MSI 载荷。
6. 根据最终签名 MSI 的真实大小、SHA-256 和证书 SHA-256 生成 schema v2 更新清单；使用 GitHub Environment 中的日常 Ed25519 私钥签出 `.sig`。
7. 生成 `release-manifest.json`、`SHA256SUMS.txt`，执行完整 smoke、更新集成、审计和构建布局验证。
8. 创建 GitHub Draft Release，上传所有资产；从 Release 下载每个资产重新校验名称、大小和 SHA-256，并验证 `latest/download/update-manifest.json` 解析出的 MSI URL。
9. 只有步骤 1–8 全部通过后才将 Draft Release 发布为 stable。发布后写出 job summary，列出版本、资产哈希、SignPath 签名证书 SHA-256 和验证报告。

这样，公开用户永远不会读取到“清单已经可见、MSI 尚未上传”或“MSI 已上传、清单/签名尚未上传”的半成品状态。

### 5.3 构建脚本的预期拆分

现有 `scripts/build-release.ps1 -Sign` 以本地证书为中心。实施时应拆为可独立验证的阶段，而不是把远程签名令牌塞进现有脚本：

- `prepare-runtime`：编译 EXE、复制运行时文件、生成待签名输入。
- `package-from-signed-runtime`：仅接受已验证签名 EXE，生成 ZIP 和待签名 MSI。
- `finalize-signed-release`：仅接受已验证签名 MSI，生成所有哈希、更新清单与 Ed25519 分离签名。
- `verify-signed-release`：离线复核 EXE/MSI、清单、签名和发布布局；供 CI、人工发布前检查和回归测试共用。

脚本必须拒绝 unsigned EXE 进入 MSI 阶段、拒绝 unsigned MSI 进入 finalize 阶段。SignPath 的具体项目配置和 GitHub Action 只在开户后按其官方接入说明写入，避免事先猜测服务端配置字段。

## 6. 用户端更新体验与频率

### 6.1 默认与节流策略

- 新安装的、已配置更新源的 MSI 版本：默认开启“启动时自动检查稳定版更新”。
- 已存在的用户设置：保留原值，不强制替用户开启网络检查。
- 便携版或未配置可信更新源的版本：更新开关禁用并解释原因，不发起网络请求。
- 自动检查：启动后延迟执行，且距上次尝试不足 24 小时时跳过。
- 自动检查失败：记录脱敏诊断，6 小时内不重复自动尝试；不弹出错误窗口打扰用户。
- 手动“检查更新”：不受自动节流影响。

检查状态需原子持久化在用户数据目录，至少包含最近一次尝试时间、最近成功时间、最近发现版本和最近错误摘要；不得把密钥、响应正文或 URL 凭据写入诊断。

### 6.2 提示与安装

发现更高的已验证版本时：

- 显示 Windows Toast；Toast 不可用时使用托盘气泡回退。
- 通知点击后将主窗口带到更新区域；不使用任务栏闪烁作为更新提醒。
- 设置页显示版本号、简短状态、“查看更新说明”和“立即更新”按钮。
- “立即更新”执行下载、校验、安装助手和正常 UAC；下载或验证失败时保留主程序，给出可读错误和“重试”入口。
- 下载完成后由更新助手关闭应用并使用 MSI Major Upgrade 安装；数据迁移前继续执行现有 SQLite 备份与完整性校验。

## 7. 预期源码和文件改动范围

| 区域 | 预期文件 | 主要改动 |
| --- | --- | --- |
| 更新信任协议 | `src/updater.rs`、其单元测试 | CMS 单证书固定迁移为 Ed25519 清单签名、双公钥选择、清单中 MSI 签名者指纹、证书轮换路径 |
| 编译时配置 | `build.rs`、`Cargo.toml` | 只编译非秘密更新公钥与 feed URL；不再需要把远程 Authenticode 叶证书作为唯一更新根 |
| 设置与调度 | `src/model.rs`、`src/ui/settings.rs`、`src/ui/settings_callbacks.rs`、`src/ui/runtime_bridge.rs` | 新安装默认、持久化 24 小时/失败退避、保留旧用户选择 |
| 更新通知/UI | `src/ui/background_operations.rs`、`src/ui/update_callbacks.rs`、`src/ui/notification_callbacks.rs`、`ui/main.slint` | 后台发现、Toast/托盘回退、进入更新页、状态与可访问性文本 |
| 发布工具 | `scripts/build-release.ps1`、`build.bat`、新增阶段化脚本 | 远程签名前后阶段、最终 manifest/校验和生成、拒绝错误顺序的输入 |
| 验证工具 | `scripts/test-signing-update.ps1`、`scripts/smoke-release.ps1`、`scripts/audit-release.ps1`、`scripts/validate-build-layout.ps1` | Ed25519、SignPath 签名工件、发布资产与更新 feed 的完整验证 |
| GitHub 自动化 | `.github/workflows/signed-release.yml`，必要时新增 release 工作流 | tag 校验、Environment、SignPath 签名、Draft Release、资产上传/复核、最终发布 |
| 文档与法律文件 | `LICENSE`、`CODE_SIGNING_POLICY.md`、`README.md`、`RELEASE_NOTES.md`、`docs/release-signing-and-updates.md` | 开源许可证、签名政策、用户更新说明、维护者发布操作与应急轮换流程 |

实施时必须保留当前工作区已有的未提交用户改动；更新改造只在上述范围内最小化叠加，不覆盖同步、提醒或设置界面的其他优化。

## 8. 分阶段实施与验收门槛

### 阶段 0：外部资格与所有者决策（必须先完成）

所有者需要完成：

1. 明确选择并授权加入一个 OSI 认可的开源许可证。推荐 MIT；Apache-2.0 也可，但这是法律选择，不能由自动化或代理代替决定。
2. 开启 GitHub MFA，确认仓库公开状态、发布维护者和稳定版 tag 保护规则。
3. 以仓库所有者身份申请并接入 SignPath Foundation，接受其项目规则并配置项目维护者批准人。
4. 在 GitHub 创建受保护的 `release-signing` Environment；确认审批人、允许的 tag/分支和 Secret 管理责任人。
5. 在安全设备上生成日常 Ed25519 私钥和离线 recovery 私钥；仅把日常私钥作为 Environment Secret 保存，recovery 私钥不得上传 GitHub。

验收：不提交任何私钥/PFX；`LICENSE`、代码签名政策和 SignPath 项目接入状态已由项目所有者确认。

### 阶段 1：更新协议与本地验证

1. 实现 schema v2、Ed25519 分离签名和两把内置公钥。
2. 将 MSI Authenticode 检查改为“Windows 信任 + 已签名清单中声明的证书 SHA-256”。
3. 保持版本单调、大小/哈希双检、HTTPS/重定向限制、更新助手路径约束和数据迁移前备份。
4. 实现自动检查持久化节流和用户可见的 Toast/托盘提示。

验收：篡改清单、篡改签名、错误 keyId、错误产品/通道、证书不匹配、错误哈希、降级版本、未受 Windows 信任的 MSI 均被拒绝；recovery 密钥签署的轮换版本可被旧客户端接受。

### 阶段 2：可重复的远程签名打包

1. 将本地 PFX 签名发布流程拆分为准备、SignPath EXE、打包、SignPath MSI、最终封装五个步骤。
2. 为每步添加输入/输出断言和本地可运行的验证入口。
3. 替换现有仅上传 Actions artifact 的工作流，增加 Draft Release 创建、上传、重新下载验证和最终发布。

验收：在不访问本地 PFX 的前提下，从 tag 生成一个完整、可验证的 Draft Release；任何一个签名、文件或哈希缺失都会阻止公开发布。

### 阶段 3：首次引导发布

1. 递增 `Cargo.toml` 版本并生成首个 SignPath 签名的 MSI/ZIP stable Release。
2. 将该版本设为 GitHub Latest，并验证 `latest/download/update-manifest.json`、`.sig` 和 MSI 均可下载。
3. 在干净 Windows 用户环境中安装 MSI，开启默认更新检查，模拟/发布一个更高测试版本，验证通知、一次点击安装、程序重启和数据保留。
4. 在 Release Notes 中明确：早期未配置更新源或未签名的用户需要手动安装这一个引导版本；之后才有应用内更新。

验收：从首次引导版升级到更高 stable 版本成功，且无须用户另行下载 MSI；便携版仍明确提示手动更新。

### 阶段 4：日常发布与应急演练

1. 维护者实际执行一次“tag → workflow → SignPath 批准 → GitHub Release”的完整发布。
2. 演练 SignPath 证书轮换：用新签名 MSI 和更新清单中的新证书 SHA-256 完成一次升级。
3. 演练日常更新私钥失效：由 recovery 私钥签署一次更高版本，引导客户端迁移到新日常公钥。

验收：日常发布、证书轮换和更新密钥恢复均有可复现记录；未把任何秘密写入仓库、Release 资产或诊断日志。

## 9. 自动化测试与发布门禁

每次源码或发布流程变更至少执行：

```text
rtk cargo fmt
rtk cargo test
rtk cmd /c build.bat --package
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/validate-build-layout.ps1
```

正式远程签名发布还必须执行：

- Ed25519 更新签名的正反例测试。
- SignPath 签名 EXE/MSI 的 Authenticode 验证。
- 更新客户端下载、篡改拒绝、证书轮换和 recovery 密钥轮换集成测试。
- MSI 最小安装、升级、卸载保留用户数据和迁移前备份验证。
- GitHub Draft Release 资产清单、哈希、`latest/download` 路径和发布后下载复核。
- `smoke-release`、`audit-release` 以及所有现有布局验证。

发布脚本在任何门禁失败时必须保持 Release 为 draft 或删除未公开 draft；不能发布不完整的 stable Release。

## 10. 回滚、故障与运维原则

- 发布前发现问题：仅删除/修复 Draft Release，不让其成为 Latest。
- 公开后发现严重缺陷：发布一个更高版本的修复 MSI；客户端只允许向更高版本升级，不能依赖静默降级。
- SignPath 暂不可用：不发布新 stable；已安装版本继续正常使用，用户可从上一稳定 Release 手动下载。
- GitHub Release 资产被替换、CDN 被篡改或 URL 被重定向：Ed25519 签名、哈希、版本和 Authenticode 检查应拒绝安装。
- 日常更新私钥疑似泄露：停用该 Environment Secret，用离线 recovery 私钥发布更高版本；新版本内置新的日常公钥。
- recovery 私钥疑似泄露：暂停自动更新、在 GitHub Release 公告人工升级方式，并通过一个由仍可信密钥签署的更高版本重建密钥集合；若无可信密钥则需要人工重装引导版。

不得覆盖旧 MSI 或修改已公开 Release 的更新资产来“修复”问题；保留至少一个已知稳定 MSI、完整的校验和和发布记录，便于人工恢复与故障调查。

## 11. 当前需要用户确认的事项

路线已经确认；开始实现前只剩以下外部/法律操作需要项目所有者决定或点击：

| 事项 | 建议 | 责任人 |
| --- | --- | --- |
| 开源许可证 | MIT（最简洁）；也可明确选择 Apache-2.0 | 项目所有者 |
| SignPath Foundation 申请与项目规则 | 使用 GitHub 账号提交、启用 MFA、指定签名批准人 | 项目所有者 |
| GitHub `release-signing` Environment | 需要审批、仅允许 stable tag、保存日常更新私钥 | 项目所有者 |
| recovery 私钥保管位置 | 离线加密存储，至少保留一份可恢复副本 | 项目所有者 |
| 每次正式签名批准 | 在 SignPath 的项目批准流程中确认 | 项目维护者 |

其余工作——源码改造、密钥生成辅助工具、GitHub Actions、SignPath 接入模板、测试、Release 草稿与文档——均可由项目内实施完成。


# 发布签名与安全自动更新

本文说明 Stock IPO Reminder 的无 CA 证书自动更新方案：应用内置 Minisign 公钥验证更新清单，清单声明 MSI 的版本、大小和 SHA-256，Windows Installer 负责实际升级。Authenticode 代码签名变为完全可选能力，`signed: false` 与“Minisign 更新签名有效”可以同时成立。

## 信任模型

- 信任根是编译进 EXE 的 Minisign 公钥（仓库内 `assets/update-signing/stock-ipo-update.pub`），对应私钥保存在仓库外并加密。正式构建始终包含该公钥和固定 GitHub stable feed，不存在“漏设环境变量导致更新功能未配置”的正式包。
- 更新源固定为 `https://github.com/melody0709/StockIpoReminder/releases/latest/download/update-manifest.json` 及其 `.minisig`。
- `update-manifest.json` 为 schema v2：Minisign 默认预哈希签名覆盖清单写盘后的原始 UTF-8 字节；客户端以 `allow_legacy=false` 验证，拒绝 legacy 非预哈希签名。
- 客户端验证顺序固定：先验证清单原始字节签名（成功前不解析任何字段），再校验产品、stable 通道、严格 `x.y.z` 递增版本、RFC 3339 发布时间、最低 Windows Build、安装包文件名与版本一致、无凭据无 fragment 的 HTTPS URL、大小范围和 SHA-256 格式。
- 下载时同时执行响应大小上限、声明长度和增量 SHA-256 校验；下载完成提交到受控 pending 目录（`数据目录\updates\pending`），应用关闭或重启后可恢复为就绪状态，恢复时全部材料重新验证。
- 安装 helper 只接收数据目录和父进程 PID；它从受控 pending 目录重新验证清单签名、版本、大小和 MSI SHA-256，并以禁止写入和删除共享的只读句柄锁定 MSI 直到 `msiexec` 结束。
- 只有用户明确点击标题行的绿色「更新到 x.y.z」胶囊后才退出主程序；**这一次点击即授权「下载 + 安装 + 重启」，不再有第二次确认**（0.4.0 起的一键契约，取代 0.3.8 的两步「下载 → 重启并更新」）。如果用户在设置页开启「发现更新后提前下载」，安装包会在点击前就下载并验证好，但安装仍然只由这次点击触发。helper 等待父进程退出、轮询取得 Watchdog supervisor 互斥量后执行 `msiexec /i ... /passive /norestart`。成功后从 `HKLM\Software\StockIpoReminder\InstallFolder` 启动新版本（`--background`，即常驻托盘不弹主窗口）；返回 `3010` 提示重启 Windows；失败或取消 UAC 时保留可重试 pending 并恢复启动当前版本。
- 更新只允许升级到更高的 `x.y.z` 版本；WiX Major Upgrade 继续负责程序文件事务回滚。数据迁移前仍由应用创建并校验 SQLite 备份。
- 便携版不会静默转换为安装版，应用内自动更新入口只对已由本产品 MSI 注册的安装版开放。
- 私钥丢失或泄露时，旧客户端不能安全接受未经旧私钥授权的新钥匙；必须停止自动更新并发布需要人工安装的新引导版。

Minisign 解决的是“应用确认更新确实由项目维护者发布”，不是“Windows 显示已认证发布者”。没有 Authenticode 时 UAC、SmartScreen 和“未知发布者”提示属正常现象，本项目不绕过任何系统安全交互。

## 密钥管理

| 内容 | 存放位置 | 用途 |
| --- | --- | --- |
| 私钥 | `C:\Users\kawae\OneDrive\vault\StockIpoReminder\update-signing\stock-ipo-update.key` | 本地签署 `update-manifest.json`（强密码加密） |
| 密码备忘 | 同目录 `stock-ipo-update.password.txt` | 仅在忘记密码前临时存在，记入密码管理器后删除 |
| 公钥原件 | 同目录 `stock-ipo-update.pub` | 私钥恢复与人工复核 |
| 公钥副本 | `assets/update-signing/stock-ipo-update.pub` | 提交仓库并编译进 EXE |

- 私钥必须设置强密码；不得使用 `-W` 无密码模式生成正式私钥。
- 私钥不得进入 Git、构建目录、诊断包、日志、命令行正文或 GitHub Release；OneDrive 不算离线备份，另保留两份离线加密备份。
- 首次生成见 `scripts/generate-update-signing-key.ps1`（已生成正式密钥对后不要重复运行）。
- 暂不实现多密钥、在线密钥服务或自动轮换；确需轮换时，先用旧私钥签署一个同时内置新公钥的过渡版本。

## 本地发版顺序

1. 修改 `Cargo.toml` 版本，并同步所有「重复声明版本」的文档。缺任何一项都会造成「基线已经升到新版本、文档仍写旧版本」的不一致：

   - `CHANGELOG.md`：把 `[未发布]` 小节提升为 `## <版本> — <日期>`，并另起一个空的 `[未发布]`。
   - `RELEASE_NOTES.md`：新增本版本小节，只写用户能感知到的变化。
   - `README.md`：「当前版本」行，以及「安装版」「便携版」两节中的 `StockIpoReminder-<版本>-win-x64.msi` / `StockIpoReminder-<版本>-win-x64-portable.zip` 文件名。
   - 顺序要求：`RELEASE_NOTES.md` 与 `README.md` 会被复制进 `build/packages/<版本>/`，因此必须在 `build.bat --package` **之前**改完，否则包里带的是旧文案。
2. 运行测试和 `rtk cmd /c build.bat --package`，得到最终 MSI 和便携 ZIP。
3. 根据最终 MSI 生成并签名 `update-manifest.json`：

```text
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/sign-update-manifest.ps1
```

脚本会让 minisign 交互式询问私钥密码（密码不进入命令行）；签名后自动用编译进客户端的同一公钥复核，回填 `release-manifest.json` 的 `updateManifest`、`updateManifestSignature`、`updateSignatureAlgorithm=minisign` 和公钥 key ID，最后重新生成 `SHA256SUMS.txt`。

4. 运行验证（见下节），全部通过后创建 GitHub Draft Release 并一次上传全部资产：MSI、便携 ZIP、`README.md`、`RELEASE_NOTES.md`、`release-manifest.json`、`SHA256SUMS.txt`、`update-manifest.json`、`update-manifest.json.minisig`。
5. 通过经过身份验证的 Draft 下载重新验证全部资产的名称、大小、哈希和 Minisign 签名；验证通过后一次性把 Draft 发布为 stable/latest。不得发布后再补传或覆盖更新清单和 MSI——Draft 中资产的上传先后顺序不构成安全边界，真正的发布边界是“完整 Draft 一次性公开”。
6. 发布后从公开的 `releases/latest/download/update-manifest.json` 和 `.minisig` 再做一次只读验证。
7. 收尾核对文档版本一致性（见下节「文档版本一致性自检」），避免出现「基线已升到新版本、README 仍写旧版本」的不一致。

## 可选 Authenticode

如未来取得代码签名证书，`rtk cmd /c build.bat --package --sign` 可为 EXE/MSI 提供带 RFC 3161 时间戳的 Authenticode 签名（证书放当前用户存储或短生命周期 PFX，密码仅经环境变量传递）。该开关不再决定客户端是否包含更新能力，也不再生成 CMS 清单。

## CI 密钥保护

- Minisign 私钥永不上 CI；CI 只做构建、测试和可选 Authenticode。
- 正式 PFX 只存放在受保护的 CI secret 或独立签名服务中，不提交 Base64、密码或私钥文件。
- 发布工作流仅允许手动触发，并绑定需要审批的 GitHub Environment。
- PFX 只写入 runner 临时目录，签名完成后在 `finally` 中删除；构建产物中不得包含 PFX。
- 日志不得打印 PFX 密码、私钥内容或带凭据 URL。
- 发布前必须核对 `release-manifest.json` 的 `signed`、`signerSha256`、`timestampUrl`、更新清单文件名、`updateSignatureAlgorithm` 和所有 SHA-256。

## 验证

```text
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-signing-update.ps1
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-release.ps1
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/audit-release.ps1
rtk pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/test-update-helper-recovery.ps1
```

`test-signing-update.ps1` 在沙盒生成一次性无密码 Minisign 测试密钥（绝不进入生产信任根），验证预哈希签名接受、篡改清单拒绝、错误密钥拒绝、正式信任根拒绝测试密钥、legacy 签名拒绝和安装包哈希拒绝。`audit-release.ps1` 会在发布目录存在更新清单时，用仓库公钥和发布 EXE 各自复核一遍。`test-update-helper-recovery.ps1` 用生产密钥签名的 pending 加无效 MSI 验证安装助手的验签、锁定、等待与失败恢复路径（不触发 UAC）。真实 MSI 闭环（引导版升级到更高测试版本、UAC 取消、安装失败、`3010` 与正常成功路径）需在本机用独立 `--data-root` 人工执行并记录。

## 文档版本一致性自检

`Cargo.toml` 是唯一版本来源，但仍有几处必须人工同步的版本声明。发版收尾时逐个核对：

```text
rtk rg -n "StockIpoReminder-0\.[0-9]+\.[0-9]+-win-x64" README.md
rtk rg -n "当前版本" README.md
```

- `README.md` 的「当前版本」行与两处安装包 / 便携包文件名必须等于 `Cargo.toml` 的版本，且不得残留指向旧版本的文件名。
- `RELEASE_NOTES.md` 首个小节必须是本次版本；`CHANGELOG.md` 的 `[未发布]` 必须为空且上一节是本次版本。
- 行为发生变化时，同时核对受影响的专题文档：`docs/release-signing-and-updates.md`（更新链路，含一键点击契约）、`docs/secondary-notifications.md`、`docs/crash-reporting.md`，以及 `README.md` 中描述该行为的段落。
- `.plan/feat/*.md`、`plan/*.md` 属历史设计/审查记录，其中的旧版本号和旧决策**按原样保留**，只在文首「当前版本」这类现在时声明上更新，并在需要时追加一节说明后续版本的取代关系。

# 发布签名与安全自动更新

本文说明 Stock IPO Reminder 的正式 Authenticode 签名和稳定版自动更新流程。未提供正式证书和 HTTPS 更新源时，构建仍可用于内部测试，但 `release-manifest.json` 会明确记录 `signed: false`，应用内自动更新保持关闭。

## 信任模型

- EXE 与 MSI 使用同一张具有 Code Signing EKU 的 Authenticode 证书签名，并使用 RFC 3161 HTTPS 时间戳。
- 构建时把签名证书的 SHA-256 指纹和稳定版更新清单 HTTPS URL 编译进 EXE。
- `update-manifest.json` 使用同一证书生成 detached CMS/PKCS#7 签名 `update-manifest.json.p7s`。
- 客户端先验证 CMS 签名和固定证书指纹，再检查产品、stable 通道、版本、最低 Windows Build、MSI 大小和 SHA-256。
- 下载完成后再次验证 MSI SHA-256、Windows Authenticode 信任和固定证书指纹，全部通过后才允许用户明确启动安装。
- 更新只允许升级到更高的 `x.y.z` 版本；WiX Major Upgrade 继续负责程序文件事务回滚。数据迁移前仍由应用创建并校验 SQLite 备份。
- 便携版不会静默转换为安装版，应用内自动更新入口只对已由本产品 MSI 注册的安装版开放。

## 本地或隔离签名机

优先把私钥导入签名机当前用户证书存储，并只传递 SHA-1 证书查找指纹：

```text
set STOCK_IPO_SIGNING_CERTIFICATE_THUMBPRINT=<certificate SHA-1 thumbprint>
set STOCK_IPO_UPDATE_FEED_URL=https://updates.example.com/stock-ipo-reminder/stable/update-manifest.json
rtk cmd /c build.bat --package --sign
```

也可以通过短生命周期 PFX 文件签名。密码只能通过指定环境变量传递，不写入命令、仓库或日志。构建脚本把 PFX 临时导入当前用户证书存储，`signtool` 仅按指纹选取证书，完成后删除临时导入项，密码不会作为 `signtool` 命令行参数出现：

```text
set STOCK_IPO_SIGNING_PFX_PATH=D:\secure\stock-ipo-reminder-signing.pfx
set STOCK_IPO_SIGNING_PFX_PASSWORD=<secret>
set STOCK_IPO_UPDATE_FEED_URL=https://updates.example.com/stock-ipo-reminder/stable/update-manifest.json
rtk cmd /c build.bat --package --sign
```

签名构建会拒绝缺少 Code Signing EKU、缺少私钥、非 HTTPS 时间戳或非 HTTPS 更新源。默认时间戳服务为 `https://timestamp.digicert.com`，可通过 `scripts/build-release.ps1 -TimestampUrl` 显式替换。

## 发布文件

签名发布目录除 MSI、便携 ZIP、发布清单和校验和外，还包含：

```text
update-manifest.json
update-manifest.json.p7s
```

应先完整运行 smoke、签名/更新集成测试和 release audit，再把以下文件原子发布到稳定版更新目录：

```text
StockIpoReminder-<version>-win-x64.msi
RELEASE_NOTES.md
update-manifest.json.p7s
update-manifest.json
```

最后发布 `update-manifest.json`，避免客户端先看到尚未上传完整的版本。不得覆盖旧 MSI；保留至少一个已知稳定版本，供人工回滚和故障调查。

## CI 密钥保护

- 正式 PFX 只存放在受保护的 CI secret 或独立签名服务中，不提交 Base64、密码或私钥文件。
- 发布工作流仅允许手动触发，并应绑定需要审批的 GitHub Environment。
- PFX 只写入 runner 临时目录，签名完成后在 `finally` 中删除；构建产物中不得包含 PFX。
- 日志不得打印 PFX 密码、私钥内容或带凭据 URL。
- 发布前必须核对 `release-manifest.json` 的 `signed`、`signerSha256`、`timestampUrl`、更新清单文件名和所有 SHA-256。

## 验证

```text
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/test-signing-update.ps1
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-release.ps1
rtk powershell -NoProfile -ExecutionPolicy Bypass -File scripts/audit-release.ps1
```

`test-signing-update.ps1` 创建一天有效的临时代码签名证书，验证 Authenticode 文件签名、detached CMS、证书固定、安装包哈希和清单篡改拒绝，随后删除测试证书。测试只允许自测命令接受“唯一信任错误为临时自签根”；生产下载和安装路径仍强制要求 Windows 系统信任。它不替代正式 CA 证书、RFC 3161 时间戳和线上 HTTPS 更新源验收。

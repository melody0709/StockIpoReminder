# 脱敏崩溃报告共享

Stock IPO Reminder 的崩溃报告共享是可选能力，默认关闭。没有正式接收服务或隐私政策时，Watchdog 报告只保存在当前用户的 `diagnostics\crashes` 目录，不会离开电脑。

## 发布配置

构建必须同时提供两个无凭据 HTTPS URL；只提供其中一个、使用 HTTP 或在 URL 中嵌入用户名/密码时，客户端会把上传视为未配置：

```text
set STOCK_IPO_CRASH_REPORT_URL=https://reports.example.com/v1/crashes
set STOCK_IPO_CRASH_REPORT_PRIVACY_URL=https://reports.example.com/privacy
rtk cmd /c build.bat --package
```

受保护签名工作流使用同名 GitHub Environment variables。URL 不是私钥，但仍应由发布环境控制，避免非预期接收端进入正式二进制。`release-manifest.json` 会记录 `crashReportUrl` 和 `crashReportPrivacyUrl`；两者必须同时存在或同时为 `null`。

## 用户同意

- 设置默认值为关闭。
- 用户可明确点击“发送最近报告”，这是一次性主动操作。
- 用户开启“异常退出后自动发送下一份报告”并保存设置后，应用下次启动会尝试发送一份尚未成功发送的报告。
- 设置页会显示当前配置、最近结果、数据范围、24 小时限流和隐私政策入口。
- `--skip-crash-upload` 用于隔离 smoke，不改变正常用户设置。

## 客户端数据边界

客户端只接受当前数据目录下 `diagnostics\crashes` 顶层的 `crash-recovery-*.json`，单文件上限 128 KiB。它不会递归扫描任意目录，也不会上传诊断 ZIP、数据库、日志、公告、缓存或临时文件。

发送前会递归删除名称涉及以下内容的字段：

```text
authorization cookie password secret token command argument
path directory username hostname device account holding position credential
```

所有剩余字符串还会经过通用日志脱敏，移除 HTTP 查询参数、认证头和 Windows 绝对路径，并限制单字符串长度。载荷只包含产品、应用版本、平台常量、发送时间、原始报告 SHA-256 和二次脱敏后的 JSON；不生成或发送设备 ID。

## 网络与限流

- 请求只允许无凭据 HTTPS，30 秒超时，禁止 HTTP 重定向。
- 服务端只有返回 2xx 才视为成功。
- 客户端在实际网络请求前记录一次尝试，过去滚动 24 小时最多 3 次，失败也计入限流，避免故障循环产生上传风暴。
- 同一 SHA-256 报告成功后不重复发送；尝试记录最多保留 100 条、30 天，成功报告哈希单独保留最多 500 条且不包含报告正文。
- 最近结果写入 `diagnostics\crash-upload-last-result.json`，不包含响应正文或服务端秘密。

## 正式服务上线前必须补齐

客户端主路径不能替代服务端治理。正式接收端上线前仍必须完成：

1. 明确收集目的、字段、保留期限、访问权限、删除流程和联系渠道，并在隐私政策中公开。
2. 服务端请求体上限、速率限制、拒绝日志正文和未知字段、存储加密、访问审计及密钥轮换。
3. 不以 IP、User-Agent 或报告哈希拼接长期设备画像；基础设施日志的 IP 保留时间必须最小化并写入政策。
4. 提供删除/纠错渠道和事件响应流程；确认备份与日志中的数据也按期限删除。
5. 在隔离测试环境验证 2xx、4xx、5xx、超时、TLS 失败、证书错误、断网和服务端限流，确认客户端不会泄漏额外字段或无限重试。

在以上服务端证据完成前，计划文档应将此功能标记为“已实现，外部接入待补”，不得描述为已经上线。

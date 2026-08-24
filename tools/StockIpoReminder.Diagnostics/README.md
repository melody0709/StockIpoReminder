# 隔离联网诊断工具

此工具只用于人工契约探测和发布前诊断，不参与普通单元测试，也不启动后台 Host。

```powershell
rtk .\.tools\dotnet\dotnet.exe run --project tools\StockIpoReminder.Diagnostics -- --bse-sample --output artifacts\diagnostics\bse-sample.json
rtk .\.tools\dotnet\dotnet.exe run --project tools\StockIpoReminder.Diagnostics -- --sync --output artifacts\diagnostics\full-sync.json
```

每次运行会在系统临时目录创建唯一数据根目录和 SQLite。默认结束后删除；只有显式传入 `--keep` 才会保留。报告不包含原始响应、公告全文、Cookie、授权头、完整本地文件路径或 URL 查询参数。

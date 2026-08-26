# 固定响应样本

这些样本来自 2026-08-24 对公开网页后端的真实响应，并裁剪为单条或双条记录；字段值保持原样，外层分页结构与线上一致。它们只用于离线契约回归，普通测试不访问网络。

- `collectors/eastmoney-20260824.json`：东方财富 `RPTA_APP_IPOAPPLY`。
- `collectors/sse-20260824.json`：上交所 IPO 列表。
- `collectors/cninfo-20260824.json`：巨潮 IPO 列表。
- `collectors/bse-page0-20260824.jsonp`、`bse-page1-20260824.jsonp`：北交所 JSONP，保留日期、新旧代码和分页元数据。
- `announcements/sse-601123-20260824.json`：上交所发行公告检索。
- `announcements/cninfo-301688-20260824.json`：巨潮发行公告检索。
- `announcements/cninfo-301689-empty-20260824.json`：巨潮公告健康空结果契约。
- `announcements/cninfo-sse-603448-20260826.json`：巨潮沪市镜像发行公告检索，并包含一条错误证券代码用于身份隔离回归。
- `announcements/sse-javascript-challenge-20260826.html`：上交所静态 PDF 域名返回的 JavaScript Cookie 验证页裁剪样本。
- `announcements/bse-detail-920289-20260824.jsonp`：北交所公开发行详情接口。
- `announcements/bse-disclosure-920289-20260824.jsonp`：北交所发行信息披露兜底接口。
- `announcements/bse-920289-excerpt-20260820.txt`：正式发行公告关键字段摘录。

新增或更新样本时必须记录抓取日期，并同步校验关键字段和 schema fingerprint，避免接口改版被误判为“今日无新股”。

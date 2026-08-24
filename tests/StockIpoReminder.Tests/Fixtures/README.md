# 固定响应样本

这些样本来自 2026-08-24 对公开网页后端的真实响应，并裁剪为单条或双条记录；字段值保持原样，外层分页结构保持与线上一致。它们只用于离线契约回归，普通测试不访问网络。

- `Collectors/eastmoney-20260824.json`：东方财富 `RPTA_APP_IPOAPPLY`，电科思仪。
- `Collectors/sse-20260824.json`：上交所 IPO 列表，马矿股份、高凯技术。
- `Collectors/cninfo-20260824.json`：巨潮 IPO 列表，电科思仪。
- `Collectors/bse-page0-20260824.jsonp`、`bse-page1-20260824.jsonp`：北交所 JSONP，华汇智能、金钛股份；保留 epoch 日期、新旧代码及分页元数据。
- `Announcements/sse-601123-20260824.json`：上交所公告检索，马矿股份发行公告。
- `Announcements/cninfo-301688-20260824.json`：巨潮公告检索，格林生物发行公告。
- `Announcements/cninfo-301689-empty-20260824.json`：巨潮公告检索对证券 `301689` 的真实空结果契约裁剪；线上响应将 `announcements` 返回为 `null`，并以 `totalAnnouncement=0`、`totalRecordNum=0` 明确表示健康空结果。
- `Announcements/bse-detail-920289-20260824.jsonp`：北交所公开发行详情接口的真实裁剪响应，保留项目身份、无关附件、风险公告和正式发行公告。
- `Announcements/bse-disclosure-920289-20260824.jsonp`：北交所公开发行信息披露兜底接口的真实裁剪响应，华汇智能正式发行公告。
- `Announcements/bse-920289-excerpt-20260820.txt`：华汇智能正式发行公告的关键字段摘录，用于定向解析回归。

新增或更新样本时必须记录抓取日期，测试同时校验关键字段和 schema fingerprint，避免接口改版被误判为“今日无新股”。

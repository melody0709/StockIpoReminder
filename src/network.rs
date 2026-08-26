use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use reqwest::{
    blocking::{Client, ClientBuilder, Response},
    header::{RANGE, RETRY_AFTER},
};
use serde_json::Value;

use crate::{core::*, model::*};

pub struct CollectorOutput {
    pub source: &'static str,
    pub started: ChinaDateTime,
    pub raw: String,
    pub hash: String,
    pub schema: String,
    pub candidates: Vec<Candidate>,
    pub audit: CollectorAudit,
}

#[derive(Debug, Clone)]
pub struct CollectorAudit {
    pub declared_count: Option<usize>,
    pub detail_count: usize,
    pub accepted_count: usize,
    pub issues: Vec<String>,
}

impl CollectorAudit {
    pub fn state(&self) -> HealthState {
        if self.issues.is_empty() {
            HealthState::Healthy
        } else {
            HealthState::Warning
        }
    }

    pub fn summary(&self) -> Option<String> {
        (!self.issues.is_empty()).then(|| {
            format!(
                "采集计数/明细核验异常：declared={:?}, details={}, accepted={}；{}",
                self.declared_count,
                self.detail_count,
                self.accepted_count,
                self.issues.join("；")
            )
        })
    }
}

#[derive(Debug)]
pub struct HttpStatusError {
    status: u16,
    host: String,
    retry_after: Option<ChinaDateTime>,
}

impl fmt::Display for HttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "HTTP {}：{}", self.status, self.host)?;
        if let Some(retry_after) = self.retry_after {
            write!(
                formatter,
                "；Retry-After={}",
                retry_after.format("%Y-%m-%d %H:%M:%S %:z")
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for HttpStatusError {}

pub fn client() -> Result<Client> {
    Ok(ClientBuilder::new()
        .timeout(Duration::from_secs(45))
        .cookie_store(true)
        .user_agent(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) StockIpoReminder-Rust/",
            env!("CARGO_PKG_VERSION")
        ))
        .brotli(true)
        .gzip(true)
        .deflate(true)
        .build()?)
}

pub fn collect_all(client: &Client) -> Vec<Result<CollectorOutput>> {
    vec![
        collect_eastmoney(client),
        collect_sse(client),
        collect_cninfo(client),
        collect_bse(client),
    ]
}

pub fn probe_source(client: &Client, source: &str) -> Result<()> {
    let url = source_probe_url(source).context("未知数据源，无法执行低频健康探测")?;
    ensure_allowed(url, false)?;
    let response = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .timeout(Duration::from_secs(10))
        .send()?;
    let _ = checked_response(response, false)?;
    Ok(())
}

fn source_probe_url(source: &str) -> Option<&'static str> {
    match source {
        "eastmoney" => Some("https://www.eastmoney.com/"),
        "sse" | "sse-announcement" => Some("https://www.sse.com.cn/"),
        "cninfo" | "cninfo-announcement" => Some("https://www.cninfo.com.cn/"),
        "bse" | "bse-announcement" => Some("https://www.bseinfo.net/newshare/listofissues.html"),
        _ => None,
    }
}

pub fn collect_eastmoney(client: &Client) -> Result<CollectorOutput> {
    let started = now_china();
    let url = "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPTA_APP_IPOAPPLY&columns=ALL&sortColumns=APPLY_DATE%2CSECURITY_CODE&sortTypes=-1%2C-1&pageNumber=1&pageSize=500&source=WEB&client=WEB";
    ensure_allowed(url, false)?;
    let raw = get_text(client, url, None)?;
    let candidates = parse_eastmoney(&raw, started)?;
    Ok(output("eastmoney", started, raw, candidates))
}
pub fn collect_sse(client: &Client) -> Result<CollectorOutput> {
    let started = now_china();
    let url = "https://query.sse.com.cn/commonQuery.do?sqlId=COMMON_SSE_IPO_IPO_LIST_L&isPagination=true&pageHelp.pageNo=1&pageHelp.pageSize=500";
    let raw = get_text(client, url, Some("https://www.sse.com.cn/"))?;
    let candidates = parse_sse(&raw, started)?;
    Ok(output("sse", started, raw, candidates))
}
pub fn collect_cninfo(client: &Client) -> Result<CollectorOutput> {
    let started = now_china();
    let url = "https://www.cninfo.com.cn/neweipo/index/ipoListQuery";
    let raw = get_text(client, url, Some("https://www.cninfo.com.cn/new/index"))?;
    let candidates = parse_cninfo(&raw, started)?;
    Ok(output("cninfo", started, raw, candidates))
}
pub fn collect_bse(client: &Client) -> Result<CollectorOutput> {
    let started = now_china();
    let _ = get_text(
        client,
        "https://www.bseinfo.net/newshare/listofissues.html",
        None,
    )?;
    let mut page = 0;
    let mut total = 1;
    let mut raws = Vec::new();
    let mut all = Vec::new();
    let mut detail_count = 0usize;
    let mut declared_count = None;
    while page < total && page < 50 {
        let response = client
            .post("https://www.bseinfo.net/newShareController/infoResult.do?callback=ipoCb")
            .header(
                "Referer",
                "https://www.bseinfo.net/newshare/listofissues.html",
            )
            .form(&[
                ("statetypes", "1"),
                ("page", &page.to_string()),
                ("isNewThree", "1"),
                ("sortfield", "purchaseDate"),
                ("sorttype", "desc"),
            ])
            .send()?;
        let raw = response_text(response, false)?;
        let parsed = parse_bse_page_with_meta(&raw, started)?;
        detail_count += parsed.detail_count;
        declared_count = parsed.declared_count.or(declared_count);
        all.extend(parsed.candidates);
        raws.push(raw);
        total = parsed.total_pages;
        page += 1;
    }
    let raw = raws.join("\n");
    Ok(output_with_counts(
        "bse",
        started,
        raw,
        all,
        declared_count,
        detail_count,
    ))
}

fn output(
    source: &'static str,
    started: ChinaDateTime,
    raw: String,
    candidates: Vec<Candidate>,
) -> CollectorOutput {
    let (declared_count, detail_count) =
        response_counts(source, &raw).unwrap_or((None, candidates.len()));
    output_with_counts(
        source,
        started,
        raw,
        candidates,
        declared_count,
        detail_count,
    )
}

fn output_with_counts(
    source: &'static str,
    started: ChinaDateTime,
    raw: String,
    candidates: Vec<Candidate>,
    declared_count: Option<usize>,
    detail_count: usize,
) -> CollectorOutput {
    let schema = schema_fingerprint(&raw);
    let hash = sha256(raw.as_bytes());
    let audit = collector_audit(declared_count, detail_count, candidates.len());
    CollectorOutput {
        source,
        started,
        raw,
        hash,
        schema,
        candidates,
        audit,
    }
}

fn response_counts(source: &str, raw: &str) -> Result<(Option<usize>, usize)> {
    let root: Value = serde_json::from_str(raw)?;
    let (declared_count, details) = match source {
        "eastmoney" => (
            root.pointer("/result/count").and_then(Value::as_u64),
            root.pointer("/result/data").and_then(Value::as_array),
        ),
        "sse" => (
            root.pointer("/pageHelp/total").and_then(Value::as_u64),
            root.pointer("/pageHelp/data").and_then(Value::as_array),
        ),
        "cninfo" => (
            root.get("count").and_then(Value::as_u64),
            root.get("data").and_then(Value::as_array),
        ),
        _ => (None, None),
    };
    let details = details.context("采集响应缺少可计数的明细数组")?;
    Ok((declared_count.map(|value| value as usize), details.len()))
}

fn collector_audit(
    declared_count: Option<usize>,
    detail_count: usize,
    accepted_count: usize,
) -> CollectorAudit {
    let mut issues = Vec::new();
    if let Some(declared_count) = declared_count
        && declared_count != detail_count
    {
        issues.push(format!(
            "上游声明 {declared_count} 条，但本轮取得 {detail_count} 条明细"
        ));
    }
    if accepted_count != detail_count {
        issues.push(format!(
            "{detail_count} 条明细中仅 {accepted_count} 条通过身份和必填字段校验"
        ));
    }
    CollectorAudit {
        declared_count,
        detail_count,
        accepted_count,
        issues,
    }
}

pub fn parse_eastmoney(raw: &str, fetched: ChinaDateTime) -> Result<Vec<Candidate>> {
    let root: Value = serde_json::from_str(raw)?;
    if root.get("success").and_then(Value::as_bool) == Some(false) {
        bail!("东方财富响应 success=false")
    };
    let data = root
        .pointer("/result/data")
        .and_then(Value::as_array)
        .context("东方财富响应缺少 result.data")?;
    let today = fetched.date_naive();
    Ok(data
        .iter()
        .filter_map(|item| {
            let code = text(item, "SECURITY_CODE")?;
            let name = text(item, "SECURITY_NAME")?;
            let market = text(item, "MARKET_TYPE_NEW");
            let exchange = detect_exchange(
                Some(&code),
                market.as_deref(),
                integer(item, "IS_BEIJING", 1.0, false) == Some(1),
            );
            let apply_date = date(item, "APPLY_DATE");
            let state = text(item, "ISSUE_STATE");
            let status = match state.as_deref() {
                Some("2" | "暂停发行" | "暂缓发行") => IssueStatus::Suspended,
                Some("3" | "终止发行") => IssueStatus::Terminated,
                _ => status_from_dates(apply_date, today, false, false),
            };
            Some(Candidate {
                source: "eastmoney".into(),
                priority: 100,
                fetched_at: fetched,
                published_at: None,
                exchange,
                board: detect_board(exchange, Some(&code), market.as_deref()),
                security_code: Some(code),
                apply_code: text(item, "APPLY_CODE"),
                legacy_code: None,
                name: Some(name),
                apply_date,
                issue_price: number(item, "ISSUE_PRICE", false),
                lot_size: integer(item, "EACHBALLOT_SHARES", 1.0, true),
                max_apply_quantity: integer(item, "ONLINE_APPLY_UPPER", 1.0, true),
                required_market_value: number(item, "TOP_APPLY_MARKETCAP", true),
                required_cash: None,
                ballot_date: date(item, "BALLOT_NUM_DATE"),
                payment_date: date(item, "BALLOT_PAY_DATE"),
                listing_date: date(item, "LISTING_DATE"),
                status,
                announcement_url: None,
                sessions: vec![],
                announcement_derived: false,
            })
        })
        .collect())
}

pub fn parse_sse(raw: &str, fetched: ChinaDateTime) -> Result<Vec<Candidate>> {
    let root: Value = serde_json::from_str(raw)?;
    let data = root
        .pointer("/pageHelp/data")
        .and_then(Value::as_array)
        .context("上交所响应缺少 pageHelp.data")?;
    let today = fetched.date_naive();
    Ok(data
        .iter()
        .filter_map(|item| {
            let code = text(item, "SECURITY_CODE")?;
            let name = text(item, "SECURITY_NAME")?;
            let apply_date = date(item, "ONLINE_ISSUANCE_DATE");
            Some(Candidate {
                source: "sse".into(),
                priority: 200,
                fetched_at: fetched,
                published_at: None,
                exchange: Exchange::Shanghai,
                board: detect_board(Exchange::Shanghai, Some(&code), None),
                security_code: Some(code),
                apply_code: None,
                legacy_code: None,
                name: Some(name),
                apply_date,
                issue_price: number(item, "ISSUE_PRICE", true),
                lot_size: None,
                max_apply_quantity: integer(item, "ONLINE_PURCHASE_LIMIT", 10_000.0, true),
                required_market_value: None,
                required_cash: None,
                ballot_date: None,
                payment_date: date(item, "PAYMENT_START_DATE"),
                listing_date: date(item, "LISTED_DATE"),
                status: if matches!(text(item, "IPO_OVERALL_STATUS").as_deref(), Some("3" | "4")) {
                    IssueStatus::Terminated
                } else {
                    status_from_dates(apply_date, today, false, false)
                },
                announcement_url: text(item, "ANNOUNCEMENT_URL"),
                sessions: vec![],
                announcement_derived: false,
            })
        })
        .collect())
}

pub fn parse_cninfo(raw: &str, fetched: ChinaDateTime) -> Result<Vec<Candidate>> {
    let root: Value = serde_json::from_str(raw)?;
    if root.get("code").and_then(Value::as_i64) != Some(200) {
        bail!("巨潮响应状态异常")
    };
    let data = root
        .get("data")
        .and_then(Value::as_array)
        .context("巨潮响应缺少 data")?;
    let today = fetched.date_naive();
    Ok(data
        .iter()
        .filter_map(|item| {
            let code = text(item, "obSecCode0007")?;
            let name = text(item, "obSecName0007")?;
            let apply_date = date(item, "f035d0089Date");
            Some(Candidate {
                source: "cninfo".into(),
                priority: 200,
                fetched_at: fetched,
                published_at: None,
                exchange: Exchange::Shenzhen,
                board: detect_board(Exchange::Shenzhen, Some(&code), None),
                security_code: Some(code.clone()),
                apply_code: Some(code),
                legacy_code: None,
                name: Some(name),
                apply_date,
                issue_price: number(item, "f008n0089", true),
                lot_size: None,
                max_apply_quantity: integer(item, "f042n0089", 10_000.0, true),
                required_market_value: None,
                required_cash: None,
                ballot_date: date(item, "f108d0089"),
                payment_date: None,
                listing_date: date(item, "f007d0007"),
                status: status_from_dates(apply_date, today, false, false),
                announcement_url: None,
                sessions: vec![],
                announcement_derived: false,
            })
        })
        .collect())
}

struct BsePage {
    candidates: Vec<Candidate>,
    total_pages: usize,
    declared_count: Option<usize>,
    detail_count: usize,
}

pub fn parse_bse_page(raw: &str, fetched: ChinaDateTime) -> Result<(Vec<Candidate>, usize)> {
    let page = parse_bse_page_with_meta(raw, fetched)?;
    Ok((page.candidates, page.total_pages))
}

fn parse_bse_page_with_meta(raw: &str, fetched: ChinaDateTime) -> Result<BsePage> {
    let json = unwrap_jsonp(raw)?;
    let root: Value = serde_json::from_str(json)?;
    let payload = root
        .as_array()
        .and_then(|a| a.first())
        .context("北交所响应不是非空数组")?;
    let info = payload.get("listInfo").context("北交所响应缺少 listInfo")?;
    let data = info
        .get("content")
        .and_then(Value::as_array)
        .context("北交所响应缺少 listInfo.content")?;
    let detail_count = data.len();
    let today = fetched.date_naive();
    let candidates = data
        .iter()
        .filter_map(|item| {
            let code = text(item, "fxCode")?;
            let name = text(item, "stockName")?;
            let apply = epoch_date(item, "purchaseDate");
            Some(Candidate {
                source: "bse".into(),
                priority: 200,
                fetched_at: fetched,
                published_at: None,
                exchange: Exchange::Beijing,
                board: Board::Beijing,
                security_code: Some(code.clone()),
                apply_code: Some(code),
                legacy_code: text(item, "stockCode"),
                name: Some(name),
                apply_date: apply,
                issue_price: number(item, "issuePrice", true),
                lot_size: None,
                max_apply_quantity: None,
                required_market_value: None,
                required_cash: None,
                ballot_date: epoch_date(item, "issueResultDate"),
                payment_date: None,
                listing_date: epoch_date(item, "enterPremiumDate"),
                status: status_from_dates(
                    apply,
                    today,
                    epoch_date(item, "suspendDate").is_some(),
                    epoch_date(item, "terminationDate").is_some(),
                ),
                announcement_url: text(item, "id").map(|id| {
                    format!("https://www.bseinfo.net/newshare/listofissues_detail.html?id={id}")
                }),
                sessions: vec![],
                announcement_derived: false,
            })
        })
        .collect();
    Ok(BsePage {
        candidates,
        total_pages: info
            .get("totalPages")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize,
        declared_count: info
            .get("totalElements")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        detail_count,
    })
}

pub fn get_text(client: &Client, url: &str, referer: Option<&str>) -> Result<String> {
    ensure_allowed(url, false)?;
    let mut request = client.get(url);
    if let Some(referer) = referer {
        request = request.header("Referer", referer);
    }
    response_text(request.send()?, false)
}

pub fn checked_response(response: Response, announcement: bool) -> Result<Response> {
    ensure_allowed(response.url().as_str(), announcement)?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let host = response.url().host_str().unwrap_or_default().to_owned();
    let retry_after = if matches!(status, 429 | 503) {
        parse_retry_after(response.headers().get(RETRY_AFTER), now_china())
    } else {
        None
    };
    Err(HttpStatusError {
        status,
        host,
        retry_after,
    }
    .into())
}

pub fn retry_after_from_error(error: &anyhow::Error) -> Option<ChinaDateTime> {
    error
        .downcast_ref::<HttpStatusError>()
        .and_then(|failure| failure.retry_after)
}

fn response_text(response: Response, announcement: bool) -> Result<String> {
    Ok(checked_response(response, announcement)?.text()?)
}

fn parse_retry_after(
    value: Option<&reqwest::header::HeaderValue>,
    now: ChinaDateTime,
) -> Option<ChinaDateTime> {
    let value = value?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        return (seconds >= 0).then(|| now + chrono::Duration::seconds(seconds));
    }
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|value| value.with_timezone(&china_offset()))
}
pub fn ensure_allowed(value: &str, announcement: bool) -> Result<()> {
    let url = url::Url::parse(value)?;
    let allowed = if announcement {
        ANNOUNCEMENT_HOSTS
    } else {
        DATA_HOSTS
    };
    if url.scheme() != "https" || !allowed.contains(&url.host_str().unwrap_or_default()) {
        bail!(
            "拒绝访问白名单外地址：{}",
            url.host_str().unwrap_or_default()
        )
    }
    Ok(())
}
const DATA_HOSTS: &[&str] = &[
    "www.eastmoney.com",
    "datacenter-web.eastmoney.com",
    "query.sse.com.cn",
    "www.sse.com.cn",
    "www.cninfo.com.cn",
    "static.cninfo.com.cn",
    "disc.static.szse.cn",
    "www.bseinfo.net",
    "www.bse.cn",
    "www.microsoft.com",
    "www.cloudflare.com",
];
const ANNOUNCEMENT_HOSTS: &[&str] = &[
    "query.sse.com.cn",
    "www.sse.com.cn",
    "static.sse.com.cn",
    "www.cninfo.com.cn",
    "static.cninfo.com.cn",
    "disc.static.szse.cn",
    "www.bseinfo.net",
    "bseinfo.net",
    "www.bse.cn",
    "bse.cn",
];

pub fn text(item: &Value, key: &str) -> Option<String> {
    let value = item.get(key)?;
    let text = match value {
        Value::String(v) => v.clone(),
        Value::Null => return None,
        _ => value.to_string().trim_matches('"').to_owned(),
    };
    let text = text.trim();
    if text.is_empty() || matches!(text, "-" | "--" | "N/A" | "NULL" | "无") {
        None
    } else {
        Some(text.to_owned())
    }
}
fn number(item: &Value, key: &str, zero_missing: bool) -> Option<f64> {
    let value = text(item, key)?.replace(',', "").parse().ok()?;
    if zero_missing && value == 0.0 {
        None
    } else {
        Some(value)
    }
}
fn integer(item: &Value, key: &str, multiplier: f64, zero_missing: bool) -> Option<i64> {
    let value = number(item, key, zero_missing)? * multiplier;
    Some(value.round() as i64)
}
fn date(item: &Value, key: &str) -> Option<NaiveDate> {
    parse_date(&text(item, key)?)
}
fn epoch_date(item: &Value, key: &str) -> Option<NaiveDate> {
    let millis = item.get(key)?.get("time")?.as_i64()?;
    Some(
        Utc.timestamp_millis_opt(millis)
            .single()?
            .with_timezone(&china_offset())
            .date_naive(),
    )
}
fn unwrap_jsonp(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        return Ok(trimmed);
    }
    let start = trimmed.find('(').context("无效 JSONP")?;
    let end = trimmed.rfind(')').context("无效 JSONP")?;
    Ok(&trimmed[start + 1..end])
}
fn schema_fingerprint(raw: &str) -> String {
    let mut keys = BTreeSet::new();
    if let Ok(value) = serde_json::from_str::<Value>(raw)
        .or_else(|_| unwrap_jsonp(raw).and_then(|v| serde_json::from_str(v).map_err(Into::into)))
    {
        collect_keys(&value, &mut keys);
    }
    sha256(keys.into_iter().collect::<Vec<_>>().join("\n"))
}
fn collect_keys(value: &Value, keys: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                keys.insert(key.clone());
                collect_keys(value, keys)
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_keys(item, keys)
            }
        }
        _ => {}
    }
}

pub fn encode_query(values: &HashMap<&str, String>) -> String {
    values
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn fixture(path: &str) -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/collectors")
                .join(path),
        )
        .unwrap()
    }

    #[test]
    fn parses_all_collector_fixtures() {
        let fetched = crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
            crate::model::time(12, 0),
        );
        assert!(
            !parse_eastmoney(&fixture("eastmoney-20260824.json"), fetched)
                .unwrap()
                .is_empty()
        );
        assert!(
            !parse_sse(&fixture("sse-20260824.json"), fetched)
                .unwrap()
                .is_empty()
        );
        assert!(
            !parse_cninfo(&fixture("cninfo-20260824.json"), fetched)
                .unwrap()
                .is_empty()
        );
        let (page0, _) = parse_bse_page(&fixture("bse-page0-20260824.jsonp"), fetched).unwrap();
        let (page1, _) = parse_bse_page(&fixture("bse-page1-20260824.jsonp"), fetched).unwrap();
        assert!(!page0.is_empty() || !page1.is_empty());
    }

    #[test]
    fn collector_audit_detects_declared_detail_and_parser_count_mismatches() {
        let raw = fixture("sse-20260824.json");
        let fetched = crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
            crate::model::time(12, 0),
        );
        let accepted = parse_sse(&raw, fetched).unwrap().len();
        let (declared, details) = response_counts("sse", &raw).unwrap();
        let healthy = collector_audit(declared, details, accepted);
        assert_eq!(healthy.state(), HealthState::Healthy);
        assert!(healthy.summary().is_none());

        let truncated = collector_audit(Some(2), 1, 1);
        assert_eq!(truncated.state(), HealthState::Warning);
        assert!(truncated.summary().unwrap().contains("声明 2 条"));

        let rejected = collector_audit(None, 2, 1);
        assert_eq!(rejected.state(), HealthState::Warning);
        assert!(rejected.summary().unwrap().contains("仅 1 条通过"));
    }

    #[test]
    fn rejects_html_and_non_allowlisted_hosts() {
        assert!(parse_sse("<html>WAF</html>", crate::core::now_china()).is_err());
        assert!(ensure_allowed("https://example.com/file.pdf", true).is_err());
        assert!(ensure_allowed("http://www.sse.com.cn/file.pdf", true).is_err());
        assert!(ensure_allowed("https://query.sse.com.cn/query", true).is_ok());
        assert!(ensure_allowed("https://static.sse.com.cn/file.pdf", true).is_ok());
        assert!(ensure_allowed("https://www.cninfo.com.cn/query", true).is_ok());
    }

    #[test]
    fn parses_retry_after_delta_seconds_and_http_date() {
        use reqwest::header::HeaderValue;

        let now = crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
            crate::model::time(12, 0),
        );
        assert_eq!(
            parse_retry_after(Some(&HeaderValue::from_static("120")), now),
            Some(now + chrono::Duration::minutes(2))
        );
        assert_eq!(
            parse_retry_after(
                Some(&HeaderValue::from_static("Wed, 26 Aug 2026 04:05:00 GMT")),
                now,
            ),
            Some(now + chrono::Duration::minutes(5))
        );
        assert_eq!(
            parse_retry_after(Some(&HeaderValue::from_static("-1")), now),
            None
        );
    }

    #[test]
    fn low_frequency_probe_targets_are_allowlisted_and_source_specific() {
        for source in [
            "eastmoney",
            "sse",
            "cninfo",
            "bse",
            "sse-announcement",
            "cninfo-announcement",
            "bse-announcement",
        ] {
            let url = source_probe_url(source).unwrap();
            assert!(ensure_allowed(url, false).is_ok(), "source={source}");
        }
        assert!(source_probe_url("unknown").is_none());
    }
}

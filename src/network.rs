use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::blocking::{Client, ClientBuilder};
use serde_json::Value;

use crate::{core::*, model::*};

pub struct CollectorOutput {
    pub source: &'static str,
    pub started: ChinaDateTime,
    pub raw: String,
    pub hash: String,
    pub schema: String,
    pub candidates: Vec<Candidate>,
}

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
    while page < total && page < 50 {
        let raw = client
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
            .send()?
            .error_for_status()?
            .text()?;
        let (parsed, pages) = parse_bse_page(&raw, started)?;
        all.extend(parsed);
        raws.push(raw);
        total = pages;
        page += 1;
    }
    let raw = raws.join("\n");
    Ok(output("bse", started, raw, all))
}

fn output(
    source: &'static str,
    started: ChinaDateTime,
    raw: String,
    candidates: Vec<Candidate>,
) -> CollectorOutput {
    let schema = schema_fingerprint(&raw);
    let hash = sha256(raw.as_bytes());
    CollectorOutput {
        source,
        started,
        raw,
        hash,
        schema,
        candidates,
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

pub fn parse_bse_page(raw: &str, fetched: ChinaDateTime) -> Result<(Vec<Candidate>, usize)> {
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
    Ok((
        candidates,
        info.get("totalPages")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize,
    ))
}

pub fn get_text(client: &Client, url: &str, referer: Option<&str>) -> Result<String> {
    ensure_allowed(url, false)?;
    let mut request = client.get(url);
    if let Some(referer) = referer {
        request = request.header("Referer", referer);
    }
    Ok(request.send()?.error_for_status()?.text()?)
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
    "www.sse.com.cn",
    "static.cninfo.com.cn",
    "disc.static.szse.cn",
    "www.bseinfo.net",
    "www.bse.cn",
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
    fn rejects_html_and_non_allowlisted_hosts() {
        assert!(parse_sse("<html>WAF</html>", crate::core::now_china()).is_err());
        assert!(ensure_allowed("https://example.com/file.pdf", true).is_err());
        assert!(ensure_allowed("http://www.sse.com.cn/file.pdf", true).is_err());
    }
}

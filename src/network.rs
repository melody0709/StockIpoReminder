use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    io::Read,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use reqwest::{
    blocking::{Client, ClientBuilder, Response},
    header::{RANGE, RETRY_AFTER},
    redirect::Policy,
};
use serde_json::Value;

use crate::{core::*, model::*};

const IPO_WINDOW_PAST_DAYS: i64 = 60;
const IPO_WINDOW_FUTURE_DAYS: i64 = 60;
const MAX_BOUNDED_PAGES: usize = 5;
const EASTMONEY_PAGE_SIZE: usize = 100;
const SSE_PAGE_SIZE: usize = 100;
const MAX_DATA_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ANNOUNCEMENT_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const MAX_RETRY_AFTER_SECONDS: i64 = 24 * 60 * 60;
const EASTMONEY_COLUMNS: &str = "SECURITY_CODE,SECURITY_NAME,MARKET_TYPE_NEW,IS_BEIJING,APPLY_DATE,ISSUE_STATE,APPLY_CODE,ISSUE_PRICE,EACHBALLOT_SHARES,ONLINE_APPLY_UPPER,TOP_APPLY_MARKETCAP,BALLOT_NUM_DATE,BALLOT_PAY_DATE,LISTING_DATE";
const BSE_COLUMNS: &str = "id,fxCode,stockCode,stockName,purchaseDate,issuePrice,issueResultDate,enterPremiumDate,suspendDate,terminationDate";

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
    build_client(business_redirect_allowed)
}

pub fn time_client() -> Result<Client> {
    build_client(time_redirect_allowed)
}

fn build_client(redirect_allowed: fn(&url::Url) -> bool) -> Result<Client> {
    Ok(ClientBuilder::new()
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("重定向次数超过上限")
            } else if redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("重定向目标不在 HTTPS 白名单内")
            }
        }))
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

fn ipo_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (
        today - chrono::Duration::days(IPO_WINDOW_PAST_DAYS),
        today + chrono::Duration::days(IPO_WINDOW_FUTURE_DAYS),
    )
}

fn eastmoney_url(today: NaiveDate, page: usize) -> Result<String> {
    let (from, to) = ipo_window(today);
    let filter = format!(
        "(APPLY_DATE>='{}')(APPLY_DATE<='{}')",
        from.format("%Y-%m-%d"),
        to.format("%Y-%m-%d")
    );
    let mut url = url::Url::parse("https://datacenter-web.eastmoney.com/api/data/v1/get")?;
    url.query_pairs_mut()
        .append_pair("reportName", "RPTA_APP_IPOAPPLY")
        .append_pair("columns", EASTMONEY_COLUMNS)
        .append_pair("sortColumns", "APPLY_DATE,SECURITY_CODE")
        .append_pair("sortTypes", "-1,-1")
        .append_pair("pageNumber", &page.to_string())
        .append_pair("pageSize", &EASTMONEY_PAGE_SIZE.to_string())
        .append_pair("source", "WEB")
        .append_pair("client", "WEB")
        .append_pair("filter", &filter);
    Ok(url.into())
}

fn sse_url(page: usize) -> Result<String> {
    let mut url = url::Url::parse("https://query.sse.com.cn/commonQuery.do")?;
    url.query_pairs_mut()
        .append_pair("sqlId", "COMMON_SSE_IPO_IPO_LIST_L")
        .append_pair("isPagination", "true")
        .append_pair("pageHelp.pageNo", &page.to_string())
        .append_pair("pageHelp.pageSize", &SSE_PAGE_SIZE.to_string())
        .append_pair("pageHelp.cacheSize", "1")
        .append_pair("isIssue", "1");
    Ok(url.into())
}

pub fn collect_eastmoney(client: &Client, cancelled: &dyn Fn() -> bool) -> Result<CollectorOutput> {
    let started = now_china();
    let mut page = 1usize;
    let mut total_pages = 1usize;
    let mut declared_count = None;
    let mut detail_count = 0usize;
    let mut raws = Vec::new();
    let mut candidates = Vec::new();
    while page <= total_pages && page <= MAX_BOUNDED_PAGES {
        ensure_not_cancelled(cancelled)?;
        let url = eastmoney_url(started.date_naive(), page)?;
        let raw = get_text(client, &url, None, cancelled)?;
        ensure_not_cancelled(cancelled)?;
        let (page_declared, page_details, pages) = eastmoney_page_counts(&raw)?;
        declared_count = declared_count.or(page_declared);
        detail_count += page_details;
        total_pages = pages;
        candidates.extend(parse_eastmoney(&raw, started)?);
        raws.push(raw);
        page += 1;
    }
    Ok(output_with_counts(
        "eastmoney",
        started,
        combine_raw_pages(raws),
        candidates,
        declared_count,
        detail_count,
    ))
}
pub fn collect_sse(client: &Client, cancelled: &dyn Fn() -> bool) -> Result<CollectorOutput> {
    let started = now_china();
    let mut page = 1usize;
    let mut total_pages = 1usize;
    let mut declared_count = None;
    let mut detail_count = 0usize;
    let mut raws = Vec::new();
    let mut candidates = Vec::new();
    while page <= total_pages && page <= MAX_BOUNDED_PAGES {
        ensure_not_cancelled(cancelled)?;
        let url = sse_url(page)?;
        let raw = get_text(
            client,
            &url,
            Some("https://www.sse.com.cn/ipo/listing/"),
            cancelled,
        )?;
        ensure_not_cancelled(cancelled)?;
        let (page_declared, page_details, pages) = sse_page_counts(&raw)?;
        declared_count = declared_count.or(page_declared);
        detail_count += page_details;
        total_pages = pages;
        candidates.extend(parse_sse(&raw, started)?);
        raws.push(raw);
        page += 1;
    }
    Ok(output_with_counts(
        "sse",
        started,
        combine_raw_pages(raws),
        candidates,
        declared_count,
        detail_count,
    ))
}
pub fn collect_cninfo(client: &Client, cancelled: &dyn Fn() -> bool) -> Result<CollectorOutput> {
    let started = now_china();
    let url = "https://www.cninfo.com.cn/neweipo/index/ipoListQuery";
    ensure_not_cancelled(cancelled)?;
    let raw = get_text(
        client,
        url,
        Some("https://www.cninfo.com.cn/new/index"),
        cancelled,
    )?;
    ensure_not_cancelled(cancelled)?;
    let candidates = parse_cninfo(&raw, started)?;
    Ok(output("cninfo", started, raw, candidates))
}
pub fn collect_bse(client: &Client, cancelled: &dyn Fn() -> bool) -> Result<CollectorOutput> {
    let started = now_china();
    let (from, to) = ipo_window(started.date_naive());
    let from = from.format("%Y-%m-%d").to_string();
    let to = to.format("%Y-%m-%d").to_string();
    ensure_not_cancelled(cancelled)?;
    let _ = get_text(
        client,
        "https://www.bseinfo.net/newshare/listofissues.html",
        None,
        cancelled,
    )?;
    ensure_not_cancelled(cancelled)?;
    let mut page = 0;
    let mut total = 1;
    let mut raws = Vec::new();
    let mut all = Vec::new();
    let mut detail_count = 0usize;
    let mut declared_count = None;
    while page < total && page < MAX_BOUNDED_PAGES {
        ensure_not_cancelled(cancelled)?;
        let page_number = page.to_string();
        let response = client
            .post("https://www.bseinfo.net/newShareController/infoResult.do?callback=ipoCb")
            .header(
                "Referer",
                "https://www.bseinfo.net/newshare/listofissues.html",
            )
            .form(&[
                ("statetypes", "1"),
                ("page", page_number.as_str()),
                ("isNewThree", "1"),
                ("sortfield", "purchaseDate"),
                ("sorttype", "desc"),
                ("startTime", from.as_str()),
                ("endTime", to.as_str()),
                ("needFields", BSE_COLUMNS),
            ])
            .send()?;
        ensure_not_cancelled(cancelled)?;
        let raw = response_text(response, false, cancelled)?;
        let parsed = parse_bse_page_with_meta(&raw, started)?;
        detail_count += parsed.detail_count;
        declared_count = parsed.declared_count.or(declared_count);
        all.extend(parsed.candidates);
        raws.push(raw);
        total = parsed.total_pages;
        page += 1;
    }
    let raw = combine_raw_pages(raws);
    Ok(output_with_counts(
        "bse",
        started,
        raw,
        all,
        declared_count,
        detail_count,
    ))
}

fn ensure_not_cancelled(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        bail!("同步已取消")
    }
    Ok(())
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

fn eastmoney_page_counts(raw: &str) -> Result<(Option<usize>, usize, usize)> {
    let root: Value = serde_json::from_str(raw)?;
    if eastmoney_empty(&root) {
        return Ok((Some(0), 0, 0));
    }
    if root.get("success").and_then(Value::as_bool) == Some(false) {
        bail!("东方财富响应 success=false")
    }
    let result = root
        .get("result")
        .and_then(Value::as_object)
        .context("东方财富响应缺少 result")?;
    let details = result
        .get("data")
        .and_then(Value::as_array)
        .context("东方财富响应缺少 result.data")?;
    Ok((
        result
            .get("count")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        details.len(),
        result.get("pages").and_then(Value::as_u64).unwrap_or(1) as usize,
    ))
}

fn sse_page_counts(raw: &str) -> Result<(Option<usize>, usize, usize)> {
    let root: Value = serde_json::from_str(raw)?;
    let page = root
        .get("pageHelp")
        .and_then(Value::as_object)
        .context("上交所响应缺少 pageHelp")?;
    let details = page
        .get("data")
        .and_then(Value::as_array)
        .context("上交所响应缺少 pageHelp.data")?;
    Ok((
        page.get("total")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        details.len(),
        page.get("pageCount").and_then(Value::as_u64).unwrap_or(1) as usize,
    ))
}

fn eastmoney_empty(root: &Value) -> bool {
    root.get("success").and_then(Value::as_bool) == Some(false)
        && root.get("code").and_then(Value::as_i64) == Some(9201)
        && root.get("result").is_none_or(Value::is_null)
}

fn combine_raw_pages(raws: Vec<String>) -> String {
    let pages = raws
        .iter()
        .map(|raw| parse_payload(raw))
        .collect::<Result<Vec<_>>>();
    match pages {
        Ok(pages) => Value::Array(pages).to_string(),
        Err(_) => raws.join("\n"),
    }
}

fn response_counts(source: &str, raw: &str) -> Result<(Option<usize>, usize)> {
    if source == "eastmoney" {
        let (declared_count, detail_count, _) = eastmoney_page_counts(raw)?;
        return Ok((declared_count, detail_count));
    }
    if source == "sse" {
        let (declared_count, detail_count, _) = sse_page_counts(raw)?;
        return Ok((declared_count, detail_count));
    }
    let root: Value = serde_json::from_str(raw)?;
    let (declared_count, details) = match source {
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
    if eastmoney_empty(&root) {
        return Ok(Vec::new());
    }
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

#[cfg(test)]
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

pub fn get_text(
    client: &Client,
    url: &str,
    referer: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    ensure_allowed(url, false)?;
    let mut request = client.get(url);
    if let Some(referer) = referer {
        request = request.header("Referer", referer);
    }
    response_text(request.send()?, false, cancelled)
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

pub(crate) fn response_text(
    response: Response,
    announcement: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    let mut response = checked_response(response, announcement)?;
    let limit = if announcement {
        MAX_ANNOUNCEMENT_RESPONSE_BYTES
    } else {
        MAX_DATA_RESPONSE_BYTES
    };
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("远端响应超过 {} MiB 大小上限", limit / 1024 / 1024);
    }
    let charset = declared_charset(response.headers());
    let bytes = read_limited(&mut response, limit, Some(cancelled))?;
    decode_response_bytes(&bytes, charset)
}

/// 从 Content-Type 提取 charset 参数（小写化，去掉引号）。
fn declared_charset(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|content_type| {
            content_type.split(';').skip(1).find_map(|parameter| {
                let parameter = parameter.trim();
                let (name, value) = parameter.split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("charset")
                    .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
            })
        })
}

/// 按声明字符集解码响应体。仅接受 UTF-8 与显式声明的 GBK/GB2312/GB18030；
/// 对无声明的非法 UTF-8 不做猜测式兜底，只报错并附有限长度 hex 诊断。
fn decode_response_bytes(bytes: &[u8], charset: Option<String>) -> Result<String> {
    const HEX_PREVIEW_BYTES: usize = 48;
    let hex_preview = |data: &[u8]| -> String {
        data.iter()
            .take(HEX_PREVIEW_BYTES)
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let declared = charset.as_deref().unwrap_or_default();
    if matches!(declared, "gbk" | "gb2312" | "gb18030") {
        // GB18030 是 GBK/GB2312 的官方超集，统一按 GB18030 解码。
        let (decoded, _, had_errors) = encoding_rs::GB18030.decode(bytes);
        if had_errors {
            bail!(
                "远端响应按 GB18030 解码存在无法映射的字节，前 {HEX_PREVIEW_BYTES} 字节 hex：{}",
                hex_preview(bytes)
            );
        }
        Ok(decoded.into_owned())
    } else {
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => Ok(text),
            Err(error) => {
                let bytes = error.into_bytes();
                bail!(
                    "远端响应不是有效 UTF-8（charset 声明：{}），前 {HEX_PREVIEW_BYTES} 字节 hex：{}",
                    if declared.is_empty() {
                        "未声明"
                    } else {
                        declared
                    },
                    hex_preview(&bytes)
                );
            }
        }
    }
}

/// 分块读取响应体：每次成功 read 后检查取消标志，退出请求不再需要等待
/// 整段响应读完。逐次 read 的 stall 超时与体积上限保持不变。
fn read_limited(
    reader: &mut impl Read,
    limit: u64,
    cancelled: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    const CHUNK_SIZE: usize = 64 * 1024;
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];
    loop {
        if cancelled.is_some_and(|check| check()) {
            bail!("同步已取消：读取响应体时检测到退出请求");
        }
        let read = reader.read(&mut chunk).context("无法读取远端响应")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() as u64 > limit {
            bail!("远端响应超过 {} MiB 大小上限", limit / 1024 / 1024);
        }
    }
    Ok(bytes)
}

fn parse_retry_after(
    value: Option<&reqwest::header::HeaderValue>,
    now: ChinaDateTime,
) -> Option<ChinaDateTime> {
    let value = value?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        if !(0..=MAX_RETRY_AFTER_SECONDS).contains(&seconds) {
            return None;
        }
        return chrono::Duration::try_seconds(seconds)
            .and_then(|delay| now.checked_add_signed(delay));
    }
    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|value| value.with_timezone(&china_offset()))?;
    let maximum = now.checked_add_signed(chrono::Duration::seconds(MAX_RETRY_AFTER_SECONDS))?;
    (retry_at >= now && retry_at <= maximum).then_some(retry_at)
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
];
const TIME_HOSTS: &[&str] = &["www.microsoft.com", "www.cloudflare.com"];
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

fn business_redirect_allowed(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| DATA_HOSTS.contains(&host) || ANNOUNCEMENT_HOSTS.contains(&host))
}

fn time_redirect_allowed(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| TIME_HOSTS.contains(&host))
}

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
    let content_start = start + 1;
    if end < content_start {
        bail!("无效 JSONP：右括号位于左括号之前");
    }
    Ok(&trimmed[content_start..end])
}

fn parse_payload(raw: &str) -> Result<Value> {
    serde_json::from_str(raw)
        .or_else(|_| unwrap_jsonp(raw).and_then(|value| Ok(serde_json::from_str(value)?)))
}

fn schema_fingerprint(raw: &str) -> String {
    let mut keys = BTreeSet::new();
    if let Ok(value) = parse_payload(raw) {
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

    #[test]
    fn declared_charset_parses_content_type_parameters() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json; Charset=\"GBK\""),
        );
        assert_eq!(declared_charset(&headers).as_deref(), Some("gbk"));
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        assert_eq!(declared_charset(&headers), None);
    }

    #[test]
    fn decode_response_bytes_only_honors_declared_gb_charsets() {
        assert_eq!(decode_response_bytes(b"plain", None).unwrap(), "plain");
        let gb_bytes = vec![0xD6, 0xD0]; // “中”的 GBK 编码
        assert_eq!(
            decode_response_bytes(&gb_bytes, Some("gb2312".into())).unwrap(),
            "中"
        );
        // 无声明的非法 UTF-8：不猜测，报错并带 hex 诊断
        let error = decode_response_bytes(&gb_bytes, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("D6"), "unexpected error: {error}");
        assert!(error.contains("未声明"));
        // 未知 charset 同样不兜底
        assert!(decode_response_bytes(&gb_bytes, Some("big5".into())).is_err());
    }

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
    fn collector_queries_are_bounded_to_the_reminder_scope() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        assert_eq!(
            ipo_window(today),
            (
                NaiveDate::from_ymd_opt(2026, 6, 27).unwrap(),
                NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
            )
        );

        let eastmoney = url::Url::parse(&eastmoney_url(today, 2).unwrap()).unwrap();
        let eastmoney: std::collections::HashMap<_, _> =
            eastmoney.query_pairs().into_owned().collect();
        assert_eq!(
            eastmoney.get("filter").map(String::as_str),
            Some("(APPLY_DATE>='2026-06-27')(APPLY_DATE<='2026-10-25')")
        );
        assert_eq!(
            eastmoney.get("columns").map(String::as_str),
            Some(EASTMONEY_COLUMNS)
        );
        assert_eq!(eastmoney.get("pageNumber").map(String::as_str), Some("2"));
        assert_eq!(eastmoney.get("pageSize").map(String::as_str), Some("100"));

        let sse = url::Url::parse(&sse_url(3).unwrap()).unwrap();
        let sse: std::collections::HashMap<_, _> = sse.query_pairs().into_owned().collect();
        assert_eq!(sse.get("isIssue").map(String::as_str), Some("1"));
        assert_eq!(sse.get("pageHelp.cacheSize").map(String::as_str), Some("1"));
        assert_eq!(sse.get("pageHelp.pageNo").map(String::as_str), Some("3"));
        assert_eq!(
            sse.get("pageHelp.pageSize").map(String::as_str),
            Some("100")
        );
    }

    #[test]
    fn eastmoney_empty_window_is_a_healthy_empty_result() {
        let raw = r#"{"version":null,"result":null,"success":false,"message":"返回数据为空","code":9201}"#;
        let fetched = crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
            crate::model::time(12, 0),
        );
        assert!(parse_eastmoney(raw, fetched).unwrap().is_empty());
        assert_eq!(response_counts("eastmoney", raw).unwrap(), (Some(0), 0));
        assert_eq!(collector_audit(Some(0), 0, 0).state(), HealthState::Healthy);
    }

    #[test]
    fn combined_pages_keep_a_parseable_schema_snapshot() {
        let raw = combine_raw_pages(vec![
            r#"{"page":{"first":1}}"#.to_owned(),
            r#"callback([{"second":2}])"#.to_owned(),
        ]);
        assert!(serde_json::from_str::<Value>(&raw).is_ok());
        assert_ne!(schema_fingerprint(&raw), sha256(b""));
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
        assert_eq!(
            parse_retry_after(Some(&HeaderValue::from_static("86401")), now),
            None
        );
        assert_eq!(
            parse_retry_after(
                Some(&HeaderValue::from_static("Wed, 02 Sep 2026 04:05:00 GMT")),
                now,
            ),
            None
        );
    }

    #[test]
    fn malformed_jsonp_and_oversized_streams_return_errors_without_panicking() {
        assert!(unwrap_jsonp(")(").is_err());
        assert!(unwrap_jsonp("foo)bar(baz").is_err());

        let not_cancelled = || false;
        let mut exact = std::io::Cursor::new(b"1234".to_vec());
        assert_eq!(
            read_limited(&mut exact, 4, Some(&not_cancelled)).unwrap(),
            b"1234"
        );
        let mut oversized = std::io::Cursor::new(b"12345".to_vec());
        assert!(read_limited(&mut oversized, 4, Some(&not_cancelled)).is_err());
    }

    #[test]
    fn read_limited_stops_promptly_when_cancelled() {
        struct InfiniteOnes;
        impl Read for InfiniteOnes {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                for byte in buf.iter_mut() {
                    *byte = 1;
                }
                Ok(buf.len())
            }
        }

        // 前 3 次 read 检查放行，第 4 次取消：读取循环必须立即停止并返回
        // 可识别的取消错误，而不是一直读到体积上限。
        let cancel_after = std::cell::Cell::new(3usize);
        let cancelled = || {
            if cancel_after.get() > 0 {
                cancel_after.set(cancel_after.get() - 1);
                false
            } else {
                true
            }
        };
        let mut reader = InfiniteOnes;
        let error = read_limited(&mut reader, u64::MAX, Some(&cancelled)).unwrap_err();
        assert!(error.to_string().contains("同步已取消"));
        assert_eq!(cancel_after.get(), 0);
    }

    #[test]
    fn time_probe_hosts_are_not_business_data_hosts() {
        assert!(ensure_allowed("https://www.microsoft.com/", false).is_err());
        assert!(ensure_allowed("https://www.cloudflare.com/", true).is_err());
        assert!(!business_redirect_allowed(
            &url::Url::parse("https://www.microsoft.com/").unwrap()
        ));
        assert!(time_redirect_allowed(
            &url::Url::parse("https://www.microsoft.com/").unwrap()
        ));
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

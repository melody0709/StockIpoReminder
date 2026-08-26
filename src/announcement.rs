use std::{
    collections::{HashMap, HashSet},
    path::Path,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, TimeZone, Utc};
use reqwest::blocking::Client;
use serde_json::Value;

use crate::{
    core::{at, china_offset, now_china, parse_date, sha256},
    model::*,
    network::{checked_response, encode_query, ensure_allowed, text},
};

const METADATA_VERSION: &str = "announcement-metadata-v1";

#[derive(Debug)]
pub struct SearchOutput {
    pub references: Vec<AnnouncementRef>,
    pub warning: Option<String>,
    pub used_mirror: bool,
}

impl SearchOutput {
    fn direct(references: Vec<AnnouncementRef>) -> Self {
        Self {
            references,
            warning: None,
            used_mirror: false,
        }
    }
}

pub fn search(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<SearchOutput> {
    match event.exchange {
        Exchange::Shanghai => search_sse(client, event, from, to),
        Exchange::Shenzhen => {
            search_cninfo_market(client, event, from, to, "szse", "cninfo-announcement")
                .map(SearchOutput::direct)
        }
        Exchange::Beijing => search_bse(client, event, from, to).map(SearchOutput::direct),
        _ => Ok(SearchOutput::direct(Vec::new())),
    }
}

fn search_sse(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<SearchOutput> {
    let official = search_sse_official(client, event, from, to);
    let mirror = search_cninfo_market(client, event, from, to, "sse", "sse-announcement");
    match (official, mirror) {
        (Ok(_official), Ok(mirror)) if !mirror.is_empty() => Ok(SearchOutput {
            references: mirror,
            warning: None,
            used_mirror: true,
        }),
        (Ok(official), Ok(_)) => Ok(SearchOutput {
            warning: (!official.is_empty())
                .then(|| "巨潮沪市公告镜像未命中，已回退上交所公告直链".to_owned()),
            references: official,
            used_mirror: false,
        }),
        (Ok(official), Err(error)) => Ok(SearchOutput {
            references: official,
            warning: Some(format!("巨潮沪市公告镜像不可用：{error:#}")),
            used_mirror: false,
        }),
        (Err(error), Ok(mirror)) => Ok(SearchOutput {
            references: mirror,
            warning: Some(format!(
                "上交所公告检索失败，已由巨潮沪市镜像接管：{error:#}"
            )),
            used_mirror: true,
        }),
        (Err(official_error), Err(mirror_error)) => bail!(
            "上交所公告检索与巨潮沪市镜像均失败：上交所={official_error:#}；巨潮={mirror_error:#}"
        ),
    }
}

fn search_sse_official(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AnnouncementRef>> {
    let values = HashMap::from([
        ("isPagination", "true".to_owned()),
        ("productId", event.security_code.clone()),
        ("keyWord", String::new()),
        (
            "securityType",
            "0101,120100,020100,020200,120200".to_owned(),
        ),
        ("reportType2", "DQGG".to_owned()),
        ("reportType", "ALL".to_owned()),
        ("beginDate", from.format("%Y-%m-%d").to_string()),
        ("endDate", to.format("%Y-%m-%d").to_string()),
        ("pageHelp.pageSize", "100".to_owned()),
        ("pageHelp.pageNo", "1".to_owned()),
        ("pageHelp.beginPage", "1".to_owned()),
        ("pageHelp.cacheSize", "1".to_owned()),
        ("pageHelp.endPage", "5".to_owned()),
    ]);
    let url = format!(
        "https://query.sse.com.cn/security/stock/queryCompanyBulletin.do?{}",
        encode_query(&values)
    );
    ensure_allowed(&url, true)?;
    let response = checked_response(
        client
            .get(url)
            .header("Referer", "https://www.sse.com.cn/")
            .send()?,
        true,
    )?;
    let raw = response.text()?;
    parse_sse_references(&raw)
}

pub fn parse_sse_references(raw: &str) -> Result<Vec<AnnouncementRef>> {
    let root: Value = serde_json::from_str(raw)?;
    let rows = root
        .pointer("/pageHelp/data")
        .and_then(Value::as_array)
        .context("上交所公告响应缺少 pageHelp.data")?;
    let mut result = Vec::new();
    for item in rows {
        let Some(title) = text(item, "TITLE") else {
            continue;
        };
        let Some(path) = text(item, "URL") else {
            continue;
        };
        if !is_relevant(&title) {
            continue;
        }
        let url = if path.starts_with("https://") {
            path.clone()
        } else {
            format!(
                "https://www.sse.com.cn{}",
                if path.starts_with('/') {
                    path
                } else {
                    format!("/{path}")
                }
            )
        };
        ensure_allowed(&url, true)?;
        let id = Path::new(url::Url::parse(&url)?.path())
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("sse")
            .to_owned();
        result.push(AnnouncementRef {
            provider: "sse-announcement".into(),
            announcement_id: id,
            title: title.clone(),
            url,
            published_at: text(item, "SSEDATE")
                .and_then(|v| parse_date(&v))
                .map(|date| at(date, time(0, 0))),
            announcement_type: Some(announcement_type(&title)),
        });
    }
    Ok(result)
}

fn search_cninfo_market(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    column: &str,
    provider: &str,
) -> Result<Vec<AnnouncementRef>> {
    let landing = "https://www.cninfo.com.cn/new/index";
    ensure_allowed(landing, true)?;
    let _landing_response = checked_response(client.get(landing).send()?, true)?;
    thread::sleep(Duration::from_millis(350));
    let url = "https://www.cninfo.com.cn/new/hisAnnouncement/query";
    ensure_allowed(url, true)?;
    let response = checked_response(
        client
            .post(url)
            .header("Referer", landing)
            .form(&[
                ("pageNum", "1"),
                ("pageSize", "100"),
                ("column", column),
                ("tabName", "fulltext"),
                ("searchkey", event.security_code.as_str()),
                (
                    "seDate",
                    &format!("{}~{}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d")),
                ),
                ("plate", ""),
                ("stock", ""),
                ("category", ""),
                ("trade", ""),
                ("sortName", ""),
                ("sortType", ""),
            ])
            .send()?,
        true,
    )?;
    let raw = response.text()?;
    parse_cninfo_references_for_event(&raw, Some(&event.security_code), provider)
}

#[cfg(test)]
fn parse_cninfo_references(raw: &str) -> Result<Vec<AnnouncementRef>> {
    parse_cninfo_references_for_event(raw, None, "cninfo-announcement")
}

fn parse_cninfo_references_for_event(
    raw: &str,
    expected_code: Option<&str>,
    provider: &str,
) -> Result<Vec<AnnouncementRef>> {
    let root: Value = serde_json::from_str(raw).context("巨潮公告响应不是有效 JSON")?;
    let Some(value) = root.get("announcements") else {
        bail!("巨潮公告响应缺少 announcements")
    };
    if value.is_null() {
        let total_announcements = cninfo_count(&root, "totalAnnouncement")?;
        let total_records = cninfo_count(&root, "totalRecordNum")?;
        if total_announcements == 0 && total_records == 0 {
            return Ok(Vec::new());
        }
        bail!(
            "巨潮公告响应 announcements=null，但计数非零：totalAnnouncement={total_announcements}, totalRecordNum={total_records}"
        );
    }
    let rows = value
        .as_array()
        .context("巨潮公告 announcements 不是数组")?;
    let mut result = Vec::new();
    for item in rows {
        if expected_code.is_some_and(|expected| text(item, "secCode").as_deref() != Some(expected))
        {
            continue;
        }
        let (Some(title), Some(path), Some(id)) = (
            text(item, "announcementTitle"),
            text(item, "adjunctUrl"),
            text(item, "announcementId"),
        ) else {
            continue;
        };
        if !is_relevant(&title) {
            continue;
        }
        if !path.to_ascii_lowercase().ends_with(".pdf")
            || text(item, "adjunctType").is_some_and(|value| !value.eq_ignore_ascii_case("pdf"))
        {
            continue;
        }
        let url = if path.starts_with("https://") {
            path
        } else {
            format!("https://static.cninfo.com.cn/{path}")
        };
        ensure_allowed(&url, true)?;
        let published_at = item
            .get("announcementTime")
            .and_then(Value::as_i64)
            .and_then(|epoch| Utc.timestamp_millis_opt(epoch).single())
            .map(|value| value.with_timezone(&china_offset()));
        result.push(AnnouncementRef {
            provider: provider.into(),
            announcement_id: if provider == "sse-announcement" {
                format!("cninfo-{id}")
            } else {
                id
            },
            title: title.clone(),
            url,
            published_at,
            announcement_type: Some(announcement_type(&title)),
        });
    }
    deduplicate(result)
}

fn cninfo_count(root: &Value, key: &str) -> Result<u64> {
    let value = root
        .get(key)
        .with_context(|| format!("巨潮公告健康空结果缺少 {key}"))?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .with_context(|| format!("巨潮公告 {key} 不是非负整数"))
}

fn search_bse(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AnnouncementRef>> {
    let mut errors = Vec::new();
    if let Some(detail_id) = event.announcement_url.as_deref().and_then(detail_id) {
        match search_bse_pages(client, event, from, to, true, &detail_id) {
            Ok(rows) if !rows.is_empty() => return Ok(rows),
            Ok(_) => {}
            Err(error) => errors.push(error),
        }
    }
    let terms = [
        event.apply_code.as_deref(),
        Some(event.security_code.as_str()),
        event.legacy_code.as_deref(),
        Some(event.name.as_str()),
    ];
    let mut attempted = false;
    for term in terms.into_iter().flatten() {
        match search_bse_pages(client, event, from, to, false, term) {
            Ok(rows) => {
                attempted = true;
                if !rows.is_empty() {
                    return Ok(rows);
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if !attempted && !errors.is_empty() {
        bail!("北交所公告检索失败：{}", errors[0]);
    }
    Ok(Vec::new())
}

fn search_bse_pages(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    detail: bool,
    value: &str,
) -> Result<Vec<AnnouncementRef>> {
    let mut result = Vec::new();
    let mut page = 0usize;
    let mut total_pages = 1usize;
    while page < total_pages && page < 10 {
        let mut query: HashMap<&str, String> = if detail {
            HashMap::from([
                ("callback", "ipoDetailCb".into()),
                ("id", value.into()),
                ("page", page.to_string()),
                ("pageSize", "100".into()),
            ])
        } else {
            HashMap::from([
                ("callback", "ipoDisclosureCb".into()),
                ("disclosureTypes[]", "9533".into()),
                ("page", page.to_string()),
                ("companyCd", value.into()),
                ("startTime", from.format("%Y-%m-%d").to_string()),
                ("endTime", to.format("%Y-%m-%d").to_string()),
                ("keyword", String::new()),
                ("isLink", "1".into()),
            ])
        };
        let endpoint = if detail {
            "https://www.bseinfo.net/newShareController/infoDetailResult.do"
        } else {
            "https://www.bseinfo.net/disclosureInfoController/zoneInfoResult.do"
        };
        let url = format!("{endpoint}?{}", encode_query(&query));
        query.clear();
        ensure_allowed(&url, true)?;
        let response = checked_response(
            client
                .get(url)
                .header(
                    "Referer",
                    "https://www.bseinfo.net/newshare/listofissues.html",
                )
                .send()?,
            true,
        )?;
        let raw = response.text()?;
        let (rows, pages) = parse_bse_references(&raw, event, from, to)?;
        result.extend(rows);
        total_pages = pages;
        page += 1;
    }
    deduplicate(result)
}

pub fn parse_bse_references(
    raw: &str,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<(Vec<AnnouncementRef>, usize)> {
    let json = unwrap_jsonp(raw)?;
    let root: Value = serde_json::from_str(json)?;
    let payload = root
        .as_array()
        .and_then(|rows| rows.first())
        .context("北交所公告响应不是非空数组")?;
    if let Some(new_share) = payload.get("newShare") {
        if !matches_identity(
            text(new_share, "fxCode").as_deref(),
            text(new_share, "stockName").as_deref(),
            event.name.as_str(),
            event,
        ) {
            return Ok((Vec::new(), 1));
        }
    }
    let info = payload
        .get("listInfo")
        .context("北交所公告响应缺少 listInfo")?;
    let rows = info
        .get("content")
        .and_then(Value::as_array)
        .context("北交所公告响应缺少 listInfo.content")?;
    let mut result = Vec::new();
    for item in rows {
        let title = [
            text(item, "disclosureTitle"),
            text(item, "disclosurePostTitle"),
        ]
        .into_iter()
        .flatten()
        .collect::<String>();
        let Some(path) = text(item, "destFilePath") else {
            continue;
        };
        let date = text(item, "publishDate")
            .and_then(|value| parse_date(&value))
            .or_else(|| epoch_object_date(item, "pubDate"));
        if title.is_empty()
            || !is_relevant(&title)
            || date.is_none_or(|date| date < from || date > to)
        {
            continue;
        }
        if !matches_identity(
            text(item, "companyCd").as_deref(),
            text(item, "companyName").as_deref(),
            &title,
            event,
        ) {
            continue;
        }
        if !path.to_ascii_lowercase().ends_with(".pdf")
            && text(item, "fileExt").is_none_or(|v| !v.eq_ignore_ascii_case("pdf"))
        {
            continue;
        }
        let url = if path.starts_with("https://") {
            path
        } else {
            format!(
                "https://www.bseinfo.net{}",
                if path.starts_with('/') {
                    path
                } else {
                    format!("/{path}")
                }
            )
        };
        ensure_allowed(&url, true)?;
        let id = Path::new(url::Url::parse(&url)?.path())
            .file_stem()
            .and_then(|v| v.to_str())
            .map(str::to_owned)
            .or_else(|| text(item, "disclosureCode"))
            .unwrap_or_else(|| sha256(&url)[..16].to_owned());
        result.push(AnnouncementRef {
            provider: "bse-announcement".into(),
            announcement_id: id,
            title: title.clone(),
            url,
            published_at: date.map(|date| at(date, time(0, 0))),
            announcement_type: Some(announcement_type(&title)),
        });
    }
    let total_pages = info
        .get("totalPages")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    Ok((deduplicate(result)?, total_pages))
}

fn detail_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some("www.bseinfo.net" | "bseinfo.net" | "www.bse.cn" | "bse.cn")
        )
    {
        return None;
    }
    parsed
        .query_pairs()
        .find(|(key, value)| key.eq_ignore_ascii_case("id") && value.parse::<u64>().is_ok())
        .map(|(_, value)| value.into_owned())
}

fn matches_identity(
    code: Option<&str>,
    company_name: Option<&str>,
    title: &str,
    event: &IpoEvent,
) -> bool {
    let known: HashSet<&str> = [
        Some(event.security_code.as_str()),
        event.apply_code.as_deref(),
        event.legacy_code.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if code.is_none_or(|code| !known.contains(code.trim())) {
        return false;
    }
    let expected = normalize_identity(&event.name);
    normalize_identity(company_name.unwrap_or_default()) == expected
        || normalize_identity(title).starts_with(&expected)
}

fn normalize_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn epoch_object_date(item: &Value, key: &str) -> Option<NaiveDate> {
    let epoch = item.get(key)?.get("time")?.as_i64()?;
    Some(
        Utc.timestamp_millis_opt(epoch)
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

fn deduplicate(rows: Vec<AnnouncementRef>) -> Result<Vec<AnnouncementRef>> {
    let mut urls = HashSet::new();
    let mut result: Vec<_> = rows
        .into_iter()
        .filter(|row| urls.insert(row.url.to_ascii_lowercase()))
        .collect();
    result.sort_by_key(|row| {
        std::cmp::Reverse((announcement_priority(&row.title), row.published_at))
    });
    Ok(result)
}

fn announcement_priority(title: &str) -> u8 {
    if ["终止发行", "中止发行", "暂缓发行"]
        .iter()
        .any(|keyword| title.contains(keyword))
    {
        120
    } else if title.contains("发行公告") && !title.contains("发行结果") {
        110
    } else if title.contains("发行安排及初步询价公告") {
        105
    } else if title.contains("网上发行申购情况") || title.contains("中签率公告") {
        90
    } else if title.contains("中签结果") || title.contains("发行结果") {
        85
    } else if title.contains("招股意向书提示性公告") {
        80
    } else if title.contains("招股说明书") || title.contains("招股意向书") {
        70
    } else {
        60
    }
}

fn is_relevant(title: &str) -> bool {
    let required = [
        "发行公告",
        "网上发行公告",
        "招股说明书",
        "发行结果公告",
        "申购",
        "首次公开发行",
    ];
    let excluded = [
        "风险特别公告",
        "路演公告",
        "财务报告",
        "审计报告",
        "投资者关系",
        "公司章程",
        "法律意见",
        "核查报告",
        "保荐书",
        "批复",
        "招股说明书附录",
        "招股意向书附录",
    ];
    required.iter().any(|keyword| title.contains(keyword))
        && !excluded.iter().any(|keyword| title.contains(keyword))
}

fn announcement_type(title: &str) -> String {
    if title.contains("发行结果") {
        "发行结果公告"
    } else if title.contains("招股") {
        "招股说明书"
    } else {
        "发行公告"
    }
    .to_owned()
}

pub fn metadata_document(event: &IpoEvent, reference: AnnouncementRef) -> AnnouncementDocument {
    let identity_hash = sha256(
        format!(
            "{}|{}|{}",
            reference.provider, reference.announcement_id, reference.url
        )
        .as_bytes(),
    );
    AnnouncementDocument {
        id: format!("metadata-{}", &identity_hash[..32]),
        event_id: event.id.clone(),
        reference,
        local_path: String::new(),
        file_hash: identity_hash,
        text_hash: None,
        status: ExtractionStatus::Unsupported,
        parser_version: METADATA_VERSION.into(),
        fields: Vec::new(),
        downloaded_at: now_china(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(path: &str) -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/announcements")
                .join(path),
        )
        .unwrap()
    }

    #[test]
    fn parses_provider_fixtures() {
        assert_eq!(
            parse_sse_references(&fixture("sse-601123-20260824.json"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parse_cninfo_references(&fixture("cninfo-301688-20260824.json"))
                .unwrap()
                .len(),
            1
        );
        let mirror = parse_cninfo_references_for_event(
            &fixture("cninfo-sse-603448-20260826.json"),
            Some("603448"),
            "sse-announcement",
        )
        .unwrap();
        assert_eq!(mirror.len(), 1);
        assert_eq!(mirror[0].provider, "sse-announcement");
        assert_eq!(mirror[0].announcement_id, "cninfo-1225499048");
        assert_eq!(
            mirror[0].url,
            "https://static.cninfo.com.cn/finalpage/2026-08-25/1225499048.PDF"
        );
    }

    #[test]
    fn prioritizes_primary_announcements_and_excludes_adviser_documents() {
        assert!(is_relevant("测试股份首次公开发行股票并上市发行公告"));
        assert!(!is_relevant(
            "律师事务所关于测试股份首次公开发行股票并上市的法律意见书"
        ));
        let published = at(NaiveDate::from_ymd_opt(2026, 8, 25).unwrap(), time(0, 0));
        let rows = deduplicate(vec![
            AnnouncementRef {
                provider: "sse-announcement".into(),
                announcement_id: "prospectus".into(),
                title: "测试股份首次公开发行股票并上市招股说明书".into(),
                url: "https://static.cninfo.com.cn/prospectus.pdf".into(),
                published_at: Some(published),
                announcement_type: Some("招股说明书".into()),
            },
            AnnouncementRef {
                provider: "sse-announcement".into(),
                announcement_id: "issue".into(),
                title: "测试股份首次公开发行股票并上市发行公告".into(),
                url: "https://static.cninfo.com.cn/issue.pdf".into(),
                published_at: Some(published - chrono::Duration::days(1)),
                announcement_type: Some("发行公告".into()),
            },
        ])
        .unwrap();
        assert_eq!(rows[0].announcement_id, "issue");
    }

    #[test]
    fn cninfo_null_announcements_require_zero_counts() {
        assert!(
            parse_cninfo_references(&fixture("cninfo-301689-empty-20260824.json"))
                .unwrap()
                .is_empty()
        );
        let inconsistent = r#"{
            "announcements": null,
            "totalAnnouncement": 1,
            "totalRecordNum": 1
        }"#;
        assert!(
            parse_cninfo_references(inconsistent)
                .unwrap_err()
                .to_string()
                .contains("计数非零")
        );
    }

    #[test]
    fn metadata_documents_never_claim_a_local_pdf_or_parsed_fields() {
        let now = now_china();
        let event = IpoEvent {
            id: "shanghai:601001".into(),
            exchange: Exchange::Shanghai,
            board: Board::Main,
            security_code: "601001".into(),
            apply_code: Some("780001".into()),
            legacy_code: None,
            name: "测试股份".into(),
            apply_date: Some(now.date_naive()),
            issue_price: Some(10.0),
            lot_size: Some(500),
            max_apply_quantity: Some(10_000),
            required_market_value: None,
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: IssueStatus::Active,
            lifecycle_status: LifecycleStatus::ActiveUnconfirmed,
            event_version: 1,
            announcement_url: None,
            data_quality_status: DataQualityStatus::MultiSourceVerified,
            data_conflict: false,
            manual_override_fields: Vec::new(),
            sessions: Vec::new(),
            first_seen_at: now,
            updated_at: now,
        };
        let reference = AnnouncementRef {
            provider: "sse-announcement".into(),
            announcement_id: "announcement-1".into(),
            title: "首次公开发行公告".into(),
            url: "https://www.sse.com.cn/test.pdf".into(),
            published_at: Some(now),
            announcement_type: Some("发行公告".into()),
        };
        let first = metadata_document(&event, reference.clone());
        let second = metadata_document(&event, reference);
        assert_eq!(first.id, second.id);
        assert!(first.local_path.is_empty());
        assert!(first.text_hash.is_none());
        assert!(first.fields.is_empty());
        assert_eq!(first.status, ExtractionStatus::Unsupported);
        assert_eq!(first.parser_version, METADATA_VERSION);
    }
}

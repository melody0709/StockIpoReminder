use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, TimeZone, Utc};
use regex::Regex;
use reqwest::{blocking::Client, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    core::{at, china_offset, now_china, parse_date, sha256},
    model::*,
    network::{encode_query, ensure_allowed, text},
};

pub const MAX_DOWNLOAD_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_HTML_BYTES: u64 = 4 * 1024 * 1024;
pub const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;
pub const MAX_PDF_PAGES: usize = 20;
pub const MAX_EXTRACTED_CHARACTERS: usize = 256_000;
pub const PDF_WORKER_TIMEOUT: Duration = Duration::from_secs(60);
const PARSER_VERSION: &str = "rust-announcement-v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerRequest {
    input_path: PathBuf,
    max_pages: usize,
    max_characters: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PdfWorkerResponse {
    success: bool,
    text: String,
    text_hash: Option<String>,
    page_count: usize,
    truncated: bool,
    error: Option<String>,
}

pub fn try_run_pdf_worker(arguments: &[String]) -> Result<Option<i32>> {
    let Some(request_path) = argument_value(arguments, "--pdf-worker-request") else {
        return Ok(None);
    };
    let response_path = argument_value(arguments, "--pdf-worker-response")
        .context("PDF Worker 缺少 --pdf-worker-response")?;
    let response = match (|| -> Result<PdfWorkerResponse> {
        let request: PdfWorkerRequest = serde_json::from_slice(&fs::read(&request_path)?)?;
        if request.max_pages == 0 || request.max_pages > 100 || request.max_characters == 0 {
            bail!("PDF Worker 参数超出允许范围");
        }
        extract_pdf_text(&request)
    })() {
        Ok(response) => response,
        Err(error) => PdfWorkerResponse {
            success: false,
            text: String::new(),
            text_hash: None,
            page_count: 0,
            truncated: false,
            error: Some(format!("{error:#}")),
        },
    };
    let temporary = response_path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary, serde_json::to_vec(&response)?)?;
    fs::rename(&temporary, &response_path)?;
    Ok(Some(if response.success { 0 } else { 2 }))
}

fn argument_value(arguments: &[String], name: &str) -> Option<PathBuf> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| PathBuf::from(&pair[1]))
}

fn extract_pdf_text(request: &PdfWorkerRequest) -> Result<PdfWorkerResponse> {
    let document = lopdf::Document::load(&request.input_path)
        .with_context(|| format!("无法打开 PDF：{}", request.input_path.display()))?;
    let pages: Vec<u32> = document
        .get_pages()
        .keys()
        .copied()
        .take(request.max_pages)
        .collect();
    let page_count = document.get_pages().len();
    let mut combined = String::new();
    for page in &pages {
        let page_text = document.extract_text(&[*page]).unwrap_or_default();
        if combined.len() < request.max_characters {
            combined.push_str(&page_text);
            combined.push('\n');
        }
    }
    let (text, character_truncated) = take_characters(&combined, request.max_characters);
    let truncated = character_truncated || page_count > request.max_pages;
    let text_hash = (!text.trim().is_empty()).then(|| sha256(text.as_bytes()));
    Ok(PdfWorkerResponse {
        success: true,
        text,
        text_hash,
        page_count,
        truncated,
        error: None,
    })
}

fn take_characters(value: &str, maximum: usize) -> (String, bool) {
    let mut iterator = value.chars();
    let text: String = iterator.by_ref().take(maximum).collect();
    (text, iterator.next().is_some())
}

pub fn search(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AnnouncementRef>> {
    match event.exchange {
        Exchange::Shanghai => search_sse(client, event, from, to),
        Exchange::Shenzhen => search_cninfo(client, event, from, to),
        Exchange::Beijing => search_bse(client, event, from, to),
        _ => Ok(Vec::new()),
    }
}

fn search_sse(
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
    let raw = client
        .get(url)
        .header("Referer", "https://www.sse.com.cn/")
        .send()?
        .error_for_status()?
        .text()?;
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

fn search_cninfo(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AnnouncementRef>> {
    let landing = "https://www.cninfo.com.cn/new/index";
    ensure_allowed(landing, true)?;
    let _ = client.get(landing).send()?.error_for_status()?;
    thread::sleep(Duration::from_millis(350));
    let url = "https://www.cninfo.com.cn/new/hisAnnouncement/query";
    ensure_allowed(url, true)?;
    let raw = client
        .post(url)
        .header("Referer", landing)
        .form(&[
            ("pageNum", "1"),
            ("pageSize", "100"),
            ("column", "szse"),
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
        .send()?
        .error_for_status()?
        .text()?;
    parse_cninfo_references(&raw)
}

pub fn parse_cninfo_references(raw: &str) -> Result<Vec<AnnouncementRef>> {
    let root: Value = serde_json::from_str(raw).context("巨潮公告响应不是有效 JSON")?;
    let Some(value) = root.get("announcements") else {
        bail!("巨潮公告响应缺少 announcements")
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let rows = value
        .as_array()
        .context("巨潮公告 announcements 不是数组")?;
    let mut result = Vec::new();
    for item in rows {
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
            provider: "cninfo-announcement".into(),
            announcement_id: id,
            title: title.clone(),
            url,
            published_at,
            announcement_type: Some(announcement_type(&title)),
        });
    }
    Ok(result)
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
        let raw = client
            .get(url)
            .header(
                "Referer",
                "https://www.bseinfo.net/newshare/listofissues.html",
            )
            .send()?
            .error_for_status()?
            .text()?;
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
    result.sort_by_key(|row| std::cmp::Reverse(row.published_at));
    Ok(result)
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

pub fn download_and_parse(
    client: &Client,
    data_root: &Path,
    event: &IpoEvent,
    reference: AnnouncementRef,
) -> Result<(AnnouncementDocument, Option<Candidate>)> {
    ensure_allowed(&reference.url, true)?;
    let temporary_directory = data_root.join("temp");
    fs::create_dir_all(&temporary_directory)?;
    let operation_id = Uuid::new_v4().simple().to_string();
    let temporary_path = temporary_directory.join(format!("{operation_id}.download"));
    let mut response = client.get(&reference.url).send()?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        bail!("公告文件超过 32MiB 上限");
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut target = File::create(&temporary_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; DOWNLOAD_BUFFER_BYTES];
    let mut prefix = Vec::with_capacity(512);
    let mut length = 0u64;
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length += count as u64;
        if length > MAX_DOWNLOAD_BYTES {
            let _ = fs::remove_file(&temporary_path);
            bail!("公告下载超过 32MiB 上限");
        }
        if prefix.len() < 512 {
            prefix.extend_from_slice(&buffer[..count.min(512 - prefix.len())]);
        }
        hasher.update(&buffer[..count]);
        target.write_all(&buffer[..count])?;
    }
    target.flush()?;
    drop(target);
    let file_hash = hex::encode(hasher.finalize());
    let pdf = prefix.starts_with(b"%PDF-");
    let looks_html = content_type.contains("html")
        || String::from_utf8_lossy(&prefix)
            .to_ascii_lowercase()
            .contains("<html");
    if content_type.contains("pdf") && !pdf {
        let _ = fs::remove_file(&temporary_path);
        bail!("公告响应声明为 PDF，但缺少 PDF 签名");
    }
    if !pdf && !looks_html {
        let _ = fs::remove_file(&temporary_path);
        bail!("公告文件既不是 PDF，也不是 HTML");
    }
    if looks_html && length > MAX_HTML_BYTES {
        let _ = fs::remove_file(&temporary_path);
        bail!("公告 HTML 超过 4MiB 上限");
    }
    let directory = data_root
        .join("announcements")
        .join(sanitize(&reference.provider))
        .join(sanitize(&event.id));
    fs::create_dir_all(&directory)?;
    let extension = if pdf { "pdf" } else { "html" };
    let local_path = directory.join(format!(
        "{}-{}.{}",
        sanitize(&reference.announcement_id),
        &file_hash[..16],
        extension
    ));
    if local_path.exists() {
        fs::remove_file(&temporary_path)?;
    } else {
        fs::rename(&temporary_path, &local_path)?;
    }

    let (extracted_text, text_hash, truncated) = if pdf {
        let response = extract_pdf_in_worker(data_root, &local_path)?;
        if !response.success {
            bail!("PDF Worker 失败：{}", response.error.unwrap_or_default());
        }
        (response.text, response.text_hash, response.truncated)
    } else {
        let html = fs::read_to_string(&local_path).or_else(|_| {
            fs::read(&local_path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        })?;
        let plain = html_to_text(&html);
        let (plain, truncated) = take_characters(&plain, MAX_EXTRACTED_CHARACTERS);
        let hash = (!plain.trim().is_empty()).then(|| sha256(plain.as_bytes()));
        (plain, hash, truncated)
    };
    let fields = parse_fields(&extracted_text, &reference.title)?;
    let status = if extracted_text.trim().is_empty() {
        ExtractionStatus::Failed
    } else if fields.iter().any(|field| field.confidence >= 0.90) {
        ExtractionStatus::Extracted
    } else {
        ExtractionStatus::LowConfidence
    };
    let document = AnnouncementDocument {
        id: Uuid::new_v4().simple().to_string(),
        event_id: event.id.clone(),
        reference: reference.clone(),
        local_path: local_path.to_string_lossy().into_owned(),
        file_hash,
        text_hash,
        status,
        parser_version: format!(
            "{PARSER_VERSION}{}",
            if truncated { "+truncated" } else { "" }
        ),
        fields,
        downloaded_at: now_china(),
    };
    let candidate = candidate_from_document(event, &document);
    Ok((document, candidate))
}

fn extract_pdf_in_worker(data_root: &Path, input_path: &Path) -> Result<PdfWorkerResponse> {
    let directory = data_root.join("temp");
    fs::create_dir_all(&directory)?;
    let operation = Uuid::new_v4().simple().to_string();
    let request_path = directory.join(format!("{operation}.worker-request.json"));
    let response_path = directory.join(format!("{operation}.worker-response.json"));
    fs::write(
        &request_path,
        serde_json::to_vec(&PdfWorkerRequest {
            input_path: input_path.to_owned(),
            max_pages: MAX_PDF_PAGES,
            max_characters: MAX_EXTRACTED_CHARACTERS,
        })?,
    )?;
    let executable = std::env::current_exe()?;
    let mut child = Command::new(executable)
        .args([
            "--pdf-worker-request",
            request_path.to_string_lossy().as_ref(),
            "--pdf-worker-response",
            response_path.to_string_lossy().as_ref(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= PDF_WORKER_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            cleanup_worker_files(&request_path, &response_path);
            bail!("PDF Worker 超过 60 秒超时");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let response = fs::read(&response_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PdfWorkerResponse>(&bytes).ok());
    cleanup_worker_files(&request_path, &response_path);
    let response = response.context("PDF Worker 未生成有效响应")?;
    if !status.success() || !response.success {
        bail!(
            "PDF Worker 失败：{}",
            response.error.clone().unwrap_or_default()
        );
    }
    Ok(response)
}

fn cleanup_worker_files(request: &Path, response: &Path) {
    let _ = fs::remove_file(request);
    let _ = fs::remove_file(response);
}

fn html_to_text(html: &str) -> String {
    let scripts = Regex::new("(?is)<(script|style)[^>]*>.*?</(script|style)>")
        .unwrap()
        .replace_all(html, " ");
    let tags = Regex::new("(?is)<[^>]+>")
        .unwrap()
        .replace_all(&scripts, " ");
    let decoded = tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&quot;", "\"");
    Regex::new(r"\s+")
        .unwrap()
        .replace_all(&decoded, " ")
        .trim()
        .to_owned()
}

pub fn parse_fields(text_value: &str, title: &str) -> Result<Vec<ParsedField>> {
    let compact = Regex::new(r"\s+")?.replace_all(text_value, " ");
    let mut fields = Vec::new();
    capture(
        &mut fields,
        &compact,
        "SecurityCode",
        r"证券代码\s*[:：]?\s*(\d{6})",
        0.99,
        |v| Some(v.to_owned()),
    )?;
    capture(
        &mut fields,
        &compact,
        "ApplyCode",
        r"申购代码\s*[:：]?\s*(\d{6})",
        0.99,
        |v| Some(v.to_owned()),
    )?;
    capture(
        &mut fields,
        &compact,
        "IssuePrice",
        r"(?:发行价格|发行价)(?:为)?\s*[:：]?\s*([0-9]+(?:\.[0-9]+)?)\s*元",
        0.98,
        |v| v.parse::<f64>().ok().map(|n| n.to_string()),
    )?;
    capture(
        &mut fields,
        &compact,
        "LotSize",
        r"每(?:一|个)申购单位(?:为|是)?\s*(\d+)\s*股",
        0.95,
        |v| v.parse::<i64>().ok().map(|n| n.to_string()),
    )?;
    capture(
        &mut fields,
        &compact,
        "MaxApplyQuantity",
        r"(?:申购上限|最多可申购|不超过)[^。；]{0,40}?([0-9]+(?:\.[0-9]+)?)\s*(万)?股",
        0.92,
        |v| {
            let mut parts = v.split('|');
            let number = parts.next()?.parse::<f64>().ok()?;
            Some(
                (number
                    * if parts.next() == Some("万") {
                        10_000.0
                    } else {
                        1.0
                    })
                .round()
                .to_string(),
            )
        },
    )?;
    capture_date(
        &mut fields,
        &compact,
        "ApplyDate",
        r"(?:网上申购日|申购日期|申购日)(?:为)?\s*[:：]?\s*(\d{4})年(\d{1,2})月(\d{1,2})日",
    )?;
    capture_date(
        &mut fields,
        &compact,
        "BallotDate",
        r"(?:中签率公告日|摇号抽签日)(?:为)?\s*[:：]?\s*(\d{4})年(\d{1,2})月(\d{1,2})日",
    )?;
    let session_regex = Regex::new(r"(\d{1,2}:\d{2})\s*[-—至]\s*(\d{1,2}:\d{2})")?;
    for (index, capture) in session_regex.captures_iter(&compact).take(4).enumerate() {
        fields.push(ParsedField {
            name: format!("Session{}", index + 1),
            value: format!("{}-{}", &capture[1], &capture[2]),
            confidence: 0.95,
            evidence: Some(capture[0].to_owned()),
            character_offset: capture.get(0).map(|m| m.start()),
        });
    }
    if compact.contains("全额缴付申购资金") || compact.contains("全额缴付申购款") {
        fields.push(ParsedField {
            name: "FundingMode".into(),
            value: "FullCash".into(),
            confidence: 0.98,
            evidence: Some("全额缴付申购资金".into()),
            character_offset: compact.find("全额缴付"),
        });
    }
    let definite_termination = ["决定终止发行", "本次发行终止", "终止本次发行"]
        .iter()
        .any(|value| compact.contains(value) || title.contains(value));
    if definite_termination {
        fields.push(ParsedField {
            name: "IssueStatus".into(),
            value: (IssueStatus::Terminated as i32).to_string(),
            confidence: 0.99,
            evidence: Some("终止发行决定".into()),
            character_offset: None,
        });
    }
    Ok(fields)
}

fn capture<F>(
    fields: &mut Vec<ParsedField>,
    text_value: &str,
    name: &str,
    pattern: &str,
    confidence: f64,
    transform: F,
) -> Result<()>
where
    F: Fn(&str) -> Option<String>,
{
    let regex = Regex::new(pattern)?;
    if let Some(captures) = regex.captures(text_value) {
        let Some(value) = captures.get(1) else {
            return Ok(());
        };
        let transformed_input = if name == "MaxApplyQuantity" {
            format!(
                "{}|{}",
                value.as_str(),
                captures.get(2).map(|m| m.as_str()).unwrap_or_default()
            )
        } else {
            value.as_str().to_owned()
        };
        if let Some(value) = transform(&transformed_input) {
            fields.push(ParsedField {
                name: name.into(),
                value,
                confidence,
                evidence: captures.get(0).map(|m| m.as_str().to_owned()),
                character_offset: captures.get(0).map(|m| m.start()),
            });
        }
    }
    Ok(())
}

fn capture_date(
    fields: &mut Vec<ParsedField>,
    text_value: &str,
    name: &str,
    pattern: &str,
) -> Result<()> {
    let regex = Regex::new(pattern)?;
    if let Some(captures) = regex.captures(text_value) {
        let date = format!(
            "{:04}-{:02}-{:02}",
            captures[1].parse::<u32>()?,
            captures[2].parse::<u32>()?,
            captures[3].parse::<u32>()?
        );
        fields.push(ParsedField {
            name: name.into(),
            value: date,
            confidence: 0.98,
            evidence: captures.get(0).map(|m| m.as_str().to_owned()),
            character_offset: captures.get(0).map(|m| m.start()),
        });
    }
    Ok(())
}

pub fn candidate_from_document(
    event: &IpoEvent,
    document: &AnnouncementDocument,
) -> Option<Candidate> {
    let values: HashMap<&str, &str> = document
        .fields
        .iter()
        .filter(|field| field.confidence >= 0.90)
        .map(|field| (field.name.as_str(), field.value.as_str()))
        .collect();
    if values.is_empty() {
        return None;
    }
    let sessions: Vec<SubscriptionSession> = (1..=4)
        .filter_map(|number| {
            let value = values.get(format!("Session{number}").as_str())?;
            let (start, end) = value.split_once('-')?;
            Some(SubscriptionSession {
                session_number: number,
                official_start: chrono::NaiveTime::parse_from_str(start, "%H:%M").ok()?,
                official_end: chrono::NaiveTime::parse_from_str(end, "%H:%M").ok()?,
                broker_accept_start: None,
                safety_cutoff: None,
                funding_mode: if values.get("FundingMode") == Some(&"FullCash") {
                    FundingMode::FullCash
                } else {
                    FundingMode::MarketValue
                },
                allocation_time_sensitive: event.exchange == Exchange::Beijing,
                source: document.reference.provider.clone(),
                source_published_at: document.reference.published_at,
            })
        })
        .collect();
    Some(Candidate {
        source: document.reference.provider.clone(),
        priority: 300,
        fetched_at: document.downloaded_at,
        published_at: document.reference.published_at,
        exchange: event.exchange,
        board: event.board,
        security_code: values
            .get("SecurityCode")
            .map(|v| (*v).to_owned())
            .or_else(|| Some(event.security_code.clone())),
        apply_code: values.get("ApplyCode").map(|v| (*v).to_owned()),
        legacy_code: None,
        name: Some(event.name.clone()),
        apply_date: values.get("ApplyDate").and_then(|v| parse_date(v)),
        issue_price: values.get("IssuePrice").and_then(|v| v.parse().ok()),
        lot_size: values.get("LotSize").and_then(|v| v.parse().ok()),
        max_apply_quantity: values.get("MaxApplyQuantity").and_then(|v| v.parse().ok()),
        required_market_value: None,
        required_cash: None,
        ballot_date: values.get("BallotDate").and_then(|v| parse_date(v)),
        payment_date: None,
        listing_date: None,
        status: values
            .get("IssueStatus")
            .and_then(|v| v.parse().ok())
            .map(IssueStatus::from_i32)
            .unwrap_or(IssueStatus::Unknown),
        announcement_url: Some(document.reference.url.clone()),
        sessions,
        announcement_derived: true,
    })
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(100)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn parses_announcement_fields_without_false_termination() {
        let fields = parse_fields(&fixture("bse-920289-excerpt-20260820.txt"), "发行公告").unwrap();
        assert!(fields.iter().any(|field| field.name == "ApplyCode"));
        assert!(fields.iter().any(|field| field.name == "IssuePrice"));
        assert!(!fields.iter().any(|field| field.name == "IssueStatus"));
    }
}

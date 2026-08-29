use super::*;

pub(crate) fn ensure_not_cancelled(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        bail!("同步已取消")
    }
    Ok(())
}

pub(crate) fn detail_id(url: &str) -> Option<String> {
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

pub(crate) fn matches_identity(
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

pub(crate) fn normalize_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect()
}

pub(crate) fn epoch_object_date(item: &Value, key: &str) -> Option<NaiveDate> {
    let epoch = item.get(key)?.get("time")?.as_i64()?;
    Some(
        Utc.timestamp_millis_opt(epoch)
            .single()?
            .with_timezone(&china_offset())
            .date_naive(),
    )
}

pub(crate) fn unwrap_jsonp(raw: &str) -> Result<&str> {
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

pub(crate) fn deduplicate(rows: Vec<AnnouncementRef>) -> Result<Vec<AnnouncementRef>> {
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

pub(crate) fn announcement_priority(title: &str) -> u8 {
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

pub(crate) fn is_relevant(title: &str) -> bool {
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

pub(crate) fn announcement_type(title: &str) -> String {
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

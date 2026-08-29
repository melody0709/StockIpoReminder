use super::*;

pub(crate) fn search_cninfo_market(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    column: &str,
    provider: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<ReferenceSearch> {
    let landing = "https://www.cninfo.com.cn/new/index";
    ensure_allowed(landing, true)?;
    ensure_not_cancelled(cancelled)?;
    let _landing_response = checked_response(client.get(landing).send()?, true)?;
    ensure_not_cancelled(cancelled)?;
    thread::sleep(Duration::from_millis(350));
    ensure_not_cancelled(cancelled)?;
    let url = "https://www.cninfo.com.cn/new/hisAnnouncement/query";
    ensure_allowed(url, true)?;
    let date_range = format!("{}~{}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d"));
    let mut references = Vec::new();
    let mut page = 1usize;
    let truncated = loop {
        ensure_not_cancelled(cancelled)?;
        let form = HashMap::from([
            ("pageNum", page.to_string()),
            ("pageSize", ANNOUNCEMENT_PAGE_SIZE.to_string()),
            ("column", column.to_owned()),
            ("tabName", "fulltext".to_owned()),
            ("searchkey", event.security_code.clone()),
            ("seDate", date_range.clone()),
            ("plate", String::new()),
            ("stock", String::new()),
            ("category", String::new()),
            ("trade", String::new()),
            ("sortName", String::new()),
            ("sortType", String::new()),
        ]);
        let response = checked_response(
            client
                .post(url)
                .header("Referer", landing)
                .form(&form)
                .send()?,
            true,
        )?;
        ensure_not_cancelled(cancelled)?;
        let raw = response_text(response, true, cancelled)?;
        let page_result =
            parse_cninfo_reference_page_for_event(&raw, Some(&event.security_code), provider)?;
        references.extend(page_result.references);
        let has_next = pagination_has_next(page, page_result.raw_count, page_result.total)?;
        if !has_next || page >= MAX_ANNOUNCEMENT_PAGES {
            break has_next;
        }
        page += 1;
    };
    Ok(ReferenceSearch {
        references: deduplicate(references)?,
        truncated,
    })
}

#[cfg(test)]
pub(crate) fn parse_cninfo_references(raw: &str) -> Result<Vec<AnnouncementRef>> {
    parse_cninfo_references_for_event(raw, None, "cninfo-announcement")
}

#[cfg(test)]
pub(crate) fn parse_cninfo_references_for_event(
    raw: &str,
    expected_code: Option<&str>,
    provider: &str,
) -> Result<Vec<AnnouncementRef>> {
    parse_cninfo_reference_page_for_event(raw, expected_code, provider)
        .map(|value| value.references)
        .and_then(deduplicate)
}

pub(crate) fn parse_cninfo_reference_page_for_event(
    raw: &str,
    expected_code: Option<&str>,
    provider: &str,
) -> Result<ReferencePage> {
    let root: Value = serde_json::from_str(raw).context("巨潮公告响应不是有效 JSON")?;
    let total_announcements = cninfo_count(&root, "totalAnnouncement")?;
    let total_records = cninfo_count(&root, "totalRecordNum")?;
    let Some(value) = root.get("announcements") else {
        bail!("巨潮公告响应缺少 announcements")
    };
    if value.is_null() {
        if total_announcements == Some(0) && total_records == Some(0) {
            return Ok(ReferencePage {
                references: Vec::new(),
                raw_count: 0,
                total: Some(0),
            });
        }
        bail!(
            "巨潮公告响应 announcements=null，但健康空结果计数缺失或非零：totalAnnouncement={total_announcements:?}, totalRecordNum={total_records:?}"
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
    let total = total_announcements
        .into_iter()
        .chain(total_records)
        .max()
        .map(|value| usize::try_from(value).context("巨潮公告 total 超出范围"))
        .transpose()?;
    // 去重统一在外层合并时执行（L12），页解析只解析和校验。
    Ok(ReferencePage {
        references: result,
        raw_count: rows.len(),
        total,
    })
}

pub(crate) fn cninfo_count(root: &Value, key: &str) -> Result<Option<u64>> {
    let Some(value) = root.get(key) else {
        return Ok(None);
    };
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .map(Some)
        .with_context(|| format!("巨潮公告 {key} 不是非负整数"))
}

use super::*;

pub(crate) fn search_bse(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    cancelled: &dyn Fn() -> bool,
) -> Result<ReferenceSearch> {
    let mut errors = Vec::new();
    if let Some(detail_id) = event.announcement_url.as_deref().and_then(detail_id) {
        match search_bse_pages(client, event, from, to, true, &detail_id, cancelled) {
            Ok(rows) if !rows.references.is_empty() => return Ok(rows),
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
        ensure_not_cancelled(cancelled)?;
        match search_bse_pages(client, event, from, to, false, term, cancelled) {
            Ok(rows) => {
                attempted = true;
                if !rows.references.is_empty() {
                    return Ok(rows);
                }
            }
            Err(error) => errors.push(error),
        }
    }
    if !attempted && !errors.is_empty() {
        bail!("北交所公告检索失败：{}", errors[0]);
    }
    Ok(ReferenceSearch {
        references: Vec::new(),
        truncated: false,
    })
}

pub(crate) fn search_bse_pages(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    detail: bool,
    value: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<ReferenceSearch> {
    let mut result = Vec::new();
    let mut page = 0usize;
    let mut total_pages = 1usize;
    while page < total_pages && page < MAX_ANNOUNCEMENT_PAGES {
        ensure_not_cancelled(cancelled)?;
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
        ensure_not_cancelled(cancelled)?;
        let raw = response_text(response, true, cancelled)?;
        let (rows, pages) = parse_bse_references(&raw, event, from, to)?;
        result.extend(rows);
        total_pages = pages;
        page += 1;
    }
    Ok(ReferenceSearch {
        references: deduplicate(result)?,
        truncated: total_pages > MAX_ANNOUNCEMENT_PAGES,
    })
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

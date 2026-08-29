use super::*;

pub(crate) fn search_sse(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    cancelled: &dyn Fn() -> bool,
) -> Result<SearchOutput> {
    let official = search_sse_official(client, event, from, to, cancelled);
    ensure_not_cancelled(cancelled)?;
    let mirror = search_cninfo_market(
        client,
        event,
        from,
        to,
        "sse",
        "sse-announcement",
        cancelled,
    );
    ensure_not_cancelled(cancelled)?;
    match (official, mirror) {
        (Ok(official), Ok(mirror)) => {
            let used_mirror = !mirror.references.is_empty();
            let truncated = official.truncated || mirror.truncated;
            let mut references = official.references;
            references.extend(mirror.references);
            Ok(SearchOutput {
                references: deduplicate(references)?,
                warning: truncated.then(|| {
                    format!(
                        "上交所或巨潮公告结果超过 {} 页安全上限，本轮结果已明确标记为不完整",
                        MAX_ANNOUNCEMENT_PAGES
                    )
                }),
                used_mirror,
            })
        }
        (Ok(official), Err(error)) => Ok(SearchOutput {
            references: official.references,
            warning: Some(if official.truncated {
                format!(
                    "上交所公告结果超过 {} 页安全上限，本轮结果已明确标记为不完整；巨潮沪市公告镜像不可用：{error:#}",
                    MAX_ANNOUNCEMENT_PAGES
                )
            } else {
                format!("巨潮沪市公告镜像不可用：{error:#}")
            }),
            used_mirror: false,
        }),
        (Err(error), Ok(mirror)) => Ok(SearchOutput {
            references: mirror.references,
            warning: Some(if mirror.truncated {
                format!(
                    "上交所公告检索失败，巨潮镜像超过 {} 页安全上限：{error:#}",
                    MAX_ANNOUNCEMENT_PAGES
                )
            } else {
                format!("上交所公告检索失败，已由巨潮沪市镜像接管：{error:#}")
            }),
            used_mirror: true,
        }),
        (Err(official_error), Err(mirror_error)) => bail!(
            "上交所公告检索与巨潮沪市镜像均失败：上交所={official_error:#}；巨潮={mirror_error:#}"
        ),
    }
}

pub(crate) fn search_sse_official(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    cancelled: &dyn Fn() -> bool,
) -> Result<ReferenceSearch> {
    let mut references = Vec::new();
    let mut page = 1usize;
    let truncated = loop {
        ensure_not_cancelled(cancelled)?;
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
            ("pageHelp.pageSize", ANNOUNCEMENT_PAGE_SIZE.to_string()),
            ("pageHelp.pageNo", page.to_string()),
            ("pageHelp.beginPage", page.to_string()),
            ("pageHelp.cacheSize", "1".to_owned()),
            ("pageHelp.endPage", page.to_string()),
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
        ensure_not_cancelled(cancelled)?;
        let raw = response_text(response, true, cancelled)?;
        let page_result = parse_sse_reference_page(&raw)?;
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
pub fn parse_sse_references(raw: &str) -> Result<Vec<AnnouncementRef>> {
    parse_sse_reference_page(raw)
        .map(|value| value.references)
        .and_then(deduplicate)
}

pub(crate) fn parse_sse_reference_page(raw: &str) -> Result<ReferencePage> {
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
    let total = match root.pointer("/pageHelp/total") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let parsed = value
                .as_u64()
                .or_else(|| {
                    value
                        .as_str()
                        .and_then(|raw| raw.trim().parse::<u64>().ok())
                })
                .with_context(|| format!("上交所公告响应 pageHelp.total 格式非法：{value}"))?;
            Some(usize::try_from(parsed).context("上交所公告响应 pageHelp.total 超出范围")?)
        }
    };
    // 去重统一在外层合并时执行（L12），页解析只解析和校验。
    Ok(ReferencePage {
        references: result,
        raw_count: rows.len(),
        total,
    })
}

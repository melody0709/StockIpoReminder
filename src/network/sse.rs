use super::*;

pub(crate) fn sse_url(page: usize) -> Result<String> {
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

pub(crate) fn sse_page_counts(raw: &str) -> Result<(Option<usize>, usize, usize)> {
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

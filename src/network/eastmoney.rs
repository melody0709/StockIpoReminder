use super::*;

pub(crate) fn eastmoney_url(today: NaiveDate, page: usize) -> Result<String> {
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

pub(crate) fn eastmoney_page_counts(raw: &str) -> Result<(Option<usize>, usize, usize)> {
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

pub(crate) fn eastmoney_empty(root: &Value) -> bool {
    root.get("success").and_then(Value::as_bool) == Some(false)
        && root.get("code").and_then(Value::as_i64) == Some(9201)
        && root.get("result").is_none_or(Value::is_null)
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

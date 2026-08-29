use super::*;

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

pub(crate) struct BsePage {
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

pub(crate) fn parse_bse_page_with_meta(raw: &str, fetched: ChinaDateTime) -> Result<BsePage> {
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

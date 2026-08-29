use super::*;

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

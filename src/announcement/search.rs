use super::*;

pub fn search(
    client: &Client,
    event: &IpoEvent,
    from: NaiveDate,
    to: NaiveDate,
    cancelled: &dyn Fn() -> bool,
) -> Result<SearchOutput> {
    ensure_not_cancelled(cancelled)?;
    match event.exchange {
        Exchange::Shanghai => search_sse(client, event, from, to, cancelled),
        Exchange::Shenzhen => search_cninfo_market(
            client,
            event,
            from,
            to,
            "szse",
            "cninfo-announcement",
            cancelled,
        )
        .map(|result| result.output("巨潮")),
        Exchange::Beijing => {
            search_bse(client, event, from, to, cancelled).map(|result| result.output("北交所"))
        }
        _ => Ok(SearchOutput::direct(Vec::new())),
    }
}

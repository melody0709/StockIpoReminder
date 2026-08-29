use super::*;

pub(crate) fn ipo_window(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    (
        today - chrono::Duration::days(IPO_WINDOW_PAST_DAYS),
        today + chrono::Duration::days(IPO_WINDOW_FUTURE_DAYS),
    )
}

pub(crate) fn ensure_not_cancelled(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        bail!("同步已取消")
    }
    Ok(())
}

pub(crate) fn output(
    source: &'static str,
    started: ChinaDateTime,
    raw: String,
    candidates: Vec<Candidate>,
) -> CollectorOutput {
    let (declared_count, detail_count) =
        response_counts(source, &raw).unwrap_or((None, candidates.len()));
    output_with_counts(
        source,
        started,
        raw,
        candidates,
        declared_count,
        detail_count,
    )
}

pub(crate) fn output_with_counts(
    source: &'static str,
    started: ChinaDateTime,
    raw: String,
    candidates: Vec<Candidate>,
    declared_count: Option<usize>,
    detail_count: usize,
) -> CollectorOutput {
    let schema = schema_fingerprint(&raw);
    let hash = sha256(raw.as_bytes());
    let audit = collector_audit(declared_count, detail_count, candidates.len());
    CollectorOutput {
        source,
        started,
        raw,
        hash,
        schema,
        candidates,
        audit,
    }
}

pub(crate) fn combine_raw_pages(raws: Vec<String>) -> String {
    let pages = raws
        .iter()
        .map(|raw| parse_payload(raw))
        .collect::<Result<Vec<_>>>();
    match pages {
        Ok(pages) => Value::Array(pages).to_string(),
        Err(_) => raws.join("\n"),
    }
}

pub(crate) fn response_counts(source: &str, raw: &str) -> Result<(Option<usize>, usize)> {
    if source == "eastmoney" {
        let (declared_count, detail_count, _) = eastmoney_page_counts(raw)?;
        return Ok((declared_count, detail_count));
    }
    if source == "sse" {
        let (declared_count, detail_count, _) = sse_page_counts(raw)?;
        return Ok((declared_count, detail_count));
    }
    let root: Value = serde_json::from_str(raw)?;
    let (declared_count, details) = match source {
        "cninfo" => (
            root.get("count").and_then(Value::as_u64),
            root.get("data").and_then(Value::as_array),
        ),
        _ => (None, None),
    };
    let details = details.context("采集响应缺少可计数的明细数组")?;
    Ok((declared_count.map(|value| value as usize), details.len()))
}

pub(crate) fn collector_audit(
    declared_count: Option<usize>,
    detail_count: usize,
    accepted_count: usize,
) -> CollectorAudit {
    let mut issues = Vec::new();
    if let Some(declared_count) = declared_count
        && declared_count != detail_count
    {
        issues.push(format!(
            "上游声明 {declared_count} 条，但本轮取得 {detail_count} 条明细"
        ));
    }
    if accepted_count != detail_count {
        issues.push(format!(
            "{detail_count} 条明细中仅 {accepted_count} 条通过身份和必填字段校验"
        ));
    }
    CollectorAudit {
        declared_count,
        detail_count,
        accepted_count,
        issues,
    }
}

pub fn text(item: &Value, key: &str) -> Option<String> {
    let value = item.get(key)?;
    let text = match value {
        Value::String(v) => v.clone(),
        Value::Null => return None,
        _ => value.to_string().trim_matches('"').to_owned(),
    };
    let text = text.trim();
    if text.is_empty() || matches!(text, "-" | "--" | "N/A" | "NULL" | "无") {
        None
    } else {
        Some(text.to_owned())
    }
}
pub(crate) fn number(item: &Value, key: &str, zero_missing: bool) -> Option<f64> {
    let value = text(item, key)?.replace(',', "").parse().ok()?;
    if zero_missing && value == 0.0 {
        None
    } else {
        Some(value)
    }
}
pub(crate) fn integer(item: &Value, key: &str, multiplier: f64, zero_missing: bool) -> Option<i64> {
    let value = number(item, key, zero_missing)? * multiplier;
    Some(value.round() as i64)
}
pub(crate) fn date(item: &Value, key: &str) -> Option<NaiveDate> {
    parse_date(&text(item, key)?)
}
pub(crate) fn epoch_date(item: &Value, key: &str) -> Option<NaiveDate> {
    let millis = item.get(key)?.get("time")?.as_i64()?;
    Some(
        Utc.timestamp_millis_opt(millis)
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

pub(crate) fn parse_payload(raw: &str) -> Result<Value> {
    serde_json::from_str(raw)
        .or_else(|_| unwrap_jsonp(raw).and_then(|value| Ok(serde_json::from_str(value)?)))
}

pub(crate) fn schema_fingerprint(raw: &str) -> String {
    let mut keys = BTreeSet::new();
    if let Ok(value) = parse_payload(raw) {
        collect_keys(&value, &mut keys);
    }
    sha256(keys.into_iter().collect::<Vec<_>>().join("\n"))
}
pub(crate) fn collect_keys(value: &Value, keys: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                keys.insert(key.clone());
                collect_keys(value, keys)
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_keys(item, keys)
            }
        }
        _ => {}
    }
}

pub fn encode_query(values: &HashMap<&str, String>) -> String {
    values
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                url::form_urlencoded::byte_serialize(k.as_bytes()).collect::<String>(),
                url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

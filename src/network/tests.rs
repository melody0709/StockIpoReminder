use super::*;
use std::{fs, path::Path};

#[test]
fn declared_charset_parses_content_type_parameters() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json; Charset=\"GBK\""),
    );
    assert_eq!(declared_charset(&headers).as_deref(), Some("gbk"));
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    assert_eq!(declared_charset(&headers), None);
}

#[test]
fn decode_response_bytes_only_honors_declared_gb_charsets() {
    assert_eq!(decode_response_bytes(b"plain", None).unwrap(), "plain");
    let gb_bytes = vec![0xD6, 0xD0]; // “中”的 GBK 编码
    assert_eq!(
        decode_response_bytes(&gb_bytes, Some("gb2312".into())).unwrap(),
        "中"
    );
    // 无声明的非法 UTF-8：不猜测，报错并带 hex 诊断
    let error = decode_response_bytes(&gb_bytes, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("D6"), "unexpected error: {error}");
    assert!(error.contains("未声明"));
    // 未知 charset 同样不兜底
    assert!(decode_response_bytes(&gb_bytes, Some("big5".into())).is_err());
}

fn fixture(path: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/collectors")
            .join(path),
    )
    .unwrap()
}

#[test]
fn parses_all_collector_fixtures() {
    let fetched = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
        crate::model::time(12, 0),
    );
    assert!(
        !parse_eastmoney(&fixture("eastmoney-20260824.json"), fetched)
            .unwrap()
            .is_empty()
    );
    assert!(
        !parse_sse(&fixture("sse-20260824.json"), fetched)
            .unwrap()
            .is_empty()
    );
    assert!(
        !parse_cninfo(&fixture("cninfo-20260824.json"), fetched)
            .unwrap()
            .is_empty()
    );
    let (page0, _) = parse_bse_page(&fixture("bse-page0-20260824.jsonp"), fetched).unwrap();
    let (page1, _) = parse_bse_page(&fixture("bse-page1-20260824.jsonp"), fetched).unwrap();
    assert!(!page0.is_empty() || !page1.is_empty());
}

#[test]
fn collector_audit_detects_declared_detail_and_parser_count_mismatches() {
    let raw = fixture("sse-20260824.json");
    let fetched = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
        crate::model::time(12, 0),
    );
    let accepted = parse_sse(&raw, fetched).unwrap().len();
    let (declared, details) = response_counts("sse", &raw).unwrap();
    let healthy = collector_audit(declared, details, accepted);
    assert_eq!(healthy.state(), HealthState::Healthy);
    assert!(healthy.summary().is_none());

    let truncated = collector_audit(Some(2), 1, 1);
    assert_eq!(truncated.state(), HealthState::Warning);
    assert!(truncated.summary().unwrap().contains("声明 2 条"));

    let rejected = collector_audit(None, 2, 1);
    assert_eq!(rejected.state(), HealthState::Warning);
    assert!(rejected.summary().unwrap().contains("仅 1 条通过"));
}

#[test]
fn collector_queries_are_bounded_to_the_reminder_scope() {
    let today = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
    assert_eq!(
        ipo_window(today),
        (
            NaiveDate::from_ymd_opt(2026, 6, 27).unwrap(),
            NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
        )
    );

    let eastmoney = url::Url::parse(&eastmoney_url(today, 2).unwrap()).unwrap();
    let eastmoney: std::collections::HashMap<_, _> = eastmoney.query_pairs().into_owned().collect();
    assert_eq!(
        eastmoney.get("filter").map(String::as_str),
        Some("(APPLY_DATE>='2026-06-27')(APPLY_DATE<='2026-10-25')")
    );
    assert_eq!(
        eastmoney.get("columns").map(String::as_str),
        Some(EASTMONEY_COLUMNS)
    );
    assert_eq!(eastmoney.get("pageNumber").map(String::as_str), Some("2"));
    assert_eq!(eastmoney.get("pageSize").map(String::as_str), Some("100"));

    let sse = url::Url::parse(&sse_url(3).unwrap()).unwrap();
    let sse: std::collections::HashMap<_, _> = sse.query_pairs().into_owned().collect();
    assert_eq!(sse.get("isIssue").map(String::as_str), Some("1"));
    assert_eq!(sse.get("pageHelp.cacheSize").map(String::as_str), Some("1"));
    assert_eq!(sse.get("pageHelp.pageNo").map(String::as_str), Some("3"));
    assert_eq!(
        sse.get("pageHelp.pageSize").map(String::as_str),
        Some("100")
    );
}

#[test]
fn eastmoney_empty_window_is_a_healthy_empty_result() {
    let raw =
        r#"{"version":null,"result":null,"success":false,"message":"返回数据为空","code":9201}"#;
    let fetched = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
        crate::model::time(12, 0),
    );
    assert!(parse_eastmoney(raw, fetched).unwrap().is_empty());
    assert_eq!(response_counts("eastmoney", raw).unwrap(), (Some(0), 0));
    assert_eq!(collector_audit(Some(0), 0, 0).state(), HealthState::Healthy);
}

#[test]
fn combined_pages_keep_a_parseable_schema_snapshot() {
    let raw = combine_raw_pages(vec![
        r#"{"page":{"first":1}}"#.to_owned(),
        r#"callback([{"second":2}])"#.to_owned(),
    ]);
    assert!(serde_json::from_str::<Value>(&raw).is_ok());
    assert_ne!(schema_fingerprint(&raw), sha256(b""));
}

#[test]
fn rejects_html_and_non_allowlisted_hosts() {
    assert!(parse_sse("<html>WAF</html>", crate::core::now_china()).is_err());
    assert!(ensure_allowed("https://example.com/file.pdf", true).is_err());
    assert!(ensure_allowed("http://www.sse.com.cn/file.pdf", true).is_err());
    assert!(ensure_allowed("https://query.sse.com.cn/query", true).is_ok());
    assert!(ensure_allowed("https://static.sse.com.cn/file.pdf", true).is_ok());
    assert!(ensure_allowed("https://www.cninfo.com.cn/query", true).is_ok());
}

#[test]
fn parses_retry_after_delta_seconds_and_http_date() {
    use reqwest::header::HeaderValue;

    let now = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
        crate::model::time(12, 0),
    );
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_static("120")), now),
        Some(now + chrono::Duration::minutes(2))
    );
    assert_eq!(
        parse_retry_after(
            Some(&HeaderValue::from_static("Wed, 26 Aug 2026 04:05:00 GMT")),
            now,
        ),
        Some(now + chrono::Duration::minutes(5))
    );
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_static("-1")), now),
        None
    );
    assert_eq!(
        parse_retry_after(Some(&HeaderValue::from_static("86401")), now),
        None
    );
    assert_eq!(
        parse_retry_after(
            Some(&HeaderValue::from_static("Wed, 02 Sep 2026 04:05:00 GMT")),
            now,
        ),
        None
    );
}

#[test]
fn malformed_jsonp_and_oversized_streams_return_errors_without_panicking() {
    assert!(unwrap_jsonp(")(").is_err());
    assert!(unwrap_jsonp("foo)bar(baz").is_err());

    let not_cancelled = || false;
    let mut exact = std::io::Cursor::new(b"1234".to_vec());
    assert_eq!(
        read_limited(&mut exact, 4, Some(&not_cancelled)).unwrap(),
        b"1234"
    );
    let mut oversized = std::io::Cursor::new(b"12345".to_vec());
    assert!(read_limited(&mut oversized, 4, Some(&not_cancelled)).is_err());
}

#[test]
fn read_limited_stops_promptly_when_cancelled() {
    struct InfiniteOnes;
    impl Read for InfiniteOnes {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            for byte in buf.iter_mut() {
                *byte = 1;
            }
            Ok(buf.len())
        }
    }

    // 前 3 次 read 检查放行，第 4 次取消：读取循环必须立即停止并返回
    // 可识别的取消错误，而不是一直读到体积上限。
    let cancel_after = std::cell::Cell::new(3usize);
    let cancelled = || {
        if cancel_after.get() > 0 {
            cancel_after.set(cancel_after.get() - 1);
            false
        } else {
            true
        }
    };
    let mut reader = InfiniteOnes;
    let error = read_limited(&mut reader, u64::MAX, Some(&cancelled)).unwrap_err();
    assert!(error.to_string().contains("同步已取消"));
    assert_eq!(cancel_after.get(), 0);
}

#[test]
fn time_probe_hosts_are_not_business_data_hosts() {
    assert!(ensure_allowed("https://www.microsoft.com/", false).is_err());
    assert!(ensure_allowed("https://www.cloudflare.com/", true).is_err());
    assert!(!business_redirect_allowed(
        &url::Url::parse("https://www.microsoft.com/").unwrap()
    ));
    assert!(time_redirect_allowed(
        &url::Url::parse("https://www.microsoft.com/").unwrap()
    ));
}

#[test]
fn low_frequency_probe_targets_are_allowlisted_and_source_specific() {
    for source in [
        "eastmoney",
        "sse",
        "cninfo",
        "bse",
        "sse-announcement",
        "cninfo-announcement",
        "bse-announcement",
    ] {
        let url = source_probe_url(source).unwrap();
        assert!(ensure_allowed(url, false).is_ok(), "source={source}");
    }
    assert!(source_probe_url("unknown").is_none());
}

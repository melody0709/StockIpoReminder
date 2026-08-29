use super::*;
use std::fs;

fn fixture(path: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/announcements")
            .join(path),
    )
    .unwrap()
}

#[test]
fn parses_provider_fixtures() {
    assert_eq!(
        parse_sse_references(&fixture("sse-601123-20260824.json"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        parse_cninfo_references(&fixture("cninfo-301688-20260824.json"))
            .unwrap()
            .len(),
        1
    );
    let mirror = parse_cninfo_references_for_event(
        &fixture("cninfo-sse-603448-20260826.json"),
        Some("603448"),
        "sse-announcement",
    )
    .unwrap();
    assert_eq!(mirror.len(), 1);
    assert_eq!(mirror[0].provider, "sse-announcement");
    assert_eq!(mirror[0].announcement_id, "cninfo-1225499048");
    assert_eq!(
        mirror[0].url,
        "https://static.cninfo.com.cn/finalpage/2026-08-25/1225499048.PDF"
    );
}

#[test]
fn prioritizes_primary_announcements_and_excludes_adviser_documents() {
    assert!(is_relevant("测试股份首次公开发行股票并上市发行公告"));
    assert!(!is_relevant(
        "律师事务所关于测试股份首次公开发行股票并上市的法律意见书"
    ));
    let published = at(NaiveDate::from_ymd_opt(2026, 8, 25).unwrap(), time(0, 0));
    let rows = deduplicate(vec![
        AnnouncementRef {
            provider: "sse-announcement".into(),
            announcement_id: "prospectus".into(),
            title: "测试股份首次公开发行股票并上市招股说明书".into(),
            url: "https://static.cninfo.com.cn/prospectus.pdf".into(),
            published_at: Some(published),
            announcement_type: Some("招股说明书".into()),
        },
        AnnouncementRef {
            provider: "sse-announcement".into(),
            announcement_id: "issue".into(),
            title: "测试股份首次公开发行股票并上市发行公告".into(),
            url: "https://static.cninfo.com.cn/issue.pdf".into(),
            published_at: Some(published - chrono::Duration::days(1)),
            announcement_type: Some("发行公告".into()),
        },
    ])
    .unwrap();
    assert_eq!(rows[0].announcement_id, "issue");
}

#[test]
fn pagination_probe_decision_table() {
    // total 缺失：满页继续探测，短页/空页结束。
    assert!(pagination_has_next(1, 100, None).unwrap());
    assert!(!pagination_has_next(1, 99, None).unwrap());
    assert!(!pagination_has_next(1, 0, None).unwrap());
    // total 已知：按声明页数判定。
    assert!(pagination_has_next(1, 100, Some(250)).unwrap());
    assert!(!pagination_has_next(3, 50, Some(250)).unwrap());
    assert!(!pagination_has_next(1, 0, Some(0)).unwrap());
    assert!(pagination_has_next(1, 100, Some(1)).is_err());
    assert!(pagination_has_next(2, 100, Some(150)).is_err());
    assert!(pagination_has_next(1, 0, Some(250)).is_err());
}

#[test]
fn sse_page_preserves_missing_total_and_rejects_invalid_total() {
    let missing = r#"{"pageHelp":{"data":[{"TITLE":"首次公开发行股票网上发行公告","URL":"https://www.sse.com.cn/a/b/1234/","SSEDATE":"2026-08-24"}]}}"#;
    let page = parse_sse_reference_page(missing).unwrap();
    assert_eq!(page.total, None, "total 缺失不得回退为行数");
    assert_eq!(page.raw_count, 1);
    assert_eq!(page.references.len(), 1);

    let string_total = r#"{"pageHelp":{"total":"250","data":[]}}"#;
    assert_eq!(
        parse_sse_reference_page(string_total).unwrap().total,
        Some(250),
        "total 为数字字符串时应可解析"
    );

    let invalid = r#"{"pageHelp":{"total":"abc","data":[]}}"#;
    let error = parse_sse_reference_page(invalid).unwrap_err().to_string();
    assert!(error.contains("格式非法"), "unexpected error: {error}");
}

#[test]
fn cninfo_page_preserves_missing_total() {
    let raw = r#"{"announcements":[{"announcementTitle":"首次公开发行股票网上发行公告","adjunctUrl":"a/b/1234.pdf","announcementId":"1234","secCode":"301688"}]}"#;
    let page =
        parse_cninfo_reference_page_for_event(raw, Some("301688"), "cninfo-announcement").unwrap();
    assert_eq!(page.total, None, "total 缺失不得回退为行数");
    assert_eq!(page.raw_count, 1);
    assert_eq!(page.references.len(), 1);
}

#[test]
fn cninfo_null_announcements_require_zero_counts() {
    assert!(
        parse_cninfo_references(&fixture("cninfo-301689-empty-20260824.json"))
            .unwrap()
            .is_empty()
    );
    let inconsistent = r#"{
        "announcements": null,
        "totalAnnouncement": 1,
        "totalRecordNum": 1
    }"#;
    assert!(
        parse_cninfo_references(inconsistent)
            .unwrap_err()
            .to_string()
            .contains("计数缺失或非零")
    );
}

#[test]
fn malformed_jsonp_is_rejected_without_panicking() {
    assert!(unwrap_jsonp(")(").is_err());
    assert!(unwrap_jsonp("foo)bar(baz").is_err());
}

#[test]
fn metadata_documents_never_claim_a_local_pdf_or_parsed_fields() {
    let now = now_china();
    let event = IpoEvent {
        id: "shanghai:601001".into(),
        exchange: Exchange::Shanghai,
        board: Board::Main,
        security_code: "601001".into(),
        apply_code: Some("780001".into()),
        legacy_code: None,
        name: "测试股份".into(),
        apply_date: Some(now.date_naive()),
        issue_price: Some(10.0),
        lot_size: Some(500),
        max_apply_quantity: Some(10_000),
        required_market_value: None,
        required_cash: None,
        ballot_date: None,
        payment_date: None,
        listing_date: None,
        status: IssueStatus::Active,
        lifecycle_status: LifecycleStatus::ActiveUnconfirmed,
        event_version: 1,
        announcement_url: None,
        data_quality_status: DataQualityStatus::MultiSourceVerified,
        data_conflict: false,
        manual_override_fields: Vec::new(),
        sessions: Vec::new(),
        first_seen_at: now,
        updated_at: now,
    };
    let reference = AnnouncementRef {
        provider: "sse-announcement".into(),
        announcement_id: "announcement-1".into(),
        title: "首次公开发行公告".into(),
        url: "https://www.sse.com.cn/test.pdf".into(),
        published_at: Some(now),
        announcement_type: Some("发行公告".into()),
    };
    let first = metadata_document(&event, reference.clone());
    let second = metadata_document(&event, reference);
    assert_eq!(first.id, second.id);
    assert!(first.local_path.is_empty());
    assert!(first.text_hash.is_none());
    assert!(first.fields.is_empty());
    assert_eq!(first.status, ExtractionStatus::Unsupported);
    assert_eq!(first.parser_version, METADATA_VERSION);
}

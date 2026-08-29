use super::*;

#[test]
fn lifecycle_deadline_targets_the_next_real_boundary() {
    let test = TestDatabase::new();
    let now = now_china();
    let tomorrow = now.date_naive() + chrono::Duration::days(1);
    let mut event = test.event();
    event.apply_date = Some(tomorrow);
    event.lifecycle_status = LifecycleStatus::Scheduled;
    test.database.upsert_event(event).unwrap();

    assert_eq!(
        test.database.next_lifecycle_transition_at(now).unwrap(),
        Some(crate::core::at(tomorrow, crate::model::time(0, 0)))
    );
}

#[test]
fn critical_change_increments_version_and_outbox_is_claimable() {
    let test = TestDatabase::new();
    let first = test.database.upsert_event(test.event()).unwrap();
    assert_eq!(first.event_version, 1);
    let mut changed = first.clone();
    changed.issue_price = Some(11.0);
    changed.updated_at = now_china();
    let changed = test.database.upsert_event(changed).unwrap();
    assert_eq!(changed.event_version, 2);
    let due = test.database.claim_due(50).unwrap();
    assert!(!due.is_empty());
    test.database.complete_delivery(&due[0], "test").unwrap();
}

#[test]
fn unchanged_event_upsert_preserves_event_and_outbox_timestamps() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let before: (String, Option<String>) = test
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT e.updated_at,(SELECT MAX(updated_at) FROM reminder_outbox WHERE ipo_event_id=e.id) FROM ipo_events e WHERE e.id=?1",
                [&event.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

    let mut identical = event.clone();
    identical.updated_at += chrono::Duration::hours(1);
    let returned = test.database.upsert_event(identical).unwrap();
    let after: (String, Option<String>) = test
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT e.updated_at,(SELECT MAX(updated_at) FROM reminder_outbox WHERE ipo_event_id=e.id) FROM ipo_events e WHERE e.id=?1",
                [&event.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

    assert_eq!(before, after);
    assert_eq!(format_dt(returned.updated_at), format_dt(event.updated_at));
}

#[test]
fn unchanged_field_sources_preserve_the_original_fetch_timestamp() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    let candidate = Candidate {
        source: "fixture".into(),
        priority: 100,
        fetched_at: now,
        published_at: Some(now - chrono::Duration::minutes(1)),
        exchange: event.exchange,
        board: event.board,
        security_code: Some(event.security_code.clone()),
        apply_code: event.apply_code.clone(),
        legacy_code: event.legacy_code.clone(),
        name: Some(event.name.clone()),
        apply_date: event.apply_date,
        issue_price: event.issue_price,
        lot_size: event.lot_size,
        max_apply_quantity: event.max_apply_quantity,
        required_market_value: event.required_market_value,
        required_cash: event.required_cash,
        ballot_date: event.ballot_date,
        payment_date: event.payment_date,
        listing_date: event.listing_date,
        status: event.status,
        announcement_url: event.announcement_url.clone(),
        sessions: event.sessions.clone(),
    };
    test.database
        .replace_field_sources(&event.id, std::slice::from_ref(&candidate))
        .unwrap();
    let before: String = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT MIN(fetched_at) FROM ipo_field_sources WHERE ipo_event_id=?1",
            [&event.id],
            |row| row.get(0),
        )
        .unwrap();

    let mut repeated = candidate;
    repeated.fetched_at += chrono::Duration::hours(1);
    test.database
        .replace_field_sources(&event.id, &[repeated])
        .unwrap();
    let after: String = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT MIN(fetched_at) FROM ipo_field_sources WHERE ipo_event_id=?1",
            [&event.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn missing_optional_fields_retain_known_values_without_false_review() {
    let test = TestDatabase::new();
    let now = now_china();
    let mut input = test.event();
    input.apply_date = Some(now.date_naive());
    input.required_market_value = Some(100_000.0);
    input.required_cash = Some(50_000.0);
    let event = test.database.upsert_event(input).unwrap();
    test.database
        .acknowledge(&event.id, event.event_version)
        .unwrap();

    let mut partial = test.database.event(&event.id).unwrap().unwrap();
    partial.apply_code = None;
    partial.apply_date = None;
    partial.issue_price = None;
    partial.lot_size = None;
    partial.max_apply_quantity = None;
    partial.required_market_value = None;
    partial.required_cash = None;
    partial.updated_at = now_china();
    let saved = test.database.upsert_event(partial).unwrap();

    assert_eq!(saved.event_version, event.event_version);
    assert_eq!(saved.lifecycle_status, LifecycleStatus::Acknowledged);
    assert_eq!(saved.apply_code.as_deref(), Some("780001"));
    assert_eq!(saved.apply_date, Some(now.date_naive()));
    assert_eq!(saved.issue_price, Some(10.0));
    assert_eq!(saved.lot_size, Some(500));
    assert_eq!(saved.max_apply_quantity, Some(10_000));
    assert_eq!(saved.required_market_value, Some(100_000.0));
    assert_eq!(saved.required_cash, Some(50_000.0));
}

#[test]
fn noncritical_change_enqueues_exactly_one_message_without_version_bump() {
    let test = TestDatabase::new();
    let original = test.database.upsert_event(test.event()).unwrap();
    let mut changed = original.clone();
    changed.name = "测试股份新简称".into();
    changed.listing_date = Some(now_china().date_naive() + chrono::Duration::days(10));
    changed.updated_at += chrono::Duration::seconds(1);
    let saved = test.database.upsert_event(changed.clone()).unwrap();
    assert_eq!(saved.event_version, original.event_version);

    let connection = test.database.open().unwrap();
    let (count, message): (i64, String) = connection
            .query_row(
                "SELECT COUNT(*), MAX(message) FROM reminder_outbox WHERE ipo_event_id=?1 AND reminder_level=?2",
                params![saved.id, ReminderLevel::DataChanged as i32],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(count, 1);
    assert!(message.contains("证券简称"));
    assert!(message.contains("上市日期"));
    drop(connection);

    changed.updated_at += chrono::Duration::seconds(1);
    test.database.upsert_event(changed).unwrap();
    let count: i64 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND reminder_level=?2",
            params![saved.id, ReminderLevel::DataChanged as i32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    let delivery = test
        .database
        .claim_due_at(100, now_china() + chrono::Duration::minutes(1))
        .unwrap()
        .into_iter()
        .find(|delivery| delivery.level == ReminderLevel::DataChanged)
        .unwrap();
    assert!(delivery.message.as_deref().unwrap().contains("仅提醒一次"));
    test.database.complete_delivery(&delivery, "test").unwrap();
    assert!(test.database.complete_delivery(&delivery, "test").is_err());
    let shown_count: i64 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM reminder_log WHERE dedupe_key=?1",
            [delivery.dedupe_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shown_count, 1);
}

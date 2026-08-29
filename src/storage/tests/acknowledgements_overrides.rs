use super::*;

#[test]
fn issue_status_override_mapping_splits_postpone_and_suspend() {
    assert_eq!(
        parse_issue_status_override("延期发行"),
        Some(IssueStatus::Postponed)
    );
    assert_eq!(
        parse_issue_status_override("Postponed"),
        Some(IssueStatus::Postponed)
    );
    assert_eq!(
        parse_issue_status_override("暂缓发行"),
        Some(IssueStatus::Suspended)
    );
    assert_eq!(
        parse_issue_status_override("暂停发行"),
        Some(IssueStatus::Suspended)
    );
    assert_eq!(
        parse_issue_status_override("中止发行"),
        Some(IssueStatus::Suspended)
    );
}

#[test]
fn upsert_replan_respects_manual_status_override() {
    let test = TestDatabase::new();
    let database = &test.database;
    let event = database.upsert_event(test.event()).unwrap();
    assert!(
        database.reminder_state_summary().unwrap().pending > 0,
        "新事件应规划出未投递的申购提醒"
    );

    database
        .apply_manual_override(
            &event.id,
            event.event_version,
            "IssueStatus",
            "延期发行",
            "测试核验：延期发行",
            None,
        )
        .unwrap();
    assert_eq!(
        database.reminder_state_summary().unwrap().pending,
        0,
        "Postponed 覆盖应取消未投递的申购提醒"
    );

    // 非关键变更再次触发同步重规划；活跃覆盖必须仍然生效。
    let mut renamed = event.clone();
    renamed.name = "覆盖测试股份B".into();
    renamed.updated_at = now_china() + chrono::Duration::seconds(1);
    let renamed = database.upsert_event(renamed).unwrap();
    assert_eq!(
        pending_reminder_rows_excluding_data_changed(database),
        0,
        "同步重规划不得绕过人工 Postponed 覆盖重建申购提醒"
    );

    // 关键字段变化会创建新 event_version；IssueStatus 覆盖仍必须继承到
    // 新版本，网络的新日期/正常状态不能悄悄解除用户的 Postponed 决定。
    let mut rescheduled = renamed;
    rescheduled.apply_date = Some(now_china().date_naive() + chrono::Duration::days(1));
    rescheduled.status = IssueStatus::Upcoming;
    rescheduled.lifecycle_status = LifecycleStatus::Scheduled;
    rescheduled.updated_at = now_china() + chrono::Duration::seconds(2);
    let rescheduled = database.upsert_event(rescheduled).unwrap();
    assert_eq!(rescheduled.event_version, event.event_version + 1);
    let still_overridden = database.event(&event.id).unwrap().unwrap();
    assert_eq!(still_overridden.status, IssueStatus::Postponed);
    assert_eq!(still_overridden.event_version, rescheduled.event_version);
    assert!(
        still_overridden
            .manual_override_fields
            .contains(&"IssueStatus".to_owned())
    );
    assert_eq!(pending_reminder_rows_excluding_data_changed(database), 0);

    let override_id: i64 = database
            .open()
            .unwrap()
            .query_row(
                "SELECT id FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 AND field_name='IssueStatus' AND revoked_at IS NULL",
                params![event.id, rescheduled.event_version],
                |row| row.get(0),
            )
            .unwrap();
    database
        .revoke_manual_override(&event.id, rescheduled.event_version, override_id)
        .unwrap();
    assert_eq!(
        database.event(&event.id).unwrap().unwrap().status,
        IssueStatus::Upcoming
    );
    assert!(
        pending_reminder_rows_excluding_data_changed(database) > 0,
        "撤销覆盖后应按当前可信数据重新规划提醒"
    );
}

#[test]
fn acknowledgement_override_and_backoff_roundtrip() {
    let test = TestDatabase::new();
    let mut input = test.event();
    input.apply_date = Some(now_china().date_naive());
    let event = test.database.upsert_event(input).unwrap();
    test.database
        .apply_manual_override(
            &event.id,
            event.event_version,
            "IssuePrice",
            "12.50",
            "公告人工核验",
            None,
        )
        .unwrap();
    assert_eq!(
        test.database.event(&event.id).unwrap().unwrap().issue_price,
        Some(12.5)
    );
    test.database
        .acknowledge(&event.id, event.event_version)
        .unwrap();
    assert_eq!(
        test.database
            .event(&event.id)
            .unwrap()
            .unwrap()
            .lifecycle_status,
        LifecycleStatus::Acknowledged
    );
    let next = test
        .database
        .save_source_run(
            "fixture",
            now_china(),
            HealthState::Failed,
            0,
            None,
            None,
            None,
            Some("test"),
        )
        .unwrap()
        .unwrap();
    assert!(
        !test
            .database
            .source_can_attempt("fixture", now_china())
            .unwrap()
            .0
    );
    assert!(next > now_china());
    test.database
        .save_source_run(
            "fixture",
            now_china(),
            HealthState::Healthy,
            1,
            Some("healthy"),
            None,
            Some("fixture-v1"),
            None,
        )
        .unwrap();
    assert!(
        test.database
            .source_can_attempt("fixture", now_china())
            .unwrap()
            .0
    );
}

#[test]
fn acknowledged_event_requires_review_after_limits_or_sessions_change() {
    let test = TestDatabase::new();
    let now = now_china();
    let mut input = test.event();
    input.apply_date = Some(now.date_naive());
    input.sessions = vec![SubscriptionSession {
        session_number: 1,
        official_start: NaiveTime::from_hms_opt(9, 30, 0).unwrap(),
        official_end: NaiveTime::from_hms_opt(15, 0, 0).unwrap(),
        broker_accept_start: Some(NaiveTime::from_hms_opt(9, 15, 0).unwrap()),
        safety_cutoff: Some(NaiveTime::from_hms_opt(14, 55, 0).unwrap()),
        funding_mode: FundingMode::MarketValue,
        allocation_time_sensitive: false,
        source: "fixture-a".into(),
        source_published_at: Some(now),
    }];
    let event = test.database.upsert_event(input).unwrap();
    test.database
        .acknowledge(&event.id, event.event_version)
        .unwrap();

    let mut changed = test.database.event(&event.id).unwrap().unwrap();
    changed.max_apply_quantity = Some(20_000);
    changed.sessions[0].official_end = NaiveTime::from_hms_opt(14, 30, 0).unwrap();
    changed.updated_at = now_china();
    let changed = test.database.upsert_event(changed).unwrap();
    assert_eq!(changed.event_version, event.event_version + 1);
    assert_eq!(
        changed.lifecycle_status,
        LifecycleStatus::AcknowledgedNeedsReview
    );

    let (needs_review_at, review_reason): (Option<String>, Option<String>) = test
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT needs_review_at,review_reason FROM acknowledgements WHERE ipo_event_id=?1 AND event_version=?2",
                params![event.id, event.event_version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert!(needs_review_at.is_some());
    let review_reason = review_reason.unwrap();
    assert!(review_reason.contains("申购上限"));
    assert!(review_reason.contains("官方申购时段或资金规则"));
}

#[test]
fn future_event_cannot_be_acknowledged() {
    let test = TestDatabase::new();
    let now = now_china();
    let mut input = test.event();
    input.apply_date = Some(now.date_naive() + chrono::Duration::days(1));
    input.lifecycle_status = LifecycleStatus::Scheduled;
    let event = test.database.upsert_event(input).unwrap();

    let error = test
        .database
        .acknowledge_at(&event.id, event.event_version, now)
        .unwrap_err();
    assert!(error.to_string().contains("只能在申购日当天确认已申购"));
    assert_eq!(
        test.database
            .event(&event.id)
            .unwrap()
            .unwrap()
            .lifecycle_status,
        LifecycleStatus::Scheduled,
    );
    let acknowledgement_count: i64 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM acknowledgements WHERE ipo_event_id=?1",
            [&event.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(acknowledgement_count, 0);
}

#[test]
fn lifecycle_refresh_repairs_legacy_future_acknowledgement() {
    let test = TestDatabase::new();
    let now = now_china();
    let date = now.date_naive() + chrono::Duration::days(1);
    let mut input = test.event();
    input.apply_date = Some(date);
    input.ballot_date = Some(date + chrono::Duration::days(1));
    input.payment_date = Some(date + chrono::Duration::days(2));
    input.listing_date = Some(date + chrono::Duration::days(8));
    input.lifecycle_status = LifecycleStatus::Scheduled;
    let event = test.database.upsert_event(input).unwrap();
    test.database
        .acknowledge_at(
            &event.id,
            event.event_version,
            crate::core::at(date, chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap()),
        )
        .unwrap();

    assert!(test.database.refresh_lifecycle().unwrap());

    assert_eq!(
        test.database
            .event(&event.id)
            .unwrap()
            .unwrap()
            .lifecycle_status,
        LifecycleStatus::Scheduled,
    );
    let (revoked, pending): (i64, i64) = test.database.open().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM acknowledgements WHERE ipo_event_id=?1 AND event_version=?2 AND revoked_at IS NOT NULL), (SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND event_version=?2 AND delivery_state=?3)",
            params![event.id, event.event_version, DeliveryState::Pending as i32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
    assert_eq!(revoked, 1);
    assert!(pending > 0);
    assert!(!test.database.refresh_lifecycle().unwrap());
}

#[test]
fn acknowledgement_can_be_revoked_before_cutoff_and_reminders_are_replanned() {
    let test = TestDatabase::new();
    let date = chrono::NaiveDate::from_ymd_opt(2030, 1, 8).unwrap();
    let confirmation_time =
        crate::core::at(date, chrono::NaiveTime::from_hms_opt(10, 0, 0).unwrap());
    let mut input = test.event();
    input.apply_date = Some(date);
    input.ballot_date = Some(date + chrono::Duration::days(1));
    input.payment_date = Some(date + chrono::Duration::days(2));
    input.listing_date = Some(date + chrono::Duration::days(8));
    input.lifecycle_status = LifecycleStatus::Scheduled;
    let event = test.database.upsert_event(input).unwrap();

    test.database
        .acknowledge_at(&event.id, event.event_version, confirmation_time)
        .unwrap();
    assert_eq!(
        test.database
            .event(&event.id)
            .unwrap()
            .unwrap()
            .lifecycle_status,
        LifecycleStatus::Acknowledged,
    );
    let cancelled: i64 = test.database.open().unwrap().query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND event_version=?2 AND delivery_state=?3",
            params![event.id, event.event_version, DeliveryState::Cancelled as i32],
            |row| row.get(0),
        ).unwrap();
    assert!(cancelled > 0);
    let pending_post_apply: i64 = test.database.open().unwrap().query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND event_version=?2 AND delivery_state=?3 AND reminder_level IN (?4,?5,?6,?7)",
            params![
                event.id,
                event.event_version,
                DeliveryState::Pending as i32,
                ReminderLevel::BallotCheck as i32,
                ReminderLevel::PaymentMorning as i32,
                ReminderLevel::PaymentFollowUp as i32,
                ReminderLevel::ListingMorning as i32,
            ],
            |row| row.get(0),
        ).unwrap();
    assert_eq!(pending_post_apply, 4);

    test.database
        .revoke_acknowledgement_at(
            &event.id,
            event.event_version,
            confirmation_time + chrono::Duration::minutes(1),
        )
        .unwrap();
    assert_eq!(
        test.database
            .event(&event.id)
            .unwrap()
            .unwrap()
            .lifecycle_status,
        LifecycleStatus::ActiveUnconfirmed,
    );
    let (revoked, pending): (i64, i64) = test.database.open().unwrap().query_row(
            "SELECT EXISTS(SELECT 1 FROM acknowledgements WHERE ipo_event_id=?1 AND event_version=?2 AND revoked_at IS NOT NULL), (SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND event_version=?2 AND delivery_state=?3)",
            params![event.id, event.event_version, DeliveryState::Pending as i32],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
    assert_eq!(revoked, 1);
    assert!(pending > 0);
    let pending_post_apply: i64 = test.database.open().unwrap().query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE ipo_event_id=?1 AND event_version=?2 AND delivery_state=?3 AND reminder_level IN (?4,?5,?6,?7)",
            params![
                event.id,
                event.event_version,
                DeliveryState::Pending as i32,
                ReminderLevel::BallotCheck as i32,
                ReminderLevel::PaymentMorning as i32,
                ReminderLevel::PaymentFollowUp as i32,
                ReminderLevel::ListingMorning as i32,
            ],
            |row| row.get(0),
        ).unwrap();
    assert_eq!(pending_post_apply, 0);
}

#[test]
fn detail_evidence_and_manual_override_lifecycle_roundtrip() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    let candidate = Candidate {
        source: "fixture-source".into(),
        priority: 80,
        fetched_at: now,
        published_at: Some(now),
        exchange: Exchange::Shanghai,
        board: Board::Main,
        security_code: Some("601001".into()),
        apply_code: Some("780001".into()),
        legacy_code: None,
        name: Some("测试股份".into()),
        apply_date: event.apply_date,
        issue_price: Some(10.0),
        lot_size: Some(500),
        max_apply_quantity: Some(10_000),
        required_market_value: None,
        required_cash: None,
        ballot_date: None,
        payment_date: None,
        listing_date: None,
        status: IssueStatus::Active,
        announcement_url: Some("https://example.com/announcement.pdf".into()),
        sessions: Vec::new(),
    };
    test.database
        .replace_field_sources(&event.id, &[candidate])
        .unwrap();
    let sources = test.database.field_sources(&event.id).unwrap();
    assert!(sources.iter().any(|source| {
        source.field_name == "IssuePrice"
            && source.normalized_value.as_deref() == Some("10")
            && source.source == "fixture-source"
            && source.priority == 80
    }));

    let document = AnnouncementDocument {
        id: "document-1".into(),
        event_id: event.id.clone(),
        reference: AnnouncementRef {
            provider: "fixture-announcement".into(),
            announcement_id: "announcement-1".into(),
            title: "首次公开发行公告".into(),
            url: "https://example.com/announcement.pdf".into(),
            published_at: Some(now),
            announcement_type: Some("发行公告".into()),
        },
        local_path: "announcements/announcement-1.pdf".into(),
        file_hash: "abc123".into(),
        text_hash: Some("def456".into()),
        status: ExtractionStatus::Extracted,
        parser_version: "test-parser".into(),
        fields: vec![ParsedField {
            name: "IssuePrice".into(),
            value: "12.50".into(),
            confidence: 0.98,
            evidence: Some("发行价格为每股 12.50 元".into()),
            character_offset: Some(42),
        }],
        downloaded_at: now,
    };
    test.database.save_announcement(&document).unwrap();
    let announcements = test.database.announcements(&event.id).unwrap();
    assert_eq!(announcements.len(), 1);
    assert_eq!(announcements[0].reference.title, "首次公开发行公告");
    assert_eq!(announcements[0].fields.len(), 1);
    assert_eq!(announcements[0].fields[0].value, "12.50");

    test.database
        .apply_manual_override(
            &event.id,
            event.event_version,
            "IssuePrice",
            "12.50",
            "已逐项核对发行公告",
            Some(&document.id),
        )
        .unwrap();
    test.database
        .apply_manual_override(
            &event.id,
            event.event_version,
            "OfficialSessions",
            "09:30-11:30，13:00-15:00",
            "公告列明申购时段",
            Some(&document.id),
        )
        .unwrap();
    test.database
        .apply_manual_override(
            &event.id,
            event.event_version,
            "IssueStatus",
            "延期发行",
            "公告宣布延期",
            Some(&document.id),
        )
        .unwrap();
    let overridden = test.database.event(&event.id).unwrap().unwrap();
    assert_eq!(overridden.issue_price, Some(12.5));
    assert_eq!(overridden.sessions.len(), 2);
    assert_eq!(overridden.status, IssueStatus::Postponed);
    assert!(
        overridden
            .manual_override_fields
            .contains(&"IssuePrice".to_owned())
    );
    assert!(
        overridden
            .manual_override_fields
            .contains(&"OfficialSessions".to_owned())
    );
    assert!(
        overridden
            .manual_override_fields
            .contains(&"IssueStatus".to_owned())
    );

    let overrides = test
        .database
        .manual_overrides(&event.id, event.event_version)
        .unwrap();
    assert_eq!(overrides.len(), 3);
    let price_override = overrides
        .iter()
        .find(|entry| entry.field_name == "IssuePrice")
        .unwrap();
    assert_eq!(
        price_override.announcement_document_id.as_deref(),
        Some("document-1")
    );
    test.database
        .revoke_manual_override(&event.id, event.event_version, price_override.id)
        .unwrap();
    let overrides = test
        .database
        .manual_overrides(&event.id, event.event_version)
        .unwrap();
    assert!(
        overrides
            .iter()
            .find(|entry| entry.id == price_override.id)
            .unwrap()
            .revoked_at
            .is_some()
    );
    let after_revoke = test.database.event(&event.id).unwrap().unwrap();
    assert_eq!(after_revoke.issue_price, Some(10.0));
    assert!(
        !after_revoke
            .manual_override_fields
            .contains(&"IssuePrice".to_owned())
    );
}

#[test]
fn manual_override_rejects_empty_reason_invalid_values_and_foreign_announcement() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();

    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "IssuePrice",
                "12.50",
                "   ",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "IssuePrice",
                "0",
                "无效价格",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "ApplyCode",
                "123",
                "无效代码",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "OfficialSessions",
                "15:00-09:30",
                "无效时段",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "IssueStatus",
                "未知状态",
                "无效状态",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "Unsupported",
                "value",
                "不支持字段",
                None,
            )
            .is_err()
    );
    assert!(
        test.database
            .apply_manual_override(
                &event.id,
                event.event_version,
                "IssuePrice",
                "12.50",
                "公告核验",
                Some("missing-document"),
            )
            .is_err()
    );
}

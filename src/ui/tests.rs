use super::*;

fn filter_event(exchange: Exchange, status: LifecycleStatus) -> IpoEvent {
    let now = crate::core::now_china();
    IpoEvent {
        id: "fixture-filter".into(),
        exchange,
        board: Board::Main,
        security_code: "601001".into(),
        apply_code: Some("780001".into()),
        legacy_code: Some("730001".into()),
        name: "筛选测试股份".into(),
        apply_date: Some(now.date_naive()),
        issue_price: None,
        lot_size: None,
        max_apply_quantity: None,
        required_market_value: None,
        required_cash: None,
        ballot_date: None,
        payment_date: None,
        listing_date: None,
        status: model::IssueStatus::Active,
        lifecycle_status: status,
        event_version: 1,
        announcement_url: None,
        data_quality_status: DataQualityStatus::MultiSourceVerified,
        data_conflict: false,
        manual_override_fields: Vec::new(),
        sessions: Vec::new(),
        first_seen_at: now,
        updated_at: now,
    }
}

#[test]
fn reminder_alerts_follow_settings() {
    let mut settings = AppSettings::default();
    let alerts = ReminderAlerts::from_settings(&settings);
    assert!(alerts.sound && alerts.flash && alerts.toast);

    settings.sound_enabled = false;
    settings.flash_taskbar = false;
    settings.toast_enabled = false;
    let alerts = ReminderAlerts::from_settings(&settings);
    assert!(!alerts.sound && !alerts.flash && !alerts.toast);
}

#[test]
fn reminder_alerts_fail_closed_without_settings() {
    let alerts = ReminderAlerts::fail_closed();
    assert_eq!(alerts, ReminderAlerts::default());
    assert!(!alerts.sound && !alerts.flash && !alerts.toast);
}

#[test]
fn acknowledgement_is_available_only_on_the_apply_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();

    assert!(can_acknowledge_on(
        Some(today),
        LifecycleStatus::Scheduled,
        today,
    ));
    assert!(!can_acknowledge_on(
        Some(today + chrono::Duration::days(1)),
        LifecycleStatus::Scheduled,
        today,
    ));
    assert!(!can_acknowledge_on(
        Some(today),
        LifecycleStatus::ExpiredUnconfirmed,
        today,
    ));
}

#[test]
fn sync_interval_parser_supports_minutes_and_hours() {
    assert_eq!(
        parse_sync_interval("30", 0, "普通日期自动同步间隔").unwrap(),
        30
    );
    assert_eq!(
        parse_sync_interval("2", 1, "普通日期自动同步间隔").unwrap(),
        120
    );
    assert!(parse_sync_interval("4", 0, "申购日自动同步间隔").is_err());
    assert_eq!(sync_interval_display(120), ("2".into(), 1));
    assert_eq!(sync_interval_display(20), ("20".into(), 0));
}

#[test]
fn settings_reset_uses_all_application_defaults() {
    let stored = AppSettings {
        active_day_sync_minutes: 5,
        normal_sync_minutes: 120,
        sound_enabled: false,
        notification_self_test_completed: true,
        ..AppSettings::default()
    };

    assert_eq!(
        settings_base_for_save(stored.clone(), false).normal_sync_minutes,
        120
    );
    assert_eq!(settings_base_for_save(stored, true).normal_sync_minutes, 30);

    let defaults = settings_base_for_save(AppSettings::default(), true);
    assert_eq!(defaults.active_day_sync_minutes, 20);
    assert!(defaults.sound_enabled);
    assert!(!defaults.notification_self_test_completed);
}

#[test]
fn notification_self_test_requires_each_enabled_channel() {
    let mut settings = AppSettings::default();
    settings.notification_window_test_passed = Some(true);
    settings.notification_balloon_test_passed = Some(true);
    settings.notification_sound_test_passed = Some(true);
    assert!(!settings.notification_tests_complete());

    settings.notification_flash_test_passed = Some(true);
    assert!(settings.notification_tests_complete());

    settings.notification_balloon_test_passed = Some(false);
    settings.notification_toast_test_passed = Some(true);
    assert!(settings.notification_tests_complete());

    settings.notification_toast_test_passed = Some(false);
    settings.toast_enabled = false;
    assert!(settings.notification_tests_complete());
}

#[test]
fn task_filter_combines_text_market_and_status() {
    let event = filter_event(Exchange::Shanghai, LifecycleStatus::ActiveUnconfirmed);
    assert!(task_matches_filter(&event, "测试", 0, 0));
    assert!(task_matches_filter(&event, "780001", 1, 1));
    assert!(task_matches_filter(&event, "730001", 0, 0));
    assert!(!task_matches_filter(&event, "不存在", 0, 0));
    assert!(!task_matches_filter(&event, "", 2, 0));
    assert!(!task_matches_filter(&event, "", 0, 2));

    let mut review = event;
    review.data_quality_status = DataQualityStatus::ManualReviewRequired;
    assert!(task_matches_filter(&review, "人工核验", 1, 3));
}

#[test]
fn task_filter_handles_thousands_of_fixed_rows_without_losing_matches() {
    let events = (0..2_000)
        .map(|index| {
            let mut event = filter_event(
                if index % 3 == 0 {
                    Exchange::Shanghai
                } else if index % 3 == 1 {
                    Exchange::Shenzhen
                } else {
                    Exchange::Beijing
                },
                LifecycleStatus::ActiveUnconfirmed,
            );
            event.id = format!("fixture-filter-{index}");
            event.security_code = format!("{:06}", 600_000 + index);
            event.apply_code = Some(format!("{:06}", 700_000 + index));
            event.name = format!("固定压力样本{index}");
            event
        })
        .collect::<Vec<_>>();

    let exact = events
        .iter()
        .filter(|event| task_matches_filter(event, "固定压力样本1999", 0, 0))
        .count();
    assert_eq!(exact, 1);
    let shanghai = events
        .iter()
        .filter(|event| task_matches_filter(event, "固定压力样本", 1, 1))
        .count();
    assert_eq!(shanghai, 667);
}

#[test]
fn final_reminder_wins_over_data_changed_for_the_same_event() {
    let event = filter_event(Exchange::Shanghai, LifecycleStatus::ActiveUnconfirmed);
    let now = crate::core::now_china();
    let deliveries = vec![
        ReminderDelivery {
            outbox_id: 1,
            event: event.clone(),
            due_at: now,
            level: ReminderLevel::Final,
            dedupe_key: "final".into(),
            attempt_count: 1,
            message: None,
        },
        ReminderDelivery {
            outbox_id: 2,
            event,
            due_at: now,
            level: ReminderLevel::DataChanged,
            dedupe_key: "changed".into(),
            attempt_count: 1,
            message: Some("发行信息变化".into()),
        },
    ];
    let batch = reminder_batch(&deliveries, None);
    assert!(
        reminder_display_priority(ReminderLevel::Final)
            > reminder_display_priority(ReminderLevel::DataChanged)
    );
    assert!(
        batch
            .body
            .contains(reminder_level_text(ReminderLevel::Final))
    );
    assert!(
        !batch
            .body
            .contains(reminder_level_text(ReminderLevel::DataChanged))
    );
}

#[test]
fn operation_gate_allows_only_one_owner_and_releases_on_drop() {
    let flag = Arc::new(AtomicBool::new(false));
    let first = OperationGate::acquire(Arc::clone(&flag)).unwrap();
    assert!(OperationGate::acquire(Arc::clone(&flag)).is_none());
    drop(first);
    assert!(OperationGate::acquire(flag).is_some());
}

use super::*;
use crate::model::{
    AnnouncementRef, Board, DataQualityStatus, Exchange, IssueStatus, LifecycleStatus,
};
use std::time::Instant;
use uuid::Uuid;

fn sync_at(kind: SyncConclusionKind, finished_at: ChinaDateTime) -> SyncConclusion {
    SyncConclusion {
        kind,
        started_at: finished_at - chrono::Duration::seconds(1),
        finished_at,
        today_count: usize::from(kind == SyncConclusionKind::HealthyNonempty),
        event_count: 0,
        announcement_count: 0,
        successful_sources: Vec::new(),
        missing_sources: Vec::new(),
        summary: "fixture".into(),
    }
}

fn event_at(now: ChinaDateTime, lifecycle_status: LifecycleStatus) -> IpoEvent {
    IpoEvent {
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
        lifecycle_status,
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
fn automatic_sync_uses_the_configured_interval() {
    let mut settings = AppSettings {
        normal_sync_minutes: 90,
        active_day_sync_minutes: 15,
        ..AppSettings::default()
    };
    assert_eq!(
        automatic_sync_interval_for(&settings, false),
        Duration::from_secs(90 * 60)
    );
    assert_eq!(
        automatic_sync_interval_for(&settings, true),
        Duration::from_secs(15 * 60)
    );

    settings.normal_sync_minutes = 0;
    assert_eq!(
        automatic_sync_interval_for(&settings, false),
        Duration::from_secs(5 * 60)
    );

    settings.active_day_sync_minutes = i32::MAX;
    assert_eq!(
        automatic_sync_interval_for(&settings, true),
        Duration::from_secs(7 * 24 * 60 * 60)
    );
}

#[test]
fn automatic_sync_uses_exact_fixed_checks_and_stops_periodic_idle_sync() {
    let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
    let settings = AppSettings::default();

    let before_morning_check = crate::core::at(date, crate::model::time(7, 55));
    let last_sync = sync_at(
        SyncConclusionKind::HealthyNonempty,
        crate::core::at(date, crate::model::time(7, 50)),
    );
    let schedule = automatic_sync_schedule_for(
        &settings,
        before_morning_check,
        true,
        false,
        false,
        Some(&last_sync),
        None,
        "fixture-a",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(date, crate::model::time(8, 0))
    );
    assert!(schedule.reason.contains("08:00"));

    let after_missed_check = crate::core::at(date, crate::model::time(8, 5));
    let old_sync = sync_at(
        SyncConclusionKind::HealthyNonempty,
        crate::core::at(date, crate::model::time(7, 0)),
    );
    let schedule = automatic_sync_schedule_for(
        &settings,
        after_missed_check,
        true,
        false,
        false,
        Some(&old_sync),
        None,
        "fixture-a",
    );
    assert_eq!(schedule.due_at, after_missed_check);

    let before_evening_check = crate::core::at(date, crate::model::time(19, 55));
    let evening_sync = sync_at(
        SyncConclusionKind::HealthyEmpty,
        crate::core::at(date, crate::model::time(19, 0)),
    );
    let schedule = automatic_sync_schedule_for(
        &settings,
        before_evening_check,
        false,
        true,
        false,
        Some(&evening_sync),
        None,
        "fixture-a",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(date, crate::model::time(20, 0))
    );
    assert!(schedule.reason.contains("20:00"));

    let normal_jitter = sync_jitter_seconds("fixture-a", before_evening_check, false);
    let active_jitter = sync_jitter_seconds("fixture-a", before_evening_check, true);
    assert!((0..=90).contains(&normal_jitter));
    assert!((0..=20).contains(&active_jitter));
    assert_eq!(
        normal_jitter,
        sync_jitter_seconds("fixture-a", before_evening_check, false)
    );

    let after_window = crate::core::at(date, crate::model::time(23, 30));
    let schedule = automatic_sync_schedule_for(
        &settings,
        after_window,
        true,
        false,
        false,
        Some(&old_sync),
        None,
        "fixture-after-window",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(date + chrono::Duration::days(1), crate::model::time(6, 0))
    );
}

#[test]
fn healthy_idle_sync_skips_the_rest_of_the_day_and_the_weekend() {
    let settings = AppSettings::default();
    let thursday = NaiveDate::from_ymd_opt(2026, 8, 27).unwrap();
    let thursday_now = crate::core::at(thursday, crate::model::time(10, 0));
    let thursday_sync = sync_at(SyncConclusionKind::HealthyEmpty, thursday_now);
    let schedule = automatic_sync_schedule_for(
        &settings,
        thursday_now,
        false,
        false,
        false,
        Some(&thursday_sync),
        None,
        "idle-thursday",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 28).unwrap(),
            crate::model::time(8, 0),
        )
    );

    let friday = NaiveDate::from_ymd_opt(2026, 8, 28).unwrap();
    let friday_now = crate::core::at(friday, crate::model::time(10, 0));
    let friday_sync = sync_at(SyncConclusionKind::HealthyEmpty, friday_now);
    let schedule = automatic_sync_schedule_for(
        &settings,
        friday_now,
        false,
        false,
        false,
        Some(&friday_sync),
        None,
        "idle-friday",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(
            NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
            crate::model::time(8, 0),
        )
    );
}

#[test]
fn degraded_sync_keeps_a_bounded_retry_deadline() {
    let settings = AppSettings {
        normal_sync_minutes: 30,
        ..AppSettings::default()
    };
    let date = NaiveDate::from_ymd_opt(2026, 8, 27).unwrap();
    let now = crate::core::at(date, crate::model::time(10, 0));
    let last = sync_at(
        SyncConclusionKind::Unknown,
        crate::core::at(date, crate::model::time(9, 55)),
    );
    let retry = crate::core::at(date, crate::model::time(10, 7));
    let schedule = automatic_sync_schedule_for(
        &settings,
        now,
        false,
        false,
        false,
        Some(&last),
        Some(retry),
        "degraded",
    );
    assert_eq!(schedule.due_at, retry);
    assert!(schedule.reason.contains("退避"));
}

#[test]
fn skipped_startup_sync_remains_suppressed_after_deadline_recalculation() {
    let date = NaiveDate::from_ymd_opt(2026, 8, 27).unwrap();
    let now = crate::core::at(date, crate::model::time(11, 0));
    let not_before = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 28).unwrap(),
        crate::model::time(8, 0),
    );
    let mut first = AutomaticSyncSchedule {
        due_at: now,
        reason: "工作日一次发现同步".into(),
    };
    apply_sync_not_before(&mut first, Some(not_before));
    assert_eq!(first.due_at, not_before);

    let mut recalculated = AutomaticSyncSchedule {
        due_at: now,
        reason: "工作日一次发现同步".into(),
    };
    apply_sync_not_before(&mut recalculated, Some(not_before));
    assert_eq!(recalculated.due_at, not_before);
    assert!(recalculated.reason.contains("跳过"));
}

#[test]
fn acknowledged_tasks_stop_active_sync_but_unknown_follow_up_dates_are_discovered() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-sync-state-test-{}",
        Uuid::new_v4().simple()
    ));
    let database = Database::new(&root);
    database.initialize().unwrap();
    let settings = AppSettings::default();
    database.save_settings(&settings).unwrap();
    let now = now_china();
    database
        .upsert_event(event_at(now, LifecycleStatus::Acknowledged))
        .unwrap();

    assert!(!has_sync_relevant_events_on(
        &database,
        &settings,
        now.date_naive()
    ));
    assert!(has_unknown_follow_up_events(&database, &settings, now));

    let mut complete = database.event("shanghai:601001").unwrap().unwrap();
    complete.ballot_date = Some(now.date_naive() + chrono::Duration::days(1));
    complete.payment_date = Some(now.date_naive() + chrono::Duration::days(2));
    complete.listing_date = Some(now.date_naive() + chrono::Duration::days(7));
    complete.updated_at = now + chrono::Duration::seconds(1);
    database.upsert_event(complete).unwrap();
    assert!(!has_unknown_follow_up_events(&database, &settings, now));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deadline_scheduler_prioritizes_real_weekend_reminders_and_overdue_recovery() {
    let friday = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 28).unwrap(),
        crate::model::time(10, 0),
    );
    let monday = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
        crate::model::time(8, 0),
    );
    let sunday_reminder = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 30).unwrap(),
        crate::model::time(20, 0),
    );
    let mut deadline = WakeDeadline::new(monday, "工作日发现同步".into());
    deadline.consider(Some(sunday_reminder), "本地提醒到期");
    assert_eq!(deadline.at, sunday_reminder);
    assert_eq!(deadline.reason, "本地提醒到期");
    assert_eq!(
        duration_until(friday - chrono::Duration::seconds(1), friday),
        Duration::ZERO
    );
}

#[test]
fn enabled_markets_define_required_sync_sources() {
    let all_sources = HashSet::from(["eastmoney", "sse", "cninfo", "bse"]);
    assert!(missing_required_sources(&AppSettings::default(), &all_sources).is_empty());

    let shanghai_only = AppSettings {
        shenzhen_enabled: false,
        beijing_enabled: false,
        ..AppSettings::default()
    };
    let shanghai_sources = HashSet::from(["eastmoney", "sse"]);
    assert!(missing_required_sources(&shanghai_only, &shanghai_sources).is_empty());

    let without_bse = HashSet::from(["eastmoney", "sse", "cninfo"]);
    assert_eq!(
        missing_required_sources(&AppSettings::default(), &without_bse),
        vec!["bse"]
    );
}

#[test]
fn completion_text_only_claims_no_ipo_after_complete_coverage() {
    let started = now_china();
    let sources = HashSet::from(["eastmoney", "sse", "cninfo", "bse"]);
    let no_ipo = sync_conclusion(started, started, 0, 0, 0, &sources, &[]);
    assert_eq!(no_ipo.kind, SyncConclusionKind::HealthyEmpty);
    assert!(no_ipo.summary.contains("今日无新股"));

    let unknown = sync_conclusion(started, started, 0, 0, 0, &sources, &["bse"]);
    assert_eq!(unknown.kind, SyncConclusionKind::Unknown);
    assert!(!unknown.summary.contains("今日无新股"));
    assert!(unknown.summary.contains("暂未获取到今日任务"));
    assert!(unknown.summary.contains("来源覆盖不完整"));

    let retained = sync_conclusion(started, started, 2, 1, 0, &sources, &["cninfo"]);
    assert_eq!(retained.kind, SyncConclusionKind::DegradedCached);
    assert!(retained.summary.contains("已保留现有今日任务"));
    assert!(retained.summary.contains("来源覆盖不完整"));
}

#[test]
fn windows_time_service_status_degrades_clock_health_without_hiding_failure() {
    let (warning, warning_text) = add_windows_time_status(
        HealthState::Healthy,
        "网络时间样本正常".into(),
        Ok(Some(false)),
    );
    assert_eq!(warning, HealthState::Warning);
    assert!(warning_text.contains("W32Time"));

    let (failed, _) = add_windows_time_status(
        HealthState::Failed,
        "网络时间偏差过大".into(),
        Ok(Some(false)),
    );
    assert_eq!(failed, HealthState::Failed);
}

#[test]
fn clock_health_matrix_covers_missing_samples_large_drift_and_service_failures() {
    let (unknown, text) = evaluate_clock_offsets(Vec::new(), "fixture", Ok(Some(true)));
    assert_eq!(unknown, HealthState::Unknown);
    assert!(text.contains("0/2"));

    let (one_sample, text) = evaluate_clock_offsets(vec![30_000], "fixture", Ok(Some(true)));
    assert_eq!(one_sample, HealthState::Warning);
    assert!(text.contains("样本不足"));

    let (healthy, _) = evaluate_clock_offsets(vec![-20_000, 20_000], "fixture", Ok(Some(true)));
    assert_eq!(healthy, HealthState::Healthy);

    let (large_drift, text) = evaluate_clock_offsets(
        vec![6 * 60 * 1000, 7 * 60 * 1000],
        "fixture",
        Ok(Some(false)),
    );
    assert_eq!(large_drift, HealthState::Failed);
    assert!(text.contains("偏差过大"));
    assert!(text.contains("未运行"));

    let (service_error, text) =
        evaluate_clock_offsets(vec![0, 0], "fixture", Err(anyhow::anyhow!("access denied")));
    assert_eq!(service_error, HealthState::Warning);
    assert!(text.contains("状态读取失败"));
}

#[test]
fn automatic_sync_handles_cross_day_boundaries_and_clock_rollback_inputs() {
    let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
    let settings = AppSettings::default();
    let at_window_end = crate::core::at(date, crate::model::time(22, 0));
    let failed = sync_at(SyncConclusionKind::Unknown, at_window_end);
    let schedule = automatic_sync_schedule_for(
        &settings,
        at_window_end,
        false,
        false,
        false,
        Some(&failed),
        None,
        "cross-day",
    );
    assert_eq!(
        schedule.due_at,
        crate::core::at(date + chrono::Duration::days(1), crate::model::time(6, 0))
    );

    let after_rollback = crate::core::at(date, crate::model::time(7, 30));
    let future_last_sync = crate::core::at(date, crate::model::time(9, 0));
    let future = sync_at(SyncConclusionKind::HealthyNonempty, future_last_sync);
    let schedule = automatic_sync_schedule_for(
        &settings,
        after_rollback,
        true,
        false,
        false,
        Some(&future),
        None,
        "clock-rollback",
    );
    assert!(schedule.due_at > after_rollback);
    assert!(schedule.due_at <= after_rollback + chrono::Duration::minutes(11));
}

#[test]
fn announcement_run_state_distinguishes_failure_warning_and_success() {
    let mut failed = AnnouncementRunStats::new(now_china());
    failed.attempted_events = 1;
    failed.record_issue("公告元数据检索失败");
    assert_eq!(failed.state(), HealthState::Failed);

    let mut warning = AnnouncementRunStats::new(now_china());
    warning.attempted_events = 2;
    warning.successful_searches = 2;
    warning.references_found = 2;
    warning.metadata_records = 2;
    warning.record_issue("一个事件由备用镜像接管");
    assert_eq!(warning.state(), HealthState::Warning);

    let mut healthy = AnnouncementRunStats::new(now_china());
    healthy.attempted_events = 1;
    healthy.successful_searches = 1;
    assert_eq!(healthy.state(), HealthState::Healthy);
}

#[test]
fn new_event_is_saved_before_its_announcement_metadata() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-rust-runtime-test-{}",
        Uuid::new_v4().simple()
    ));
    let database = Database::new(&root);
    database.initialize().unwrap();
    let now = now_china();
    let event = event_at(now, LifecycleStatus::ActiveUnconfirmed);
    let document = announcement::metadata_document(
        &event,
        AnnouncementRef {
            provider: "sse-announcement".into(),
            announcement_id: "announcement-1".into(),
            title: "首次公开发行公告".into(),
            url: "https://www.sse.com.cn/test.pdf".into(),
            published_at: Some(now),
            announcement_type: Some("发行公告".into()),
        },
    );

    persist_reconciled_group(&database, event, &[], &[document]).unwrap();

    assert!(database.event("shanghai:601001").unwrap().is_some());
    assert_eq!(
        database.announcement_titles("shanghai:601001").unwrap(),
        vec!["首次公开发行公告"]
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn runtime_start_does_not_wait_for_database_initialization() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-runtime-start-test-{}",
        Uuid::new_v4().simple()
    ));
    let started = Instant::now();
    let (runtime, worker) = start_with_initializer(root.clone(), false, |_, _| {
        thread::sleep(Duration::from_millis(400));
        anyhow::bail!("simulated initialization failure")
    })
    .unwrap();

    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(!runtime.is_ready());
    assert!(runtime.settings().is_err());
    worker.join().unwrap();
    assert_eq!(runtime.snapshot().health_state, HealthState::Failed);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stop_signal_is_observed_by_sync_cancellation_boundaries() {
    let stop = AtomicBool::new(false);
    assert!(ensure_not_stopping(&stop).is_ok());
    stop.store(true, Ordering::Release);
    assert!(ensure_not_stopping(&stop).is_err());
}

#[test]
fn snapshot_revision_only_changes_when_visible_state_changes() {
    let ui_state = RuntimeUiState::new();

    update_snapshot(&ui_state, |_| {});
    assert_eq!(ui_state.snapshot.read().unwrap().revision, 0);

    update_snapshot(&ui_state, |value| {
        value.status_text = "后台提醒服务已就绪".into();
    });
    assert_eq!(ui_state.snapshot.read().unwrap().revision, 1);

    update_snapshot(&ui_state, |value| {
        value.status_text = "后台提醒服务已就绪".into();
    });
    assert_eq!(ui_state.snapshot.read().unwrap().revision, 1);
}

#[test]
fn daily_maintenance_skips_repeated_backup_without_business_changes() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-rust-maintenance-test-{}",
        Uuid::new_v4().simple()
    ));
    let database = Database::new(&root);
    database.initialize().unwrap();
    database.save_settings(&AppSettings::default()).unwrap();

    run_daily_maintenance(&database, &root);
    let backup_directory = root.join("backups");
    let first = std::fs::read_dir(&backup_directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "db"))
        .unwrap();
    let old = backup_directory.join("stock-ipo-reminder-20000101-000000-000.db");
    let old_fingerprint = backup_fingerprint_path(&old);
    std::fs::rename(&first, &old).unwrap();
    std::fs::rename(backup_fingerprint_path(&first), old_fingerprint).unwrap();

    run_daily_maintenance(&database, &root);
    let second_count = std::fs::read_dir(&backup_directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "db"))
        .count();
    assert_eq!(second_count, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn daily_maintenance_creates_a_new_backup_after_business_changes() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-rust-maintenance-change-test-{}",
        Uuid::new_v4().simple()
    ));
    let database = Database::new(&root);
    database.initialize().unwrap();
    database.save_settings(&AppSettings::default()).unwrap();
    run_daily_maintenance(&database, &root);
    let backup_directory = root.join("backups");
    let first = std::fs::read_dir(&backup_directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "db"))
        .unwrap();
    let old = backup_directory.join("stock-ipo-reminder-20000101-000000-000.db");
    std::fs::rename(&first, &old).unwrap();
    std::fs::rename(
        backup_fingerprint_path(&first),
        backup_fingerprint_path(&old),
    )
    .unwrap();

    let mut settings = database.settings().unwrap();
    settings.sound_enabled = !settings.sound_enabled;
    database.save_settings(&settings).unwrap();
    run_daily_maintenance(&database, &root);

    let count = std::fs::read_dir(&backup_directory)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "db"))
        .count();
    assert_eq!(count, 2);
    let _ = std::fs::remove_dir_all(root);
}

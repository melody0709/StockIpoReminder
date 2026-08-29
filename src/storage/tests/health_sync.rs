use super::*;

#[test]
fn merged_runtime_heartbeats_update_both_components() {
    let test = TestDatabase::new();
    test.database.touch_runtime_heartbeats(now_china()).unwrap();
    let connection = test.database.open().unwrap();
    for component in ["scheduler", "delivery"] {
        let count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM app_heartbeat WHERE component=?1 AND heartbeat_at IS NOT NULL",
                    params![component],
                    |row| row.get(0),
                )
                .unwrap();
        assert_eq!(count, 1, "component {component} 应被合并心跳更新");
    }
}

#[test]
fn retry_after_takes_priority_and_local_backoff_has_bounded_jitter() {
    let test = TestDatabase::new();
    let retry_after = now_china() + chrono::Duration::minutes(12);
    let next = test
        .database
        .save_source_run_with_retry_after(
            "retry-after-fixture",
            now_china(),
            HealthState::Failed,
            0,
            None,
            None,
            None,
            Some("rate limited"),
            Some(retry_after),
        )
        .unwrap()
        .unwrap();
    assert_eq!(next, retry_after);

    let fixed = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
    );
    let first = source_backoff_delay("fixture", 1, fixed).num_seconds();
    let saturated = source_backoff_delay("fixture", 8, fixed).num_seconds();
    assert!((60..=66).contains(&first));
    assert!((1800..=1980).contains(&saturated));
}

#[test]
fn backoff_health_probe_is_persistent_rate_limited_and_keeps_api_backoff() {
    let test = TestDatabase::new();
    let now = now_china();
    let retry_after = now + chrono::Duration::hours(2);
    test.database
        .save_source_run_with_retry_after(
            "probe-fixture",
            now,
            HealthState::Failed,
            0,
            None,
            None,
            None,
            Some("rate limited"),
            Some(retry_after),
        )
        .unwrap();
    let connection = test.database.open().unwrap();
    let next_probe: Option<String> = connection
        .query_row(
            "SELECT next_probe_at FROM source_backoff WHERE source='probe-fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(next_probe.is_some());
    connection
        .execute(
            "UPDATE source_backoff SET next_probe_at=?1 WHERE source='probe-fixture'",
            [format_dt(now - chrono::Duration::seconds(1))],
        )
        .unwrap();
    drop(connection);

    assert!(
        test.database
            .try_claim_source_probe("probe-fixture", now)
            .unwrap()
    );
    assert!(
        !test
            .database
            .try_claim_source_probe("probe-fixture", now)
            .unwrap()
    );
    test.database
        .save_source_probe_run("probe-fixture", now, true, None)
        .unwrap();

    let connection = test.database.open().unwrap();
    let (probe_success, probe_run_success): (Option<i32>, i32) = connection
            .query_row(
                "SELECT last_probe_success, (SELECT success FROM sync_runs WHERE source='health-probe:probe-fixture' ORDER BY id DESC LIMIT 1) FROM source_backoff WHERE source='probe-fixture'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!(probe_success, Some(1));
    assert_eq!(probe_run_success, 1);
    assert!(
        !test
            .database
            .source_can_attempt("probe-fixture", now)
            .unwrap()
            .0
    );
}

#[test]
fn warning_source_run_is_successful_without_losing_diagnostics() {
    let test = TestDatabase::new();
    test.database
        .save_source_run(
            "fixture-announcement",
            now_china(),
            HealthState::Warning,
            2,
            Some("attemptedEvents=2, documents=2, issues=1"),
            None,
            Some("announcement-run-v2"),
            Some("一个事件已由备用镜像接管"),
        )
        .unwrap();
    let connection = test.database.open().unwrap();
    let (state, failures, last_success, last_error): (i32, i32, Option<String>, Option<String>) =
            connection
                .query_row(
                    "SELECT health_state,consecutive_failures,last_success_at,last_error FROM source_health WHERE source='fixture-announcement'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
    assert_eq!(state, HealthState::Warning as i32);
    assert_eq!(failures, 0);
    assert!(last_success.is_some());
    assert_eq!(last_error.as_deref(), Some("一个事件已由备用镜像接管"));
    assert!(
        test.database
            .source_can_attempt("fixture-announcement", now_china())
            .unwrap()
            .0
    );
    let successful_run: i32 = connection
            .query_row(
                "SELECT success FROM sync_runs WHERE source='fixture-announcement' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(successful_run, 1);
    assert_eq!(
        test.database
            .health_details()
            .unwrap()
            .sources
            .into_iter()
            .find(|source| source.source == "fixture-announcement")
            .unwrap()
            .state,
        HealthState::Warning
    );
}

#[test]
fn source_freshness_respects_the_configured_sync_interval() {
    let test = TestDatabase::new();
    let settings = AppSettings {
        normal_sync_minutes: 240,
        active_day_sync_minutes: 10,
        ..AppSettings::default()
    };
    test.database.save_settings(&settings).unwrap();
    let last_success = now_china() - chrono::Duration::hours(3);
    test.database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO source_health(source,last_attempt_at,last_success_at,last_record_count,consecutive_failures,health_state) VALUES('eastmoney',?1,?1,10,0,?2)",
                params![format_dt(last_success), HealthState::Healthy as i32],
            )
            .unwrap();
    let source = test
        .database
        .health_details()
        .unwrap()
        .sources
        .into_iter()
        .find(|source| source.source == "eastmoney")
        .unwrap();
    assert_eq!(source.state, HealthState::Healthy);
}

#[test]
fn runtime_heartbeat_health_transitions_from_healthy_to_warning_and_failed() {
    let test = TestDatabase::new();
    let now = now_china();
    test.database
        .save_settings(&AppSettings::default())
        .unwrap();
    test.database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO source_health(source,last_attempt_at,last_success_at,last_record_count,consecutive_failures,health_state) VALUES('eastmoney',?1,?1,0,0,?2)",
                params![format_dt(now), HealthState::Healthy as i32],
            )
            .unwrap();

    let missing = test.database.health_details().unwrap();
    assert_eq!(missing.overall_state, HealthState::Failed);
    assert!(missing.scheduler_heartbeat.is_none());
    assert!(missing.delivery_heartbeat.is_none());

    test.database.touch_heartbeat("scheduler", now).unwrap();
    test.database.touch_heartbeat("delivery", now).unwrap();
    assert_eq!(
        test.database.health_details().unwrap().overall_state,
        HealthState::Healthy
    );

    test.database
        .touch_heartbeat("scheduler", now - chrono::Duration::minutes(5))
        .unwrap();
    assert_eq!(
        test.database.health_details().unwrap().overall_state,
        HealthState::Warning
    );

    test.database
        .touch_heartbeat("scheduler", now - chrono::Duration::minutes(16))
        .unwrap();
    assert_eq!(
        test.database.health_details().unwrap().overall_state,
        HealthState::Failed
    );
}

#[test]
fn sync_conclusion_migration_and_all_kinds_roundtrip() {
    let test = TestDatabase::new();
    let connection = test.database.open().unwrap();
    let (migration_applied, table_exists): (i32, i32) = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=4), EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync_conclusions')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
    assert_eq!((migration_applied, table_exists), (1, 1));
    drop(connection);

    let started = crate::core::at(
        NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
        NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
    );
    for (index, kind) in [
        SyncConclusionKind::Unknown,
        SyncConclusionKind::HealthyNonempty,
        SyncConclusionKind::HealthyEmpty,
        SyncConclusionKind::DegradedCached,
    ]
    .into_iter()
    .enumerate()
    {
        let conclusion = SyncConclusion {
            kind,
            started_at: started + chrono::Duration::minutes(index as i64),
            finished_at: started + chrono::Duration::minutes(index as i64 + 1),
            today_count: index,
            event_count: index + 10,
            announcement_count: index + 20,
            successful_sources: vec!["eastmoney".into(), "sse".into()],
            missing_sources: vec!["cninfo".into()],
            summary: format!("fixture-{kind:?}"),
        };
        test.database.save_sync_conclusion(&conclusion).unwrap();
        let loaded = test.database.latest_sync_conclusion().unwrap().unwrap();
        assert_eq!(loaded.kind, conclusion.kind);
        assert_eq!(loaded.started_at, conclusion.started_at);
        assert_eq!(loaded.finished_at, conclusion.finished_at);
        assert_eq!(loaded.today_count, conclusion.today_count);
        assert_eq!(loaded.event_count, conclusion.event_count);
        assert_eq!(loaded.announcement_count, conclusion.announcement_count);
        assert_eq!(loaded.successful_sources, conclusion.successful_sources);
        assert_eq!(loaded.missing_sources, conclusion.missing_sources);
        assert_eq!(loaded.summary, conclusion.summary);
    }

    let connection = test.database.open().unwrap();
    let healthy_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_runs WHERE source='sync-conclusion' AND success=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let degraded_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sync_runs WHERE source='sync-conclusion' AND success=0 AND error IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(healthy_count, 2);
    assert_eq!(degraded_count, 2);
}

#[test]
fn daily_health_summary_is_exactly_once_across_restarts() {
    let test = TestDatabase::new();
    let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
    let before = crate::core::at(date, NaiveTime::from_hms_opt(7, 59, 59).unwrap());
    let due = crate::core::at(date, NaiveTime::from_hms_opt(8, 0, 0).unwrap());

    assert!(!test.database.try_mark_health_summary_due(before).unwrap());
    assert!(test.database.try_mark_health_summary_due(due).unwrap());
    assert!(!test.database.try_mark_health_summary_due(due).unwrap());

    let reopened = Database::new(&test.root);
    reopened.initialize().unwrap();
    assert!(
        !reopened
            .try_mark_health_summary_due(due + chrono::Duration::hours(3))
            .unwrap()
    );
    assert!(
        reopened
            .try_mark_health_summary_due(due + chrono::Duration::days(1))
            .unwrap()
    );
}

#[test]
fn operation_health_migration_and_failure_affect_overall_health() {
    let test = TestDatabase::new();
    assert_eq!(
        test.database.schema_version().unwrap(),
        LATEST_SCHEMA_VERSION
    );
    let table_exists: i32 = test
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='operation_health')",
                [],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(table_exists, 1);

    test.database
        .save_operation_health(
            "database-backup",
            HealthState::Failed,
            Some("fixture failure"),
        )
        .unwrap();
    let entries = test.database.operation_health().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].component, "database-backup");
    assert_eq!(entries[0].state, HealthState::Failed);
    assert_eq!(entries[0].last_error.as_deref(), Some("fixture failure"));
    assert_eq!(
        test.database.health_details().unwrap().overall_state,
        HealthState::Failed
    );

    test.database
        .save_operation_health("database-backup", HealthState::Healthy, None)
        .unwrap();
    let entry = test.database.operation_health().unwrap().remove(0);
    assert_eq!(entry.state, HealthState::Healthy);
    assert!(entry.last_success_at.is_some());
    assert!(entry.last_error.is_none());
}

#[test]
fn diagnostic_summary_queries_return_structured_runtime_history() {
    let test = TestDatabase::new();
    let conclusion = SyncConclusion {
        kind: SyncConclusionKind::DegradedCached,
        started_at: now_china() - chrono::Duration::minutes(1),
        finished_at: now_china(),
        today_count: 1,
        event_count: 2,
        announcement_count: 3,
        successful_sources: vec!["eastmoney".into()],
        missing_sources: vec!["bse".into()],
        summary: "fixture degraded".into(),
    };
    test.database.save_sync_conclusion(&conclusion).unwrap();

    let runs = test.database.recent_sync_runs(10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].source, "sync-conclusion");
    assert!(!runs[0].success);
    assert_eq!(runs[0].record_count, 1);
    assert!(runs[0].error.as_deref().unwrap().contains("fixture"));

    let reminders = test.database.reminder_state_summary().unwrap();
    assert_eq!(reminders.pending, 0);
    assert_eq!(reminders.shown_last_seven_days, 0);
    assert!(test.database.recent_reminder_log(10).unwrap().is_empty());
}

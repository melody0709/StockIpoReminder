use super::*;

#[test]
fn secondary_notification_outbox_retries_independently_and_completes_as_a_batch() {
    let test = TestDatabase::new();
    let mut settings = test.database.settings().unwrap_or_default();
    settings.secondary_notification_enabled = true;
    settings.secondary_notification_provider = SecondaryNotificationProvider::PushPlus;
    test.database.save_settings(&settings).unwrap();
    test.database.upsert_event(test.event()).unwrap();

    let now = now_china();
    let local = test.database.claim_due_at(50, now).unwrap();
    assert!(!local.is_empty());
    let first = test.database.claim_secondary_due_at(50, now).unwrap();
    assert!(!first.is_empty());
    assert!(
        first
            .iter()
            .all(|delivery| delivery.provider == SecondaryNotificationProvider::PushPlus)
    );
    test.database
        .fail_secondary_deliveries(&first, "fixture unavailable")
        .unwrap();
    assert!(
        test.database
            .claim_secondary_due_at(50, now + chrono::Duration::seconds(30))
            .unwrap()
            .is_empty()
    );

    let retry = test
        .database
        .claim_secondary_due_at(50, now + chrono::Duration::minutes(2))
        .unwrap();
    assert_eq!(retry.len(), first.len());
    test.database
        .complete_secondary_deliveries(&retry, "pushplus-test")
        .unwrap();
    let summary = test.database.secondary_notification_summary().unwrap();
    assert_eq!(summary.delivered, retry.len() as i64);
    assert_eq!(summary.retrying, 0);
    assert_eq!(summary.requests_last_hour, 2);
    assert!(summary.latest_success_at.is_some());
}

#[test]
fn secondary_provider_switch_and_same_provider_reenable_revive_pending_work() {
    let test = TestDatabase::new();
    let mut settings = AppSettings {
        secondary_notification_enabled: true,
        secondary_notification_provider: SecondaryNotificationProvider::PushPlus,
        ..AppSettings::default()
    };
    test.database.save_settings(&settings).unwrap();
    test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    assert!(!test.database.claim_due_at(50, now).unwrap().is_empty());

    settings.secondary_notification_enabled = false;
    test.database.save_settings(&settings).unwrap();
    settings.secondary_notification_enabled = true;
    test.database.save_settings(&settings).unwrap();
    let reenabled = test
        .database
        .claim_secondary_due_at(50, now + chrono::Duration::seconds(1))
        .unwrap();
    assert!(!reenabled.is_empty());
    assert!(
        reenabled
            .iter()
            .all(|delivery| { delivery.provider == SecondaryNotificationProvider::PushPlus })
    );
    test.database
        .fail_secondary_deliveries(&reenabled, "switch fixture")
        .unwrap();

    settings.secondary_notification_provider = SecondaryNotificationProvider::WeCom;
    test.database.save_settings(&settings).unwrap();
    let switched = test
        .database
        .claim_secondary_due_at(50, now + chrono::Duration::minutes(2))
        .unwrap();
    assert!(!switched.is_empty());
    assert!(
        switched
            .iter()
            .all(|delivery| { delivery.provider == SecondaryNotificationProvider::WeCom })
    );
}

#[test]
fn v10_migration_changes_secondary_identity_to_reminder_and_provider() {
    let test = TestDatabase::new_at_v9();
    assert_eq!(test.database.schema_version().unwrap(), 9);
    let settings = AppSettings {
        secondary_notification_enabled: true,
        secondary_notification_provider: SecondaryNotificationProvider::PushPlus,
        ..AppSettings::default()
    };
    test.database
        .open()
        .unwrap()
        .execute(
            "INSERT INTO app_settings(id,json_value,updated_at) VALUES(1,?1,?2)",
            params![
                serde_json::to_string(&settings).unwrap(),
                format_dt(now_china())
            ],
        )
        .unwrap();
    test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    let connection = test.database.open().unwrap();
    let reminder_id: i64 = connection
        .query_row(
            "SELECT id FROM reminder_outbox ORDER BY due_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
            .execute(
                "INSERT INTO secondary_notification_outbox(
                    reminder_outbox_id,provider,state,attempt_count,next_attempt_at,created_at,updated_at
                 ) VALUES(?1,?2,?3,0,?4,?4,?4)",
                params![
                    reminder_id,
                    SecondaryNotificationProvider::PushPlus as i32,
                    SECONDARY_PENDING,
                    format_dt(now),
                ],
            )
            .unwrap();
    migrate_secondary_notification_identity_v10(&connection).unwrap();
    drop(connection);

    let mut switched = settings;
    switched.secondary_notification_provider = SecondaryNotificationProvider::WeCom;
    test.database.save_settings(&switched).unwrap();
    let providers: i64 = test
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(DISTINCT provider) FROM secondary_notification_outbox WHERE reminder_outbox_id=?1",
                [reminder_id],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(providers, 2);
    assert_eq!(test.database.schema_version().unwrap(), 10);
    test.database.integrity_check().unwrap();
}

#[test]
fn secondary_notification_enforces_hourly_request_quota() {
    let test = TestDatabase::new();
    let mut settings = test.database.settings().unwrap_or_default();
    settings.secondary_notification_enabled = true;
    settings.secondary_notification_provider = SecondaryNotificationProvider::PushPlus;
    test.database.save_settings(&settings).unwrap();
    test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    assert!(!test.database.claim_due_at(50, now).unwrap().is_empty());
    let connection = test.database.open().unwrap();
    connection
            .execute(
                "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,0,1,'old-fixture')",
                params![
                    format_dt(now - chrono::Duration::days(31)),
                    SecondaryNotificationProvider::PushPlus as i32
                ],
            )
            .unwrap();
    for _ in 0..SECONDARY_REQUESTS_PER_HOUR {
        connection
                .execute(
                    "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,0,1,'fixture')",
                    params![format_dt(now), SecondaryNotificationProvider::PushPlus as i32],
                )
                .unwrap();
    }
    assert!(
        test.database
            .claim_secondary_due_at(50, now + chrono::Duration::minutes(1))
            .unwrap()
            .is_empty()
    );
    let old_records: i64 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE error='old-fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_records, 0);
}

#[test]
fn secondary_notification_stops_after_five_failed_attempts() {
    let test = TestDatabase::new();
    let mut settings = test.database.settings().unwrap_or_default();
    settings.secondary_notification_enabled = true;
    settings.secondary_notification_provider = SecondaryNotificationProvider::PushPlus;
    test.database.save_settings(&settings).unwrap();
    test.database.upsert_event(test.event()).unwrap();
    let mut now = now_china();
    assert!(!test.database.claim_due_at(50, now).unwrap().is_empty());

    for advance_minutes in [0, 2, 8, 24, 55] {
        let attempt_at = now + chrono::Duration::minutes(advance_minutes);
        let deliveries = test
            .database
            .claim_secondary_due_at(50, attempt_at)
            .unwrap();
        assert!(!deliveries.is_empty());
        test.database
            .fail_secondary_deliveries(&deliveries, "fixture unavailable")
            .unwrap();
    }
    now += chrono::Duration::minutes(120);
    assert!(
        test.database
            .claim_secondary_due_at(50, now)
            .unwrap()
            .is_empty()
    );
    let summary = test.database.secondary_notification_summary().unwrap();
    assert!(summary.exhausted > 0);
    assert_eq!(summary.retrying, 0);
}

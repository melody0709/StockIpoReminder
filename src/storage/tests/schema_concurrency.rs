use super::*;

#[test]
fn migration_and_integrity_are_compatible() {
    let test = TestDatabase::new();
    test.database.integrity_check().unwrap();
    assert!(test.database.path().exists());
}

#[test]
fn deferred_read_then_write_upgrade_fails_after_concurrent_commit() {
    // M3 机制复现：A 以 DEFERRED 开启事务并完成读取（持有读快照），
    // B 提交一次写入后，A 再尝试写入会立即得到 BUSY（读快照失效，
    // busy_timeout 对该场景不生效）。顺序由单线程语句交错精确控制，
    // 不依赖线程调度。
    let test = TestDatabase::new();
    test.database
        .save_settings(&AppSettings::default())
        .unwrap();
    let mut reader = test.database.open().unwrap();
    let writer = test.database.open().unwrap();

    let tx = reader.transaction().unwrap();
    let _: Option<String> = tx
        .query_row(
            "SELECT json_value FROM app_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    writer
        .execute(
            "UPDATE app_settings SET json_value='from-b',updated_at=?1 WHERE id=1",
            params![format_dt(now_china())],
        )
        .unwrap();

    let error = tx
        .execute("UPDATE app_settings SET json_value='from-a' WHERE id=1", [])
        .unwrap_err();
    assert!(
        matches!(
            error,
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::DatabaseBusy
        ),
        "DEFERRED 读后写升级应立即返回 BUSY，实际：{error}"
    );
    drop(tx);
}

#[test]
fn immediate_transactions_serialize_writers_deterministically() {
    // 修复后的语义：A 用 IMMEDIATE 在 BEGIN 时取得写权并完成读-改-写；
    // B 的写入在 A 提交前明确失败（短 busy_timeout），A 提交后结果完整，
    // 不存在读快照失效导致的升级错误或丢失更新。
    let test = TestDatabase::new();
    test.database
        .save_settings(&AppSettings::default())
        .unwrap();
    let mut a = test.database.open().unwrap();
    let b = test.database.open().unwrap();
    b.busy_timeout(std::time::Duration::from_millis(50))
        .unwrap();

    let tx = a
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let _: Option<String> = tx
        .query_row(
            "SELECT json_value FROM app_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    tx.execute("UPDATE app_settings SET json_value='from-a' WHERE id=1", [])
        .unwrap();

    let b_error = b
        .execute("UPDATE app_settings SET json_value='from-b' WHERE id=1", [])
        .unwrap_err();
    assert!(
        matches!(
            b_error,
            rusqlite::Error::SqliteFailure(failure, _)
                if failure.code == rusqlite::ErrorCode::DatabaseBusy
        ),
        "A 持有写权时 B 的写入应明确 BUSY，实际：{b_error}"
    );
    tx.commit().unwrap();

    let committed: String = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT json_value FROM app_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(committed, "from-a");
}

#[test]
fn concurrent_read_write_paths_do_not_error() {
    // 真实读-改-写路径（save_settings_and_replan / upsert_event）并发写：
    // 全部改为 IMMEDIATE 后 BEGIN 会等待而非升级失败，因此并发下不应出错。
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let settings = AppSettings::default();
    let mut renamed = event.clone();

    let settings_database = test.database.clone();
    let rename_database = test.database.clone();
    let settings_handle = std::thread::spawn(move || {
        for _ in 0..25 {
            settings_database
                .save_settings_and_replan(&settings)
                .unwrap();
        }
    });
    let rename_handle = std::thread::spawn(move || {
        for index in 0..25 {
            renamed.name = format!("并发重命名{index}");
            renamed.updated_at = now_china();
            rename_database.upsert_event(renamed.clone()).unwrap();
        }
    });
    settings_handle.join().unwrap();
    rename_handle.join().unwrap();

    test.database
        .claim_due(20)
        .unwrap()
        .first()
        .cloned()
        .map(|delivery| test.database.fail_delivery(delivery.outbox_id, "并发验证"))
        .transpose()
        .unwrap();
}

#[test]
fn settings_and_replanning_roll_back_together_on_sql_failure() {
    let test = TestDatabase::new();
    let original = AppSettings::default();
    test.database.save_settings(&original).unwrap();
    test.database.upsert_event(test.event()).unwrap();
    test.database
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_reminder_insert BEFORE INSERT ON reminder_outbox
                 BEGIN SELECT RAISE(ABORT,'fixture replan failure'); END;
                 CREATE TRIGGER fail_reminder_update BEFORE UPDATE ON reminder_outbox
                 BEGIN SELECT RAISE(ABORT,'fixture replan failure'); END;",
        )
        .unwrap();
    let mut changed = original.clone();
    changed.safety_cutoff = NaiveTime::from_hms_opt(14, 54, 0).unwrap();
    assert!(test.database.save_settings_and_replan(&changed).is_err());
    assert_eq!(
        test.database.settings().unwrap().safety_cutoff,
        original.safety_cutoff
    );
}

#[test]
fn malformed_settings_json_is_reported_and_does_not_partially_upsert_events() {
    let test = TestDatabase::new();
    test.database
        .open()
        .unwrap()
        .execute(
            "INSERT INTO app_settings(id,json_value,updated_at) VALUES(1,'{broken',?1)",
            [format_dt(now_china())],
        )
        .unwrap();
    assert!(test.database.settings().is_err());
    assert!(test.database.upsert_event(test.event()).is_err());
    assert!(test.database.event("shanghai:601001").unwrap().is_none());
}

#[test]
fn sync_schedule_v3_migrates_legacy_defaults() {
    let test = TestDatabase::new();
    let legacy = AppSettings {
        normal_sync_minutes: 1440,
        active_day_sync_minutes: 1440,
        ..AppSettings::default()
    };
    test.database.save_settings(&legacy).unwrap();
    test.database
        .open()
        .unwrap()
        .execute("DELETE FROM schema_migrations WHERE version=3", [])
        .unwrap();

    test.database.initialize().unwrap();

    let migrated = test.database.settings().unwrap();
    assert_eq!(migrated.normal_sync_minutes, 30);
    assert_eq!(migrated.active_day_sync_minutes, 10);
    let applied: i32 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=3)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(applied, 1);
}

#[test]
fn raw_payload_migration_discards_bodies_and_future_runs_keep_metadata_only() {
    let test = TestDatabase::new();
    let connection = test.database.open().unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version=9", [])
        .unwrap();
    connection
            .execute(
                "INSERT INTO raw_payloads(source,fetched_at,success,record_count,raw_hash,schema_fingerprint,payload,error)
                 VALUES('fixture',?1,1,1,'old-hash','fixture-schema','large-response',NULL)",
                [format_dt(now_china())],
            )
            .unwrap();
    drop(connection);

    test.database.initialize().unwrap();
    test.database
        .save_source_run(
            "fixture",
            now_china(),
            HealthState::Healthy,
            2,
            Some("another-large-response"),
            Some("new-hash"),
            Some("fixture-schema"),
            None,
        )
        .unwrap();

    let connection = test.database.open().unwrap();
    let (rows, bodies, hashes): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*),COUNT(payload),COUNT(raw_hash) FROM raw_payloads WHERE source='fixture'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
    assert_eq!((rows, bodies, hashes), (2, 0, 2));
    assert_eq!(
        test.database.schema_version().unwrap(),
        LATEST_SCHEMA_VERSION
    );
}

#[test]
fn quiet_reminder_migration_cancels_legacy_interruptions() {
    let test = TestDatabase::new();
    let now = now_china();
    let mut event = test.event();
    event.apply_date = Some(now.date_naive() + chrono::Duration::days(2));
    event.lifecycle_status = LifecycleStatus::Scheduled;
    let event = test.database.upsert_event(event).unwrap();
    let connection = test.database.open().unwrap();
    connection
        .execute("DELETE FROM schema_migrations WHERE version=11", [])
        .unwrap();
    for (key, level) in [
        ("legacy-advance", ReminderLevel::Advance),
        ("legacy-morning", ReminderLevel::Morning),
        ("legacy-change", ReminderLevel::DataChanged),
    ] {
        connection
            .execute(
                "INSERT INTO reminder_outbox(
                    ipo_event_id,event_version,due_at,reminder_level,dedupe_key,
                    delivery_state,created_at,updated_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
                params![
                    event.id,
                    event.event_version,
                    format_dt(now),
                    level as i32,
                    key,
                    DeliveryState::Pending as i32,
                    format_dt(now),
                ],
            )
            .unwrap();
    }
    drop(connection);

    test.database.initialize().unwrap();
    let cancelled: i64 = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM reminder_outbox
             WHERE dedupe_key LIKE 'legacy-%' AND delivery_state=?1",
            [DeliveryState::Cancelled as i32],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cancelled, 3);
    assert_eq!(test.database.schema_version().unwrap(), 11);
}

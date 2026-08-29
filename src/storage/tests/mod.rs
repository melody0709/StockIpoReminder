use super::*;
use uuid::Uuid;

struct TestDatabase {
    root: PathBuf,
    database: Database,
}
impl TestDatabase {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("stock-ipo-rust-test-{}", Uuid::new_v4().simple()));
        let database = Database::new(&root);
        database.initialize().unwrap();
        Self { root, database }
    }
    fn new_at_v9() -> Self {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-rust-v9-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = Database::new(&root);
        let connection = database.open().unwrap();
        connection.execute_batch(MIGRATION_SQL).unwrap();
        migrate_sync_schedule_v3(&connection).unwrap();
        migrate_sync_conclusions_v4(&connection).unwrap();
        migrate_operation_health_v5(&connection).unwrap();
        migrate_source_probes_v6(&connection).unwrap();
        migrate_outbox_messages_v7(&connection).unwrap();
        migrate_secondary_notifications_v8(&connection).unwrap();
        migrate_raw_payload_metadata_v9(&connection).unwrap();
        drop(connection);
        Self { root, database }
    }
    fn event(&self) -> IpoEvent {
        let now = now_china();
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
            lifecycle_status: LifecycleStatus::ActiveUnconfirmed,
            event_version: 1,
            announcement_url: None,
            data_quality_status: DataQualityStatus::SingleSource,
            data_conflict: false,
            manual_override_fields: Vec::new(),
            sessions: Vec::new(),
            first_seen_at: now,
            updated_at: now,
        }
    }
}
impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

mod acknowledgements_overrides;
mod events;
mod health_sync;
mod maintenance_backup;
mod reminders;
mod schema_concurrency;
mod secondary;

fn pending_reminder_rows_excluding_data_changed(database: &Database) -> i64 {
    database
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM reminder_outbox WHERE delivery_state=0 AND reminder_level<>90",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
}

fn make_file_old(path: &Path) {
    let old = std::time::SystemTime::now() - StdDuration::from_secs(48 * 3600);
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
}

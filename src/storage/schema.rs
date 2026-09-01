use super::*;

impl Database {
    pub fn initialize(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = self.open()?;
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
        connection.execute_batch(MIGRATION_SQL)?;
        migrate_sync_schedule_v3(&connection)?;
        migrate_sync_conclusions_v4(&connection)?;
        migrate_operation_health_v5(&connection)?;
        migrate_source_probes_v6(&connection)?;
        migrate_outbox_messages_v7(&connection)?;
        migrate_secondary_notifications_v8(&connection)?;
        migrate_raw_payload_metadata_v9(&connection)?;
        migrate_secondary_notification_identity_v10(&connection)?;
        migrate_quiet_reminders_v11(&connection)?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.open()?.query_row(
            "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn integrity_check(&self) -> Result<()> {
        let result: String = self
            .open()?
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if result != "ok" {
            bail!("SQLite integrity_check={result}")
        } else {
            Ok(())
        }
    }
}

pub(super) fn migrate_sync_schedule_v3(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=3)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let json: Option<String> = connection
        .query_row(
            "SELECT json_value FROM app_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json) = json {
        if let Ok(mut settings) = serde_json::from_str::<AppSettings>(&json) {
            let original_normal = settings.normal_sync_minutes;
            let original_active = settings.active_day_sync_minutes;
            if original_normal == 1440 && original_active == 1440 {
                settings.normal_sync_minutes = 30;
                settings.active_day_sync_minutes = 10;
            } else if original_active == original_normal {
                settings.active_day_sync_minutes = original_normal.clamp(5, 10);
            }
            settings.normal_sync_minutes = settings.normal_sync_minutes.clamp(5, 7 * 24 * 60);
            settings.active_day_sync_minutes = settings
                .active_day_sync_minutes
                .clamp(5, settings.normal_sync_minutes);
            connection.execute(
                "UPDATE app_settings SET json_value=?1,updated_at=?2 WHERE id=1",
                params![serde_json::to_string(&settings)?, format_dt(now_china())],
            )?;
        }
    }
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(3,?1)",
        [format_dt(now_china())],
    )?;
    Ok(())
}

pub(super) fn migrate_sync_conclusions_v4(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=4)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_conclusions(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at TEXT NOT NULL,
            finished_at TEXT NOT NULL,
            conclusion_kind INTEGER NOT NULL,
            today_count INTEGER NOT NULL,
            event_count INTEGER NOT NULL,
            announcement_count INTEGER NOT NULL,
            successful_sources_json TEXT NOT NULL,
            missing_sources_json TEXT NOT NULL,
            summary TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_sync_conclusions_finished_at
            ON sync_conclusions(finished_at DESC);",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(4,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_operation_health_v5(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=5)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS operation_health(
            component TEXT PRIMARY KEY,
            last_attempt_at TEXT NOT NULL,
            last_success_at TEXT NULL,
            health_state INTEGER NOT NULL,
            last_error TEXT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_operation_health_state
            ON operation_health(health_state,last_attempt_at DESC);",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(5,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_source_probes_v6(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=6)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "ALTER TABLE source_backoff ADD COLUMN next_probe_at TEXT NULL;
         ALTER TABLE source_backoff ADD COLUMN last_probe_at TEXT NULL;
         ALTER TABLE source_backoff ADD COLUMN last_probe_success INTEGER NULL;
         ALTER TABLE source_backoff ADD COLUMN last_probe_error TEXT NULL;",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(6,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_outbox_messages_v7(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=7)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch("ALTER TABLE reminder_outbox ADD COLUMN message TEXT NULL;")?;
    transaction.execute(
        "UPDATE reminder_outbox SET delivery_state=?1,lease_until=NULL,updated_at=?2 WHERE delivery_state IN (?3,?4,?5) AND NOT EXISTS(SELECT 1 FROM ipo_events e WHERE e.id=reminder_outbox.ipo_event_id AND e.event_version=reminder_outbox.event_version)",
        params![
            DeliveryState::Cancelled as i32,
            format_dt(now_china()),
            DeliveryState::Pending as i32,
            DeliveryState::Leased as i32,
            DeliveryState::Failed as i32,
        ],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(7,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_secondary_notifications_v8(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=8)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS secondary_notification_outbox(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reminder_outbox_id INTEGER NOT NULL UNIQUE REFERENCES reminder_outbox(id) ON DELETE CASCADE,
            provider INTEGER NOT NULL,
            state INTEGER NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT NOT NULL,
            lease_until TEXT NULL,
            last_error TEXT NULL,
            delivered_at TEXT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_secondary_notification_due ON secondary_notification_outbox(state,next_attempt_at,provider);
        CREATE TABLE IF NOT EXISTS secondary_notification_attempts(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            attempted_at TEXT NOT NULL,
            provider INTEGER NOT NULL,
            success INTEGER NOT NULL,
            batch_size INTEGER NOT NULL,
            error TEXT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_secondary_notification_attempts_time ON secondary_notification_attempts(attempted_at DESC);",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(8,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_raw_payload_metadata_v9(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=9)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE raw_payloads SET payload=NULL WHERE payload IS NOT NULL",
        [],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(9,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_secondary_notification_identity_v10(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=10)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let now = now_china();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE secondary_notification_outbox_v10(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            reminder_outbox_id INTEGER NOT NULL REFERENCES reminder_outbox(id) ON DELETE CASCADE,
            provider INTEGER NOT NULL,
            state INTEGER NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at TEXT NOT NULL,
            lease_until TEXT NULL,
            last_error TEXT NULL,
            delivered_at TEXT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            UNIQUE(reminder_outbox_id,provider)
        );
        INSERT INTO secondary_notification_outbox_v10(
            id,reminder_outbox_id,provider,state,attempt_count,next_attempt_at,lease_until,last_error,
            delivered_at,created_at,updated_at
        )
        SELECT id,reminder_outbox_id,provider,state,attempt_count,next_attempt_at,lease_until,last_error,
               delivered_at,created_at,updated_at
        FROM secondary_notification_outbox;
        DROP TABLE secondary_notification_outbox;
        ALTER TABLE secondary_notification_outbox_v10 RENAME TO secondary_notification_outbox;
        CREATE INDEX ix_secondary_notification_due
            ON secondary_notification_outbox(state,next_attempt_at,provider);",
    )?;

    let settings = settings_from_connection(&transaction)?;
    if settings.secondary_notification_enabled
        && !matches!(
            settings.secondary_notification_provider,
            SecondaryNotificationProvider::Disabled | SecondaryNotificationProvider::Unknown
        )
    {
        transaction.execute(
            "INSERT OR IGNORE INTO secondary_notification_outbox(
                reminder_outbox_id,provider,state,attempt_count,next_attempt_at,created_at,updated_at
             )
             SELECT s.reminder_outbox_id,?1,?2,0,?3,?3,?3
             FROM secondary_notification_outbox s
             JOIN reminder_outbox r ON r.id=s.reminder_outbox_id
             WHERE s.provider<>?1 AND s.state=?4
               AND r.delivery_state NOT IN (?5,?6)
               AND r.due_at>=?7
               AND NOT EXISTS(
                   SELECT 1 FROM secondary_notification_outbox delivered
                   WHERE delivered.reminder_outbox_id=s.reminder_outbox_id
                     AND delivered.state=?8
               )",
            params![
                settings.secondary_notification_provider as i32,
                SECONDARY_PENDING,
                format_dt(now),
                SECONDARY_CANCELLED,
                DeliveryState::Cancelled as i32,
                DeliveryState::Collapsed as i32,
                format_dt(now - chrono::Duration::days(1)),
                SECONDARY_DELIVERED,
            ],
        )?;
    }
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(10,?1)",
        [format_dt(now)],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_quiet_reminders_v11(connection: &Connection) -> Result<()> {
    let applied = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=11)",
        [],
        |row| row.get::<_, i32>(0),
    )? != 0;
    if applied {
        return Ok(());
    }

    let now = now_china();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE reminder_outbox
         SET delivery_state=?1,lease_until=NULL,updated_at=?2
         WHERE delivery_state IN (?3,?4,?5)
           AND (
               reminder_level IN (?6,?7,?8,?9,?10)
               OR (
                   reminder_level=?11
                   AND EXISTS(
                       SELECT 1 FROM ipo_events e
                       WHERE e.id=reminder_outbox.ipo_event_id
                         AND e.apply_date>?12
                   )
               )
           )",
        params![
            DeliveryState::Cancelled as i32,
            format_dt(now),
            DeliveryState::Pending as i32,
            DeliveryState::Leased as i32,
            DeliveryState::Failed as i32,
            ReminderLevel::Advance as i32,
            ReminderLevel::Morning as i32,
            ReminderLevel::BrokerOpening as i32,
            ReminderLevel::MarketOpening as i32,
            ReminderLevel::AfternoonOpening as i32,
            ReminderLevel::DataChanged as i32,
            format_date(now.date_naive()),
        ],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(11,?1)",
        [format_dt(now)],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) const MIGRATION_SQL: &str = r#"CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY,applied_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS ipo_events(id TEXT PRIMARY KEY,exchange INTEGER NOT NULL,board INTEGER NOT NULL,security_code TEXT NOT NULL,apply_code TEXT NULL,legacy_code TEXT NULL,name TEXT NOT NULL,apply_date TEXT NULL,issue_price NUMERIC NULL,lot_size INTEGER NULL,max_apply_quantity INTEGER NULL,required_market_value NUMERIC NULL,required_cash NUMERIC NULL,ballot_date TEXT NULL,payment_date TEXT NULL,listing_date TEXT NULL,issue_status INTEGER NOT NULL,lifecycle_status INTEGER NOT NULL,event_version INTEGER NOT NULL,announcement_url TEXT NULL,data_quality_status INTEGER NOT NULL,data_conflict INTEGER NOT NULL DEFAULT 0,sessions_json TEXT NOT NULL DEFAULT '[]',first_seen_at TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE INDEX IF NOT EXISTS ix_ipo_events_apply_date ON ipo_events(apply_date);CREATE UNIQUE INDEX IF NOT EXISTS ux_ipo_events_exchange_security ON ipo_events(exchange,security_code);CREATE TABLE IF NOT EXISTS ipo_field_sources(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,field_name TEXT NOT NULL,normalized_value TEXT NULL,raw_value TEXT NULL,source TEXT NOT NULL,source_published_at TEXT NULL,fetched_at TEXT NOT NULL,raw_hash TEXT NULL,priority INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS ix_field_sources_event ON ipo_field_sources(ipo_event_id,field_name);CREATE TABLE IF NOT EXISTS acknowledgements(ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,confirmed_at TEXT NOT NULL,confirmed_data_hash TEXT NOT NULL,needs_review_at TEXT NULL,review_reason TEXT NULL,reconfirmed_at TEXT NULL,revoked_at TEXT NULL,PRIMARY KEY(ipo_event_id,event_version));CREATE TABLE IF NOT EXISTS reminder_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,due_at TEXT NOT NULL,reminder_level INTEGER NOT NULL,dedupe_key TEXT NOT NULL UNIQUE,lease_until TEXT NULL,delivery_state INTEGER NOT NULL,attempt_count INTEGER NOT NULL DEFAULT 0,last_error TEXT NULL,delivered_at TEXT NULL,acknowledged_at TEXT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE INDEX IF NOT EXISTS ix_outbox_due ON reminder_outbox(delivery_state,due_at);CREATE TABLE IF NOT EXISTS reminder_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL,scheduled_at TEXT NOT NULL,shown_at TEXT NOT NULL,reminder_level INTEGER NOT NULL,delivery_channel TEXT NOT NULL,dedupe_key TEXT NOT NULL,result TEXT NOT NULL);CREATE TABLE IF NOT EXISTS raw_payloads(id INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,fetched_at TEXT NOT NULL,success INTEGER NOT NULL,record_count INTEGER NOT NULL,raw_hash TEXT NULL,schema_fingerprint TEXT NULL,payload TEXT NULL,error TEXT NULL);CREATE INDEX IF NOT EXISTS ix_raw_payloads_source_time ON raw_payloads(source,fetched_at DESC);CREATE TABLE IF NOT EXISTS sync_runs(id INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,started_at TEXT NOT NULL,finished_at TEXT NOT NULL,success INTEGER NOT NULL,record_count INTEGER NOT NULL,error TEXT NULL);CREATE TABLE IF NOT EXISTS source_health(source TEXT PRIMARY KEY,last_attempt_at TEXT NULL,last_success_at TEXT NULL,last_record_count INTEGER NOT NULL DEFAULT 0,schema_fingerprint TEXT NULL,consecutive_failures INTEGER NOT NULL DEFAULT 0,health_state INTEGER NOT NULL,last_error TEXT NULL);CREATE TABLE IF NOT EXISTS source_backoff(source TEXT PRIMARY KEY,failure_count INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT NULL,last_failure_at TEXT NULL,last_success_at TEXT NULL,last_error TEXT NULL);CREATE TABLE IF NOT EXISTS announcement_documents(id TEXT PRIMARY KEY,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,provider TEXT NOT NULL,announcement_id TEXT NOT NULL,announcement_type TEXT NULL,title TEXT NOT NULL,published_at TEXT NULL,source_url TEXT NOT NULL,local_path TEXT NOT NULL,file_hash TEXT NOT NULL,extraction_status INTEGER NOT NULL,extracted_text_hash TEXT NULL,parser_version TEXT NOT NULL,parsed_fields_json TEXT NOT NULL,downloaded_at TEXT NOT NULL);CREATE UNIQUE INDEX IF NOT EXISTS ux_announcements_provider_id_hash ON announcement_documents(provider,announcement_id,file_hash);CREATE TABLE IF NOT EXISTS manual_overrides(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,field_name TEXT NOT NULL,override_value TEXT NOT NULL,reason TEXT NOT NULL,announcement_document_id TEXT NULL,created_at TEXT NOT NULL,revoked_at TEXT NULL);CREATE TABLE IF NOT EXISTS app_settings(id INTEGER PRIMARY KEY CHECK(id=1),json_value TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS app_heartbeat(component TEXT PRIMARY KEY,heartbeat_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS health_summary_log(summary_date TEXT PRIMARY KEY,sent_at TEXT NOT NULL);INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(1,CURRENT_TIMESTAMP);INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(2,CURRENT_TIMESTAMP);"#;

use super::*;

impl Database {
    pub fn settings(&self) -> Result<AppSettings> {
        settings_from_connection(&self.open()?)
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        let now = now_china();
        let mut connection = self.open()?;
        // 事务内先读旧设置再写入：DEFERRED 在 WAL 下读后升级写会因并发提交
        // 立即返回 BUSY（读快照失效），必须用 IMMEDIATE 在 BEGIN 时取得写权。
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = settings_from_connection(&transaction)?;
        save_settings_tx(&transaction, settings, &previous, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn save_settings_and_replan(&self, settings: &AppSettings) -> Result<()> {
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = settings_from_connection(&transaction)?;
        save_settings_tx(&transaction, settings, &previous, now)?;
        let from = format_date(now.date_naive() - chrono::Duration::days(60));
        let to = format_date(now.date_naive() + chrono::Duration::days(60));
        let mut events = {
            let mut statement = transaction.prepare(
                "SELECT * FROM ipo_events WHERE apply_date>=?1 AND apply_date<=?2 ORDER BY apply_date,id",
            )?;
            statement
                .query_map(params![from, to], map_event)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for event in &mut events {
            apply_manual_overrides(&transaction, event)?;
            reconcile_schedule_tx(&transaction, event, settings, now)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

pub(super) fn save_settings_tx(
    transaction: &rusqlite::Transaction<'_>,
    settings: &AppSettings,
    previous: &AppSettings,
    now: ChinaDateTime,
) -> Result<()> {
    let json = serde_json::to_string(settings)?;
    transaction.execute(
        "INSERT INTO app_settings(id,json_value,updated_at) VALUES(1,?1,?2)
         ON CONFLICT(id) DO UPDATE SET json_value=excluded.json_value,updated_at=excluded.updated_at",
        params![json, format_dt(now)],
    )?;

    let new_provider = settings.secondary_notification_provider;
    let new_enabled = settings.secondary_notification_enabled
        && !matches!(
            new_provider,
            SecondaryNotificationProvider::Disabled | SecondaryNotificationProvider::Unknown
        );
    let provider_changed = new_provider != previous.secondary_notification_provider;
    let channel_activated =
        new_enabled && (!previous.secondary_notification_enabled || provider_changed);
    if channel_activated {
        transaction.execute(
            "INSERT INTO secondary_notification_outbox(
                reminder_outbox_id,provider,state,attempt_count,next_attempt_at,lease_until,last_error,
                delivered_at,created_at,updated_at
             )
             SELECT DISTINCT s.reminder_outbox_id,?1,?2,0,?3,NULL,NULL,NULL,?3,?3
             FROM secondary_notification_outbox s
             JOIN reminder_outbox r ON r.id=s.reminder_outbox_id
             WHERE s.state IN (?4,?5,?6,?7,?8)
               AND r.delivery_state NOT IN (?9,?10)
               AND r.due_at>=?11
               AND NOT EXISTS(
                   SELECT 1 FROM secondary_notification_outbox delivered
                   WHERE delivered.reminder_outbox_id=s.reminder_outbox_id
                     AND delivered.state=?12
               )
             ON CONFLICT(reminder_outbox_id,provider) DO UPDATE SET
                state=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                           THEN excluded.state ELSE secondary_notification_outbox.state END,
                attempt_count=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                   THEN 0 ELSE secondary_notification_outbox.attempt_count END,
                next_attempt_at=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                     THEN excluded.next_attempt_at ELSE secondary_notification_outbox.next_attempt_at END,
                lease_until=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                 THEN NULL ELSE secondary_notification_outbox.lease_until END,
                last_error=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                THEN NULL ELSE secondary_notification_outbox.last_error END,
                delivered_at=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                  THEN NULL ELSE secondary_notification_outbox.delivered_at END,
                updated_at=CASE WHEN secondary_notification_outbox.state IN (?13,?14)
                                THEN excluded.updated_at ELSE secondary_notification_outbox.updated_at END",
            params![
                new_provider as i32,
                SECONDARY_PENDING,
                format_dt(now),
                SECONDARY_PENDING,
                SECONDARY_LEASED,
                SECONDARY_RETRYING,
                SECONDARY_CANCELLED,
                SECONDARY_EXHAUSTED,
                DeliveryState::Cancelled as i32,
                DeliveryState::Collapsed as i32,
                format_dt(now - chrono::Duration::days(1)),
                SECONDARY_DELIVERED,
                SECONDARY_CANCELLED,
                SECONDARY_EXHAUSTED,
            ],
        )?;
    }

    if !new_enabled {
        transaction.execute(
            "UPDATE secondary_notification_outbox
             SET state=?1,lease_until=NULL,updated_at=?2
             WHERE state IN (?3,?4,?5)",
            params![
                SECONDARY_CANCELLED,
                format_dt(now),
                SECONDARY_PENDING,
                SECONDARY_LEASED,
                SECONDARY_RETRYING,
            ],
        )?;
    } else if provider_changed {
        transaction.execute(
            "UPDATE secondary_notification_outbox
             SET state=?1,lease_until=NULL,updated_at=?2
             WHERE provider<>?3 AND state IN (?4,?5,?6)",
            params![
                SECONDARY_CANCELLED,
                format_dt(now),
                new_provider as i32,
                SECONDARY_PENDING,
                SECONDARY_LEASED,
                SECONDARY_RETRYING,
            ],
        )?;
    }
    Ok(())
}

pub(super) fn settings_from_connection(connection: &Connection) -> Result<AppSettings> {
    let json: Option<String> = connection
        .query_row(
            "SELECT json_value FROM app_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match json {
        Some(value) => serde_json::from_str(&value).context("应用设置 JSON 已损坏"),
        None => Ok(AppSettings::default()),
    }
}

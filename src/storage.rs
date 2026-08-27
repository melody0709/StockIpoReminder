use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{
    core::{
        critical_change_reason, event_hash, noncritical_change_reason, now_china, plan_reminders,
        sha256,
    },
    model::*,
};

const SECONDARY_PENDING: i32 = 0;
const SECONDARY_LEASED: i32 = 1;
const SECONDARY_DELIVERED: i32 = 2;
const SECONDARY_RETRYING: i32 = 3;
const SECONDARY_EXHAUSTED: i32 = 4;
const SECONDARY_CANCELLED: i32 = 5;
const SECONDARY_MAX_ATTEMPTS: i32 = 5;
const SECONDARY_REQUESTS_PER_HOUR: i64 = 20;
const SECONDARY_ATTEMPT_RETENTION_DAYS: i64 = 30;
const SECONDARY_OUTBOX_RETENTION_DAYS: i64 = 90;
const SECONDARY_MAX_ATTEMPT_RECORDS: i64 = 2000;
const LOCAL_DELIVERY_PERSISTENT_FAILURE_MINUTES: i64 = 15;
const BACKUP_PAGES_PER_STEP: i32 = 1024;
const BACKUP_STEP_PAUSE: StdDuration = StdDuration::from_millis(1);
pub const LATEST_SCHEMA_VERSION: i64 = 9;

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    pub fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("stock-ipo-reminder.db"),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn open(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(StdDuration::from_secs(10))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        Ok(connection)
    }

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

    pub fn settings(&self) -> Result<AppSettings> {
        let connection = self.open()?;
        let json: Option<String> = connection
            .query_row(
                "SELECT json_value FROM app_settings WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default())
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        let json = serde_json::to_string(settings)?;
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let previous: AppSettings = transaction
            .query_row(
                "SELECT json_value FROM app_settings WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        transaction.execute("INSERT INTO app_settings(id,json_value,updated_at) VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET json_value=excluded.json_value,updated_at=excluded.updated_at", params![json, format_dt(now)])?;
        if !settings.secondary_notification_enabled
            || settings.secondary_notification_provider != previous.secondary_notification_provider
        {
            transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,lease_until=NULL,updated_at=?2 WHERE state IN (?3,?4,?5)",
                params![
                    SECONDARY_CANCELLED,
                    format_dt(now),
                    SECONDARY_PENDING,
                    SECONDARY_LEASED,
                    SECONDARY_RETRYING,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn event(&self, id: &str) -> Result<Option<IpoEvent>> {
        let connection = self.open()?;
        let mut event = connection
            .query_row("SELECT * FROM ipo_events WHERE id=?1", [id], map_event)
            .optional()?;
        if let Some(value) = &mut event {
            apply_manual_overrides(&connection, value)?;
        }
        Ok(event)
    }

    pub fn events(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<IpoEvent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT * FROM ipo_events WHERE apply_date BETWEEN ?1 AND ?2 ORDER BY apply_date,exchange,security_code")?;
        let rows = statement.query_map(params![format_date(from), format_date(to)], map_event)?;
        let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for event in &mut events {
            apply_manual_overrides(&connection, event)?;
        }
        Ok(events)
    }

    pub fn today_events(&self) -> Result<Vec<IpoEvent>> {
        let today = now_china().date_naive();
        self.events(today, today)
    }

    pub fn future_events(&self, days: i64) -> Result<Vec<IpoEvent>> {
        let today = now_china().date_naive();
        self.events(
            today + chrono::Duration::days(1),
            today + chrono::Duration::days(days),
        )
    }

    pub fn upsert_event(&self, mut event: IpoEvent) -> Result<IpoEvent> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let mut change_notification = None::<(bool, String, ChinaDateTime)>;
        let existing = transaction
            .query_row(
                "SELECT * FROM ipo_events WHERE id=?1",
                [&event.id],
                map_event,
            )
            .optional()?;
        if let Some(previous) = &existing {
            retain_known_optional_fields(previous, &mut event);
            let critical_reason = critical_change_reason(previous, &event);
            let noncritical_reason = noncritical_change_reason(previous, &event);
            let critical_changed = critical_reason.is_some();
            event.event_version = previous.event_version + i32::from(critical_changed);
            event.first_seen_at = previous.first_seen_at;
            change_notification = critical_reason
                .as_ref()
                .map(|reason| (true, reason.clone(), previous.updated_at))
                .or_else(|| noncritical_reason.map(|reason| (false, reason, previous.updated_at)));
            if critical_changed && previous.lifecycle_status == LifecycleStatus::Acknowledged {
                event.lifecycle_status = LifecycleStatus::AcknowledgedNeedsReview;
                transaction.execute("UPDATE acknowledgements SET needs_review_at=?1,review_reason=?2 WHERE ipo_event_id=?3 AND event_version=?4 AND revoked_at IS NULL", params![format_dt(event.updated_at), critical_reason.as_deref(), event.id, previous.event_version])?;
            } else if previous.lifecycle_status == LifecycleStatus::Acknowledged {
                event.lifecycle_status = LifecycleStatus::Acknowledged;
            }
        }
        let sessions = serde_json::to_string(&event.sessions)?;
        transaction.execute(
            UPSERT_EVENT_SQL,
            params![
                event.id,
                event.exchange as i32,
                event.board as i32,
                event.security_code,
                event.apply_code,
                event.legacy_code,
                event.name,
                event.apply_date.map(format_date),
                event.issue_price,
                event.lot_size,
                event.max_apply_quantity,
                event.required_market_value,
                event.required_cash,
                event.ballot_date.map(format_date),
                event.payment_date.map(format_date),
                event.listing_date.map(format_date),
                event.status as i32,
                event.lifecycle_status as i32,
                event.event_version,
                event.announcement_url,
                event.data_quality_status as i32,
                i32::from(event.data_conflict),
                sessions,
                format_dt(event.first_seen_at),
                format_dt(event.updated_at)
            ],
        )?;
        let settings: AppSettings = transaction
            .query_row("SELECT json_value FROM app_settings WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        transaction.execute(
            "UPDATE reminder_outbox SET delivery_state=?1,lease_until=NULL,updated_at=?2 WHERE ipo_event_id=?3 AND event_version<>?4 AND delivery_state IN (?5,?6,?7)",
            params![
                DeliveryState::Cancelled as i32,
                format_dt(event.updated_at),
                event.id,
                event.event_version,
                DeliveryState::Pending as i32,
                DeliveryState::Leased as i32,
                DeliveryState::Failed as i32,
            ],
        )?;
        reconcile_schedule_tx(&transaction, &event, &settings, now_china())?;
        if let Some((critical, reason, previous_updated_at)) = change_notification {
            enqueue_change_notification_tx(
                &transaction,
                &event,
                critical,
                &reason,
                previous_updated_at,
            )?;
        }
        transaction.commit()?;
        Ok(event)
    }

    pub fn replan_all(&self) -> Result<()> {
        let settings = self.settings()?;
        let now = now_china();
        for event in self.events(
            now.date_naive() - chrono::Duration::days(60),
            now.date_naive() + chrono::Duration::days(60),
        )? {
            self.reconcile_schedule(&event, &settings, now)?;
        }
        Ok(())
    }
    pub fn reconcile_schedule(
        &self,
        event: &IpoEvent,
        settings: &AppSettings,
        now: ChinaDateTime,
    ) -> Result<()> {
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        reconcile_schedule_tx(&tx, event, settings, now)?;
        tx.commit()?;
        Ok(())
    }

    pub fn acknowledge(&self, event_id: &str, version: i32) -> Result<()> {
        self.acknowledge_at(event_id, version, now_china())
    }

    fn acknowledge_at(&self, event_id: &str, version: i32, now: ChinaDateTime) -> Result<()> {
        let mut event = self.event(event_id)?.context("申购任务不存在")?;
        if event.event_version != version {
            bail!("申购数据已更新，请刷新后确认")
        }
        let apply_date = event
            .apply_date
            .context("任务缺少申购日期，不能确认已申购")?;
        if apply_date != now.date_naive() {
            bail!("只能在申购日当天确认已申购")
        }
        if !matches!(
            event.lifecycle_status,
            LifecycleStatus::Scheduled
                | LifecycleStatus::ActiveUnconfirmed
                | LifecycleStatus::Acknowledged
                | LifecycleStatus::AcknowledgedNeedsReview
        ) {
            bail!("当前任务状态不能确认已申购")
        }
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO acknowledgements(ipo_event_id,event_version,confirmed_at,confirmed_data_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(ipo_event_id,event_version) DO UPDATE SET confirmed_at=excluded.confirmed_at,confirmed_data_hash=excluded.confirmed_data_hash,reconfirmed_at=excluded.confirmed_at,revoked_at=NULL,needs_review_at=NULL,review_reason=NULL",params![event_id,version,format_dt(now),event_hash(&event)])?;
        tx.execute("UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4",params![LifecycleStatus::Acknowledged as i32,format_dt(now),event_id,version])?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=?1,acknowledged_at=?2,updated_at=?2 WHERE ipo_event_id=?3 AND event_version=?4 AND delivery_state IN (0,1,5)",params![DeliveryState::Cancelled as i32,format_dt(now),event_id,version])?;
        event.lifecycle_status = LifecycleStatus::Acknowledged;
        event.updated_at = now;
        let settings: AppSettings = tx
            .query_row(
                "SELECT json_value FROM app_settings WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        reconcile_schedule_tx(&tx, &event, &settings, now)?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_acknowledgement(&self, event_id: &str, version: i32) -> Result<()> {
        self.revoke_acknowledgement_at(event_id, version, now_china())
    }

    fn revoke_acknowledgement_at(
        &self,
        event_id: &str,
        version: i32,
        now: ChinaDateTime,
    ) -> Result<()> {
        let mut event = self.event(event_id)?.context("申购任务不存在")?;
        if event.event_version != version || event.lifecycle_status != LifecycleStatus::Acknowledged
        {
            bail!("当前没有可撤销的有效确认");
        }
        let settings = self.settings()?;
        let date = event.apply_date.context("任务缺少申购日期")?;
        if now >= crate::core::at(date, crate::core::effective_cutoff(&event, &settings)) {
            bail!("已超过安全截止时间，不能撤销确认");
        }

        let restored_status = if date > now.date_naive() {
            LifecycleStatus::Scheduled
        } else {
            LifecycleStatus::ActiveUnconfirmed
        };
        event.lifecycle_status = restored_status;
        event.updated_at = now;
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let acknowledgement_changed = transaction.execute(
            "UPDATE acknowledgements SET revoked_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND revoked_at IS NULL",
            params![format_dt(now), event_id, version],
        )?;
        if acknowledgement_changed != 1 {
            bail!("当前没有可撤销的有效确认");
        }
        let event_changed = transaction.execute(
            "UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4 AND lifecycle_status=?5",
            params![
                restored_status as i32,
                format_dt(now),
                event_id,
                version,
                LifecycleStatus::Acknowledged as i32,
            ],
        )?;
        if event_changed != 1 {
            bail!("申购任务状态已变化，请刷新后重试");
        }
        reconcile_schedule_tx(&transaction, &event, &settings, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn refresh_lifecycle(&self) -> Result<bool> {
        let now = now_china();
        let settings = self.settings()?;
        let mut changed = false;
        for event in self.future_events(60)? {
            if event.lifecycle_status == LifecycleStatus::Acknowledged {
                self.revoke_acknowledgement_at(&event.id, event.event_version, now)?;
                changed = true;
            }
        }
        for mut event in self.today_events()? {
            let next = if event.lifecycle_status == LifecycleStatus::Scheduled {
                Some(LifecycleStatus::ActiveUnconfirmed)
            } else if matches!(
                event.lifecycle_status,
                LifecycleStatus::ActiveUnconfirmed | LifecycleStatus::AcknowledgedNeedsReview
            ) && now
                >= crate::core::at(
                    now.date_naive(),
                    crate::core::effective_cutoff(&event, &settings),
                )
            {
                Some(LifecycleStatus::ExpiredUnconfirmed)
            } else {
                None
            };
            if let Some(status) = next {
                event.lifecycle_status = status;
                event.updated_at = now;
                self.open()?.execute("UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4",params![status as i32,format_dt(now),event.id,event.event_version])?;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn claim_due(&self, limit: usize) -> Result<Vec<ReminderDelivery>> {
        self.claim_due_at(limit, now_china())
    }

    fn claim_due_at(&self, limit: usize, now: ChinaDateTime) -> Result<Vec<ReminderDelivery>> {
        let mut connection = self.open()?;
        let formatted_now = format_dt(now);
        let has_due_work = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM reminder_outbox
                WHERE (delivery_state=?1 AND lease_until<=?2)
                   OR (delivery_state IN (?3,?4) AND due_at<=?2 AND (lease_until IS NULL OR lease_until<=?2))
            )",
            params![
                DeliveryState::Leased as i32,
                &formatted_now,
                DeliveryState::Pending as i32,
                DeliveryState::Failed as i32,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if !has_due_work {
            return Ok(Vec::new());
        }
        let lease = now + chrono::Duration::minutes(2);
        let tx = connection.transaction()?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=0,lease_until=NULL,updated_at=?1 WHERE delivery_state=1 AND lease_until<=?1",[&formatted_now])?;
        tx.execute(
            "UPDATE reminder_outbox AS stale
             SET delivery_state=?1,lease_until=NULL,updated_at=?2
             WHERE stale.delivery_state IN (?3,?4)
               AND stale.due_at<=?2
               AND stale.reminder_level BETWEEN ?5 AND ?6
               AND EXISTS(
                   SELECT 1 FROM reminder_outbox AS newer
                   WHERE newer.ipo_event_id=stale.ipo_event_id
                     AND newer.event_version=stale.event_version
                     AND newer.delivery_state IN (?3,?4,?7)
                     AND newer.due_at<=?2
                     AND newer.reminder_level BETWEEN ?5 AND ?6
                     AND (newer.due_at>stale.due_at OR (newer.due_at=stale.due_at AND newer.reminder_level>stale.reminder_level))
               )",
            params![
                DeliveryState::Collapsed as i32,
                formatted_now,
                DeliveryState::Pending as i32,
                DeliveryState::Failed as i32,
                ReminderLevel::Advance as i32,
                ReminderLevel::Final as i32,
                DeliveryState::Delivered as i32,
            ],
        )?;
        let ids: Vec<i64> = {
            let mut s=tx.prepare("SELECT o.id FROM reminder_outbox o JOIN ipo_events e ON e.id=o.ipo_event_id AND e.event_version=o.event_version WHERE o.delivery_state IN (0,5) AND o.due_at<=?1 AND (o.lease_until IS NULL OR o.lease_until<=?1) ORDER BY o.due_at,o.id LIMIT ?2")?;
            s.query_map(params![formatted_now, limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            tx.execute("UPDATE reminder_outbox SET delivery_state=1,lease_until=?1,attempt_count=attempt_count+1,updated_at=?2 WHERE id=?3",params![format_dt(lease),format_dt(now),id])?;
        }
        let mut deliveries = Vec::new();
        for id in ids {
            deliveries.push(tx.query_row("SELECT o.id,o.due_at,o.reminder_level,o.dedupe_key,o.attempt_count,o.message,e.* FROM reminder_outbox o JOIN ipo_events e ON e.id=o.ipo_event_id AND e.event_version=o.event_version WHERE o.id=?1",[id],map_delivery)?);
        }
        let settings: AppSettings = tx
            .query_row(
                "SELECT json_value FROM app_settings WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        if settings.secondary_notification_enabled
            && !matches!(
                settings.secondary_notification_provider,
                SecondaryNotificationProvider::Disabled | SecondaryNotificationProvider::Unknown
            )
        {
            for delivery in &deliveries {
                tx.execute(
                    "INSERT OR IGNORE INTO secondary_notification_outbox(reminder_outbox_id,provider,state,attempt_count,next_attempt_at,created_at,updated_at) VALUES(?1,?2,?3,0,?4,?4,?4)",
                    params![
                        delivery.outbox_id,
                        settings.secondary_notification_provider as i32,
                        SECONDARY_PENDING,
                        format_dt(now),
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(deliveries)
    }

    pub fn complete_delivery(&self, delivery: &ReminderDelivery, channel: &str) -> Result<()> {
        let now = now_china();
        let mut c = self.open()?;
        let tx = c.transaction()?;
        let changed = tx.execute("UPDATE reminder_outbox SET delivery_state=2,delivered_at=?1,lease_until=NULL,updated_at=?1 WHERE id=?2 AND delivery_state=1",params![format_dt(now),delivery.outbox_id])?;
        if changed != 1 {
            bail!("提醒投递租约已失效或已完成")
        }
        tx.execute("INSERT INTO reminder_log(ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,dedupe_key,result) VALUES(?1,?2,?3,?4,?5,?6,'shown')",params![delivery.event.id,format_dt(delivery.due_at),format_dt(now),delivery.level as i32,channel,delivery.dedupe_key])?;
        tx.commit()?;
        Ok(())
    }
    pub fn fail_delivery(&self, id: i64, error: &str) -> Result<()> {
        self.fail_delivery_at(id, error, now_china())
    }

    fn fail_delivery_at(&self, id: i64, error: &str, now: ChinaDateTime) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let attempt_count: i32 = transaction
            .query_row(
                "SELECT attempt_count FROM reminder_outbox WHERE id=?1 AND delivery_state=?2",
                params![id, DeliveryState::Leased as i32],
                |row| row.get(0),
            )
            .optional()?
            .context("提醒投递租约已失效或已完成")?;
        let retry = now + chrono::Duration::minutes(local_delivery_retry_minutes(attempt_count));
        let changed = transaction.execute("UPDATE reminder_outbox SET delivery_state=?1,lease_until=?2,last_error=?3,updated_at=?4 WHERE id=?5 AND delivery_state=?6",params![DeliveryState::Failed as i32,format_dt(retry),limit(error,1000),format_dt(now),id,DeliveryState::Leased as i32])?;
        if changed != 1 {
            bail!("提醒投递租约已失效或已完成")
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn claim_secondary_due(&self, limit: usize) -> Result<Vec<SecondaryNotificationDelivery>> {
        self.claim_secondary_due_at(limit, now_china())
    }

    fn claim_secondary_due_at(
        &self,
        limit: usize,
        now: ChinaDateTime,
    ) -> Result<Vec<SecondaryNotificationDelivery>> {
        let mut connection = self.open()?;
        let formatted_now = format_dt(now);
        let has_due_work = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM secondary_notification_outbox
                WHERE (state=?1 AND lease_until<=?2)
                   OR (state IN (?3,?4) AND attempt_count<?5 AND next_attempt_at<=?2)
            )",
            params![
                SECONDARY_LEASED,
                &formatted_now,
                SECONDARY_PENDING,
                SECONDARY_RETRYING,
                SECONDARY_MAX_ATTEMPTS,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if !has_due_work {
            return Ok(Vec::new());
        }
        let transaction = connection.transaction()?;
        prune_secondary_notification_history(&transaction, now)?;
        transaction.execute(
            "UPDATE secondary_notification_outbox SET state=?1,lease_until=NULL,next_attempt_at=?2,updated_at=?2 WHERE state=?3 AND lease_until<=?2",
            params![SECONDARY_RETRYING, formatted_now, SECONDARY_LEASED],
        )?;
        transaction.execute(
            "UPDATE secondary_notification_outbox SET state=?1,lease_until=NULL,updated_at=?2 WHERE state IN (?3,?4) AND NOT EXISTS(SELECT 1 FROM reminder_outbox r JOIN ipo_events e ON e.id=r.ipo_event_id AND e.event_version=r.event_version WHERE r.id=secondary_notification_outbox.reminder_outbox_id AND r.delivery_state NOT IN (?5,?6))",
            params![
                SECONDARY_CANCELLED,
                formatted_now,
                SECONDARY_PENDING,
                SECONDARY_RETRYING,
                DeliveryState::Cancelled as i32,
                DeliveryState::Collapsed as i32,
            ],
        )?;
        let requests_last_hour: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        if requests_last_hour >= SECONDARY_REQUESTS_PER_HOUR {
            transaction.commit()?;
            return Ok(Vec::new());
        }
        let provider: Option<i32> = transaction
            .query_row(
                "SELECT provider FROM secondary_notification_outbox WHERE state IN (?1,?2) AND attempt_count<?3 AND next_attempt_at<=?4 ORDER BY next_attempt_at,id LIMIT 1",
                params![SECONDARY_PENDING, SECONDARY_RETRYING, SECONDARY_MAX_ATTEMPTS, formatted_now],
                |row| row.get(0),
            )
            .optional()?;
        let Some(provider) = provider else {
            transaction.commit()?;
            return Ok(Vec::new());
        };
        let ids: Vec<i64> = {
            let mut statement = transaction.prepare(
                "SELECT id FROM secondary_notification_outbox WHERE provider=?1 AND state IN (?2,?3) AND attempt_count<?4 AND next_attempt_at<=?5 ORDER BY next_attempt_at,id LIMIT ?6",
            )?;
            statement
                .query_map(
                    params![
                        provider,
                        SECONDARY_PENDING,
                        SECONDARY_RETRYING,
                        SECONDARY_MAX_ATTEMPTS,
                        formatted_now,
                        limit.max(1) as i64,
                    ],
                    |row| row.get(0),
                )?
                .collect::<rusqlite::Result<_>>()?
        };
        transaction.execute(
            "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,-1,?3,NULL)",
            params![formatted_now, provider, ids.len() as i64],
        )?;
        let request_attempt_id = transaction.last_insert_rowid();
        let lease = format_dt(now + chrono::Duration::minutes(2));
        for id in &ids {
            transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,lease_until=?2,attempt_count=attempt_count+1,updated_at=?3 WHERE id=?4 AND state IN (?5,?6)",
                params![
                    SECONDARY_LEASED,
                    lease,
                    formatted_now,
                    id,
                    SECONDARY_PENDING,
                    SECONDARY_RETRYING,
                ],
            )?;
        }
        let mut deliveries = Vec::new();
        for id in ids {
            let mut delivery = transaction.query_row(
                "SELECT s.id,s.reminder_outbox_id,s.provider,r.due_at,r.reminder_level,s.attempt_count,r.message,e.* FROM secondary_notification_outbox s JOIN reminder_outbox r ON r.id=s.reminder_outbox_id JOIN ipo_events e ON e.id=r.ipo_event_id AND e.event_version=r.event_version WHERE s.id=?1 AND s.state=?2",
                params![id, SECONDARY_LEASED],
                map_secondary_delivery,
            )?;
            delivery.request_attempt_id = request_attempt_id;
            deliveries.push(delivery);
        }
        transaction.commit()?;
        Ok(deliveries)
    }

    pub fn complete_secondary_deliveries(
        &self,
        deliveries: &[SecondaryNotificationDelivery],
        channel: &str,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        prune_secondary_notification_history(&transaction, now)?;
        for delivery in deliveries {
            let changed = transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,delivered_at=?2,lease_until=NULL,last_error=NULL,updated_at=?2 WHERE id=?3 AND state=?4",
                params![SECONDARY_DELIVERED, format_dt(now), delivery.id, SECONDARY_LEASED],
            )?;
            if changed != 1 {
                bail!("第二通知通道租约已失效或已完成");
            }
            transaction.execute(
                "INSERT INTO reminder_log(ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,dedupe_key,result) SELECT ipo_event_id,due_at,?1,reminder_level,?2,dedupe_key,'sent' FROM reminder_outbox WHERE id=?3",
                params![format_dt(now), channel, delivery.reminder_outbox_id],
            )?;
        }
        let attempt_id = common_secondary_attempt_id(deliveries)?;
        let changed = transaction.execute(
            "UPDATE secondary_notification_attempts SET success=1,error=NULL WHERE id=?1 AND success=-1",
            [attempt_id],
        )?;
        if changed != 1 {
            bail!("第二通知通道批次配额记录已失效");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_secondary_deliveries(
        &self,
        deliveries: &[SecondaryNotificationDelivery],
        error: &str,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        for delivery in deliveries {
            let exhausted = delivery.attempt_count >= SECONDARY_MAX_ATTEMPTS;
            let delay_minutes = match delivery.attempt_count {
                0 | 1 => 1,
                2 => 5,
                3 => 15,
                _ => 30,
            };
            let changed = transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,next_attempt_at=?2,lease_until=NULL,last_error=?3,updated_at=?4 WHERE id=?5 AND state=?6",
                params![
                    if exhausted { SECONDARY_EXHAUSTED } else { SECONDARY_RETRYING },
                    format_dt(now + chrono::Duration::minutes(delay_minutes)),
                    limit(error, 1000),
                    format_dt(now),
                    delivery.id,
                    SECONDARY_LEASED,
                ],
            )?;
            if changed != 1 {
                bail!("第二通知通道失败记录的租约已失效");
            }
        }
        let attempt_id = common_secondary_attempt_id(deliveries)?;
        let changed = transaction.execute(
            "UPDATE secondary_notification_attempts SET success=0,error=?1 WHERE id=?2 AND success=-1",
            params![limit(error, 1000), attempt_id],
        )?;
        if changed != 1 {
            bail!("第二通知通道批次失败配额记录已失效");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn secondary_notification_summary(&self) -> Result<SecondaryNotificationSummary> {
        let connection = self.open()?;
        let count = |state: i32| -> Result<i64> {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM secondary_notification_outbox WHERE state=?1",
                [state],
                |row| row.get(0),
            )?)
        };
        let requests_last_hour = connection.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now_china() - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        let latest_success_at = connection
            .query_row(
                "SELECT MAX(attempted_at) FROM secondary_notification_attempts WHERE success=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )?
            .and_then(|value| parse_dt(&value).ok());
        let latest_error = connection
            .query_row(
                "SELECT error FROM secondary_notification_attempts WHERE success=0 ORDER BY attempted_at DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(SecondaryNotificationSummary {
            pending: count(SECONDARY_PENDING)?,
            leased: count(SECONDARY_LEASED)?,
            delivered: count(SECONDARY_DELIVERED)?,
            retrying: count(SECONDARY_RETRYING)?,
            exhausted: count(SECONDARY_EXHAUSTED)?,
            cancelled: count(SECONDARY_CANCELLED)?,
            requests_last_hour,
            latest_success_at,
            latest_error,
        })
    }

    pub fn reserve_secondary_notification_test(
        &self,
        provider: SecondaryNotificationProvider,
    ) -> Result<Option<i64>> {
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        if count >= SECONDARY_REQUESTS_PER_HOUR {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,-1,0,NULL)",
            params![format_dt(now), provider as i32],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Some(id))
    }

    pub fn finish_secondary_notification_test(
        &self,
        attempt_id: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.open()?.execute(
            "UPDATE secondary_notification_attempts SET success=?1,error=?2 WHERE id=?3 AND success=-1",
            params![
                i32::from(error.is_none()),
                error.map(|value| limit(value, 1000)),
                attempt_id,
            ],
        )?;
        if changed != 1 {
            bail!("第二通知通道测试配额记录已失效");
        }
        Ok(())
    }

    pub fn save_source_run(
        &self,
        source: &str,
        started: ChinaDateTime,
        state: HealthState,
        count: usize,
        raw: Option<&str>,
        hash: Option<&str>,
        schema: Option<&str>,
        error: Option<&str>,
    ) -> Result<Option<ChinaDateTime>> {
        self.save_source_run_with_retry_after(
            source, started, state, count, raw, hash, schema, error, None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_source_run_with_retry_after(
        &self,
        source: &str,
        started: ChinaDateTime,
        state: HealthState,
        count: usize,
        _raw: Option<&str>,
        hash: Option<&str>,
        schema: Option<&str>,
        error: Option<&str>,
        retry_after: Option<ChinaDateTime>,
    ) -> Result<Option<ChinaDateTime>> {
        if !matches!(
            state,
            HealthState::Healthy | HealthState::Warning | HealthState::Failed
        ) {
            bail!("来源运行状态无效：{state:?}");
        }
        let now = now_china();
        let success = state != HealthState::Failed;
        let limited_error = error.map(|value| limit(value, 2000));
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO raw_payloads(source,fetched_at,success,record_count,raw_hash,schema_fingerprint,payload,error) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![source, format_dt(now), i32::from(success), count as i64, hash, schema, Option::<String>::None, limited_error],
        )?;
        transaction.execute(
            "INSERT INTO sync_runs(source,started_at,finished_at,success,record_count,error) VALUES(?1,?2,?3,?4,?5,?6)",
            params![source, format_dt(started), format_dt(now), i32::from(success), count as i64, limited_error],
        )?;
        transaction.execute(
            "INSERT INTO source_health(source,last_attempt_at,last_success_at,last_record_count,schema_fingerprint,consecutive_failures,health_state,last_error) VALUES(?1,?2,CASE WHEN ?3<>3 THEN ?2 END,?4,?5,CASE WHEN ?3=3 THEN 1 ELSE 0 END,?3,?6) ON CONFLICT(source) DO UPDATE SET last_attempt_at=excluded.last_attempt_at,last_success_at=CASE WHEN excluded.health_state<>3 THEN excluded.last_attempt_at ELSE source_health.last_success_at END,last_record_count=excluded.last_record_count,schema_fingerprint=COALESCE(excluded.schema_fingerprint,source_health.schema_fingerprint),consecutive_failures=CASE WHEN excluded.health_state=3 THEN source_health.consecutive_failures+1 ELSE 0 END,health_state=excluded.health_state,last_error=excluded.last_error",
            params![source, format_dt(now), state as i32, count as i64, schema, limited_error],
        )?;
        let next_attempt = if state == HealthState::Failed {
            let failures: i32 = transaction
                .query_row(
                    "SELECT failure_count FROM source_backoff WHERE source=?1",
                    [source],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0)
                + 1;
            let retry_after = retry_after
                .filter(|value| *value > now)
                .map(|value| value.min(now + chrono::Duration::hours(24)));
            let next =
                retry_after.unwrap_or_else(|| now + source_backoff_delay(source, failures, now));
            let next_probe = source_probe_time(now, next);
            transaction.execute(
                "INSERT INTO source_backoff(source,failure_count,next_attempt_at,last_failure_at,last_error,next_probe_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(source) DO UPDATE SET failure_count=excluded.failure_count,next_attempt_at=excluded.next_attempt_at,last_failure_at=excluded.last_failure_at,last_error=excluded.last_error,next_probe_at=excluded.next_probe_at",
                params![source, failures, format_dt(next), format_dt(now), limited_error, format_dt(next_probe)],
            )?;
            Some(next)
        } else {
            transaction.execute(
                "INSERT INTO source_backoff(source,failure_count,next_attempt_at,last_success_at,last_error,next_probe_at) VALUES(?1,0,NULL,?2,NULL,NULL) ON CONFLICT(source) DO UPDATE SET failure_count=0,next_attempt_at=NULL,last_success_at=excluded.last_success_at,last_error=NULL,next_probe_at=NULL",
                params![source, format_dt(now)],
            )?;
            None
        };
        transaction.commit()?;
        Ok(next_attempt)
    }

    pub fn save_sync_conclusion(&self, conclusion: &SyncConclusion) -> Result<()> {
        let success = conclusion.kind.is_healthy();
        let error = (!success).then(|| limit(&conclusion.summary, 2000));
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO sync_conclusions(started_at,finished_at,conclusion_kind,today_count,event_count,announcement_count,successful_sources_json,missing_sources_json,summary) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                format_dt(conclusion.started_at),
                format_dt(conclusion.finished_at),
                conclusion.kind as i32,
                conclusion.today_count as i64,
                conclusion.event_count as i64,
                conclusion.announcement_count as i64,
                serde_json::to_string(&conclusion.successful_sources)?,
                serde_json::to_string(&conclusion.missing_sources)?,
                limit(&conclusion.summary, 2000),
            ],
        )?;
        transaction.execute(
            "INSERT INTO sync_runs(source,started_at,finished_at,success,record_count,error) VALUES('sync-conclusion',?1,?2,?3,?4,?5)",
            params![
                format_dt(conclusion.started_at),
                format_dt(conclusion.finished_at),
                i32::from(success),
                conclusion.today_count as i64,
                error
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn latest_sync_conclusion(&self) -> Result<Option<SyncConclusion>> {
        self.open()?
            .query_row(
                "SELECT conclusion_kind,started_at,finished_at,today_count,event_count,announcement_count,successful_sources_json,missing_sources_json,summary FROM sync_conclusions ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    let successful_sources: String = row.get(6)?;
                    let missing_sources: String = row.get(7)?;
                    Ok(SyncConclusion {
                        kind: SyncConclusionKind::from_i32(row.get(0)?),
                        started_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                        finished_at: parse_dt(&row.get::<_, String>(2)?).map_err(to_sql_error)?,
                        today_count: row.get::<_, i64>(3)? as usize,
                        event_count: row.get::<_, i64>(4)? as usize,
                        announcement_count: row.get::<_, i64>(5)? as usize,
                        successful_sources: serde_json::from_str(&successful_sources)
                            .map_err(|error| to_sql_error(error.into()))?,
                        missing_sources: serde_json::from_str(&missing_sources)
                            .map_err(|error| to_sql_error(error.into()))?,
                        summary: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn recent_sync_runs(&self, limit: usize) -> Result<Vec<SyncRunSummary>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT source,started_at,finished_at,success,record_count,error FROM sync_runs ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.clamp(1, 200) as i64], |row| {
            Ok(SyncRunSummary {
                source: row.get(0)?,
                started_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                finished_at: parse_dt(&row.get::<_, String>(2)?).map_err(to_sql_error)?,
                success: row.get::<_, i32>(3)? != 0,
                record_count: row.get(4)?,
                error: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn recent_reminder_log(&self, limit: usize) -> Result<Vec<ReminderLogSummary>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,result FROM reminder_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.clamp(1, 200) as i64], |row| {
            Ok(ReminderLogSummary {
                event_id: row.get(0)?,
                scheduled_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                shown_at: parse_dt(&row.get::<_, String>(2)?).map_err(to_sql_error)?,
                reminder_level: ReminderLevel::from_i32(row.get(3)?),
                delivery_channel: row.get(4)?,
                result: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn reminder_state_summary(&self) -> Result<ReminderStateSummary> {
        let connection = self.open()?;
        let (pending, leased, delivered, collapsed, cancelled, failed) = connection.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN delivery_state=0 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=1 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=2 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=3 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=4 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=5 THEN 1 ELSE 0 END),0)
             FROM reminder_outbox",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        let shown_last_seven_days = connection.query_row(
            "SELECT COUNT(*) FROM reminder_log WHERE shown_at>=?1",
            [format_dt(now_china() - chrono::Duration::days(7))],
            |row| row.get(0),
        )?;
        let latest_shown: Option<String> = connection
            .query_row(
                "SELECT shown_at FROM reminder_log ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let oldest_failed: Option<String> = connection.query_row(
            "SELECT MIN(updated_at) FROM reminder_outbox WHERE delivery_state=?1",
            [DeliveryState::Failed as i32],
            |row| row.get(0),
        )?;
        let latest_error: Option<String> = connection
            .query_row(
                "SELECT last_error FROM reminder_outbox WHERE delivery_state=?1 AND last_error IS NOT NULL ORDER BY updated_at DESC,id DESC LIMIT 1",
                [DeliveryState::Failed as i32],
                |row| row.get(0),
            )
            .optional()?;
        Ok(ReminderStateSummary {
            pending,
            leased,
            delivered,
            collapsed,
            cancelled,
            failed,
            oldest_failed_at: oldest_failed
                .as_deref()
                .and_then(|value| parse_dt(value).ok()),
            latest_error,
            shown_last_seven_days,
            latest_shown_at: latest_shown
                .as_deref()
                .and_then(|value| parse_dt(value).ok()),
        })
    }

    pub fn save_operation_health(
        &self,
        component: &str,
        state: HealthState,
        error: Option<&str>,
    ) -> Result<()> {
        if !matches!(
            state,
            HealthState::Healthy | HealthState::Warning | HealthState::Failed
        ) {
            bail!("运维组件状态无效：{state:?}");
        }
        let now = now_china();
        let limited_error = error.map(|value| limit(value, 2000));
        self.open()?.execute(
            "INSERT INTO operation_health(component,last_attempt_at,last_success_at,health_state,last_error) VALUES(?1,?2,CASE WHEN ?3<>3 THEN ?2 END,?3,?4)
             ON CONFLICT(component) DO UPDATE SET
               last_attempt_at=excluded.last_attempt_at,
               last_success_at=CASE WHEN excluded.health_state<>3 THEN excluded.last_attempt_at ELSE operation_health.last_success_at END,
               health_state=excluded.health_state,
               last_error=excluded.last_error",
            params![component, format_dt(now), state as i32, limited_error],
        )?;
        Ok(())
    }

    pub fn operation_health(&self) -> Result<Vec<OperationHealthEntry>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT component,last_attempt_at,last_success_at,health_state,last_error FROM operation_health ORDER BY component",
        )?;
        let rows = statement.query_map([], |row| {
            let last_attempt: Option<String> = row.get(1)?;
            let last_success: Option<String> = row.get(2)?;
            Ok(OperationHealthEntry {
                component: row.get(0)?,
                state: HealthState::from_i32(row.get(3)?),
                last_attempt_at: last_attempt
                    .as_deref()
                    .and_then(|value| parse_dt(value).ok()),
                last_success_at: last_success
                    .as_deref()
                    .and_then(|value| parse_dt(value).ok()),
                last_error: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn save_announcement(&self, document: &AnnouncementDocument) -> Result<()> {
        self.open()?.execute("INSERT OR IGNORE INTO announcement_documents(id,ipo_event_id,provider,announcement_id,announcement_type,title,published_at,source_url,local_path,file_hash,extraction_status,extracted_text_hash,parser_version,parsed_fields_json,downloaded_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![document.id,document.event_id,document.reference.provider,document.reference.announcement_id,document.reference.announcement_type,document.reference.title,document.reference.published_at.map(format_dt),document.reference.url,document.local_path,document.file_hash,document.status as i32,document.text_hash,document.parser_version,serde_json::to_string(&document.fields)?,format_dt(document.downloaded_at)])?;
        Ok(())
    }
    pub fn replace_field_sources(&self, event_id: &str, candidates: &[Candidate]) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM ipo_field_sources WHERE ipo_event_id=?1",
            [event_id],
        )?;
        for candidate in candidates {
            let values = [
                ("SecurityCode", candidate.security_code.clone()),
                ("ApplyCode", candidate.apply_code.clone()),
                ("LegacyCode", candidate.legacy_code.clone()),
                ("Name", candidate.name.clone()),
                ("ApplyDate", candidate.apply_date.map(format_date)),
                (
                    "IssuePrice",
                    candidate.issue_price.map(|value| value.to_string()),
                ),
                ("LotSize", candidate.lot_size.map(|value| value.to_string())),
                (
                    "MaxApplyQuantity",
                    candidate.max_apply_quantity.map(|value| value.to_string()),
                ),
                (
                    "RequiredMarketValue",
                    candidate
                        .required_market_value
                        .map(|value| value.to_string()),
                ),
                (
                    "RequiredCash",
                    candidate.required_cash.map(|value| value.to_string()),
                ),
                ("BallotDate", candidate.ballot_date.map(format_date)),
                ("PaymentDate", candidate.payment_date.map(format_date)),
                ("ListingDate", candidate.listing_date.map(format_date)),
                ("IssueStatus", Some((candidate.status as i32).to_string())),
            ];
            for (field, value) in values {
                let Some(value) = value else { continue };
                transaction.execute(
                    "INSERT INTO ipo_field_sources(ipo_event_id,field_name,normalized_value,raw_value,source,source_published_at,fetched_at,raw_hash,priority) VALUES(?1,?2,?3,?3,?4,?5,?6,NULL,?7)",
                    params![event_id, field, value, candidate.source, candidate.published_at.map(format_dt), format_dt(candidate.fetched_at), candidate.priority],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
    pub fn touch_heartbeat(&self, component: &str, now: ChinaDateTime) -> Result<()> {
        self.open()?.execute(
            "INSERT INTO app_heartbeat(component,heartbeat_at) VALUES(?1,?2) ON CONFLICT(component) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",
            params![component, format_dt(now)],
        )?;
        Ok(())
    }

    pub fn touch_runtime_heartbeats(&self, now: ChinaDateTime) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let heartbeat_at = format_dt(now);
        for component in ["scheduler", "delivery"] {
            transaction.execute(
                "INSERT INTO app_heartbeat(component,heartbeat_at) VALUES(?1,?2) ON CONFLICT(component) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",
                params![component, &heartbeat_at],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn source_can_attempt(
        &self,
        source: &str,
        now: ChinaDateTime,
    ) -> Result<(bool, Option<ChinaDateTime>)> {
        let connection = self.open()?;
        let next: Option<Option<String>> = connection
            .query_row(
                "SELECT next_attempt_at FROM source_backoff WHERE source=?1",
                [source],
                |row| row.get(0),
            )
            .optional()?;
        let next = next.flatten().and_then(|value| parse_dt(&value).ok());
        Ok((next.is_none_or(|value| value <= now), next))
    }

    pub fn try_claim_source_probe(&self, source: &str, now: ChinaDateTime) -> Result<bool> {
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        let values: Option<(Option<String>, Option<String>)> = transaction
            .query_row(
                "SELECT next_attempt_at,next_probe_at FROM source_backoff WHERE source=?1",
                [source],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((next_attempt, next_probe)) = values else {
            return Ok(false);
        };
        let next_attempt = next_attempt
            .as_deref()
            .and_then(|value| parse_dt(value).ok());
        let next_probe = next_probe.as_deref().and_then(|value| parse_dt(value).ok());
        let Some(next_attempt) = next_attempt.filter(|value| *value > now) else {
            return Ok(false);
        };
        if next_probe.is_none_or(|value| value > now) {
            return Ok(false);
        }
        let following_probe = (now + chrono::Duration::minutes(30)).min(next_attempt);
        let changed = transaction.execute(
            "UPDATE source_backoff SET next_probe_at=?1 WHERE source=?2 AND next_attempt_at>?3 AND next_probe_at<=?3",
            params![format_dt(following_probe), source, format_dt(now)],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn save_source_probe_run(
        &self,
        source: &str,
        started_at: ChinaDateTime,
        success: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let finished_at = now_china();
        let error = error.map(|value| limit(value, 2000));
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO sync_runs(source,started_at,finished_at,success,record_count,error) VALUES(?1,?2,?3,?4,0,?5)",
            params![
                format!("health-probe:{source}"),
                format_dt(started_at),
                format_dt(finished_at),
                i32::from(success),
                error,
            ],
        )?;
        transaction.execute(
            "UPDATE source_backoff SET last_probe_at=?1,last_probe_success=?2,last_probe_error=?3 WHERE source=?4",
            params![format_dt(finished_at), i32::from(success), error, source],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_count(&self) -> Result<i64> {
        Ok(self.open()?.query_row(
            "SELECT COUNT(*) FROM ipo_events WHERE apply_date=?1 AND lifecycle_status IN (?2,?3)",
            params![
                format_date(now_china().date_naive()),
                LifecycleStatus::ActiveUnconfirmed as i32,
                LifecycleStatus::AcknowledgedNeedsReview as i32
            ],
            |row| row.get(0),
        )?)
    }

    pub fn health_text(&self) -> Result<(HealthState, String)> {
        let details = self.health_details()?;
        let failed = details
            .sources
            .iter()
            .filter(|source| source.state == HealthState::Failed)
            .count();
        let warning = details
            .sources
            .iter()
            .filter(|source| source.state == HealthState::Warning)
            .count();
        let operation_failed = details
            .operations
            .iter()
            .filter(|operation| operation.state == HealthState::Failed)
            .count();
        let operation_warning = details
            .operations
            .iter()
            .filter(|operation| operation.state == HealthState::Warning)
            .count();
        let latest = details
            .sources
            .iter()
            .filter_map(|source| source.last_success_at)
            .max()
            .map(|value| value.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "尚无成功同步".into());
        Ok((
            details.overall_state,
            format!(
                "数据源失败 {failed} 个，警告 {warning} 个；运维失败 {operation_failed} 项，警告 {operation_warning} 项；最近成功：{latest}"
            ),
        ))
    }

    pub fn health_details(&self) -> Result<HealthDetails> {
        let now = now_china();
        let settings = self.settings()?;
        let events: Vec<IpoEvent> = self
            .today_events()?
            .into_iter()
            .filter(|event| settings.exchange_enabled(event.exchange))
            .collect();
        let active_window = events.iter().any(|event| {
            matches!(
                event.lifecycle_status,
                LifecycleStatus::Scheduled
                    | LifecycleStatus::ActiveUnconfirmed
                    | LifecycleStatus::AcknowledgedNeedsReview
            )
        });
        let sync_minutes = if active_window {
            settings.active_day_sync_minutes
        } else {
            settings.normal_sync_minutes
        }
        .clamp(5, 7 * 24 * 60) as i64;
        let minimum_stale = chrono::Duration::hours(if active_window { 1 } else { 2 });
        let minimum_failed = chrono::Duration::hours(if active_window { 2 } else { 6 });
        let scheduled_stale = chrono::Duration::minutes(sync_minutes + 15);
        let scheduled_failed = chrono::Duration::minutes(sync_minutes * 2 + 30);
        let stale_after = minimum_stale.max(scheduled_stale);
        let failed_after = minimum_failed.max(scheduled_failed);
        let expected_announcement_sources: HashSet<&'static str> = self
            .events(
                now.date_naive() - chrono::Duration::days(7),
                now.date_naive() + chrono::Duration::days(45),
            )?
            .into_iter()
            .filter(|event| settings.exchange_enabled(event.exchange))
            .filter_map(|event| match event.exchange {
                Exchange::Shanghai => Some("sse-announcement"),
                Exchange::Shenzhen => Some("cninfo-announcement"),
                Exchange::Beijing => Some("bse-announcement"),
                _ => None,
            })
            .collect();
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT source,last_success_at,last_record_count,consecutive_failures,health_state,last_error FROM source_health ORDER BY source")?;
        let rows = statement.query_map([], |row| {
            let last_success: Option<String> = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                last_success,
                row.get::<_, i64>(2)?,
                row.get::<_, i32>(3)?,
                row.get::<_, i32>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut sources = Vec::new();
        for row in rows {
            let (
                source,
                last_success,
                last_record_count,
                consecutive_failures,
                stored_state,
                last_error,
            ) = row?;
            let last_success_at = last_success
                .as_deref()
                .and_then(|value| parse_dt(value).ok());
            let age = last_success_at.map(|value| now - value);
            let stored_state = HealthState::from_i32(stored_state);
            let expected = !source.ends_with("-announcement")
                || expected_announcement_sources.contains(source.as_str());
            let state = if !expected {
                match stored_state {
                    HealthState::Failed | HealthState::Unknown => HealthState::Warning,
                    state => state,
                }
            } else {
                match stored_state {
                    HealthState::Failed => HealthState::Failed,
                    HealthState::Warning => {
                        if age.is_none_or(|value| value > failed_after) {
                            HealthState::Failed
                        } else {
                            HealthState::Warning
                        }
                    }
                    HealthState::Healthy => {
                        if age.is_none_or(|value| value > failed_after) {
                            HealthState::Failed
                        } else if age.is_some_and(|value| value > stale_after) {
                            HealthState::Warning
                        } else {
                            HealthState::Healthy
                        }
                    }
                    HealthState::Unknown => HealthState::Failed,
                }
            };
            sources.push(SourceHealthEntry {
                source,
                state,
                last_record_count,
                last_success_at,
                consecutive_failures,
                last_error,
            });
        }
        drop(statement);
        let heartbeat = |component: &str| -> Result<Option<ChinaDateTime>> {
            let value: Option<String> = connection
                .query_row(
                    "SELECT heartbeat_at FROM app_heartbeat WHERE component=?1",
                    [component],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(value.as_deref().and_then(|value| parse_dt(value).ok()))
        };
        let scheduler_heartbeat = heartbeat("scheduler")?;
        let delivery_heartbeat = heartbeat("delivery")?;
        let operations = self.operation_health()?;
        let reminder_state = self.reminder_state_summary()?;
        let heartbeat_limit = now - chrono::Duration::minutes(3);
        let persistent_delivery_failure = reminder_state.failed > 0
            && reminder_state.oldest_failed_at.is_some_and(|value| {
                value <= now - chrono::Duration::minutes(LOCAL_DELIVERY_PERSISTENT_FAILURE_MINUTES)
            });
        let quality_warning = events.iter().any(|event| {
            matches!(
                event.data_quality_status,
                DataQualityStatus::DataConflict
                    | DataQualityStatus::Stale
                    | DataQualityStatus::ManualReviewRequired
            )
        });
        let overall_state = if sources.is_empty()
            || sources
                .iter()
                .all(|source| source.state == HealthState::Failed)
            || scheduler_heartbeat.is_none_or(|value| value < heartbeat_limit)
            || operations
                .iter()
                .any(|operation| operation.state == HealthState::Failed)
            || persistent_delivery_failure
        {
            HealthState::Failed
        } else if sources
            .iter()
            .any(|source| source.state != HealthState::Healthy)
            || delivery_heartbeat.is_none_or(|value| value < heartbeat_limit)
            || quality_warning
            || reminder_state.failed > 0
            || operations
                .iter()
                .any(|operation| operation.state == HealthState::Warning)
        {
            HealthState::Warning
        } else {
            HealthState::Healthy
        };
        Ok(HealthDetails {
            overall_state,
            today_task_count: events.len(),
            pending_confirmation_count: events
                .iter()
                .filter(|event| {
                    matches!(
                        event.lifecycle_status,
                        LifecycleStatus::Scheduled
                            | LifecycleStatus::ActiveUnconfirmed
                            | LifecycleStatus::AcknowledgedNeedsReview
                    )
                })
                .count(),
            conflict_count: events
                .iter()
                .filter(|event| event.data_quality_status == DataQualityStatus::DataConflict)
                .count(),
            manual_review_count: events
                .iter()
                .filter(|event| {
                    event.data_quality_status == DataQualityStatus::ManualReviewRequired
                })
                .count(),
            delivery_retry_count: reminder_state.failed.max(0) as usize,
            oldest_delivery_retry_at: reminder_state.oldest_failed_at,
            latest_delivery_error: reminder_state.latest_error,
            scheduler_heartbeat,
            delivery_heartbeat,
            sources,
            operations,
        })
    }

    pub fn try_mark_health_summary_sent(
        &self,
        date: NaiveDate,
        now: ChinaDateTime,
    ) -> Result<bool> {
        Ok(self.open()?.execute(
            "INSERT OR IGNORE INTO health_summary_log(summary_date,sent_at) VALUES(?1,?2)",
            params![format_date(date), format_dt(now)],
        )? == 1)
    }

    pub fn try_mark_health_summary_due(&self, now: ChinaDateTime) -> Result<bool> {
        if now.time() < crate::model::time(8, 0) {
            return Ok(false);
        }
        self.try_mark_health_summary_sent(now.date_naive(), now)
    }

    #[cfg(test)]
    pub fn announcement_titles(&self, event_id: &str) -> Result<Vec<String>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT title FROM announcement_documents WHERE ipo_event_id=?1 ORDER BY published_at DESC,downloaded_at DESC")?;
        let rows = statement.query_map([event_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn field_sources(&self, event_id: &str) -> Result<Vec<FieldSourceEntry>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT field_name,raw_value,normalized_value,source,priority,fetched_at FROM ipo_field_sources WHERE ipo_event_id=?1 ORDER BY field_name,priority DESC,fetched_at DESC,id",
        )?;
        let rows = statement.query_map([event_id], |row| {
            let fetched_at: String = row.get(5)?;
            Ok(FieldSourceEntry {
                field_name: row.get(0)?,
                raw_value: row.get(1)?,
                normalized_value: row.get(2)?,
                source: row.get(3)?,
                priority: row.get(4)?,
                fetched_at: parse_dt(&fetched_at).map_err(to_sql_error)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn announcements(&self, event_id: &str) -> Result<Vec<AnnouncementDocument>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id,ipo_event_id,provider,announcement_id,announcement_type,title,published_at,source_url,local_path,file_hash,extraction_status,extracted_text_hash,parser_version,parsed_fields_json,downloaded_at FROM announcement_documents WHERE ipo_event_id=?1 ORDER BY published_at DESC,downloaded_at DESC,id",
        )?;
        let rows = statement.query_map([event_id], |row| {
            let published_at: Option<String> = row.get(6)?;
            let fields_json: String = row.get(13)?;
            let downloaded_at: String = row.get(14)?;
            Ok(AnnouncementDocument {
                id: row.get(0)?,
                event_id: row.get(1)?,
                reference: AnnouncementRef {
                    provider: row.get(2)?,
                    announcement_id: row.get(3)?,
                    announcement_type: row.get(4)?,
                    title: row.get(5)?,
                    published_at: published_at
                        .as_deref()
                        .and_then(|value| parse_dt(value).ok()),
                    url: row.get(7)?,
                },
                local_path: row.get(8)?,
                file_hash: row.get(9)?,
                status: ExtractionStatus::from_i32(row.get(10)?),
                text_hash: row.get(11)?,
                parser_version: row.get(12)?,
                fields: serde_json::from_str(&fields_json).unwrap_or_default(),
                downloaded_at: parse_dt(&downloaded_at).map_err(to_sql_error)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn manual_overrides(
        &self,
        event_id: &str,
        version: i32,
    ) -> Result<Vec<ManualOverrideEntry>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id,field_name,override_value,reason,announcement_document_id,created_at,revoked_at FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 ORDER BY created_at DESC,id DESC",
        )?;
        let rows = statement.query_map(params![event_id, version], |row| {
            let created_at: String = row.get(5)?;
            let revoked_at: Option<String> = row.get(6)?;
            Ok(ManualOverrideEntry {
                id: row.get(0)?,
                field_name: row.get(1)?,
                override_value: row.get(2)?,
                reason: row.get(3)?,
                announcement_document_id: row.get(4)?,
                created_at: parse_dt(&created_at).map_err(to_sql_error)?,
                revoked_at: revoked_at.as_deref().and_then(|value| parse_dt(value).ok()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn apply_manual_override(
        &self,
        event_id: &str,
        version: i32,
        field: &str,
        value: &str,
        reason: &str,
        announcement_id: Option<&str>,
    ) -> Result<()> {
        let event = self.event(event_id)?.context("发行任务不存在")?;
        if event.event_version != version {
            bail!("发行任务版本已变化，请刷新后重试");
        }
        let value = normalize_manual_override(field, value)?;
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("人工覆盖必须填写核验理由");
        }
        if let Some(announcement_id) = announcement_id.filter(|value| !value.trim().is_empty()) {
            let belongs_to_event = self.open()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM announcement_documents WHERE id=?1 AND ipo_event_id=?2)",
                params![announcement_id, event_id],
                |row| row.get::<_, i32>(0),
            )? != 0;
            if !belongs_to_event {
                bail!("所选依据公告不存在或不属于当前发行任务");
            }
        }
        self.open()?.execute(
            "INSERT INTO manual_overrides(ipo_event_id,event_version,field_name,override_value,reason,announcement_document_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![event_id, version, field, limit(&value, 200), limit(reason, 500), announcement_id, format_dt(now_china())],
        )?;
        if let Some(event) = self.event(event_id)? {
            self.reconcile_schedule(&event, &self.settings()?, now_china())?;
        }
        Ok(())
    }

    pub fn revoke_manual_override(
        &self,
        event_id: &str,
        version: i32,
        override_id: i64,
    ) -> Result<()> {
        let changed = self.open()?.execute(
            "UPDATE manual_overrides SET revoked_at=?1 WHERE id=?2 AND ipo_event_id=?3 AND event_version=?4 AND revoked_at IS NULL",
            params![format_dt(now_china()), override_id, event_id, version],
        )?;
        if changed == 0 {
            bail!("人工覆盖记录不存在、已经撤销或属于旧的数据版本");
        }
        if let Some(event) = self.event(event_id)? {
            self.reconcile_schedule(&event, &self.settings()?, now_china())?;
        }
        Ok(())
    }

    pub fn maintenance(&self, data_root: &Path) -> Result<()> {
        let connection = self.open()?;
        let now = now_china();
        connection.execute(
            "DELETE FROM raw_payloads WHERE fetched_at < ?1",
            [format_dt(now - chrono::Duration::days(14))],
        )?;
        connection.execute(
            "DELETE FROM sync_runs WHERE finished_at < ?1",
            [format_dt(now - chrono::Duration::days(90))],
        )?;
        connection.execute(
            "DELETE FROM reminder_log WHERE shown_at < ?1",
            [format_dt(now - chrono::Duration::days(180))],
        )?;
        prune_secondary_notification_history(&connection, now)?;
        let temporary = data_root.join("temp");
        if temporary.exists() {
            for entry in fs::read_dir(temporary)? {
                let entry = entry?;
                if entry
                    .metadata()?
                    .modified()
                    .ok()
                    .and_then(|value| value.elapsed().ok())
                    .is_some_and(|age| age > StdDuration::from_secs(24 * 3600))
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    pub fn compact_if_needed(&self) -> Result<bool> {
        let connection = self.open()?;
        let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let free_pages: i64 =
            connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        if page_count < 256 || free_pages.saturating_mul(4) < page_count {
            return Ok(false);
        }
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(true)
    }

    pub fn backup(&self, backup_dir: &Path) -> Result<PathBuf> {
        self.backup_with_commit_hook(backup_dir, |_| Ok(()))
    }

    fn backup_with_commit_hook(
        &self,
        backup_dir: &Path,
        before_commit: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(backup_dir)?;
        let timestamp = now_china();
        let target = backup_dir.join(format!(
            "stock-ipo-reminder-{}.db",
            timestamp.format("%Y%m%d-%H%M%S-%3f")
        ));
        let temporary = backup_dir.join(format!(
            ".stock-ipo-reminder-backup-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let source = self.open()?;
        let result = (|| -> Result<()> {
            let mut destination = Connection::open(&temporary)?;
            {
                let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
                backup.run_to_completion(BACKUP_PAGES_PER_STEP, BACKUP_STEP_PAUSE, None)?;
            }
            let integrity: String =
                destination.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            if integrity != "ok" {
                bail!("备份完整性检查失败：{integrity}")
            }
            drop(destination);
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&temporary)?
                .sync_all()?;
            before_commit(&temporary)?;
            fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(target)
    }
}

fn retain_known_optional_fields(previous: &IpoEvent, current: &mut IpoEvent) {
    if current.apply_code.is_none() {
        current.apply_code.clone_from(&previous.apply_code);
    }
    if current.legacy_code.is_none() {
        current.legacy_code.clone_from(&previous.legacy_code);
    }
    if current.apply_date.is_none() {
        current.apply_date = previous.apply_date;
    }
    if current.issue_price.is_none() {
        current.issue_price = previous.issue_price;
    }
    if current.lot_size.is_none() {
        current.lot_size = previous.lot_size;
    }
    if current.max_apply_quantity.is_none() {
        current.max_apply_quantity = previous.max_apply_quantity;
    }
    if current.required_market_value.is_none() {
        current.required_market_value = previous.required_market_value;
    }
    if current.required_cash.is_none() {
        current.required_cash = previous.required_cash;
    }
    if current.ballot_date.is_none() {
        current.ballot_date = previous.ballot_date;
    }
    if current.payment_date.is_none() {
        current.payment_date = previous.payment_date;
    }
    if current.listing_date.is_none() {
        current.listing_date = previous.listing_date;
    }
    if current.announcement_url.is_none() {
        current
            .announcement_url
            .clone_from(&previous.announcement_url);
    }
}

fn migrate_sync_schedule_v3(connection: &Connection) -> Result<()> {
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

fn migrate_sync_conclusions_v4(connection: &Connection) -> Result<()> {
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

fn migrate_operation_health_v5(connection: &Connection) -> Result<()> {
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

fn migrate_source_probes_v6(connection: &Connection) -> Result<()> {
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

fn migrate_outbox_messages_v7(connection: &Connection) -> Result<()> {
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

fn migrate_secondary_notifications_v8(connection: &Connection) -> Result<()> {
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

fn migrate_raw_payload_metadata_v9(connection: &Connection) -> Result<()> {
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

fn source_backoff_delay(source: &str, failures: i32, now: ChinaDateTime) -> chrono::Duration {
    let base_minutes = match failures {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        5 => 15,
        _ => 30,
    };
    let base_seconds = base_minutes * 60;
    let maximum_jitter = base_seconds / 10;
    let digest = sha256(format!("{source}|{failures}|{}", now.timestamp_millis()));
    let seed = u64::from_str_radix(&digest[..8], 16).unwrap_or_default();
    let jitter = (seed % (maximum_jitter as u64 + 1)) as i64;
    chrono::Duration::seconds(base_seconds as i64 + jitter)
}

fn local_delivery_retry_minutes(attempt_count: i32) -> i64 {
    match attempt_count {
        i32::MIN..=1 => 1,
        2 => 5,
        3 => 15,
        _ => 30,
    }
}

fn source_probe_time(now: ChinaDateTime, next_attempt: ChinaDateTime) -> ChinaDateTime {
    (now + chrono::Duration::minutes(10)).min(next_attempt)
}

fn enqueue_change_notification_tx(
    transaction: &rusqlite::Transaction<'_>,
    event: &IpoEvent,
    critical: bool,
    reason: &str,
    previous_updated_at: ChinaDateTime,
) -> Result<()> {
    let message = if critical {
        format!("{reason}。请重新核对任务详情；若此前已确认，必须重新确认。")
    } else {
        format!("{reason}。关键申购条件未变化，本次变更仅提醒一次。")
    };
    let fingerprint = sha256(format!(
        "{}|{}|{}|{}|{}",
        event.id,
        event.event_version,
        previous_updated_at.timestamp_micros(),
        event.updated_at.timestamp_micros(),
        reason
    ));
    let dedupe_key = format!(
        "{}:{}:change:{}",
        event.id,
        event.event_version,
        &fingerprint[..24]
    );
    transaction.execute(
        "INSERT OR IGNORE INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,delivery_state,message,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?3,?3)",
        params![
            event.id,
            event.event_version,
            format_dt(event.updated_at),
            ReminderLevel::DataChanged as i32,
            dedupe_key,
            DeliveryState::Pending as i32,
            limit(&message, 1000),
        ],
    )?;
    Ok(())
}

fn reconcile_schedule_tx(
    tx: &rusqlite::Transaction<'_>,
    event: &IpoEvent,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> Result<()> {
    let planned = plan_reminders(event, settings, now);
    tx.execute("UPDATE reminder_outbox SET delivery_state=4,updated_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND reminder_level<>?4 AND delivery_state IN (0,1,5)",params![format_dt(now),event.id,event.event_version,ReminderLevel::DataChanged as i32])?;
    for item in planned {
        tx.execute("INSERT INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,delivery_state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,0,?6,?6) ON CONFLICT(dedupe_key) DO UPDATE SET delivery_state=CASE WHEN reminder_outbox.delivery_state IN (2,3) THEN reminder_outbox.delivery_state ELSE 0 END,due_at=excluded.due_at,reminder_level=excluded.reminder_level,updated_at=excluded.updated_at",params![item.event_id,item.event_version,format_dt(item.due_at),item.level as i32,item.dedupe_key,format_dt(now)])?;
    }
    Ok(())
}

fn map_event(row: &Row<'_>) -> rusqlite::Result<IpoEvent> {
    let sessions_json: String = row.get("sessions_json")?;
    Ok(IpoEvent {
        id: row.get("id")?,
        exchange: Exchange::from_i32(row.get("exchange")?),
        board: Board::from_i32(row.get("board")?),
        security_code: row.get("security_code")?,
        apply_code: row.get("apply_code")?,
        legacy_code: row.get("legacy_code")?,
        name: row.get("name")?,
        apply_date: parse_optional_date(row.get("apply_date")?),
        issue_price: row.get("issue_price")?,
        lot_size: row.get("lot_size")?,
        max_apply_quantity: row.get("max_apply_quantity")?,
        required_market_value: row.get("required_market_value")?,
        required_cash: row.get("required_cash")?,
        ballot_date: parse_optional_date(row.get("ballot_date")?),
        payment_date: parse_optional_date(row.get("payment_date")?),
        listing_date: parse_optional_date(row.get("listing_date")?),
        status: IssueStatus::from_i32(row.get("issue_status")?),
        lifecycle_status: LifecycleStatus::from_i32(row.get("lifecycle_status")?),
        event_version: row.get("event_version")?,
        announcement_url: row.get("announcement_url")?,
        data_quality_status: DataQualityStatus::from_i32(row.get("data_quality_status")?),
        data_conflict: row.get::<_, i32>("data_conflict")? != 0,
        manual_override_fields: Vec::new(),
        sessions: serde_json::from_str(&sessions_json).unwrap_or_default(),
        first_seen_at: parse_dt(&row.get::<_, String>("first_seen_at")?).map_err(to_sql_error)?,
        updated_at: parse_dt(&row.get::<_, String>("updated_at")?).map_err(to_sql_error)?,
    })
}
fn map_delivery(row: &Row<'_>) -> rusqlite::Result<ReminderDelivery> {
    let event = map_event_offset(row, 6)?;
    Ok(ReminderDelivery {
        outbox_id: row.get(0)?,
        due_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
        level: ReminderLevel::from_i32(row.get(2)?),
        dedupe_key: row.get(3)?,
        attempt_count: row.get(4)?,
        message: row.get(5)?,
        event,
    })
}

fn map_secondary_delivery(row: &Row<'_>) -> rusqlite::Result<SecondaryNotificationDelivery> {
    let event = map_event_offset(row, 7)?;
    Ok(SecondaryNotificationDelivery {
        id: row.get(0)?,
        reminder_outbox_id: row.get(1)?,
        request_attempt_id: 0,
        provider: SecondaryNotificationProvider::from_i32(row.get(2)?),
        due_at: parse_dt(&row.get::<_, String>(3)?).map_err(to_sql_error)?,
        level: ReminderLevel::from_i32(row.get(4)?),
        attempt_count: row.get(5)?,
        message: row.get(6)?,
        event,
    })
}

fn common_secondary_attempt_id(deliveries: &[SecondaryNotificationDelivery]) -> Result<i64> {
    let first = deliveries
        .first()
        .context("第二通知通道批次为空")?
        .request_attempt_id;
    if first <= 0
        || deliveries
            .iter()
            .any(|delivery| delivery.request_attempt_id != first)
    {
        bail!("第二通知通道批次配额记录不一致");
    }
    Ok(first)
}

fn prune_secondary_notification_history(connection: &Connection, now: ChinaDateTime) -> Result<()> {
    connection.execute(
        "DELETE FROM secondary_notification_attempts WHERE attempted_at<?1",
        [format_dt(
            now - chrono::Duration::days(SECONDARY_ATTEMPT_RETENTION_DAYS),
        )],
    )?;
    connection.execute(
        "DELETE FROM secondary_notification_attempts WHERE id NOT IN (SELECT id FROM secondary_notification_attempts ORDER BY attempted_at DESC,id DESC LIMIT ?1)",
        [SECONDARY_MAX_ATTEMPT_RECORDS],
    )?;
    connection.execute(
        "DELETE FROM secondary_notification_outbox WHERE state IN (?1,?2,?3) AND updated_at<?4",
        params![
            SECONDARY_DELIVERED,
            SECONDARY_EXHAUSTED,
            SECONDARY_CANCELLED,
            format_dt(now - chrono::Duration::days(SECONDARY_OUTBOX_RETENTION_DAYS)),
        ],
    )?;
    Ok(())
}
fn map_event_offset(row: &Row<'_>, offset: usize) -> rusqlite::Result<IpoEvent> {
    let sessions: String = row.get(offset + 22)?;
    Ok(IpoEvent {
        id: row.get(offset)?,
        exchange: Exchange::from_i32(row.get(offset + 1)?),
        board: Board::from_i32(row.get(offset + 2)?),
        security_code: row.get(offset + 3)?,
        apply_code: row.get(offset + 4)?,
        legacy_code: row.get(offset + 5)?,
        name: row.get(offset + 6)?,
        apply_date: parse_optional_date(row.get(offset + 7)?),
        issue_price: row.get(offset + 8)?,
        lot_size: row.get(offset + 9)?,
        max_apply_quantity: row.get(offset + 10)?,
        required_market_value: row.get(offset + 11)?,
        required_cash: row.get(offset + 12)?,
        ballot_date: parse_optional_date(row.get(offset + 13)?),
        payment_date: parse_optional_date(row.get(offset + 14)?),
        listing_date: parse_optional_date(row.get(offset + 15)?),
        status: IssueStatus::from_i32(row.get(offset + 16)?),
        lifecycle_status: LifecycleStatus::from_i32(row.get(offset + 17)?),
        event_version: row.get(offset + 18)?,
        announcement_url: row.get(offset + 19)?,
        data_quality_status: DataQualityStatus::from_i32(row.get(offset + 20)?),
        data_conflict: row.get::<_, i32>(offset + 21)? != 0,
        manual_override_fields: Vec::new(),
        sessions: serde_json::from_str(&sessions).unwrap_or_default(),
        first_seen_at: parse_dt(&row.get::<_, String>(offset + 23)?).map_err(to_sql_error)?,
        updated_at: parse_dt(&row.get::<_, String>(offset + 24)?).map_err(to_sql_error)?,
    })
}

fn apply_manual_overrides(connection: &Connection, event: &mut IpoEvent) -> Result<()> {
    let mut statement = connection.prepare("SELECT field_name,override_value FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 AND revoked_at IS NULL ORDER BY id")?;
    let rows = statement.query_map(params![event.id, event.event_version], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    event.manual_override_fields.clear();
    for row in rows {
        let (field, value) = row?;
        if !event.manual_override_fields.contains(&field) {
            event.manual_override_fields.push(field.clone());
        }
        match field.as_str() {
            "ApplyCode" => event.apply_code = Some(value),
            "ApplyDate" => event.apply_date = parse_date_value(&value),
            "IssuePrice" => event.issue_price = value.parse().ok(),
            "LotSize" => event.lot_size = value.parse().ok(),
            "MaxApplyQuantity" => event.max_apply_quantity = value.parse().ok(),
            "OfficialSessions" => event.sessions = parse_override_sessions(&value, event.exchange)?,
            "IssueStatus" => {
                event.status =
                    parse_issue_status_override(&value).context("人工覆盖的发行状态无效")?
            }
            _ => {}
        }
    }
    Ok(())
}

fn normalize_manual_override(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    match field {
        "ApplyCode"
            if value.chars().count() == 6
                && value.chars().all(|character| character.is_ascii_digit()) =>
        {
            Ok(value.to_owned())
        }
        "ApplyDate" => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(format_date)
            .context("申购日期格式无效，请使用 yyyy-MM-dd"),
        "IssuePrice" => {
            let number: f64 = value.parse().context("发行价格必须是大于 0 的数字")?;
            if !number.is_finite() || number <= 0.0 {
                bail!("发行价格必须是大于 0 的数字");
            }
            Ok(number.to_string())
        }
        "LotSize" => normalize_positive_integer(value, "申购单位必须是大于 0 的整数股数"),
        "MaxApplyQuantity" => normalize_positive_integer(value, "申购上限必须是大于 0 的整数股数"),
        "OfficialSessions" => normalize_session_text(value),
        "IssueStatus" => parse_issue_status_override(value)
            .map(issue_status_override_text)
            .context("发行状态无效"),
        "ApplyCode" => bail!("申购代码必须是 6 位数字"),
        _ => bail!("该字段不允许人工覆盖：{field}"),
    }
}

fn normalize_positive_integer(value: &str, error: &str) -> Result<String> {
    let number: i64 = value.parse().with_context(|| error.to_owned())?;
    if number <= 0 {
        bail!(error.to_owned());
    }
    Ok(number.to_string())
}

fn normalize_session_text(value: &str) -> Result<String> {
    let normalized = value
        .replace('，', ",")
        .replace('；', ",")
        .replace(';', ",");
    let pairs = normalized
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !(1..=3).contains(&pairs.len()) {
        bail!("官方时段格式无效，例如 09:30-11:30,13:00-15:00");
    }
    let mut result = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let pair = pair.replace('—', "-").replace('–', "-").replace("至", "-");
        let (start, end) = pair
            .split_once('-')
            .context("官方时段格式无效，例如 09:30-11:30,13:00-15:00")?;
        let start = parse_time_value(start.trim()).context("官方时段开始时间无效")?;
        let end = parse_time_value(end.trim()).context("官方时段结束时间无效")?;
        if start >= end {
            bail!("官方时段开始时间必须早于结束时间");
        }
        result.push(format!("{}-{}", start.format("%H:%M"), end.format("%H:%M")));
    }
    Ok(result.join(","))
}

fn parse_override_sessions(value: &str, exchange: Exchange) -> Result<Vec<SubscriptionSession>> {
    let normalized = normalize_session_text(value)?;
    normalized
        .split(',')
        .enumerate()
        .map(|(index, pair)| {
            let (start, end) = pair.split_once('-').context("人工覆盖的官方时段无效")?;
            Ok(SubscriptionSession {
                session_number: index as i32 + 1,
                official_start: parse_time_value(start)
                    .context("人工覆盖的官方时段开始时间无效")?,
                official_end: parse_time_value(end).context("人工覆盖的官方时段结束时间无效")?,
                broker_accept_start: None,
                safety_cutoff: None,
                funding_mode: if exchange == Exchange::Beijing {
                    FundingMode::FullCash
                } else {
                    FundingMode::MarketValue
                },
                allocation_time_sensitive: exchange == Exchange::Beijing,
                source: "manual-override".into(),
                source_published_at: None,
            })
        })
        .collect()
}

fn parse_time_value(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M:%S"))
        .ok()
}

fn parse_issue_status_override(value: &str) -> Option<IssueStatus> {
    match value.trim() {
        "即将发行" | "正常发行" | "Upcoming" => Some(IssueStatus::Upcoming),
        "申购中" | "Active" => Some(IssueStatus::Active),
        "延期发行" | "暂缓发行" | "Postponed" => Some(IssueStatus::Postponed),
        "中止发行" | "Suspended" => Some(IssueStatus::Suspended),
        "终止发行" | "Terminated" => Some(IssueStatus::Terminated),
        "发行完成" | "Completed" => Some(IssueStatus::Completed),
        _ => None,
    }
}

fn issue_status_override_text(status: IssueStatus) -> String {
    match status {
        IssueStatus::Upcoming => "Upcoming",
        IssueStatus::Active => "Active",
        IssueStatus::Postponed => "Postponed",
        IssueStatus::Suspended => "Suspended",
        IssueStatus::Terminated => "Terminated",
        IssueStatus::Completed => "Completed",
        IssueStatus::Unknown => "Unknown",
    }
    .to_owned()
}

fn parse_date_value(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn parse_optional_date(value: Option<String>) -> Option<NaiveDate> {
    value.and_then(|v| NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok())
}
pub fn format_date(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}
pub fn format_dt(value: ChinaDateTime) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
pub fn parse_dt(value: &str) -> Result<ChinaDateTime> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&crate::core::china_offset()))
}
fn to_sql_error(error: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
}
fn limit(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

const UPSERT_EVENT_SQL: &str = "INSERT INTO ipo_events(id,exchange,board,security_code,apply_code,legacy_code,name,apply_date,issue_price,lot_size,max_apply_quantity,required_market_value,required_cash,ballot_date,payment_date,listing_date,issue_status,lifecycle_status,event_version,announcement_url,data_quality_status,data_conflict,sessions_json,first_seen_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25) ON CONFLICT(id) DO UPDATE SET exchange=excluded.exchange,board=excluded.board,security_code=excluded.security_code,apply_code=COALESCE(excluded.apply_code,ipo_events.apply_code),legacy_code=COALESCE(excluded.legacy_code,ipo_events.legacy_code),name=excluded.name,apply_date=COALESCE(excluded.apply_date,ipo_events.apply_date),issue_price=COALESCE(excluded.issue_price,ipo_events.issue_price),lot_size=COALESCE(excluded.lot_size,ipo_events.lot_size),max_apply_quantity=COALESCE(excluded.max_apply_quantity,ipo_events.max_apply_quantity),required_market_value=COALESCE(excluded.required_market_value,ipo_events.required_market_value),required_cash=COALESCE(excluded.required_cash,ipo_events.required_cash),ballot_date=COALESCE(excluded.ballot_date,ipo_events.ballot_date),payment_date=COALESCE(excluded.payment_date,ipo_events.payment_date),listing_date=COALESCE(excluded.listing_date,ipo_events.listing_date),issue_status=excluded.issue_status,lifecycle_status=excluded.lifecycle_status,event_version=excluded.event_version,announcement_url=COALESCE(excluded.announcement_url,ipo_events.announcement_url),data_quality_status=excluded.data_quality_status,data_conflict=excluded.data_conflict,sessions_json=excluded.sessions_json,updated_at=excluded.updated_at";

const MIGRATION_SQL: &str = r#"CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY,applied_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS ipo_events(id TEXT PRIMARY KEY,exchange INTEGER NOT NULL,board INTEGER NOT NULL,security_code TEXT NOT NULL,apply_code TEXT NULL,legacy_code TEXT NULL,name TEXT NOT NULL,apply_date TEXT NULL,issue_price NUMERIC NULL,lot_size INTEGER NULL,max_apply_quantity INTEGER NULL,required_market_value NUMERIC NULL,required_cash NUMERIC NULL,ballot_date TEXT NULL,payment_date TEXT NULL,listing_date TEXT NULL,issue_status INTEGER NOT NULL,lifecycle_status INTEGER NOT NULL,event_version INTEGER NOT NULL,announcement_url TEXT NULL,data_quality_status INTEGER NOT NULL,data_conflict INTEGER NOT NULL DEFAULT 0,sessions_json TEXT NOT NULL DEFAULT '[]',first_seen_at TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE INDEX IF NOT EXISTS ix_ipo_events_apply_date ON ipo_events(apply_date);CREATE UNIQUE INDEX IF NOT EXISTS ux_ipo_events_exchange_security ON ipo_events(exchange,security_code);CREATE TABLE IF NOT EXISTS ipo_field_sources(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,field_name TEXT NOT NULL,normalized_value TEXT NULL,raw_value TEXT NULL,source TEXT NOT NULL,source_published_at TEXT NULL,fetched_at TEXT NOT NULL,raw_hash TEXT NULL,priority INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS ix_field_sources_event ON ipo_field_sources(ipo_event_id,field_name);CREATE TABLE IF NOT EXISTS acknowledgements(ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,confirmed_at TEXT NOT NULL,confirmed_data_hash TEXT NOT NULL,needs_review_at TEXT NULL,review_reason TEXT NULL,reconfirmed_at TEXT NULL,revoked_at TEXT NULL,PRIMARY KEY(ipo_event_id,event_version));CREATE TABLE IF NOT EXISTS reminder_outbox(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,due_at TEXT NOT NULL,reminder_level INTEGER NOT NULL,dedupe_key TEXT NOT NULL UNIQUE,lease_until TEXT NULL,delivery_state INTEGER NOT NULL,attempt_count INTEGER NOT NULL DEFAULT 0,last_error TEXT NULL,delivered_at TEXT NULL,acknowledged_at TEXT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE INDEX IF NOT EXISTS ix_outbox_due ON reminder_outbox(delivery_state,due_at);CREATE TABLE IF NOT EXISTS reminder_log(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL,scheduled_at TEXT NOT NULL,shown_at TEXT NOT NULL,reminder_level INTEGER NOT NULL,delivery_channel TEXT NOT NULL,dedupe_key TEXT NOT NULL,result TEXT NOT NULL);CREATE TABLE IF NOT EXISTS raw_payloads(id INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,fetched_at TEXT NOT NULL,success INTEGER NOT NULL,record_count INTEGER NOT NULL,raw_hash TEXT NULL,schema_fingerprint TEXT NULL,payload TEXT NULL,error TEXT NULL);CREATE INDEX IF NOT EXISTS ix_raw_payloads_source_time ON raw_payloads(source,fetched_at DESC);CREATE TABLE IF NOT EXISTS sync_runs(id INTEGER PRIMARY KEY AUTOINCREMENT,source TEXT NOT NULL,started_at TEXT NOT NULL,finished_at TEXT NOT NULL,success INTEGER NOT NULL,record_count INTEGER NOT NULL,error TEXT NULL);CREATE TABLE IF NOT EXISTS source_health(source TEXT PRIMARY KEY,last_attempt_at TEXT NULL,last_success_at TEXT NULL,last_record_count INTEGER NOT NULL DEFAULT 0,schema_fingerprint TEXT NULL,consecutive_failures INTEGER NOT NULL DEFAULT 0,health_state INTEGER NOT NULL,last_error TEXT NULL);CREATE TABLE IF NOT EXISTS source_backoff(source TEXT PRIMARY KEY,failure_count INTEGER NOT NULL DEFAULT 0,next_attempt_at TEXT NULL,last_failure_at TEXT NULL,last_success_at TEXT NULL,last_error TEXT NULL);CREATE TABLE IF NOT EXISTS announcement_documents(id TEXT PRIMARY KEY,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,provider TEXT NOT NULL,announcement_id TEXT NOT NULL,announcement_type TEXT NULL,title TEXT NOT NULL,published_at TEXT NULL,source_url TEXT NOT NULL,local_path TEXT NOT NULL,file_hash TEXT NOT NULL,extraction_status INTEGER NOT NULL,extracted_text_hash TEXT NULL,parser_version TEXT NOT NULL,parsed_fields_json TEXT NOT NULL,downloaded_at TEXT NOT NULL);CREATE UNIQUE INDEX IF NOT EXISTS ux_announcements_provider_id_hash ON announcement_documents(provider,announcement_id,file_hash);CREATE TABLE IF NOT EXISTS manual_overrides(id INTEGER PRIMARY KEY AUTOINCREMENT,ipo_event_id TEXT NOT NULL REFERENCES ipo_events(id) ON DELETE CASCADE,event_version INTEGER NOT NULL,field_name TEXT NOT NULL,override_value TEXT NOT NULL,reason TEXT NOT NULL,announcement_document_id TEXT NULL,created_at TEXT NOT NULL,revoked_at TEXT NULL);CREATE TABLE IF NOT EXISTS app_settings(id INTEGER PRIMARY KEY CHECK(id=1),json_value TEXT NOT NULL,updated_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS app_heartbeat(component TEXT PRIMARY KEY,heartbeat_at TEXT NOT NULL);CREATE TABLE IF NOT EXISTS health_summary_log(summary_date TEXT PRIMARY KEY,sent_at TEXT NOT NULL);INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(1,CURRENT_TIMESTAMP);INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(2,CURRENT_TIMESTAMP);"#;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct TestDatabase {
        root: PathBuf,
        database: Database,
    }
    impl TestDatabase {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("stock-ipo-rust-test-{}", Uuid::new_v4().simple()));
            let database = Database::new(&root);
            database.initialize().unwrap();
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

    #[test]
    fn migration_and_integrity_are_compatible() {
        let test = TestDatabase::new();
        test.database.integrity_check().unwrap();
        assert!(test.database.path().exists());
    }

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
    fn runtime_heartbeats_are_committed_together() {
        let test = TestDatabase::new();
        let now = now_china();

        test.database.touch_runtime_heartbeats(now).unwrap();

        let connection = test.database.open().unwrap();
        let (count, minimum, maximum): (i64, String, String) = connection
            .query_row(
                "SELECT COUNT(*),MIN(heartbeat_at),MAX(heartbeat_at) FROM app_heartbeat WHERE component IN ('scheduler','delivery')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(minimum, format_dt(now));
        assert_eq!(maximum, format_dt(now));
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
    fn backup_is_integrity_checked_and_leaves_no_temporary_file() {
        let test = TestDatabase::new();
        test.database.upsert_event(test.event()).unwrap();
        let backup_directory = test.root.join("backups");

        let path = test.database.backup(&backup_directory).unwrap();

        assert!(path.exists());
        let integrity: String = Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        assert!(
            fs::read_dir(&backup_directory)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn interrupted_backup_commit_preserves_existing_backups_and_cleans_temporary_file() {
        let test = TestDatabase::new();
        test.database.upsert_event(test.event()).unwrap();
        let backup_directory = test.root.join("backups");
        let existing = test.database.backup(&backup_directory).unwrap();

        let error = test
            .database
            .backup_with_commit_hook(&backup_directory, |_| {
                bail!("simulated interruption before atomic commit")
            })
            .unwrap_err();
        assert!(error.to_string().contains("simulated interruption"));
        assert!(existing.exists());
        assert!(
            fs::read_dir(&backup_directory)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.path().extension().is_none_or(|value| value != "tmp"))
        );
    }

    #[test]
    fn overdue_apply_reminders_collapse_to_the_latest_due_level() {
        let test = TestDatabase::new();
        let event = test.database.upsert_event(test.event()).unwrap();
        let date = event.apply_date.unwrap();
        let claim_at = crate::core::at(date, crate::model::time(14, 56));

        let deliveries = test.database.claim_due_at(50, claim_at).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].level, ReminderLevel::Final);
        let summary = test.database.reminder_state_summary().unwrap();
        assert!(summary.collapsed > 0);
        assert_eq!(summary.leased, 1);
    }

    #[test]
    fn local_delivery_failures_use_bounded_backoff_and_expose_error_summary() {
        let test = TestDatabase::new();
        let event = test.database.upsert_event(test.event()).unwrap();
        let date = event.apply_date.unwrap();
        let first_attempt = crate::core::at(date, crate::model::time(14, 56));
        let first = test.database.claim_due_at(50, first_attempt).unwrap();
        assert_eq!(first.len(), 1);
        test.database
            .fail_delivery_at(first[0].outbox_id, "fixture render failure", first_attempt)
            .unwrap();
        assert!(
            test.database
                .claim_due_at(50, first_attempt + chrono::Duration::seconds(59))
                .unwrap()
                .is_empty()
        );

        let second_attempt = first_attempt + chrono::Duration::minutes(1);
        let second = test.database.claim_due_at(50, second_attempt).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].attempt_count, 2);
        test.database
            .fail_delivery_at(
                second[0].outbox_id,
                "fixture render failure again",
                second_attempt,
            )
            .unwrap();
        assert!(
            test.database
                .claim_due_at(50, second_attempt + chrono::Duration::minutes(4))
                .unwrap()
                .is_empty()
        );
        let retry_summary = test.database.reminder_state_summary().unwrap();
        assert_eq!(retry_summary.failed, 1);
        assert!(
            retry_summary
                .latest_error
                .as_deref()
                .is_some_and(|value| value.contains("again"))
        );
        assert_eq!(
            test.database
                .claim_due_at(50, second_attempt + chrono::Duration::minutes(5))
                .unwrap()
                .len(),
            1
        );
        let summary = test.database.reminder_state_summary().unwrap();
        assert_eq!(summary.failed, 0);
        assert!(summary.latest_error.is_none());
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

    #[test]
    fn outbox_recovers_across_reopen_at_queue_lease_display_and_confirmation_stages() {
        let test = TestDatabase::new();
        let event = test.database.upsert_event(test.event()).unwrap();
        let first_claim_at = now_china() - chrono::Duration::minutes(5);

        let reopened = Database::new(&test.root);
        reopened.initialize().unwrap();
        let leased = reopened.claim_due_at(100, first_claim_at).unwrap();
        assert!(!leased.is_empty());

        let before_expiry = Database::new(&test.root);
        before_expiry.initialize().unwrap();
        assert!(
            before_expiry
                .claim_due_at(100, first_claim_at + chrono::Duration::minutes(1))
                .unwrap()
                .is_empty()
        );

        let after_expiry = Database::new(&test.root);
        after_expiry.initialize().unwrap();
        let reclaimed = after_expiry
            .claim_due_at(100, first_claim_at + chrono::Duration::minutes(3))
            .unwrap();
        assert_eq!(reclaimed.len(), leased.len());
        assert!(reclaimed.iter().all(|delivery| delivery.attempt_count == 2));

        let displayed = reclaimed[0].clone();
        after_expiry.complete_delivery(&displayed, "test").unwrap();
        let after_display_crash = Database::new(&test.root);
        after_display_crash.initialize().unwrap();
        let remaining = after_display_crash
            .claim_due_at(100, first_claim_at + chrono::Duration::minutes(6))
            .unwrap();
        assert!(
            remaining
                .iter()
                .all(|delivery| delivery.outbox_id != displayed.outbox_id)
        );

        let mut today_event = after_display_crash.event(&event.id).unwrap().unwrap();
        today_event.apply_date = Some(now_china().date_naive());
        today_event.lifecycle_status = LifecycleStatus::ActiveUnconfirmed;
        today_event.updated_at = now_china();
        let today_event = after_display_crash.upsert_event(today_event).unwrap();
        after_display_crash
            .acknowledge_at(&today_event.id, today_event.event_version, now_china())
            .unwrap();

        let after_confirmation_crash = Database::new(&test.root);
        after_confirmation_crash.initialize().unwrap();
        assert!(
            after_confirmation_crash
                .claim_due_at(100, now_china() + chrono::Duration::minutes(3))
                .unwrap()
                .is_empty()
        );
    }
}

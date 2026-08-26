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
    core::{event_hash, now_china, plan_reminders},
    model::*,
};

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
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;",
        )?;
        Ok(connection)
    }

    pub fn initialize(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.open()?.execute_batch(MIGRATION_SQL)?;
        Ok(())
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
        self.open()?.execute("INSERT INTO app_settings(id,json_value,updated_at) VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET json_value=excluded.json_value,updated_at=excluded.updated_at", params![json, format_dt(now_china())])?;
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
        let existing = transaction
            .query_row(
                "SELECT * FROM ipo_events WHERE id=?1",
                [&event.id],
                map_event,
            )
            .optional()?;
        if let Some(previous) = &existing {
            let critical_changed = previous.apply_code != event.apply_code
                || previous.apply_date != event.apply_date
                || previous.issue_price != event.issue_price
                || previous.status != event.status;
            event.event_version = previous.event_version + i32::from(critical_changed);
            event.first_seen_at = previous.first_seen_at;
            if critical_changed && previous.lifecycle_status == LifecycleStatus::Acknowledged {
                event.lifecycle_status = LifecycleStatus::AcknowledgedNeedsReview;
                transaction.execute("UPDATE acknowledgements SET needs_review_at=?1,review_reason='关键申购字段已变化' WHERE ipo_event_id=?2 AND event_version=?3 AND revoked_at IS NULL", params![format_dt(event.updated_at), event.id, previous.event_version])?;
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
        reconcile_schedule_tx(&transaction, &event, &settings, now_china())?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn replan_all(&self) -> Result<()> {
        let settings = self.settings()?;
        let now = now_china();
        for event in self.events(
            now.date_naive() - chrono::Duration::days(1),
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
        let event = self.event(event_id)?.context("申购任务不存在")?;
        if event.event_version != version {
            bail!("申购数据已更新，请刷新后确认")
        }
        let now = now_china();
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        tx.execute("INSERT INTO acknowledgements(ipo_event_id,event_version,confirmed_at,confirmed_data_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(ipo_event_id,event_version) DO UPDATE SET confirmed_at=excluded.confirmed_at,confirmed_data_hash=excluded.confirmed_data_hash,reconfirmed_at=excluded.confirmed_at,revoked_at=NULL,needs_review_at=NULL,review_reason=NULL",params![event_id,version,format_dt(now),event_hash(&event)])?;
        tx.execute("UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4",params![LifecycleStatus::Acknowledged as i32,format_dt(now),event_id,version])?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=?1,acknowledged_at=?2,updated_at=?2 WHERE ipo_event_id=?3 AND event_version=?4 AND delivery_state IN (0,1,5)",params![DeliveryState::Cancelled as i32,format_dt(now),event_id,version])?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_acknowledgement(&self, event_id: &str, version: i32) -> Result<()> {
        let mut event = self.event(event_id)?.context("申购任务不存在")?;
        if event.event_version != version || event.lifecycle_status != LifecycleStatus::Acknowledged
        {
            bail!("当前没有可撤销的有效确认");
        }
        let settings = self.settings()?;
        let now = now_china();
        let date = event.apply_date.context("任务缺少申购日期")?;
        if now >= crate::core::at(date, crate::core::effective_cutoff(&event, &settings)) {
            bail!("已超过安全截止时间，不能撤销确认");
        }

        event.lifecycle_status = LifecycleStatus::ActiveUnconfirmed;
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
                LifecycleStatus::ActiveUnconfirmed as i32,
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

    pub fn refresh_lifecycle(&self) -> Result<()> {
        let now = now_china();
        let settings = self.settings()?;
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
            }
        }
        self.touch_heartbeat("scheduler", now)
    }

    pub fn claim_due(&self, limit: usize) -> Result<Vec<ReminderDelivery>> {
        let now = now_china();
        let lease = now + chrono::Duration::minutes(2);
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=0,lease_until=NULL,updated_at=?1 WHERE delivery_state=1 AND lease_until<?1",[format_dt(now)])?;
        let ids: Vec<i64> = {
            let mut s=tx.prepare("SELECT id FROM reminder_outbox WHERE delivery_state IN (0,5) AND due_at<=?1 AND (lease_until IS NULL OR lease_until<?1) ORDER BY due_at,id LIMIT ?2")?;
            s.query_map(params![format_dt(now), limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            tx.execute("UPDATE reminder_outbox SET delivery_state=1,lease_until=?1,attempt_count=attempt_count+1,updated_at=?2 WHERE id=?3",params![format_dt(lease),format_dt(now),id])?;
        }
        let mut deliveries = Vec::new();
        for id in ids {
            deliveries.push(tx.query_row("SELECT o.id,o.due_at,o.reminder_level,o.dedupe_key,o.attempt_count,e.* FROM reminder_outbox o JOIN ipo_events e ON e.id=o.ipo_event_id WHERE o.id=?1",[id],map_delivery)?);
        }
        tx.commit()?;
        Ok(deliveries)
    }

    pub fn complete_delivery(&self, delivery: &ReminderDelivery, channel: &str) -> Result<()> {
        let now = now_china();
        let mut c = self.open()?;
        let tx = c.transaction()?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=2,delivered_at=?1,lease_until=NULL,updated_at=?1 WHERE id=?2 AND delivery_state=1",params![format_dt(now),delivery.outbox_id])?;
        tx.execute("INSERT INTO reminder_log(ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,dedupe_key,result) VALUES(?1,?2,?3,?4,?5,?6,'shown')",params![delivery.event.id,format_dt(delivery.due_at),format_dt(now),delivery.level as i32,channel,delivery.dedupe_key])?;
        tx.commit()?;
        Ok(())
    }
    pub fn fail_delivery(&self, id: i64, error: &str) -> Result<()> {
        let retry = now_china() + chrono::Duration::minutes(1);
        self.open()?.execute("UPDATE reminder_outbox SET delivery_state=5,lease_until=?1,last_error=?2,updated_at=?3 WHERE id=?4",params![format_dt(retry),limit(error,1000),format_dt(now_china()),id])?;
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
        if !matches!(
            state,
            HealthState::Healthy | HealthState::Warning | HealthState::Failed
        ) {
            bail!("来源运行状态无效：{state:?}");
        }
        let now = now_china();
        let success = state != HealthState::Failed;
        let limited_raw = raw.map(|value| limit(value, 1_000_000));
        let limited_error = error.map(|value| limit(value, 2000));
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO raw_payloads(source,fetched_at,success,record_count,raw_hash,schema_fingerprint,payload,error) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![source, format_dt(now), i32::from(success), count as i64, hash, schema, limited_raw, limited_error],
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
            let minutes = match failures {
                1 => 1,
                2 => 2,
                3 => 4,
                4 => 8,
                5 => 15,
                _ => 30,
            };
            let next = now + chrono::Duration::minutes(minutes);
            transaction.execute(
                "INSERT INTO source_backoff(source,failure_count,next_attempt_at,last_failure_at,last_error) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(source) DO UPDATE SET failure_count=excluded.failure_count,next_attempt_at=excluded.next_attempt_at,last_failure_at=excluded.last_failure_at,last_error=excluded.last_error",
                params![source, failures, format_dt(next), format_dt(now), limited_error],
            )?;
            Some(next)
        } else {
            transaction.execute(
                "INSERT INTO source_backoff(source,failure_count,next_attempt_at,last_success_at,last_error) VALUES(?1,0,NULL,?2,NULL) ON CONFLICT(source) DO UPDATE SET failure_count=0,next_attempt_at=NULL,last_success_at=excluded.last_success_at,last_error=NULL",
                params![source, format_dt(now)],
            )?;
            None
        };
        transaction.commit()?;
        Ok(next_attempt)
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
    pub fn has_announcement(&self, provider: &str, id: &str, hash: &str) -> Result<bool> {
        Ok(self.open()?.query_row("SELECT EXISTS(SELECT 1 FROM announcement_documents WHERE provider=?1 AND announcement_id=?2 AND file_hash=?3)",params![provider,id,hash],|r|r.get::<_,i32>(0))?!=0)
    }
    pub fn touch_heartbeat(&self, component: &str, now: ChinaDateTime) -> Result<()> {
        self.open()?.execute("INSERT INTO app_heartbeat(component,heartbeat_at) VALUES(?1,?2) ON CONFLICT(component) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",params![component,format_dt(now)])?;
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
        let latest = details
            .sources
            .iter()
            .filter_map(|source| source.last_success_at)
            .max()
            .map(|value| value.format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "尚无成功同步".into());
        Ok((
            details.overall_state,
            format!("数据源失败 {failed} 个，警告 {warning} 个；最近成功：{latest}"),
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
        let active_window = !events.is_empty();
        let sync_minutes = settings.normal_sync_minutes.clamp(5, 7 * 24 * 60) as i64;
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
        let heartbeat_limit = now - chrono::Duration::minutes(3);
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
        {
            HealthState::Failed
        } else if sources
            .iter()
            .any(|source| source.state != HealthState::Healthy)
            || delivery_heartbeat.is_none_or(|value| value < heartbeat_limit)
            || quality_warning
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
            scheduler_heartbeat,
            delivery_heartbeat,
            sources,
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
            "SELECT field_name,raw_value,normalized_value,source,priority,source_published_at,fetched_at,raw_hash FROM ipo_field_sources WHERE ipo_event_id=?1 ORDER BY field_name,priority DESC,fetched_at DESC,id",
        )?;
        let rows = statement.query_map([event_id], |row| {
            let source_published_at: Option<String> = row.get(5)?;
            let fetched_at: String = row.get(6)?;
            Ok(FieldSourceEntry {
                field_name: row.get(0)?,
                raw_value: row.get(1)?,
                normalized_value: row.get(2)?,
                source: row.get(3)?,
                priority: row.get(4)?,
                source_published_at: source_published_at
                    .as_deref()
                    .and_then(|value| parse_dt(value).ok()),
                fetched_at: parse_dt(&fetched_at).map_err(to_sql_error)?,
                raw_hash: row.get(7)?,
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
            "SELECT id,ipo_event_id,event_version,field_name,override_value,reason,announcement_document_id,created_at,revoked_at FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 ORDER BY created_at DESC,id DESC",
        )?;
        let rows = statement.query_map(params![event_id, version], |row| {
            let created_at: String = row.get(7)?;
            let revoked_at: Option<String> = row.get(8)?;
            Ok(ManualOverrideEntry {
                id: row.get(0)?,
                event_id: row.get(1)?,
                event_version: row.get(2)?,
                field_name: row.get(3)?,
                override_value: row.get(4)?,
                reason: row.get(5)?,
                announcement_document_id: row.get(6)?,
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

    pub fn revoke_manual_overrides(&self, event_id: &str, version: i32) -> Result<usize> {
        let count = self.open()?.execute(
            "UPDATE manual_overrides SET revoked_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND revoked_at IS NULL",
            params![format_dt(now_china()), event_id, version],
        )?;
        if let Some(event) = self.event(event_id)? {
            self.reconcile_schedule(&event, &self.settings()?, now_china())?;
        }
        Ok(count)
    }

    pub fn maintenance(&self, data_root: &Path) -> Result<()> {
        let connection = self.open()?;
        connection.execute(
            "DELETE FROM raw_payloads WHERE fetched_at < ?1",
            [format_dt(now_china() - chrono::Duration::days(14))],
        )?;
        connection.execute(
            "DELETE FROM sync_runs WHERE finished_at < ?1",
            [format_dt(now_china() - chrono::Duration::days(90))],
        )?;
        connection.execute(
            "DELETE FROM reminder_log WHERE shown_at < ?1",
            [format_dt(now_china() - chrono::Duration::days(180))],
        )?;
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

    pub fn backup(&self, backup_dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(backup_dir)?;
        let target = backup_dir.join(format!(
            "stock-ipo-reminder-{}.db",
            now_china().format("%Y%m%d-%H%M%S")
        ));
        let source = self.open()?;
        let mut destination = Connection::open(&target)?;
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
            backup.run_to_completion(128, StdDuration::from_millis(50), None)?;
        }
        let result: String = destination.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if result != "ok" {
            bail!("备份完整性检查失败：{result}")
        }
        Ok(target)
    }
}

fn reconcile_schedule_tx(
    tx: &rusqlite::Transaction<'_>,
    event: &IpoEvent,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> Result<()> {
    let planned = plan_reminders(event, settings);
    tx.execute("UPDATE reminder_outbox SET delivery_state=4,updated_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND delivery_state IN (0,1,5)",params![format_dt(now),event.id,event.event_version])?;
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
    let event = map_event_offset(row, 5)?;
    Ok(ReminderDelivery {
        outbox_id: row.get(0)?,
        due_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
        level: ReminderLevel::from_i32(row.get(2)?),
        dedupe_key: row.get(3)?,
        attempt_count: row.get(4)?,
        event,
    })
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
                apply_date: Some(now.date_naive() - chrono::Duration::days(1)),
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
        let event = test.database.upsert_event(test.event()).unwrap();
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
    fn acknowledgement_can_be_revoked_before_cutoff_and_reminders_are_replanned() {
        let test = TestDatabase::new();
        let mut input = test.event();
        input.apply_date = Some(now_china().date_naive() + chrono::Duration::days(1));
        input.lifecycle_status = LifecycleStatus::Scheduled;
        let event = test.database.upsert_event(input).unwrap();

        test.database
            .acknowledge(&event.id, event.event_version)
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

        test.database
            .revoke_acknowledgement(&event.id, event.event_version)
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
            announcement_derived: false,
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
}

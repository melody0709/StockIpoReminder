use super::*;

impl Database {
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
        let connection = self.open()?;
        let current: Option<(i32, Option<String>)> = connection
            .query_row(
                "SELECT health_state,last_error FROM operation_health WHERE component=?1",
                [component],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if current == Some((state as i32, limited_error.clone())) {
            return Ok(());
        }
        connection.execute(
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
                state: HealthState::from_i32_tracked("health_state", row.get(3)?),
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
    pub fn touch_heartbeat(&self, component: &str, now: ChinaDateTime) -> Result<()> {
        self.open()?.execute(
            "INSERT INTO app_heartbeat(component,heartbeat_at) VALUES(?1,?2) ON CONFLICT(component) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",
            params![component, format_dt(now)],
        )?;
        Ok(())
    }

    /// 调度/投递低频心跳的合并写入：单连接单语句更新两个组件，
    /// 供运行时主循环每轮使用。
    pub fn touch_runtime_heartbeats(&self, now: ChinaDateTime) -> Result<()> {
        self.open()?.execute(
            "INSERT INTO app_heartbeat(component,heartbeat_at) VALUES('scheduler',?1),('delivery',?1) ON CONFLICT(component) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",
            params![format_dt(now)],
        )?;
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

    pub fn next_source_retry_at(&self) -> Result<Option<ChinaDateTime>> {
        let value: Option<String> = self.open()?.query_row(
            "SELECT MIN(next_attempt_at) FROM source_backoff
             WHERE next_attempt_at IS NOT NULL
               AND source IN ('eastmoney','sse','cninfo','bse')",
            [],
            |row| row.get(0),
        )?;
        value.map(|value| parse_dt(&value)).transpose()
    }

    pub fn try_claim_source_probe(&self, source: &str, now: ChinaDateTime) -> Result<bool> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        let minimum_stale = chrono::Duration::hours(1);
        let minimum_failed = chrono::Duration::hours(2);
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
                        if active_window && age.is_none_or(|value| value > failed_after) {
                            HealthState::Failed
                        } else {
                            HealthState::Warning
                        }
                    }
                    HealthState::Healthy => {
                        if active_window && age.is_none_or(|value| value > failed_after) {
                            HealthState::Failed
                        } else if active_window && age.is_some_and(|value| value > stale_after) {
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
        let runtime_heartbeat_state = [scheduler_heartbeat, delivery_heartbeat]
            .into_iter()
            .map(|heartbeat| match heartbeat {
                None => HealthState::Failed,
                Some(value)
                    if value
                        <= now - chrono::Duration::minutes(RUNTIME_HEARTBEAT_FAILED_MINUTES) =>
                {
                    HealthState::Failed
                }
                Some(value)
                    if value
                        <= now - chrono::Duration::minutes(RUNTIME_HEARTBEAT_WARNING_MINUTES) =>
                {
                    HealthState::Warning
                }
                Some(_) => HealthState::Healthy,
            })
            .max_by_key(|state| *state as i32)
            .unwrap_or(HealthState::Failed);
        let operations = self.operation_health()?;
        let reminder_state = self.reminder_state_summary()?;
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
            || operations
                .iter()
                .any(|operation| operation.state == HealthState::Failed)
            || runtime_heartbeat_state == HealthState::Failed
            || persistent_delivery_failure
        {
            HealthState::Failed
        } else if sources
            .iter()
            .any(|source| source.state != HealthState::Healthy)
            || quality_warning
            || reminder_state.failed > 0
            || runtime_heartbeat_state == HealthState::Warning
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

    #[cfg(test)]
    pub fn try_mark_health_summary_due(&self, now: ChinaDateTime) -> Result<bool> {
        if now.time() < crate::model::time(8, 0) {
            return Ok(false);
        }
        self.try_mark_health_summary_sent(now.date_naive(), now)
    }

    pub fn health_summary_sent_on(&self, date: NaiveDate) -> Result<bool> {
        Ok(self.open()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM health_summary_log WHERE summary_date=?1)",
            [format_date(date)],
            |row| row.get::<_, i32>(0),
        )? != 0)
    }
}

pub(super) fn source_backoff_delay(
    source: &str,
    failures: i32,
    now: ChinaDateTime,
) -> chrono::Duration {
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

pub(super) fn source_probe_time(now: ChinaDateTime, next_attempt: ChinaDateTime) -> ChinaDateTime {
    (now + chrono::Duration::minutes(10)).min(next_attempt)
}

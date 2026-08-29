use super::*;

impl Database {
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
                        kind: SyncConclusionKind::from_i32_tracked("sync_kind", row.get(0)?),
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
}

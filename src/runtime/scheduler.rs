use super::*;

#[derive(Debug, Clone)]
pub(crate) struct AutomaticSyncSchedule {
    pub(crate) due_at: ChinaDateTime,
    pub(crate) reason: String,
}

pub(crate) fn apply_sync_not_before(
    schedule: &mut AutomaticSyncSchedule,
    not_before: Option<ChinaDateTime>,
) {
    if let Some(not_before) = not_before
        && schedule.due_at < not_before
    {
        schedule.due_at = not_before;
        schedule.reason = "已跳过本次启动同步，等待下一个工作日核验".into();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WakeDeadline {
    pub(crate) at: ChinaDateTime,
    pub(crate) reason: String,
}

impl WakeDeadline {
    pub(crate) fn new(at: ChinaDateTime, reason: String) -> Self {
        Self { at, reason }
    }

    pub(crate) fn consider(&mut self, candidate: Option<ChinaDateTime>, reason: &str) {
        if let Some(candidate) = candidate
            && candidate < self.at
        {
            self.at = candidate;
            self.reason = reason.to_owned();
        }
    }
}

pub(crate) fn duration_until(deadline: ChinaDateTime, now: ChinaDateTime) -> Duration {
    Duration::from_millis((deadline - now).num_milliseconds().max(0) as u64)
}

pub(crate) fn send_ui_event(
    events: &mpsc::Sender<UiEvent>,
    ui_state: &RuntimeUiState,
    event: UiEvent,
) -> std::result::Result<(), mpsc::SendError<UiEvent>> {
    events.send(event)?;
    ui_state.notify();
    Ok(())
}

pub(crate) fn next_health_summary_at(
    settings: &AppSettings,
    snapshot: &RuntimeSnapshot,
    last_health_date: Option<NaiveDate>,
    now: ChinaDateTime,
) -> Option<ChinaDateTime> {
    if !settings.daily_health_summary_enabled
        || last_health_date == Some(now.date_naive())
        || (snapshot.today_count == 0
            && !matches!(
                snapshot.health_state,
                HealthState::Warning | HealthState::Failed
            ))
    {
        return None;
    }
    let today = now.date_naive();
    if !is_workday(today) && snapshot.today_count == 0 {
        return None;
    }
    let anchor = at(today, crate::model::time(8, 0));
    Some(anchor.max(now))
}

#[cfg(test)]
pub(crate) fn automatic_sync_interval_for(settings: &AppSettings, active_day: bool) -> Duration {
    let configured = if active_day {
        settings.active_day_sync_minutes
    } else {
        settings.normal_sync_minutes
    };
    let minutes = configured.clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES) as u64;
    Duration::from_secs(minutes * 60)
}

pub(crate) fn automatic_sync_schedule(
    database: &Database,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> AutomaticSyncSchedule {
    let active_day = has_active_sync_tasks(database, settings, now.date_naive());
    let has_tomorrow_event = has_sync_relevant_events_on(
        database,
        settings,
        now.date_naive() + chrono::Duration::days(1),
    );
    let needs_follow_up_discovery = has_unknown_follow_up_events(database, settings, now);
    let last_sync = database.latest_sync_conclusion().ok().flatten();
    let next_source_retry = database.next_source_retry_at().ok().flatten();
    let identity = database.path().to_string_lossy();
    automatic_sync_schedule_for(
        settings,
        now,
        active_day,
        has_tomorrow_event,
        needs_follow_up_discovery,
        last_sync.as_ref(),
        next_source_retry,
        &identity,
    )
}

pub(crate) fn automatic_sync_schedule_for(
    settings: &AppSettings,
    now: ChinaDateTime,
    active_day: bool,
    has_tomorrow_event: bool,
    needs_follow_up_discovery: bool,
    last_sync: Option<&SyncConclusion>,
    next_source_retry: Option<ChinaDateTime>,
    jitter_identity: &str,
) -> AutomaticSyncSchedule {
    let last_finished = last_sync.map(|value| value.finished_at);
    let last_is_healthy = last_sync.is_some_and(|value| value.kind.is_healthy());
    let mut schedule = if active_day {
        let minutes = settings
            .active_day_sync_minutes
            .clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES);
        let jitter_seconds = sync_jitter_seconds(jitter_identity, now, true);
        let base = last_finished
            .filter(|last| *last <= now)
            .unwrap_or(now - chrono::Duration::minutes(minutes as i64));
        let due_at = normalize_sync_window(
            (base
                + chrono::Duration::minutes(minutes as i64)
                + chrono::Duration::seconds(jitter_seconds))
            .max(now),
            true,
        );
        AutomaticSyncSchedule {
            due_at,
            reason: format!("申购日必要同步（{minutes} 分钟间隔）"),
        }
    } else if last_sync.is_some() && !last_is_healthy {
        let minutes = settings
            .normal_sync_minutes
            .clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES);
        let jitter_seconds = sync_jitter_seconds(jitter_identity, now, false);
        let interval_due = last_finished.filter(|last| *last <= now).unwrap_or(now)
            + chrono::Duration::minutes(minutes as i64)
            + chrono::Duration::seconds(jitter_seconds);
        let retry_due = next_source_retry
            .map(|value| value.min(interval_due))
            .unwrap_or(interval_due)
            .max(now);
        AutomaticSyncSchedule {
            due_at: normalize_sync_window(retry_due, has_tomorrow_event),
            reason: "来源覆盖未恢复，按退避执行必要重试".into(),
        }
    } else {
        let due_at = next_discovery_sync_at(now, last_finished);
        AutomaticSyncSchedule {
            due_at,
            reason: if needs_follow_up_discovery {
                "工作日发现同步（补齐中签、缴款或上市日期）".into()
            } else if last_sync.is_some_and(|value| value.kind == SyncConclusionKind::HealthyEmpty)
            {
                "健康空结果后的下一个工作日核验".into()
            } else {
                "工作日一次发现同步".into()
            },
        }
    };

    if active_day {
        consider_fixed_sync(
            &mut schedule,
            now,
            last_finished,
            at(now.date_naive(), crate::model::time(8, 0)),
            "申购日 08:00 定点跨源核验",
        );
    }
    if has_tomorrow_event {
        consider_fixed_sync(
            &mut schedule,
            now,
            last_finished,
            at(now.date_naive(), crate::model::time(20, 0)),
            "申购日前一日 20:00 定点跨源核验",
        );
    }
    schedule
}

pub(crate) fn consider_fixed_sync(
    schedule: &mut AutomaticSyncSchedule,
    now: ChinaDateTime,
    last_sync: Option<ChinaDateTime>,
    anchor: ChinaDateTime,
    reason: &str,
) {
    if last_sync.is_some_and(|last| last >= anchor) {
        return;
    }
    let (due_at, reason) = if anchor <= now {
        (normalize_sync_window(now, true), format!("补做{reason}"))
    } else {
        (anchor, reason.to_owned())
    };
    if due_at < schedule.due_at {
        schedule.due_at = due_at;
        schedule.reason = reason;
    }
}

pub(crate) fn normalize_sync_window(value: ChinaDateTime, allow_weekend: bool) -> ChinaDateTime {
    let mut normalized = if value.hour() < SYNC_WINDOW_START_HOUR {
        at(
            value.date_naive(),
            crate::model::time(SYNC_WINDOW_START_HOUR, 0),
        )
    } else if value.hour() >= SYNC_WINDOW_END_HOUR {
        at(
            value.date_naive() + chrono::Duration::days(1),
            crate::model::time(SYNC_WINDOW_START_HOUR, 0),
        )
    } else {
        value
    };
    if !allow_weekend && !is_workday(normalized.date_naive()) {
        normalized = next_workday_at(normalized.date_naive(), SYNC_WINDOW_START_HOUR);
    }
    normalized
}

pub(crate) fn in_sync_window(value: ChinaDateTime) -> bool {
    value.hour() >= SYNC_WINDOW_START_HOUR && value.hour() < SYNC_WINDOW_END_HOUR
}

pub(crate) fn next_sync_window_start(value: ChinaDateTime) -> ChinaDateTime {
    normalize_sync_window(value, true)
}

pub(crate) fn sync_jitter_seconds(identity: &str, now: ChinaDateTime, active_day: bool) -> i64 {
    let maximum = if active_day { 20 } else { 90 };
    let seed = sha256(format!(
        "{identity}|{}|{:02}:{:02}|{active_day}",
        now.date_naive(),
        now.hour(),
        now.minute()
    ));
    let value = u64::from_str_radix(&seed[..8], 16).unwrap_or_default();
    (value % (maximum + 1)) as i64
}

pub(crate) fn next_discovery_sync_at(
    now: ChinaDateTime,
    last_sync: Option<ChinaDateTime>,
) -> ChinaDateTime {
    let today = now.date_naive();
    let today_anchor = at(today, crate::model::time(DAILY_DISCOVERY_HOUR, 0));
    if is_workday(today) && last_sync.is_none_or(|last| last.date_naive() < today) {
        return today_anchor.max(now);
    }
    next_workday_at(today, DAILY_DISCOVERY_HOUR)
}

pub(crate) fn next_workday_at(date: NaiveDate, hour: u32) -> ChinaDateTime {
    next_workday_at_time(date, crate::model::time(hour, 0))
}

pub(crate) fn next_workday_at_time(date: NaiveDate, time: chrono::NaiveTime) -> ChinaDateTime {
    let mut candidate = date + chrono::Duration::days(1);
    while !is_workday(candidate) {
        candidate += chrono::Duration::days(1);
    }
    at(candidate, time)
}

pub(crate) fn is_workday(date: NaiveDate) -> bool {
    !matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
}

pub(crate) fn next_maintenance_at(
    now: ChinaDateTime,
    last_maintenance_date: Option<NaiveDate>,
) -> ChinaDateTime {
    let today = now.date_naive();
    let time = crate::model::time(DAILY_MAINTENANCE_HOUR, DAILY_MAINTENANCE_MINUTE);
    if is_workday(today) && last_maintenance_date != Some(today) {
        return at(today, time).max(now);
    }
    next_workday_at_time(today, time)
}

pub(crate) fn has_active_sync_tasks(
    database: &Database,
    settings: &AppSettings,
    date: NaiveDate,
) -> bool {
    has_sync_relevant_events_on(database, settings, date)
}

pub(crate) fn has_sync_relevant_events_on(
    database: &Database,
    settings: &AppSettings,
    date: NaiveDate,
) -> bool {
    database.events(date, date).is_ok_and(|events| {
        events.iter().any(|event| {
            settings.exchange_enabled(event.exchange)
                && matches!(
                    event.lifecycle_status,
                    LifecycleStatus::Discovered
                        | LifecycleStatus::Scheduled
                        | LifecycleStatus::ActiveUnconfirmed
                        | LifecycleStatus::AcknowledgedNeedsReview
                )
        })
    })
}

pub(crate) fn has_unknown_follow_up_events(
    database: &Database,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> bool {
    database
        .events(
            now.date_naive() - chrono::Duration::days(90),
            now.date_naive(),
        )
        .is_ok_and(|events| {
            events.into_iter().any(|event| {
                if !settings.exchange_enabled(event.exchange)
                    || event.lifecycle_status != LifecycleStatus::Acknowledged
                {
                    return false;
                }
                let Some(apply_date) = event.apply_date else {
                    return false;
                };
                let post_apply_missing = settings.post_apply_reminders_enabled
                    && apply_date >= now.date_naive() - chrono::Duration::days(30)
                    && (event.ballot_date.is_none() || event.payment_date.is_none());
                let listing_missing =
                    settings.listing_reminders_enabled && event.listing_date.is_none();
                post_apply_missing || listing_missing
            })
        })
}

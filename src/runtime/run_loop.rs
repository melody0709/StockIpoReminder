use super::*;

pub(crate) fn run_loop(
    database: &Database,
    data_root: &Path,
    initial_reason: Option<SyncRequest>,
    suppress_overdue_initial_sync: bool,
    commands: &mpsc::Receiver<RuntimeCommand>,
    events: &mpsc::Sender<UiEvent>,
    ui_state: &RuntimeUiState,
    stop_requested: &AtomicBool,
) -> Result<()> {
    let client = network::client()?;
    let time_client = network::time_client()?;
    database.save_operation_health("runtime", HealthState::Healthy, None)?;
    let mut requested_reason = initial_reason;
    let mut forced_sync_retry = None::<ChinaDateTime>;
    let mut last_health_date = database
        .health_summary_sent_on(now_china().date_naive())
        .ok()
        .filter(|sent| *sent)
        .map(|_| now_china().date_naive());
    let mut last_maintenance_date = None::<NaiveDate>;
    let initial_now = now_china();
    let initial_settings = database.settings()?;
    let initial_schedule = automatic_sync_schedule(database, &initial_settings, initial_now);
    let mut automatic_sync_not_before = (suppress_overdue_initial_sync
        && initial_schedule.due_at <= initial_now)
        .then(|| next_workday_at(initial_now.date_naive(), DAILY_DISCOVERY_HOUR));
    let mut refresh_requested = true;

    loop {
        if stop_requested.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = now_china();
        database.touch_runtime_heartbeats(now)?;
        // 本轮是否发生过会改变 deadline 基础数据的写入；未写入时
        // deadline 计算直接复用本轮开头读取的结果，避免重复查询。
        let mut did_writes = false;
        let lifecycle_due = database.next_lifecycle_transition_at(now)?;
        if lifecycle_due.is_some_and(|due_at| due_at <= now) {
            if database.refresh_lifecycle()? {
                refresh_requested = true;
            }
            did_writes = true;
        }

        let local_delivery_due = database.next_local_delivery_at()?;
        let secondary_delivery_due = database.next_secondary_delivery_at(now)?;
        if local_delivery_due.is_some_and(|due_at| due_at <= now)
            || secondary_delivery_due.is_some_and(|due_at| due_at <= now)
        {
            if run_delivery_cycle(database, events, ui_state, data_root)? {
                refresh_requested = true;
            }
            did_writes = true;
        }

        let settings = database.settings()?;
        let mut sync_schedule = automatic_sync_schedule(database, &settings, now);
        apply_sync_not_before(&mut sync_schedule, automatic_sync_not_before);
        if let Some(retry_at) = forced_sync_retry {
            sync_schedule = AutomaticSyncSchedule {
                due_at: retry_at,
                reason: "上次同步异常后的必要重试".into(),
            };
        }
        let requested_due = requested_reason
            .as_ref()
            .is_some_and(|request| request.allow_outside_window || in_sync_window(now));
        if requested_due || sync_schedule.due_at <= now {
            let reason = if requested_due {
                requested_reason.take().unwrap().reason
            } else {
                sync_schedule.reason.clone()
            };
            did_writes = true;
            match synchronize(database, &client, ui_state, &reason, stop_requested) {
                Ok(()) => forced_sync_retry = None,
                Err(_) if stop_requested.load(Ordering::Acquire) => return Ok(()),
                Err(error) => {
                    let message = format!("{error:#}");
                    operations::log("ERROR", &format!("同步失败（{reason}）：{message}"));
                    update_snapshot(&ui_state, |value| {
                        value.is_synchronizing = false;
                        value.status_text = "同步失败，继续使用 SQLite 缓存".into();
                        value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
                        value.last_sync_succeeded = Some(false);
                        value.last_error = Some(message.clone());
                    });
                    let retry_minutes = settings
                        .normal_sync_minutes
                        .clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES)
                        as i64;
                    forced_sync_retry = Some(normalize_sync_window(
                        now_china() + chrono::Duration::minutes(retry_minutes),
                        true,
                    ));
                }
            }
            automatic_sync_not_before = None;
            if stop_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            let (clock_state, clock_text) =
                check_clock(&time_client, "同步后的时间校验", Some(stop_requested));
            update_snapshot(ui_state, |value| {
                value.clock_state = clock_state;
                value.clock_text = clock_text;
            });
            refresh_snapshot(database, ui_state);
            #[cfg(windows)]
            crate::windows_integration::trim_working_set();
        }

        let summary_now = now_china();
        let visible_snapshot = ui_state
            .snapshot
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let health_due =
            next_health_summary_at(&settings, &visible_snapshot, last_health_date, summary_now);
        if health_due.is_some_and(|due_at| due_at <= summary_now) {
            let details = database.health_details()?;
            let should_send = details.today_task_count > 0
                || matches!(
                    details.overall_state,
                    HealthState::Warning | HealthState::Failed
                );
            if should_send {
                let (state, text) = database.health_text()?;
                send_ui_event(events, ui_state, UiEvent::Health { state, text })?;
                let date = summary_now.date_naive();
                let _ = database.try_mark_health_summary_sent(date, summary_now)?;
                last_health_date = Some(date);
            }
        }

        let maintenance_now = now_china();
        if next_maintenance_at(maintenance_now, last_maintenance_date) <= maintenance_now {
            run_daily_maintenance(database, data_root);
            last_maintenance_date = Some(maintenance_now.date_naive());
        }

        if refresh_requested {
            refresh_snapshot(database, ui_state);
            refresh_requested = false;
        }

        let now = now_china();
        // deadline 计算：本轮发生过写入时基础数据已过期，必须重算；
        // 否则复用本轮开头读取的三个 next_* 结果与同步计划。
        let (deadline_local, deadline_secondary, deadline_lifecycle) = if did_writes {
            (
                database.next_local_delivery_at()?,
                database.next_secondary_delivery_at(now)?,
                database.next_lifecycle_transition_at(now)?,
            )
        } else {
            (local_delivery_due, secondary_delivery_due, lifecycle_due)
        };
        let mut next_sync = if did_writes {
            let mut schedule = automatic_sync_schedule(database, &settings, now);
            apply_sync_not_before(&mut schedule, automatic_sync_not_before);
            schedule
        } else {
            sync_schedule
        };
        if let Some(retry_at) = forced_sync_retry {
            next_sync = AutomaticSyncSchedule {
                due_at: retry_at,
                reason: "上次同步异常后的必要重试".into(),
            };
        }
        let visible_snapshot = ui_state
            .snapshot
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        let mut deadline = WakeDeadline::new(next_sync.due_at, next_sync.reason);
        if let Some(request) = requested_reason.as_ref()
            && !request.allow_outside_window
            && !in_sync_window(now)
        {
            deadline.consider(Some(next_sync_window_start(now)), "等待自动同步窗口");
        }
        deadline.consider(deadline_local, "本地提醒到期");
        deadline.consider(deadline_secondary, "第二通知通道到期或重试");
        deadline.consider(deadline_lifecycle, "任务生命周期切换");
        deadline.consider(
            next_health_summary_at(&settings, &visible_snapshot, last_health_date, now),
            "每日健康摘要",
        );
        deadline.consider(
            Some(next_maintenance_at(now, last_maintenance_date)),
            "工作日数据库维护",
        );
        update_snapshot(ui_state, |value| {
            value.next_wake_text = format!(
                "后台正常休眠 · 下一次唤醒 {}（{}）",
                deadline.at.format("%Y-%m-%d %H:%M:%S"),
                deadline.reason
            );
        });
        let wait = duration_until(deadline.at, now).min(RUNTIME_HEARTBEAT_INTERVAL);
        match commands.recv_timeout(wait) {
            Ok(RuntimeCommand::Sync(request)) => {
                requested_reason = Some(request);
                while let Ok(command) = commands.try_recv() {
                    match command {
                        RuntimeCommand::Sync(request) => requested_reason = Some(request),
                        RuntimeCommand::Wake | RuntimeCommand::Recovery => refresh_requested = true,
                        RuntimeCommand::Stop => return Ok(()),
                    }
                }
            }
            Ok(RuntimeCommand::Wake) | Ok(RuntimeCommand::Recovery) => refresh_requested = true,
            Ok(RuntimeCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

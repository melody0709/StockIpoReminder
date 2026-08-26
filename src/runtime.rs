use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Timelike, Utc};

use crate::{
    announcement,
    core::{at, group_candidates, now_china, reconcile_candidates, sha256},
    model::{
        AnnouncementDocument, AppSettings, Candidate, ChinaDateTime, DataQualityStatus, Exchange,
        ExtractionStatus, FieldSourceEntry, HealthState, IpoEvent, LifecycleStatus,
        ManualOverrideEntry, ReminderDelivery, SyncConclusion, SyncConclusionKind,
    },
    network::{self, CollectorOutput},
    operations, secondary_notification,
    storage::Database,
    windows_integration,
};

const MINIMUM_SYNC_MINUTES: i32 = 5;
const MAXIMUM_SYNC_MINUTES: i32 = 7 * 24 * 60;
const DELIVERY_INTERVAL: Duration = Duration::from_secs(10);
const CLOCK_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const SYNC_WINDOW_START_HOUR: u32 = 6;
const SYNC_WINDOW_END_HOUR: u32 = 22;

#[derive(Debug, Clone)]
struct AutomaticSyncSchedule {
    due_at: ChinaDateTime,
    reason: String,
}

#[derive(Debug)]
struct AnnouncementRunStats {
    started: ChinaDateTime,
    attempted_events: usize,
    successful_searches: usize,
    references_found: usize,
    documents_processed: usize,
    documents_succeeded: usize,
    mirror_events: usize,
    issue_count: usize,
    issues: Vec<String>,
    retry_after: Option<ChinaDateTime>,
}

impl AnnouncementRunStats {
    fn new(started: ChinaDateTime) -> Self {
        Self {
            started,
            attempted_events: 0,
            successful_searches: 0,
            references_found: 0,
            documents_processed: 0,
            documents_succeeded: 0,
            mirror_events: 0,
            issue_count: 0,
            issues: Vec::new(),
            retry_after: None,
        }
    }

    fn record_issue(&mut self, message: impl Into<String>) {
        self.issue_count += 1;
        if self.issues.len() < 3 {
            self.issues
                .push(message.into().chars().take(700).collect::<String>());
        }
    }

    fn observe_retry_after(&mut self, value: Option<ChinaDateTime>) {
        let Some(value) = value else { return };
        self.retry_after = Some(match self.retry_after {
            Some(current) => current.max(value),
            None => value,
        });
    }

    fn state(&self) -> HealthState {
        if self.successful_searches == 0
            || (self.references_found > 0 && self.documents_succeeded == 0 && self.issue_count > 0)
        {
            HealthState::Failed
        } else if self.issue_count > 0 {
            HealthState::Warning
        } else {
            HealthState::Healthy
        }
    }

    fn summary(&self) -> String {
        format!(
            "attemptedEvents={}, searchSuccesses={}, references={}, documents={}, successfulDocuments={}, mirrorEvents={}, issues={}",
            self.attempted_events,
            self.successful_searches,
            self.references_found,
            self.documents_processed,
            self.documents_succeeded,
            self.mirror_events,
            self.issue_count
        )
    }

    fn error_summary(&self) -> Option<String> {
        (self.issue_count > 0).then(|| {
            format!(
                "公告源部分或全部失败：{}；{}",
                self.summary(),
                self.issues.join("；")
            )
        })
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub is_synchronizing: bool,
    pub status_text: String,
    pub last_sync_text: String,
    pub last_sync_succeeded: Option<bool>,
    pub health_text: String,
    pub health_state: HealthState,
    pub clock_text: String,
    pub clock_state: HealthState,
    pub pending_count: i64,
    pub today_count: usize,
    pub last_error: Option<String>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            is_synchronizing: true,
            status_text: "正在后台准备本地数据库…".into(),
            last_sync_text: "尚未同步".into(),
            last_sync_succeeded: None,
            health_text: "正在读取健康状态…".into(),
            health_state: HealthState::Unknown,
            clock_text: "系统时间尚未检查".into(),
            clock_state: HealthState::Unknown,
            pending_count: 0,
            today_count: 0,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Reminder(ReminderDelivery),
    Health { state: HealthState, text: String },
}

enum RuntimeCommand {
    Sync(String),
    Wake,
    Recovery,
    Stop,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    database: Database,
    command_sender: mpsc::Sender<RuntimeCommand>,
    event_receiver: Arc<Mutex<mpsc::Receiver<UiEvent>>>,
    snapshot: Arc<RwLock<RuntimeSnapshot>>,
    ready: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
}

impl RuntimeHandle {
    pub fn request_sync(&self, reason: impl Into<String>) {
        let _ = self
            .command_sender
            .send(RuntimeCommand::Sync(reason.into()));
    }
    pub fn wake(&self) {
        let _ = self.command_sender.send(RuntimeCommand::Wake);
    }
    pub fn recovery(&self) {
        let _ = self.command_sender.send(RuntimeCommand::Recovery);
    }
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
        let _ = self.command_sender.send(RuntimeCommand::Stop);
    }
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
    pub fn try_event(&self) -> Option<UiEvent> {
        self.event_receiver.lock().ok()?.try_recv().ok()
    }
    pub fn complete_delivery(&self, delivery: &ReminderDelivery) -> Result<()> {
        self.ensure_ready()?;
        self.database.complete_delivery(delivery, "slint+tray")
    }
    pub fn fail_delivery(&self, delivery: &ReminderDelivery, error: &str) -> Result<()> {
        self.ensure_ready()?;
        self.database.fail_delivery(delivery.outbox_id, error)
    }
    pub fn settings(&self) -> Result<AppSettings> {
        self.ensure_ready()?;
        self.database.settings()
    }
    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.ensure_ready()?;
        self.database.save_settings(settings)?;
        self.database.replan_all()?;
        Ok(())
    }
    pub fn reserve_secondary_notification_test(
        &self,
        provider: crate::model::SecondaryNotificationProvider,
    ) -> Result<Option<i64>> {
        self.ensure_ready()?;
        self.database.reserve_secondary_notification_test(provider)
    }
    pub fn finish_secondary_notification_test(
        &self,
        attempt_id: i64,
        error: Option<&str>,
    ) -> Result<()> {
        self.ensure_ready()?;
        self.database
            .finish_secondary_notification_test(attempt_id, error)
    }
    pub fn secondary_notification_summary(
        &self,
    ) -> Result<crate::model::SecondaryNotificationSummary> {
        self.ensure_ready()?;
        self.database.secondary_notification_summary()
    }
    pub fn today_events(&self) -> Result<Vec<IpoEvent>> {
        self.ensure_ready()?;
        self.database.today_events()
    }
    pub fn future_events(&self) -> Result<Vec<IpoEvent>> {
        self.ensure_ready()?;
        self.database.future_events(60)
    }
    pub fn health_details(&self) -> Result<crate::model::HealthDetails> {
        self.ensure_ready()?;
        self.database.health_details()
    }
    pub fn event(&self, id: &str) -> Result<Option<IpoEvent>> {
        self.ensure_ready()?;
        self.database.event(id)
    }
    pub fn announcement_titles(&self, id: &str) -> Result<Vec<String>> {
        self.ensure_ready()?;
        self.database.announcement_titles(id)
    }
    pub fn field_sources(&self, id: &str) -> Result<Vec<FieldSourceEntry>> {
        self.ensure_ready()?;
        self.database.field_sources(id)
    }
    pub fn announcements(&self, id: &str) -> Result<Vec<AnnouncementDocument>> {
        self.ensure_ready()?;
        self.database.announcements(id)
    }
    pub fn manual_overrides(&self, id: &str, version: i32) -> Result<Vec<ManualOverrideEntry>> {
        self.ensure_ready()?;
        self.database.manual_overrides(id, version)
    }
    pub fn acknowledge(&self, id: &str, version: i32) -> Result<()> {
        self.ensure_ready()?;
        self.database.acknowledge(id, version)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_acknowledgement(&self, id: &str, version: i32) -> Result<()> {
        self.ensure_ready()?;
        self.database.revoke_acknowledgement(id, version)?;
        self.wake();
        Ok(())
    }
    pub fn apply_override(
        &self,
        id: &str,
        version: i32,
        field: &str,
        value: &str,
        reason: &str,
        announcement_id: Option<&str>,
    ) -> Result<()> {
        self.ensure_ready()?;
        self.database
            .apply_manual_override(id, version, field, value, reason, announcement_id)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_override(&self, id: &str, version: i32, override_id: i64) -> Result<()> {
        self.ensure_ready()?;
        self.database
            .revoke_manual_override(id, version, override_id)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_overrides(&self, id: &str, version: i32) -> Result<usize> {
        self.ensure_ready()?;
        let count = self.database.revoke_manual_overrides(id, version)?;
        self.wake();
        Ok(count)
    }
    pub fn database(&self) -> Result<&Database> {
        self.ensure_ready()?;
        Ok(&self.database)
    }

    fn ensure_ready(&self) -> Result<()> {
        if self.is_ready() {
            Ok(())
        } else {
            anyhow::bail!("本地数据库仍在后台初始化，请稍候")
        }
    }
}

pub fn start(data_root: PathBuf, startup_sync: bool) -> Result<(RuntimeHandle, JoinHandle<()>)> {
    start_with_initializer(data_root, startup_sync, |database, data_root| {
        let _upgrade_backup =
            operations::prepare_database_upgrade(data_root, env!("CARGO_PKG_VERSION"))?;
        database.initialize()?;
        match database.compact_if_needed() {
            Ok(true) => operations::log("INFO", "已回收历史接口正文释放的 SQLite 空闲空间"),
            Ok(false) => {}
            Err(error) => operations::log("WARN", &format!("SQLite 空闲空间回收跳过：{error:#}")),
        }
        database.integrity_check()?;
        operations::mark_database_version(data_root, env!("CARGO_PKG_VERSION"))?;
        Ok(())
    })
}

fn start_with_initializer<F>(
    data_root: PathBuf,
    startup_sync: bool,
    initialize: F,
) -> Result<(RuntimeHandle, JoinHandle<()>)>
where
    F: FnOnce(&Database, &Path) -> Result<()> + Send + 'static,
{
    let database = Database::new(&data_root);
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let snapshot = Arc::new(RwLock::new(RuntimeSnapshot::default()));
    let ready = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let handle = RuntimeHandle {
        database: database.clone(),
        command_sender,
        event_receiver: Arc::new(Mutex::new(event_receiver)),
        snapshot: Arc::clone(&snapshot),
        ready: Arc::clone(&ready),
        stop_requested: Arc::clone(&stop_requested),
    };
    let thread = thread::Builder::new()
        .name("stock-ipo-runtime".into())
        .spawn(move || {
            if let Err(error) = initialize(&database, &data_root) {
                crate::operations::log("ERROR", &format!("本地数据库初始化失败：{error:#}"));
                update_snapshot(&snapshot, |value| {
                    value.is_synchronizing = false;
                    value.status_text = "本地数据库初始化失败，请查看日志".into();
                    value.health_text = "后台提醒服务未启动".into();
                    value.health_state = HealthState::Failed;
                    value.last_error = Some(format!("{error:#}"));
                });
                return;
            }
            ready.store(true, Ordering::Release);
            update_snapshot(&snapshot, |value| {
                value.is_synchronizing = false;
                value.status_text = "本地数据已就绪，正在启动后台提醒服务…".into();
            });
            if stop_requested.load(Ordering::Acquire) {
                return;
            }
            if let Err(error) = run_loop(
                database,
                data_root,
                startup_sync,
                command_receiver,
                event_sender,
                snapshot.clone(),
            ) {
                crate::operations::log("ERROR", &format!("后台运行时异常：{error:#}"));
                update_snapshot(&snapshot, |value| {
                    value.is_synchronizing = false;
                    value.status_text = "后台运行时异常，继续使用本地数据".into();
                    value.last_error = Some(format!("{error:#}"));
                });
            }
        })?;
    Ok((handle, thread))
}

fn run_loop(
    database: Database,
    data_root: PathBuf,
    startup_sync: bool,
    commands: mpsc::Receiver<RuntimeCommand>,
    events: mpsc::Sender<UiEvent>,
    snapshot: Arc<RwLock<RuntimeSnapshot>>,
) -> Result<()> {
    let client = network::client()?;
    let initial_schedule = automatic_sync_schedule(&database, now_china());
    let mut next_sync = if startup_sync {
        Instant::now()
    } else {
        schedule_instant(&initial_schedule, now_china())
    };
    let mut next_sync_reason = initial_schedule.reason;
    let mut next_delivery = Instant::now();
    let mut next_clock = Instant::now();
    let mut next_maintenance = Instant::now();
    let mut last_health_date = None;
    let mut requested_reason = startup_sync.then(|| "程序启动".to_owned());
    refresh_snapshot(&database, &snapshot);

    loop {
        let now = Instant::now();
        if now >= next_sync || requested_reason.is_some() {
            let reason = requested_reason
                .take()
                .unwrap_or_else(|| next_sync_reason.clone());
            if let Err(error) = synchronize(&database, &client, &data_root, &snapshot, &reason) {
                let message = format!("{error:#}");
                operations::log("ERROR", &format!("同步失败（{reason}）：{message}"));
                update_snapshot(&snapshot, |value| {
                    value.is_synchronizing = false;
                    value.status_text = "同步失败，继续使用 SQLite 缓存".into();
                    value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
                    value.last_sync_succeeded = Some(false);
                    value.last_error = Some(message.clone());
                });
            }
            let schedule = automatic_sync_schedule(&database, now_china());
            next_sync = schedule_instant(&schedule, now_china());
            next_sync_reason = schedule.reason;
            refresh_snapshot(&database, &snapshot);
            #[cfg(windows)]
            crate::windows_integration::trim_working_set();
        }
        if Instant::now() >= next_delivery {
            let _ = run_delivery_cycle(&database, &events, &snapshot, &data_root);
            let china_now = now_china();
            if last_health_date != Some(china_now.date_naive())
                && let Ok(settings) = database.settings()
                && settings.daily_health_summary_enabled
            {
                match database.try_mark_health_summary_due(china_now) {
                    Ok(should_send) => {
                        if should_send && let Ok((state, text)) = database.health_text() {
                            let _ = events.send(UiEvent::Health { state, text });
                        }
                        if china_now.time() >= crate::model::time(8, 0) {
                            last_health_date = Some(china_now.date_naive());
                        }
                    }
                    Err(error) => operations::log(
                        "ERROR",
                        &format!("每日健康摘要去重状态写入失败：{error:#}"),
                    ),
                }
            }
            next_delivery = Instant::now() + DELIVERY_INTERVAL;
        }

        if Instant::now() >= next_maintenance {
            run_daily_maintenance(&database, &data_root);
            next_maintenance = Instant::now() + Duration::from_secs(60 * 60);
        }

        if Instant::now() >= next_clock {
            let (state, text) = check_clock(&client, "周期或恢复检查");
            update_snapshot(&snapshot, |value| {
                value.clock_state = state;
                value.clock_text = text;
            });
            next_clock = Instant::now() + CLOCK_CHECK_INTERVAL;
        }

        let wait = next_sync
            .min(next_delivery)
            .min(next_clock)
            .min(next_maintenance)
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(10));
        match commands.recv_timeout(wait) {
            Ok(RuntimeCommand::Sync(reason)) => {
                requested_reason = Some(reason);
                while let Ok(command) = commands.try_recv() {
                    match command {
                        RuntimeCommand::Sync(reason) => requested_reason = Some(reason),
                        RuntimeCommand::Wake => next_delivery = Instant::now(),
                        RuntimeCommand::Recovery => {
                            next_delivery = Instant::now();
                            next_clock = Instant::now();
                        }
                        RuntimeCommand::Stop => return Ok(()),
                    }
                }
            }
            Ok(RuntimeCommand::Wake) => next_delivery = Instant::now(),
            Ok(RuntimeCommand::Recovery) => {
                next_delivery = Instant::now();
                next_clock = Instant::now();
            }
            Ok(RuntimeCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn automatic_sync_interval_for(settings: &AppSettings, active_day: bool) -> Duration {
    let configured = if active_day {
        settings.active_day_sync_minutes
    } else {
        settings.normal_sync_minutes
    };
    let minutes = configured.clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES) as u64;
    Duration::from_secs(minutes * 60)
}

fn automatic_sync_schedule(database: &Database, now: ChinaDateTime) -> AutomaticSyncSchedule {
    let settings = database.settings().unwrap_or_default();
    let active_day = has_active_sync_tasks(database, &settings);
    let has_tomorrow_event = has_sync_relevant_events_on(
        database,
        &settings,
        now.date_naive() + chrono::Duration::days(1),
    );
    let last_sync = database
        .latest_sync_conclusion()
        .ok()
        .flatten()
        .map(|conclusion| conclusion.finished_at);
    let identity = database.path().to_string_lossy();
    automatic_sync_schedule_for(
        &settings,
        now,
        active_day,
        has_tomorrow_event,
        last_sync,
        &identity,
    )
}

fn automatic_sync_schedule_for(
    settings: &AppSettings,
    now: ChinaDateTime,
    active_day: bool,
    has_tomorrow_event: bool,
    last_sync: Option<ChinaDateTime>,
    jitter_identity: &str,
) -> AutomaticSyncSchedule {
    let minutes = if active_day {
        settings.active_day_sync_minutes
    } else {
        settings.normal_sync_minutes
    }
    .clamp(MINIMUM_SYNC_MINUTES, MAXIMUM_SYNC_MINUTES);
    let interval_text = if minutes % 60 == 0 {
        format!("每 {} 小时", minutes / 60)
    } else {
        format!("每 {minutes} 分钟")
    };
    let jitter_seconds = sync_jitter_seconds(jitter_identity, now, active_day);
    let interval_due = normalize_sync_window(
        now + chrono::Duration::minutes(minutes as i64) + chrono::Duration::seconds(jitter_seconds),
    );
    let mut schedule = AutomaticSyncSchedule {
        due_at: interval_due,
        reason: if active_day {
            format!("申购日自动同步（{interval_text}，抖动 {jitter_seconds} 秒）")
        } else {
            format!("常规自动同步（{interval_text}，抖动 {jitter_seconds} 秒）")
        },
    };

    if active_day {
        consider_fixed_sync(
            &mut schedule,
            now,
            last_sync,
            at(now.date_naive(), crate::model::time(8, 0)),
            "申购日 08:00 定点跨源核验",
        );
    }
    if has_tomorrow_event {
        consider_fixed_sync(
            &mut schedule,
            now,
            last_sync,
            at(now.date_naive(), crate::model::time(20, 0)),
            "申购日前一日 20:00 定点跨源核验",
        );
    }
    schedule
}

fn consider_fixed_sync(
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
        (now, format!("补做{reason}"))
    } else {
        (anchor, reason.to_owned())
    };
    if due_at < schedule.due_at {
        schedule.due_at = due_at;
        schedule.reason = reason;
    }
}

fn normalize_sync_window(value: ChinaDateTime) -> ChinaDateTime {
    if value.hour() < SYNC_WINDOW_START_HOUR {
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
    }
}

fn sync_jitter_seconds(identity: &str, now: ChinaDateTime, active_day: bool) -> i64 {
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

fn schedule_instant(schedule: &AutomaticSyncSchedule, now: ChinaDateTime) -> Instant {
    let milliseconds = (schedule.due_at - now).num_milliseconds().max(0) as u64;
    Instant::now() + Duration::from_millis(milliseconds)
}

fn has_active_sync_tasks(database: &Database, settings: &AppSettings) -> bool {
    has_sync_relevant_events_on(database, settings, now_china().date_naive())
}

fn has_sync_relevant_events_on(
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
                        | LifecycleStatus::Acknowledged
                        | LifecycleStatus::AcknowledgedNeedsReview
                )
        })
    })
}

fn synchronize(
    database: &Database,
    client: &reqwest::blocking::Client,
    data_root: &Path,
    snapshot: &Arc<RwLock<RuntimeSnapshot>>,
    reason: &str,
) -> Result<()> {
    update_snapshot(snapshot, |value| {
        value.is_synchronizing = true;
        value.status_text = format!("正在同步：{reason}");
        value.last_error = None;
    });
    let started = now_china();
    let settings = database.settings()?;
    let mut candidates = Vec::<Candidate>::new();
    let collectors: [(
        &str,
        fn(&reqwest::blocking::Client) -> Result<CollectorOutput>,
    ); 4] = [
        ("eastmoney", network::collect_eastmoney),
        ("sse", network::collect_sse),
        ("cninfo", network::collect_cninfo),
        ("bse", network::collect_bse),
    ];
    let mut successful_sources = 0usize;
    let mut failed_sources = 0usize;
    let mut successful_source_names = HashSet::<&'static str>::new();
    for (source, collect) in collectors {
        let now = now_china();
        if !database.source_can_attempt(source, now)?.0 {
            probe_backed_off_source(database, client, source, now)?;
            continue;
        }
        match collect(client) {
            Ok(output) => {
                let record_count = output.candidates.len();
                let audit_state = output.audit.state();
                let audit_summary = output.audit.summary();
                database.save_source_run(
                    output.source,
                    output.started,
                    audit_state,
                    record_count,
                    Some(&output.raw),
                    Some(&output.hash),
                    Some(&output.schema),
                    audit_summary.as_deref(),
                )?;
                candidates.extend(output.candidates);
                successful_sources += 1;
                if audit_state == HealthState::Healthy {
                    successful_source_names.insert(output.source);
                    operations::log(
                        "INFO",
                        &format!("数据源 {} 同步成功，{} 条记录", output.source, record_count),
                    );
                } else {
                    operations::log(
                        "WARN",
                        &format!(
                            "数据源 {} 响应可解析但覆盖不完整，{} 条记录：{}",
                            output.source,
                            record_count,
                            audit_summary.as_deref().unwrap_or("计数/明细核验异常")
                        ),
                    );
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                database.save_source_run_with_retry_after(
                    source,
                    now,
                    HealthState::Failed,
                    0,
                    None,
                    None,
                    None,
                    Some(&message),
                    network::retry_after_from_error(&error),
                )?;
                failed_sources += 1;
                operations::log("ERROR", &format!("数据源 {source} 同步失败：{message}"));
            }
        }
    }
    if successful_sources == 0 && candidates.is_empty() {
        let reason_text = if failed_sources == 0 {
            "所有来源都在退避期内"
        } else {
            "所有数据源同步失败"
        };
        let today_count = database
            .today_events()
            .map(|events| {
                events
                    .into_iter()
                    .filter(|event| settings.exchange_enabled(event.exchange))
                    .count()
            })
            .unwrap_or_default();
        let missing_sources = missing_required_sources(&settings, &successful_source_names);
        let conclusion = sync_conclusion(
            started,
            now_china(),
            today_count,
            0,
            0,
            &successful_source_names,
            &missing_sources,
        );
        let message = format!("{reason_text}；{}", conclusion.summary);
        update_snapshot(snapshot, |value| {
            value.is_synchronizing = false;
            value.status_text = format!("{message}，继续使用 SQLite 缓存");
            value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
            value.last_sync_succeeded = Some(false);
            value.last_error = Some(message.clone());
        });
        database.save_sync_conclusion(&conclusion)?;
        return Ok(());
    }

    let today = now_china().date_naive();
    candidates.retain(|candidate| {
        settings.exchange_enabled(candidate.exchange) && candidate.stable_identity().is_some()
    });
    let groups: Vec<Vec<Candidate>> = group_candidates(candidates)
        .into_values()
        .filter(|group| {
            group.iter().any(|candidate| {
                candidate
                    .apply_date
                    .is_some_and(|date| date >= today - chrono::Duration::days(30))
                    || matches!(
                        candidate.status,
                        crate::model::IssueStatus::Upcoming | crate::model::IssueStatus::Active
                    )
            })
        })
        .collect();
    let candidate_count: usize = groups.iter().map(Vec::len).sum();
    operations::log(
        "INFO",
        &format!(
            "近期候选过滤完成：candidates={candidate_count}, groups={}",
            groups.len()
        ),
    );
    let mut event_count = 0usize;
    let mut announcement_count = 0usize;
    let mut announcement_attempts = HashMap::<&'static str, bool>::new();
    let mut announcement_runs = HashMap::<&'static str, AnnouncementRunStats>::new();
    for group in groups {
        let identity = group.first().and_then(Candidate::stable_identity);
        let existing = identity
            .as_deref()
            .and_then(|id| database.event(id).ok().flatten());
        let Some(mut provisional) =
            reconcile_candidates(&group, existing.as_ref(), &settings, now_china())
        else {
            continue;
        };
        let mut combined = group;
        let mut documents = Vec::new();
        let existing_official_evidence = existing.as_ref().is_some_and(|event| {
            event.data_quality_status == DataQualityStatus::AnnouncementVerified
        });
        if should_check_announcements(&provisional) {
            let provider = match provisional.exchange {
                Exchange::Shanghai => "sse-announcement",
                Exchange::Shenzhen => "cninfo-announcement",
                Exchange::Beijing => "bse-announcement",
                _ => "announcement",
            };
            let now = now_china();
            let can_attempt = if let Some(can_attempt) = announcement_attempts.get(provider) {
                *can_attempt
            } else {
                let can_attempt = database.source_can_attempt(provider, now)?.0;
                if !can_attempt {
                    probe_backed_off_source(database, client, provider, now)?;
                }
                announcement_attempts.insert(provider, can_attempt);
                can_attempt
            };
            if can_attempt {
                let stats = announcement_runs
                    .entry(provider)
                    .or_insert_with(|| AnnouncementRunStats::new(now));
                stats.attempted_events += 1;
                let from =
                    provisional.apply_date.unwrap_or(now.date_naive()) - chrono::Duration::days(14);
                let to =
                    provisional.apply_date.unwrap_or(now.date_naive()) + chrono::Duration::days(7);
                match announcement::search(client, &provisional, from, to) {
                    Ok(output) => {
                        stats.successful_searches += 1;
                        stats.references_found += output.references.len();
                        if output.used_mirror {
                            stats.mirror_events += 1;
                        }
                        if let Some(warning) = output.warning {
                            stats.record_issue(format!("event={}：{warning}", provisional.id));
                            operations::log(
                                "WARN",
                                &format!(
                                    "公告来源降级：event={}, provider={provider}, warning={warning}",
                                    provisional.id
                                ),
                            );
                        }
                        let mut usable_official_evidence = existing_official_evidence;
                        let mut event_successful_documents = 0usize;
                        let mut event_document_issues = Vec::new();
                        for reference in output.references.into_iter().take(5) {
                            let reference_title = reference.title.clone();
                            match announcement::download_and_parse(
                                client,
                                data_root,
                                &provisional,
                                reference,
                            ) {
                                Ok((document, candidate)) => {
                                    stats.documents_processed += 1;
                                    if document.status == ExtractionStatus::Failed {
                                        event_document_issues.push(format!(
                                            "{}：公告正文未能提取",
                                            document.reference.title
                                        ));
                                    } else {
                                        stats.documents_succeeded += 1;
                                        event_successful_documents += 1;
                                    }
                                    if let Some(candidate) = candidate {
                                        usable_official_evidence = true;
                                        combined.push(candidate);
                                        documents.push(document);
                                        break;
                                    }
                                    documents.push(document);
                                }
                                Err(error) => {
                                    stats.observe_retry_after(network::retry_after_from_error(
                                        &error,
                                    ));
                                    event_document_issues
                                        .push(format!("{reference_title}：{error:#}"));
                                }
                            }
                        }
                        if !event_document_issues.is_empty() {
                            let health_relevant = event_successful_documents == 0;
                            let detail = event_document_issues
                                .iter()
                                .take(2)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("；");
                            if health_relevant {
                                stats.record_issue(format!("event={}：{detail}", provisional.id));
                            }
                            operations::log(
                                if health_relevant { "ERROR" } else { "WARN" },
                                &format!(
                                    "公告文档处理{}：event={}, provider={provider}, issues={}",
                                    if health_relevant {
                                        "失败"
                                    } else {
                                        "部分跳过（已有可用正式公告）"
                                    },
                                    provisional.id,
                                    event_document_issues.len(),
                                ),
                            );
                        }
                        if requires_official_evidence(&provisional) && !usable_official_evidence {
                            provisional.data_quality_status =
                                DataQualityStatus::ManualReviewRequired;
                        }
                    }
                    Err(error) => {
                        stats.observe_retry_after(network::retry_after_from_error(&error));
                        stats.record_issue(format!("event={}：{error:#}", provisional.id));
                        operations::log(
                            "ERROR",
                            &format!(
                                "公告检索失败：event={}, provider={provider}, error={error:#}",
                                provisional.id
                            ),
                        );
                        if requires_official_evidence(&provisional) && !existing_official_evidence {
                            provisional.data_quality_status =
                                DataQualityStatus::ManualReviewRequired;
                        }
                    }
                }
            } else if requires_official_evidence(&provisional) && !existing_official_evidence {
                provisional.data_quality_status = DataQualityStatus::ManualReviewRequired;
            }
        }
        let mut resolved =
            reconcile_candidates(&combined, existing.as_ref(), &settings, now_china())
                .unwrap_or(provisional.clone());
        resolved.data_quality_status = final_data_quality(
            resolved.data_quality_status,
            provisional.data_quality_status,
            existing_official_evidence,
        );
        persist_reconciled_group(database, resolved, &combined, &documents)?;
        announcement_count += documents.len();
        event_count += 1;
    }
    for provider in [
        "sse-announcement",
        "cninfo-announcement",
        "bse-announcement",
    ] {
        let Some(stats) = announcement_runs.remove(provider) else {
            continue;
        };
        let state = stats.state();
        let summary = stats.summary();
        let error = stats.error_summary();
        database.save_source_run_with_retry_after(
            provider,
            stats.started,
            state,
            stats.documents_processed,
            Some(&summary),
            None,
            Some("announcement-run-v2"),
            error.as_deref(),
            stats.retry_after,
        )?;
        operations::log(
            if state == HealthState::Failed {
                "ERROR"
            } else if state == HealthState::Warning {
                "WARN"
            } else {
                "INFO"
            },
            &format!("公告源 {provider} 本轮状态 {state:?}：{summary}"),
        );
    }
    database.touch_heartbeat("synchronization", now_china())?;
    let today_count = database
        .today_events()?
        .into_iter()
        .filter(|event| settings.exchange_enabled(event.exchange))
        .count();
    let missing_sources = missing_required_sources(&settings, &successful_source_names);
    let conclusion = sync_conclusion(
        started,
        now_china(),
        today_count,
        event_count,
        announcement_count,
        &successful_source_names,
        &missing_sources,
    );
    database.save_sync_conclusion(&conclusion)?;
    update_snapshot(snapshot, |value| {
        value.is_synchronizing = false;
        value.status_text = conclusion.summary.clone();
        value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
        value.last_sync_succeeded = Some(conclusion.kind.is_healthy());
        value.last_error = (!conclusion.kind.is_healthy())
            .then(|| format!("启用市场来源覆盖不完整：{}", missing_sources.join("、")));
    });
    operations::log(
        match conclusion.kind.health_state() {
            HealthState::Healthy => "INFO",
            HealthState::Warning => "WARN",
            _ => "ERROR",
        },
        &format!(
            "{}；conclusion={:?}, candidates={candidate_count}, events={event_count}, announcements={announcement_count}, sources={successful_sources}, failed={failed_sources}",
            conclusion.summary, conclusion.kind
        ),
    );
    Ok(())
}

fn missing_required_sources(
    settings: &AppSettings,
    successful_sources: &HashSet<&'static str>,
) -> Vec<&'static str> {
    let mut required = vec!["eastmoney"];
    if settings.shanghai_enabled {
        required.push("sse");
    }
    if settings.shenzhen_enabled {
        required.push("cninfo");
    }
    if settings.beijing_enabled {
        required.push("bse");
    }
    required
        .into_iter()
        .filter(|source| !successful_sources.contains(source))
        .collect()
}

fn probe_backed_off_source(
    database: &Database,
    client: &reqwest::blocking::Client,
    source: &str,
    now: ChinaDateTime,
) -> Result<()> {
    if !database.try_claim_source_probe(source, now)? {
        return Ok(());
    }
    let started_at = now_china();
    match network::probe_source(client, source) {
        Ok(()) => {
            database.save_source_probe_run(source, started_at, true, None)?;
            operations::log(
                "INFO",
                &format!("退避期低频健康探测成功：source={source}；保留原 API 退避"),
            );
        }
        Err(error) => {
            let message = format!("{error:#}");
            database.save_source_probe_run(source, started_at, false, Some(&message))?;
            operations::log(
                "WARN",
                &format!("退避期低频健康探测失败：source={source}，error={message}"),
            );
        }
    }
    Ok(())
}

fn sync_conclusion(
    started_at: ChinaDateTime,
    finished_at: ChinaDateTime,
    today_count: usize,
    event_count: usize,
    announcement_count: usize,
    successful_sources: &HashSet<&'static str>,
    missing_sources: &[&str],
) -> SyncConclusion {
    let kind = if missing_sources.is_empty() {
        if today_count == 0 {
            SyncConclusionKind::HealthyEmpty
        } else {
            SyncConclusionKind::HealthyNonempty
        }
    } else if today_count == 0 {
        SyncConclusionKind::Unknown
    } else {
        SyncConclusionKind::DegradedCached
    };
    let summary = match kind {
        SyncConclusionKind::HealthyEmpty => {
            format!(
                "同步完成：今日无新股（启用市场来源覆盖正常）；本轮更新 {event_count} 个任务、{announcement_count} 份公告"
            )
        }
        SyncConclusionKind::HealthyNonempty => {
            format!(
                "同步完成：今日任务 {today_count} 只；本轮更新 {event_count} 个任务、{announcement_count} 份公告"
            )
        }
        SyncConclusionKind::DegradedCached => format!(
            "已保留现有今日任务：来源覆盖不完整（{}）；本轮更新 {event_count} 个任务、{announcement_count} 份公告",
            missing_sources.join("、")
        ),
        _ => format!(
            "暂未获取到今日任务：来源覆盖不完整（{}）；本轮更新 {event_count} 个任务、{announcement_count} 份公告",
            missing_sources.join("、")
        ),
    };
    let mut successful_sources = successful_sources
        .iter()
        .map(|source| (*source).to_owned())
        .collect::<Vec<_>>();
    successful_sources.sort();
    SyncConclusion {
        kind,
        started_at,
        finished_at,
        today_count,
        event_count,
        announcement_count,
        successful_sources,
        missing_sources: missing_sources
            .iter()
            .map(|source| (*source).to_owned())
            .collect(),
        summary,
    }
}

fn persist_reconciled_group(
    database: &Database,
    resolved: IpoEvent,
    candidates: &[Candidate],
    documents: &[AnnouncementDocument],
) -> Result<IpoEvent> {
    // announcement_documents.ipo_event_id has a foreign key to ipo_events.id,
    // so a newly discovered event must be committed before its documents.
    let saved = database.upsert_event(resolved)?;
    database.replace_field_sources(&saved.id, candidates)?;
    for document in documents {
        database.save_announcement(document)?;
    }
    Ok(saved)
}

fn should_check_announcements(event: &IpoEvent) -> bool {
    let today = now_china().date_naive();
    event.apply_date.is_some_and(|date| {
        date >= today - chrono::Duration::days(7) && date <= today + chrono::Duration::days(45)
    })
}

fn requires_official_evidence(event: &IpoEvent) -> bool {
    let today = now_china().date_naive();
    event.apply_date.is_some_and(|date| {
        date >= today - chrono::Duration::days(7) && date <= today + chrono::Duration::days(7)
    })
}

fn final_data_quality(
    resolved: DataQualityStatus,
    provisional: DataQualityStatus,
    existing_official_evidence: bool,
) -> DataQualityStatus {
    if provisional == DataQualityStatus::ManualReviewRequired {
        DataQualityStatus::ManualReviewRequired
    } else if existing_official_evidence && resolved != DataQualityStatus::DataConflict {
        DataQualityStatus::AnnouncementVerified
    } else {
        resolved
    }
}

fn run_delivery_cycle(
    database: &Database,
    events: &mpsc::Sender<UiEvent>,
    snapshot: &Arc<RwLock<RuntimeSnapshot>>,
    data_root: &Path,
) -> Result<()> {
    database.touch_heartbeat("delivery", now_china())?;
    database.refresh_lifecycle()?;
    for delivery in database.claim_due(20)? {
        if let Err(error) = events.send(UiEvent::Reminder(delivery.clone())) {
            database.fail_delivery(delivery.outbox_id, &error.to_string())?;
        }
    }
    let secondary = database.claim_secondary_due(20)?;
    if !secondary.is_empty() {
        match secondary_notification::send_batch(data_root, &secondary) {
            Ok(receipt) => {
                database.complete_secondary_deliveries(
                    &secondary,
                    secondary_notification::provider_label(receipt.provider),
                )?;
                operations::log("INFO", &receipt.message());
            }
            Err(error) => {
                let message = operations::redact(&format!("{error:#}"));
                database.fail_secondary_deliveries(&secondary, &message)?;
                operations::log("WARN", &format!("第二通知通道发送失败：{message}"));
            }
        }
    }
    refresh_snapshot(database, snapshot);
    Ok(())
}

fn run_daily_maintenance(database: &Database, data_root: &Path) {
    let backup_directory = data_root.join("backups");
    let today = now_china().date_naive();
    if fs_needs_daily_backup(&backup_directory, today) {
        match database.backup(&backup_directory) {
            Ok(path) => {
                retain_latest_backups(&backup_directory, 7, Some(&path));
                if let Err(error) =
                    database.save_operation_health("database-backup", HealthState::Healthy, None)
                {
                    operations::log("ERROR", &format!("SQLite 备份健康状态写入失败：{error:#}"));
                }
                operations::log("INFO", &format!("每日 SQLite 备份完成：{}", path.display()));
            }
            Err(error) => {
                let message = format!("{error:#}");
                let _ = database.save_operation_health(
                    "database-backup",
                    HealthState::Failed,
                    Some(&message),
                );
                operations::log("ERROR", &format!("每日 SQLite 备份失败：{message}"));
            }
        }
    }
    match database.maintenance(data_root) {
        Ok(()) => {
            if let Err(error) =
                database.save_operation_health("database-maintenance", HealthState::Healthy, None)
            {
                operations::log("ERROR", &format!("数据库维护健康状态写入失败：{error:#}"));
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = database.save_operation_health(
                "database-maintenance",
                HealthState::Failed,
                Some(&message),
            );
            operations::log("ERROR", &format!("本地数据维护失败：{message}"));
        }
    }
    match operations::maintain_logs(data_root) {
        Ok(()) => {
            if let Err(error) =
                database.save_operation_health("log-retention", HealthState::Healthy, None)
            {
                operations::log("ERROR", &format!("日志保留健康状态写入失败：{error:#}"));
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = database.save_operation_health(
                "log-retention",
                HealthState::Failed,
                Some(&message),
            );
            operations::log("ERROR", &format!("日志保留清理失败：{message}"));
        }
    }
}

fn check_clock(client: &reqwest::blocking::Client, reason: &str) -> (HealthState, String) {
    let windows_time = windows_integration::windows_time_service_running();
    let endpoints = ["https://www.microsoft.com/", "https://www.cloudflare.com/"];
    let mut offsets = Vec::new();
    for endpoint in endpoints {
        let start = Utc::now();
        let url = format!("{endpoint}?clock_probe={}", start.timestamp_millis());
        let response = client
            .get(url)
            .header(reqwest::header::CACHE_CONTROL, "no-cache, no-store")
            .send();
        let end = Utc::now();
        let Ok(response) = response else { continue };
        let Some(raw) = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
        else {
            continue;
        };
        let Ok(server) = DateTime::parse_from_rfc2822(raw) else {
            continue;
        };
        let midpoint = start + (end - start) / 2;
        offsets.push((server.with_timezone(&Utc) - midpoint).num_milliseconds());
    }
    evaluate_clock_offsets(offsets, reason, windows_time)
}

fn evaluate_clock_offsets(
    mut offsets: Vec<i64>,
    reason: &str,
    windows_time: Result<Option<bool>>,
) -> (HealthState, String) {
    offsets.sort_unstable();
    if offsets.is_empty() {
        return add_windows_time_status(
            HealthState::Unknown,
            format!("无法取得独立网络时间样本（0/2，{reason}），未据此修改任务状态"),
            windows_time,
        );
    }
    let offset = if offsets.len() % 2 == 1 {
        offsets[offsets.len() / 2]
    } else {
        (offsets[offsets.len() / 2 - 1] + offsets[offsets.len() / 2]) / 2
    };
    let absolute = offset.unsigned_abs();
    let state = if offsets.len() < 2 || absolute > 2 * 60 * 1000 {
        if absolute > 5 * 60 * 1000 {
            HealthState::Failed
        } else {
            HealthState::Warning
        }
    } else {
        HealthState::Healthy
    };
    let prefix = match state {
        HealthState::Healthy => "系统时间正常",
        HealthState::Warning if offsets.len() < 2 => "系统时间样本不足",
        HealthState::Warning => "系统时间可能有偏差",
        HealthState::Failed => "系统时间偏差过大",
        _ => "系统时间状态未知",
    };
    add_windows_time_status(
        state,
        format!(
            "{prefix}：估算偏差 {:+.0} 秒，有效样本 {}/2（{reason}）",
            offset as f64 / 1000.0,
            offsets.len()
        ),
        windows_time,
    )
}

fn add_windows_time_status(
    state: HealthState,
    text: String,
    service_status: Result<Option<bool>>,
) -> (HealthState, String) {
    match service_status {
        Ok(Some(true)) => (state, format!("{text}；Windows Time 服务正在运行")),
        Ok(Some(false)) => (
            if state == HealthState::Failed {
                state
            } else {
                HealthState::Warning
            },
            format!("{text}；Windows Time 服务未运行，请检查 W32Time 配置"),
        ),
        Ok(None) => (state, text),
        Err(error) => (
            if state == HealthState::Failed {
                state
            } else {
                HealthState::Warning
            },
            format!(
                "{text}；Windows Time 服务状态读取失败：{}",
                operations::redact(&format!("{error:#}"))
            ),
        ),
    }
}

fn refresh_snapshot(database: &Database, snapshot: &Arc<RwLock<RuntimeSnapshot>>) {
    let events = database.today_events().unwrap_or_default();
    let pending = database.pending_count().unwrap_or_default();
    let health = database
        .health_text()
        .unwrap_or_else(|error| (HealthState::Failed, format!("健康状态读取失败：{error}")));
    update_snapshot(snapshot, |value| {
        value.today_count = events.len();
        value.pending_count = pending;
        value.health_state = health.0;
        value.health_text = health.1;
        if !value.is_synchronizing && value.status_text.starts_with("正在初始化") {
            value.status_text = "后台提醒服务已就绪".into();
        }
    });
}

fn update_snapshot(
    snapshot: &Arc<RwLock<RuntimeSnapshot>>,
    update: impl FnOnce(&mut RuntimeSnapshot),
) {
    if let Ok(mut value) = snapshot.write() {
        update(&mut value);
    }
}

fn fs_needs_daily_backup(directory: &Path, date: chrono::NaiveDate) -> bool {
    let prefix = format!("stock-ipo-reminder-{}", date.format("%Y%m%d"));
    std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn retain_latest_backups(directory: &Path, count: usize, preserve: Option<&Path>) {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|value| value == "db"))
        .collect();
    paths.sort();
    let remove_count = paths.len().saturating_sub(count);
    for path in paths.into_iter().take(remove_count) {
        if preserve != Some(path.as_path()) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AnnouncementRef, Board, Exchange, ExtractionStatus, IssueStatus, LifecycleStatus,
    };
    use uuid::Uuid;

    #[test]
    fn automatic_sync_uses_the_configured_interval() {
        let mut settings = AppSettings {
            normal_sync_minutes: 90,
            active_day_sync_minutes: 15,
            ..AppSettings::default()
        };
        assert_eq!(
            automatic_sync_interval_for(&settings, false),
            Duration::from_secs(90 * 60)
        );
        assert_eq!(
            automatic_sync_interval_for(&settings, true),
            Duration::from_secs(15 * 60)
        );

        settings.normal_sync_minutes = 0;
        assert_eq!(
            automatic_sync_interval_for(&settings, false),
            Duration::from_secs(5 * 60)
        );

        settings.active_day_sync_minutes = i32::MAX;
        assert_eq!(
            automatic_sync_interval_for(&settings, true),
            Duration::from_secs(7 * 24 * 60 * 60)
        );
    }

    #[test]
    fn automatic_sync_respects_window_fixed_checks_and_jitter_bounds() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let settings = AppSettings::default();

        let before_window = crate::core::at(date, crate::model::time(5, 0));
        let schedule = automatic_sync_schedule_for(
            &settings,
            before_window,
            false,
            false,
            Some(before_window),
            "fixture-a",
        );
        assert_eq!(schedule.due_at.date_naive(), date);
        assert_eq!(schedule.due_at.hour(), 6);

        let after_window = crate::core::at(date, crate::model::time(22, 5));
        let schedule = automatic_sync_schedule_for(
            &settings,
            after_window,
            false,
            false,
            Some(after_window),
            "fixture-a",
        );
        assert_eq!(
            schedule.due_at,
            crate::core::at(date + chrono::Duration::days(1), crate::model::time(6, 0))
        );

        let before_morning_check = crate::core::at(date, crate::model::time(7, 55));
        let schedule = automatic_sync_schedule_for(
            &settings,
            before_morning_check,
            true,
            false,
            Some(crate::core::at(date, crate::model::time(7, 0))),
            "fixture-a",
        );
        assert_eq!(
            schedule.due_at,
            crate::core::at(date, crate::model::time(8, 0))
        );
        assert!(schedule.reason.contains("08:00"));

        let after_missed_check = crate::core::at(date, crate::model::time(8, 5));
        let schedule = automatic_sync_schedule_for(
            &settings,
            after_missed_check,
            true,
            false,
            Some(crate::core::at(date, crate::model::time(7, 0))),
            "fixture-a",
        );
        assert_eq!(schedule.due_at, after_missed_check);
        assert!(schedule.reason.starts_with("补做"));

        let before_evening_check = crate::core::at(date, crate::model::time(19, 55));
        let schedule = automatic_sync_schedule_for(
            &settings,
            before_evening_check,
            false,
            true,
            Some(crate::core::at(date, crate::model::time(19, 0))),
            "fixture-a",
        );
        assert_eq!(
            schedule.due_at,
            crate::core::at(date, crate::model::time(20, 0))
        );
        assert!(schedule.reason.contains("20:00"));

        let normal_jitter = sync_jitter_seconds("fixture-a", before_evening_check, false);
        let active_jitter = sync_jitter_seconds("fixture-a", before_evening_check, true);
        assert!((0..=90).contains(&normal_jitter));
        assert!((0..=20).contains(&active_jitter));
        assert_eq!(
            normal_jitter,
            sync_jitter_seconds("fixture-a", before_evening_check, false)
        );
    }

    #[test]
    fn enabled_markets_define_required_sync_sources() {
        let all_sources = HashSet::from(["eastmoney", "sse", "cninfo", "bse"]);
        assert!(missing_required_sources(&AppSettings::default(), &all_sources).is_empty());

        let shanghai_only = AppSettings {
            shenzhen_enabled: false,
            beijing_enabled: false,
            ..AppSettings::default()
        };
        let shanghai_sources = HashSet::from(["eastmoney", "sse"]);
        assert!(missing_required_sources(&shanghai_only, &shanghai_sources).is_empty());

        let without_bse = HashSet::from(["eastmoney", "sse", "cninfo"]);
        assert_eq!(
            missing_required_sources(&AppSettings::default(), &without_bse),
            vec!["bse"]
        );
    }

    #[test]
    fn completion_text_only_claims_no_ipo_after_complete_coverage() {
        let started = now_china();
        let sources = HashSet::from(["eastmoney", "sse", "cninfo", "bse"]);
        let no_ipo = sync_conclusion(started, started, 0, 0, 0, &sources, &[]);
        assert_eq!(no_ipo.kind, SyncConclusionKind::HealthyEmpty);
        assert!(no_ipo.summary.contains("今日无新股"));

        let unknown = sync_conclusion(started, started, 0, 0, 0, &sources, &["bse"]);
        assert_eq!(unknown.kind, SyncConclusionKind::Unknown);
        assert!(!unknown.summary.contains("今日无新股"));
        assert!(unknown.summary.contains("暂未获取到今日任务"));
        assert!(unknown.summary.contains("来源覆盖不完整"));

        let retained = sync_conclusion(started, started, 2, 1, 0, &sources, &["cninfo"]);
        assert_eq!(retained.kind, SyncConclusionKind::DegradedCached);
        assert!(retained.summary.contains("已保留现有今日任务"));
        assert!(retained.summary.contains("来源覆盖不完整"));
    }

    #[test]
    fn windows_time_service_status_degrades_clock_health_without_hiding_failure() {
        let (warning, warning_text) = add_windows_time_status(
            HealthState::Healthy,
            "网络时间样本正常".into(),
            Ok(Some(false)),
        );
        assert_eq!(warning, HealthState::Warning);
        assert!(warning_text.contains("W32Time"));

        let (failed, _) = add_windows_time_status(
            HealthState::Failed,
            "网络时间偏差过大".into(),
            Ok(Some(false)),
        );
        assert_eq!(failed, HealthState::Failed);
    }

    #[test]
    fn clock_health_matrix_covers_missing_samples_large_drift_and_service_failures() {
        let (unknown, text) = evaluate_clock_offsets(Vec::new(), "fixture", Ok(Some(true)));
        assert_eq!(unknown, HealthState::Unknown);
        assert!(text.contains("0/2"));

        let (one_sample, text) = evaluate_clock_offsets(vec![30_000], "fixture", Ok(Some(true)));
        assert_eq!(one_sample, HealthState::Warning);
        assert!(text.contains("样本不足"));

        let (healthy, _) = evaluate_clock_offsets(vec![-20_000, 20_000], "fixture", Ok(Some(true)));
        assert_eq!(healthy, HealthState::Healthy);

        let (large_drift, text) = evaluate_clock_offsets(
            vec![6 * 60 * 1000, 7 * 60 * 1000],
            "fixture",
            Ok(Some(false)),
        );
        assert_eq!(large_drift, HealthState::Failed);
        assert!(text.contains("偏差过大"));
        assert!(text.contains("未运行"));

        let (service_error, text) =
            evaluate_clock_offsets(vec![0, 0], "fixture", Err(anyhow::anyhow!("access denied")));
        assert_eq!(service_error, HealthState::Warning);
        assert!(text.contains("状态读取失败"));
    }

    #[test]
    fn automatic_sync_handles_cross_day_boundaries_and_clock_rollback_inputs() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let settings = AppSettings::default();
        let at_window_end = crate::core::at(date, crate::model::time(22, 0));
        let schedule = automatic_sync_schedule_for(
            &settings,
            at_window_end,
            false,
            false,
            Some(at_window_end),
            "cross-day",
        );
        assert_eq!(
            schedule.due_at,
            crate::core::at(date + chrono::Duration::days(1), crate::model::time(6, 0))
        );

        let after_rollback = crate::core::at(date, crate::model::time(7, 30));
        let future_last_sync = crate::core::at(date, crate::model::time(9, 0));
        let schedule = automatic_sync_schedule_for(
            &settings,
            after_rollback,
            true,
            false,
            Some(future_last_sync),
            "clock-rollback",
        );
        assert!(schedule.due_at > after_rollback);
        assert!(schedule.due_at <= after_rollback + chrono::Duration::minutes(11));
    }

    #[test]
    fn announcement_run_state_distinguishes_failure_warning_and_success() {
        let mut failed = AnnouncementRunStats::new(now_china());
        failed.attempted_events = 1;
        failed.successful_searches = 1;
        failed.references_found = 1;
        failed.documents_processed = 1;
        failed.record_issue("PDF 下载失败");
        assert_eq!(failed.state(), HealthState::Failed);

        let mut warning = AnnouncementRunStats::new(now_china());
        warning.attempted_events = 2;
        warning.successful_searches = 2;
        warning.references_found = 2;
        warning.documents_processed = 1;
        warning.documents_succeeded = 1;
        warning.record_issue("一个事件由备用镜像接管");
        assert_eq!(warning.state(), HealthState::Warning);

        let mut healthy = AnnouncementRunStats::new(now_china());
        healthy.attempted_events = 1;
        healthy.successful_searches = 1;
        assert_eq!(healthy.state(), HealthState::Healthy);
    }

    #[test]
    fn prior_announcement_evidence_is_preserved_without_hiding_conflicts() {
        assert_eq!(
            final_data_quality(
                DataQualityStatus::MultiSourceVerified,
                DataQualityStatus::MultiSourceVerified,
                true,
            ),
            DataQualityStatus::AnnouncementVerified
        );
        assert_eq!(
            final_data_quality(
                DataQualityStatus::DataConflict,
                DataQualityStatus::DataConflict,
                true,
            ),
            DataQualityStatus::DataConflict
        );
        assert_eq!(
            final_data_quality(
                DataQualityStatus::MultiSourceVerified,
                DataQualityStatus::ManualReviewRequired,
                true,
            ),
            DataQualityStatus::ManualReviewRequired
        );
    }

    #[test]
    fn new_event_is_saved_before_its_announcement_documents() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-rust-runtime-test-{}",
            Uuid::new_v4().simple()
        ));
        let database = Database::new(&root);
        database.initialize().unwrap();
        let now = now_china();
        let event = IpoEvent {
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
            data_quality_status: DataQualityStatus::AnnouncementVerified,
            data_conflict: false,
            manual_override_fields: Vec::new(),
            sessions: Vec::new(),
            first_seen_at: now,
            updated_at: now,
        };
        let document = AnnouncementDocument {
            id: "document-1".into(),
            event_id: event.id.clone(),
            reference: AnnouncementRef {
                provider: "sse-announcement".into(),
                announcement_id: "announcement-1".into(),
                title: "首次公开发行公告".into(),
                url: "https://www.sse.com.cn/test.pdf".into(),
                published_at: Some(now),
                announcement_type: Some("发行公告".into()),
            },
            local_path: "announcements/test.pdf".into(),
            file_hash: "abc123".into(),
            text_hash: Some("def456".into()),
            status: ExtractionStatus::Extracted,
            parser_version: "rust-test".into(),
            fields: Vec::new(),
            downloaded_at: now,
        };

        persist_reconciled_group(&database, event, &[], &[document]).unwrap();

        assert!(database.event("shanghai:601001").unwrap().is_some());
        assert_eq!(
            database.announcement_titles("shanghai:601001").unwrap(),
            vec!["首次公开发行公告"]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_start_does_not_wait_for_database_initialization() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-runtime-start-test-{}",
            Uuid::new_v4().simple()
        ));
        let started = Instant::now();
        let (runtime, worker) = start_with_initializer(root.clone(), false, |_, _| {
            thread::sleep(Duration::from_millis(400));
            anyhow::bail!("simulated initialization failure")
        })
        .unwrap();

        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(!runtime.is_ready());
        assert!(runtime.settings().is_err());
        worker.join().unwrap();
        assert_eq!(runtime.snapshot().health_state, HealthState::Failed);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn daily_maintenance_creates_one_backup_without_a_successful_sync() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-rust-maintenance-test-{}",
            Uuid::new_v4().simple()
        ));
        let database = Database::new(&root);
        database.initialize().unwrap();

        run_daily_maintenance(&database, &root);
        let backup_directory = root.join("backups");
        let first_count = std::fs::read_dir(&backup_directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "db"))
            .count();
        assert_eq!(first_count, 1);

        run_daily_maintenance(&database, &root);
        let second_count = std::fs::read_dir(&backup_directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|value| value == "db"))
            .count();
        assert_eq!(second_count, 1);
        let _ = std::fs::remove_dir_all(root);
    }
}

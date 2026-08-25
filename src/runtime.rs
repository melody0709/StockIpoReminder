use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Datelike, Timelike, Utc};

use crate::{
    announcement,
    core::{group_candidates, now_china, reconcile_candidates},
    model::{
        AnnouncementDocument, AppSettings, Candidate, DataQualityStatus, FieldSourceEntry,
        HealthState, IpoEvent, ManualOverrideEntry, ReminderDelivery,
    },
    network::{self, CollectorOutput},
    operations,
    storage::Database,
};

const AUTOMATIC_SYNC_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const DELIVERY_INTERVAL: Duration = Duration::from_secs(10);
const CLOCK_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

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
            is_synchronizing: false,
            status_text: "正在初始化后台提醒服务…".into(),
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
        let _ = self.command_sender.send(RuntimeCommand::Stop);
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
    pub fn settings(&self) -> Result<AppSettings> {
        self.database.settings()
    }
    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.database.save_settings(settings)?;
        self.database.replan_all()?;
        Ok(())
    }
    pub fn today_events(&self) -> Result<Vec<IpoEvent>> {
        self.database.today_events()
    }
    pub fn future_events(&self) -> Result<Vec<IpoEvent>> {
        self.database.future_events(60)
    }
    pub fn health_details(&self) -> Result<crate::model::HealthDetails> {
        self.database.health_details()
    }
    pub fn event(&self, id: &str) -> Result<Option<IpoEvent>> {
        self.database.event(id)
    }
    pub fn announcement_titles(&self, id: &str) -> Result<Vec<String>> {
        self.database.announcement_titles(id)
    }
    pub fn field_sources(&self, id: &str) -> Result<Vec<FieldSourceEntry>> {
        self.database.field_sources(id)
    }
    pub fn announcements(&self, id: &str) -> Result<Vec<AnnouncementDocument>> {
        self.database.announcements(id)
    }
    pub fn manual_overrides(&self, id: &str, version: i32) -> Result<Vec<ManualOverrideEntry>> {
        self.database.manual_overrides(id, version)
    }
    pub fn acknowledge(&self, id: &str, version: i32) -> Result<()> {
        self.database.acknowledge(id, version)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_acknowledgement(&self, id: &str, version: i32) -> Result<()> {
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
        self.database
            .apply_manual_override(id, version, field, value, reason, announcement_id)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_override(&self, id: &str, version: i32, override_id: i64) -> Result<()> {
        self.database
            .revoke_manual_override(id, version, override_id)?;
        self.wake();
        Ok(())
    }
    pub fn revoke_overrides(&self, id: &str, version: i32) -> Result<usize> {
        let count = self.database.revoke_manual_overrides(id, version)?;
        self.wake();
        Ok(count)
    }
    pub fn database(&self) -> &Database {
        &self.database
    }
}

pub fn start(data_root: PathBuf, startup_sync: bool) -> Result<(RuntimeHandle, JoinHandle<()>)> {
    let database = Database::new(&data_root);
    database.initialize()?;
    database.integrity_check()?;
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let snapshot = Arc::new(RwLock::new(RuntimeSnapshot::default()));
    let handle = RuntimeHandle {
        database: database.clone(),
        command_sender,
        event_receiver: Arc::new(Mutex::new(event_receiver)),
        snapshot: Arc::clone(&snapshot),
    };
    let thread = thread::Builder::new()
        .name("stock-ipo-runtime".into())
        .spawn(move || {
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
    let mut next_sync = if startup_sync {
        Instant::now()
    } else {
        Instant::now() + AUTOMATIC_SYNC_INTERVAL
    };
    let mut next_delivery = Instant::now();
    let mut next_clock = Instant::now();
    let mut last_health_date = None;
    let mut requested_reason = startup_sync.then(|| "程序启动".to_owned());
    refresh_snapshot(&database, &snapshot);

    loop {
        let now = Instant::now();
        if now >= next_sync || requested_reason.is_some() {
            let reason = requested_reason
                .take()
                .unwrap_or_else(|| "24 小时定时同步".into());
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
            next_sync = Instant::now() + AUTOMATIC_SYNC_INTERVAL;
            refresh_snapshot(&database, &snapshot);
            #[cfg(windows)]
            crate::windows_integration::trim_working_set();
        }
        if Instant::now() >= next_delivery {
            let _ = run_delivery_cycle(&database, &events, &snapshot);
            let china_now = now_china();
            if china_now.hour() >= 8 && last_health_date != Some(china_now.date_naive()) {
                if let Ok(settings) = database.settings() {
                    if settings.daily_health_summary_enabled
                        && database
                            .try_mark_health_summary_sent(china_now.date_naive(), china_now)
                            .unwrap_or(false)
                    {
                        if let Ok((state, text)) = database.health_text() {
                            let _ = events.send(UiEvent::Health { state, text });
                        }
                    }
                }
                last_health_date = Some(china_now.date_naive());
            }
            next_delivery = Instant::now() + DELIVERY_INTERVAL;
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
    for (source, collect) in collectors {
        let now = now_china();
        if !database.source_can_attempt(source, now)?.0 {
            continue;
        }
        match collect(client) {
            Ok(output) => {
                let record_count = output.candidates.len();
                database.save_collector_run(
                    output.source,
                    output.started,
                    true,
                    record_count,
                    Some(&output.raw),
                    Some(&output.hash),
                    Some(&output.schema),
                    None,
                )?;
                database.record_source_success(output.source, now_china())?;
                candidates.extend(output.candidates);
                successful_sources += 1;
                operations::log(
                    "INFO",
                    &format!("数据源 {} 同步成功，{} 条记录", output.source, record_count),
                );
            }
            Err(error) => {
                let message = format!("{error:#}");
                database.save_collector_run(
                    source,
                    now,
                    false,
                    0,
                    None,
                    None,
                    None,
                    Some(&message),
                )?;
                let _ = database.record_source_failure(source, now_china(), &message)?;
                failed_sources += 1;
                operations::log("ERROR", &format!("数据源 {source} 同步失败：{message}"));
            }
        }
    }
    if successful_sources == 0 && candidates.is_empty() {
        let message = if failed_sources == 0 {
            "所有来源都在退避期内"
        } else {
            "所有数据源同步失败"
        };
        update_snapshot(snapshot, |value| {
            value.is_synchronizing = false;
            value.status_text = format!("{message}，继续使用 SQLite 缓存");
            value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
            value.last_sync_succeeded = Some(false);
            value.last_error = Some(message.into());
        });
        return Ok(());
    }

    let settings = database.settings()?;
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
        if should_check_announcements(&provisional) {
            let provider = match provisional.exchange {
                crate::model::Exchange::Shanghai => "sse-announcement",
                crate::model::Exchange::Shenzhen => "cninfo-announcement",
                crate::model::Exchange::Beijing => "bse-announcement",
                _ => "announcement",
            };
            let now = now_china();
            if database.source_can_attempt(provider, now)?.0 {
                let from =
                    provisional.apply_date.unwrap_or(now.date_naive()) - chrono::Duration::days(14);
                let to =
                    provisional.apply_date.unwrap_or(now.date_naive()) + chrono::Duration::days(7);
                match announcement::search(client, &provisional, from, to) {
                    Ok(references) => {
                        let mut processing_failed = false;
                        for reference in references.into_iter().take(5) {
                            match announcement::download_and_parse(
                                client,
                                data_root,
                                &provisional,
                                reference,
                            ) {
                                Ok((document, candidate)) => {
                                    if let Some(candidate) = candidate {
                                        combined.push(candidate);
                                    }
                                    documents.push(document);
                                }
                                Err(error) => {
                                    processing_failed = true;
                                    operations::log(
                                        "ERROR",
                                        &format!(
                                            "公告处理失败：event={}, provider={provider}, error={error:#}",
                                            provisional.id,
                                        ),
                                    );
                                }
                            }
                        }
                        if processing_failed && requires_official_evidence(&provisional) {
                            provisional.data_quality_status =
                                DataQualityStatus::ManualReviewRequired;
                        }
                        database.record_source_success(provider, now_china())?;
                    }
                    Err(error) => {
                        let _ = database.record_source_failure(
                            provider,
                            now_china(),
                            &format!("{error:#}"),
                        );
                        if requires_official_evidence(&provisional) {
                            provisional.data_quality_status =
                                DataQualityStatus::ManualReviewRequired;
                        }
                    }
                }
            }
        }
        let mut resolved =
            reconcile_candidates(&combined, existing.as_ref(), &settings, now_china())
                .unwrap_or(provisional.clone());
        if provisional.data_quality_status == DataQualityStatus::ManualReviewRequired {
            resolved.data_quality_status = DataQualityStatus::ManualReviewRequired;
        }
        persist_reconciled_group(database, resolved, &combined, &documents)?;
        announcement_count += documents.len();
        event_count += 1;
    }
    database.touch_heartbeat("synchronization", now_china())?;
    let backups = data_root.join("backups");
    if started.day() != now_china().day()
        || fs_needs_daily_backup(&backups, now_china().date_naive())
    {
        if let Ok(path) = database.backup(&backups) {
            retain_latest_backups(&backups, 7, Some(&path));
        }
    }
    let _ = database.maintenance(data_root);
    update_snapshot(snapshot, |value| {
        value.is_synchronizing = false;
        value.status_text = format!("同步完成：{event_count} 个任务，{announcement_count} 份公告");
        value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
        value.last_sync_succeeded = Some(true);
        value.last_error = None;
    });
    operations::log(
        "INFO",
        &format!(
            "同步完成：candidates={candidate_count}, events={event_count}, announcements={announcement_count}, sources={successful_sources}, failed={failed_sources}"
        ),
    );
    Ok(())
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

fn run_delivery_cycle(
    database: &Database,
    events: &mpsc::Sender<UiEvent>,
    snapshot: &Arc<RwLock<RuntimeSnapshot>>,
) -> Result<()> {
    database.touch_heartbeat("delivery", now_china())?;
    database.refresh_lifecycle()?;
    for delivery in database.claim_due(20)? {
        match events.send(UiEvent::Reminder(delivery.clone())) {
            Ok(()) => database.complete_delivery(&delivery, "slint+tray")?,
            Err(error) => database.fail_delivery(delivery.outbox_id, &error.to_string())?,
        }
    }
    refresh_snapshot(database, snapshot);
    Ok(())
}

fn check_clock(client: &reqwest::blocking::Client, reason: &str) -> (HealthState, String) {
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
    offsets.sort_unstable();
    if offsets.is_empty() {
        return (
            HealthState::Unknown,
            format!("无法取得独立网络时间样本（0/2，{reason}），未据此修改任务状态"),
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
    (
        state,
        format!(
            "{prefix}：估算偏差 {:+.0} 秒，有效样本 {}/2（{reason}）",
            offset as f64 / 1000.0,
            offsets.len()
        ),
    )
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
    fn automatic_sync_is_once_per_day() {
        assert_eq!(AUTOMATIC_SYNC_INTERVAL, Duration::from_hours(24));
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
}

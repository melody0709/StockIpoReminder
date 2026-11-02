use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSnapshot {
    pub revision: u64,
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
    pub next_wake_text: String,
    pub last_error: Option<String>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
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
            next_wake_text: "后台调度正在初始化…".into(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Reminder(ReminderDelivery),
    Finance(Vec<crate::finance::FinanceReminder>),
    Health { state: HealthState, text: String },
}

pub(crate) enum RuntimeCommand {
    Sync(SyncRequest),
    SyncFinished {
        reason: String,
        result: std::result::Result<(), String>,
    },
    Wake,
    Recovery,
    Stop,
}

#[derive(Debug, Default)]
pub(crate) struct FinanceRuntimeState {
    in_flight: HashSet<String>,
    retry_pending: BTreeMap<String, crate::model::ChinaDateTime>,
    shown_this_session: HashSet<String>,
}

impl FinanceRuntimeState {
    pub(crate) fn is_blocked(&self, key: &str, now: crate::model::ChinaDateTime) -> bool {
        self.in_flight.contains(key)
            || self.shown_this_session.contains(key)
            || self
                .retry_pending
                .get(key)
                .is_some_and(|retry_at| *retry_at > now)
    }

    pub(crate) fn begin(&mut self, keys: impl IntoIterator<Item = String>) {
        for key in keys {
            self.in_flight.insert(key);
        }
    }

    pub(crate) fn retry_pending(&self) -> &BTreeMap<String, crate::model::ChinaDateTime> {
        &self.retry_pending
    }

    pub(crate) fn presented(&mut self, keys: &[String]) -> Result<()> {
        self.ensure_keys(keys)?;
        for key in keys {
            self.in_flight.remove(key);
            self.retry_pending.remove(key);
            self.shown_this_session.insert(key.clone());
        }
        Ok(())
    }

    pub(crate) fn failed(
        &mut self,
        keys: &[String],
        now: crate::model::ChinaDateTime,
        valid_until: crate::model::ChinaDateTime,
    ) {
        for key in keys {
            self.in_flight.remove(key);
            let retry_at = (now + chrono::Duration::seconds(30)).min(valid_until);
            if retry_at > now {
                self.retry_pending.insert(key.clone(), retry_at);
            }
        }
    }

    pub(crate) fn discard(&mut self, keys: &[String]) {
        for key in keys {
            self.in_flight.remove(key);
            self.retry_pending.remove(key);
        }
    }

    fn ensure_keys(&self, keys: &[String]) -> Result<()> {
        if keys.is_empty() {
            anyhow::bail!("提醒 key 不能为空");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SyncRequest {
    pub(crate) reason: String,
    pub(crate) allow_outside_window: bool,
}

type UiNotifier = Arc<dyn Fn() + Send + Sync + 'static>;

#[derive(Clone)]
pub(crate) struct RuntimeUiState {
    pub(crate) snapshot: Arc<RwLock<RuntimeSnapshot>>,
    pub(crate) finance: Arc<Mutex<FinanceRuntimeState>>,
    pub(crate) notifier: Arc<Mutex<Option<UiNotifier>>>,
}

impl RuntimeUiState {
    pub(crate) fn new() -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(RuntimeSnapshot::default())),
            finance: Arc::new(Mutex::new(FinanceRuntimeState::default())),
            notifier: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn notify(&self) {
        let notifier = self
            .notifier
            .lock()
            .ok()
            .and_then(|value| value.as_ref().cloned());
        if let Some(notifier) = notifier {
            notifier();
        }
    }
}

#[derive(Clone)]
pub struct RuntimeHandle {
    database: Database,
    command_sender: mpsc::Sender<RuntimeCommand>,
    event_receiver: Arc<Mutex<mpsc::Receiver<UiEvent>>>,
    ui_state: RuntimeUiState,
    ready: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
}

impl RuntimeHandle {
    pub fn request_sync(&self, reason: impl Into<String>) {
        let _ = self.command_sender.send(RuntimeCommand::Sync(SyncRequest {
            reason: reason.into(),
            allow_outside_window: true,
        }));
    }
    pub fn wake(&self) {
        let _ = self.command_sender.send(RuntimeCommand::Wake);
    }
    pub fn notify_ui(&self) {
        self.ui_state.notify();
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
        self.ui_state
            .snapshot
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }
    pub fn try_event(&self) -> Option<UiEvent> {
        self.event_receiver.lock().ok()?.try_recv().ok()
    }
    pub fn install_ui_notifier(&self, notifier: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut target) = self.ui_state.notifier.lock() {
            *target = Some(Arc::new(notifier));
        }
        self.ui_state.notify();
    }
    pub fn remove_ui_notifier(&self) {
        if let Ok(mut target) = self.ui_state.notifier.lock() {
            *target = None;
        }
    }
    pub fn complete_delivery(&self, delivery: &ReminderDelivery) -> Result<()> {
        self.ensure_ready()?;
        self.database.complete_delivery(delivery, "slint+tray")?;
        self.wake();
        Ok(())
    }
    pub fn fail_delivery(&self, delivery: &ReminderDelivery, error: &str) -> Result<()> {
        self.ensure_ready()?;
        self.database.fail_delivery(delivery.outbox_id, error)?;
        self.wake();
        Ok(())
    }
    pub fn settings(&self) -> Result<AppSettings> {
        self.ensure_ready()?;
        self.database.settings()
    }
    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        self.ensure_ready()?;
        self.database.save_settings_and_replan(settings)?;
        self.wake();
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
    pub fn repo_settings(&self) -> Result<crate::finance::RepoSettings> {
        self.ensure_ready()?;
        self.database.repo_settings()
    }
    pub fn save_repo_settings(&self, settings: &crate::finance::RepoSettings) -> Result<()> {
        self.ensure_ready()?;
        self.database.save_repo_settings(settings)?;
        self.wake();
        Ok(())
    }
    pub fn repo_day_acked(&self, date: chrono::NaiveDate) -> Result<bool> {
        self.ensure_ready()?;
        self.database.repo_day_acked(date)
    }
    pub fn set_repo_day_acked(&self, date: chrono::NaiveDate, acked: bool) -> Result<()> {
        self.ensure_ready()?;
        self.database.set_repo_day_acked(date, acked)?;
        self.wake();
        Ok(())
    }
    pub fn credit_cards(&self) -> Result<Vec<crate::finance::CreditCard>> {
        self.ensure_ready()?;
        self.database.credit_cards()
    }
    pub fn save_credit_card(&self, card: &crate::finance::CreditCard) -> Result<()> {
        self.ensure_ready()?;
        self.database.save_credit_card(card)?;
        self.wake();
        Ok(())
    }
    pub fn delete_credit_card(&self, id: &str) -> Result<()> {
        self.ensure_ready()?;
        self.database.delete_credit_card(id)?;
        self.wake();
        Ok(())
    }
    pub fn credit_card_settled(&self, id: &str) -> Result<Option<String>> {
        self.ensure_ready()?;
        self.database.credit_card_settled(id)
    }
    pub fn mark_credit_card_paid(&self, id: &str, cycle: &str) -> Result<()> {
        self.ensure_ready()?;
        let card = self
            .database
            .credit_card(id)?
            .ok_or_else(|| anyhow::anyhow!("信用卡不存在"))?;
        let due = crate::finance::due_on_for_cycle(cycle, card.due_day)?;
        self.database.mark_credit_card_paid(id, cycle, due)?;
        self.wake();
        Ok(())
    }
    pub fn undo_credit_card_action(&self, id: &str) -> Result<()> {
        self.ensure_ready()?;
        self.database.undo_credit_card_action(id)?;
        self.wake();
        Ok(())
    }
    pub fn finance_presented(&self, reminders: &[crate::finance::FinanceReminder]) -> Result<()> {
        self.ensure_ready()?;
        for reminder in reminders {
            self.database.mark_finance_shown(
                reminder.kind.as_str(),
                &reminder.source_id,
                &reminder.occurrence_key,
                reminder.event_date,
            )?;
        }
        let keys = reminders
            .iter()
            .map(|reminder| reminder.occurrence_key.clone())
            .collect::<Vec<_>>();
        if let Ok(mut state) = self.ui_state.finance.lock() {
            state.presented(&keys)?;
        }
        self.wake();
        Ok(())
    }
    pub fn finance_present_failed(
        &self,
        reminders: &[crate::finance::FinanceReminder],
        category: &str,
    ) {
        let _ = category;
        let now = crate::core::now_china();
        if let Ok(mut state) = self.ui_state.finance.lock() {
            for reminder in reminders {
                state.failed(
                    std::slice::from_ref(&reminder.occurrence_key),
                    now,
                    reminder.valid_until,
                );
            }
        }
        self.wake();
    }
    pub fn finance_discard(&self, keys: &[String]) {
        if let Ok(mut state) = self.ui_state.finance.lock() {
            state.discard(keys);
        }
        self.wake();
    }
    pub fn finance_reminder_is_current(
        &self,
        reminder: &crate::finance::FinanceReminder,
    ) -> Result<bool> {
        self.ensure_ready()?;
        Ok(
            crate::finance::due_finance_reminders(
                &self.database,
                crate::core::now_china(),
                |_| false,
            )?
            .iter()
            .any(|item| item.occurrence_key == reminder.occurrence_key),
        )
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

pub(crate) fn start_with_initializer<F>(
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
    let ui_state = RuntimeUiState::new();
    let ready = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let command_sender_for_run = command_sender.clone();
    let handle = RuntimeHandle {
        database: database.clone(),
        command_sender,
        event_receiver: Arc::new(Mutex::new(event_receiver)),
        ui_state: ui_state.clone(),
        ready: Arc::clone(&ready),
        stop_requested: Arc::clone(&stop_requested),
    };
    let thread = thread::Builder::new()
        .name("stock-ipo-runtime".into())
        .spawn(move || {
            if let Err(error) = initialize(&database, &data_root) {
                crate::operations::log("ERROR", &format!("本地数据库初始化失败：{error:#}"));
                update_snapshot(&ui_state, |value| {
                    value.is_synchronizing = false;
                    value.status_text = "本地数据库初始化失败，请查看日志".into();
                    value.health_text = "后台提醒服务未启动".into();
                    value.health_state = HealthState::Failed;
                    value.last_error = Some(format!("{error:#}"));
                });
                return;
            }
            ready.store(true, Ordering::Release);
            update_snapshot(&ui_state, |value| {
                value.is_synchronizing = false;
                value.status_text = "本地数据已就绪，正在启动后台提醒服务…".into();
            });
            if stop_requested.load(Ordering::Acquire) {
                return;
            }
            let mut initial_reason = startup_sync.then(|| SyncRequest {
                reason: "程序启动".to_owned(),
                allow_outside_window: false,
            });
            let mut first_attempt = true;
            let mut failure_count = 0usize;
            let mut last_failure = None::<Instant>;
            loop {
                if stop_requested.load(Ordering::Acquire) {
                    return;
                }
                let suppress_overdue_initial_sync = first_attempt && !startup_sync;
                first_attempt = false;
                match run_loop(
                    &database,
                    &data_root,
                    initial_reason.take(),
                    suppress_overdue_initial_sync,
                    &command_receiver,
                    &command_sender_for_run,
                    &event_sender,
                    &ui_state,
                    Arc::clone(&stop_requested),
                ) {
                    Ok(()) => return,
                    Err(error) => {
                        let failure_now = Instant::now();
                        if last_failure.is_none_or(|previous| {
                            failure_now.duration_since(previous) > RUNTIME_FAILURE_RESET_AFTER
                        }) {
                            failure_count = 0;
                        }
                        last_failure = Some(failure_now);
                        failure_count = failure_count.saturating_add(1);
                        let delay = RUNTIME_RESTART_DELAYS[failure_count
                            .saturating_sub(1)
                            .min(RUNTIME_RESTART_DELAYS.len() - 1)];
                        let message = format!("{error:#}");
                        crate::operations::log(
                            "ERROR",
                            &format!(
                                "后台运行时异常，将在 {} 秒后重启本地服务：{message}",
                                delay.as_secs()
                            ),
                        );
                        if let Err(health_error) = database.save_operation_health(
                            "runtime",
                            HealthState::Failed,
                            Some(&message),
                        ) {
                            crate::operations::log(
                                "WARN",
                                &format!("后台运行时失败状态写入失败：{health_error:#}"),
                            );
                        }
                        update_snapshot(&ui_state, |value| {
                            value.is_synchronizing = false;
                            value.health_state = HealthState::Failed;
                            value.status_text = format!(
                                "后台服务异常，{} 秒后自动恢复；本地数据仍可查看",
                                delay.as_secs()
                            );
                            value.health_text = "提醒与同步服务正在自动恢复".into();
                            value.last_error = Some(message.clone());
                        });
                        match command_receiver.recv_timeout(delay) {
                            Ok(RuntimeCommand::Sync(request)) => initial_reason = Some(request),
                            Ok(RuntimeCommand::SyncFinished { .. }) => {}
                            Ok(RuntimeCommand::Wake) | Ok(RuntimeCommand::Recovery) => {}
                            Ok(RuntimeCommand::Stop)
                            | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                }
            }
        })?;
    Ok((handle, thread))
}

use super::*;

/// 更新流程的 UI 编排：检查、下载、安装、pending 恢复、缓存与节流。
/// 状态机：idle -> available -> downloading -> installing；失败回到 available/failed。
/// 用户点击一次「更新」即完成下载、验证、退出与安装，新版由 helper 以托盘方式拉起。
pub(crate) struct UpdateController {
    window: slint::Weak<MainWindow>,
    data_root: PathBuf,
    runtime: RuntimeHandle,
    available: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    check_busy: Arc<AtomicBool>,
    download_busy: Arc<AtomicBool>,
    install_busy: Arc<AtomicBool>,
    update_configured: bool,
    skip_update_check: bool,
    ready: AtomicBool,
    installing: AtomicBool,
    /// 清单不可用但兜底探测到更高版本时为真：胶囊只打开下载页，不进入安装路径。
    manual_only: AtomicBool,
    #[cfg(windows)]
    tray: Arc<native_tray::NativeTray>,
}

/// 下载触发来源：一次性点击（下载完自动安装）或「发现更新后自动下载」的预取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadTrigger {
    OneClick,
    Prefetch,
}

const LATEST_NOTE_TTL: Duration = Duration::from_secs(5);

impl UpdateController {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        window: slint::Weak<MainWindow>,
        data_root: PathBuf,
        runtime: RuntimeHandle,
        update_configured: bool,
        skip_update_check: bool,
        #[cfg(windows)] tray: Arc<native_tray::NativeTray>,
    ) -> Self {
        Self {
            window,
            data_root,
            runtime,
            available: Arc::new(Mutex::new(None)),
            check_busy: Arc::new(AtomicBool::new(false)),
            download_busy: Arc::new(AtomicBool::new(false)),
            install_busy: Arc::new(AtomicBool::new(false)),
            update_configured,
            skip_update_check,
            ready: AtomicBool::new(false),
            installing: AtomicBool::new(false),
            manual_only: AtomicBool::new(false),
            #[cfg(windows)]
            tray,
        }
    }

    /// 启动即恢复已验证的待安装更新；损坏或过期的 pending 安全清理。
    pub(crate) fn restore_pending(self: &Arc<Self>) {
        let window = self.window.clone();
        let data_root = self.data_root.clone();
        let controller = Arc::clone(self);
        std::thread::spawn(move || match updater::verify_pending(&data_root) {
            Ok(pending) => {
                let version = pending.manifest.version.clone();
                let notes = updater::release_notes_url(&pending.manifest).unwrap_or_default();
                let dismissed = updater::banner_dismissed_for(&data_root, &version);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = window.upgrade() else { return };
                    ui.set_update_ready(true);
                    ui.set_update_version(version.clone().into());
                    ui.set_update_release_notes_url(notes.into());
                    ui.set_update_status(
                        format!("{version} 已下载并验证，点击更新后自动重启升级").into(),
                    );
                    if !dismissed {
                        Self::publish_ready(&ui, &version);
                    }
                    controller.sync_tray_badge((!dismissed).then_some(version.as_str()));
                });
            }
            Err(error) => {
                if updater::pending_manifest_present(&data_root) {
                    operations::log(
                        "WARN",
                        &format!(
                            "待安装更新重新验证失败，已安全清理：{}",
                            operations::redact(&format!("{error:#}"))
                        ),
                    );
                    updater::cleanup_pending(&data_root);
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = window.upgrade() else { return };
                        ui.set_update_ready(false);
                        ui.set_update_status(
                            "上次待安装的更新已失效并被清理；可重新检查更新".into(),
                        );
                    });
                }
            }
        });
    }

    /// 新版启动后的一次性回执：只在 helper 记录的成功结果与当前版本一致时提示一次。
    pub(crate) fn show_startup_receipt(&self) {
        let Some(version) = updater::consume_upgrade_receipt(&self.data_root) else {
            return;
        };
        let window = self.window.clone();
        if let Some(ui) = window.upgrade() {
            ui.set_update_status(format!("已更新到 {version}").into());
            Self::flash_note(&window, format!("已更新到 {version}"), NoteTone::Positive);
        }
    }

    /// 自动检查（启动或驻留周期）：读取设置与节流后才发起；手动检查不受节流限制。
    /// 启动路径使用更短的阈值，避免「刚检查完就发布新版本」让用户干等一个周期。
    pub(crate) fn auto_check_if_due(self: &Arc<Self>, settings: &AppSettings, startup: bool) {
        if !settings.automatic_updates_enabled || !self.update_configured || self.skip_update_check
        {
            return;
        }
        let now = chrono::Utc::now();
        let due = if startup {
            updater::startup_check_due_from_state(&self.data_root, now)
        } else {
            updater::automatic_check_due_from_state(&self.data_root, now)
        };
        if !due {
            return;
        }
        // 自动检查开始时即记录本次尝试时间；失败也等待下一个周期。
        updater::record_automatic_check(&self.data_root);
        let controller = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            controller.check(false);
        });
    }

    /// `force=true` 为手动检查：绕过缓存与节流，并把失败原因显示给用户。
    pub(crate) fn check(self: &Arc<Self>, force: bool) {
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.check_busy)) else {
            if force && let Some(ui) = self.window.upgrade() {
                ui.set_update_status("已有更新检查正在运行".into());
            }
            return;
        };
        if force && let Some(ui) = self.window.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
            ui.set_update_note("正在检查更新…".into());
            ui.set_update_note_tone(NoteTone::Neutral as i32);
        }
        let window = self.window.clone();
        let available = Arc::clone(&self.available);
        let data_root = self.data_root.clone();
        let controller = Arc::clone(self);
        let auto_download_enabled = self
            .runtime
            .settings()
            .map(|settings| settings.automatic_update_download_enabled)
            .unwrap_or(false);
        std::thread::spawn(move || {
            let result = updater::check_for_update_with_cache(force);
            drop(gate);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = window.upgrade() else { return };
                match result {
                    Ok(updater::UpdateCheck::UpToDate) => {
                        if let Ok(mut value) = available.lock() {
                            *value = None;
                        }
                        controller.manual_only.store(false, Ordering::Release);
                        controller.sync_tray_badge(None);
                        ui.set_update_available(false);
                        ui.set_update_version("".into());
                        ui.set_update_release_notes_url("".into());
                        if force {
                            let current = env!("CARGO_PKG_VERSION");
                            ui.set_update_status(format!("当前已是最新版本 {current}").into());
                            Self::flash_note(
                                &window,
                                format!("已是最新版本 v{current}"),
                                NoteTone::Positive,
                            );
                        } else if !ui.get_update_ready() {
                            ui.set_update_status(
                                format!("当前已是最新版本 {}", env!("CARGO_PKG_VERSION")).into(),
                            );
                        }
                    }
                    Ok(updater::UpdateCheck::Available(update)) => {
                        let version = update.manifest.version.clone();
                        let release_notes =
                            updater::release_notes_url(&update.manifest).unwrap_or_default();
                        if let Ok(mut value) = available.lock() {
                            *value = Some(update);
                        }
                        controller.manual_only.store(false, Ordering::Release);
                        ui.set_update_available(true);
                        ui.set_update_version(version.clone().into());
                        ui.set_update_release_notes_url(release_notes.into());
                        ui.set_update_status(
                            format!("发现已签名更新 {version}，点击更新后自动重启升级").into(),
                        );
                        let dismissed = updater::banner_dismissed_for(&data_root, &version);
                        if dismissed {
                            Self::clear_pill(&ui);
                        } else {
                            Self::publish_available(&ui, &version);
                        }
                        controller.sync_tray_badge((!dismissed).then_some(version.as_str()));
                        if !force
                            && auto_download_enabled
                            && !controller.ready.load(Ordering::Acquire)
                        {
                            // 已授权预取：先把更新包下载好，点击时可直接安装。
                            controller.download(DownloadTrigger::Prefetch);
                        }
                    }
                    Err(error) => {
                        let message = format!("检查更新失败：{error:#}");
                        if force {
                            // 手动检查：失败必须是可读的，并尽量给出手动下载兜底。
                            match updater::probe_latest_release_tag()
                                .ok()
                                .and_then(|tag| updater::higher_release_version(&tag))
                            {
                                Some(version) => {
                                    controller.manual_only.store(true, Ordering::Release);
                                    controller.sync_tray_badge(None);
                                    ui.set_update_available(false);
                                    ui.set_update_version(version.clone().into());
                                    ui.set_update_status(
                                        format!("无法验证更新清单；发现 {version}，可手动下载")
                                            .into(),
                                    );
                                    Self::publish_manual_only(&ui, &version);
                                }
                                None => {
                                    controller.sync_tray_badge(None);
                                    ui.set_update_available(false);
                                    ui.set_update_status(message.clone().into());
                                    Self::flash_note(&window, message, NoteTone::Error);
                                }
                            }
                        } else {
                            operations::log(
                                "WARN",
                                &format!("自动检查更新失败：{}", operations::redact(&message)),
                            );
                        }
                    }
                }
            });
        });
    }

    /// 下载更新；`OneClick` 在验证完成后立即进入安装，`Prefetch` 只准备好待安装文件。
    pub(crate) fn download(self: &Arc<Self>, trigger: DownloadTrigger) {
        if self.ready.load(Ordering::Acquire) {
            // 已经下载并验证过：点击时直接进入安装，不再重复下载。
            if trigger == DownloadTrigger::OneClick {
                self.install();
            }
            return;
        }
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.download_busy)) else {
            if trigger == DownloadTrigger::OneClick
                && let Some(ui) = self.window.upgrade()
            {
                ui.set_update_status("已有更新下载任务正在运行".into());
            }
            return;
        };
        let Some(update) = self
            .available
            .lock()
            .ok()
            .and_then(|value| value.as_ref().cloned())
        else {
            drop(gate);
            if let Some(ui) = self.window.upgrade() {
                ui.set_update_status("没有可下载的已验证更新，请先检查更新".into());
            }
            return;
        };
        if let Some(ui) = self.window.upgrade() {
            ui.set_update_downloading(true);
            ui.set_update_download_failed(false);
            ui.set_update_download_progress(0);
            ui.set_update_status("正在下载并验证更新安装包…".into());
            Self::publish_downloading(&ui, &update.manifest.version, 0, None);
        }
        let window = self.window.clone();
        let data_root = self.data_root.clone();
        let controller = Arc::clone(self);
        let progress_state = Arc::new(Mutex::new(-1i32));
        let progress_window = self.window.clone();
        let progress = move |downloaded: u64, total: u64| {
            let percent = if total == 0 {
                0
            } else {
                ((downloaded.saturating_mul(100)) / total) as i32
            };
            let mut last = progress_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *last == percent {
                return;
            }
            *last = percent;
            let progress_window = progress_window.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = progress_window.upgrade() {
                    ui.set_update_download_progress(percent);
                    ui.set_update_status(format!("正在下载更新 {percent}%").into());
                    Self::publish_downloading(&ui, "", percent, Some((downloaded, total)));
                }
            });
        };
        std::thread::spawn(move || {
            let result = updater::download_and_verify_update(&data_root, &update, Some(&progress));
            drop(gate);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = window.upgrade() else { return };
                ui.set_update_downloading(false);
                ui.set_update_download_progress(-1);
                match result {
                    Ok(pending) => {
                        let version = pending.manifest.version.clone();
                        controller.ready.store(true, Ordering::Release);
                        ui.set_update_ready(true);
                        ui.set_update_version(version.clone().into());
                        ui.set_update_status(
                            format!("{version} 已下载并验证，点击更新后自动重启升级").into(),
                        );
                        let dismissed = updater::banner_dismissed_for(&data_root, &version);
                        if dismissed {
                            Self::clear_pill(&ui);
                        } else {
                            Self::publish_ready(&ui, &version);
                        }
                        controller.sync_tray_badge((!dismissed).then_some(version.as_str()));
                        if trigger == DownloadTrigger::OneClick {
                            controller.install();
                        }
                    }
                    Err(error) => {
                        ui.set_update_download_failed(true);
                        let message = format!("更新下载或验证失败：{error:#}");
                        ui.set_update_status(message.clone().into());
                        Self::publish_failed(&ui, &update.manifest.version, &message);
                    }
                }
            });
        });
    }

    /// 退出主程序并把安装交给 helper；新版由 helper 以托盘方式启动。
    pub(crate) fn install(self: &Arc<Self>) {
        if self.installing.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.install_busy)) else {
            self.installing.store(false, Ordering::Release);
            if let Some(ui) = self.window.upgrade() {
                ui.set_update_status("已有更新安装任务正在运行".into());
            }
            return;
        };
        self.sync_tray_badge(None);
        if let Some(ui) = self.window.upgrade() {
            let version = ui.get_update_version().to_string();
            ui.set_update_status("已下载并验证，正在退出并安装…".into());
            Self::publish_installing(&ui, &version);
        }
        let window = self.window.clone();
        let data_root = self.data_root.clone();
        std::thread::spawn(move || {
            let result = updater::request_install(&data_root);
            drop(gate);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = window.upgrade() else { return };
                match result {
                    Ok(detail) => {
                        ui.set_update_status(detail.into());
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        ui.set_update_status(format!("无法启动更新安装：{error:#}").into());
                        let _ = slint::quit_event_loop();
                    }
                }
            });
        });
    }

    /// 用户选择「稍后」：只隐藏当前版本，更高版本会重新展示。
    pub(crate) fn dismiss(&self, version: &str) {
        if version.is_empty() {
            return;
        }
        updater::dismiss_banner_for(&self.data_root, version);
        self.sync_tray_badge(None);
        if let Some(ui) = self.window.upgrade() {
            Self::clear_pill(&ui);
        }
    }

    /// 清单不可用时打开手动下载页（只读兜底，不进入安装路径）。
    pub(crate) fn open_manual_download(&self) {
        let Some(ui) = self.window.upgrade() else {
            return;
        };
        let target = ui.get_update_release_notes_url();
        if target.is_empty() {
            ui.set_status_text("当前没有可用的发布页地址".into());
            return;
        }
        ui.set_status_text(match windows_integration::open_external(&target) {
            Ok(()) => "已使用默认浏览器打开更新下载页".into(),
            Err(error) => format!("无法打开更新下载页：{error:#}").into(),
        });
    }

    /// 长驻时的周期再检查：复用 Slint 事件循环定时器，不新增长驻线程。
    /// 返回的 Timer 必须由调用方持有，否则会被立即销毁。
    /// 闭包只持 Weak，避免应用退出时定时器在主线程 TLS 析构中释放
    /// Arc<UpdateController>（内含托盘句柄）进而触发托盘线程 join 崩溃。
    pub(crate) fn start_recheck_timer(self: &Arc<Self>) -> Timer {
        let weak = Arc::downgrade(self);
        let timer = Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1800),
            move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                std::thread::spawn(move || match controller.runtime.settings() {
                    Ok(settings) => controller.auto_check_if_due(&settings, false),
                    Err(error) => {
                        operations::log("WARN", &format!("更新周期检查读取设置失败：{error:#}"))
                    }
                });
            },
        );
        timer
    }

    /// 用户点击绿色胶囊：兜底状态下只打开下载页，正常状态下一次性完成下载与安装。
    pub(crate) fn request_update(self: &Arc<Self>) {
        if self.manual_only.load(Ordering::Acquire) {
            self.open_manual_download();
            return;
        }
        self.download(DownloadTrigger::OneClick);
    }

    /// 同步托盘菜单的更新徽标：有可执行更新时显示版本，其余情况清空。
    fn sync_tray_badge(&self, label: Option<&str>) {
        #[cfg(windows)]
        self.tray.set_update_badge(label);
        #[cfg(not(windows))]
        let _ = label;
    }

    fn clear_pill(ui: &MainWindow) {
        ui.set_update_pill_visible(false);
        ui.set_update_pill_label("".into());
        ui.set_update_note("".into());
        ui.set_update_progress(-1);
    }

    fn publish_available(ui: &MainWindow, version: &str) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_label(format!("更新 {version}").into());
        ui.set_update_pill_enabled(true);
        ui.set_update_pill_manual_only(false);
        ui.set_update_note(format!("发现 {version}，点击后自动下载并重启升级").into());
        ui.set_update_note_tone(NoteTone::Neutral as i32);
        ui.set_update_progress(-1);
    }

    fn publish_manual_only(ui: &MainWindow, version: &str) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_label(format!("打开下载页").into());
        ui.set_update_pill_enabled(true);
        ui.set_update_pill_manual_only(true);
        ui.set_update_note(format!("无法验证更新清单；发现 {version}，请手动下载安装").into());
        ui.set_update_note_tone(NoteTone::Warning as i32);
        ui.set_update_progress(-1);
    }

    fn publish_downloading(
        ui: &MainWindow,
        version: &str,
        percent: i32,
        bytes: Option<(u64, u64)>,
    ) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_enabled(false);
        ui.set_update_pill_manual_only(false);
        if !version.is_empty() {
            ui.set_update_pill_label(format!("更新 {version}").into());
        } else {
            ui.set_update_pill_label(format!("下载 {percent}%").into());
        }
        ui.set_update_note(
            match bytes {
                Some((downloaded, total)) => format!(
                    "正在下载并验证 · {} / {}",
                    format_bytes(downloaded),
                    format_bytes(total)
                ),
                None => "正在下载并验证更新安装包…".into(),
            }
            .into(),
        );
        ui.set_update_note_tone(NoteTone::Neutral as i32);
        ui.set_update_progress(percent);
    }

    fn publish_ready(ui: &MainWindow, version: &str) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_label(format!("更新 {version}").into());
        ui.set_update_pill_enabled(true);
        ui.set_update_pill_manual_only(false);
        ui.set_update_note(format!("{version} 已就绪，点击后立即重启升级").into());
        ui.set_update_note_tone(NoteTone::Positive as i32);
        ui.set_update_progress(-1);
    }

    fn publish_installing(ui: &MainWindow, version: &str) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_label("正在重启更新…".into());
        ui.set_update_pill_enabled(false);
        ui.set_update_pill_manual_only(false);
        ui.set_update_note(
            if version.is_empty() {
                "已下载并验证，正在退出并安装".to_owned()
            } else {
                format!("{version} 正在安装，程序将自动重启到托盘")
            }
            .into(),
        );
        ui.set_update_note_tone(NoteTone::Positive as i32);
        ui.set_update_progress(-1);
    }

    fn publish_failed(ui: &MainWindow, version: &str, message: &str) {
        ui.set_update_pill_visible(true);
        ui.set_update_pill_label(format!("重试").into());
        ui.set_update_pill_enabled(true);
        ui.set_update_pill_manual_only(false);
        ui.set_update_note(message.to_owned().into());
        ui.set_update_note_tone(NoteTone::Error as i32);
        ui.set_update_progress(-1);
        if !version.is_empty() {
            ui.set_update_version(version.to_owned().into());
        }
    }

    /// 展示一条会自动消失的细行提示（检查结果、升级回执）。
    fn flash_note(window: &slint::Weak<MainWindow>, message: String, tone: NoteTone) {
        if let Some(ui) = window.upgrade() {
            ui.set_update_note(message.into());
            ui.set_update_note_tone(tone as i32);
            ui.set_update_progress(-1);
        }
        let weak = window.clone();
        Timer::single_shot(LATEST_NOTE_TTL, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_update_note("".into());
            }
        });
    }
}

/// 细行提示的语义色调。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteTone {
    Neutral,
    Positive,
    Warning,
    Error,
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub(crate) struct OperationGate(Arc<AtomicBool>);

impl OperationGate {
    pub(crate) fn acquire(flag: Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag))
    }
}

impl Drop for OperationGate {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) fn start_crash_upload(
    window: slint::Weak<MainWindow>,
    data_root: PathBuf,
    busy: Arc<AtomicBool>,
    automatic: bool,
) {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if let Some(ui) = window.upgrade() {
            ui.set_crash_upload_status("已有一项崩溃报告发送任务正在运行".into());
        }
        return;
    }
    if let Some(ui) = window.upgrade() {
        ui.set_crash_upload_busy(true);
    }
    std::thread::spawn(move || {
        let result = crash_upload::upload_next(&data_root);
        match &result {
            Ok(outcome) => operations::log("INFO", &outcome.message()),
            Err(error) => operations::log(
                "WARN",
                &format!(
                    "崩溃报告发送失败：{}",
                    operations::redact(&format!("{error:#}"))
                ),
            ),
        }
        busy.store(false, Ordering::Release);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = window.upgrade() else { return };
            ui.set_crash_upload_busy(false);
            ui.set_crash_upload_status(match result {
                Ok(outcome) => outcome.message().into(),
                Err(error) if automatic => format!("自动发送崩溃报告失败：{error:#}").into(),
                Err(error) => format!("发送崩溃报告失败：{error:#}").into(),
            });
        });
    });
}

pub(crate) fn start_secondary_notification_test(
    window: slint::Weak<MainWindow>,
    data_root: PathBuf,
    provider: SecondaryNotificationProvider,
    busy: Arc<AtomicBool>,
    runtime: RuntimeHandle,
) {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if let Some(ui) = window.upgrade() {
            ui.set_secondary_notification_status("已有第二通知通道测试正在运行".into());
        }
        return;
    }
    let attempt_id = match runtime.reserve_secondary_notification_test(provider) {
        Ok(Some(value)) => value,
        Ok(None) => {
            busy.store(false, Ordering::Release);
            if let Some(ui) = window.upgrade() {
                ui.set_secondary_notification_status(
                    "过去 1 小时已发送 20 个第二通知通道批次，请稍后再测试".into(),
                );
            }
            return;
        }
        Err(error) => {
            busy.store(false, Ordering::Release);
            if let Some(ui) = window.upgrade() {
                ui.set_secondary_notification_status(
                    format!("无法读取第二通知通道配额：{error:#}").into(),
                );
            }
            return;
        }
    };
    if let Some(ui) = window.upgrade() {
        ui.set_secondary_notification_busy(true);
    }
    std::thread::spawn(move || {
        let result = secondary_notification::send_test(&data_root, provider);
        let record_error = result
            .as_ref()
            .err()
            .map(|error| operations::redact(&format!("{error:#}")));
        if let Err(error) =
            runtime.finish_secondary_notification_test(attempt_id, record_error.as_deref())
        {
            operations::log("WARN", &format!("无法记录第二通知通道测试配额：{error:#}"));
        }
        match &result {
            Ok(receipt) => operations::log("INFO", &receipt.message()),
            Err(error) => operations::log("WARN", &format!("第二通知通道用户测试失败：{error:#}")),
        }
        busy.store(false, Ordering::Release);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = window.upgrade() else { return };
            ui.set_secondary_notification_busy(false);
            ui.set_secondary_notification_status(match result {
                Ok(receipt) => format!("{}；请在目标应用中确认消息内容", receipt.message()).into(),
                Err(error) => format!(
                    "第二通知通道测试失败：{}",
                    operations::redact(&format!("{error:#}"))
                )
                .into(),
            });
        });
    });
}

pub(crate) fn record_notification_test_result(
    runtime: &RuntimeHandle,
    channel: i32,
    passed: bool,
) -> Result<()> {
    let mut settings = runtime.settings()?;
    match channel {
        0 => settings.notification_window_test_passed = Some(passed),
        1 => settings.notification_toast_test_passed = Some(passed),
        2 => settings.notification_balloon_test_passed = Some(passed),
        3 => settings.notification_sound_test_passed = Some(passed),
        _ => anyhow::bail!("提醒通道测试类型无效"),
    }
    settings.notification_self_test_completed = settings.notification_tests_complete();
    settings.onboarding_completed = settings.notification_self_test_completed;
    runtime.save_settings(&settings)
}

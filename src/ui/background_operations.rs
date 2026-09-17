use std::path::Path;

use super::*;

/// 更新流程的 UI 编排：检查、下载、安装、pending 恢复与 24 小时节流。
/// 状态机：idle -> checking -> available -> downloading -> ready -> installing。
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
    #[cfg(windows)]
    tray: Arc<native_tray::NativeTray>,
}

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
            #[cfg(windows)]
            tray,
        }
    }

    /// 启动即恢复已验证的待安装更新；损坏或过期的 pending 安全清理。
    pub(crate) fn restore_pending(&self) {
        let window = self.window.clone();
        let data_root = self.data_root.clone();
        std::thread::spawn(move || match updater::verify_pending(&data_root) {
            Ok(pending) => {
                let version = pending.manifest.version.clone();
                let manifest = pending.manifest.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = window.upgrade() else { return };
                    ui.set_update_ready(true);
                    ui.set_update_version(version.clone().into());
                    ui.set_update_release_notes_url(
                        updater::release_notes_url(&manifest)
                            .unwrap_or_default()
                            .into(),
                    );
                    ui.set_update_status(
                        format!("检测到已验证的待安装更新 {version}，可重启并更新").into(),
                    );
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

    /// 自动检查（含启动与驻留周期）：读取设置与 24 小时节流后才发起；
    /// 手动检查不受节流限制。检查本身必须回到 UI 事件循环发起。
    pub(crate) fn auto_check_if_due(self: &Arc<Self>, settings: &AppSettings) {
        if !settings.automatic_updates_enabled || !self.update_configured || self.skip_update_check
        {
            return;
        }
        if !updater::automatic_check_due_from_state(&self.data_root, chrono::Utc::now()) {
            return;
        }
        // 自动检查开始时即记录本次尝试时间；失败也等待下一个周期。
        updater::record_automatic_check(&self.data_root);
        let controller = Arc::clone(self);
        let _ = slint::invoke_from_event_loop(move || {
            controller.check(true);
        });
    }

    pub(crate) fn check(self: &Arc<Self>, automatic: bool) {
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.check_busy)) else {
            if !automatic && let Some(ui) = self.window.upgrade() {
                ui.set_update_status("已有更新检查正在运行".into());
            }
            return;
        };
        if let Some(ui) = self.window.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
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
        let notify = self.update_notifier();
        std::thread::spawn(move || {
            let result = updater::check_for_update();
            drop(gate);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = window.upgrade() else { return };
                match result {
                    Ok(updater::UpdateCheck::UpToDate) => {
                        if let Ok(mut value) = available.lock() {
                            *value = None;
                        }
                        ui.set_update_available(false);
                        ui.set_update_version("".into());
                        ui.set_update_release_notes_url("".into());
                        if !ui.get_update_ready() {
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
                        ui.set_update_available(true);
                        ui.set_update_version(version.clone().into());
                        ui.set_update_release_notes_url(release_notes.into());
                        ui.set_update_status(
                            format!("发现已签名更新 {version}，可下载并安装").into(),
                        );
                        if automatic && auto_download_enabled {
                            // 已授权自动下载：跳过“发现”提醒，下载验证完成后再提醒“已准备好”。
                            controller.download(true);
                        } else if automatic && updater::should_notify_version(&data_root, &version)
                        {
                            updater::mark_version_notified(&data_root, &version);
                            notify(&data_root, &version, false);
                        }
                    }
                    Err(error) => {
                        ui.set_update_available(false);
                        let message = format!("检查更新失败：{error:#}");
                        if automatic {
                            operations::log(
                                "WARN",
                                &format!("自动检查更新失败：{}", operations::redact(&message)),
                            );
                        }
                        if !ui.get_update_ready() {
                            ui.set_update_status(message.into());
                        }
                    }
                }
            });
        });
    }

    pub(crate) fn download(&self, automatic: bool) {
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.download_busy)) else {
            if !automatic && let Some(ui) = self.window.upgrade() {
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
        }
        let window = self.window.clone();
        let data_root = self.data_root.clone();
        let notify = self.update_notifier();
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
                        ui.set_update_ready(true);
                        ui.set_update_version(version.clone().into());
                        ui.set_update_status(
                            format!("{version} 已下载并验证；点击“重启并更新”完成升级").into(),
                        );
                        if automatic && updater::should_notify_version(&data_root, &version) {
                            updater::mark_version_notified(&data_root, &version);
                            notify(&data_root, &version, true);
                        }
                    }
                    Err(error) => {
                        ui.set_update_download_failed(true);
                        ui.set_update_status(format!("更新下载或验证失败：{error:#}").into());
                    }
                }
            });
        });
    }

    pub(crate) fn install(&self) {
        let Some(gate) = OperationGate::acquire(Arc::clone(&self.install_busy)) else {
            if let Some(ui) = self.window.upgrade() {
                ui.set_update_status("已有更新安装任务正在运行".into());
            }
            return;
        };
        if let Some(ui) = self.window.upgrade() {
            ui.set_update_status("正在确认待安装更新…".into());
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
                        ui.set_update_status(format!("无法启动更新安装：{error:#}").into())
                    }
                }
            });
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
            Duration::from_secs(3600),
            move || {
                let Some(controller) = weak.upgrade() else {
                    return;
                };
                std::thread::spawn(move || match controller.runtime.settings() {
                    Ok(settings) => controller.auto_check_if_due(&settings),
                    Err(error) => {
                        operations::log("WARN", &format!("更新周期检查读取设置失败：{error:#}"))
                    }
                });
            },
        );
        timer
    }

    /// 返回统一的更新提醒闭包：ready=false 提醒“发现新版本”，ready=true 提醒“已准备好”。
    /// Toast 不可用时由托盘自动回退气泡。
    fn update_notifier(&self) -> Arc<dyn Fn(&Path, &str, bool) + Send + Sync> {
        #[cfg(windows)]
        let tray = Arc::clone(&self.tray);
        let runtime = self.runtime.clone();
        Arc::new(move |data_root: &Path, version: &str, ready: bool| {
            let _ = data_root;
            let title = if ready {
                "A 股打新提醒 · 更新已就绪"
            } else {
                "A 股打新提醒 · 发现新版本"
            };
            let body = if ready {
                format!("新版本 {version} 已下载并验证；点击这里后可选择重启并更新。")
            } else {
                format!("新版本 {version} 已发布；点击这里查看更新详情。")
            };
            let activation = if ready {
                updater::ACTIVATION_UPDATE_READY
            } else {
                updater::ACTIVATION_UPDATE_AVAILABLE
            };
            #[cfg(windows)]
            {
                // 与提醒呈现一致的 fail-closed：设置读取失败时不打扰用户。
                match runtime.settings() {
                    Ok(settings) if settings.toast_enabled => {
                        tray.notify(title, &body, Some(activation));
                    }
                    Ok(_) => {}
                    Err(error) => operations::log(
                        "WARN",
                        &format!("更新提醒读取通知设置失败，跳过提醒：{error:#}"),
                    ),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (title, body, activation, &runtime);
            }
        })
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

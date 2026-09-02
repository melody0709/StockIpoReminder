use super::*;

pub(crate) fn start_update_check(
    window: slint::Weak<MainWindow>,
    state: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    busy: Arc<AtomicBool>,
    automatic: bool,
) {
    let Some(gate) = OperationGate::acquire(busy) else {
        if !automatic && let Some(ui) = window.upgrade() {
            ui.set_update_status("已有更新检查正在运行".into());
        }
        return;
    };
    std::thread::spawn(move || {
        let result = updater::check_for_update();
        drop(gate);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = window.upgrade() else { return };
            match result {
                Ok(updater::UpdateCheck::UpToDate) => {
                    if let Ok(mut value) = state.lock() {
                        *value = None;
                    }
                    ui.set_update_available(false);
                    ui.set_update_version("".into());
                    ui.set_update_status(
                        format!("当前已是最新版本 {}", env!("CARGO_PKG_VERSION")).into(),
                    );
                }
                Ok(updater::UpdateCheck::Available(update)) => {
                    let version = update.manifest.version.clone();
                    if let Ok(mut value) = state.lock() {
                        *value = Some(update);
                    }
                    ui.set_update_available(true);
                    ui.set_update_version(version.clone().into());
                    ui.set_update_status(
                        if automatic {
                            format!("发现已签名更新 {version}，请在设置页确认安装")
                        } else {
                            format!("发现已签名更新 {version}，可下载并安装")
                        }
                        .into(),
                    );
                }
                Err(error) => {
                    ui.set_update_available(false);
                    ui.set_update_status(format!("检查更新失败：{error:#}").into());
                }
            }
        });
    });
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

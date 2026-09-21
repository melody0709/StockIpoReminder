use super::*;

#[cfg(windows)]
static MAIN_WINDOW_NATIVE_PREPARED: AtomicBool = AtomicBool::new(false);

/// 等待原生窗口尺寸与 Slint 缓存对齐的轮询节奏：间隔与最大次数。
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(120);
const SETTLE_POLL_ATTEMPTS: usize = 8;

#[derive(Default)]
pub(crate) struct RuntimeUiBridgeState {
    startup_applied: bool,
    last_ui_revision: Option<u64>,
    #[cfg(windows)]
    last_tray_status: Option<(i64, bool)>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_runtime_ui_bridge(
    window: slint::Weak<MainWindow>,
    reminder_window: slint::Weak<ReminderWindow>,
    runtime: RuntimeHandle,
    data_root: PathBuf,
    update_controller: Arc<UpdateController>,
    crash_upload_busy: Arc<AtomicBool>,
    crash_upload_configured: bool,
    skip_auto_start_registration: bool,
    skip_crash_upload: bool,
    #[cfg(windows)] tray: Arc<native_tray::NativeTray>,
) {
    let callback_pending = Arc::new(AtomicBool::new(false));
    let bridge_state = Arc::new(Mutex::new(RuntimeUiBridgeState::default()));
    let notifier_runtime = runtime.clone();
    runtime.install_ui_notifier(move || {
        if callback_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let pending_for_callback = Arc::clone(&callback_pending);
        let pending_on_error = Arc::clone(&callback_pending);
        let window = window.clone();
        let reminder_window = reminder_window.clone();
        let runtime = notifier_runtime.clone();
        let data_root = data_root.clone();
        let update_controller = Arc::clone(&update_controller);
        let crash_upload_busy = Arc::clone(&crash_upload_busy);
        let bridge_state = Arc::clone(&bridge_state);
        #[cfg(windows)]
        let tray = Arc::clone(&tray);
        let queued = slint::invoke_from_event_loop(move || {
            pending_for_callback.store(false, Ordering::Release);
            drain_runtime_ui(
                &window,
                &reminder_window,
                &runtime,
                &data_root,
                &update_controller,
                &crash_upload_busy,
                crash_upload_configured,
                skip_auto_start_registration,
                skip_crash_upload,
                &bridge_state,
                #[cfg(windows)]
                &tray,
            );
        });
        if queued.is_err() {
            pending_on_error.store(false, Ordering::Release);
        }
    });
}

/// 提醒呈现时的副作用开关。独立成结构以便对「设置读取结果 → 副作用」
/// 的决策做单元测试（成功、全部关闭、读取失败 fail-closed）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReminderAlerts {
    pub(crate) sound: bool,
    pub(crate) toast: bool,
}

impl ReminderAlerts {
    pub(crate) fn from_settings(settings: &AppSettings) -> Self {
        Self {
            sound: settings.sound_enabled,
            toast: cfg!(windows) && settings.toast_enabled,
        }
    }

    /// 设置读取失败时默认关闭全部副作用。
    pub(crate) fn fail_closed() -> Self {
        Self::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_runtime_ui(
    window: &slint::Weak<MainWindow>,
    reminder_window: &slint::Weak<ReminderWindow>,
    runtime: &RuntimeHandle,
    data_root: &PathBuf,
    update_controller: &Arc<UpdateController>,
    crash_upload_busy: &Arc<AtomicBool>,
    crash_upload_configured: bool,
    skip_auto_start_registration: bool,
    skip_crash_upload: bool,
    bridge_state: &Arc<Mutex<RuntimeUiBridgeState>>,
    #[cfg(windows)] tray: &Arc<native_tray::NativeTray>,
) {
    let Some(ui) = window.upgrade() else { return };
    let snapshot = runtime.snapshot();
    let mut apply_startup = false;
    let mut refresh = false;
    // revision 只有在刷新真正完成后才提交；提前提交会让失败后的同一 revision
    // 不再触发刷新，任务列表可能长期为空。
    let mut pending_revision = None::<u64>;
    if let Ok(state) = bridge_state.lock() {
        if !state.startup_applied && runtime.is_ready() {
            apply_startup = true;
        }
        if state.last_ui_revision != Some(snapshot.revision) {
            pending_revision = Some(snapshot.revision);
            refresh = true;
        }
    }

    if apply_startup {
        let settings = match runtime.settings() {
            Ok(settings) => settings,
            Err(error) => {
                let message = format!("读取应用设置失败：{error:#}");
                operations::log("ERROR", &message);
                ui.set_status_text(message.into());
                return;
            }
        };
        if let Ok(mut state) = bridge_state.lock() {
            state.startup_applied = true;
        }
        let onboarding_completed = settings.onboarding_completed
            || settings.notification_self_test_completed
            || settings.notification_tests_complete();
        if onboarding_completed && !skip_auto_start_registration {
            match env::current_exe() {
                Ok(executable) => {
                    if let Err(error) = windows_integration::set_auto_start(
                        settings.auto_start_enabled,
                        &executable,
                        data_root,
                    ) {
                        operations::log("WARN", &format!("校准开机自启动失败：{error:#}"));
                    }
                }
                Err(error) => operations::log(
                    "WARN",
                    &format!("读取当前程序路径失败，无法校准开机自启动：{error:#}"),
                ),
            }
        }
        apply_settings(&ui, &settings);
        refresh_secondary_notification_ui(&ui, data_root, &settings, runtime);
        if !onboarding_completed {
            ui.set_active_page(3);
        }
        // 恢复已验证的待安装更新（ready 状态），损坏的 pending 会被安全清理。
        update_controller.restore_pending();
        {
            // 启动 3 秒后按设置与 24 小时节流执行自动检查。
            // 定时器闭包只持 Weak：应用快速退出时该定时器可能永不触发，
            // 其闭包会在主线程 TLS 析构中被 Slint TimerList 释放，届时
            // 不能让 Arc<UpdateController>（内含托盘句柄）成为最后引用，
            // 否则托盘线程 join 会发生在 TLS 析构中导致进程崩溃。
            let controller = Arc::downgrade(update_controller);
            let startup_settings = settings.clone();
            Timer::single_shot(Duration::from_secs(3), move || {
                let Some(controller) = controller.upgrade() else {
                    return;
                };
                controller.auto_check_if_due(&startup_settings, true);
            });
        }
        if settings.crash_report_upload_enabled && crash_upload_configured && !skip_crash_upload {
            let upload_window = ui.as_weak();
            let upload_root = data_root.clone();
            let upload_busy = Arc::clone(crash_upload_busy);
            Timer::single_shot(Duration::from_secs(5), move || {
                start_crash_upload(
                    upload_window.clone(),
                    upload_root.clone(),
                    Arc::clone(&upload_busy),
                    true,
                );
            });
        }
    }
    if refresh
        && refresh_ui(&ui, runtime)
        && let Some(revision) = pending_revision
        && let Ok(mut state) = bridge_state.lock()
    {
        state.last_ui_revision = Some(revision);
    }

    #[cfg(windows)]
    {
        let tray_status = (
            snapshot.pending_count,
            snapshot.last_sync_succeeded == Some(false)
                || snapshot.health_state == HealthState::Failed,
        );
        let update_tray = bridge_state.lock().is_ok_and(|mut state| {
            if state.last_tray_status == Some(tray_status) {
                false
            } else {
                state.last_tray_status = Some(tray_status);
                true
            }
        });
        if update_tray {
            tray.set_status(tray_status.0, tray_status.1);
        }
    }

    let Some(reminder_window) = reminder_window.upgrade() else {
        return;
    };
    let mut deliveries = Vec::new();
    let mut health_summary = None;
    while let Some(event) = runtime.try_event() {
        match event {
            UiEvent::Reminder(delivery) => deliveries.push(delivery),
            UiEvent::Health { state, text } => health_summary = Some((state, text)),
        }
    }
    if !deliveries.is_empty() {
        let batch = reminder_batch(&deliveries, health_summary.as_ref().map(|value| &value.1));
        reminder_window.set_reminder_title(batch.title.clone().into());
        reminder_window.set_reminder_body(batch.body.clone().into());
        reminder_window.set_reminder_event_id(batch.event_id.clone().into());
        reminder_window.set_reminder_event_version(batch.event_version);
        reminder_window.set_batch_count(deliveries.len() as i32);
        reminder_window.set_can_acknowledge(batch.can_acknowledge);
        let shown = show_dedicated_reminder(&reminder_window);
        // 设置读取失败时 fail-closed：不回退默认值，避免在用户明确关闭
        // 声音/Toast 的情况下误触发提醒副作用。
        let alerts = match runtime.settings() {
            Ok(settings) => ReminderAlerts::from_settings(&settings),
            Err(error) => {
                operations::log(
                    "ERROR",
                    &format!("提醒呈现时读取设置失败，跳过声音/Toast 副作用：{error:#}"),
                );
                ReminderAlerts::fail_closed()
            }
        };
        if alerts.sound {
            windows_integration::play_alert();
        }
        #[cfg(windows)]
        if alerts.toast {
            tray.notify(
                &batch.title,
                &batch.body,
                (!batch.event_id.is_empty()).then_some(batch.event_id.as_str()),
            );
        }
        if shown {
            let completion_runtime = runtime.clone();
            let visibility_window = reminder_window.as_weak();
            Timer::single_shot(Duration::from_millis(150), move || {
                let visibility = visibility_window
                    .upgrade()
                    .context("专用提醒窗口已被销毁")
                    .and_then(|window| {
                        windows_integration::confirm_window_visible(window.window())
                    });
                match visibility {
                    Ok(()) => {
                        for delivery in deliveries {
                            if let Err(error) = completion_runtime.complete_delivery(&delivery) {
                                operations::log(
                                    "ERROR",
                                    &format!("提醒可见后完成 Outbox 投递状态失败：{error:#}"),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        let message = format!("提醒窗口可见性确认失败：{error:#}");
                        operations::log("ERROR", &message);
                        for delivery in deliveries {
                            if let Err(fail_error) =
                                completion_runtime.fail_delivery(&delivery, &message)
                            {
                                operations::log(
                                    "ERROR",
                                    &format!("记录提醒投递失败状态时出错：{fail_error:#}"),
                                );
                            }
                        }
                    }
                }
            });
        } else {
            // 窗口完全无法显示：既不能完成也不能保持租约，走既有 fail_delivery
            // 退避与 last_error 记录，避免 2 分钟租约到期后无退避地无限重试。
            for delivery in &deliveries {
                if let Err(error) =
                    runtime.fail_delivery(delivery, "专用提醒窗口显示失败，已记录并按既有退避重试")
                {
                    operations::log("ERROR", &format!("记录提醒投递失败状态时出错：{error:#}"));
                }
            }
        }
    } else if let Some((state, text)) = health_summary {
        reminder_window.set_reminder_title(match state {
            HealthState::Failed => "每日健康摘要 · 需要处理".into(),
            HealthState::Warning => "每日健康摘要 · 存在警告".into(),
            _ => "每日健康摘要".into(),
        });
        reminder_window.set_reminder_body(text.clone().into());
        reminder_window.set_reminder_event_id("".into());
        reminder_window.set_reminder_event_version(0);
        reminder_window.set_batch_count(0);
        reminder_window.set_can_acknowledge(false);
        let _ = show_dedicated_reminder(&reminder_window);
        #[cfg(windows)]
        if runtime
            .settings()
            .map(|settings| settings.toast_enabled)
            .unwrap_or_else(|error| {
                operations::log(
                    "ERROR",
                    &format!("健康摘要读取通知设置失败，仍尝试 Toast：{error:#}"),
                );
                true
            })
        {
            tray.notify("A 股打新提醒 · 健康摘要", &text, None);
        }
    }
}

pub(crate) fn schedule_reminder_window_smoke(
    reminder: slint::Weak<ReminderWindow>,
    report_path: PathBuf,
) {
    Timer::single_shot(Duration::from_millis(100), move || {
        let Some(window) = reminder.upgrade() else {
            let _ = write_reminder_window_smoke_report(
                &report_path,
                false,
                false,
                None,
                Some("提醒窗口对象已销毁"),
            );
            let _ = slint::quit_event_loop();
            return;
        };
        window.set_reminder_title("发布验证 · 专用提醒窗口".into());
        window.set_reminder_body("该窗口用于验证置顶、工作区定位、可见性以及不抢键盘焦点。".into());
        window.set_reminder_event_id("".into());
        window.set_reminder_event_version(0);
        window.set_batch_count(0);
        window.set_can_acknowledge(false);
        let shown = show_dedicated_reminder(&window);
        let verify = window.as_weak();
        Timer::single_shot(Duration::from_millis(350), move || {
            let Some(window) = verify.upgrade() else {
                let _ = write_reminder_window_smoke_report(
                    &report_path,
                    shown,
                    false,
                    None,
                    Some("提醒窗口在验证前已销毁"),
                );
                let _ = slint::quit_event_loop();
                return;
            };
            let visibility = windows_integration::confirm_window_visible(window.window());
            let foreground = windows_integration::window_is_foreground(window.window());
            let error = visibility
                .as_ref()
                .err()
                .map(|value| format!("{value:#}"))
                .or_else(|| foreground.as_ref().err().map(|value| format!("{value:#}")));
            let _ = write_reminder_window_smoke_report(
                &report_path,
                shown,
                visibility.is_ok(),
                foreground.ok(),
                error.as_deref(),
            );
            let _ = window.hide();
            let _ = slint::quit_event_loop();
        });
    });
}

pub(crate) fn write_reminder_window_smoke_report(
    path: &std::path::Path,
    show_succeeded: bool,
    visible_in_work_area: bool,
    became_foreground: Option<bool>,
    error: Option<&str>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let success = show_succeeded && visible_in_work_area && became_foreground == Some(false);
    let report = serde_json::json!({
        "schemaVersion": "1",
        "success": success,
        "version": env!("CARGO_PKG_VERSION"),
        "generatedAt": core::now_china().to_rfc3339(),
        "showSucceeded": show_succeeded,
        "visibleInWorkArea": visible_in_work_area,
        "becameForeground": became_foreground,
        "noFocusSteal": became_foreground == Some(false),
        "error": error.map(operations::redact)
    });
    fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

pub(crate) fn force_full_repaint(window: &MainWindow) {
    window.set_repaint_token(window.get_repaint_token().wrapping_add(1));
    window.window().request_redraw();
}

pub(crate) fn show_and_repaint(window: &MainWindow) {
    // 首帧尺寸必须在 show() 之前定下来：此时原生窗口尚未创建，winit 会把尺寸写进窗口属性，
    // 窗口"一出生"就是目标尺寸，不再有"先按最小尺寸显示、再被程序改大"的过程。
    if let Some(size) = apply_initial_main_window_size(window) {
        operations::log(
            "INFO",
            &format!(
                "主窗口首次显示前已设定尺寸：logicalWidth={} logicalHeight={} event=main_window_size_preapplied",
                size.width, size.height
            ),
        );
    }
    if let Err(error) = window.show() {
        operations::log("ERROR", &format!("无法显示主窗口：{error}"));
        return;
    }
    #[cfg(windows)]
    if MAIN_WINDOW_NATIVE_PREPARED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(50), move || {
            let Some(window) = weak.upgrade() else {
                MAIN_WINDOW_NATIVE_PREPARED.store(false, Ordering::Release);
                return;
            };
            apply_restored_main_window_size(&window);
            let work_area_ready = windows_integration::fit_window_to_work_area(window.window())
                .map_or_else(
                    |error| {
                        operations::log(
                            "WARN",
                            &format!("首次打开主窗口时调整工作区失败：{error:#}"),
                        );
                        false
                    },
                    |()| true,
                );
            let icon_ready = windows_integration::install_window_icon(window.window()).map_or_else(
                |error| {
                    operations::log("WARN", &format!("首次打开主窗口时设置图标失败：{error:#}"));
                    false
                },
                |()| true,
            );
            // 常驻托盘期间显示缩放变化时，窗口缓存的 DPI 不会刷新；此时界面会按旧缩放渲染，
            // 只能重启程序恢复。这里只记录一次，便于用户与支持人员定位。
            if let Some((window_dpi, monitor_dpi)) =
                windows_integration::window_dpi_mismatch(window.window())
            {
                operations::log(
                    "WARN",
                    &format!(
                        "主窗口缩放已过期：windowDpi={window_dpi} monitorDpi={monitor_dpi}，界面按旧缩放渲染，重启程序可恢复 event=main_window_dpi_stale"
                    ),
                );
            }
            if !(work_area_ready && icon_ready) {
                MAIN_WINDOW_NATIVE_PREPARED.store(false, Ordering::Release);
            }
        });
    }
    force_full_repaint(window);
    schedule_settled_repaint(window);
}

/// 等原生窗口尺寸与 Slint 缓存对齐后，再整窗重绘一次。
///
/// 原生窗口与软件渲染器要经过几次 Windows 消息才对齐；对齐之前提交的重绘覆盖不到新暴露的
/// 区域，那块区域会一直空着（透出桌面），只有下一次真正的尺寸变化才会把它画上。
fn schedule_settled_repaint(window: &MainWindow) {
    poll_settled_repaint(window.as_weak(), 0);
}

fn poll_settled_repaint(window: slint::Weak<MainWindow>, attempt: usize) {
    Timer::single_shot(SETTLE_POLL_INTERVAL, move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        let settled = windows_integration::window_size_settled(window.window());
        if settled || attempt + 1 >= SETTLE_POLL_ATTEMPTS {
            force_full_repaint(&window);
            return;
        }
        poll_settled_repaint(window.as_weak(), attempt + 1);
    });
}

pub(crate) struct ReminderBatch {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) event_id: String,
    pub(crate) event_version: i32,
    pub(crate) can_acknowledge: bool,
}

pub(crate) fn reminder_batch(
    deliveries: &[ReminderDelivery],
    health_summary: Option<&String>,
) -> ReminderBatch {
    let mut grouped = BTreeMap::<String, (ReminderDelivery, usize)>::new();
    for delivery in deliveries {
        let entry = grouped
            .entry(delivery.event.id.clone())
            .or_insert_with(|| (delivery.clone(), 0));
        entry.1 += 1;
        if reminder_display_priority(delivery.level) > reminder_display_priority(entry.0.level) {
            entry.0 = delivery.clone();
        }
    }

    let mut batch = if grouped.len() == 1 {
        let (delivery, count) = grouped.into_values().next().unwrap();
        let title = if count > 1 {
            format!(
                "{}（{}）{} · 合并 {count} 条",
                delivery.event.name,
                delivery.event.display_code(),
                reminder_title_text(delivery.level),
            )
        } else {
            format!(
                "{}（{}）{}",
                delivery.event.name,
                delivery.event.display_code(),
                reminder_title_text(delivery.level),
            )
        };
        let mut body = reminder_body(&delivery.event, delivery.level, delivery.message.as_deref());
        if count > 1 {
            body.push_str(&format!("\n\n已合并该任务的 {count} 个到期提醒。"));
        }
        ReminderBatch {
            title,
            body,
            event_id: delivery.event.id.clone(),
            event_version: delivery.event.event_version,
            can_acknowledge: is_pending(&delivery.event),
        }
    } else {
        let unique_count = grouped.len();
        let mut lines = Vec::new();
        for (_, (delivery, count)) in grouped {
            let summary = delivery
                .message
                .as_deref()
                .and_then(|message| message.lines().next())
                .unwrap_or_else(|| reminder_level_text(delivery.level));
            let merged = if count > 1 {
                format!("，合并 {count} 条")
            } else {
                String::new()
            };
            lines.push(format!(
                "• {}（{}）：{}{}",
                delivery.event.name,
                delivery.event.display_code(),
                summary,
                merged
            ));
        }
        ReminderBatch {
            title: format!("{unique_count} 只新股 · {} 条提醒", deliveries.len()),
            body: format!(
                "{}\n\n请打开今日任务逐只核对；程序不会把批量显示视为已申购。",
                lines.join("\n")
            ),
            event_id: String::new(),
            event_version: 0,
            can_acknowledge: false,
        }
    };
    if let Some(summary) = health_summary {
        batch.body.push_str("\n\n系统健康摘要：");
        batch.body.push_str(summary);
    }
    batch
}

pub(crate) fn reminder_display_priority(level: ReminderLevel) -> i32 {
    match level {
        ReminderLevel::Final => i32::MAX,
        ReminderLevel::DataChanged => ReminderLevel::Final as i32 - 1,
        _ => level as i32,
    }
}

pub(crate) fn show_dedicated_reminder(window: &ReminderWindow) -> bool {
    let shown = match windows_integration::show_reminder_window(window.window()) {
        Ok(()) => {
            let _ = windows_integration::install_window_icon(window.window());
            true
        }
        Err(error) => {
            operations::log("ERROR", &format!("专用提醒窗口无激活显示失败：{error:#}"));
            window.show().is_ok()
        }
    };
    if shown {
        force_reminder_repaint(window);

        // SetWindowPos and the software renderer can finish the first native
        // show on different Windows messages. Invalidate the whole reminder
        // once more after that transition so no stale backing buffer remains.
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(50), move || {
            if let Some(window) = weak.upgrade() {
                force_reminder_repaint(&window);
            }
        });
    }
    shown
}

pub(crate) fn force_reminder_repaint(window: &ReminderWindow) {
    window.set_repaint_token(window.get_repaint_token().wrapping_add(1));
    window.window().request_redraw();
}

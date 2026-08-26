#![cfg_attr(windows, windows_subsystem = "windows")]

mod announcement;
mod core;
mod crash_upload;
mod deployment;
mod model;
mod network;
mod operations;
mod runtime;
mod secondary_notification;
mod storage;
mod updater;
mod watchdog;
mod windows_integration;

use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use model::{
    AppSettings, Board, DataQualityStatus, Exchange, HealthState, IpoEvent, LifecycleStatus,
    ReminderDelivery, ReminderLevel, SecondaryNotificationProvider,
};
use runtime::{RuntimeHandle, UiEvent};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, Timer, TimerMode, VecModel};

slint::include_modules!();

fn main() -> Result<()> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if let Some(exit_code) = announcement::try_run_pdf_worker(&arguments)? {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = deployment::try_handle(&arguments)? {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = updater::try_handle(&arguments)? {
        std::process::exit(exit_code);
    }

    let options = RuntimeOptions::parse(&arguments);
    fs::create_dir_all(&options.data_root)
        .with_context(|| format!("无法创建数据目录：{}", options.data_root.display()))?;
    operations::initialize(&options.data_root)?;
    if let Some(exit_code) = operations::try_run_self_test(&arguments, &options.data_root)? {
        std::process::exit(exit_code);
    }

    if watchdog::should_supervise(&arguments) {
        let Some(_supervisor) =
            windows_integration::SingleInstance::try_acquire_supervisor(&options.data_root)?
        else {
            activate_existing_instance(&options.data_root);
            return Ok(());
        };
        if windows_integration::application_instance_running(&options.data_root)? {
            activate_existing_instance(&options.data_root);
            return Ok(());
        }
        return watchdog::supervise(&arguments, &options.data_root);
    }

    let _instance = windows_integration::SingleInstance::acquire(&options.data_root)?;
    run_application(options)
}

fn activate_existing_instance(data_root: &std::path::Path) {
    for _ in 0..30 {
        if windows_integration::application_instance_running(data_root).unwrap_or(false) {
            match windows_integration::request_activate_existing(data_root) {
                Ok(()) => operations::log("INFO", "第二次启动已请求现有实例显示主窗口"),
                Err(error) => {
                    operations::log("ERROR", &format!("第二次启动无法唤醒现有实例：{error:#}"))
                }
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    operations::log(
        "WARN",
        "检测到 Watchdog 已存在，但主程序在 3 秒内尚未建立单实例锁",
    );
}

fn run_application(options: RuntimeOptions) -> Result<()> {
    let reminder_window_smoke_report = options.reminder_window_smoke_report.clone();
    let windows_recovery_smoke_report = options.windows_recovery_smoke_report.clone();
    if let Err(error) = windows_integration::initialize_notification_platform() {
        operations::log(
            "WARN",
            &format!("Windows Toast 平台初始化失败，将在需要时回退托盘气泡：{error:#}"),
        );
    }
    let (runtime, runtime_thread) =
        runtime::start(options.data_root.clone(), !options.skip_startup_sync)?;

    let ui = MainWindow::new().context("无法创建 Slint 主窗口")?;
    ui.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let reminder_window = ReminderWindow::new().context("无法创建专用提醒窗口")?;
    reminder_window
        .window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    ui.set_data_root_text(format!("数据目录：{}", options.data_root.display()).into());
    let msi_installed = match deployment::installed_msi_product_code() {
        Ok(Some(_)) => {
            ui.set_uninstall_available(true);
            ui.set_uninstall_status(
                "已检测到 Windows Installer 安装，可在此安全卸载；默认保留当前用户数据。".into(),
            );
            true
        }
        Ok(None) => {
            ui.set_uninstall_available(false);
            ui.set_uninstall_status(
                "当前运行副本未检测到已注册的 MSI；便携版请直接退出后删除程序文件。".into(),
            );
            false
        }
        Err(error) => {
            ui.set_uninstall_available(false);
            ui.set_uninstall_status(format!("无法读取 MSI 安装状态：{error:#}").into());
            false
        }
    };
    let update_configured = updater::configured() && msi_installed;
    ui.set_update_configured(update_configured);
    ui.set_update_status(
        updater::last_result(&options.data_root)
            .unwrap_or_else(|| {
                if updater::configured() && !msi_installed {
                    "安全自动更新仅对已安装的 MSI 版本启用；便携版请手动下载并核对发布包。".into()
                } else {
                    updater::configuration_status()
                }
            })
            .into(),
    );
    let crash_upload_configured = crash_upload::configured();
    ui.set_crash_upload_configured(crash_upload_configured);
    ui.set_crash_upload_privacy_url(crash_upload::privacy_url().unwrap_or_default().into());
    ui.set_crash_upload_status(
        crash_upload::last_result(&options.data_root)
            .unwrap_or_else(crash_upload::configuration_status)
            .into(),
    );
    let initial_settings = runtime.settings().unwrap_or_default();
    if initial_settings.onboarding_completed && !options.skip_auto_start_registration {
        if let Err(error) = windows_integration::set_auto_start(
            initial_settings.auto_start_enabled,
            &env::current_exe()?,
            &options.data_root,
        ) {
            operations::log("WARN", &format!("校准开机自启动失败：{error:#}"));
        }
    }
    apply_settings(&ui, &initial_settings);
    refresh_secondary_notification_ui(&ui, &options.data_root, &initial_settings, &runtime);
    if !initial_settings.onboarding_completed {
        ui.set_active_page(3);
    }
    refresh_ui(&ui, &runtime);
    let available_update = Arc::new(Mutex::new(None::<updater::AvailableUpdate>));
    let crash_upload_busy = Arc::new(AtomicBool::new(false));
    let secondary_notification_busy = Arc::new(AtomicBool::new(false));

    #[cfg(windows)]
    let tray = Arc::new(
        native_tray::NativeTray::start(
            ui.as_weak(),
            runtime.clone(),
            options.data_root.clone(),
            options.windows_recovery_smoke_report.is_some(),
        )
        .context("无法创建 windows-rs 系统托盘")?,
    );

    #[cfg(windows)]
    wire_callbacks(
        &ui,
        &reminder_window,
        runtime.clone(),
        options.data_root.clone(),
        Arc::clone(&available_update),
        Arc::clone(&crash_upload_busy),
        Arc::clone(&secondary_notification_busy),
        Arc::clone(&tray),
    );
    #[cfg(not(windows))]
    wire_callbacks(
        &ui,
        &reminder_window,
        runtime.clone(),
        options.data_root.clone(),
        Arc::clone(&available_update),
        Arc::clone(&crash_upload_busy),
        Arc::clone(&secondary_notification_busy),
    );
    wire_reminder_callbacks(&reminder_window, &ui, runtime.clone());

    if initial_settings.automatic_updates_enabled && update_configured && !options.skip_update_check
    {
        let update_window = ui.as_weak();
        let update_state = Arc::clone(&available_update);
        Timer::single_shot(Duration::from_secs(3), move || {
            start_update_check(update_window.clone(), Arc::clone(&update_state), true);
        });
    }
    if initial_settings.crash_report_upload_enabled
        && crash_upload_configured
        && !options.skip_crash_upload
    {
        let upload_window = ui.as_weak();
        let upload_root = options.data_root.clone();
        let upload_busy = Arc::clone(&crash_upload_busy);
        Timer::single_shot(Duration::from_secs(5), move || {
            start_crash_upload(
                upload_window.clone(),
                upload_root.clone(),
                Arc::clone(&upload_busy),
                true,
            );
        });
    }

    let weak = ui.as_weak();
    let reminder_weak = reminder_window.as_weak();
    let polling_runtime = runtime.clone();
    #[cfg(windows)]
    let polling_tray = Arc::clone(&tray);
    let polling_data_root = options.data_root.clone();
    let polling_secondary_busy = Arc::clone(&secondary_notification_busy);
    let polling_timer = Timer::default();
    let mut secondary_status_tick = 0_u8;
    polling_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(ui) = weak.upgrade() else { return };
        refresh_ui(&ui, &polling_runtime);
        secondary_status_tick = (secondary_status_tick + 1) % 5;
        if ui.get_active_page() == 3
            && secondary_status_tick == 0
            && !polling_secondary_busy.load(Ordering::Acquire)
        {
            let settings = polling_runtime.settings().unwrap_or_default();
            refresh_secondary_notification_ui(&ui, &polling_data_root, &settings, &polling_runtime);
        }
        #[cfg(windows)]
        {
            let snapshot = polling_runtime.snapshot();
            polling_tray.set_status(
                snapshot.pending_count,
                snapshot.last_sync_succeeded == Some(false)
                    || snapshot.health_state == HealthState::Failed,
            );
        }
        let Some(reminder_window) = reminder_weak.upgrade() else {
            return;
        };
        let mut deliveries = Vec::new();
        let mut health_summary = None;
        while let Some(event) = polling_runtime.try_event() {
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
            let settings = polling_runtime.settings().unwrap_or_default();
            if settings.sound_enabled {
                windows_integration::play_alert();
            }
            if settings.flash_taskbar {
                windows_integration::flash_window(reminder_window.window());
            }
            #[cfg(windows)]
            if settings.toast_enabled {
                polling_tray.notify(
                    &batch.title,
                    &batch.body,
                    (!batch.event_id.is_empty()).then_some(batch.event_id.as_str()),
                );
            }
            if shown {
                let completion_runtime = polling_runtime.clone();
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
                                if let Err(error) = completion_runtime.complete_delivery(&delivery)
                                {
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
            if polling_runtime.settings().unwrap_or_default().toast_enabled {
                polling_tray.notify("A 股打新提醒 · 健康摘要", &text, None);
            }
        }
    });

    ui.show().context("无法显示主窗口")?;
    #[cfg(windows)]
    {
        let icon_window = ui.as_weak();
        Timer::single_shot(Duration::from_millis(50), move || {
            if let Some(window) = icon_window.upgrade() {
                let _ = windows_integration::fit_window_to_work_area(window.window());
                let _ = windows_integration::install_window_icon(window.window());
            }
        });
    }
    if options.background {
        ui.hide().context("无法隐藏主窗口")?;
    }
    if let Some(exit_after) = options.exit_after {
        Timer::single_shot(exit_after, || {
            let _ = slint::quit_event_loop();
        });
    }
    if let Some(report_path) = reminder_window_smoke_report {
        schedule_reminder_window_smoke(reminder_window.as_weak(), report_path);
    }
    #[cfg(windows)]
    if let Some(report_path) = windows_recovery_smoke_report {
        tray.schedule_recovery_smoke(report_path)?;
    }

    // The application is tray-resident: hiding the last visible window must not
    // terminate Slint's event loop. Only an explicit quit action should exit.
    let run_result = slint::run_event_loop_until_quit().context("Slint 事件循环异常");
    let _ = ui.hide();
    let _ = reminder_window.hide();
    runtime.stop();
    let _ = runtime_thread.join();
    drop(polling_timer);
    run_result
}

fn schedule_reminder_window_smoke(reminder: slint::Weak<ReminderWindow>, report_path: PathBuf) {
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

fn write_reminder_window_smoke_report(
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

fn force_full_repaint(window: &MainWindow) {
    window.set_repaint_token(window.get_repaint_token().wrapping_add(1));
    window.window().request_redraw();
}

fn show_and_repaint(window: &MainWindow) {
    let _ = window.show();
    force_full_repaint(window);

    // The native window and the software renderer finish restoring on
    // different Windows messages. Re-mark the complete root dirty after that
    // transition so a recycled backing buffer cannot leak through the UI.
    let weak = window.as_weak();
    Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(window) = weak.upgrade() {
            force_full_repaint(&window);
        }
    });
}

struct ReminderBatch {
    title: String,
    body: String,
    event_id: String,
    event_version: i32,
    can_acknowledge: bool,
}

fn reminder_batch(
    deliveries: &[ReminderDelivery],
    health_summary: Option<&String>,
) -> ReminderBatch {
    let mut grouped = BTreeMap::<String, (ReminderDelivery, usize)>::new();
    for delivery in deliveries {
        let entry = grouped
            .entry(delivery.event.id.clone())
            .or_insert_with(|| (delivery.clone(), 0));
        entry.1 += 1;
        if delivery.level as i32 > entry.0.level as i32 {
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

fn show_dedicated_reminder(window: &ReminderWindow) -> bool {
    match windows_integration::show_reminder_window(window.window()) {
        Ok(()) => {
            let _ = windows_integration::install_window_icon(window.window());
            true
        }
        Err(error) => {
            operations::log("ERROR", &format!("专用提醒窗口无激活显示失败：{error:#}"));
            window.show().is_ok()
        }
    }
}

fn wire_reminder_callbacks(
    reminder: &ReminderWindow,
    main_window: &MainWindow,
    runtime: RuntimeHandle,
) {
    let reminder_weak = reminder.as_weak();
    let main_weak = main_window.as_weak();
    let details_runtime = runtime.clone();
    reminder.on_open_details(move |event_id| {
        if let Some(reminder) = reminder_weak.upgrade() {
            let _ = reminder.hide();
        }
        if let Some(main_window) = main_weak.upgrade() {
            main_window.set_active_page(0);
            show_and_repaint(&main_window);
            if !event_id.is_empty() {
                show_event_details(&main_window, &details_runtime, event_id.as_str());
            }
        }
    });

    let reminder_weak = reminder.as_weak();
    let main_weak = main_window.as_weak();
    reminder.on_request_acknowledgement(move |event_id, event_version| {
        if let Some(reminder) = reminder_weak.upgrade() {
            let _ = reminder.hide();
        }
        if let Some(main_window) = main_weak.upgrade() {
            main_window.set_confirmation_is_revoke(false);
            main_window.set_confirmation_event_id(event_id);
            main_window.set_confirmation_event_version(event_version);
            main_window.set_confirmation_title("二次确认：已经提交申购委托？".into());
            main_window.set_confirmation_body(
                "请确认已经在券商客户端提交申购委托。本程序不会检查委托是否受理或成功；确认后停止申购日重复提醒，并按已启用设置安排中签、缴款和上市日提示。".into(),
            );
            main_window.set_show_confirmation(true);
            show_and_repaint(&main_window);
        }
    });

    let reminder_weak = reminder.as_weak();
    reminder.on_dismiss(move || {
        if let Some(reminder) = reminder_weak.upgrade() {
            let _ = reminder.hide();
        }
    });
}

fn wire_callbacks(
    ui: &MainWindow,
    reminder_window: &ReminderWindow,
    runtime: RuntimeHandle,
    data_root: PathBuf,
    available_update: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    crash_upload_busy: Arc<AtomicBool>,
    secondary_notification_busy: Arc<AtomicBool>,
    #[cfg(windows)] tray: Arc<native_tray::NativeTray>,
) {
    let weak = ui.as_weak();
    let settings_runtime = runtime.clone();
    let settings_data_root = data_root.clone();
    ui.on_save_settings(move || {
        let Some(ui) = weak.upgrade() else { return };
        let result = (|| -> Result<()> {
            if !ui.get_shanghai_enabled() && !ui.get_shenzhen_enabled() && !ui.get_beijing_enabled()
            {
                anyhow::bail!("至少需要启用一个市场");
            }
            let mut settings = settings_runtime.settings().unwrap_or_default();
            settings.auto_start_enabled = ui.get_auto_start();
            settings.shanghai_enabled = ui.get_shanghai_enabled();
            settings.shenzhen_enabled = ui.get_shenzhen_enabled();
            settings.beijing_enabled = ui.get_beijing_enabled();
            settings.shanghai_broker_accept_start =
                parse_time(ui.get_shanghai_start().as_str(), "沪市券商受理开始")?;
            settings.shenzhen_broker_accept_start =
                parse_time(ui.get_shenzhen_start().as_str(), "深市券商受理开始")?;
            settings.beijing_broker_accept_start =
                parse_time(ui.get_beijing_start().as_str(), "北交所券商受理开始")?;
            settings.safety_cutoff = parse_time(ui.get_safety_cutoff().as_str(), "安全截止时间")?;
            if settings.safety_cutoff < NaiveTime::from_hms_opt(13, 0, 0).unwrap()
                || settings.safety_cutoff > NaiveTime::from_hms_opt(15, 0, 0).unwrap()
            {
                anyhow::bail!("安全截止时间应在 13:00 到 15:00 之间，建议使用 14:55");
            }
            settings.beijing_reservation_supported = ui.get_beijing_reservation();
            settings.sound_enabled = ui.get_sound_enabled();
            settings.flash_taskbar = ui.get_flash_taskbar();
            settings.toast_enabled = ui.get_toast_enabled();
            settings.daily_health_summary_enabled = ui.get_health_summary_enabled();
            settings.post_apply_reminders_enabled = ui.get_post_apply_reminders_enabled();
            settings.listing_reminders_enabled = ui.get_listing_reminders_enabled();
            settings.automatic_updates_enabled = ui.get_automatic_updates_enabled();
            settings.crash_report_upload_enabled = ui.get_crash_upload_enabled();
            let secondary_provider =
                secondary_provider_from_index(ui.get_secondary_notification_provider_index());
            let secondary_secret = ui
                .get_secondary_notification_secret_entry()
                .trim()
                .to_owned();
            if !secondary_secret.is_empty() {
                secondary_notification::save_secret(
                    &settings_data_root,
                    secondary_provider,
                    &secondary_secret,
                )?;
            }
            settings.secondary_notification_provider = secondary_provider;
            settings.secondary_notification_enabled = ui.get_secondary_notification_enabled()
                && secondary_provider != SecondaryNotificationProvider::Disabled;
            if settings.secondary_notification_enabled
                && !secondary_notification::configured(&settings_data_root, &settings)
            {
                anyhow::bail!("启用第二通知通道前必须保存与当前服务商匹配的有效凭据");
            }
            if settings.notification_tests_started() {
                settings.notification_self_test_completed = settings.notification_tests_complete();
                settings.onboarding_completed = settings.notification_self_test_completed;
            }
            let normal_sync_minutes = parse_sync_interval(
                ui.get_normal_sync_interval_value().as_str(),
                ui.get_normal_sync_interval_unit_index(),
                "普通日期自动同步间隔",
            )?;
            let active_day_sync_minutes = parse_sync_interval(
                ui.get_active_sync_interval_value().as_str(),
                ui.get_active_sync_interval_unit_index(),
                "申购日自动同步间隔",
            )?;
            if active_day_sync_minutes > normal_sync_minutes {
                anyhow::bail!("申购日自动同步间隔不能大于普通日期间隔");
            }
            settings.normal_sync_minutes = normal_sync_minutes;
            settings.active_day_sync_minutes = active_day_sync_minutes;
            settings_runtime.save_settings(&settings)?;
            windows_integration::set_auto_start(
                settings.auto_start_enabled,
                &env::current_exe()?,
                &settings_data_root,
            )?;
            settings_runtime.request_sync("设置变更");
            Ok(())
        })();
        let saved = result.is_ok();
        ui.set_status_text(match result {
            Ok(()) => "设置已保存，提醒计划已重算".into(),
            Err(error) => format!("保存设置失败：{error:#}").into(),
        });
        let saved_settings = settings_runtime.settings().unwrap_or_default();
        apply_settings(&ui, &saved_settings);
        if saved {
            ui.set_secondary_notification_secret_entry("".into());
        }
        refresh_secondary_notification_ui(
            &ui,
            &settings_data_root,
            &saved_settings,
            &settings_runtime,
        );
        refresh_ui(&ui, &settings_runtime);
    });

    let sync_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_sync_now(move || {
        sync_runtime.request_sync("用户手动同步");
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text("已提交手动同步请求…".into());
        }
    });

    let refresh_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_refresh_data(move || {
        if let Some(ui) = weak.upgrade() {
            refresh_ui(&ui, &refresh_runtime);
        }
    });

    let select_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_select_task(move |event_id| {
        let Some(ui) = weak.upgrade() else { return };
        show_event_details(&ui, &select_runtime, event_id.as_str());
    });

    let acknowledge_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_acknowledge(move |event_id, version| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_status_text(
            match acknowledge_runtime.acknowledge(event_id.as_str(), version) {
                Ok(()) => "已记录确认，当前版本后续提醒已取消".into(),
                Err(error) => format!("确认失败：{error:#}").into(),
            },
        );
        refresh_ui(&ui, &acknowledge_runtime);
    });

    let revoke_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_revoke_acknowledgement(move |event_id, version| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_status_text(
            match revoke_runtime.revoke_acknowledgement(event_id.as_str(), version) {
                Ok(()) => "已撤销确认，截止时间前的提醒已重新规划".into(),
                Err(error) => format!("撤销失败：{error:#}").into(),
            },
        );
        refresh_ui(&ui, &revoke_runtime);
    });

    let override_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_apply_override(
        move |event_id, version, field_index, value, reason, announcement_index| {
            let Some(ui) = weak.upgrade() else { return };
            let field = override_field_name(field_index);
            let announcements = override_runtime
                .announcements(event_id.as_str())
                .unwrap_or_default();
            let announcement_id = (announcement_index > 0)
                .then(|| announcements.get((announcement_index - 1) as usize))
                .flatten()
                .map(|document| document.id.as_str());
            match override_runtime.apply_override(
                event_id.as_str(),
                version,
                field,
                value.as_str(),
                reason.as_str(),
                announcement_id,
            ) {
                Ok(()) => {
                    ui.set_status_text("人工覆盖已保存，并已重新规划提醒".into());
                    ui.set_override_status("人工覆盖已保存，提醒计划已重算".into());
                    ui.set_override_value("".into());
                    ui.set_override_reason("".into());
                }
                Err(error) => {
                    ui.set_status_text(format!("保存人工覆盖失败：{error:#}").into());
                    ui.set_override_status(format!("保存失败：{error:#}").into());
                }
            }
            refresh_ui(&ui, &override_runtime);
            show_event_details(&ui, &override_runtime, event_id.as_str());
            ui.set_details_active_tab(3);
        },
    );

    let revoke_override_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_revoke_override(move |event_id, version, override_id| {
        let Some(ui) = weak.upgrade() else { return };
        let result = override_id
            .as_str()
            .parse::<i64>()
            .context("人工覆盖记录编号无效")
            .and_then(|override_id| {
                revoke_override_runtime.revoke_override(event_id.as_str(), version, override_id)
            });
        match result {
            Ok(()) => {
                ui.set_status_text("人工覆盖已撤销，并已重新规划提醒".into());
                ui.set_override_status("人工覆盖已撤销，提醒计划已重算".into());
            }
            Err(error) => {
                ui.set_status_text(format!("撤销人工覆盖失败：{error:#}").into());
                ui.set_override_status(format!("撤销失败：{error:#}").into());
            }
        }
        refresh_ui(&ui, &revoke_override_runtime);
        show_event_details(&ui, &revoke_override_runtime, event_id.as_str());
        ui.set_details_active_tab(3);
    });

    let weak = ui.as_weak();
    ui.on_open_external(move |target| {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(match windows_integration::open_external(target.as_str()) {
                Ok(()) => "已使用默认浏览器打开公告原文".into(),
                Err(error) => format!("打开公告原文失败：{error:#}").into(),
            });
        }
    });

    let weak = ui.as_weak();
    ui.on_open_local(move |target| {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(
                match windows_integration::open_local_file(std::path::Path::new(target.as_str())) {
                    Ok(()) => "已使用默认程序打开本地公告".into(),
                    Err(error) => format!("打开本地公告失败：{error:#}").into(),
                },
            );
        }
    });

    let diagnostic_runtime = runtime.clone();
    let diagnostic_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_create_diagnostics(move || {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_status_text(
            match operations::create_diagnostic_bundle(
                &diagnostic_root,
                diagnostic_runtime.database(),
            ) {
                Ok(path) => {
                    if let Some(directory) = path.parent() {
                        let _ = windows_integration::open_folder(directory);
                    }
                    format!("诊断包已生成：{}", path.display()).into()
                }
                Err(error) => format!("诊断包生成失败：{error:#}").into(),
            },
        );
    });

    let open_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_open_data_folder(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(match windows_integration::open_folder(&open_root) {
                Ok(()) => "已打开数据目录".into(),
                Err(error) => format!("打开数据目录失败：{error:#}").into(),
            });
        }
    });

    let copy_runtime = runtime.clone();
    let copy_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_copy_diagnostics(move || {
        if let Some(ui) = weak.upgrade() {
            let summary = diagnostic_summary(&copy_runtime, &copy_root);
            ui.set_status_text(match windows_integration::copy_text(&summary) {
                Ok(()) => "诊断摘要已复制到剪贴板".into(),
                Err(error) => format!("复制诊断摘要失败：{error:#}").into(),
            });
        }
    });

    #[cfg(windows)]
    let test_tray = Arc::clone(&tray);
    let test_runtime = runtime.clone();
    let weak = ui.as_weak();
    let test_reminder = reminder_window.as_weak();
    ui.on_test_notification_channel(move |channel| {
        let Some(ui) = weak.upgrade() else { return };
        let (title, question) = match channel {
            0 => {
                if let Some(reminder) = test_reminder.upgrade() {
                    reminder.set_reminder_title("置顶提醒窗口测试".into());
                    reminder.set_reminder_body(
                        "这是独立于主窗口的置顶提醒。它应显示在工作区右下角，并且不应抢走你当前正在输入内容的窗口焦点。".into(),
                    );
                    reminder.set_reminder_event_id("".into());
                    reminder.set_reminder_event_version(0);
                    reminder.set_batch_count(0);
                    reminder.set_can_acknowledge(false);
                    let _ = show_dedicated_reminder(&reminder);
                }
                (
                    "确认置顶提醒窗口测试",
                    "你是否看到了右下角的独立置顶提醒窗口？",
                )
            }
            1 => {
                if let Err(error) = windows_integration::show_windows_toast(
                    "A 股打新提醒 · Windows Toast 测试",
                    "这是原生 Windows Toast 测试消息；若系统通知被关闭，正式提醒会自动回退到托盘气泡。",
                ) {
                    let save_result =
                        record_notification_test_result(&test_runtime, channel, false);
                    apply_settings(&ui, &test_runtime.settings().unwrap_or_default());
                    ui.set_status_text(match save_result {
                        Ok(()) => format!(
                            "Windows Toast 未能提交：{error:#}；可继续测试托盘气泡回退"
                        )
                        .into(),
                        Err(save_error) => format!(
                            "Windows Toast 未能提交：{error:#}；保存测试结果也失败：{save_error:#}"
                        )
                        .into(),
                    });
                    return;
                }
                (
                    "确认 Windows Toast 测试",
                    "你是否看到了由 Windows 通知中心显示的 Toast 测试消息？",
                )
            }
            2 => {
                #[cfg(windows)]
                test_tray.notify_balloon(
                    "A 股打新提醒 · 托盘气泡测试",
                    "这是 Windows Toast 不可用时使用的托盘气泡回退测试消息",
                    None,
                );
                (
                    "确认托盘气泡回退测试",
                    "你是否看到了 Windows 托盘区域弹出的回退测试气泡？",
                )
            }
            3 => {
                windows_integration::play_alert();
                ("确认声音测试", "你是否听到了 Windows 提示音？")
            }
            4 => {
                windows_integration::flash_window(ui.window());
                (
                    "确认任务栏闪烁测试",
                    "你是否观察到本程序的任务栏按钮闪烁或突出显示？",
                )
            }
            _ => return,
        };
        ui.set_notification_test_channel(channel);
        ui.set_notification_test_title(title.into());
        ui.set_notification_test_question(question.into());
        ui.set_show_notification_confirmation(true);
        show_and_repaint(&ui);
    });

    let complete_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_complete_notification_test(move |channel, passed| {
        let Some(ui) = weak.upgrade() else { return };
        let result = record_notification_test_result(&complete_runtime, channel, passed);
        if result.is_ok() {
            apply_settings(&ui, &complete_runtime.settings().unwrap_or_default());
        } else if let Err(error) = result {
            ui.set_notification_test_status(format!("保存测试结果失败：{error:#}").into());
        }
    });

    let check_window = ui.as_weak();
    let check_state = Arc::clone(&available_update);
    ui.on_check_for_updates(move || {
        if let Some(ui) = check_window.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
            ui.set_update_available(false);
        }
        start_update_check(check_window.clone(), Arc::clone(&check_state), false);
    });

    let install_window = ui.as_weak();
    let install_state = Arc::clone(&available_update);
    let update_root = data_root.clone();
    ui.on_install_update(move || {
        let Some(ui) = install_window.upgrade() else {
            return;
        };
        let update = install_state
            .lock()
            .ok()
            .and_then(|value| value.as_ref().cloned());
        let Some(update) = update else {
            ui.set_update_status("没有可安装且已验证的更新".into());
            return;
        };
        ui.set_update_status(format!("正在下载并验证 {} 安装包…", update.manifest.version).into());
        let result_window = install_window.clone();
        let data_root = update_root.clone();
        std::thread::spawn(move || {
            let result = updater::download_and_request_install(&data_root, &update);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = result_window.upgrade() {
                    match result {
                        Ok(detail) => {
                            ui.set_update_status(detail.into());
                            let _ = slint::quit_event_loop();
                        }
                        Err(error) => {
                            ui.set_update_status(format!("更新下载或验证失败：{error:#}").into())
                        }
                    }
                }
            });
        });
    });

    let crash_window = ui.as_weak();
    let crash_root = data_root.clone();
    let crash_busy = Arc::clone(&crash_upload_busy);
    ui.on_upload_crash_report(move || {
        if let Some(ui) = crash_window.upgrade() {
            ui.set_crash_upload_status("正在二次脱敏并发送最近的崩溃报告…".into());
        }
        start_crash_upload(
            crash_window.clone(),
            crash_root.clone(),
            Arc::clone(&crash_busy),
            false,
        );
    });

    let privacy_window = ui.as_weak();
    ui.on_open_crash_upload_privacy(move || {
        if let Some(ui) = privacy_window.upgrade() {
            ui.set_status_text(match crash_upload::privacy_url() {
                Some(url) => match windows_integration::open_external(url) {
                    Ok(()) => "已使用默认浏览器打开崩溃报告隐私政策".into(),
                    Err(error) => format!("无法打开隐私政策：{error:#}").into(),
                },
                None => "当前构建未配置崩溃报告隐私政策".into(),
            });
        }
    });

    let secondary_test_window = ui.as_weak();
    let secondary_test_root = data_root.clone();
    let secondary_test_runtime = runtime.clone();
    let secondary_test_busy = Arc::clone(&secondary_notification_busy);
    ui.on_test_secondary_notification(move || {
        let settings = secondary_test_runtime.settings().unwrap_or_default();
        if let Some(ui) = secondary_test_window.upgrade() {
            ui.set_secondary_notification_status("正在发送用户主动测试消息…".into());
        }
        start_secondary_notification_test(
            secondary_test_window.clone(),
            secondary_test_root.clone(),
            settings.secondary_notification_provider,
            Arc::clone(&secondary_test_busy),
            secondary_test_runtime.clone(),
        );
    });

    let secondary_clear_window = ui.as_weak();
    let secondary_clear_root = data_root.clone();
    let secondary_clear_runtime = runtime.clone();
    ui.on_clear_secondary_notification_secret(move || {
        let Some(ui) = secondary_clear_window.upgrade() else {
            return;
        };
        let result = (|| -> Result<()> {
            secondary_notification::clear_secret(&secondary_clear_root)?;
            let mut settings = secondary_clear_runtime.settings().unwrap_or_default();
            settings.secondary_notification_enabled = false;
            secondary_clear_runtime.save_settings(&settings)?;
            Ok(())
        })();
        let settings = secondary_clear_runtime.settings().unwrap_or_default();
        apply_settings(&ui, &settings);
        refresh_secondary_notification_ui(
            &ui,
            &secondary_clear_root,
            &settings,
            &secondary_clear_runtime,
        );
        ui.set_status_text(match result {
            Ok(()) => "第二通知通道凭据已清除并停止发送".into(),
            Err(error) => format!("清除第二通知通道凭据失败：{error:#}").into(),
        });
    });

    let uninstall_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_request_uninstall(move |purge_data, confirmation| {
        let Some(ui) = weak.upgrade() else { return };
        ui.set_uninstall_status("正在启动当前用户卸载助手…".into());
        match deployment::request_msi_uninstall(&uninstall_root, purge_data, confirmation.as_str())
        {
            Ok(detail) => {
                ui.set_uninstall_status(detail.into());
                ui.set_show_uninstall_confirmation(false);
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                ui.set_uninstall_status(format!("无法启动卸载：{error:#}").into());
            }
        }
    });

    let weak = ui.as_weak();
    ui.on_hide_to_tray(move || {
        if let Some(ui) = weak.upgrade() {
            let _ = ui.hide();
        }
    });
    ui.on_exit_app(|| {
        let _ = slint::quit_event_loop();
    });
}

fn start_update_check(
    window: slint::Weak<MainWindow>,
    state: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    automatic: bool,
) {
    std::thread::spawn(move || {
        let result = updater::check_for_update();
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

fn start_crash_upload(
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

fn start_secondary_notification_test(
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

fn record_notification_test_result(
    runtime: &RuntimeHandle,
    channel: i32,
    passed: bool,
) -> Result<()> {
    let mut settings = runtime.settings().unwrap_or_default();
    match channel {
        0 => settings.notification_window_test_passed = Some(passed),
        1 => settings.notification_toast_test_passed = Some(passed),
        2 => settings.notification_balloon_test_passed = Some(passed),
        3 => settings.notification_sound_test_passed = Some(passed),
        4 => settings.notification_flash_test_passed = Some(passed),
        _ => anyhow::bail!("提醒通道测试类型无效"),
    }
    settings.notification_self_test_completed = settings.notification_tests_complete();
    settings.onboarding_completed = settings.notification_self_test_completed;
    runtime.save_settings(&settings)
}

fn parse_time(value: &str, field: &str) -> Result<NaiveTime> {
    let mut parts = value.trim().split(':');
    let hour: u32 = parts
        .next()
        .context("缺少小时")?
        .parse()
        .with_context(|| format!("{field} 的小时无效"))?;
    let minute: u32 = parts
        .next()
        .context("缺少分钟")?
        .parse()
        .with_context(|| format!("{field} 的分钟无效"))?;
    if parts.next().is_some() {
        anyhow::bail!("{field} 必须使用 HH:mm 格式");
    }
    NaiveTime::from_hms_opt(hour, minute, 0)
        .with_context(|| format!("{field} 必须使用有效的 HH:mm 时间"))
}

fn parse_sync_interval(value: &str, unit_index: i32, field: &str) -> Result<i32> {
    let amount: i32 = value
        .trim()
        .parse()
        .with_context(|| format!("{field}必须是整数"))?;
    if amount <= 0 {
        anyhow::bail!("{field}必须大于 0");
    }
    let minutes = match unit_index {
        0 => amount,
        1 => amount
            .checked_mul(60)
            .with_context(|| format!("{field}超出可用范围"))?,
        _ => anyhow::bail!("{field}单位无效"),
    };
    if !(5..=7 * 24 * 60).contains(&minutes) {
        anyhow::bail!("{field}应在 5 分钟到 7 天之间");
    }
    Ok(minutes)
}

fn sync_interval_display(minutes: i32) -> (String, i32) {
    let minutes = minutes.clamp(5, 7 * 24 * 60);
    if minutes % 60 == 0 {
        ((minutes / 60).to_string(), 1)
    } else {
        (minutes.to_string(), 0)
    }
}

fn apply_settings(ui: &MainWindow, settings: &AppSettings) {
    ui.set_auto_start(settings.auto_start_enabled);
    ui.set_shanghai_enabled(settings.shanghai_enabled);
    ui.set_shenzhen_enabled(settings.shenzhen_enabled);
    ui.set_beijing_enabled(settings.beijing_enabled);
    ui.set_shanghai_start(
        settings
            .shanghai_broker_accept_start
            .format("%H:%M")
            .to_string()
            .into(),
    );
    ui.set_shenzhen_start(
        settings
            .shenzhen_broker_accept_start
            .format("%H:%M")
            .to_string()
            .into(),
    );
    ui.set_beijing_start(
        settings
            .beijing_broker_accept_start
            .format("%H:%M")
            .to_string()
            .into(),
    );
    ui.set_safety_cutoff(settings.safety_cutoff.format("%H:%M").to_string().into());
    ui.set_beijing_reservation(settings.beijing_reservation_supported);
    ui.set_sound_enabled(settings.sound_enabled);
    ui.set_flash_taskbar(settings.flash_taskbar);
    ui.set_toast_enabled(settings.toast_enabled);
    ui.set_health_summary_enabled(settings.daily_health_summary_enabled);
    ui.set_post_apply_reminders_enabled(settings.post_apply_reminders_enabled);
    ui.set_listing_reminders_enabled(settings.listing_reminders_enabled);
    ui.set_automatic_updates_enabled(settings.automatic_updates_enabled);
    ui.set_crash_upload_enabled(settings.crash_report_upload_enabled);
    ui.set_secondary_notification_enabled(settings.secondary_notification_enabled);
    ui.set_secondary_notification_provider_index(secondary_provider_index(
        settings.secondary_notification_provider,
    ));
    let (normal_value, normal_unit) = sync_interval_display(settings.normal_sync_minutes);
    ui.set_normal_sync_interval_value(normal_value.into());
    ui.set_normal_sync_interval_unit_index(normal_unit);
    let (active_value, active_unit) = sync_interval_display(settings.active_day_sync_minutes);
    ui.set_active_sync_interval_value(active_value.into());
    ui.set_active_sync_interval_unit_index(active_unit);
    ui.set_notification_test_completed(settings.notification_self_test_completed);
    ui.set_onboarding_completed(settings.onboarding_completed);
    let (window_status, window_level) =
        notification_test_display(settings.notification_window_test_passed, "提醒窗口");
    ui.set_notification_window_test_status(window_status.into());
    ui.set_notification_window_test_level(window_level);
    let (toast_status, toast_level) =
        notification_test_display(settings.notification_toast_test_passed, "Windows Toast");
    ui.set_notification_toast_test_status(toast_status.into());
    ui.set_notification_toast_test_level(toast_level);
    let (balloon_status, balloon_level) =
        notification_test_display(settings.notification_balloon_test_passed, "气泡回退");
    ui.set_notification_balloon_test_status(balloon_status.into());
    ui.set_notification_balloon_test_level(balloon_level);
    let (sound_status, sound_level) =
        notification_test_display(settings.notification_sound_test_passed, "声音");
    ui.set_notification_sound_test_status(sound_status.into());
    ui.set_notification_sound_test_level(sound_level);
    let (flash_status, flash_level) =
        notification_test_display(settings.notification_flash_test_passed, "任务栏闪烁");
    ui.set_notification_flash_test_status(flash_status.into());
    ui.set_notification_flash_test_level(flash_level);
    let (platform_status, platform_level) =
        toast_platform_display(&windows_integration::toast_diagnostics());
    ui.set_notification_platform_status(platform_status.into());
    ui.set_notification_platform_level(platform_level);
    ui.set_notification_test_status(
        if settings.notification_self_test_completed && !settings.notification_tests_started() {
            "旧版整体测试已通过；建议使用上方按钮逐项复测".into()
        } else if settings.notification_self_test_completed {
            "当前启用的提醒通道已确认；系统通知的 Toast 或气泡回退至少一项可用".into()
        } else if settings.notification_tests_started() {
            "仍有启用的通道未测试或未通过，请逐项处理".into()
        } else {
            "尚未完成提醒通道测试".into()
        },
    );
}

fn secondary_provider_index(provider: SecondaryNotificationProvider) -> i32 {
    match provider {
        SecondaryNotificationProvider::WeCom => 1,
        SecondaryNotificationProvider::DingTalk => 2,
        SecondaryNotificationProvider::Feishu => 3,
        SecondaryNotificationProvider::PushPlus => 4,
        _ => 0,
    }
}

fn secondary_provider_from_index(index: i32) -> SecondaryNotificationProvider {
    match index {
        1 => SecondaryNotificationProvider::WeCom,
        2 => SecondaryNotificationProvider::DingTalk,
        3 => SecondaryNotificationProvider::Feishu,
        4 => SecondaryNotificationProvider::PushPlus,
        _ => SecondaryNotificationProvider::Disabled,
    }
}

fn refresh_secondary_notification_ui(
    ui: &MainWindow,
    data_root: &std::path::Path,
    settings: &AppSettings,
    runtime: &RuntimeHandle,
) {
    ui.set_secondary_notification_configured(secondary_notification::configured(
        data_root, settings,
    ));
    let mut status = secondary_notification::configuration_status(data_root, settings);
    if let Ok(summary) = runtime.secondary_notification_summary() {
        if summary.exhausted > 0 {
            status.push_str(&format!(
                " 当前有 {} 条远程提醒已达到 5 次重试上限，请检查凭据或服务状态。",
                summary.exhausted
            ));
        } else if summary.retrying + summary.pending + summary.leased > 0 {
            status.push_str(&format!(
                " 待发送/重试 {} 条；过去 1 小时已使用 {} / 20 个请求批次。",
                summary.retrying + summary.pending + summary.leased,
                summary.requests_last_hour
            ));
        } else if let Some(latest) = summary.latest_success_at {
            status.push_str(&format!(
                " 最近一次成功：{}；过去 1 小时已使用 {} / 20 个请求批次。",
                latest.format("%m-%d %H:%M"),
                summary.requests_last_hour
            ));
        }
        if let Some(error) = summary.latest_error.as_deref() {
            status.push_str(&format!(" 最近错误：{}", operations::redact(error)));
        }
    }
    ui.set_secondary_notification_status(status.into());
}

fn toast_platform_display(diagnostics: &windows_integration::ToastDiagnostics) -> (String, i32) {
    if !diagnostics.supported {
        return ("Windows Toast：当前平台不支持".into(), 2);
    }
    if diagnostics.notifications_enabled {
        let registration = if diagnostics.shortcut_aumid_matches {
            "开始菜单 AUMID 已匹配"
        } else if diagnostics.common_start_menu_shortcut_present {
            "开始菜单 AUMID 不匹配，正式通知会回退托盘气泡"
        } else {
            "未发现安装快捷方式；便携运行会在失败时回退托盘气泡"
        };
        let presentation = diagnostics
            .user_notification_state
            .as_deref()
            .map(toast_presentation_text)
            .unwrap_or("呈现状态未知");
        return (
            format!("Windows Toast：权限已启用；{registration}；{presentation}"),
            if diagnostics.shortcut_aumid_matches {
                1
            } else {
                0
            },
        );
    }
    if let Some(setting) = diagnostics.notification_setting.as_deref() {
        return (
            format!(
                "Windows Toast：{}；正式提醒将回退托盘气泡",
                toast_setting_text(setting)
            ),
            2,
        );
    }
    if let Some(error) = diagnostics.error.as_deref() {
        return (
            format!("Windows Toast：不可用（{error}）；正式提醒将回退托盘气泡"),
            2,
        );
    }
    (
        "Windows Toast：状态未知；正式提醒会保留托盘气泡回退".into(),
        0,
    )
}

fn toast_setting_text(setting: &str) -> &'static str {
    match setting {
        "enabled" => "权限已启用",
        "disabledForApplication" => "已在系统设置中针对本应用关闭",
        "disabledForUser" => "当前用户已关闭系统通知",
        "disabledByGroupPolicy" => "被组策略关闭",
        "disabledByManifestOrRegistration" => "安装注册或应用标识不完整",
        _ => "权限状态未知",
    }
}

fn toast_presentation_text(state: &str) -> &'static str {
    match state {
        "acceptsNotifications" => "系统当前允许呈现通知",
        "quietTime" => "系统处于初始静默期",
        "busy" => "系统当前忙碌，通知可能延后",
        "fullScreen" => "当前全屏，通知可能进入通知中心",
        "presentationMode" => "当前演示模式，通知可能被抑制",
        "notPresent" => "当前用户不在场，通知可能延后",
        "app" => "Windows 应用正在运行，通知可能延后",
        _ => "呈现状态未知",
    }
}

fn notification_test_display(value: Option<bool>, label: &str) -> (String, i32) {
    match value {
        Some(true) => (format!("{label}：通过"), 1),
        Some(false) => (format!("{label}：未通过"), 2),
        None => (format!("{label}：未测试"), 0),
    }
}

fn refresh_ui(ui: &MainWindow, runtime: &RuntimeHandle) {
    let snapshot = runtime.snapshot();
    ui.set_is_synchronizing(snapshot.is_synchronizing);
    ui.set_status_text(snapshot.status_text.clone().into());
    ui.set_sync_text(if snapshot.last_sync_text == "尚未同步" {
        "尚未完成同步".into()
    } else {
        format!("最近同步 {}", snapshot.last_sync_text).into()
    });
    ui.set_health_text(snapshot.health_text.clone().into());
    ui.set_clock_text(snapshot.clock_text.clone().into());
    let level = if snapshot.last_sync_succeeded == Some(false)
        || snapshot.health_state == HealthState::Failed
        || snapshot.clock_state == HealthState::Failed
    {
        2
    } else if snapshot.last_sync_succeeded == Some(true)
        && snapshot.health_state == HealthState::Healthy
        && snapshot.clock_state != HealthState::Failed
    {
        1
    } else {
        0
    };
    ui.set_runtime_level(level);

    let settings = runtime.settings().unwrap_or_default();
    let today = runtime.today_events().unwrap_or_default();
    let future = runtime.future_events().unwrap_or_default();
    let filter_text = ui.get_task_filter_text().to_string();
    let market_filter = ui.get_task_market_filter_index();
    let status_filter = ui.get_task_status_filter_index();
    let visible_today = today
        .iter()
        .filter(|event| task_matches_filter(event, &filter_text, market_filter, status_filter))
        .collect::<Vec<_>>();
    let visible_future = future
        .iter()
        .filter(|event| task_matches_filter(event, &filter_text, market_filter, status_filter))
        .collect::<Vec<_>>();
    ui.set_today_count(today.len() as i32);
    ui.set_today_visible_count(visible_today.len() as i32);
    ui.set_future_count(future.len() as i32);
    ui.set_future_visible_count(visible_future.len() as i32);
    ui.set_pending_count(today.iter().filter(|event| is_pending(event)).count() as i32);
    ui.set_acknowledged_count(
        today
            .iter()
            .filter(|event| event.lifecycle_status == LifecycleStatus::Acknowledged)
            .count() as i32,
    );
    ui.set_issue_count(
        today
            .iter()
            .filter(|event| event_needs_review(event))
            .count() as i32,
    );
    ui.set_today_tasks(ModelRc::from(Rc::new(VecModel::from(
        visible_today
            .into_iter()
            .map(|event| task_row(event, &settings))
            .collect::<Vec<_>>(),
    ))));
    ui.set_future_tasks(ModelRc::from(Rc::new(VecModel::from(
        visible_future
            .into_iter()
            .map(|event| task_row(event, &settings))
            .collect::<Vec<_>>(),
    ))));

    if let Ok(health) = runtime.health_details() {
        ui.set_health_title(match health.overall_state {
            HealthState::Healthy => "程序与数据源运行正常".into(),
            HealthState::Warning => "存在待核验任务或异常数据源".into(),
            HealthState::Failed => "提醒系统需要立即检查".into(),
            _ => "健康状态尚未建立".into(),
        });
        ui.set_health_summary(
            format!(
                "今日任务 {} 只，待确认 {} 只，来源冲突 {} 只，待人工核验 {} 只，本地投递重试 {} 条。{}",
                health.today_task_count,
                health.pending_confirmation_count,
                health.conflict_count,
                health.manual_review_count,
                health.delivery_retry_count,
                health
                    .latest_delivery_error
                    .as_deref()
                    .map(|error| format!(" 最近错误：{}", operations::redact(error)))
                    .unwrap_or_default(),
            )
            .into(),
        );
        ui.set_heartbeat_text(
            format!(
                "调度心跳 {} · 投递心跳 {}",
                format_timestamp(health.scheduler_heartbeat),
                format_timestamp(health.delivery_heartbeat),
            )
            .into(),
        );
        let sources = health
            .sources
            .into_iter()
            .map(|source| SourceHealthRow {
                source: source.source.into(),
                state: health_state_text(source.state).into(),
                record_text: format!("记录 {}", source.last_record_count).into(),
                last_success_text: format!(
                    "最近成功 {} · 连续失败 {}",
                    format_timestamp(source.last_success_at),
                    source.consecutive_failures
                )
                .into(),
                error_text: source.last_error.unwrap_or_default().into(),
                state_level: health_state_level(source.state),
            })
            .collect::<Vec<_>>();
        ui.set_source_health(ModelRc::from(Rc::new(VecModel::from(sources))));
    }
}

fn task_matches_filter(
    event: &IpoEvent,
    text: &str,
    market_filter: i32,
    status_filter: i32,
) -> bool {
    let market_matches = match market_filter {
        1 => event.exchange == Exchange::Shanghai,
        2 => event.exchange == Exchange::Shenzhen,
        3 => event.exchange == Exchange::Beijing,
        _ => true,
    };
    let status_matches = match status_filter {
        1 => is_pending(event),
        2 => event.lifecycle_status == LifecycleStatus::Acknowledged,
        3 => event_needs_review(event),
        _ => true,
    };
    if !market_matches || !status_matches {
        return false;
    }
    let needle = text.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    [
        event.name.as_str(),
        event.security_code.as_str(),
        event.apply_code.as_deref().unwrap_or_default(),
        event.legacy_code.as_deref().unwrap_or_default(),
        lifecycle_text(event.lifecycle_status),
        quality_text(event.data_quality_status),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(&needle))
}

fn task_row(event: &IpoEvent, settings: &AppSettings) -> TaskRow {
    let cutoff = crate::core::effective_cutoff(event, settings);
    let price = event
        .issue_price
        .map(|value| format!("{value:.2} 元"))
        .unwrap_or_else(|| "价格待公布".into());
    let max = event
        .max_apply_quantity
        .map(|value| format!("上限 {value} 股"))
        .unwrap_or_else(|| "上限待公布".into());
    let lot = event
        .lot_size
        .map(|value| format!("单位 {value} 股"))
        .unwrap_or_else(|| "单位待公布".into());
    let session = if event.sessions.is_empty() {
        default_session_text(event.exchange).to_owned()
    } else {
        event
            .sessions
            .iter()
            .map(|session| {
                format!(
                    "{}–{}",
                    session.official_start.format("%H:%M"),
                    session.official_end.format("%H:%M")
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    };
    let mut warnings = Vec::new();
    if event.exchange == Exchange::Beijing {
        warnings.push(
            "北交所通常需全额缴付申购资金；不足 100 股余股顺序可能受提交时间影响。".to_owned(),
        );
    }
    if matches!(
        event.data_quality_status,
        DataQualityStatus::DataConflict
            | DataQualityStatus::Stale
            | DataQualityStatus::ManualReviewRequired
    ) {
        warnings.push(format!(
            "数据状态：{}，请核对正式公告。",
            quality_text(event.data_quality_status)
        ));
    }
    if event.lifecycle_status == LifecycleStatus::AcknowledgedNeedsReview {
        warnings.push("关键申购信息已变化，旧确认已失效，请核对后重新确认。".to_owned());
    }
    TaskRow {
        event_id: event.id.clone().into(),
        event_version: event.event_version,
        name: event.name.clone().into(),
        market_and_codes: format!(
            "{} · 股票 {} · 申购 {}",
            market_name(event.exchange, event.board),
            event.security_code,
            event.apply_code.as_deref().unwrap_or("待核验")
        )
        .into(),
        date_and_cutoff: format!(
            "{} / {}",
            event
                .apply_date
                .map(|date| date.to_string())
                .unwrap_or_else(|| "待公告确认".into()),
            cutoff.format("%H:%M")
        )
        .into(),
        numbers: format!("{price} · {max} · {lot}").into(),
        session: session.into(),
        status: lifecycle_text(event.lifecycle_status).into(),
        quality: quality_text(event.data_quality_status).into(),
        updated: format!("最后更新 {}", event.updated_at.format("%m-%d %H:%M")).into(),
        warning: warnings.join("\n").into(),
        needs_review: event_needs_review(event),
        can_acknowledge: is_pending(event),
        can_revoke: event.lifecycle_status == LifecycleStatus::Acknowledged,
        confirm_label: if event.lifecycle_status == LifecycleStatus::AcknowledgedNeedsReview {
            "重新确认"
        } else {
            "确认已申购"
        }
        .into(),
    }
}

fn show_event_details(ui: &MainWindow, runtime: &RuntimeHandle, event_id: &str) {
    match runtime.event(event_id) {
        Ok(Some(event)) => {
            let field_sources = runtime.field_sources(event_id).unwrap_or_default();
            let announcements = runtime.announcements(event_id).unwrap_or_default();
            let overrides = runtime
                .manual_overrides(event_id, event.event_version)
                .unwrap_or_default();
            let settings = runtime.settings().unwrap_or_default();
            ui.set_selected_event_id(event.id.clone().into());
            ui.set_selected_event_version(event.event_version);
            ui.set_selected_title(format!("{} · {}", event.name, event.display_code()).into());
            ui.set_selected_summary(
                format!(
                    "{} · 股票代码 {} · 申购日 {} · 数据版本 {}",
                    market_name(event.exchange, event.board),
                    event.security_code,
                    event
                        .apply_date
                        .map(|date| date.to_string())
                        .unwrap_or_else(|| "待核验".into()),
                    event.event_version,
                )
                .into(),
            );
            ui.set_selected_quality(quality_text(event.data_quality_status).into());
            ui.set_selected_quality_alert(matches!(
                event.data_quality_status,
                DataQualityStatus::DataConflict
                    | DataQualityStatus::ManualReviewRequired
                    | DataQualityStatus::Stale
            ));
            ui.set_selected_warning(if !event.manual_override_fields.is_empty() {
                format!(
                    "当前有效人工覆盖：{}。所有原始来源仍保留在“字段来源”中。",
                    event
                        .manual_override_fields
                        .iter()
                        .map(|field| field_display_name(field))
                        .collect::<Vec<_>>()
                        .join("、"),
                )
                .into()
            } else if event.data_conflict {
                "关键字段存在来源冲突，请以最新正式发行公告为准。".into()
            } else {
                "".into()
            });
            let announcement_titles = announcements
                .iter()
                .map(|document| document.reference.title.clone())
                .collect::<Vec<_>>();
            ui.set_selected_detail(
                format_event_details(&event, &announcement_titles, &settings).into(),
            );
            ui.set_details_active_tab(0);
            ui.set_override_field_index(0);
            ui.set_override_announcement_index(0);
            ui.set_override_value("".into());
            ui.set_override_reason("".into());
            ui.set_override_status("".into());

            let field_rows = field_sources
                .into_iter()
                .map(|source| FieldSourceRow {
                    field_name: field_display_name(&source.field_name).into(),
                    normalized_value: source.normalized_value.unwrap_or_else(|| "—".into()).into(),
                    raw_value: source.raw_value.unwrap_or_else(|| "—".into()).into(),
                    source_text: format!("{} · 优先级 {}", source.source, source.priority).into(),
                    fetched_text: format!("抓取 {}", source.fetched_at.format("%Y-%m-%d %H:%M"))
                        .into(),
                })
                .collect::<Vec<_>>();
            ui.set_field_source_rows(ModelRc::from(Rc::new(VecModel::from(field_rows))));

            let announcement_rows = announcements
                .iter()
                .map(|document| {
                    let evidence = if document.fields.is_empty() {
                        "未提取到高置信度字段，请人工查看原文。".into()
                    } else {
                        document
                            .fields
                            .iter()
                            .take(6)
                            .map(|field| {
                                format!(
                                    "{}={}（{:.0}%）",
                                    field_display_name(&field.name),
                                    field.value,
                                    field.confidence * 100.0,
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("；")
                    };
                    let hash_preview = document.file_hash.chars().take(12).collect::<String>();
                    AnnouncementRow {
                        id: document.id.clone().into(),
                        title: document.reference.title.clone().into(),
                        metadata: format!(
                            "{} · {} · {} · SHA-256 {}…",
                            document.reference.provider,
                            document
                                .reference
                                .published_at
                                .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_else(|| "发布时间未知".into()),
                            extraction_text(document.status),
                            hash_preview,
                        )
                        .into(),
                        evidence: evidence.into(),
                        source_url: document.reference.url.clone().into(),
                        local_path: document.local_path.clone().into(),
                        local_available: std::path::Path::new(&document.local_path).is_file(),
                    }
                })
                .collect::<Vec<_>>();
            ui.set_announcement_rows(ModelRc::from(Rc::new(VecModel::from(announcement_rows))));
            let mut announcement_choices = vec![slint::SharedString::from("不指定公告")];
            announcement_choices.extend(
                announcements
                    .iter()
                    .map(|document| slint::SharedString::from(document.reference.title.as_str())),
            );
            ui.set_announcement_choices(ModelRc::from(Rc::new(VecModel::from(
                announcement_choices,
            ))));

            let override_rows = overrides
                .into_iter()
                .map(|entry| {
                    let announcement_title =
                        entry.announcement_document_id.as_deref().and_then(|id| {
                            announcements
                                .iter()
                                .find(|document| document.id == id)
                                .map(|document| document.reference.title.as_str())
                        });
                    OverrideRow {
                        id: entry.id.to_string().into(),
                        summary: format!(
                            "{} = {}",
                            field_display_name(&entry.field_name),
                            entry.override_value
                        )
                        .into(),
                        metadata: format!(
                            "理由：{} · {}{}{}",
                            entry.reason,
                            entry.created_at.format("%Y-%m-%d %H:%M"),
                            announcement_title
                                .map(|title| format!(" · 依据公告：{title}"))
                                .unwrap_or_else(|| " · 未关联依据公告".into()),
                            entry
                                .revoked_at
                                .map(|value| format!(
                                    " · 已于 {} 撤销",
                                    value.format("%Y-%m-%d %H:%M")
                                ))
                                .unwrap_or_else(|| " · 当前有效".into()),
                        )
                        .into(),
                        can_revoke: entry.revoked_at.is_none(),
                    }
                })
                .collect::<Vec<_>>();
            ui.set_override_rows(ModelRc::from(Rc::new(VecModel::from(override_rows))));
            ui.set_show_details(true);
        }
        Ok(None) => ui.set_status_text("任务已不存在，请刷新列表".into()),
        Err(error) => ui.set_status_text(format!("读取详情失败：{error:#}").into()),
    }
}

fn override_field_name(index: i32) -> &'static str {
    match index {
        0 => "ApplyCode",
        1 => "ApplyDate",
        2 => "IssuePrice",
        3 => "MaxApplyQuantity",
        4 => "LotSize",
        5 => "OfficialSessions",
        6 => "IssueStatus",
        _ => "",
    }
}

fn field_display_name(name: &str) -> &str {
    match name {
        "SecurityCode" => "股票代码",
        "ApplyCode" => "申购代码",
        "LegacyCode" => "历史代码",
        "Name" => "股票简称",
        "ApplyDate" => "申购日期",
        "IssuePrice" => "发行价格",
        "LotSize" => "申购单位",
        "MaxApplyQuantity" => "申购上限",
        "RequiredMarketValue" => "所需市值",
        "RequiredCash" => "所需现金",
        "BallotDate" => "中签日期",
        "PaymentDate" => "缴款日期",
        "ListingDate" => "上市日期",
        "IssueStatus" | "Status" => "发行状态",
        "OfficialSessions" | "Sessions" => "官方申购时段",
        _ => name,
    }
}

fn extraction_text(status: model::ExtractionStatus) -> &'static str {
    match status {
        model::ExtractionStatus::Extracted => "文本已解析",
        model::ExtractionStatus::LowConfidence => "低置信度",
        model::ExtractionStatus::Failed => "解析失败",
        model::ExtractionStatus::Unsupported => "不支持自动解析",
        _ => "待解析",
    }
}

fn format_event_details(
    event: &IpoEvent,
    announcements: &[String],
    settings: &AppSettings,
) -> String {
    let sessions = if event.sessions.is_empty() {
        default_session_text(event.exchange).to_owned()
    } else {
        event
            .sessions
            .iter()
            .map(|session| {
                format!(
                    "{}–{}",
                    session.official_start.format("%H:%M"),
                    session.official_end.format("%H:%M")
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    };
    let announcement_text = if announcements.is_empty() {
        "暂无已保存正式公告".into()
    } else {
        announcements
            .iter()
            .take(8)
            .map(|title| format!("• {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "市场：{}\n证券代码：{}\n申购代码：{}\n申购日期：{}\n发行价格：{}\n申购单位：{} 股\n申购上限：{} 股\n所需市值：{}\n所需现金：{}\n中签日期：{}\n缴款日期：{}\n上市日期：{}\n官方申购时段：{}\n安全截止：{}\n任务状态：{}\n数据质量：{}\n事件版本：{}\n最后更新：{}\n\n正式公告\n{}",
        market_name(event.exchange, event.board),
        event.security_code,
        event.apply_code.as_deref().unwrap_or("待核验"),
        event
            .apply_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .issue_price
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "待核验".into()),
        event
            .lot_size
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .max_apply_quantity
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .required_market_value
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "不适用或待核验".into()),
        event
            .required_cash
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "不适用或待核验".into()),
        event
            .ballot_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .payment_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .listing_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        sessions,
        crate::core::effective_cutoff(event, settings).format("%H:%M"),
        lifecycle_text(event.lifecycle_status),
        quality_text(event.data_quality_status),
        event.event_version,
        event.updated_at.format("%Y-%m-%d %H:%M:%S"),
        announcement_text,
    )
}

fn diagnostic_summary(runtime: &RuntimeHandle, data_root: &PathBuf) -> String {
    let snapshot = runtime.snapshot();
    let toast_diagnostics = windows_integration::toast_diagnostics();
    let (toast_status, _) = toast_platform_display(&toast_diagnostics);
    let mut lines = vec![
        "A 股新股申购提醒 - 诊断摘要".to_owned(),
        format!(
            "生成时间：{}",
            crate::core::now_china().format("%Y-%m-%d %H:%M:%S %:z")
        ),
        format!("运行状态：{}", snapshot.status_text),
        format!("最近同步：{}", snapshot.last_sync_text),
        format!("同步成功：{:?}", snapshot.last_sync_succeeded),
        format!("系统时间：{}", snapshot.clock_text),
        toast_status,
        format!(
            "Toast AUMID：{}，进程标识：{}，开始菜单匹配：{}",
            toast_diagnostics.app_user_model_id,
            toast_diagnostics.process_identity_set,
            toast_diagnostics.shortcut_aumid_matches
        ),
        format!("数据目录：{}", data_root.display()),
    ];
    if let Ok(health) = runtime.health_details() {
        lines.push(format!("总体健康：{:?}", health.overall_state));
        lines.push(format!(
            "今日任务：{}，待确认：{}",
            health.today_task_count, health.pending_confirmation_count
        ));
        for source in health.sources {
            lines.push(format!(
                "{}：{}，最近成功 {}，连续失败 {}",
                source.source,
                health_state_text(source.state),
                format_timestamp(source.last_success_at),
                source.consecutive_failures
            ));
        }
    }
    lines.join("\r\n")
}

fn is_pending(event: &IpoEvent) -> bool {
    can_acknowledge_on(
        event.apply_date,
        event.lifecycle_status,
        crate::core::now_china().date_naive(),
    )
}

fn can_acknowledge_on(
    apply_date: Option<chrono::NaiveDate>,
    lifecycle_status: LifecycleStatus,
    today: chrono::NaiveDate,
) -> bool {
    apply_date == Some(today)
        && matches!(
            lifecycle_status,
            LifecycleStatus::Scheduled
                | LifecycleStatus::ActiveUnconfirmed
                | LifecycleStatus::AcknowledgedNeedsReview
        )
}

fn event_needs_review(event: &IpoEvent) -> bool {
    event.data_conflict
        || matches!(
            event.data_quality_status,
            DataQualityStatus::DataConflict
                | DataQualityStatus::Stale
                | DataQualityStatus::ManualReviewRequired
        )
        || matches!(
            event.lifecycle_status,
            LifecycleStatus::AcknowledgedNeedsReview | LifecycleStatus::ExpiredUnconfirmed
        )
}

fn market_name(exchange: Exchange, board: Board) -> &'static str {
    match (exchange, board) {
        (Exchange::Shanghai, Board::Star) => "沪市·科创板",
        (Exchange::Shanghai, _) => "沪市·主板",
        (Exchange::Shenzhen, Board::ChiNext) => "深市·创业板",
        (Exchange::Shenzhen, _) => "深市·主板",
        (Exchange::Beijing, _) => "北交所",
        _ => "未知市场",
    }
}

fn default_session_text(exchange: Exchange) -> &'static str {
    if exchange == Exchange::Shanghai {
        "09:30–11:30；13:00–15:00（默认，公告优先）"
    } else {
        "09:15–11:30；13:00–15:00（默认，公告优先）"
    }
}

fn quality_text(status: DataQualityStatus) -> &'static str {
    match status {
        DataQualityStatus::AnnouncementVerified => "正式公告已核验",
        DataQualityStatus::MultiSourceVerified => "多源一致",
        DataQualityStatus::SingleSource => "单一来源待核验",
        DataQualityStatus::DataConflict => "来源冲突",
        DataQualityStatus::Stale => "数据陈旧",
        DataQualityStatus::ManualReviewRequired => "待人工核验",
        _ => "状态未知",
    }
}

fn health_state_text(state: HealthState) -> &'static str {
    match state {
        HealthState::Healthy => "正常",
        HealthState::Warning => "陈旧/警告",
        HealthState::Failed => "失败",
        _ => "未知",
    }
}

fn health_state_level(state: HealthState) -> i32 {
    match state {
        HealthState::Healthy => 1,
        HealthState::Warning => 2,
        HealthState::Failed => 3,
        _ => 0,
    }
}

fn format_timestamp(value: Option<model::ChinaDateTime>) -> String {
    value
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "无".into())
}

fn reminder_body(event: &IpoEvent, level: ReminderLevel, message: Option<&str>) -> String {
    if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
        return format!(
            "{message}\n申购代码：{}\n请打开任务详情核对最新信息。",
            event.display_code()
        );
    }
    if level == ReminderLevel::BallotCheck {
        return format!(
            "中签结果查询日期：{}\n股票代码：{}\n请登录券商客户端或核对正式公告查询中签结果。本程序不会读取券商账户，也不会自动判断是否中签。",
            event
                .ballot_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    if matches!(
        level,
        ReminderLevel::PaymentMorning | ReminderLevel::PaymentFollowUp
    ) {
        let phase = if level == ReminderLevel::PaymentMorning {
            "今天是公开数据标记的缴款日，请尽早检查"
        } else {
            "缴款日已到下午，请再次确认"
        };
        return format!(
            "{phase}\n缴款日期：{}\n股票代码：{}\n请登录券商客户端核对是否中签，并按券商规则确保资金账户足额。具体到账要求以正式公告和券商为准；本程序不会读取账户或确认缴款结果。",
            event
                .payment_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    if level == ReminderLevel::ListingMorning {
        return format!(
            "公开数据标记今天为上市日：{}\n股票代码：{}\n请在开盘前核对交易所公告和行情软件。本提醒不读取持仓、不跟踪收益，也不代表证券已经可以正常交易。",
            event
                .listing_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    let level_text = reminder_level_text(level);
    format!(
        "{level_text}\n申购代码：{}\n发行价：{}\n请在券商客户端完成后点击“确认已申购”。",
        event.display_code(),
        event
            .issue_price
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "待核验".into())
    )
}

fn reminder_title_text(level: ReminderLevel) -> &'static str {
    match level {
        ReminderLevel::BallotCheck => "中签查询提醒",
        ReminderLevel::PaymentMorning | ReminderLevel::PaymentFollowUp => "缴款资金提醒",
        ReminderLevel::ListingMorning => "上市日提醒",
        _ => "打新提醒",
    }
}

fn reminder_level_text(level: ReminderLevel) -> &'static str {
    match level {
        ReminderLevel::Advance => "明日申购预告",
        ReminderLevel::Morning => "今日申购提醒",
        ReminderLevel::BrokerOpening | ReminderLevel::MarketOpening => "申购通道即将开放",
        ReminderLevel::FifteenMinutes
        | ReminderLevel::FiveMinutes
        | ReminderLevel::TwoMinutes
        | ReminderLevel::Final => "接近安全截止时间",
        ReminderLevel::DataChanged => "申购任务信息有变化",
        ReminderLevel::BallotCheck => "请查询中签结果",
        ReminderLevel::PaymentMorning => "缴款日，请尽早核对中签与资金",
        ReminderLevel::PaymentFollowUp => "缴款日下午，请再次确认资金状态",
        ReminderLevel::ListingMorning => "公开数据标记今天为上市日，请核对正式公告",
        _ => "申购任务尚未确认",
    }
}

fn lifecycle_text(status: LifecycleStatus) -> &'static str {
    match status {
        LifecycleStatus::Scheduled => "待申购日",
        LifecycleStatus::ActiveUnconfirmed => "今日待确认",
        LifecycleStatus::Acknowledged => "已确认",
        LifecycleStatus::AcknowledgedNeedsReview => "已确认但需复核",
        LifecycleStatus::SuspendedOrCancelled => "暂停或终止",
        LifecycleStatus::ExpiredUnconfirmed => "已过截止时间",
        _ => "已发现",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_event(exchange: Exchange, status: LifecycleStatus) -> IpoEvent {
        let now = crate::core::now_china();
        IpoEvent {
            id: "fixture-filter".into(),
            exchange,
            board: Board::Main,
            security_code: "601001".into(),
            apply_code: Some("780001".into()),
            legacy_code: Some("730001".into()),
            name: "筛选测试股份".into(),
            apply_date: Some(now.date_naive()),
            issue_price: None,
            lot_size: None,
            max_apply_quantity: None,
            required_market_value: None,
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: model::IssueStatus::Active,
            lifecycle_status: status,
            event_version: 1,
            announcement_url: None,
            data_quality_status: DataQualityStatus::MultiSourceVerified,
            data_conflict: false,
            manual_override_fields: Vec::new(),
            sessions: Vec::new(),
            first_seen_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn acknowledgement_is_available_only_on_the_apply_date() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();

        assert!(can_acknowledge_on(
            Some(today),
            LifecycleStatus::Scheduled,
            today,
        ));
        assert!(!can_acknowledge_on(
            Some(today + chrono::Duration::days(1)),
            LifecycleStatus::Scheduled,
            today,
        ));
        assert!(!can_acknowledge_on(
            Some(today),
            LifecycleStatus::ExpiredUnconfirmed,
            today,
        ));
    }

    #[test]
    fn sync_interval_parser_supports_minutes_and_hours() {
        assert_eq!(
            parse_sync_interval("30", 0, "普通日期自动同步间隔").unwrap(),
            30
        );
        assert_eq!(
            parse_sync_interval("2", 1, "普通日期自动同步间隔").unwrap(),
            120
        );
        assert!(parse_sync_interval("4", 0, "申购日自动同步间隔").is_err());
        assert_eq!(sync_interval_display(120), ("2".into(), 1));
        assert_eq!(sync_interval_display(10), ("10".into(), 0));
    }

    #[test]
    fn notification_self_test_requires_each_enabled_channel() {
        let mut settings = AppSettings::default();
        settings.notification_window_test_passed = Some(true);
        settings.notification_balloon_test_passed = Some(true);
        settings.notification_sound_test_passed = Some(true);
        assert!(!settings.notification_tests_complete());

        settings.notification_flash_test_passed = Some(true);
        assert!(settings.notification_tests_complete());

        settings.notification_balloon_test_passed = Some(false);
        settings.notification_toast_test_passed = Some(true);
        assert!(settings.notification_tests_complete());

        settings.notification_toast_test_passed = Some(false);
        settings.toast_enabled = false;
        assert!(settings.notification_tests_complete());
    }

    #[test]
    fn task_filter_combines_text_market_and_status() {
        let event = filter_event(Exchange::Shanghai, LifecycleStatus::ActiveUnconfirmed);
        assert!(task_matches_filter(&event, "测试", 0, 0));
        assert!(task_matches_filter(&event, "780001", 1, 1));
        assert!(task_matches_filter(&event, "730001", 0, 0));
        assert!(!task_matches_filter(&event, "不存在", 0, 0));
        assert!(!task_matches_filter(&event, "", 2, 0));
        assert!(!task_matches_filter(&event, "", 0, 2));

        let mut review = event;
        review.data_quality_status = DataQualityStatus::ManualReviewRequired;
        assert!(task_matches_filter(&review, "人工核验", 1, 3));
    }

    #[test]
    fn task_filter_handles_thousands_of_fixed_rows_without_losing_matches() {
        let events = (0..2_000)
            .map(|index| {
                let mut event = filter_event(
                    if index % 3 == 0 {
                        Exchange::Shanghai
                    } else if index % 3 == 1 {
                        Exchange::Shenzhen
                    } else {
                        Exchange::Beijing
                    },
                    LifecycleStatus::ActiveUnconfirmed,
                );
                event.id = format!("fixture-filter-{index}");
                event.security_code = format!("{:06}", 600_000 + index);
                event.apply_code = Some(format!("{:06}", 700_000 + index));
                event.name = format!("固定压力样本{index}");
                event
            })
            .collect::<Vec<_>>();

        let exact = events
            .iter()
            .filter(|event| task_matches_filter(event, "固定压力样本1999", 0, 0))
            .count();
        assert_eq!(exact, 1);
        let shanghai = events
            .iter()
            .filter(|event| task_matches_filter(event, "固定压力样本", 1, 1))
            .count();
        assert_eq!(shanghai, 667);
    }
}

struct RuntimeOptions {
    data_root: PathBuf,
    background: bool,
    exit_after: Option<Duration>,
    skip_startup_sync: bool,
    skip_auto_start_registration: bool,
    skip_update_check: bool,
    skip_crash_upload: bool,
    reminder_window_smoke_report: Option<PathBuf>,
    windows_recovery_smoke_report: Option<PathBuf>,
}

impl RuntimeOptions {
    fn parse(arguments: &[String]) -> Self {
        let mut data_root = env::var_os("STOCK_IPO_REMINDER_DATA_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(env::temp_dir)
                    .join("StockIpoReminder")
            });
        let mut background = false;
        let mut exit_after = None;
        let mut skip_startup_sync = false;
        let mut skip_auto_start_registration = false;
        let mut skip_update_check = false;
        let mut skip_crash_upload = false;
        let mut reminder_window_smoke_report = None;
        let mut windows_recovery_smoke_report = None;
        let mut index = 0;
        while index < arguments.len() {
            match arguments[index].as_str() {
                "--data-root" if index + 1 < arguments.len() => {
                    index += 1;
                    data_root = PathBuf::from(&arguments[index]);
                }
                "--background" => background = true,
                "--exit-after-seconds" if index + 1 < arguments.len() => {
                    index += 1;
                    exit_after = arguments[index]
                        .parse::<u64>()
                        .ok()
                        .map(Duration::from_secs);
                }
                "--skip-startup-sync" => skip_startup_sync = true,
                "--skip-auto-start-registration" => skip_auto_start_registration = true,
                "--skip-update-check" => skip_update_check = true,
                "--skip-crash-upload" => skip_crash_upload = true,
                "--reminder-window-smoke-report" if index + 1 < arguments.len() => {
                    index += 1;
                    reminder_window_smoke_report = Some(PathBuf::from(&arguments[index]));
                }
                "--windows-recovery-smoke-report" if index + 1 < arguments.len() => {
                    index += 1;
                    windows_recovery_smoke_report = Some(PathBuf::from(&arguments[index]));
                }
                _ => {}
            }
            index += 1;
        }
        Self {
            data_root,
            background,
            exit_after,
            skip_startup_sync,
            skip_auto_start_registration,
            skip_update_check,
            skip_crash_upload,
            reminder_window_smoke_report,
            windows_recovery_smoke_report,
        }
    }
}

#[cfg(windows)]
mod native_tray {
    use std::{
        mem::size_of,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
            mpsc,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail};
    use slint::{ComponentHandle, Timer, Weak};
    use windows::{
        Win32::{
            Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
            System::{
                EventNotificationService::IsNetworkAlive,
                LibraryLoader::GetModuleHandleW,
                RemoteDesktop::{
                    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                    WTSUnRegisterSessionNotification,
                },
            },
            UI::{
                Shell::{
                    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_RESPECT_QUIET_TIME,
                    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_BALLOONUSERCLICK,
                    NOTIFYICONDATAW, Shell_NotifyIconW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, MF_SEPARATOR,
                    MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL,
                    PBT_APMRESUMESTANDBY, PBT_APMRESUMESUSPEND, PostMessageW, PostQuitMessage,
                    RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
                    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
                    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
                    WM_DESTROY, WM_LBUTTONDBLCLK, WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMECHANGE,
                    WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_UNLOCK,
                },
            },
        },
        core::w,
    };

    use crate::{MainWindow, runtime::RuntimeHandle, windows_integration};

    const TRAY_MESSAGE: u32 = WM_APP + 41;
    const SHOW_COMMAND: usize = 1001;
    const SYNC_COMMAND: usize = 1002;
    const SETTINGS_COMMAND: usize = 1003;
    const EXIT_COMMAND: usize = 1004;
    const TODAY_COMMAND: usize = 1005;
    const FUTURE_COMMAND: usize = 1006;
    const LOGS_COMMAND: usize = 1007;
    const ICON_ID: u32 = 1;
    const NETWORK_TIMER_ID: usize = 2001;
    const NETWORK_POLL_MILLISECONDS: u32 = 10_000;

    struct Callbacks {
        show: Box<dyn Fn() + Send + Sync>,
        activate: Box<dyn Fn() + Send + Sync>,
        today: Box<dyn Fn() + Send + Sync>,
        future: Box<dyn Fn() + Send + Sync>,
        logs: Box<dyn Fn() + Send + Sync>,
        notification: Box<dyn Fn(Option<String>) + Send + Sync>,
        sync: Box<dyn Fn() + Send + Sync>,
        settings: Box<dyn Fn() + Send + Sync>,
        exit: Box<dyn Fn() + Send + Sync>,
        recovery: Box<dyn Fn() + Send + Sync>,
    }

    static CALLBACKS: OnceLock<Callbacks> = OnceLock::new();
    static LAST_NOTIFICATION_EVENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    static LAST_RECOVERY: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    static NETWORK_AVAILABLE: AtomicBool = AtomicBool::new(false);
    static TOAST_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
    static TASKBAR_CREATED_MESSAGE: AtomicU32 = AtomicU32::new(0);
    static ACTIVATE_INSTANCE_MESSAGE: AtomicU32 = AtomicU32::new(0);
    static TASKBAR_READD_SUCCEEDED: AtomicU32 = AtomicU32::new(0);
    static TASKBAR_READD_FAILED: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_POWER_MESSAGES: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_UNLOCK_MESSAGES: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_TIME_MESSAGES: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_ACCEPTED: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_SUPPRESSED: AtomicU32 = AtomicU32::new(0);
    static RECOVERY_CALLBACKS: AtomicU32 = AtomicU32::new(0);

    pub struct NativeTray {
        hwnd: Arc<AtomicIsize>,
        thread: Option<JoinHandle<()>>,
    }

    impl NativeTray {
        pub fn start(
            window: Weak<MainWindow>,
            runtime: RuntimeHandle,
            data_root: std::path::PathBuf,
            recovery_smoke_mode: bool,
        ) -> Result<Self> {
            let activation_message_name = windows_integration::activation_message_name(&data_root);
            let show_window = window.clone();
            let activation_window = window.clone();
            let today_window = window.clone();
            let future_window = window.clone();
            let notification_window = window.clone();
            let settings_window = window.clone();
            let exit_window = window;
            let sync_runtime = runtime.clone();
            let notification_runtime = runtime.clone();
            let recovery_runtime = runtime;
            let _ = CALLBACKS.set(Callbacks {
                show: Box::new(move || {
                    let weak = show_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            super::show_and_repaint(&window);
                        }
                    });
                }),
                activate: Box::new(move || {
                    let weak = activation_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            super::show_and_repaint(&window);
                            let verify = window.as_weak();
                            Timer::single_shot(Duration::from_millis(150), move || {
                                let Some(window) = verify.upgrade() else {
                                    crate::operations::log(
                                        "ERROR",
                                        "第二次启动唤醒后主窗口对象已销毁",
                                    );
                                    return;
                                };
                                match windows_integration::confirm_window_visible(window.window()) {
                                    Ok(()) => crate::operations::log(
                                        "INFO",
                                        "第二次启动唤醒：主窗口已可见 event=second_launch_window_visible",
                                    ),
                                    Err(error) => crate::operations::log(
                                        "ERROR",
                                        &format!("第二次启动唤醒后主窗口可见性确认失败：{error:#}"),
                                    ),
                                }
                            });
                        }
                    });
                }),
                today: Box::new(move || {
                    let weak = today_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            window.set_active_page(0);
                            super::show_and_repaint(&window);
                        }
                    });
                }),
                future: Box::new(move || {
                    let weak = future_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            window.set_active_page(1);
                            super::show_and_repaint(&window);
                        }
                    });
                }),
                logs: Box::new(move || {
                    if let Err(error) = windows_integration::open_folder(&data_root.join("logs")) {
                        crate::operations::log(
                            "ERROR",
                            &format!("从托盘打开日志目录失败：{error:#}"),
                        );
                    }
                }),
                notification: Box::new(move |event_id| {
                    let weak = notification_window.clone();
                    let runtime = notification_runtime.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            super::show_and_repaint(&window);
                            if let Some(event_id) = event_id.filter(|value| !value.is_empty()) {
                                super::show_event_details(&window, &runtime, &event_id);
                            }
                        }
                    });
                }),
                sync: Box::new(move || sync_runtime.request_sync("托盘手动同步")),
                settings: Box::new(move || {
                    let weak = settings_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            window.set_active_page(3);
                            super::show_and_repaint(&window);
                        }
                    });
                }),
                exit: Box::new(move || {
                    let weak = exit_window.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(window) = weak.upgrade() {
                            if window.get_pending_count() > 0 {
                                window.set_show_exit_confirmation(true);
                                super::show_and_repaint(&window);
                                return;
                            }
                            let _ = window.hide();
                        }
                        let _ = slint::quit_event_loop();
                    });
                }),
                recovery: Box::new(move || {
                    RECOVERY_CALLBACKS.fetch_add(1, Ordering::AcqRel);
                    recovery_runtime.recovery();
                    if !recovery_smoke_mode {
                        recovery_runtime.request_sync("系统恢复或时间变化");
                    }
                }),
            });
            let hwnd = Arc::new(AtomicIsize::new(0));
            let thread_hwnd = Arc::clone(&hwnd);
            let (sender, receiver) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("stock-ipo-native-tray".into())
                .spawn(move || unsafe {
                    let _ = run_tray(thread_hwnd, sender, activation_message_name);
                })
                .context("无法启动托盘线程")?;
            match receiver.recv().context("托盘线程未返回初始化结果")? {
                Ok(()) => Ok(Self {
                    hwnd,
                    thread: Some(thread),
                }),
                Err(error) => {
                    let _ = thread.join();
                    bail!(error)
                }
            }
        }

        pub fn notify(&self, title: &str, body: &str, event_id: Option<&str>) {
            match windows_integration::show_windows_toast(title, body) {
                Ok(()) => {
                    TOAST_FALLBACK_LOGGED.store(false, Ordering::Release);
                }
                Err(error) => {
                    if !TOAST_FALLBACK_LOGGED.swap(true, Ordering::AcqRel) {
                        crate::operations::log(
                            "WARN",
                            &format!("Windows Toast 不可用，后续系统通知已回退托盘气泡：{error:#}"),
                        );
                    }
                    self.notify_balloon(title, body, event_id);
                }
            }
        }

        pub fn notify_balloon(&self, title: &str, body: &str, event_id: Option<&str>) {
            let raw = self.hwnd.load(Ordering::Acquire);
            if raw == 0 {
                return;
            }
            if let Ok(mut target) = LAST_NOTIFICATION_EVENT
                .get_or_init(|| Mutex::new(None))
                .lock()
            {
                *target = event_id.map(str::to_owned);
            }
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: HWND(raw as *mut _),
                uID: ICON_ID,
                uFlags: NIF_INFO,
                dwInfoFlags: NIIF_WARNING | NIIF_RESPECT_QUIET_TIME,
                ..Default::default()
            };
            copy_wide(title, &mut data.szInfoTitle);
            copy_wide(body, &mut data.szInfo);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }

        pub fn set_status(&self, pending_count: i64, unhealthy: bool) {
            let raw = self.hwnd.load(Ordering::Acquire);
            if raw == 0 {
                return;
            }
            let text = if unhealthy {
                format!("A 股新股申购提醒 · 状态异常 · 待确认 {pending_count}")
            } else {
                format!("A 股新股申购提醒 · 待确认 {pending_count}")
            };
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: HWND(raw as *mut _),
                uID: ICON_ID,
                uFlags: NIF_TIP,
                ..Default::default()
            };
            copy_wide(&text, &mut data.szTip);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }

        pub fn schedule_recovery_smoke(&self, report_path: std::path::PathBuf) -> Result<()> {
            let raw = self.hwnd.load(Ordering::Acquire);
            if raw == 0 {
                bail!("托盘消息窗口尚未创建");
            }
            TASKBAR_READD_SUCCEEDED.store(0, Ordering::Release);
            TASKBAR_READD_FAILED.store(0, Ordering::Release);
            RECOVERY_POWER_MESSAGES.store(0, Ordering::Release);
            RECOVERY_UNLOCK_MESSAGES.store(0, Ordering::Release);
            RECOVERY_TIME_MESSAGES.store(0, Ordering::Release);
            RECOVERY_ACCEPTED.store(0, Ordering::Release);
            RECOVERY_SUPPRESSED.store(0, Ordering::Release);
            RECOVERY_CALLBACKS.store(0, Ordering::Release);
            if let Ok(mut last) = LAST_RECOVERY.get_or_init(|| Mutex::new(None)).lock() {
                *last = None;
            }

            let hwnd = HWND(raw as *mut _);
            let icon_data = create_icon_data(hwnd)?;
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data);
            }
            thread::Builder::new()
                .name("stock-ipo-recovery-smoke".into())
                .spawn(move || {
                    let smoke_hwnd = HWND(raw as *mut _);
                    let taskbar_message = TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire);
                    unsafe {
                        let _ =
                            PostMessageW(Some(smoke_hwnd), taskbar_message, WPARAM(0), LPARAM(0));
                        let _ = PostMessageW(Some(smoke_hwnd), WM_TIMECHANGE, WPARAM(0), LPARAM(0));
                        let _ = PostMessageW(
                            Some(smoke_hwnd),
                            WM_POWERBROADCAST,
                            WPARAM(PBT_APMRESUMEAUTOMATIC as usize),
                            LPARAM(0),
                        );
                        let _ = PostMessageW(
                            Some(smoke_hwnd),
                            WM_WTSSESSION_CHANGE,
                            WPARAM(WTS_SESSION_UNLOCK as usize),
                            LPARAM(0),
                        );
                    }
                    thread::sleep(Duration::from_millis(5_300));
                    unsafe {
                        let _ = PostMessageW(Some(smoke_hwnd), WM_TIMECHANGE, WPARAM(0), LPARAM(0));
                    }
                    thread::sleep(Duration::from_millis(700));

                    let taskbar_succeeded = TASKBAR_READD_SUCCEEDED.load(Ordering::Acquire);
                    let taskbar_failed = TASKBAR_READD_FAILED.load(Ordering::Acquire);
                    let power_messages = RECOVERY_POWER_MESSAGES.load(Ordering::Acquire);
                    let unlock_messages = RECOVERY_UNLOCK_MESSAGES.load(Ordering::Acquire);
                    let time_messages = RECOVERY_TIME_MESSAGES.load(Ordering::Acquire);
                    let accepted = RECOVERY_ACCEPTED.load(Ordering::Acquire);
                    let suppressed = RECOVERY_SUPPRESSED.load(Ordering::Acquire);
                    let callbacks = RECOVERY_CALLBACKS.load(Ordering::Acquire);
                    let success = taskbar_succeeded == 1
                        && taskbar_failed == 0
                        && power_messages == 1
                        && unlock_messages == 1
                        && time_messages == 2
                        && accepted == 2
                        && suppressed == 2
                        && callbacks == 2;
                    let report = serde_json::json!({
                        "schemaVersion": "1",
                        "success": success,
                        "version": env!("CARGO_PKG_VERSION"),
                        "generatedAtUtc": chrono::Utc::now().to_rfc3339(),
                        "taskbarCreated": {
                            "iconRemovedBeforeSimulation": true,
                            "reRegistrationSucceeded": taskbar_succeeded,
                            "reRegistrationFailed": taskbar_failed
                        },
                        "recoveryMessages": {
                            "powerResume": power_messages,
                            "sessionUnlock": unlock_messages,
                            "timeChange": time_messages,
                            "acceptedAfterDebounce": accepted,
                            "suppressedByFiveSecondDebounce": suppressed,
                            "runtimeCallbacks": callbacks
                        }
                    });
                    let write_result = report_path
                        .parent()
                        .map(std::fs::create_dir_all)
                        .transpose()
                        .and_then(|_| {
                            serde_json::to_vec_pretty(&report)
                                .map_err(std::io::Error::other)
                                .and_then(|bytes| std::fs::write(&report_path, bytes))
                        });
                    if let Err(error) = write_result {
                        crate::operations::log(
                            "ERROR",
                            &format!("写入 Windows 恢复 smoke 报告失败：{error}"),
                        );
                    }
                    let _ = slint::invoke_from_event_loop(|| {
                        let _ = slint::quit_event_loop();
                    });
                })
                .context("无法启动 Windows 恢复 smoke 线程")?;
            Ok(())
        }
    }

    impl Drop for NativeTray {
        fn drop(&mut self) {
            let raw = self.hwnd.load(Ordering::Acquire);
            if raw != 0 {
                unsafe {
                    let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    unsafe fn run_tray(
        hwnd_slot: Arc<AtomicIsize>,
        initialized: mpsc::SyncSender<std::result::Result<(), String>>,
        activation_message_name: String,
    ) -> Result<()> {
        let setup = (|| -> Result<(HWND, NOTIFYICONDATAW)> {
            let module = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW 失败")?;
            let instance = HINSTANCE(module.0);
            let class_name = w!("StockIpoReminderTrayWindow");
            let window_class = WNDCLASSW {
                hInstance: instance,
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            if unsafe { RegisterClassW(&window_class) } == 0 {
                bail!("RegisterClassW 失败");
            }
            let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
            if taskbar_created == 0 {
                bail!("RegisterWindowMessageW(TaskbarCreated) 失败");
            }
            TASKBAR_CREATED_MESSAGE.store(taskbar_created, Ordering::Release);
            let activation_name: Vec<u16> = activation_message_name
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let activation_message =
                unsafe { RegisterWindowMessageW(windows::core::PCWSTR(activation_name.as_ptr())) };
            if activation_message == 0 {
                bail!("RegisterWindowMessageW(ActivateInstance) 失败");
            }
            ACTIVATE_INSTANCE_MESSAGE.store(activation_message, Ordering::Release);
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    class_name,
                    w!("Stock IPO Reminder"),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(instance),
                    None,
                )
            }
            .context("CreateWindowExW 失败")?;
            hwnd_slot.store(hwnd.0 as isize, Ordering::Release);
            unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
                .context("无法注册 Windows 会话恢复通知")?;
            NETWORK_AVAILABLE.store(network_available(), Ordering::Release);
            if unsafe {
                SetTimer(
                    Some(hwnd),
                    NETWORK_TIMER_ID,
                    NETWORK_POLL_MILLISECONDS,
                    None,
                )
            } == 0
            {
                bail!("无法创建网络恢复检测计时器");
            }
            let icon_data = create_icon_data(hwnd)?;
            if !unsafe { Shell_NotifyIconW(NIM_ADD, &icon_data) }.as_bool() {
                bail!("Shell_NotifyIconW(NIM_ADD) 失败");
            }
            Ok((hwnd, icon_data))
        })();
        let (_hwnd, icon_data) = match setup {
            Ok(value) => {
                let _ = initialized.send(Ok(()));
                value
            }
            Err(error) => {
                let _ = initialized.send(Err(format!("{error:#}")));
                return Err(error);
            }
        };
        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let _ = unsafe { KillTimer(Some(_hwnd), NETWORK_TIMER_ID) };
        let _ = unsafe { WTSUnRegisterSessionNotification(_hwnd) };
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data) };
        hwnd_slot.store(0, Ordering::Release);
        Ok(())
    }

    fn create_icon_data(hwnd: HWND) -> Result<NOTIFYICONDATAW> {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: windows_integration::application_icon()?,
            ..Default::default()
        };
        copy_wide("A 股新股申购提醒", &mut data.szTip);
        Ok(data)
    }

    fn copy_wide(value: &str, target: &mut [u16]) {
        let source: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let count = source.len().min(target.len());
        target[..count].copy_from_slice(&source[..count]);
        if count == target.len() {
            target[target.len() - 1] = 0;
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            message
                if message != 0 && message == ACTIVATE_INSTANCE_MESSAGE.load(Ordering::Acquire) =>
            {
                if let Some(callbacks) = CALLBACKS.get() {
                    (callbacks.activate)();
                }
                LRESULT(0)
            }
            message
                if message != 0 && message == TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire) =>
            {
                match create_icon_data(hwnd) {
                    Ok(icon_data) => {
                        if unsafe { Shell_NotifyIconW(NIM_ADD, &icon_data) }.as_bool() {
                            TASKBAR_READD_SUCCEEDED.fetch_add(1, Ordering::AcqRel);
                            crate::operations::log(
                                "INFO",
                                "检测到 Explorer 任务栏重建，托盘图标已重新注册",
                            );
                        } else {
                            TASKBAR_READD_FAILED.fetch_add(1, Ordering::AcqRel);
                            crate::operations::log(
                                "ERROR",
                                "检测到 Explorer 任务栏重建，但托盘图标重新注册失败",
                            );
                        }
                    }
                    Err(error) => crate::operations::log(
                        "ERROR",
                        &format!("Explorer 重启后创建托盘图标数据失败：{error:#}"),
                    ),
                }
                LRESULT(0)
            }
            TRAY_MESSAGE if lparam.0 as u32 == WM_LBUTTONDBLCLK => {
                if let Some(callbacks) = CALLBACKS.get() {
                    (callbacks.show)();
                }
                LRESULT(0)
            }
            TRAY_MESSAGE if lparam.0 as u32 == NIN_BALLOONUSERCLICK => {
                if let Some(callbacks) = CALLBACKS.get() {
                    let event_id = LAST_NOTIFICATION_EVENT
                        .get_or_init(|| Mutex::new(None))
                        .lock()
                        .ok()
                        .and_then(|target| target.clone());
                    (callbacks.notification)(event_id);
                }
                LRESULT(0)
            }
            TRAY_MESSAGE if lparam.0 as u32 == WM_RBUTTONUP => {
                unsafe { show_menu(hwnd) };
                LRESULT(0)
            }
            WM_COMMAND => {
                match wparam.0 & 0xffff {
                    SHOW_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.show)();
                        }
                    }
                    TODAY_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.today)();
                        }
                    }
                    FUTURE_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.future)();
                        }
                    }
                    LOGS_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.logs)();
                        }
                    }
                    SYNC_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.sync)();
                        }
                    }
                    SETTINGS_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.settings)();
                        }
                    }
                    EXIT_COMMAND => {
                        if let Some(callbacks) = CALLBACKS.get() {
                            (callbacks.exit)();
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST
                if matches!(
                    wparam.0 as u32,
                    PBT_APMRESUMEAUTOMATIC
                        | PBT_APMRESUMECRITICAL
                        | PBT_APMRESUMESTANDBY
                        | PBT_APMRESUMESUSPEND
                ) =>
            {
                RECOVERY_POWER_MESSAGES.fetch_add(1, Ordering::AcqRel);
                dispatch_recovery();
                LRESULT(0)
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                RECOVERY_UNLOCK_MESSAGES.fetch_add(1, Ordering::AcqRel);
                dispatch_recovery();
                LRESULT(0)
            }
            WM_TIMECHANGE => {
                RECOVERY_TIME_MESSAGES.fetch_add(1, Ordering::AcqRel);
                dispatch_recovery();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == NETWORK_TIMER_ID => {
                let available = network_available();
                let previous = NETWORK_AVAILABLE.swap(available, Ordering::AcqRel);
                if available && !previous {
                    dispatch_recovery();
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    fn network_available() -> bool {
        let mut flags = 0u32;
        unsafe { IsNetworkAlive(&mut flags) }.is_ok()
    }

    fn dispatch_recovery() {
        let now = Instant::now();
        let gate = LAST_RECOVERY.get_or_init(|| Mutex::new(None));
        let Ok(mut last) = gate.lock() else { return };
        if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(5)) {
            RECOVERY_SUPPRESSED.fetch_add(1, Ordering::AcqRel);
            return;
        }
        RECOVERY_ACCEPTED.fetch_add(1, Ordering::AcqRel);
        *last = Some(now);
        drop(last);
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.recovery)();
        }
    }

    unsafe fn show_menu(hwnd: HWND) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, SHOW_COMMAND, w!("打开主窗口"));
            let _ = AppendMenuW(menu, MF_STRING, TODAY_COMMAND, w!("今日任务"));
            let _ = AppendMenuW(menu, MF_STRING, FUTURE_COMMAND, w!("未来 60 天"));
            let _ = AppendMenuW(menu, MF_STRING, LOGS_COMMAND, w!("打开日志目录"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
            let _ = AppendMenuW(menu, MF_STRING, SYNC_COMMAND, w!("立即同步"));
            let _ = AppendMenuW(menu, MF_STRING, SETTINGS_COMMAND, w!("提醒设置"));
            let _ = AppendMenuW(menu, MF_STRING, EXIT_COMMAND, w!("安全退出"));
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                Some(0),
                hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
        }
    }
}

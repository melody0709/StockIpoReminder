#![cfg_attr(windows, windows_subsystem = "windows")]

mod announcement;
mod core;
mod deployment;
mod model;
mod network;
mod operations;
mod runtime;
mod storage;
mod windows_integration;

use std::{env, fs, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use model::{
    AppSettings, Board, DataQualityStatus, Exchange, HealthState, IpoEvent, LifecycleStatus,
    ReminderLevel,
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

    let options = RuntimeOptions::parse(&arguments);
    fs::create_dir_all(&options.data_root)
        .with_context(|| format!("无法创建数据目录：{}", options.data_root.display()))?;
    operations::initialize(&options.data_root)?;
    if let Some(exit_code) = operations::try_run_self_test(&arguments, &options.data_root)? {
        std::process::exit(exit_code);
    }
    let _instance = windows_integration::SingleInstance::acquire(&options.data_root)?;
    let (runtime, runtime_thread) =
        runtime::start(options.data_root.clone(), !options.skip_startup_sync)?;

    let ui = MainWindow::new().context("无法创建 Slint 主窗口")?;
    ui.window()
        .on_close_requested(|| CloseRequestResponse::HideWindow);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_data_root_text(format!("数据目录：{}", options.data_root.display()).into());
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
    if !initial_settings.onboarding_completed {
        ui.set_active_page(3);
    }
    refresh_ui(&ui, &runtime);

    #[cfg(windows)]
    let tray = Arc::new(
        native_tray::NativeTray::start(ui.as_weak(), runtime.clone())
            .context("无法创建 windows-rs 系统托盘")?,
    );

    #[cfg(windows)]
    wire_callbacks(
        &ui,
        runtime.clone(),
        options.data_root.clone(),
        Arc::clone(&tray),
    );
    #[cfg(not(windows))]
    wire_callbacks(&ui, runtime.clone(), options.data_root.clone());

    let weak = ui.as_weak();
    let polling_runtime = runtime.clone();
    #[cfg(windows)]
    let polling_tray = Arc::clone(&tray);
    let polling_timer = Timer::default();
    polling_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let Some(ui) = weak.upgrade() else { return };
        refresh_ui(&ui, &polling_runtime);
        #[cfg(windows)]
        {
            let snapshot = polling_runtime.snapshot();
            polling_tray.set_status(
                snapshot.pending_count,
                snapshot.last_sync_succeeded == Some(false)
                    || snapshot.health_state == HealthState::Failed,
            );
        }
        while let Some(event) = polling_runtime.try_event() {
            match event {
                UiEvent::Reminder(delivery) => {
                    let title = format!(
                        "{}（{}）打新提醒",
                        delivery.event.name,
                        delivery.event.display_code()
                    );
                    let body = reminder_body(&delivery.event, delivery.level);
                    ui.set_reminder_title(title.clone().into());
                    ui.set_reminder_body(body.clone().into());
                    ui.set_reminder_event_id(delivery.event.id.clone().into());
                    ui.set_reminder_event_version(delivery.event.event_version);
                    ui.set_show_reminder(true);
                    let settings = polling_runtime.settings().unwrap_or_default();
                    if settings.sound_enabled {
                        windows_integration::play_alert();
                    }
                    if settings.flash_taskbar {
                        windows_integration::flash_window(ui.window());
                    }
                    #[cfg(windows)]
                    if settings.toast_enabled {
                        polling_tray.notify(&title, &body, Some(&delivery.event.id));
                    }
                    show_and_repaint(&ui);
                }
                UiEvent::Health { state: _, text } => {
                    ui.set_reminder_title("每日健康摘要".into());
                    ui.set_reminder_body(text.clone().into());
                    ui.set_reminder_event_id("".into());
                    ui.set_show_reminder(true);
                    #[cfg(windows)]
                    polling_tray.notify("A 股打新提醒 · 健康摘要", &text, None);
                }
            }
        }
    });

    ui.show().context("无法显示主窗口")?;
    #[cfg(windows)]
    {
        let icon_window = ui.as_weak();
        Timer::single_shot(Duration::from_millis(50), move || {
            if let Some(window) = icon_window.upgrade() {
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

    // The application is tray-resident: hiding the last visible window must not
    // terminate Slint's event loop. Only an explicit quit action should exit.
    let run_result = slint::run_event_loop_until_quit().context("Slint 事件循环异常");
    let _ = ui.hide();
    runtime.stop();
    let _ = runtime_thread.join();
    drop(polling_timer);
    run_result
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

fn wire_callbacks(
    ui: &MainWindow,
    runtime: RuntimeHandle,
    data_root: PathBuf,
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
            settings.notification_self_test_completed = ui.get_notification_test_completed();
            settings.onboarding_completed = settings.notification_self_test_completed;
            let sync_minutes = parse_sync_interval(
                ui.get_sync_interval_value().as_str(),
                ui.get_sync_interval_unit_index(),
            )?;
            settings.normal_sync_minutes = sync_minutes;
            settings.active_day_sync_minutes = sync_minutes;
            settings_runtime.save_settings(&settings)?;
            windows_integration::set_auto_start(
                settings.auto_start_enabled,
                &env::current_exe()?,
                &settings_data_root,
            )?;
            settings_runtime.request_sync("设置变更");
            Ok(())
        })();
        ui.set_status_text(match result {
            Ok(()) => "设置已保存，提醒计划已重算".into(),
            Err(error) => format!("保存设置失败：{error:#}").into(),
        });
        apply_settings(&ui, &settings_runtime.settings().unwrap_or_default());
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

    let test_runtime = runtime.clone();
    #[cfg(windows)]
    let test_tray = Arc::clone(&tray);
    let weak = ui.as_weak();
    ui.on_test_notifications(move || {
        let Some(ui) = weak.upgrade() else { return };
        let settings = test_runtime.settings().unwrap_or_default();
        if settings.sound_enabled {
            windows_integration::play_alert();
        }
        if settings.flash_taskbar {
            windows_integration::flash_window(ui.window());
        }
        #[cfg(windows)]
        if settings.toast_enabled {
            test_tray.notify("A 股打新提醒 · 通道测试", "这是一条提醒通道测试消息", None);
        }
        ui.set_show_notification_confirmation(true);
        show_and_repaint(&ui);
    });

    let complete_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_complete_notification_test(move |completed| {
        let Some(ui) = weak.upgrade() else { return };
        let result = (|| -> Result<()> {
            let mut settings = complete_runtime.settings().unwrap_or_default();
            settings.notification_self_test_completed = completed;
            settings.onboarding_completed = completed;
            complete_runtime.save_settings(&settings)
        })();
        if result.is_ok() {
            ui.set_notification_test_completed(completed);
            ui.set_onboarding_completed(completed);
            ui.set_notification_test_status(if completed {
                "提醒通道测试已由你确认通过".into()
            } else {
                "测试未确认通过，请检查通知权限和声音设置后重试".into()
            });
        } else if let Err(error) = result {
            ui.set_notification_test_status(format!("保存测试结果失败：{error:#}").into());
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

fn parse_sync_interval(value: &str, unit_index: i32) -> Result<i32> {
    let amount: i32 = value.trim().parse().context("自动同步间隔必须是整数")?;
    if amount <= 0 {
        anyhow::bail!("自动同步间隔必须大于 0");
    }
    let minutes = match unit_index {
        0 => amount,
        1 => amount.checked_mul(60).context("自动同步间隔超出可用范围")?,
        _ => anyhow::bail!("自动同步间隔单位无效"),
    };
    if !(5..=7 * 24 * 60).contains(&minutes) {
        anyhow::bail!("自动同步间隔应在 5 分钟到 7 天之间");
    }
    Ok(minutes)
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
    let sync_minutes = settings.normal_sync_minutes.clamp(5, 7 * 24 * 60);
    if sync_minutes % 60 == 0 {
        ui.set_sync_interval_value((sync_minutes / 60).to_string().into());
        ui.set_sync_interval_unit_index(1);
    } else {
        ui.set_sync_interval_value(sync_minutes.to_string().into());
        ui.set_sync_interval_unit_index(0);
    }
    ui.set_notification_test_completed(settings.notification_self_test_completed);
    ui.set_onboarding_completed(settings.onboarding_completed);
    ui.set_notification_test_status(if settings.notification_self_test_completed {
        "提醒通道已测试".into()
    } else {
        "尚未完成提醒通道测试".into()
    });
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
    ui.set_today_count(today.len() as i32);
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
        today
            .iter()
            .map(|event| task_row(event, &settings))
            .collect::<Vec<_>>(),
    ))));
    ui.set_future_tasks(ModelRc::from(Rc::new(VecModel::from(
        future
            .iter()
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
                "今日任务 {} 只，待确认 {} 只，来源冲突 {} 只，待人工核验 {} 只。",
                health.today_task_count,
                health.pending_confirmation_count,
                health.conflict_count,
                health.manual_review_count,
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
    matches!(
        event.lifecycle_status,
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

fn reminder_body(event: &IpoEvent, level: ReminderLevel) -> String {
    let level_text = match level {
        ReminderLevel::Advance => "明日申购预告",
        ReminderLevel::Morning => "今日申购提醒",
        ReminderLevel::BrokerOpening | ReminderLevel::MarketOpening => "申购通道即将开放",
        ReminderLevel::FifteenMinutes
        | ReminderLevel::FiveMinutes
        | ReminderLevel::TwoMinutes
        | ReminderLevel::Final => "接近安全截止时间",
        _ => "申购任务尚未确认",
    };
    format!(
        "{level_text}\n申购代码：{}\n发行价：{}\n请在券商客户端完成后点击“确认已申购”。",
        event.display_code(),
        event
            .issue_price
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "待核验".into())
    )
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

struct RuntimeOptions {
    data_root: PathBuf,
    background: bool,
    exit_after: Option<Duration>,
    skip_startup_sync: bool,
    skip_auto_start_registration: bool,
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
        }
    }
}

#[cfg(windows)]
mod native_tray {
    use std::{
        mem::size_of,
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, AtomicIsize, Ordering},
            mpsc,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail};
    use slint::{ComponentHandle, Weak};
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
                    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING, NIM_ADD, NIM_DELETE,
                    NIM_MODIFY, NIN_BALLOONUSERCLICK, NOTIFYICONDATAW, Shell_NotifyIconW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                    DispatchMessageW, GetCursorPos, GetMessageW, HWND_MESSAGE, KillTimer,
                    MF_STRING, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL,
                    PBT_APMRESUMESTANDBY, PBT_APMRESUMESUSPEND, PostMessageW, PostQuitMessage,
                    RegisterClassW, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
                    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
                    WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
                    WM_POWERBROADCAST, WM_RBUTTONUP, WM_TIMECHANGE, WM_TIMER, WM_WTSSESSION_CHANGE,
                    WNDCLASSW, WTS_SESSION_UNLOCK,
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
    const ICON_ID: u32 = 1;
    const NETWORK_TIMER_ID: usize = 2001;
    const NETWORK_POLL_MILLISECONDS: u32 = 10_000;

    struct Callbacks {
        show: Box<dyn Fn() + Send + Sync>,
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

    pub struct NativeTray {
        hwnd: Arc<AtomicIsize>,
        thread: Option<JoinHandle<()>>,
    }

    impl NativeTray {
        pub fn start(window: Weak<MainWindow>, runtime: RuntimeHandle) -> Result<Self> {
            let show_window = window.clone();
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
                    recovery_runtime.recovery();
                    recovery_runtime.request_sync("系统恢复或时间变化");
                }),
            });
            let hwnd = Arc::new(AtomicIsize::new(0));
            let thread_hwnd = Arc::clone(&hwnd);
            let (sender, receiver) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("stock-ipo-native-tray".into())
                .spawn(move || unsafe {
                    let _ = run_tray(thread_hwnd, sender);
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
                dwInfoFlags: NIIF_WARNING,
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
                    Some(HWND_MESSAGE),
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
                dispatch_recovery();
                LRESULT(0)
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
                dispatch_recovery();
                LRESULT(0)
            }
            WM_TIMECHANGE => {
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
            return;
        }
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

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
mod ui;
mod updater;
mod watchdog;
mod windows_integration;

#[cfg(windows)]
mod native_tray;

use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::NaiveTime;
use model::{
    AnnouncementDocument, AppSettings, Board, DataQualityStatus, Exchange, FieldSourceEntry,
    HealthState, IpoEvent, LifecycleStatus, ManualOverrideEntry, ReminderDelivery, ReminderLevel,
    SecondaryNotificationProvider,
};
use runtime::{RuntimeHandle, UiEvent};
use slint::{CloseRequestResponse, ComponentHandle, Model, ModelRc, Timer, VecModel};
use ui::*;

slint::include_modules!();

fn main() -> Result<()> {
    let startup_started = Instant::now();
    let arguments: Vec<String> = env::args().skip(1).collect();
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
    run_application(options, startup_started)
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

fn run_application(options: RuntimeOptions, startup_started: Instant) -> Result<()> {
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
    let restored_window_size = match prepare_main_window_size_restore(&options.data_root) {
        Ok(size) => size,
        Err(error) => {
            operations::log(
                "WARN",
                &format!("恢复主窗口尺寸失败，使用默认大小：{error:#}"),
            );
            None
        }
    };
    let close_window = ui.as_weak();
    let close_data_root = options.data_root.clone();
    ui.window().on_close_requested(move || {
        if let Some(window) = close_window.upgrade() {
            persist_main_window_size(&window, &close_data_root);
        }
        CloseRequestResponse::HideWindow
    });
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
    refresh_ui(&ui, &runtime);
    let available_update = Arc::new(Mutex::new(None::<updater::AvailableUpdate>));
    let update_check_busy = Arc::new(AtomicBool::new(false));
    let update_install_busy = Arc::new(AtomicBool::new(false));
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
        Arc::clone(&update_check_busy),
        Arc::clone(&update_install_busy),
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
        Arc::clone(&update_check_busy),
        Arc::clone(&update_install_busy),
        Arc::clone(&crash_upload_busy),
        Arc::clone(&secondary_notification_busy),
    );
    wire_reminder_callbacks(&reminder_window, &ui, runtime.clone());

    install_runtime_ui_bridge(
        ui.as_weak(),
        reminder_window.as_weak(),
        runtime.clone(),
        options.data_root.clone(),
        Arc::clone(&available_update),
        Arc::clone(&update_check_busy),
        Arc::clone(&crash_upload_busy),
        update_configured,
        crash_upload_configured,
        options.skip_auto_start_registration,
        options.skip_update_check,
        options.skip_crash_upload,
        #[cfg(windows)]
        Arc::clone(&tray),
    );
    let window_size_timer = start_main_window_size_persistence(
        ui.as_weak(),
        options.data_root.clone(),
        restored_window_size,
    );

    #[cfg(windows)]
    operations::log(
        "INFO",
        &format!(
            "应用已驻留系统托盘，主窗口默认保持隐藏：elapsedMs={}",
            startup_started.elapsed().as_millis()
        ),
    );
    #[cfg(not(windows))]
    {
        ui.show().context("无法显示主窗口")?;
        apply_restored_main_window_size(&ui);
        operations::log(
            "INFO",
            &format!(
                "启动界面已呈现：elapsedMs={}",
                startup_started.elapsed().as_millis()
            ),
        );
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
    window_size_timer.stop();
    persist_main_window_size(&ui, &options.data_root);
    let _ = ui.hide();
    let _ = reminder_window.hide();
    // 等 UI 发起的后台数据库/文件操作全部安全收尾，避免进程退出时硬中断事务。
    wait_for_ui_workers();
    runtime.remove_ui_notifier();
    runtime.stop();
    let _ = runtime_thread.join();
    run_result
}

struct RuntimeOptions {
    data_root: PathBuf,
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
                // 兼容旧版自启动、Watchdog 与发布脚本；Windows 现在默认只驻留托盘。
                "--background" => {}
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

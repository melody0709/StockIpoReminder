use super::*;

pub(crate) fn wire_reminder_callbacks(
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

pub(crate) fn wire_callbacks(
    ui: &MainWindow,
    reminder_window: &ReminderWindow,
    runtime: RuntimeHandle,
    data_root: PathBuf,
    available_update: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    update_check_busy: Arc<AtomicBool>,
    update_install_busy: Arc<AtomicBool>,
    crash_upload_busy: Arc<AtomicBool>,
    secondary_notification_busy: Arc<AtomicBool>,
    #[cfg(windows)] tray: Arc<native_tray::NativeTray>,
) {
    let ui_write_busy = Arc::new(AtomicBool::new(false));
    let diagnostics_busy = Arc::new(AtomicBool::new(false));

    wire_settings_callbacks(
        ui,
        runtime.clone(),
        data_root.clone(),
        Arc::clone(&ui_write_busy),
    );
    wire_task_callbacks(ui, runtime.clone(), Arc::clone(&ui_write_busy));
    wire_diagnostic_callbacks(ui, runtime.clone(), data_root.clone(), diagnostics_busy);
    #[cfg(windows)]
    wire_notification_callbacks(ui, reminder_window, runtime.clone(), Arc::clone(&tray));
    #[cfg(not(windows))]
    wire_notification_callbacks(ui, reminder_window, runtime.clone());
    wire_update_callbacks(
        ui,
        data_root.clone(),
        available_update,
        update_check_busy,
        update_install_busy,
    );
    wire_crash_callbacks(ui, data_root.clone(), crash_upload_busy);
    wire_secondary_callbacks(ui, data_root.clone(), runtime, secondary_notification_busy);
    wire_application_callbacks(ui, data_root);
}

pub(crate) fn save_settings_with_rollback(
    runtime: &RuntimeHandle,
    data_root: &std::path::Path,
    settings: &AppSettings,
    secondary_secret: Option<&str>,
) -> Result<()> {
    let previous = runtime.settings()?;
    let secret_snapshot = secondary_notification::snapshot_secret(data_root)?;
    let executable = env::current_exe()?;
    let auto_start_snapshot = windows_integration::auto_start_registered(data_root)?;

    if let Some(secret) = secondary_secret {
        secondary_notification::save_secret(
            data_root,
            settings.secondary_notification_provider,
            secret,
        )?;
    }
    if settings.secondary_notification_enabled
        && !secondary_notification::configured(data_root, settings)
    {
        let error = anyhow::anyhow!("启用第二通知通道前必须保存与当前服务商匹配的有效凭据");
        return Err(with_rollback_errors(
            error,
            [secondary_notification::restore_secret(
                data_root,
                &secret_snapshot,
            )],
        ));
    }

    if let Err(error) = runtime.save_settings(settings) {
        return Err(with_rollback_errors(
            error.context("无法提交应用设置和提醒计划"),
            [secondary_notification::restore_secret(
                data_root,
                &secret_snapshot,
            )],
        ));
    }

    if let Err(error) =
        windows_integration::set_auto_start(settings.auto_start_enabled, &executable, data_root)
    {
        return Err(with_rollback_errors(
            error.context("无法提交开机自启动设置"),
            [
                runtime.save_settings(&previous),
                secondary_notification::restore_secret(data_root, &secret_snapshot),
                windows_integration::set_auto_start(auto_start_snapshot, &executable, data_root),
            ],
        ));
    }
    Ok(())
}

pub(crate) fn clear_secondary_notification_with_rollback(
    runtime: &RuntimeHandle,
    data_root: &std::path::Path,
) -> Result<()> {
    let previous = runtime.settings()?;
    let secret_snapshot = secondary_notification::snapshot_secret(data_root)?;
    let mut disabled = previous.clone();
    disabled.secondary_notification_enabled = false;
    runtime
        .save_settings(&disabled)
        .context("无法停止第二通知通道")?;
    if let Err(error) = secondary_notification::clear_secret(data_root) {
        return Err(with_rollback_errors(
            error,
            [
                runtime.save_settings(&previous),
                secondary_notification::restore_secret(data_root, &secret_snapshot),
            ],
        ));
    }
    Ok(())
}

pub(crate) fn with_rollback_errors<const N: usize>(
    primary: anyhow::Error,
    rollbacks: [Result<()>; N],
) -> anyhow::Error {
    let failures = rollbacks
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        primary
    } else {
        anyhow::anyhow!("{primary:#}；回滚未完全成功：{}", failures.join("；"))
    }
}

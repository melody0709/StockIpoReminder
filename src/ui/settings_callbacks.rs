use super::*;

pub(crate) fn settings_base_for_save(
    stored_settings: AppSettings,
    reset_settings_pending: bool,
) -> AppSettings {
    if reset_settings_pending {
        AppSettings::default()
    } else {
        stored_settings
    }
}

pub(crate) fn wire_settings_callbacks(
    ui: &MainWindow,
    runtime: RuntimeHandle,
    data_root: PathBuf,
    ui_write_busy: Arc<AtomicBool>,
) {
    let reset_weak = ui.as_weak();
    ui.on_reset_settings(move || {
        let Some(ui) = reset_weak.upgrade() else {
            return;
        };
        apply_settings(&ui, &AppSettings::default());
        ui.set_secondary_notification_secret_entry("".into());
        ui.set_secondary_notification_configured(false);
        ui.set_secondary_notification_status("已恢复默认值；保存后将关闭第二通知通道。".into());
        ui.set_reset_settings_pending(true);
        ui.set_status_text(
            "已恢复默认设置；点击“保存设置”后生效。本地任务、缓存和通知凭据不会删除。".into(),
        );
    });

    let weak = ui.as_weak();
    let settings_runtime = runtime.clone();
    let settings_data_root = data_root.clone();
    let settings_save_busy = Arc::clone(&ui_write_busy);
    ui.on_save_settings(move || {
        let Some(ui) = weak.upgrade() else { return };
        // 输入收集与校验留在 UI 线程；数据库写入与重规划移到工作线程。
        let collected = (|| -> Result<(AppSettings, String)> {
            if !ui.get_shanghai_enabled() && !ui.get_shenzhen_enabled() && !ui.get_beijing_enabled()
            {
                anyhow::bail!("至少需要启用一个市场");
            }
            let mut settings = settings_base_for_save(
                settings_runtime.settings()?,
                ui.get_reset_settings_pending(),
            );
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
            settings.secondary_notification_provider = secondary_provider;
            settings.secondary_notification_enabled = ui.get_secondary_notification_enabled()
                && secondary_provider != SecondaryNotificationProvider::Disabled;
            if settings.notification_tests_started() {
                settings.notification_self_test_completed = settings.notification_tests_complete();
                settings.onboarding_completed = settings.notification_self_test_completed;
            }
            let normal_sync_minutes = parse_sync_interval(
                ui.get_normal_sync_interval_value().as_str(),
                ui.get_normal_sync_interval_unit_index(),
                "来源异常重试间隔",
            )?;
            let active_day_sync_minutes = parse_sync_interval(
                ui.get_active_sync_interval_value().as_str(),
                ui.get_active_sync_interval_unit_index(),
                "当日未确认任务核验间隔",
            )?;
            if active_day_sync_minutes > normal_sync_minutes {
                anyhow::bail!("当日未确认任务核验间隔不能大于来源异常重试间隔");
            }
            settings.normal_sync_minutes = normal_sync_minutes;
            settings.active_day_sync_minutes = active_day_sync_minutes;
            Ok((settings, secondary_secret))
        })();
        let (settings, secondary_secret) = match collected {
            Ok(collected) => collected,
            Err(error) => {
                ui.set_status_text(format!("保存设置失败：{error:#}").into());
                return;
            }
        };
        if settings_save_busy.swap(true, Ordering::AcqRel) {
            ui.set_status_text("上一次设置保存仍在处理，请稍候再试".into());
            return;
        }
        ui.set_status_text("正在保存设置并重算提醒计划…".into());
        let save_runtime = settings_runtime.clone();
        let save_data_root = settings_data_root.clone();
        let save_window = ui.as_weak();
        let settings_save_busy_worker = Arc::clone(&settings_save_busy);
        let spawned = spawn_ui_worker("settings-save", move || {
            let save_error = save_settings_with_rollback(
                &save_runtime,
                &save_data_root,
                &settings,
                (!secondary_secret.is_empty()).then_some(secondary_secret.as_str()),
            )
            .err();
            if save_error.is_none() {
                save_runtime.request_sync("设置变更");
            }
            let saved_settings = save_runtime.settings();
            let callback_runtime = save_runtime.clone();
            let callback_data_root = save_data_root.clone();
            let _ = slint::invoke_from_event_loop(move || {
                settings_save_busy_worker.store(false, Ordering::Release);
                let Some(ui) = save_window.upgrade() else {
                    return;
                };
                if let Some(error) = save_error {
                    ui.set_status_text(format!("保存设置失败：{error:#}").into());
                } else {
                    ui.set_status_text("设置已保存，提醒计划已重算".into());
                    ui.set_secondary_notification_secret_entry("".into());
                    ui.set_reset_settings_pending(false);
                }
                if let Ok(saved_settings) = &saved_settings {
                    apply_settings(&ui, saved_settings);
                    refresh_secondary_notification_ui(
                        &ui,
                        &callback_data_root,
                        saved_settings,
                        &callback_runtime,
                    );
                }
                refresh_ui(&ui, &save_runtime);
            });
        });
        if let Err(error) = spawned {
            settings_save_busy.store(false, Ordering::Release);
            ui.set_status_text(format!("无法启动保存线程：{error}").into());
        }
    });
}

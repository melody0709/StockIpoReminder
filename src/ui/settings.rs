use super::*;

pub(crate) fn parse_time(value: &str, field: &str) -> Result<NaiveTime> {
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

pub(crate) fn parse_sync_interval(value: &str, unit_index: i32, field: &str) -> Result<i32> {
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

pub(crate) fn sync_interval_display(minutes: i32) -> (String, i32) {
    let minutes = minutes.clamp(5, 7 * 24 * 60);
    if minutes % 60 == 0 {
        ((minutes / 60).to_string(), 1)
    } else {
        (minutes.to_string(), 0)
    }
}

pub(crate) fn apply_settings(ui: &MainWindow, settings: &AppSettings) {
    let notification_tests_complete =
        settings.notification_self_test_completed || settings.notification_tests_complete();
    let onboarding_completed = settings.onboarding_completed || notification_tests_complete;
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
    ui.set_notification_test_completed(notification_tests_complete);
    ui.set_onboarding_completed(onboarding_completed);
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
    let (platform_status, platform_level) =
        toast_platform_display(&windows_integration::toast_diagnostics());
    ui.set_notification_platform_status(platform_status.into());
    ui.set_notification_platform_level(platform_level);
    ui.set_notification_test_status(
        if notification_tests_complete && !settings.notification_tests_started() {
            "旧版整体测试已通过；建议使用上方按钮逐项复测".into()
        } else if notification_tests_complete {
            "当前启用的提醒通道已确认；系统通知的 Toast 或气泡回退至少一项可用".into()
        } else if settings.notification_tests_started() {
            "仍有启用的通道未测试或未通过，请逐项处理".into()
        } else {
            "尚未完成提醒通道测试".into()
        },
    );
}

pub(crate) fn secondary_provider_index(provider: SecondaryNotificationProvider) -> i32 {
    match provider {
        SecondaryNotificationProvider::WeCom => 1,
        SecondaryNotificationProvider::DingTalk => 2,
        SecondaryNotificationProvider::Feishu => 3,
        SecondaryNotificationProvider::PushPlus => 4,
        _ => 0,
    }
}

pub(crate) fn secondary_provider_from_index(index: i32) -> SecondaryNotificationProvider {
    match index {
        1 => SecondaryNotificationProvider::WeCom,
        2 => SecondaryNotificationProvider::DingTalk,
        3 => SecondaryNotificationProvider::Feishu,
        4 => SecondaryNotificationProvider::PushPlus,
        _ => SecondaryNotificationProvider::Disabled,
    }
}

pub(crate) fn refresh_secondary_notification_ui(
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

pub(crate) fn toast_platform_display(
    diagnostics: &windows_integration::ToastDiagnostics,
) -> (String, i32) {
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

pub(crate) fn toast_setting_text(setting: &str) -> &'static str {
    match setting {
        "enabled" => "权限已启用",
        "disabledForApplication" => "已在系统设置中针对本应用关闭",
        "disabledForUser" => "当前用户已关闭系统通知",
        "disabledByGroupPolicy" => "被组策略关闭",
        "disabledByManifestOrRegistration" => "安装注册或应用标识不完整",
        _ => "权限状态未知",
    }
}

pub(crate) fn toast_presentation_text(state: &str) -> &'static str {
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

pub(crate) fn notification_test_display(value: Option<bool>, label: &str) -> (String, i32) {
    match value {
        Some(true) => (format!("{label}：通过"), 1),
        Some(false) => (format!("{label}：未通过"), 2),
        None => (format!("{label}：未测试"), 0),
    }
}

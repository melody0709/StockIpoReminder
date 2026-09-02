use super::*;

pub(crate) fn wire_notification_callbacks(
    ui: &MainWindow,
    reminder_window: &ReminderWindow,
    runtime: RuntimeHandle,
    #[cfg(windows)] tray: Arc<native_tray::NativeTray>,
) {
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
                    None,
                ) {
                    let save_result =
                        record_notification_test_result(&test_runtime, channel, false);
                    if let Ok(settings) = test_runtime.settings() {
                        apply_settings(&ui, &settings);
                    }
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
            if let Ok(settings) = complete_runtime.settings() {
                apply_settings(&ui, &settings);
            }
        } else if let Err(error) = result {
            ui.set_notification_test_status(format!("保存测试结果失败：{error:#}").into());
        }
    });
}

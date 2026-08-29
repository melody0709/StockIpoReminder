use super::*;

pub(crate) fn wire_secondary_callbacks(
    ui: &MainWindow,
    data_root: PathBuf,
    runtime: RuntimeHandle,
    secondary_notification_busy: Arc<AtomicBool>,
) {
    let secondary_test_window = ui.as_weak();
    let secondary_test_root = data_root.clone();
    let secondary_test_runtime = runtime.clone();
    let secondary_test_busy = Arc::clone(&secondary_notification_busy);
    ui.on_test_secondary_notification(move || {
        let settings = match secondary_test_runtime.settings() {
            Ok(settings) => settings,
            Err(error) => {
                if let Some(ui) = secondary_test_window.upgrade() {
                    ui.set_secondary_notification_status(
                        format!("无法读取第二通知通道设置：{error:#}").into(),
                    );
                }
                return;
            }
        };
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
            clear_secondary_notification_with_rollback(
                &secondary_clear_runtime,
                &secondary_clear_root,
            )?;
            Ok(())
        })();
        if let Ok(settings) = secondary_clear_runtime.settings() {
            apply_settings(&ui, &settings);
            refresh_secondary_notification_ui(
                &ui,
                &secondary_clear_root,
                &settings,
                &secondary_clear_runtime,
            );
        }
        ui.set_status_text(match result {
            Ok(()) => "第二通知通道凭据已清除并停止发送".into(),
            Err(error) => format!("清除第二通知通道凭据失败：{error:#}").into(),
        });
    });
}

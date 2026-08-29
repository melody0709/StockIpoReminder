use super::*;

pub(crate) fn wire_application_callbacks(ui: &MainWindow, data_root: PathBuf) {
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

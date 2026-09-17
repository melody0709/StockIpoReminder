use super::*;

pub(crate) fn wire_update_callbacks(ui: &MainWindow, controller: Arc<UpdateController>) {
    let check_ui = ui.as_weak();
    let check_controller = Arc::clone(&controller);
    ui.on_check_for_updates(move || {
        if let Some(ui) = check_ui.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
        }
        check_controller.check(true);
    });

    // 绿色胶囊：一次点击即授权下载、安装与重启到托盘。
    let request_controller = Arc::clone(&controller);
    ui.on_request_update(move || {
        request_controller.request_update();
    });

    let dismiss_ui = ui.as_weak();
    let dismiss_controller = Arc::clone(&controller);
    ui.on_dismiss_update(move || {
        if let Some(ui) = dismiss_ui.upgrade() {
            let version = ui.get_update_version().to_string();
            dismiss_controller.dismiss(&version);
        }
    });

    let download_controller = Arc::clone(&controller);
    ui.on_open_update_download(move || {
        download_controller.open_manual_download();
    });

    // 版本徽标：手动检查入口，直接切到设置页更新区域。
    let settings_ui = ui.as_weak();
    ui.on_open_update_settings(move || {
        if let Some(ui) = settings_ui.upgrade() {
            ui.set_active_page(3);
            ui.set_settings_section(3);
        }
    });

    let notes_ui = ui.as_weak();
    ui.on_open_release_notes(move || {
        if let Some(ui) = notes_ui.upgrade() {
            let target = ui.get_update_release_notes_url();
            ui.set_status_text(if target.is_empty() {
                "当前签名清单没有提供发布说明文件".into()
            } else {
                match windows_integration::open_external(&target) {
                    Ok(()) => "已使用默认浏览器打开发布说明".into(),
                    Err(error) => format!("无法打开发布说明：{error:#}").into(),
                }
            });
        }
    });
}

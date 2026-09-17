use super::*;

pub(crate) fn wire_update_callbacks(ui: &MainWindow, controller: Arc<UpdateController>) {
    let check_ui = ui.as_weak();
    let check_controller = Arc::clone(&controller);
    ui.on_check_for_updates(move || {
        if let Some(ui) = check_ui.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
            ui.set_update_available(false);
        }
        check_controller.check(false);
    });

    let download_ui = ui.as_weak();
    let download_controller = Arc::clone(&controller);
    ui.on_download_update(move || {
        if let Some(ui) = download_ui.upgrade() {
            ui.set_update_download_failed(false);
            ui.set_update_status("正在下载并验证更新安装包…".into());
        }
        download_controller.download(false);
    });

    let install_controller = Arc::clone(&controller);
    ui.on_install_update(move || {
        install_controller.install();
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

use super::*;

pub(crate) fn wire_crash_callbacks(
    ui: &MainWindow,
    data_root: PathBuf,
    crash_upload_busy: Arc<AtomicBool>,
) {
    let crash_window = ui.as_weak();
    let crash_root = data_root.clone();
    let crash_busy = Arc::clone(&crash_upload_busy);
    ui.on_upload_crash_report(move || {
        if let Some(ui) = crash_window.upgrade() {
            ui.set_crash_upload_status("正在二次脱敏并发送最近的崩溃报告…".into());
        }
        start_crash_upload(
            crash_window.clone(),
            crash_root.clone(),
            Arc::clone(&crash_busy),
            false,
        );
    });

    let privacy_window = ui.as_weak();
    ui.on_open_crash_upload_privacy(move || {
        if let Some(ui) = privacy_window.upgrade() {
            ui.set_status_text(match crash_upload::privacy_url() {
                Some(url) => match windows_integration::open_external(url) {
                    Ok(()) => "已使用默认浏览器打开崩溃报告隐私政策".into(),
                    Err(error) => format!("无法打开隐私政策：{error:#}").into(),
                },
                None => "当前构建未配置崩溃报告隐私政策".into(),
            });
        }
    });
}

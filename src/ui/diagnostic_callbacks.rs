use super::*;

pub(crate) fn wire_diagnostic_callbacks(
    ui: &MainWindow,
    runtime: RuntimeHandle,
    data_root: PathBuf,
    diagnostics_busy: Arc<AtomicBool>,
) {
    let weak = ui.as_weak();
    ui.on_open_external(move |target| {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(match windows_integration::open_external(target.as_str()) {
                Ok(()) => "已使用默认浏览器打开公告原文".into(),
                Err(error) => format!("打开公告原文失败：{error:#}").into(),
            });
        }
    });

    let weak = ui.as_weak();
    ui.on_open_local(move |target| {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(
                match windows_integration::open_local_file(std::path::Path::new(target.as_str())) {
                    Ok(()) => "已使用默认程序打开本地公告".into(),
                    Err(error) => format!("打开本地公告失败：{error:#}").into(),
                },
            );
        }
    });

    let diagnostic_runtime = runtime.clone();
    let diagnostic_root = data_root.clone();
    let weak = ui.as_weak();
    let diagnostics_swap_busy = Arc::clone(&diagnostics_busy);
    ui.on_create_diagnostics(move || {
        let Some(ui) = weak.upgrade() else { return };
        if diagnostics_swap_busy.swap(true, Ordering::AcqRel) {
            ui.set_status_text("诊断包正在生成中，请稍候…".into());
            return;
        }
        ui.set_status_text("正在生成诊断包（完整性检查与打包在后台进行）…".into());
        let diagnostics_window = ui.as_weak();
        let diagnostics_store_busy = Arc::clone(&diagnostics_swap_busy);
        let worker_runtime = diagnostic_runtime.clone();
        let worker_root = diagnostic_root.clone();
        let spawned = spawn_ui_worker("diagnostics", move || {
            let result = worker_runtime
                .database()
                .and_then(|database| operations::create_diagnostic_bundle(&worker_root, database));
            let _ = slint::invoke_from_event_loop(move || {
                diagnostics_store_busy.store(false, Ordering::Release);
                let Some(ui) = diagnostics_window.upgrade() else {
                    return;
                };
                ui.set_status_text(match result {
                    Ok(path) => {
                        if let Some(directory) = path.parent() {
                            let _ = windows_integration::open_folder(directory);
                        }
                        format!("诊断包已生成：{}", path.display()).into()
                    }
                    Err(error) => format!("诊断包生成失败：{error:#}").into(),
                });
            });
        });
        if let Err(error) = spawned {
            diagnostics_swap_busy.store(false, Ordering::Release);
            ui.set_status_text(format!("无法启动诊断线程：{error}").into());
        }
    });

    let open_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_open_data_folder(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text(match windows_integration::open_folder(&open_root) {
                Ok(()) => "已打开数据目录".into(),
                Err(error) => format!("打开数据目录失败：{error:#}").into(),
            });
        }
    });

    let copy_runtime = runtime.clone();
    let copy_root = data_root.clone();
    let weak = ui.as_weak();
    ui.on_copy_diagnostics(move || {
        if let Some(ui) = weak.upgrade() {
            let summary = diagnostic_summary(&copy_runtime, &copy_root);
            ui.set_status_text(match windows_integration::copy_text(&summary) {
                Ok(()) => "诊断摘要已复制到剪贴板".into(),
                Err(error) => format!("复制诊断摘要失败：{error:#}").into(),
            });
        }
    });
}

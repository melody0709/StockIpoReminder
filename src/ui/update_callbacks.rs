use super::*;

pub(crate) fn wire_update_callbacks(
    ui: &MainWindow,
    data_root: PathBuf,
    available_update: Arc<Mutex<Option<updater::AvailableUpdate>>>,
    update_check_busy: Arc<AtomicBool>,
    update_install_busy: Arc<AtomicBool>,
) {
    let check_window = ui.as_weak();
    let check_state = Arc::clone(&available_update);
    let check_busy = Arc::clone(&update_check_busy);
    ui.on_check_for_updates(move || {
        if let Some(ui) = check_window.upgrade() {
            ui.set_update_status("正在下载并验证签名更新清单…".into());
            ui.set_update_available(false);
        }
        start_update_check(
            check_window.clone(),
            Arc::clone(&check_state),
            Arc::clone(&check_busy),
            false,
        );
    });

    let install_window = ui.as_weak();
    let install_state = Arc::clone(&available_update);
    let install_busy = Arc::clone(&update_install_busy);
    let update_root = data_root.clone();
    ui.on_install_update(move || {
        let Some(ui) = install_window.upgrade() else {
            return;
        };
        let Some(gate) = OperationGate::acquire(Arc::clone(&install_busy)) else {
            ui.set_update_status("已有更新安装任务正在运行".into());
            return;
        };
        let update = install_state
            .lock()
            .ok()
            .and_then(|value| value.as_ref().cloned());
        let Some(update) = update else {
            drop(gate);
            ui.set_update_status("没有可安装且已验证的更新".into());
            return;
        };
        ui.set_update_status(format!("正在下载并验证 {} 安装包…", update.manifest.version).into());
        let result_window = install_window.clone();
        let data_root = update_root.clone();
        std::thread::spawn(move || {
            let result = updater::download_and_request_install(&data_root, &update);
            drop(gate);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = result_window.upgrade() {
                    match result {
                        Ok(detail) => {
                            ui.set_update_status(detail.into());
                            let _ = slint::quit_event_loop();
                        }
                        Err(error) => {
                            ui.set_update_status(format!("更新下载或验证失败：{error:#}").into())
                        }
                    }
                }
            });
        });
    });
}

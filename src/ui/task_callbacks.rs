use super::*;

pub(crate) fn wire_task_callbacks(
    ui: &MainWindow,
    runtime: RuntimeHandle,
    ui_write_busy: Arc<AtomicBool>,
) {
    let sync_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_sync_now(move || {
        sync_runtime.request_sync("用户手动同步");
        if let Some(ui) = weak.upgrade() {
            ui.set_status_text("已提交手动同步请求…".into());
        }
    });

    let refresh_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_refresh_data(move || {
        if let Some(ui) = weak.upgrade() {
            refresh_ui(&ui, &refresh_runtime);
        }
    });

    let select_runtime = runtime.clone();
    let weak = ui.as_weak();
    ui.on_select_task(move |event_id| {
        let Some(ui) = weak.upgrade() else { return };
        show_event_details(&ui, &select_runtime, event_id.as_str());
    });

    let acknowledge_runtime = runtime.clone();
    let weak = ui.as_weak();
    let acknowledge_busy = Arc::clone(&ui_write_busy);
    ui.on_acknowledge(move |event_id, version| {
        let Some(ui) = weak.upgrade() else { return };
        if acknowledge_busy.swap(true, Ordering::AcqRel) {
            ui.set_status_text("上一次操作仍在处理，请稍候再试".into());
            return;
        }
        ui.set_status_text("正在记录确认…".into());
        let ack_runtime = acknowledge_runtime.clone();
        let ack_event_id = event_id.to_string();
        let ack_window = ui.as_weak();
        let acknowledge_busy_worker = Arc::clone(&acknowledge_busy);
        let spawned = spawn_ui_worker("acknowledge", move || {
            let result = ack_runtime.acknowledge(&ack_event_id, version);
            let _ = slint::invoke_from_event_loop(move || {
                acknowledge_busy_worker.store(false, Ordering::Release);
                let Some(ui) = ack_window.upgrade() else {
                    return;
                };
                ui.set_status_text(match result {
                    Ok(()) => "已记录确认，当前版本后续提醒已取消".into(),
                    Err(error) => format!("确认失败：{error:#}").into(),
                });
                refresh_ui(&ui, &ack_runtime);
            });
        });
        if let Err(error) = spawned {
            acknowledge_busy.store(false, Ordering::Release);
            ui.set_status_text(format!("无法启动后台线程：{error}").into());
        }
    });

    let revoke_runtime = runtime.clone();
    let weak = ui.as_weak();
    let revoke_busy = Arc::clone(&ui_write_busy);
    ui.on_revoke_acknowledgement(move |event_id, version| {
        let Some(ui) = weak.upgrade() else { return };
        if revoke_busy.swap(true, Ordering::AcqRel) {
            ui.set_status_text("上一次操作仍在处理，请稍候再试".into());
            return;
        }
        ui.set_status_text("正在撤销确认…".into());
        let revoke_ack_runtime = revoke_runtime.clone();
        let revoke_event_id = event_id.to_string();
        let revoke_window = ui.as_weak();
        let revoke_busy_worker = Arc::clone(&revoke_busy);
        let spawned = spawn_ui_worker("revoke-acknowledgement", move || {
            let result = revoke_ack_runtime.revoke_acknowledgement(&revoke_event_id, version);
            let _ = slint::invoke_from_event_loop(move || {
                revoke_busy_worker.store(false, Ordering::Release);
                let Some(ui) = revoke_window.upgrade() else {
                    return;
                };
                ui.set_status_text(match result {
                    Ok(()) => "已撤销确认，截止时间前的提醒已重新规划".into(),
                    Err(error) => format!("撤销失败：{error:#}").into(),
                });
                refresh_ui(&ui, &revoke_ack_runtime);
            });
        });
        if let Err(error) = spawned {
            revoke_busy.store(false, Ordering::Release);
            ui.set_status_text(format!("无法启动后台线程：{error}").into());
        }
    });

    let override_runtime = runtime.clone();
    let weak = ui.as_weak();
    let apply_override_busy = Arc::clone(&ui_write_busy);
    ui.on_apply_override(
        move |event_id, version, field_index, value, reason, announcement_index| {
            let Some(ui) = weak.upgrade() else { return };
            let field = override_field_name(field_index);
            let announcement_id = if announcement_index > 0 {
                let selected = ui
                    .get_announcement_rows()
                    .row_data((announcement_index - 1) as usize);
                let Some(selected) = selected else {
                    ui.set_status_text("所选依据公告已变化，请刷新详情后重试".into());
                    ui.set_override_status("所选依据公告已变化，请刷新详情后重试".into());
                    return;
                };
                Some(selected.id.to_string())
            } else {
                None
            };
            if apply_override_busy.swap(true, Ordering::AcqRel) {
                ui.set_status_text("上一次操作仍在处理，请稍候再试".into());
                return;
            }
            ui.set_status_text("正在保存人工覆盖…".into());
            ui.set_override_status("正在保存人工覆盖…".into());
            let save_override_runtime = override_runtime.clone();
            let save_event_id = event_id.to_string();
            let save_value = value.to_string();
            let save_reason = reason.to_string();
            let override_window = ui.as_weak();
            let apply_override_busy_worker = Arc::clone(&apply_override_busy);
            let spawned = spawn_ui_worker("apply-override", move || {
                let result = save_override_runtime.apply_override(
                    &save_event_id,
                    version,
                    field,
                    &save_value,
                    &save_reason,
                    announcement_id.as_deref(),
                );
                let override_event_id = save_event_id.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    apply_override_busy_worker.store(false, Ordering::Release);
                    let Some(ui) = override_window.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            ui.set_status_text("人工覆盖已保存，并已重新规划提醒".into());
                            ui.set_override_status("人工覆盖已保存，提醒计划已重算".into());
                            ui.set_override_value("".into());
                            ui.set_override_reason("".into());
                        }
                        Err(error) => {
                            ui.set_status_text(format!("保存人工覆盖失败：{error:#}").into());
                            ui.set_override_status(format!("保存失败：{error:#}").into());
                        }
                    }
                    refresh_ui(&ui, &save_override_runtime);
                    show_event_details(&ui, &save_override_runtime, override_event_id.as_str());
                    ui.set_details_active_tab(3);
                });
            });
            if let Err(error) = spawned {
                apply_override_busy.store(false, Ordering::Release);
                ui.set_status_text(format!("无法启动后台线程：{error}").into());
            }
        },
    );

    let revoke_override_runtime = runtime.clone();
    let weak = ui.as_weak();
    let revoke_override_busy = Arc::clone(&ui_write_busy);
    ui.on_revoke_override(move |event_id, version, override_id| {
        let Some(ui) = weak.upgrade() else { return };
        let Ok(override_record_id) = override_id.as_str().parse::<i64>() else {
            let error = anyhow::anyhow!("人工覆盖记录编号无效：{}", override_id.as_str());
            ui.set_status_text(format!("撤销人工覆盖失败：{error:#}").into());
            ui.set_override_status(format!("撤销失败：{error:#}").into());
            return;
        };
        if revoke_override_busy.swap(true, Ordering::AcqRel) {
            ui.set_status_text("上一次操作仍在处理，请稍候再试".into());
            return;
        }
        ui.set_status_text("正在撤销人工覆盖…".into());
        ui.set_override_status("正在撤销人工覆盖…".into());
        let revoke_runtime2 = revoke_override_runtime.clone();
        let revoke_event_id = event_id.to_string();
        let override_window = ui.as_weak();
        let revoke_override_busy_worker = Arc::clone(&revoke_override_busy);
        let spawned = spawn_ui_worker("revoke-override", move || {
            let result =
                revoke_runtime2.revoke_override(&revoke_event_id, version, override_record_id);
            let override_event_id = revoke_event_id.clone();
            let _ = slint::invoke_from_event_loop(move || {
                revoke_override_busy_worker.store(false, Ordering::Release);
                let Some(ui) = override_window.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => {
                        ui.set_status_text("人工覆盖已撤销，并已重新规划提醒".into());
                        ui.set_override_status("人工覆盖已撤销，提醒计划已重算".into());
                    }
                    Err(error) => {
                        ui.set_status_text(format!("撤销人工覆盖失败：{error:#}").into());
                        ui.set_override_status(format!("撤销失败：{error:#}").into());
                    }
                }
                refresh_ui(&ui, &revoke_runtime2);
                show_event_details(&ui, &revoke_runtime2, override_event_id.as_str());
                ui.set_details_active_tab(3);
            });
        });
        if let Err(error) = spawned {
            revoke_override_busy.store(false, Ordering::Release);
            ui.set_status_text(format!("无法启动后台线程：{error}").into());
        }
    });
}

use super::*;

pub(crate) fn refresh_snapshot(database: &Database, ui_state: &RuntimeUiState) {
    let events = database.today_events().unwrap_or_default();
    let pending = database.pending_count().unwrap_or_default();
    let health = database
        .health_text()
        .unwrap_or_else(|error| (HealthState::Failed, format!("健康状态读取失败：{error}")));
    update_snapshot(ui_state, |value| {
        value.today_count = events.len();
        value.pending_count = pending;
        value.health_state = health.0;
        value.health_text = health.1;
        if !value.is_synchronizing
            && (value.status_text == "正在后台准备本地数据库…"
                || value.status_text.starts_with("本地数据已就绪"))
        {
            value.status_text = "后台提醒服务已就绪".into();
        }
    });
}

pub(crate) fn update_snapshot(
    ui_state: &RuntimeUiState,
    update: impl FnOnce(&mut RuntimeSnapshot),
) {
    let mut changed = false;
    if let Ok(mut value) = ui_state.snapshot.write() {
        let previous = value.clone();
        let revision = value.revision;
        update(&mut value);
        value.revision = revision;
        if *value != previous {
            value.revision = revision.wrapping_add(1);
            changed = true;
        }
    }
    if changed {
        ui_state.notify();
    }
}

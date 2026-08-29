use super::*;

pub(crate) fn run_delivery_cycle(
    database: &Database,
    events: &mpsc::Sender<UiEvent>,
    ui_state: &RuntimeUiState,
    data_root: &Path,
) -> Result<bool> {
    let local = database.claim_due(20)?;
    let mut did_work = !local.is_empty();
    for delivery in local {
        if let Err(error) = send_ui_event(events, ui_state, UiEvent::Reminder(delivery.clone())) {
            database.fail_delivery(delivery.outbox_id, &error.to_string())?;
        }
    }
    let secondary = database.claim_secondary_due(20)?;
    if !secondary.is_empty() {
        did_work = true;
        match secondary_notification::send_batch(data_root, &secondary) {
            Ok(receipt) => {
                database.complete_secondary_deliveries(
                    &secondary,
                    secondary_notification::provider_label(receipt.provider),
                )?;
                operations::log("INFO", &receipt.message());
            }
            Err(error) => {
                let message = operations::redact(&format!("{error:#}"));
                database.fail_secondary_deliveries(&secondary, &message)?;
                operations::log("WARN", &format!("第二通知通道发送失败：{message}"));
            }
        }
    }
    Ok(did_work)
}

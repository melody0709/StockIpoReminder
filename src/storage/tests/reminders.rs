use super::*;

#[test]
fn next_delivery_deadlines_include_retries_and_secondary_leases() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let now = now_china();
    let local_retry = now + chrono::Duration::minutes(5);
    let local_pending = now + chrono::Duration::minutes(10);
    let secondary_retry = now + chrono::Duration::minutes(3);
    let connection = test.database.open().unwrap();
    connection
        .execute("DELETE FROM reminder_outbox", [])
        .unwrap();
    connection
            .execute(
                "INSERT INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,lease_until,delivery_state,attempt_count,created_at,updated_at) VALUES(?1,?2,?3,?4,'fixture-failed',?5,?6,1,?7,?7)",
                params![
                    event.id,
                    event.event_version,
                    format_dt(now - chrono::Duration::minutes(1)),
                    ReminderLevel::Morning as i32,
                    format_dt(local_retry),
                    DeliveryState::Failed as i32,
                    format_dt(now),
                ],
            )
            .unwrap();
    connection
            .execute(
                "INSERT INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,delivery_state,created_at,updated_at) VALUES(?1,?2,?3,?4,'fixture-pending',?5,?6,?6)",
                params![
                    event.id,
                    event.event_version,
                    format_dt(local_pending),
                    ReminderLevel::Hourly as i32,
                    DeliveryState::Pending as i32,
                    format_dt(now),
                ],
            )
            .unwrap();
    let reminder_id = connection.last_insert_rowid();
    connection
            .execute(
                "INSERT INTO secondary_notification_outbox(reminder_outbox_id,provider,state,attempt_count,next_attempt_at,lease_until,created_at,updated_at) VALUES(?1,?2,?3,1,?4,?5,?6,?6)",
                params![
                    reminder_id,
                    SecondaryNotificationProvider::PushPlus as i32,
                    SECONDARY_LEASED,
                    format_dt(now),
                    format_dt(secondary_retry),
                    format_dt(now),
                ],
            )
            .unwrap();

    assert_eq!(
        test.database.next_local_delivery_at().unwrap(),
        Some(parse_dt(&format_dt(local_retry)).unwrap())
    );
    assert_eq!(
        test.database.next_secondary_delivery_at(now).unwrap(),
        Some(parse_dt(&format_dt(secondary_retry)).unwrap())
    );
}

#[test]
fn apply_reminders_are_not_delivered_after_the_trading_window() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let date = event.apply_date.unwrap();
    let claim_at = crate::core::at(date, crate::model::time(14, 56));

    let deliveries = test.database.claim_due_at(50, claim_at).unwrap();
    assert!(deliveries.is_empty());
    let summary = test.database.reminder_state_summary().unwrap();
    assert!(summary.collapsed > 0);
    assert_eq!(summary.leased, 0);
    assert!(summary.cancelled > 0);
}

#[test]
fn overdue_apply_reminders_are_not_delivered_during_the_lunch_break() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let claim_at = crate::core::at(event.apply_date.unwrap(), crate::model::time(12, 0));

    assert!(test.database.claim_due_at(50, claim_at).unwrap().is_empty());
    assert_eq!(test.database.reminder_state_summary().unwrap().leased, 0);
}

#[test]
fn local_delivery_failures_use_bounded_backoff_and_expose_error_summary() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let date = event.apply_date.unwrap();
    let first_attempt = crate::core::at(date, crate::model::time(10, 0));
    let first = test.database.claim_due_at(50, first_attempt).unwrap();
    assert_eq!(first.len(), 1);
    test.database
        .fail_delivery_at(first[0].outbox_id, "fixture render failure", first_attempt)
        .unwrap();
    assert!(
        test.database
            .claim_due_at(50, first_attempt + chrono::Duration::seconds(59))
            .unwrap()
            .is_empty()
    );

    let second_attempt = first_attempt + chrono::Duration::minutes(1);
    let second = test.database.claim_due_at(50, second_attempt).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].attempt_count, 2);
    test.database
        .fail_delivery_at(
            second[0].outbox_id,
            "fixture render failure again",
            second_attempt,
        )
        .unwrap();
    assert!(
        test.database
            .claim_due_at(50, second_attempt + chrono::Duration::minutes(4))
            .unwrap()
            .is_empty()
    );
    let retry_summary = test.database.reminder_state_summary().unwrap();
    assert_eq!(retry_summary.failed, 1);
    assert!(
        retry_summary
            .latest_error
            .as_deref()
            .is_some_and(|value| value.contains("again"))
    );
    assert_eq!(
        test.database
            .claim_due_at(50, second_attempt + chrono::Duration::minutes(5))
            .unwrap()
            .len(),
        1
    );
    let summary = test.database.reminder_state_summary().unwrap();
    assert_eq!(summary.failed, 0);
    assert!(summary.latest_error.is_none());
}

#[test]
fn outbox_recovers_across_reopen_at_queue_lease_display_and_confirmation_stages() {
    let test = TestDatabase::new();
    let event = test.database.upsert_event(test.event()).unwrap();
    let date = event.apply_date.unwrap();
    let first_claim_at = crate::core::at(date, crate::model::time(10, 0));

    let reopened = Database::new(&test.root);
    reopened.initialize().unwrap();
    let leased = reopened.claim_due_at(100, first_claim_at).unwrap();
    assert!(!leased.is_empty());

    let before_expiry = Database::new(&test.root);
    before_expiry.initialize().unwrap();
    assert!(
        before_expiry
            .claim_due_at(100, first_claim_at + chrono::Duration::minutes(1))
            .unwrap()
            .is_empty()
    );

    let after_expiry = Database::new(&test.root);
    after_expiry.initialize().unwrap();
    let reclaimed = after_expiry
        .claim_due_at(100, first_claim_at + chrono::Duration::minutes(3))
        .unwrap();
    assert_eq!(reclaimed.len(), leased.len());
    assert!(reclaimed.iter().all(|delivery| delivery.attempt_count == 2));

    let displayed = reclaimed[0].clone();
    after_expiry.complete_delivery(&displayed, "test").unwrap();
    let after_display_crash = Database::new(&test.root);
    after_display_crash.initialize().unwrap();
    let remaining = after_display_crash
        .claim_due_at(100, first_claim_at + chrono::Duration::minutes(6))
        .unwrap();
    assert!(
        remaining
            .iter()
            .all(|delivery| delivery.outbox_id != displayed.outbox_id)
    );

    let mut today_event = after_display_crash.event(&event.id).unwrap().unwrap();
    today_event.apply_date = Some(date);
    today_event.lifecycle_status = LifecycleStatus::ActiveUnconfirmed;
    today_event.updated_at = crate::core::at(date, crate::model::time(10, 10));
    let today_event = after_display_crash.upsert_event(today_event).unwrap();
    after_display_crash
        .acknowledge_at(
            &today_event.id,
            today_event.event_version,
            crate::core::at(date, crate::model::time(10, 10)),
        )
        .unwrap();

    let after_confirmation_crash = Database::new(&test.root);
    after_confirmation_crash.initialize().unwrap();
    assert!(
        after_confirmation_crash
            .claim_due_at(100, crate::core::at(date, crate::model::time(10, 13)))
            .unwrap()
            .is_empty()
    );
}

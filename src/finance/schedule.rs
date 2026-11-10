use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use super::{
    FinanceKind, FinanceReminder, TradingCalendar, credit_card::due_reminders as account_due,
    next_future, repo,
};
use crate::{model::ChinaDateTime, storage::Database};

fn sort_and_dedup(reminders: &mut Vec<FinanceReminder>) {
    reminders.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.source_id.cmp(&right.source_id))
            .then_with(|| left.occurrence_key.cmp(&right.occurrence_key))
    });
    let mut identities = BTreeSet::new();
    reminders.retain(|item| {
        identities.insert((
            item.kind,
            item.source_id.clone(),
            item.occurrence_key.clone(),
        ))
    });
}

pub(crate) fn due_finance_reminders(
    database: &Database,
    now: ChinaDateTime,
    is_blocked: impl Fn(&FinanceReminder) -> bool,
) -> Result<Vec<FinanceReminder>> {
    let calendar = TradingCalendar::builtin();
    let repo_settings = database.repo_settings()?;
    let mut reminders = Vec::new();
    if let Some(item) = repo::due_reminder(
        now,
        &repo_settings,
        calendar,
        |date| database.repo_day_acked(date).unwrap_or(false),
        |key| {
            database
                .finance_is_shown(FinanceKind::ReverseRepo.as_str(), repo::SOURCE_ID, key)
                .unwrap_or(false)
        },
    ) {
        if !is_blocked(&item) {
            reminders.push(item);
        }
    }
    for account in database.payment_accounts()? {
        if !account.enabled || account.closed_at.is_some() {
            continue;
        }
        let settled = database.payment_account_settled(&account.id)?;
        let kind = account.kind.finance_kind();
        for item in account_due(now, &account, settled.as_deref(), |key| {
            database
                .finance_is_shown(kind.as_str(), &account.id, key)
                .unwrap_or(false)
        }) {
            if !is_blocked(&item) {
                reminders.push(item);
            }
        }
    }
    sort_and_dedup(&mut reminders);
    Ok(reminders)
}

pub(crate) fn next_finance_wake(
    database: &Database,
    now: ChinaDateTime,
    retry_pending: &BTreeMap<super::FinanceReminderIdentity, ChinaDateTime>,
) -> Result<Option<ChinaDateTime>> {
    let calendar = TradingCalendar::builtin();
    let repo_settings = database.repo_settings()?;
    let mut next = repo::next_future_slot(now, &repo_settings, calendar, |date| {
        database.repo_day_acked(date).unwrap_or(false)
    });
    for account in database.payment_accounts()? {
        let settled = database.payment_account_settled(&account.id)?;
        if let Some(candidate) = next_future(now, &account, settled.as_deref())
            && next.is_none_or(|current| candidate < current)
        {
            next = Some(candidate);
        }
    }
    for retry_at in retry_pending.values() {
        if *retry_at > now && next.is_none_or(|current| *retry_at < current) {
            next = Some(*retry_at);
        }
    }
    Ok(next.filter(|value| *value > now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        finance::{PaymentAccount, PaymentAccountKind},
        model::time,
    };

    fn dt(value: &str) -> ChinaDateTime {
        value.parse().unwrap()
    }

    fn reminder(kind: FinanceKind, source_id: &str, key: &str) -> FinanceReminder {
        let due_at = dt("2026-10-02T09:00:00+08:00");
        FinanceReminder {
            kind,
            source_id: source_id.into(),
            occurrence_key: key.into(),
            event_date: due_at.date_naive(),
            due_at,
            valid_until: due_at,
            title: "测试".into(),
            body: "测试".into(),
            primary_action_label: "本期已还".into(),
            primary_action_cycle: Some("2026-10".into()),
        }
    }

    fn account(id: &str, kind: PaymentAccountKind) -> PaymentAccount {
        let now = dt("2026-09-01T00:00:00+08:00");
        PaymentAccount {
            id: id.into(),
            kind,
            alias: id.into(),
            bank_id: None,
            bank_name: None,
            due_day: 5,
            remind_time: time(9, 0),
            lead_days: vec![3],
            overdue_enabled: false,
            enabled: true,
            first_tracked_cycle: "2026-10".into(),
            final_cycle: kind.is_loan().then(|| "2026-10".into()),
            closed_at: None,
            rules_changed_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn aggregate_dedup_uses_kind_source_and_occurrence_key() {
        let mut reminders = vec![
            reminder(FinanceKind::CreditCard, "same-source", "same-key"),
            reminder(FinanceKind::Mortgage, "same-source", "same-key"),
            reminder(FinanceKind::CreditCard, "other-source", "same-key"),
            reminder(FinanceKind::CreditCard, "same-source", "same-key"),
        ];
        sort_and_dedup(&mut reminders);
        assert_eq!(reminders.len(), 3);
        assert!(
            reminders
                .iter()
                .any(|item| item.kind == FinanceKind::Mortgage)
        );
        assert!(
            reminders
                .iter()
                .any(|item| item.source_id == "other-source")
        );
    }

    #[test]
    fn schedule_generates_all_account_types_and_keeps_legacy_card_ledger() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-schedule-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let database = Database::new(&root);
        database.initialize().unwrap();
        for (id, kind) in [
            ("card", PaymentAccountKind::CreditCard),
            ("mortgage", PaymentAccountKind::Mortgage),
            ("auto", PaymentAccountKind::AutoLoan),
        ] {
            database.save_payment_account(&account(id, kind)).unwrap();
            database
                .set_payment_account_rules_changed_at_for_test(id, dt("2026-09-01T00:00:00+08:00"))
                .unwrap();
        }
        let legacy_key = "card:2026-10:3:2026-10-05";
        database
            .mark_finance_shown(
                "credit_card",
                "card",
                legacy_key,
                dt("2026-10-02T09:00:00+08:00").date_naive(),
            )
            .unwrap();

        let reminders =
            due_finance_reminders(&database, dt("2026-10-02T09:00:00+08:00"), |_| false).unwrap();
        assert!(
            !reminders
                .iter()
                .any(|item| item.kind == FinanceKind::CreditCard)
        );
        assert!(reminders.iter().any(|item| {
            item.kind == FinanceKind::Mortgage
                && item.occurrence_key == "mortgage:mortgage:2026-10:3:2026-10-05"
        }));
        assert!(reminders.iter().any(|item| {
            item.kind == FinanceKind::AutoLoan
                && item.occurrence_key == "auto_loan:auto:2026-10:3:2026-10-05"
        }));
        let _ = std::fs::remove_dir_all(root);
    }
}

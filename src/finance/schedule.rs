use std::collections::BTreeMap;

use anyhow::Result;

use super::{
    FinanceKind, FinanceReminder, TradingCalendar, credit_card::due_reminders as card_due,
    next_future, repo,
};
use crate::{model::ChinaDateTime, storage::Database};

pub(crate) fn due_finance_reminders(
    database: &Database,
    now: ChinaDateTime,
    is_blocked: impl Fn(&str) -> bool,
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
        if !is_blocked(&item.occurrence_key) {
            reminders.push(item);
        }
    }
    for card in database.credit_cards()? {
        if !card.enabled {
            continue;
        }
        let settled = database.credit_card_settled(&card.id)?;
        for item in card_due(now, &card, settled.as_deref(), |key| {
            database
                .finance_is_shown(FinanceKind::CreditCard.as_str(), &card.id, key)
                .unwrap_or(false)
        }) {
            if !is_blocked(&item.occurrence_key) {
                reminders.push(item);
            }
        }
    }
    reminders.sort_by(|left, right| {
        left.due_at
            .cmp(&right.due_at)
            .then_with(|| left.occurrence_key.cmp(&right.occurrence_key))
    });
    reminders.dedup_by(|left, right| left.occurrence_key == right.occurrence_key);
    Ok(reminders)
}

pub(crate) fn next_finance_wake(
    database: &Database,
    now: ChinaDateTime,
    retry_pending: &BTreeMap<String, ChinaDateTime>,
) -> Result<Option<ChinaDateTime>> {
    let calendar = TradingCalendar::builtin();
    let repo_settings = database.repo_settings()?;
    let mut next = repo::next_future_slot(now, &repo_settings, calendar, |date| {
        database.repo_day_acked(date).unwrap_or(false)
    });
    for card in database.credit_cards()? {
        let settled = database.credit_card_settled(&card.id)?;
        if let Some(candidate) = next_future(now, &card, settled.as_deref())
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

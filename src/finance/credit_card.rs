use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, NaiveTime};

use super::{FinanceKind, FinanceReminder};
use crate::{
    core::at,
    model::{ChinaDateTime, time},
};

const LOOKAHEAD_MONTHS: i32 = 3;
const MAX_LEAD_DAYS: u32 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreditCard {
    pub id: String,
    pub alias: String,
    pub bank: Option<String>,
    pub due_day: u32,
    pub remind_time: NaiveTime,
    pub lead_days: Vec<u32>,
    pub overdue_enabled: bool,
    pub enabled: bool,
    pub first_tracked_cycle: String,
    pub rules_changed_at: ChinaDateTime,
    pub created_at: ChinaDateTime,
    pub updated_at: ChinaDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CardStatus {
    pub next_due: NaiveDate,
    pub days_left: i64,
    pub overdue_days: i64,
    pub label: String,
    pub tone: i32,
}

pub(crate) fn cycle_key(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

pub(crate) fn parse_cycle(cycle: &str) -> Option<(i32, u32)> {
    let (year, month) = cycle.split_once('-')?;
    let year = year.parse().ok()?;
    let month = month.parse().ok()?;
    (1..=12).contains(&month).then_some((year, month))
}

pub(crate) fn shift_cycle(cycle: &str, months: i32) -> Result<String> {
    let (year, month) = parse_cycle(cycle).context("账期格式必须是 YYYY-MM")?;
    let total = year * 12 + month as i32 - 1 + months;
    let year = total.div_euclid(12);
    let month = total.rem_euclid(12) as u32 + 1;
    Ok(format!("{year:04}-{month:02}"))
}

pub(crate) fn due_on_for_cycle(cycle: &str, due_day: u32) -> Result<NaiveDate> {
    let (year, month) = parse_cycle(cycle).context("账期格式必须是 YYYY-MM")?;
    let last_day = last_day_of_month(year, month);
    NaiveDate::from_ymd_opt(year, month, due_day.clamp(1, last_day)).context("生成信用卡还款日失败")
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|date| date.pred_opt())
        .map(|date| date.day())
        .unwrap_or(28)
}

pub(crate) fn first_tracked_cycle(today: NaiveDate, due_day: u32) -> String {
    let current = cycle_key(today);
    let due = due_on_for_cycle(&current, due_day).unwrap_or(today);
    if today <= due {
        current
    } else {
        shift_cycle(&current, 1).unwrap_or(current)
    }
}

pub(crate) fn normalize_lead_days(raw: impl IntoIterator<Item = u32>) -> Result<Vec<u32>> {
    let mut values = BTreeSet::new();
    for value in raw {
        if value > MAX_LEAD_DAYS {
            bail!("提前提醒天数不能超过 {MAX_LEAD_DAYS} 天");
        }
        values.insert(value);
    }
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable_by(|left, right| right.cmp(left));
    if values.len() > 4 {
        bail!("最多选择 4 个提前提醒档位");
    }
    if values.is_empty() {
        values.push(0);
    }
    Ok(values)
}

pub(crate) fn validate_card(card: &CreditCard) -> Result<()> {
    if card.alias.trim().is_empty() {
        bail!("信用卡别名不能为空");
    }
    if card.alias.chars().count() > 30 {
        bail!("信用卡别名不能超过 30 个字");
    }
    if let Some(bank) = card.bank.as_ref()
        && bank.chars().count() > 20
    {
        bail!("银行名称不能超过 20 个字");
    }
    if !(1..=31).contains(&card.due_day) {
        bail!("还款日必须是 1–31");
    }
    if card.remind_time < time(8, 0) || card.remind_time > time(22, 0) {
        bail!("提醒时间必须在 08:00–22:00 之间");
    }
    normalize_lead_days(card.lead_days.iter().copied())?;
    Ok(())
}

pub(crate) fn earliest_unsettled_cycle(
    card: &CreditCard,
    settled: Option<&str>,
    today: NaiveDate,
) -> Option<String> {
    let current = cycle_key(today);
    let mut cycle = card.first_tracked_cycle.clone();
    if let Some(settled) = settled
        && let Ok(next) = shift_cycle(settled, 1)
        && next > cycle
    {
        cycle = next;
    }
    for _ in 0..=120 {
        if cycle > current {
            return None;
        }
        let due = due_on_for_cycle(&cycle, card.due_day).ok()?;
        if due <= today {
            return Some(cycle);
        }
        cycle = shift_cycle(&cycle, 1).ok()?;
    }
    None
}

/// 「撤销已还」的可用窗口：只允许撤销当前账期或上一账期的动作。
///
/// `credit_card_actions` 不裁剪，`settled` 取的是 `MAX(cycle_key)`，所以只要还过一次，
/// 撤销按钮就会永久存在——几个月后依然能把一条早已离开当前账期的记录撤掉，既容易误触，
/// 也让卡片长期停在最高的四按钮布局。跨年由 `shift_cycle` 处理。
pub(crate) fn undo_window_open(settled: Option<&str>, today: NaiveDate) -> bool {
    // 解析成 (年, 月) 再比较，不做字符串字典序比较；顺带把损坏的账期挡在外面。
    let Some(settled) = settled.and_then(parse_cycle) else {
        return false;
    };
    let Ok(previous) = shift_cycle(&cycle_key(today), -1) else {
        return false;
    };
    let Some(previous) = parse_cycle(&previous) else {
        return false;
    };
    settled >= previous
}

/// 账期比较：统一解析成 (年, 月) 再比，避免对 "YYYY-MM" 做字典序比较。
fn cycle_at_least(left: &str, right: &str) -> bool {
    match (parse_cycle(left), parse_cycle(right)) {
        (Some(left), Some(right)) => left >= right,
        _ => false,
    }
}

fn cycle_before(left: &str, right: &str) -> bool {
    match (parse_cycle(left), parse_cycle(right)) {
        (Some(left), Some(right)) => left < right,
        _ => false,
    }
}

pub(crate) fn status_for_card(
    card: &CreditCard,
    settled: Option<&str>,
    today: NaiveDate,
) -> CardStatus {
    let overdue_cycle = earliest_unsettled_cycle(card, settled, today);
    // 展示账期不能早于 first_tracked_cycle：10/6 新增、每月 5 日还款的卡首期是 11 月，
    // 若按「本月」算会显示成已经过去的 10/5，并把首期提醒窗口标成「未开始」。
    let current = cycle_key(today);
    let base = if cycle_before(&current, &card.first_tracked_cycle) {
        card.first_tracked_cycle.clone()
    } else {
        current.clone()
    };
    let next_cycle = match overdue_cycle.clone() {
        Some(cycle) => cycle,
        None => match settled {
            Some(settled) if cycle_at_least(settled, &base) => {
                shift_cycle(settled, 1).unwrap_or(base)
            }
            _ => base,
        },
    };
    let next_due = due_on_for_cycle(&next_cycle, card.due_day).unwrap_or(today);
    let days_left = (next_due - today).num_days();
    let overdue_days = overdue_cycle
        .as_deref()
        .and_then(|cycle| due_on_for_cycle(cycle, card.due_day).ok())
        .filter(|due| *due < today)
        .map(|due| (today - due).num_days())
        .unwrap_or(0);
    let (label, tone) = if !card.enabled {
        ("已暂停".to_owned(), 0)
    } else if cycle_before(&cycle_key(today), &card.first_tracked_cycle) {
        // 只看「首期账期是否已经开始」，不再用「今天早于首期还款日」：
        // 后者会把首期的提前提醒窗口也标成「未开始」。
        ("未开始".to_owned(), 0)
    } else if overdue_days > 0 {
        (format!("已逾期 {overdue_days} 天"), 3)
    } else if days_left == 0 {
        ("今天到期".to_owned(), 2)
    } else if days_left <= 7 {
        (format!("还有 {days_left} 天"), 1)
    } else {
        (format!("还有 {days_left} 天"), 0)
    };
    CardStatus {
        next_due,
        days_left,
        overdue_days,
        label,
        tone,
    }
}

fn end_of_day(date: NaiveDate) -> ChinaDateTime {
    at(date.succ_opt().unwrap_or(date), time(0, 0)) - chrono::Duration::seconds(1)
}

fn reminder(
    card: &CreditCard,
    cycle: &str,
    due: NaiveDate,
    lead: u32,
    now: ChinaDateTime,
) -> FinanceReminder {
    let remind_date = due - chrono::Duration::days(lead as i64);
    let due_at = at(remind_date, card.remind_time);
    let title = if lead == 0 {
        if due < now.date_naive() {
            format!("信用卡还款提醒 · 已逾期")
        } else {
            "信用卡还款提醒 · 今天到期".into()
        }
    } else if due < now.date_naive() {
        "信用卡还款提醒 · 已逾期".into()
    } else {
        format!("信用卡还款提醒 · {lead} 天后")
    };
    let body = format!(
        "{}{}的还款日是 {}。\n本提醒按自然日计算，请以发卡行账单为准；本程序只保存本地提醒状态，不代表银行入账。",
        card.alias,
        card.bank
            .as_ref()
            .map(|bank| format!("（{bank}）"))
            .unwrap_or_default(),
        due.format("%Y-%m-%d")
    );
    FinanceReminder {
        kind: FinanceKind::CreditCard,
        source_id: card.id.clone(),
        // 卡片 ID 必须进 key：去重与 in-flight/重试/已展示台账都只按 key 比对，
        // 不含卡片时两张同还款日、同提前档位的卡会撞成同一个键，其中一张被静默丢掉。
        occurrence_key: format!("{}:{cycle}:{lead}:{due}", card.id),
        event_date: remind_date,
        due_at,
        valid_until: end_of_day(remind_date),
        title,
        body,
        primary_action_label: "本期已还".into(),
        primary_action_cycle: Some(cycle.to_owned()),
    }
}

fn overdue_reminder(card: &CreditCard, cycle: &str, due: NaiveDate) -> FinanceReminder {
    let date = due.succ_opt().unwrap_or(due);
    FinanceReminder {
        kind: FinanceKind::CreditCard,
        source_id: card.id.clone(),
        occurrence_key: format!("{}:{cycle}:overdue:{due}", card.id),
        event_date: date,
        due_at: at(date, card.remind_time),
        valid_until: end_of_day(date),
        title: "信用卡还款提醒 · 已逾期 1 天".into(),
        body: format!(
            "{}的还款日是 {}，目前仍未标记已还。\n若已还款，请打开信用卡页标记本期；本程序不代表银行入账。",
            card.alias,
            due.format("%Y-%m-%d")
        ),
        primary_action_label: "本期已还".into(),
        primary_action_cycle: Some(cycle.to_owned()),
    }
}

pub(crate) fn due_reminders(
    now: ChinaDateTime,
    card: &CreditCard,
    settled: Option<&str>,
    is_shown: impl Fn(&str) -> bool,
) -> Vec<FinanceReminder> {
    if !card.enabled {
        return Vec::new();
    }
    let today = now.date_naive();
    let current = cycle_key(today);
    let mut reminders = Vec::new();
    for offset in -1..=LOOKAHEAD_MONTHS {
        let cycle = match shift_cycle(&current, offset) {
            Ok(cycle) => cycle,
            Err(_) => continue,
        };
        if cycle < card.first_tracked_cycle {
            continue;
        }
        if settled.is_some_and(|settled| cycle.as_str() <= settled) {
            continue;
        }
        let Ok(due) = due_on_for_cycle(&cycle, card.due_day) else {
            continue;
        };
        for lead in &card.lead_days {
            let remind_date = due - chrono::Duration::days(*lead as i64);
            let due_at = at(remind_date, card.remind_time);
            if due_at < card.rules_changed_at {
                continue;
            }
            if due_at <= now && now < end_of_day(remind_date) {
                let item = reminder(card, &cycle, due, *lead, now);
                if !is_shown(&item.occurrence_key) {
                    reminders.push(item);
                }
            }
        }
        if card.overdue_enabled {
            let date = due.succ_opt().unwrap_or(due);
            let due_at = at(date, card.remind_time);
            if due_at >= card.rules_changed_at && due_at <= now && now < end_of_day(date) {
                let item = overdue_reminder(card, &cycle, due);
                if !is_shown(&item.occurrence_key) {
                    reminders.push(item);
                }
            }
        }
    }
    reminders.sort_by_key(|item| item.due_at);
    reminders
}

pub(crate) fn next_future(
    now: ChinaDateTime,
    card: &CreditCard,
    settled: Option<&str>,
) -> Option<ChinaDateTime> {
    if !card.enabled {
        return None;
    }
    let current = cycle_key(now.date_naive());
    let mut next = None;
    for offset in -1..=LOOKAHEAD_MONTHS {
        let cycle = shift_cycle(&current, offset).ok()?;
        if cycle < card.first_tracked_cycle
            || settled.is_some_and(|settled| cycle.as_str() <= settled)
        {
            continue;
        }
        let due = due_on_for_cycle(&cycle, card.due_day).ok()?;
        for lead in &card.lead_days {
            let at_candidate = at(due - chrono::Duration::days(*lead as i64), card.remind_time);
            if at_candidate >= card.rules_changed_at
                && at_candidate > now
                && next.is_none_or(|current| at_candidate < current)
            {
                next = Some(at_candidate);
            }
        }
        if card.overdue_enabled {
            let overdue_at = at(due.succ_opt().unwrap_or(due), card.remind_time);
            if overdue_at >= card.rules_changed_at
                && overdue_at > now
                && next.is_none_or(|current| overdue_at < current)
            {
                next = Some(overdue_at);
            }
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(value: &str) -> ChinaDateTime {
        value.parse().unwrap()
    }

    fn card() -> CreditCard {
        CreditCard {
            id: "card".into(),
            alias: "招行 Visa".into(),
            bank: Some("招商银行".into()),
            due_day: 5,
            remind_time: time(9, 0),
            lead_days: vec![3, 0],
            overdue_enabled: true,
            enabled: true,
            first_tracked_cycle: "2026-10".into(),
            rules_changed_at: dt("2026-10-01T00:00:00+08:00"),
            created_at: dt("2026-10-01T00:00:00+08:00"),
            updated_at: dt("2026-10-01T00:00:00+08:00"),
        }
    }

    #[test]
    fn month_end_clamps_to_last_day() {
        assert_eq!(
            due_on_for_cycle("2026-02", 31).unwrap(),
            NaiveDate::from_ymd_opt(2026, 2, 28).unwrap()
        );
        assert_eq!(
            due_on_for_cycle("2028-02", 31).unwrap(),
            NaiveDate::from_ymd_opt(2028, 2, 29).unwrap()
        );
    }

    #[test]
    fn new_card_on_due_day_is_tracked_today() {
        assert_eq!(
            first_tracked_cycle(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(), 5),
            "2026-10"
        );
        assert_eq!(
            first_tracked_cycle(NaiveDate::from_ymd_opt(2026, 10, 6).unwrap(), 5),
            "2026-11"
        );
    }

    #[test]
    fn lead_and_due_day_generate_expected_keys() {
        let reminders = due_reminders(dt("2026-10-02T09:00:00+08:00"), &card(), None, |_| false);
        assert!(
            reminders
                .iter()
                .any(|item| item.occurrence_key == "card:2026-10:3:2026-10-05")
        );
        let due = due_reminders(dt("2026-10-05T09:00:00+08:00"), &card(), None, |_| false);
        assert!(
            due.iter()
                .any(|item| item.occurrence_key == "card:2026-10:0:2026-10-05")
        );
    }

    #[test]
    fn two_cards_with_the_same_schedule_get_distinct_keys() {
        // 同还款日、同提前档位、同日期：只有卡片 ID 不同。键里必须带 ID，
        // 否则调度的 dedup_by 会把它们当成同一条，静默少发一张卡的提醒。
        let first = card();
        let mut second = card();
        second.id = "card-2".into();
        second.alias = "建行 白金".into();
        let keys = |item: &CreditCard| {
            due_reminders(dt("2026-10-02T09:00:00+08:00"), item, None, |_| false)
                .into_iter()
                .map(|reminder| reminder.occurrence_key)
                .collect::<Vec<_>>()
        };
        let first_keys = keys(&first);
        let second_keys = keys(&second);
        assert!(!first_keys.is_empty());
        for key in &first_keys {
            assert!(!second_keys.contains(key), "两张卡不应共用提醒键：{key}");
        }
    }

    #[test]
    fn cross_month_overdue_is_found_on_first_day() {
        let mut card = card();
        card.first_tracked_cycle = "2026-02".into();
        card.due_day = 28;
        card.rules_changed_at = dt("2026-01-01T00:00:00+08:00");
        let reminders = due_reminders(
            dt("2026-03-01T09:00:00+08:00"),
            &card,
            Some("2026-01"),
            |_| false,
        );
        assert!(
            reminders
                .iter()
                .any(|item| item.occurrence_key == "card:2026-02:overdue:2026-02-28")
        );
    }

    #[test]
    fn cross_year_overdue_is_found_on_new_year() {
        let mut card = card();
        card.first_tracked_cycle = "2026-12".into();
        card.due_day = 31;
        let reminders = due_reminders(
            dt("2027-01-01T09:00:00+08:00"),
            &card,
            Some("2026-11"),
            |_| false,
        );
        assert!(
            reminders
                .iter()
                .any(|item| item.occurrence_key == "card:2026-12:overdue:2026-12-31")
        );
    }

    #[test]
    fn rules_changed_suppresses_overdue_and_lead() {
        let mut card = card();
        card.rules_changed_at = dt("2026-10-05T09:30:00+08:00");
        let reminders = due_reminders(dt("2026-10-05T09:31:00+08:00"), &card, None, |_| false);
        assert!(reminders.is_empty());
    }

    #[test]
    fn settled_through_blocks_only_settled_cycles() {
        let mut card = card();
        card.lead_days = vec![20];
        let reminders = due_reminders(
            dt("2026-10-16T09:00:00+08:00"),
            &card,
            Some("2026-10"),
            |_| false,
        );
        assert!(
            reminders
                .iter()
                .all(|item| !item.occurrence_key.starts_with("card:2026-10:"))
        );
    }

    #[test]
    fn new_card_shows_first_tracked_cycle_instead_of_a_past_due_date() {
        // 10/6 新增、每月 5 日还款：首期是 11 月，不能显示成已经过去的 10/5。
        let mut card = card();
        card.due_day = 5;
        card.first_tracked_cycle = "2026-11".into();
        let status = status_for_card(&card, None, day(2026, 10, 6));
        assert_eq!(status.next_due, day(2026, 11, 5));
        assert_eq!(status.label, "未开始");

        // 进入 11 月后就是正常倒数，不能还挂着「未开始」。
        let status = status_for_card(&card, None, day(2026, 11, 2));
        assert_eq!(status.next_due, day(2026, 11, 5));
        assert_ne!(status.label, "未开始");
        assert_eq!(status.days_left, 3);
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn undo_window_accepts_current_previous_and_prepaid_cycles() {
        let today = day(2026, 11, 2);
        assert!(undo_window_open(Some("2026-11"), today), "当前账期");
        assert!(undo_window_open(Some("2026-10"), today), "上一账期");
        assert!(undo_window_open(Some("2026-12"), today), "提前还下期");
    }

    #[test]
    fn undo_window_closes_for_older_cycles() {
        let today = day(2026, 11, 2);
        assert!(!undo_window_open(Some("2026-09"), today));
        assert!(!undo_window_open(Some("2025-11"), today));
        assert!(!undo_window_open(None, today));
    }

    #[test]
    fn undo_window_handles_year_boundary_and_corrupt_values() {
        let january = day(2026, 1, 3);
        assert!(undo_window_open(Some("2026-01"), january));
        assert!(undo_window_open(Some("2025-12"), january), "跨年上一账期");
        assert!(!undo_window_open(Some("2025-11"), january));
        // 存储值损坏时宁可关掉撤销，也不要把按钮留在界面上。
        assert!(!undo_window_open(Some("2026-13"), january));
        assert!(!undo_window_open(Some(""), january));
        assert!(!undo_window_open(Some("not-a-cycle"), january));
    }
}

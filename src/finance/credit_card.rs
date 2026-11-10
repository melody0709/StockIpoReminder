use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, NaiveTime};

use super::{FinanceKind, FinanceReminder, bank_definition};
use crate::{
    core::at,
    model::{ChinaDateTime, time},
};

const LOOKAHEAD_MONTHS: i32 = 3;
const MAX_LEAD_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PaymentAccountKind {
    CreditCard,
    Mortgage,
    AutoLoan,
    CreditLoan,
}

impl PaymentAccountKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CreditCard => "credit_card",
            Self::Mortgage => "mortgage",
            Self::AutoLoan => "auto_loan",
            Self::CreditLoan => "credit_loan",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CreditCard => "信用卡",
            Self::Mortgage => "房贷",
            Self::AutoLoan => "车贷",
            Self::CreditLoan => "信用贷",
        }
    }

    pub(crate) fn finance_kind(self) -> FinanceKind {
        match self {
            Self::CreditCard => FinanceKind::CreditCard,
            Self::Mortgage => FinanceKind::Mortgage,
            Self::AutoLoan => FinanceKind::AutoLoan,
            Self::CreditLoan => FinanceKind::CreditLoan,
        }
    }

    pub(crate) fn is_loan(self) -> bool {
        matches!(self, Self::Mortgage | Self::AutoLoan | Self::CreditLoan)
    }
}

impl std::str::FromStr for PaymentAccountKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "credit_card" | "信用卡" => Ok(Self::CreditCard),
            "mortgage" | "房贷" => Ok(Self::Mortgage),
            "auto_loan" | "车贷" => Ok(Self::AutoLoan),
            "credit_loan" | "信用贷" => Ok(Self::CreditLoan),
            _ => bail!("未知还款账户类型：{value}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PaymentAccount {
    pub id: String,
    pub kind: PaymentAccountKind,
    pub alias: String,
    pub bank_id: Option<String>,
    pub bank_name: Option<String>,
    pub due_day: u32,
    pub remind_time: NaiveTime,
    pub lead_days: Vec<u32>,
    pub overdue_enabled: bool,
    pub enabled: bool,
    pub first_tracked_cycle: String,
    pub final_cycle: Option<String>,
    pub closed_at: Option<ChinaDateTime>,
    pub rules_changed_at: ChinaDateTime,
    pub created_at: ChinaDateTime,
    pub updated_at: ChinaDateTime,
    pub amount: Option<f64>,
    pub total_amount: Option<f64>,
}

impl PaymentAccount {
    pub(crate) fn bank_display_name(&self) -> String {
        match self.bank_id.as_deref() {
            Some("other") => self
                .bank_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("其他银行")
                .to_owned(),
            Some(id) => bank_definition(id)
                .map(|bank| bank.display_name.to_owned())
                .or_else(|| {
                    self.bank_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "未知银行".to_owned()),
            None => "未填写银行".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountStatus {
    pub next_cycle: Option<String>,
    pub next_due: Option<NaiveDate>,
    pub days_left: i64,
    pub overdue_days: i64,
    pub label: String,
    pub tone: i32,
}

pub(crate) fn cycle_key(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

pub(crate) fn parse_cycle(cycle: &str) -> Option<(i32, u32)> {
    let bytes = cycle.as_bytes();
    if bytes.len() != 7
        || bytes[4] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_digit())
    {
        return None;
    }
    let year = cycle.get(..4)?.parse().ok()?;
    let month = cycle.get(5..)?.parse().ok()?;
    if !(1..=12).contains(&month) || NaiveDate::from_ymd_opt(year, month, 1).is_none() {
        return None;
    }
    Some((year, month))
}

pub(crate) fn shift_cycle(cycle: &str, months: i32) -> Result<String> {
    let (year, month) = parse_cycle(cycle).context("账期格式必须是 YYYY-MM")?;
    let total = year * 12 + month as i32 - 1 + months;
    let year = total.div_euclid(12);
    let month = total.rem_euclid(12) as u32 + 1;
    let shifted = format!("{year:04}-{month:02}");
    parse_cycle(&shifted).context("账期超出支持范围")?;
    Ok(shifted)
}

pub(crate) fn due_on_for_cycle(cycle: &str, due_day: u32) -> Result<NaiveDate> {
    let (year, month) = parse_cycle(cycle).context("账期格式必须是 YYYY-MM")?;
    let last_day = last_day_of_month(year, month);
    NaiveDate::from_ymd_opt(year, month, due_day.clamp(1, last_day)).context("生成还款日失败")
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

pub(crate) fn validate_payment_account(account: &PaymentAccount) -> Result<()> {
    if account.alias.trim().is_empty() {
        bail!("还款账户别名不能为空");
    }
    if account.alias.chars().count() > 30 {
        bail!("还款账户别名不能超过 30 个字");
    }
    if let Some(name) = account.bank_name.as_deref()
        && name.trim().chars().count() > 20
    {
        bail!("银行名称不能超过 20 个字");
    }
    if account.bank_id.as_deref() == Some("other")
        && account
            .bank_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
    {
        bail!("选择其他银行时必须填写银行名称");
    }
    if !(1..=31).contains(&account.due_day) {
        bail!("还款日必须是 1–31");
    }
    if account.remind_time < time(8, 0) || account.remind_time > time(22, 0) {
        bail!("提醒时间必须在 08:00–22:00 之间");
    }
    normalize_lead_days(account.lead_days.iter().copied())?;
    let first =
        parse_cycle(&account.first_tracked_cycle).context("首个跟踪账期格式必须是 YYYY-MM")?;
    if account.kind == PaymentAccountKind::CreditCard
        && (account.final_cycle.is_some() || account.closed_at.is_some())
    {
        bail!("信用卡不支持结束还款月或已结清状态");
    }
    if account.kind == PaymentAccountKind::CreditCard && account.total_amount.is_some() {
        bail!("信用卡不支持填写借款总额");
    }
    if let Some(amount) = account.amount {
        if !amount.is_finite() || amount < 0.0 || amount > 100_000_000.0 {
            bail!("每期还款金额必须在 0 到 100,000,000 之间");
        }
    }
    if let Some(total_amount) = account.total_amount {
        if !account.kind.is_loan() {
            bail!("信用卡不支持填写借款总额");
        }
        if !total_amount.is_finite() || total_amount < 0.0 || total_amount > 100_000_000.0 {
            bail!("借款总额必须在 0 到 100,000,000 之间");
        }
    }
    if let Some(final_cycle) = account.final_cycle.as_deref() {
        let final_cycle = parse_cycle(final_cycle).context("结束还款月格式必须是 YYYY-MM")?;
        if final_cycle < first {
            bail!("结束还款月不能早于首个跟踪账期");
        }
    }
    Ok(())
}

fn format_cny_internal(amount: f64, auto_compact: bool) -> String {
    let prefix = if amount < 0.0 { "-¥" } else { "¥" };
    let total_cents = (amount.abs() * 100.0).round() as u64;
    let int_part = total_cents / 100;
    let cent_part = total_cents % 100;
    let int_str = int_part.to_string();
    let mut with_commas = String::with_capacity(int_str.len() + int_str.len() / 3 + 4);
    let bytes = int_str.as_bytes();
    let len = bytes.len();
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            with_commas.push(',');
        }
        with_commas.push(b as char);
    }
    if auto_compact && cent_part == 0 {
        format!("{prefix}{with_commas}")
    } else {
        format!("{prefix}{with_commas}.{cent_part:02}")
    }
}

pub(crate) fn format_cny(amount: f64) -> String {
    format_cny_internal(amount, false)
}

pub(crate) fn format_cny_auto(amount: f64) -> String {
    format_cny_internal(amount, true)
}

pub(crate) fn validate_payment_account_history(
    account: &PaymentAccount,
    settled: Option<&str>,
) -> Result<()> {
    validate_payment_account(account)?;
    if let (Some(final_cycle), Some(settled)) = (account.final_cycle.as_deref(), settled) {
        let settled = parse_cycle(settled).context("已还账期格式必须是 YYYY-MM")?;
        let final_cycle = parse_cycle(final_cycle).context("结束还款月格式必须是 YYYY-MM")?;
        if final_cycle < settled {
            bail!("结束还款月不能早于最高已还账期");
        }
    }
    Ok(())
}

pub(crate) fn earliest_unsettled_cycle(
    account: &PaymentAccount,
    settled: Option<&str>,
    today: NaiveDate,
) -> Option<String> {
    let current = cycle_key(today);
    let mut cycle = account.first_tracked_cycle.clone();
    if let Some(settled) = settled
        && let Ok(next) = shift_cycle(settled, 1)
        && cycle_before(&cycle, &next)
    {
        cycle = next;
    }
    for _ in 0..=120 {
        if cycle_after(&cycle, &current)
            || account
                .final_cycle
                .as_deref()
                .is_some_and(|final_cycle| cycle_after(&cycle, final_cycle))
        {
            return None;
        }
        let due = due_on_for_cycle(&cycle, account.due_day).ok()?;
        if due <= today {
            return Some(cycle);
        }
        cycle = shift_cycle(&cycle, 1).ok()?;
    }
    None
}

/// 最高已还动作可属于上一、当前或未来账期；更旧的记录不可撤销。
pub(crate) fn undo_window_open(settled: Option<&str>, today: NaiveDate) -> bool {
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

pub(crate) fn cycle_at_least(left: &str, right: &str) -> bool {
    match (parse_cycle(left), parse_cycle(right)) {
        (Some(left), Some(right)) => left >= right,
        _ => false,
    }
}

pub(crate) fn cycle_before(left: &str, right: &str) -> bool {
    match (parse_cycle(left), parse_cycle(right)) {
        (Some(left), Some(right)) => left < right,
        _ => false,
    }
}

pub(crate) fn cycle_after(left: &str, right: &str) -> bool {
    match (parse_cycle(left), parse_cycle(right)) {
        (Some(left), Some(right)) => left > right,
        _ => false,
    }
}

pub(crate) fn status_for_account(
    account: &PaymentAccount,
    settled: Option<&str>,
    today: NaiveDate,
) -> AccountStatus {
    if account.closed_at.is_some() {
        return AccountStatus {
            next_cycle: None,
            next_due: None,
            days_left: 0,
            overdue_days: 0,
            label: "已结清".to_owned(),
            tone: 0,
        };
    }
    let overdue_cycle = earliest_unsettled_cycle(account, settled, today);
    let current = cycle_key(today);
    let base = if cycle_before(&current, &account.first_tracked_cycle) {
        account.first_tracked_cycle.clone()
    } else {
        current.clone()
    };
    let next_cycle = match overdue_cycle.clone() {
        Some(cycle) => Some(cycle),
        None => match settled {
            Some(settled) if cycle_at_least(settled, &base) => {
                Some(shift_cycle(settled, 1).unwrap_or(base))
            }
            _ => Some(base),
        },
    };
    let next_cycle = next_cycle.filter(|cycle| {
        account
            .final_cycle
            .as_deref()
            .is_none_or(|final_cycle| !cycle_after(cycle, final_cycle))
    });
    let next_due = next_cycle
        .as_deref()
        .and_then(|cycle| due_on_for_cycle(cycle, account.due_day).ok());
    let days_left = next_due.map(|due| (due - today).num_days()).unwrap_or(0);
    let overdue_days = overdue_cycle
        .as_deref()
        .and_then(|cycle| due_on_for_cycle(cycle, account.due_day).ok())
        .filter(|due| *due < today)
        .map(|due| (today - due).num_days())
        .unwrap_or(0);
    let (label, tone) = if !account.enabled {
        ("已暂停".to_owned(), 0)
    } else if next_cycle.is_none() && account.final_cycle.is_some() {
        ("还款周期已结束".to_owned(), 0)
    } else if cycle_before(&cycle_key(today), &account.first_tracked_cycle) {
        ("未开始".to_owned(), 0)
    } else if overdue_days > 0 {
        (format!("已逾期 {overdue_days} 天"), 3)
    } else if next_due.is_none() {
        ("暂无待还款账期".to_owned(), 0)
    } else if days_left == 0 {
        ("今天到期".to_owned(), 2)
    } else if days_left <= 7 {
        (format!("还有 {days_left} 天"), 1)
    } else {
        (format!("还有 {days_left} 天"), 0)
    };
    AccountStatus {
        next_cycle,
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

fn reminder_bank_name(account: &PaymentAccount) -> Option<String> {
    match account.bank_id.as_deref() {
        None => None,
        Some("other") => account
            .bank_name
            .as_ref()
            .map(|name| name.trim().to_owned()),
        Some(id) => bank_definition(id)
            .map(|bank| bank.display_name.to_owned())
            .or_else(|| {
                account
                    .bank_name
                    .as_ref()
                    .map(|name| name.trim().to_owned())
            }),
    }
}

fn occurrence_key(account: &PaymentAccount, cycle: &str, middle: &str, due: NaiveDate) -> String {
    if account.kind == PaymentAccountKind::CreditCard {
        format!("{}:{cycle}:{middle}:{due}", account.id)
    } else {
        format!(
            "{}:{}:{cycle}:{middle}:{due}",
            account.kind.as_str(),
            account.id
        )
    }
}

fn reminder(
    account: &PaymentAccount,
    cycle: &str,
    due: NaiveDate,
    lead: u32,
    now: ChinaDateTime,
) -> FinanceReminder {
    let remind_date = due - chrono::Duration::days(lead as i64);
    let due_at = at(remind_date, account.remind_time);
    let label = account.kind.label();
    let title = if lead == 0 {
        if due < now.date_naive() {
            format!("{label}还款提醒 · 已逾期")
        } else {
            format!("{label}还款提醒 · 今天到期")
        }
    } else if due < now.date_naive() {
        format!("{label}还款提醒 · 已逾期")
    } else {
        format!("{label}还款提醒 · {lead} 天后")
    };
    let rule_note = if account.kind == PaymentAccountKind::CreditCard {
        "本提醒按自然日计算，请以发卡行账单为准；本程序只保存本地提醒状态，不代表银行入账。"
    } else {
        "本提醒按自然日计算，请以贷款合同为准；本程序只保存本地提醒状态，不代表银行入账。"
    };
    let body = format!(
        "{}{}的还款日是 {}。\n{rule_note}",
        account.alias,
        reminder_bank_name(account)
            .map(|bank| format!("（{bank}）"))
            .unwrap_or_default(),
        due.format("%Y-%m-%d")
    );
    FinanceReminder {
        kind: account.kind.finance_kind(),
        source_id: account.id.clone(),
        occurrence_key: occurrence_key(account, cycle, &lead.to_string(), due),
        event_date: remind_date,
        due_at,
        valid_until: end_of_day(remind_date),
        title,
        body,
        primary_action_label: "本期已还".into(),
        primary_action_cycle: Some(cycle.to_owned()),
    }
}

fn overdue_reminder(account: &PaymentAccount, cycle: &str, due: NaiveDate) -> FinanceReminder {
    let date = due.succ_opt().unwrap_or(due);
    let label = account.kind.label();
    FinanceReminder {
        kind: account.kind.finance_kind(),
        source_id: account.id.clone(),
        occurrence_key: occurrence_key(account, cycle, "overdue", due),
        event_date: date,
        due_at: at(date, account.remind_time),
        valid_until: end_of_day(date),
        title: format!("{label}还款提醒 · 已逾期 1 天"),
        body: format!(
            "{}的还款日是 {}，目前仍未标记已还。\n若已还款，请打开还款提醒页标记本期；本程序不代表银行入账。",
            account.alias,
            due.format("%Y-%m-%d")
        ),
        primary_action_label: "本期已还".into(),
        primary_action_cycle: Some(cycle.to_owned()),
    }
}

pub(crate) fn due_reminders(
    now: ChinaDateTime,
    account: &PaymentAccount,
    settled: Option<&str>,
    is_shown: impl Fn(&str) -> bool,
) -> Vec<FinanceReminder> {
    if !account.enabled || account.closed_at.is_some() {
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
        if cycle_before(&cycle, &account.first_tracked_cycle)
            || account
                .final_cycle
                .as_deref()
                .is_some_and(|final_cycle| cycle_after(&cycle, final_cycle))
            || settled.is_some_and(|settled| cycle_at_least(settled, &cycle))
        {
            continue;
        }
        let Ok(due) = due_on_for_cycle(&cycle, account.due_day) else {
            continue;
        };
        for lead in &account.lead_days {
            let remind_date = due - chrono::Duration::days(*lead as i64);
            let due_at = at(remind_date, account.remind_time);
            if due_at < account.rules_changed_at {
                continue;
            }
            if due_at <= now && now < end_of_day(remind_date) {
                let item = reminder(account, &cycle, due, *lead, now);
                if !is_shown(&item.occurrence_key) {
                    reminders.push(item);
                }
            }
        }
        if account.overdue_enabled {
            let date = due.succ_opt().unwrap_or(due);
            let due_at = at(date, account.remind_time);
            if due_at >= account.rules_changed_at && due_at <= now && now < end_of_day(date) {
                let item = overdue_reminder(account, &cycle, due);
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
    account: &PaymentAccount,
    settled: Option<&str>,
) -> Option<ChinaDateTime> {
    if !account.enabled || account.closed_at.is_some() {
        return None;
    }
    let current = cycle_key(now.date_naive());
    let mut next = None;
    for offset in -1..=LOOKAHEAD_MONTHS {
        let cycle = shift_cycle(&current, offset).ok()?;
        if cycle_before(&cycle, &account.first_tracked_cycle)
            || account
                .final_cycle
                .as_deref()
                .is_some_and(|final_cycle| cycle_after(&cycle, final_cycle))
            || settled.is_some_and(|settled| cycle_at_least(settled, &cycle))
        {
            continue;
        }
        let due = due_on_for_cycle(&cycle, account.due_day).ok()?;
        for lead in &account.lead_days {
            let at_candidate = at(
                due - chrono::Duration::days(*lead as i64),
                account.remind_time,
            );
            if at_candidate >= account.rules_changed_at
                && at_candidate > now
                && next.is_none_or(|current| at_candidate < current)
            {
                next = Some(at_candidate);
            }
        }
        if account.overdue_enabled {
            let overdue_at = at(due.succ_opt().unwrap_or(due), account.remind_time);
            if overdue_at >= account.rules_changed_at
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

    fn card() -> PaymentAccount {
        PaymentAccount {
            id: "card".into(),
            kind: PaymentAccountKind::CreditCard,
            alias: "招行 Visa".into(),
            bank_id: Some("cmb".into()),
            bank_name: Some("招商银行".into()),
            due_day: 5,
            remind_time: time(9, 0),
            lead_days: vec![3, 0],
            overdue_enabled: true,
            enabled: true,
            first_tracked_cycle: "2026-10".into(),
            final_cycle: None,
            closed_at: None,
            rules_changed_at: dt("2026-10-01T00:00:00+08:00"),
            created_at: dt("2026-10-01T00:00:00+08:00"),
            updated_at: dt("2026-10-01T00:00:00+08:00"),
            amount: None,
            total_amount: None,
        }
    }

    fn mortgage() -> PaymentAccount {
        PaymentAccount {
            id: "loan".into(),
            kind: PaymentAccountKind::Mortgage,
            alias: "住房贷款".into(),
            final_cycle: Some("2026-11".into()),
            ..card()
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
    fn cycles_require_strict_yyyy_mm() {
        for valid in ["2026-01", "2026-12", "0000-01"] {
            assert!(parse_cycle(valid).is_some(), "{valid}");
        }
        for invalid in [
            "2026-1",
            "26-01",
            "2026-00",
            "2026-13",
            "2026-01-01",
            " 2026-01",
        ] {
            assert!(parse_cycle(invalid).is_none(), "{invalid}");
        }
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
    fn lead_and_due_day_generate_expected_legacy_card_keys() {
        let reminders = due_reminders(dt("2026-10-02T09:00:00+08:00"), &card(), None, |_| false);
        assert!(reminders.iter().any(|item| {
            item.occurrence_key == "card:2026-10:3:2026-10-05"
                && item.kind == FinanceKind::CreditCard
        }));
        let due = due_reminders(dt("2026-10-05T09:00:00+08:00"), &card(), None, |_| false);
        assert!(
            due.iter()
                .any(|item| item.occurrence_key == "card:2026-10:0:2026-10-05")
        );
    }

    #[test]
    fn loan_keys_are_namespaced_and_typed() {
        let reminders = due_reminders(dt("2026-10-02T09:00:00+08:00"), &mortgage(), None, |_| {
            false
        });
        assert!(reminders.iter().any(|item| {
            item.occurrence_key == "mortgage:loan:2026-10:3:2026-10-05"
                && item.kind == FinanceKind::Mortgage
                && item.title.starts_with("房贷还款提醒")
        }));
    }

    #[test]
    fn two_cards_with_the_same_schedule_get_distinct_keys() {
        let first = card();
        let mut second = card();
        second.id = "card-2".into();
        second.alias = "建行 白金".into();
        let keys = |item: &PaymentAccount| {
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
        let mut card = card();
        card.due_day = 5;
        card.first_tracked_cycle = "2026-11".into();
        let status = status_for_account(&card, None, day(2026, 10, 6));
        assert_eq!(status.next_cycle.as_deref(), Some("2026-11"));
        assert_eq!(status.next_due, Some(day(2026, 11, 5)));
        assert_eq!(status.label, "未开始");

        let status = status_for_account(&card, None, day(2026, 11, 2));
        assert_eq!(status.next_due, Some(day(2026, 11, 5)));
        assert_eq!(status.label, "还有 3 天");
        assert_eq!(status.days_left, 3);
    }

    #[test]
    fn final_cycle_keeps_unsettled_last_cycle_then_ends() {
        let loan = mortgage();
        let status = status_for_account(&loan, Some("2026-10"), day(2026, 12, 20));
        assert_eq!(status.next_cycle.as_deref(), Some("2026-11"));
        assert_eq!(status.next_due, Some(day(2026, 11, 5)));
        assert!(status.label.starts_with("已逾期"));

        let status = status_for_account(&loan, Some("2026-11"), day(2026, 12, 20));
        assert_eq!(status.next_cycle, None);
        assert_eq!(status.next_due, None);
        assert_eq!(status.label, "还款周期已结束");
    }

    #[test]
    fn final_cycle_limits_reminders_and_future_wakeups() {
        let loan = mortgage();
        let final_month = due_reminders(dt("2026-11-02T09:00:00+08:00"), &loan, None, |_| false);
        assert!(
            final_month
                .iter()
                .any(|item| item.primary_action_cycle.as_deref() == Some("2026-11"))
        );
        assert!(
            final_month
                .iter()
                .all(|item| item.primary_action_cycle.as_deref() != Some("2026-12"))
        );
        assert_eq!(
            next_future(dt("2026-11-06T09:00:00+08:00"), &loan, Some("2026-11")),
            None
        );
    }

    #[test]
    fn closed_has_priority_and_paused_is_distinct() {
        let mut loan = mortgage();
        loan.enabled = false;
        let paused = status_for_account(&loan, None, day(2026, 10, 20));
        assert_eq!(paused.label, "已暂停");
        assert!(paused.next_due.is_some());

        loan.closed_at = Some(dt("2026-10-20T10:00:00+08:00"));
        let closed = status_for_account(&loan, None, day(2026, 10, 20));
        assert_eq!(closed.label, "已结清");
        assert_eq!(closed.next_cycle, None);
        assert_eq!(closed.next_due, None);
        assert!(due_reminders(dt("2026-10-20T10:00:00+08:00"), &loan, None, |_| false).is_empty());
        assert_eq!(
            next_future(dt("2026-10-20T10:00:00+08:00"), &loan, None),
            None
        );
    }

    #[test]
    fn account_validation_enforces_type_and_final_cycle_rules() {
        let mut card = card();
        card.final_cycle = Some("2026-12".into());
        assert!(validate_payment_account(&card).is_err());
        card.final_cycle = None;
        card.closed_at = Some(dt("2026-10-20T10:00:00+08:00"));
        assert!(validate_payment_account(&card).is_err());

        let mut loan = mortgage();
        loan.final_cycle = Some("2026-09".into());
        assert!(validate_payment_account(&loan).is_err());
        loan.final_cycle = Some("2026-11".into());
        assert!(validate_payment_account_history(&loan, Some("2026-12")).is_err());
        assert!(validate_payment_account_history(&loan, Some("2026-11")).is_ok());
    }

    #[test]
    fn other_bank_requires_a_bounded_custom_name() {
        let mut account = card();
        account.bank_id = Some("other".into());
        account.bank_name = Some("  ".into());
        assert!(validate_payment_account(&account).is_err());
        account.bank_name = Some("地方银行".into());
        assert!(validate_payment_account(&account).is_ok());
        account.bank_name = Some("很长的银行名称".repeat(5));
        assert!(validate_payment_account(&account).is_err());
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
        assert!(!undo_window_open(Some("2026-13"), january));
        assert!(!undo_window_open(Some(""), january));
        assert!(!undo_window_open(Some("not-a-cycle"), january));
    }

    #[test]
    fn credit_loan_kind_properties_and_parsing() {
        use std::str::FromStr;
        let kind = PaymentAccountKind::CreditLoan;
        assert_eq!(kind.as_str(), "credit_loan");
        assert_eq!(kind.label(), "信用贷");
        assert!(kind.is_loan());
        assert_eq!(
            PaymentAccountKind::from_str("credit_loan").unwrap(),
            PaymentAccountKind::CreditLoan
        );
        assert_eq!(
            PaymentAccountKind::from_str("信用贷").unwrap(),
            PaymentAccountKind::CreditLoan
        );
    }

    #[test]
    fn payment_account_amount_validation() {
        let mut loan = PaymentAccount {
            id: "loan-1".into(),
            kind: PaymentAccountKind::CreditLoan,
            alias: "某消金信用贷".into(),
            final_cycle: Some("2028-10".into()),
            amount: Some(3500.0),
            total_amount: Some(100000.0),
            ..card()
        };
        assert!(validate_payment_account(&loan).is_ok());

        // 负数金额
        loan.amount = Some(-1.0);
        assert!(validate_payment_account(&loan).is_err());
        loan.amount = Some(3500.0);

        // 负数借款总额
        loan.total_amount = Some(-100.0);
        assert!(validate_payment_account(&loan).is_err());
        loan.total_amount = Some(100000.0);

        // 超过1亿上限
        loan.amount = Some(100_000_001.0);
        assert!(validate_payment_account(&loan).is_err());
        loan.amount = Some(3500.0);

        loan.total_amount = Some(100_000_001.0);
        assert!(validate_payment_account(&loan).is_err());
        loan.total_amount = Some(100000.0);

        // NaN / Inf
        loan.amount = Some(f64::NAN);
        assert!(validate_payment_account(&loan).is_err());
        loan.amount = Some(f64::INFINITY);
        assert!(validate_payment_account(&loan).is_err());
        loan.amount = Some(3500.0);

        // 信用卡不允许设置借款总额
        let mut card = card();
        card.amount = Some(5000.0);
        card.total_amount = None;
        assert!(validate_payment_account(&card).is_ok());
        card.total_amount = Some(50000.0);
        assert!(validate_payment_account(&card).is_err());
    }

    #[test]
    fn cny_formatting_helpers() {
        assert_eq!(format_cny(0.0), "¥0.00");
        assert_eq!(format_cny(5800.0), "¥5,800.00");
        assert_eq!(format_cny(1234567.89), "¥1,234,567.89");
        assert_eq!(format_cny(12500.5), "¥12,500.50");
        assert_eq!(format_cny(-5800.0), "-¥5,800.00");

        assert_eq!(format_cny_auto(0.0), "¥0");
        assert_eq!(format_cny_auto(5800.0), "¥5,800");
        assert_eq!(format_cny_auto(12500.5), "¥12,500.50");
        assert_eq!(format_cny_auto(1234567.89), "¥1,234,567.89");
        assert_eq!(format_cny_auto(-12500.0), "-¥12,500");
    }
}

use chrono::{NaiveDate, NaiveTime};

use super::{FinanceKind, FinanceReminder, TradingCalendar};
use crate::{
    core::at,
    model::{ChinaDateTime, time},
};

const LOOKAHEAD_DAYS: i64 = 45;
/// 逆回购提醒的 `source_id`：与已展示台账、内存 in-flight 的键保持一致。
pub(crate) const SOURCE_ID: &str = "repo";
const REPO_LAST_SLOT: NaiveTime = NaiveTime::from_hms_opt(15, 25, 0).unwrap();
pub(crate) const REPO_PRIMARY_DEFAULT: NaiveTime = NaiveTime::from_hms_opt(14, 50, 0).unwrap();
pub(crate) const REPO_FALLBACK_DEFAULT: NaiveTime = NaiveTime::from_hms_opt(15, 20, 0).unwrap();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoSettings {
    pub enabled: bool,
    pub primary: Option<NaiveTime>,
    pub fallback: Option<NaiveTime>,
    pub rules_changed_at: ChinaDateTime,
    pub updated_at: ChinaDateTime,
}

impl RepoSettings {
    pub(crate) fn default_at(now: ChinaDateTime) -> Self {
        Self {
            enabled: false,
            primary: Some(REPO_PRIMARY_DEFAULT),
            fallback: None,
            rules_changed_at: now,
            updated_at: now,
        }
    }

    pub(crate) fn slots(&self) -> Vec<NaiveTime> {
        let mut slots = Vec::with_capacity(2);
        if let Some(primary) = self.primary {
            slots.push(primary);
        }
        if let Some(fallback) = self.fallback
            && !slots.contains(&fallback)
        {
            slots.push(fallback);
        }
        slots.sort_unstable();
        slots
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        let slots = self.slots();
        if slots.len() != usize::from(self.primary.is_some()) + usize::from(self.fallback.is_some())
        {
            anyhow::bail!("主提醒和兜底提醒不能使用同一时间");
        }
        for slot in slots {
            if slot < time(13, 0) || slot > REPO_LAST_SLOT {
                anyhow::bail!("逆回购提醒时间必须在 13:00–15:25 之间");
            }
        }
        if self.enabled && self.primary.is_none() && self.fallback.is_none() {
            anyhow::bail!("启用逆回购提醒时至少保留一个提醒时点");
        }
        Ok(())
    }
}

pub(crate) fn valid_until_for(date: NaiveDate, slot: NaiveTime) -> ChinaDateTime {
    let candidate = at(date, slot) + chrono::Duration::minutes(10);
    candidate.min(at(date, time(15, 30)))
}

fn reminder_for(
    date: NaiveDate,
    slot: NaiveTime,
    now: ChinaDateTime,
    settings: &RepoSettings,
) -> FinanceReminder {
    let due_at = at(date, slot);
    let valid_until = valid_until_for(date, slot);
    let remaining = (at(date, time(15, 30)) - now).num_minutes().max(0);
    let is_final = settings.fallback.is_some_and(|fallback| fallback == slot);
    let title = if is_final {
        format!("逆回购提醒 · 最后 {remaining} 分钟")
    } else {
        format!("逆回购提醒 · {}", slot.format("%H:%M"))
    };
    let body = if is_final {
        format!(
            "逆回购可交易至 15:30，距离收盘约 {remaining} 分钟。\n若账户有闲置资金，可在券商客户端查看 GC001 / R-001。\n逆回购操作方向为卖出（出借资金），请以券商客户端为准。"
        )
    } else {
        format!(
            "今天是交易日。若账户有闲置资金，可在券商客户端查看 GC001 / R-001。\n逆回购可交易至 15:30；利率随资金面波动，本提醒不保证收益。"
        )
    };
    FinanceReminder {
        kind: FinanceKind::ReverseRepo,
        source_id: SOURCE_ID.into(),
        occurrence_key: format!("{}:{}", date, slot.format("%H:%M")),
        event_date: date,
        due_at,
        valid_until,
        title,
        body,
        primary_action_label: "今日已处理".into(),
        primary_action_cycle: None,
    }
}

pub(crate) fn due_reminder(
    now: ChinaDateTime,
    settings: &RepoSettings,
    calendar: &TradingCalendar,
    is_acked: impl Fn(NaiveDate) -> bool,
    is_shown: impl Fn(&str) -> bool,
) -> Option<FinanceReminder> {
    if !settings.enabled {
        return None;
    }
    let date = now.date_naive();
    if calendar.is_trading_day(date) != Some(true) || is_acked(date) {
        return None;
    }
    for slot in settings.slots() {
        let slot_at = at(date, slot);
        if slot_at < settings.rules_changed_at {
            continue;
        }
        let valid_until = valid_until_for(date, slot);
        if slot_at <= now && now < valid_until {
            let reminder = reminder_for(date, slot, now, settings);
            if !is_shown(&reminder.occurrence_key) {
                return Some(reminder);
            }
        }
    }
    None
}

pub(crate) fn next_future_slot(
    now: ChinaDateTime,
    settings: &RepoSettings,
    calendar: &TradingCalendar,
    is_acked: impl Fn(NaiveDate) -> bool,
) -> Option<ChinaDateTime> {
    if !settings.enabled {
        return None;
    }
    let today = now.date_naive();
    for offset in 0..=LOOKAHEAD_DAYS {
        let date = today + chrono::Duration::days(offset);
        if calendar.is_trading_day(date) != Some(true) || is_acked(date) {
            continue;
        }
        for slot in settings.slots() {
            let slot_at = at(date, slot);
            if slot_at >= settings.rules_changed_at && slot_at > now {
                return Some(slot_at);
            }
        }
    }
    None
}

pub(crate) fn status_text(
    now: ChinaDateTime,
    settings: &RepoSettings,
    calendar: &TradingCalendar,
    is_acked: impl Fn(NaiveDate) -> bool,
) -> String {
    if !settings.enabled {
        return "逆回购提醒已关闭".into();
    }
    let today = now.date_naive();
    if calendar.is_trading_day(today).is_none() {
        return "交易日历未覆盖今天 · 暂不提醒".into();
    }
    if calendar.is_trading_day(today) == Some(false) {
        return match next_future_slot(now, settings, calendar, is_acked) {
            Some(next) => format!("今天休市 · 下次提醒 {}", next.format("%m-%d %H:%M")),
            None => "今天休市 · 近期无提醒".into(),
        };
    }
    if is_acked(today) {
        return "今日已处理 · 不再补提醒".into();
    }
    for slot in settings.slots() {
        let slot_at = at(today, slot);
        if slot_at < settings.rules_changed_at {
            continue;
        }
        let valid_until = valid_until_for(today, slot);
        if slot_at <= now && now < valid_until {
            return format!("今天交易日 · {} 提醒待处理", slot.format("%H:%M"));
        }
    }
    match next_future_slot(now, settings, calendar, is_acked) {
        Some(next) if next.date_naive() == today => {
            let has_primary = settings.primary.is_some();
            let has_fallback = settings.fallback.is_some();
            if has_primary && has_fallback {
                let primary = settings.primary.unwrap();
                if now.time() > primary {
                    format!(
                        "今日主提醒已过 · {} 还有一次兜底",
                        settings.fallback.unwrap().format("%H:%M")
                    )
                } else {
                    format!("今天交易日 · 下次提醒 {}", next.format("%H:%M"))
                }
            } else {
                format!("今天交易日 · 下次提醒 {}", next.format("%H:%M"))
            }
        }
        Some(next) => format!("下次提醒 {}", next.format("%m-%d %H:%M")),
        None => "今日提醒已结束 · 近期无提醒".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(value: &str) -> ChinaDateTime {
        value.parse().unwrap()
    }

    fn enabled() -> RepoSettings {
        RepoSettings {
            enabled: true,
            primary: Some(REPO_PRIMARY_DEFAULT),
            fallback: Some(REPO_FALLBACK_DEFAULT),
            rules_changed_at: dt("2026-10-01T00:00:00+08:00"),
            updated_at: dt("2026-10-01T00:00:00+08:00"),
        }
    }

    #[test]
    fn valid_until_is_ten_minutes_but_never_after_close() {
        assert_eq!(
            valid_until_for(
                NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                REPO_PRIMARY_DEFAULT
            ),
            dt("2026-10-09T15:00:00+08:00")
        );
        assert_eq!(
            valid_until_for(
                NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                REPO_FALLBACK_DEFAULT
            ),
            dt("2026-10-09T15:30:00+08:00")
        );
        assert_eq!(
            valid_until_for(
                NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                REPO_LAST_SLOT
            ),
            dt("2026-10-09T15:30:00+08:00")
        );
    }

    #[test]
    fn holiday_does_not_produce_reminder() {
        let calendar = TradingCalendar::builtin();
        let reminder = due_reminder(
            dt("2026-10-01T14:50:00+08:00"),
            &enabled(),
            calendar,
            |_| false,
            |_| false,
        );
        assert!(reminder.is_none());
    }

    #[test]
    fn primary_and_fallback_use_ten_minute_windows() {
        let calendar = TradingCalendar::builtin();
        let primary = due_reminder(
            dt("2026-10-09T14:55:00+08:00"),
            &enabled(),
            calendar,
            |_| false,
            |_| false,
        )
        .unwrap();
        assert_eq!(primary.occurrence_key, "2026-10-09:14:50");

        let fallback = due_reminder(
            dt("2026-10-09T15:25:00+08:00"),
            &enabled(),
            calendar,
            |_| false,
            |key| key == "2026-10-09:14:50",
        )
        .unwrap();
        assert_eq!(fallback.occurrence_key, "2026-10-09:15:20");
        assert!(fallback.title.contains("最后 5 分钟"));
    }

    #[test]
    fn ack_and_rules_change_suppress_old_slots() {
        let calendar = TradingCalendar::builtin();
        let acked = due_reminder(
            dt("2026-10-09T14:50:00+08:00"),
            &enabled(),
            calendar,
            |date| date == NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
            |_| false,
        );
        assert!(acked.is_none());

        let mut settings = enabled();
        settings.rules_changed_at = dt("2026-10-09T14:55:00+08:00");
        assert!(
            due_reminder(
                dt("2026-10-09T14:56:00+08:00"),
                &settings,
                calendar,
                |_| false,
                |_| false
            )
            .is_none()
        );
    }

    #[test]
    fn next_slot_skips_weekend_and_holidays() {
        let calendar = TradingCalendar::builtin();
        let next = next_future_slot(
            dt("2026-10-08T15:30:00+08:00"),
            &enabled(),
            calendar,
            |_| false,
        )
        .unwrap();
        assert_eq!(next, dt("2026-10-09T14:50:00+08:00"));
    }
}

use std::{collections::BTreeSet, sync::OnceLock};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, Weekday};
use serde::Deserialize;

const CALENDAR_JSON: &str = include_str!("../../assets/trading-calendar/cn-exchange.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarFile {
    source: String,
    published_on: NaiveDate,
    coverage_start: NaiveDate,
    coverage_end: NaiveDate,
    closed_days: Vec<NaiveDate>,
}

#[derive(Debug)]
pub(crate) struct TradingCalendar {
    source: String,
    published_on: NaiveDate,
    coverage_start: NaiveDate,
    coverage_end: NaiveDate,
    closed_days: BTreeSet<NaiveDate>,
}

#[allow(dead_code)]
impl TradingCalendar {
    pub(crate) fn builtin() -> &'static Self {
        static CALENDAR: OnceLock<TradingCalendar> = OnceLock::new();
        CALENDAR.get_or_init(|| Self::parse(CALENDAR_JSON).expect("内置交易日历必须通过校验"))
    }

    fn parse(raw: &str) -> Result<Self> {
        let file: CalendarFile = serde_json::from_str(raw).context("解析内置交易日历失败")?;
        if file.coverage_start > file.coverage_end {
            bail!("交易日历覆盖区间倒置");
        }
        if file
            .closed_days
            .iter()
            .any(|date| date < &file.coverage_start || date > &file.coverage_end)
        {
            bail!("交易日历包含覆盖区间外的休市日");
        }
        if file
            .closed_days
            .iter()
            .any(|date| matches!(date.weekday(), Weekday::Sat | Weekday::Sun))
        {
            bail!("交易日历不应重复列出周末休市日");
        }
        let mut closed_days = BTreeSet::new();
        for date in file.closed_days {
            if !closed_days.insert(date) {
                bail!("交易日历包含重复日期：{date}");
            }
        }
        Ok(Self {
            source: file.source,
            published_on: file.published_on,
            coverage_start: file.coverage_start,
            coverage_end: file.coverage_end,
            closed_days,
        })
    }

    pub(crate) fn is_trading_day(&self, date: NaiveDate) -> Option<bool> {
        if date < self.coverage_start || date > self.coverage_end {
            return None;
        }
        Some(
            !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
                && !self.closed_days.contains(&date),
        )
    }

    pub(crate) fn next_trading_day(&self, date: NaiveDate) -> Option<NaiveDate> {
        let mut cursor = date;
        for _ in 0..=370 {
            if self.is_trading_day(cursor)? {
                return Some(cursor);
            }
            cursor = cursor.succ_opt()?;
        }
        None
    }

    pub(crate) fn coverage_start(&self) -> NaiveDate {
        self.coverage_start
    }

    pub(crate) fn coverage_end(&self) -> NaiveDate {
        self.coverage_end
    }

    pub(crate) fn days_until_coverage_end(&self, today: NaiveDate) -> i64 {
        (self.coverage_end - today).num_days()
    }

    pub(crate) fn coverage_text(&self) -> String {
        format!(
            "{} 至 {}（公告日期 {}）",
            self.coverage_start, self.coverage_end, self.published_on
        )
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    #[test]
    fn builtin_calendar_is_valid_and_covers_2026() {
        let calendar = TradingCalendar::builtin();
        assert_eq!(calendar.coverage_start(), date("2026-01-01"));
        assert_eq!(calendar.coverage_end(), date("2026-12-31"));
        assert!(calendar.source().contains("证券交易所"));
    }

    #[test]
    fn trading_day_requires_exchange_calendar_not_weekday() {
        let calendar = TradingCalendar::builtin();
        assert_eq!(calendar.is_trading_day(date("2026-01-02")), Some(false));
        assert_eq!(calendar.is_trading_day(date("2026-02-23")), Some(false));
        assert_eq!(calendar.is_trading_day(date("2026-10-01")), Some(false));
        assert_eq!(calendar.is_trading_day(date("2026-12-31")), Some(true));
        assert_eq!(calendar.is_trading_day(date("2026-01-03")), Some(false));
    }

    #[test]
    fn dates_outside_coverage_are_unknown() {
        let calendar = TradingCalendar::builtin();
        assert_eq!(calendar.is_trading_day(date("2025-12-31")), None);
        assert_eq!(calendar.is_trading_day(date("2027-01-04")), None);
        assert_eq!(calendar.next_trading_day(date("2027-01-04")), None);
    }

    #[test]
    fn next_trading_day_skips_holiday_and_weekend() {
        let calendar = TradingCalendar::builtin();
        assert_eq!(
            calendar.next_trading_day(date("2026-10-01")),
            Some(date("2026-10-08"))
        );
        assert_eq!(
            calendar.next_trading_day(date("2026-02-23")),
            Some(date("2026-02-24"))
        );
    }
}

use std::collections::{BTreeMap, HashMap};

use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use sha2::{Digest, Sha256};

use crate::model::*;

pub fn china_offset() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(8 * 3600).unwrap()
}
pub fn now_china() -> ChinaDateTime {
    Utc::now().with_timezone(&china_offset())
}
pub fn at(date: NaiveDate, time: NaiveTime) -> ChinaDateTime {
    china_offset()
        .from_local_datetime(&date.and_time(time))
        .single()
        .unwrap()
}
pub fn sha256(value: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(value.as_ref()))
}
pub fn parse_date(value: &str) -> Option<NaiveDate> {
    let normalized = value
        .trim()
        .replace('年', "-")
        .replace('月', "-")
        .replace('日', "")
        .replace('/', "-");
    ["%Y-%m-%d", "%Y-%m-%d %H:%M:%S", "%Y%m%d"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(&normalized, format).ok())
}

pub fn status_from_dates(
    apply_date: Option<NaiveDate>,
    today: NaiveDate,
    suspended: bool,
    terminated: bool,
) -> IssueStatus {
    if terminated {
        IssueStatus::Terminated
    } else if suspended {
        IssueStatus::Suspended
    } else {
        match apply_date {
            None => IssueStatus::Unknown,
            Some(date) if date > today => IssueStatus::Upcoming,
            Some(date) if date == today => IssueStatus::Active,
            Some(_) => IssueStatus::Completed,
        }
    }
}

pub fn detect_exchange(code: Option<&str>, market: Option<&str>, beijing: bool) -> Exchange {
    let market = market.unwrap_or_default();
    if beijing || market.contains("北交") || market.contains("北京") {
        Exchange::Beijing
    } else if market.contains('沪')
        || market.contains("上海")
        || market.contains("科创")
        || code.is_some_and(|c| c.starts_with('6'))
    {
        Exchange::Shanghai
    } else if code.is_none() {
        Exchange::Unknown
    } else if code.is_some_and(|c| c.starts_with('8') || c.starts_with('9')) {
        Exchange::Beijing
    } else {
        Exchange::Shenzhen
    }
}

pub fn detect_board(exchange: Exchange, code: Option<&str>, market: Option<&str>) -> Board {
    if exchange == Exchange::Beijing {
        Board::Beijing
    } else if market.unwrap_or_default().contains("科创")
        || code.is_some_and(|c| c.starts_with("688"))
    {
        Board::Star
    } else if market.unwrap_or_default().contains("创业")
        || code.is_some_and(|c| c.starts_with("30"))
    {
        Board::ChiNext
    } else if exchange == Exchange::Unknown {
        Board::Unknown
    } else {
        Board::Main
    }
}

pub fn default_sessions(exchange: Exchange, settings: &AppSettings) -> Vec<SubscriptionSession> {
    let (start, broker, funding, sensitive) = match exchange {
        Exchange::Shanghai => (
            time(9, 30),
            settings.shanghai_broker_accept_start,
            FundingMode::MarketValue,
            false,
        ),
        Exchange::Shenzhen => (
            time(9, 15),
            settings.shenzhen_broker_accept_start,
            FundingMode::MarketValue,
            false,
        ),
        Exchange::Beijing => (
            time(9, 15),
            settings.beijing_broker_accept_start,
            FundingMode::FullCash,
            true,
        ),
        _ => (time(9, 30), time(9, 30), FundingMode::MarketValue, false),
    };
    vec![
        SubscriptionSession {
            session_number: 1,
            official_start: start,
            official_end: time(11, 30),
            broker_accept_start: Some(broker),
            safety_cutoff: None,
            funding_mode: funding,
            allocation_time_sensitive: sensitive,
            source: "default".into(),
            source_published_at: None,
        },
        SubscriptionSession {
            session_number: 2,
            official_start: time(13, 0),
            official_end: time(15, 0),
            broker_accept_start: None,
            safety_cutoff: Some(settings.safety_cutoff.min(time(15, 0))),
            funding_mode: funding,
            allocation_time_sensitive: sensitive,
            source: "default".into(),
            source_published_at: None,
        },
    ]
}

pub fn effective_cutoff(event: &IpoEvent, settings: &AppSettings) -> NaiveTime {
    let sessions = if event.sessions.is_empty() {
        default_sessions(event.exchange, settings)
    } else {
        event.sessions.clone()
    };
    settings.safety_cutoff.min(
        sessions
            .last()
            .map(|s| s.official_end)
            .unwrap_or(time(15, 0)),
    )
}

pub fn plan_reminders(event: &IpoEvent, settings: &AppSettings) -> Vec<ReminderItem> {
    let Some(date) = event.apply_date else {
        return vec![];
    };
    if event.is_terminal()
        || event.lifecycle_status == LifecycleStatus::Acknowledged
        || !settings.exchange_enabled(event.exchange)
    {
        return vec![];
    }
    let mut sessions = if event.sessions.is_empty() {
        default_sessions(event.exchange, settings)
    } else {
        event.sessions.clone()
    };
    sessions.sort_by_key(|s| s.session_number);
    let Some(first) = sessions.first() else {
        return vec![];
    };
    let cutoff = effective_cutoff(event, settings);
    let mut due = BTreeMap::<ChinaDateTime, ReminderLevel>::new();
    let mut add = |when, level| {
        if due
            .get(&when)
            .is_none_or(|current| (*current as i32) < level as i32)
        {
            due.insert(when, level);
        }
    };
    add(
        at(date - Duration::days(1), time(20, 0)),
        ReminderLevel::Advance,
    );
    add(at(date, time(8, 30)), ReminderLevel::Morning);
    let broker = match event.exchange {
        Exchange::Shanghai => settings.shanghai_broker_accept_start,
        Exchange::Shenzhen => settings.shenzhen_broker_accept_start,
        Exchange::Beijing => settings.beijing_broker_accept_start,
        _ => first.broker_accept_start.unwrap_or(first.official_start),
    };
    if (event.exchange != Exchange::Beijing || settings.beijing_reservation_supported)
        && broker < first.official_start
    {
        add(at(date, broker), ReminderLevel::BrokerOpening);
    }
    add(
        at(date, first.official_start - Duration::minutes(5)),
        ReminderLevel::MarketOpening,
    );
    for session in &sessions {
        let mut cursor = session.official_start;
        while cursor < session.official_end {
            if cursor < cutoff {
                add(at(date, cursor), ReminderLevel::Hourly);
            }
            cursor += Duration::hours(1);
        }
    }
    if time(11, 20) < cutoff {
        add(at(date, time(11, 20)), ReminderLevel::NoonBoundary);
    }
    if time(12, 55) < cutoff {
        add(at(date, time(12, 55)), ReminderLevel::AfternoonOpening);
    }
    add_range(
        &mut add,
        date,
        cutoff - Duration::minutes(60),
        cutoff - Duration::minutes(30),
        15,
        ReminderLevel::FifteenMinutes,
    );
    add_range(
        &mut add,
        date,
        cutoff - Duration::minutes(30),
        cutoff - Duration::minutes(10),
        5,
        ReminderLevel::FiveMinutes,
    );
    add_range(
        &mut add,
        date,
        cutoff - Duration::minutes(10),
        cutoff,
        2,
        ReminderLevel::TwoMinutes,
    );
    add(at(date, cutoff), ReminderLevel::Final);
    due.into_iter()
        .map(|(when, level)| ReminderItem {
            event_id: event.id.clone(),
            event_version: event.event_version,
            due_at: when,
            level,
            dedupe_key: format!(
                "{}:{}:{}:{}",
                event.id,
                event.event_version,
                when.timestamp_micros() * 10,
                level as i32
            ),
        })
        .collect()
}

fn add_range(
    add: &mut impl FnMut(ChinaDateTime, ReminderLevel),
    date: NaiveDate,
    mut cursor: NaiveTime,
    end: NaiveTime,
    minutes: i64,
    level: ReminderLevel,
) {
    while cursor < end {
        add(at(date, cursor), level);
        cursor += Duration::minutes(minutes);
    }
}

pub fn reconcile_candidates(
    candidates: &[Candidate],
    existing: Option<&IpoEvent>,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> Option<IpoEvent> {
    let mut usable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.stable_identity().is_some())
        .collect();
    usable.sort_by_key(|candidate| (-candidate.priority, std::cmp::Reverse(candidate.fetched_at)));
    let first = *usable.first()?;
    let security_code = pick_text(&usable, |c| c.security_code.as_ref())
        .or_else(|| existing.map(|e| e.security_code.clone()))?;
    let id = format!("{}:{security_code}", exchange_name(first.exchange));
    let name = pick_text(&usable, |c| c.name.as_ref())
        .or_else(|| existing.map(|e| e.name.clone()))
        .unwrap_or_else(|| security_code.clone());
    let apply_code = pick_text(&usable, |c| c.apply_code.as_ref())
        .or_else(|| existing.and_then(|e| e.apply_code.clone()));
    let apply_date =
        pick(&usable, |c| c.apply_date).or_else(|| existing.and_then(|e| e.apply_date));
    let issue_price =
        pick(&usable, |c| c.issue_price).or_else(|| existing.and_then(|e| e.issue_price));
    let status = usable
        .iter()
        .find_map(|c| (c.status != IssueStatus::Unknown).then_some(c.status))
        .or_else(|| existing.map(|e| e.status))
        .unwrap_or(IssueStatus::Unknown);
    let conflict = conflicts(&usable, |c| c.apply_code.clone())
        || conflicts(&usable, |c| c.apply_date.map(|d| d.to_string()))
        || conflicts(&usable, |c| c.issue_price.map(|v| v.to_string()))
        || conflicts(&usable, |c| {
            (c.status != IssueStatus::Unknown).then(|| format!("{:?}", c.status))
        });
    let announcement_verified = usable.iter().any(|c| c.announcement_derived);
    let quality = if conflict {
        DataQualityStatus::DataConflict
    } else if announcement_verified {
        DataQualityStatus::AnnouncementVerified
    } else if usable.len() > 1 {
        DataQualityStatus::MultiSourceVerified
    } else {
        DataQualityStatus::SingleSource
    };
    let sessions = usable
        .iter()
        .find(|c| !c.sessions.is_empty())
        .map(|c| c.sessions.clone())
        .or_else(|| existing.map(|e| e.sessions.clone()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_sessions(first.exchange, settings));
    let lifecycle = if matches!(status, IssueStatus::Suspended | IssueStatus::Terminated) {
        LifecycleStatus::SuspendedOrCancelled
    } else if apply_date.is_some_and(|d| d > now.date_naive()) {
        LifecycleStatus::Scheduled
    } else if apply_date == Some(now.date_naive()) {
        existing
            .map(|e| e.lifecycle_status)
            .filter(|s| {
                matches!(
                    s,
                    LifecycleStatus::Acknowledged | LifecycleStatus::AcknowledgedNeedsReview
                )
            })
            .unwrap_or(LifecycleStatus::ActiveUnconfirmed)
    } else {
        LifecycleStatus::Discovered
    };
    Some(IpoEvent {
        id,
        exchange: first.exchange,
        board: usable
            .iter()
            .find_map(|c| (c.board != Board::Unknown).then_some(c.board))
            .unwrap_or(Board::Unknown),
        security_code,
        apply_code,
        legacy_code: pick_text(&usable, |c| c.legacy_code.as_ref())
            .or_else(|| existing.and_then(|e| e.legacy_code.clone())),
        name,
        apply_date,
        issue_price,
        lot_size: pick(&usable, |c| c.lot_size).or_else(|| existing.and_then(|e| e.lot_size)),
        max_apply_quantity: pick(&usable, |c| c.max_apply_quantity)
            .or_else(|| existing.and_then(|e| e.max_apply_quantity)),
        required_market_value: pick(&usable, |c| c.required_market_value)
            .or_else(|| existing.and_then(|e| e.required_market_value)),
        required_cash: pick(&usable, |c| c.required_cash)
            .or_else(|| existing.and_then(|e| e.required_cash)),
        ballot_date: pick(&usable, |c| c.ballot_date)
            .or_else(|| existing.and_then(|e| e.ballot_date)),
        payment_date: pick(&usable, |c| c.payment_date)
            .or_else(|| existing.and_then(|e| e.payment_date)),
        listing_date: pick(&usable, |c| c.listing_date)
            .or_else(|| existing.and_then(|e| e.listing_date)),
        status,
        lifecycle_status: lifecycle,
        event_version: existing.map(|e| e.event_version).unwrap_or(1),
        announcement_url: pick_text(&usable, |c| c.announcement_url.as_ref())
            .or_else(|| existing.and_then(|e| e.announcement_url.clone())),
        data_quality_status: quality,
        data_conflict: conflict,
        manual_override_fields: existing
            .map(|e| e.manual_override_fields.clone())
            .unwrap_or_default(),
        sessions,
        first_seen_at: existing.map(|e| e.first_seen_at).unwrap_or(now),
        updated_at: now,
    })
}

fn pick<T: Copy>(items: &[&Candidate], selector: impl Fn(&Candidate) -> Option<T>) -> Option<T> {
    items.iter().find_map(|c| selector(c))
}
fn pick_text(
    items: &[&Candidate],
    selector: impl Fn(&Candidate) -> Option<&String>,
) -> Option<String> {
    items
        .iter()
        .find_map(|c| selector(c).filter(|v| !v.trim().is_empty()).cloned())
}
fn conflicts(items: &[&Candidate], selector: impl Fn(&Candidate) -> Option<String>) -> bool {
    let mut values: Vec<String> = items.iter().filter_map(|c| selector(c)).collect();
    values.sort();
    values.dedup();
    values.len() > 1
}

pub fn event_hash(event: &IpoEvent) -> String {
    sha256(format!(
        "{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}",
        event.id,
        event.event_version,
        event.apply_code,
        event.apply_date,
        event.issue_price,
        event.max_apply_quantity,
        event.status
    ))
}

pub fn group_candidates(candidates: Vec<Candidate>) -> HashMap<String, Vec<Candidate>> {
    let mut groups = HashMap::new();
    for candidate in candidates {
        if let Some(key) = candidate.stable_identity() {
            groups.entry(key).or_insert_with(Vec::new).push(candidate);
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(date: NaiveDate) -> IpoEvent {
        let now = at(date, time(8, 0));
        IpoEvent {
            id: "shanghai:601001".into(),
            exchange: Exchange::Shanghai,
            board: Board::Main,
            security_code: "601001".into(),
            apply_code: Some("780001".into()),
            legacy_code: None,
            name: "测试股份".into(),
            apply_date: Some(date),
            issue_price: Some(10.0),
            lot_size: Some(500),
            max_apply_quantity: Some(10_000),
            required_market_value: Some(100_000.0),
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: IssueStatus::Active,
            lifecycle_status: LifecycleStatus::ActiveUnconfirmed,
            event_version: 1,
            announcement_url: None,
            data_quality_status: DataQualityStatus::SingleSource,
            data_conflict: false,
            manual_override_fields: Vec::new(),
            sessions: Vec::new(),
            first_seen_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn reminder_plan_contains_final_cutoff_and_unique_keys() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let reminders = plan_reminders(&event(date), &AppSettings::default());
        assert!(
            reminders.iter().any(
                |item| item.level == ReminderLevel::Final && item.due_at.time() == time(14, 55)
            )
        );
        let mut keys: Vec<_> = reminders
            .iter()
            .map(|item| item.dedupe_key.as_str())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), reminders.len());
    }

    #[test]
    fn acknowledged_event_has_no_reminders() {
        let mut value = event(NaiveDate::from_ymd_opt(2026, 8, 25).unwrap());
        value.lifecycle_status = LifecycleStatus::Acknowledged;
        assert!(plan_reminders(&value, &AppSettings::default()).is_empty());
    }

    #[test]
    fn detects_conflicting_candidates() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let now = at(date, time(8, 0));
        let base = Candidate {
            source: "a".into(),
            priority: 100,
            fetched_at: now,
            published_at: None,
            exchange: Exchange::Shanghai,
            board: Board::Main,
            security_code: Some("601001".into()),
            apply_code: Some("780001".into()),
            legacy_code: None,
            name: Some("测试股份".into()),
            apply_date: Some(date),
            issue_price: Some(10.0),
            lot_size: None,
            max_apply_quantity: None,
            required_market_value: None,
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: IssueStatus::Active,
            announcement_url: None,
            sessions: Vec::new(),
            announcement_derived: false,
        };
        let mut second = base.clone();
        second.source = "b".into();
        second.priority = 200;
        second.issue_price = Some(11.0);
        let resolved =
            reconcile_candidates(&[base, second], None, &AppSettings::default(), now).unwrap();
        assert!(resolved.data_conflict);
        assert_eq!(
            resolved.data_quality_status,
            DataQualityStatus::DataConflict
        );
    }
}

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

pub fn plan_reminders(
    event: &IpoEvent,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> Vec<ReminderItem> {
    let Some(date) = event.apply_date else {
        return vec![];
    };
    if event.is_terminal() || !settings.exchange_enabled(event.exchange) {
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
    let today = now.date_naive();
    if event.lifecycle_status != LifecycleStatus::Acknowledged && date >= today {
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
    }

    if matches!(
        event.lifecycle_status,
        LifecycleStatus::Acknowledged | LifecycleStatus::AcknowledgedNeedsReview
    ) {
        if settings.post_apply_reminders_enabled {
            // These are prompts to check the broker, not inferred exchange deadlines.
            if let Some(ballot_date) = event
                .ballot_date
                .filter(|value| *value >= date && *value >= today)
            {
                add(at(ballot_date, time(18, 0)), ReminderLevel::BallotCheck);
            }
            if let Some(payment_date) = event
                .payment_date
                .filter(|value| *value >= date && *value >= today)
            {
                add(at(payment_date, time(8, 30)), ReminderLevel::PaymentMorning);
                add(
                    at(payment_date, time(14, 0)),
                    ReminderLevel::PaymentFollowUp,
                );
            }
        }
        if settings.listing_reminders_enabled {
            if let Some(listing_date) = event
                .listing_date
                .filter(|value| *value >= date && *value >= today)
            {
                add(at(listing_date, time(8, 30)), ReminderLevel::ListingMorning);
            }
        }
    }
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
    sha256(critical_event_signature(event))
}

pub fn critical_change_reason(previous: &IpoEvent, current: &IpoEvent) -> Option<String> {
    let mut fields = Vec::new();
    if previous.apply_code != current.apply_code {
        fields.push("申购代码");
    }
    if previous.apply_date != current.apply_date {
        fields.push("申购日期");
    }
    if previous.issue_price != current.issue_price {
        fields.push("发行价格");
    }
    if previous.max_apply_quantity != current.max_apply_quantity {
        fields.push("申购上限");
    }
    if previous.lot_size != current.lot_size {
        fields.push("申购单位");
    }
    if previous.required_market_value != current.required_market_value {
        fields.push("所需市值");
    }
    if previous.required_cash != current.required_cash {
        fields.push("所需资金");
    }
    if previous.status != current.status {
        fields.push("发行状态");
    }
    if critical_session_signature(previous) != critical_session_signature(current) {
        fields.push("官方申购时段或资金规则");
    }
    (!fields.is_empty()).then(|| format!("关键申购字段已变化：{}", fields.join("、")))
}

pub fn noncritical_change_reason(previous: &IpoEvent, current: &IpoEvent) -> Option<String> {
    let mut fields = Vec::new();
    if previous.name != current.name {
        fields.push("证券简称");
    }
    if previous.legacy_code != current.legacy_code {
        fields.push("历史证券代码");
    }
    if previous.ballot_date != current.ballot_date {
        fields.push("中签结果日期");
    }
    if previous.payment_date != current.payment_date {
        fields.push("缴款日期");
    }
    if previous.listing_date != current.listing_date {
        fields.push("上市日期");
    }
    if previous.announcement_url != current.announcement_url {
        fields.push("公告链接");
    }
    (!fields.is_empty()).then(|| format!("普通任务字段已变化：{}", fields.join("、")))
}

fn critical_event_signature(event: &IpoEvent) -> String {
    format!(
        "{}|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        event.id,
        event.event_version,
        event.apply_code,
        event.apply_date,
        event.issue_price,
        event.max_apply_quantity,
        event.lot_size,
        event.required_market_value,
        event.required_cash,
        event.status,
        critical_session_signature(event)
    )
}

fn critical_session_signature(
    event: &IpoEvent,
) -> Vec<(i32, NaiveTime, NaiveTime, FundingMode, bool)> {
    let mut sessions = event
        .sessions
        .iter()
        .map(|session| {
            (
                session.session_number,
                session.official_start,
                session.official_end,
                session.funding_mode,
                session.allocation_time_sensitive,
            )
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.0);
    sessions
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
        let reminders = plan_reminders(
            &event(date),
            &AppSettings::default(),
            at(date - Duration::days(1), time(8, 0)),
        );
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
        assert!(
            plan_reminders(
                &value,
                &AppSettings::default(),
                at(value.apply_date.unwrap(), time(10, 0))
            )
            .is_empty()
        );
    }

    #[test]
    fn acknowledged_event_plans_only_known_post_apply_prompts() {
        let apply_date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let mut value = event(apply_date);
        value.lifecycle_status = LifecycleStatus::Acknowledged;
        value.ballot_date = Some(apply_date + Duration::days(1));
        value.payment_date = Some(apply_date + Duration::days(2));
        value.listing_date = Some(apply_date + Duration::days(8));

        let reminders =
            plan_reminders(&value, &AppSettings::default(), at(apply_date, time(10, 0)));
        assert_eq!(reminders.len(), 4);
        assert!(
            reminders
                .iter()
                .any(|item| item.level == ReminderLevel::BallotCheck)
        );
        assert!(
            reminders
                .iter()
                .any(|item| item.level == ReminderLevel::PaymentMorning)
        );
        assert!(
            reminders
                .iter()
                .any(|item| item.level == ReminderLevel::PaymentFollowUp)
        );
        assert!(reminders.iter().any(|item| {
            item.level == ReminderLevel::ListingMorning && item.due_at.time() == time(8, 30)
        }));

        let disabled = AppSettings {
            post_apply_reminders_enabled: false,
            listing_reminders_enabled: false,
            ..AppSettings::default()
        };
        assert!(plan_reminders(&value, &disabled, at(apply_date, time(10, 0))).is_empty());

        let listing_only = AppSettings {
            post_apply_reminders_enabled: false,
            ..AppSettings::default()
        };
        let reminders = plan_reminders(&value, &listing_only, at(apply_date, time(10, 0)));
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].level, ReminderLevel::ListingMorning);
    }

    #[test]
    fn post_apply_plan_is_time_deterministic_and_skips_expired_dates() {
        let apply_date = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let mut value = event(apply_date);
        value.lifecycle_status = LifecycleStatus::Acknowledged;
        value.ballot_date = Some(today - Duration::days(1));
        value.payment_date = Some(today);
        value.listing_date = Some(today + Duration::days(1));

        let reminders = plan_reminders(&value, &AppSettings::default(), at(today, time(13, 0)));
        assert_eq!(reminders.len(), 3);
        assert!(
            !reminders
                .iter()
                .any(|item| item.level == ReminderLevel::BallotCheck)
        );
        assert!(reminders.iter().any(|item| {
            item.level == ReminderLevel::PaymentMorning && item.due_at == at(today, time(8, 30))
        }));
        assert!(reminders.iter().any(|item| {
            item.level == ReminderLevel::PaymentFollowUp && item.due_at == at(today, time(14, 0))
        }));
        assert!(reminders.iter().any(|item| {
            item.level == ReminderLevel::ListingMorning
                && item.due_at == at(today + Duration::days(1), time(8, 30))
        }));
    }

    #[test]
    fn old_unconfirmed_or_review_event_does_not_recreate_expired_apply_reminders() {
        let apply_date = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let mut unconfirmed = event(apply_date);
        unconfirmed.lifecycle_status = LifecycleStatus::ActiveUnconfirmed;
        unconfirmed.listing_date = Some(today + Duration::days(2));
        assert!(
            plan_reminders(&unconfirmed, &AppSettings::default(), at(today, time(9, 0)),)
                .is_empty()
        );

        let mut needs_review = unconfirmed;
        needs_review.lifecycle_status = LifecycleStatus::AcknowledgedNeedsReview;
        let reminders = plan_reminders(
            &needs_review,
            &AppSettings::default(),
            at(today, time(9, 0)),
        );
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].level, ReminderLevel::ListingMorning);
    }

    #[test]
    fn critical_change_detection_covers_subscription_limits_and_rules() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let mut original = event(date);
        original.sessions = vec![SubscriptionSession {
            session_number: 1,
            official_start: time(9, 30),
            official_end: time(15, 0),
            broker_accept_start: Some(time(9, 15)),
            safety_cutoff: Some(time(14, 55)),
            funding_mode: FundingMode::MarketValue,
            allocation_time_sensitive: false,
            source: "fixture-a".into(),
            source_published_at: Some(at(date, time(7, 30))),
        }];

        let mut changed = original.clone();
        changed.max_apply_quantity = Some(20_000);
        changed.lot_size = Some(1_000);
        changed.sessions[0].official_start = time(9, 15);
        changed.sessions[0].funding_mode = FundingMode::FullCash;
        let reason = critical_change_reason(&original, &changed).unwrap();
        assert!(reason.contains("申购上限"));
        assert!(reason.contains("申购单位"));
        assert!(reason.contains("官方申购时段或资金规则"));
        assert_ne!(event_hash(&original), event_hash(&changed));

        let mut metadata_only = original.clone();
        metadata_only.sessions[0].broker_accept_start = Some(time(9, 0));
        metadata_only.sessions[0].safety_cutoff = Some(time(14, 50));
        metadata_only.sessions[0].source = "fixture-b".into();
        metadata_only.sessions[0].source_published_at = Some(at(date, time(8, 0)));
        assert!(critical_change_reason(&original, &metadata_only).is_none());
        assert_eq!(event_hash(&original), event_hash(&metadata_only));
    }

    #[test]
    fn noncritical_change_detection_excludes_subscription_conditions() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let original = event(date);
        let mut changed = original.clone();
        changed.name = "测试股份新简称".into();
        changed.listing_date = Some(date + Duration::days(10));
        let reason = noncritical_change_reason(&original, &changed).unwrap();
        assert!(reason.contains("证券简称"));
        assert!(reason.contains("上市日期"));

        let mut critical_only = original.clone();
        critical_only.issue_price = Some(11.0);
        assert!(noncritical_change_reason(&original, &critical_only).is_none());
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

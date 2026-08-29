use std::collections::{BTreeMap, HashMap, HashSet};

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
    } else if code.is_some_and(|c| c.starts_with("43") || c.starts_with('8') || c.starts_with('9'))
    {
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
    // 语义是「最后结束的时段」：取最大的 official_end，
    // 不依赖 Vec 存储顺序或场次号，对乱序数据稳健。
    settings.safety_cutoff.min(
        sessions
            .iter()
            .map(|session| session.official_end)
            .max()
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
    // Postponed 在业务上可恢复（新申购日公布后重新规划），因此不并入
    // is_terminal()；但在恢复前必须停止全部申购提醒。
    if event.is_terminal()
        || event.status == IssueStatus::Postponed
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
    let selected_apply_date =
        pick(&usable, |c| c.apply_date).or_else(|| existing.and_then(|e| e.apply_date));
    let issue_price =
        pick(&usable, |c| c.issue_price).or_else(|| existing.and_then(|e| e.issue_price));
    let selected_status = usable
        .iter()
        .find_map(|c| (c.status != IssueStatus::Unknown).then_some(c.status))
        .or_else(|| existing.map(|e| e.status))
        .unwrap_or(IssueStatus::Unknown);
    let apply_date_conflict = conflicts(&usable, |c| c.apply_date.map(|d| d.to_string()));
    let status_conflict = conflicts(&usable, |c| {
        (c.status != IssueStatus::Unknown).then(|| format!("{:?}", c.status))
    });
    let (apply_date, status) = resolve_postponed_transition(
        &usable,
        existing,
        selected_apply_date,
        selected_status,
        apply_date_conflict,
        status_conflict,
    );
    let conflict = conflicts(&usable, |c| c.apply_code.clone())
        || apply_date_conflict
        || conflicts(&usable, |c| c.issue_price.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.lot_size.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.max_apply_quantity.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.required_market_value.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.required_cash.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.ballot_date.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.payment_date.map(|v| v.to_string()))
        || conflicts(&usable, |c| c.listing_date.map(|v| v.to_string()))
        || conflicts(&usable, candidate_session_signature)
        || status_conflict;
    let distinct_sources = usable
        .iter()
        .map(|candidate| candidate.source.as_str())
        .collect::<HashSet<_>>()
        .len();
    let quality = if conflict {
        DataQualityStatus::DataConflict
    } else if distinct_sources > 1 {
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

/// 官方交易所/巨潮/北交所候选使用 200；聚合源使用 100。非人工
/// Postponed 只能由同一条高可信候选携带“正常状态 + 不同新日期”恢复。
const TRUSTED_CANDIDATE_PRIORITY: i32 = 200;

fn resolve_postponed_transition(
    candidates: &[&Candidate],
    existing: Option<&IpoEvent>,
    selected_apply_date: Option<NaiveDate>,
    selected_status: IssueStatus,
    apply_date_conflict: bool,
    status_conflict: bool,
) -> (Option<NaiveDate>, IssueStatus) {
    let Some(previous) = existing.filter(|event| {
        event.status == IssueStatus::Postponed
            && !event
                .manual_override_fields
                .iter()
                .any(|field| field == "IssueStatus")
    }) else {
        return (selected_apply_date, selected_status);
    };

    // 更严重或明确的非正常状态仍可更新；这里只阻止由日期派生出的
    // Upcoming/Active 在缺少可信新日期时把 Postponed 静默解封。
    if matches!(
        selected_status,
        IssueStatus::Suspended | IssueStatus::Terminated | IssueStatus::Completed
    ) {
        return (selected_apply_date, selected_status);
    }

    if !apply_date_conflict
        && !status_conflict
        && let Some(candidate) = candidates.iter().copied().find(|candidate| {
            candidate.priority >= TRUSTED_CANDIDATE_PRIORITY
                && matches!(
                    candidate.status,
                    IssueStatus::Upcoming | IssueStatus::Active
                )
                && candidate
                    .apply_date
                    .is_some_and(|date| Some(date) != previous.apply_date)
        })
    {
        return (candidate.apply_date, candidate.status);
    }

    // 冲突期间保留遗留日期；否则把某个冲突值写入后，会导致冲突消失时
    // “新日期”与已保存值相同，从而永远无法恢复。
    (previous.apply_date, IssueStatus::Postponed)
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

fn candidate_session_signature(candidate: &Candidate) -> Option<String> {
    if candidate.sessions.is_empty() {
        return None;
    }
    let mut sessions = candidate
        .sessions
        .iter()
        .map(|session| {
            format!(
                "{}|{}|{}|{:?}|{}",
                session.session_number,
                session.official_start,
                session.official_end,
                session.funding_mode,
                session.allocation_time_sensitive
            )
        })
        .collect::<Vec<_>>();
    sessions.sort();
    Some(sessions.join(";"))
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
    fn postponed_event_has_no_reminders() {
        let mut value = event(NaiveDate::from_ymd_opt(2026, 8, 25).unwrap());
        value.status = IssueStatus::Postponed;
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
    fn effective_cutoff_uses_latest_official_end_regardless_of_order() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let mut value = event(date);
        let settings = AppSettings::default();
        // 乱序存储：先下午场（结束 15:00），再上午场（结束 11:30）。
        // 语义是「最后结束的时段」→ max(official_end)=15:00，再与
        // 默认安全截止 14:55 取小；若错误地取 .last() 会得到 11:30。
        value.sessions = vec![
            SubscriptionSession {
                session_number: 1,
                official_start: time(13, 0),
                official_end: time(15, 0),
                broker_accept_start: Some(time(9, 15)),
                safety_cutoff: None,
                funding_mode: FundingMode::MarketValue,
                allocation_time_sensitive: false,
                source: "fixture-a".into(),
                source_published_at: None,
            },
            SubscriptionSession {
                session_number: 2,
                official_start: time(9, 30),
                official_end: time(11, 30),
                broker_accept_start: None,
                safety_cutoff: None,
                funding_mode: FundingMode::MarketValue,
                allocation_time_sensitive: false,
                source: "fixture-a".into(),
                source_published_at: None,
            },
        ];
        assert_eq!(effective_cutoff(&value, &settings), time(14, 55));
        // 空时段回退默认截止。
        value.sessions = vec![];
        assert_eq!(effective_cutoff(&value, &settings), time(14, 55));
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

    #[test]
    fn beijing_43_prefix_is_detected_without_the_optional_market_flag() {
        assert_eq!(
            detect_exchange(Some("430001"), None, false),
            Exchange::Beijing
        );
    }

    #[test]
    fn repeated_rows_from_one_source_do_not_claim_multi_source_verification() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let now = at(date, time(8, 0));
        let mut first = Candidate {
            source: "same-source".into(),
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
            lot_size: Some(500),
            max_apply_quantity: Some(10_000),
            required_market_value: Some(100_000.0),
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: IssueStatus::Active,
            announcement_url: None,
            sessions: Vec::new(),
        };
        let mut duplicate = first.clone();
        duplicate.priority = 90;
        duplicate.fetched_at += chrono::Duration::seconds(1);
        let resolved = reconcile_candidates(
            &[first.clone(), duplicate],
            None,
            &AppSettings::default(),
            now,
        )
        .unwrap();
        assert_eq!(
            resolved.data_quality_status,
            DataQualityStatus::SingleSource
        );

        first.source = "other-source".into();
        let verified = reconcile_candidates(
            &[first, resolved_candidate_fixture(date, now)],
            None,
            &AppSettings::default(),
            now,
        )
        .unwrap();
        assert_eq!(
            verified.data_quality_status,
            DataQualityStatus::MultiSourceVerified
        );
    }

    fn resolved_candidate_fixture(date: NaiveDate, now: ChinaDateTime) -> Candidate {
        Candidate {
            source: "same-source".into(),
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
            lot_size: Some(500),
            max_apply_quantity: Some(10_000),
            required_market_value: Some(100_000.0),
            required_cash: None,
            ballot_date: None,
            payment_date: None,
            listing_date: None,
            status: IssueStatus::Active,
            announcement_url: None,
            sessions: Vec::new(),
        }
    }

    #[test]
    fn non_manual_postponed_requires_a_trusted_consistent_new_date_to_resume() {
        let old_date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let new_date = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let now = at(old_date - Duration::days(1), time(8, 0));
        let mut existing = event(old_date);
        existing.status = IssueStatus::Postponed;

        let mut same_date = resolved_candidate_fixture(old_date, now);
        same_date.priority = TRUSTED_CANDIDATE_PRIORITY;
        same_date.status = IssueStatus::Upcoming;
        let unchanged =
            reconcile_candidates(&[same_date], Some(&existing), &AppSettings::default(), now)
                .unwrap();
        assert_eq!(unchanged.status, IssueStatus::Postponed);
        assert_eq!(unchanged.apply_date, Some(old_date));

        let mut untrusted = resolved_candidate_fixture(new_date, now);
        untrusted.priority = TRUSTED_CANDIDATE_PRIORITY - 1;
        untrusted.status = IssueStatus::Upcoming;
        let unchanged =
            reconcile_candidates(&[untrusted], Some(&existing), &AppSettings::default(), now)
                .unwrap();
        assert_eq!(unchanged.status, IssueStatus::Postponed);
        assert_eq!(unchanged.apply_date, Some(old_date));

        let mut trusted = resolved_candidate_fixture(new_date, now);
        trusted.priority = TRUSTED_CANDIDATE_PRIORITY;
        trusted.status = IssueStatus::Upcoming;
        let resumed =
            reconcile_candidates(&[trusted], Some(&existing), &AppSettings::default(), now)
                .unwrap();
        assert_eq!(resumed.status, IssueStatus::Upcoming);
        assert_eq!(resumed.apply_date, Some(new_date));
        assert!(!plan_reminders(&resumed, &AppSettings::default(), now).is_empty());
    }

    #[test]
    fn postponed_keeps_the_legacy_date_until_new_date_conflicts_clear() {
        let old_date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let first_new_date = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let second_new_date = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        let now = at(old_date - Duration::days(1), time(8, 0));
        let mut existing = event(old_date);
        existing.status = IssueStatus::Postponed;

        let mut first = resolved_candidate_fixture(first_new_date, now);
        first.priority = TRUSTED_CANDIDATE_PRIORITY;
        first.source = "official".into();
        first.status = IssueStatus::Upcoming;
        let mut second = resolved_candidate_fixture(second_new_date, now);
        second.source = "mirror".into();
        second.status = IssueStatus::Upcoming;

        let conflicted = reconcile_candidates(
            &[first.clone(), second],
            Some(&existing),
            &AppSettings::default(),
            now,
        )
        .unwrap();
        assert!(conflicted.data_conflict);
        assert_eq!(conflicted.status, IssueStatus::Postponed);
        assert_eq!(conflicted.apply_date, Some(old_date));
        assert!(plan_reminders(&conflicted, &AppSettings::default(), now).is_empty());

        let resumed =
            reconcile_candidates(&[first], Some(&conflicted), &AppSettings::default(), now)
                .unwrap();
        assert_eq!(resumed.status, IssueStatus::Upcoming);
        assert_eq!(resumed.apply_date, Some(first_new_date));
    }

    #[test]
    fn conflicts_cover_quantity_cash_dates_and_session_rules() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 25).unwrap();
        let now = at(date, time(8, 0));
        let base = resolved_candidate_fixture(date, now);
        let assert_conflict = |mut changed: Candidate| {
            changed.source = "other-source".into();
            let resolved =
                reconcile_candidates(&[base.clone(), changed], None, &AppSettings::default(), now)
                    .unwrap();
            assert!(resolved.data_conflict);
        };

        let mut changed = base.clone();
        changed.lot_size = Some(1_000);
        assert_conflict(changed);
        let mut changed = base.clone();
        changed.max_apply_quantity = Some(20_000);
        assert_conflict(changed);
        let mut changed = base.clone();
        changed.required_market_value = Some(200_000.0);
        assert_conflict(changed);
        let mut original_cash = base.clone();
        original_cash.required_cash = Some(40_000.0);
        let mut changed_cash = base.clone();
        changed_cash.source = "other-source".into();
        changed_cash.required_cash = Some(50_000.0);
        assert!(
            reconcile_candidates(
                &[original_cash, changed_cash],
                None,
                &AppSettings::default(),
                now,
            )
            .unwrap()
            .data_conflict
        );
        let mut changed = base.clone();
        changed.ballot_date = Some(date + Duration::days(1));
        let mut original_with_date = base.clone();
        original_with_date.ballot_date = Some(date + Duration::days(2));
        original_with_date.source = "third-source".into();
        let resolved = reconcile_candidates(
            &[original_with_date, changed],
            None,
            &AppSettings::default(),
            now,
        )
        .unwrap();
        assert!(resolved.data_conflict);
        let mut first_payment = base.clone();
        first_payment.payment_date = Some(date + Duration::days(2));
        let mut changed_payment = first_payment.clone();
        changed_payment.source = "other-source".into();
        changed_payment.payment_date = Some(date + Duration::days(3));
        assert!(
            reconcile_candidates(
                &[first_payment, changed_payment],
                None,
                &AppSettings::default(),
                now,
            )
            .unwrap()
            .data_conflict
        );
        let mut first_listing = base.clone();
        first_listing.listing_date = Some(date + Duration::days(7));
        let mut changed_listing = first_listing.clone();
        changed_listing.source = "other-source".into();
        changed_listing.listing_date = Some(date + Duration::days(8));
        assert!(
            reconcile_candidates(
                &[first_listing, changed_listing],
                None,
                &AppSettings::default(),
                now,
            )
            .unwrap()
            .data_conflict
        );

        let mut first_session = base.clone();
        first_session.sessions = default_sessions(Exchange::Shanghai, &AppSettings::default());
        let mut changed_session = first_session.clone();
        changed_session.source = "other-source".into();
        changed_session.sessions[0].official_start = time(9, 15);
        let resolved = reconcile_candidates(
            &[first_session, changed_session],
            None,
            &AppSettings::default(),
            now,
        )
        .unwrap();
        assert!(resolved.data_conflict);
    }
}

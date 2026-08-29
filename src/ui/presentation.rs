use super::*;

pub(crate) fn diagnostic_summary(runtime: &RuntimeHandle, data_root: &PathBuf) -> String {
    let snapshot = runtime.snapshot();
    let toast_diagnostics = windows_integration::toast_diagnostics();
    let (toast_status, _) = toast_platform_display(&toast_diagnostics);
    let mut lines = vec![
        "A 股新股申购提醒 - 诊断摘要".to_owned(),
        format!(
            "生成时间：{}",
            crate::core::now_china().format("%Y-%m-%d %H:%M:%S %:z")
        ),
        format!("运行状态：{}", snapshot.status_text),
        format!("最近同步：{}", snapshot.last_sync_text),
        format!("同步成功：{:?}", snapshot.last_sync_succeeded),
        format!("系统时间：{}", snapshot.clock_text),
        snapshot.next_wake_text.clone(),
        toast_status,
        format!(
            "Toast AUMID：{}，进程标识：{}，开始菜单匹配：{}",
            toast_diagnostics.app_user_model_id,
            toast_diagnostics.process_identity_set,
            toast_diagnostics.shortcut_aumid_matches
        ),
        format!("数据目录：{}", data_root.display()),
    ];
    if let Ok(health) = runtime.health_details() {
        lines.push(format!("总体健康：{:?}", health.overall_state));
        lines.push(format!(
            "今日任务：{}，待确认：{}",
            health.today_task_count, health.pending_confirmation_count
        ));
        for source in health.sources {
            lines.push(format!(
                "{}：{}，最近成功 {}，连续失败 {}",
                source.source,
                health_state_text(source.state),
                format_timestamp(source.last_success_at),
                source.consecutive_failures
            ));
        }
    }
    lines.join("\r\n")
}

pub(crate) fn is_pending(event: &IpoEvent) -> bool {
    can_acknowledge_on(
        event.apply_date,
        event.lifecycle_status,
        crate::core::now_china().date_naive(),
    )
}

pub(crate) fn can_acknowledge_on(
    apply_date: Option<chrono::NaiveDate>,
    lifecycle_status: LifecycleStatus,
    today: chrono::NaiveDate,
) -> bool {
    apply_date == Some(today)
        && matches!(
            lifecycle_status,
            LifecycleStatus::Scheduled
                | LifecycleStatus::ActiveUnconfirmed
                | LifecycleStatus::AcknowledgedNeedsReview
        )
}

pub(crate) fn event_needs_review(event: &IpoEvent) -> bool {
    event.data_conflict
        || matches!(
            event.data_quality_status,
            DataQualityStatus::DataConflict
                | DataQualityStatus::Stale
                | DataQualityStatus::ManualReviewRequired
        )
        || matches!(
            event.lifecycle_status,
            LifecycleStatus::AcknowledgedNeedsReview | LifecycleStatus::ExpiredUnconfirmed
        )
}

pub(crate) fn market_name(exchange: Exchange, board: Board) -> &'static str {
    match (exchange, board) {
        (Exchange::Shanghai, Board::Star) => "沪市·科创板",
        (Exchange::Shanghai, _) => "沪市·主板",
        (Exchange::Shenzhen, Board::ChiNext) => "深市·创业板",
        (Exchange::Shenzhen, _) => "深市·主板",
        (Exchange::Beijing, _) => "北交所",
        _ => "未知市场",
    }
}

pub(crate) fn default_session_text(exchange: Exchange) -> &'static str {
    if exchange == Exchange::Shanghai {
        "09:30–11:30；13:00–15:00（默认市场时段，请以交易所或券商当日规则为准）"
    } else {
        "09:15–11:30；13:00–15:00（默认市场时段，请以交易所或券商当日规则为准）"
    }
}

pub(crate) fn quality_text(status: DataQualityStatus) -> &'static str {
    match status {
        DataQualityStatus::AnnouncementVerified => "历史公告解析记录",
        DataQualityStatus::MultiSourceVerified => "多源一致",
        DataQualityStatus::SingleSource => "单一来源待核验",
        DataQualityStatus::DataConflict => "来源冲突",
        DataQualityStatus::Stale => "数据陈旧",
        DataQualityStatus::ManualReviewRequired => "待人工核验",
        _ => "状态未知",
    }
}

pub(crate) fn health_state_text(state: HealthState) -> &'static str {
    match state {
        HealthState::Healthy => "正常",
        HealthState::Warning => "陈旧/警告",
        HealthState::Failed => "失败",
        _ => "未知",
    }
}

pub(crate) fn health_state_level(state: HealthState) -> i32 {
    match state {
        HealthState::Healthy => 1,
        HealthState::Warning => 2,
        HealthState::Failed => 3,
        _ => 0,
    }
}

pub(crate) fn format_timestamp(value: Option<model::ChinaDateTime>) -> String {
    value
        .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "无".into())
}

pub(crate) fn reminder_body(
    event: &IpoEvent,
    level: ReminderLevel,
    message: Option<&str>,
) -> String {
    if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
        return format!(
            "{message}\n申购代码：{}\n请打开任务详情核对最新信息。",
            event.display_code()
        );
    }
    if level == ReminderLevel::BallotCheck {
        return format!(
            "中签结果查询日期：{}\n股票代码：{}\n请登录券商客户端或核对正式公告查询中签结果。本程序不会读取券商账户，也不会自动判断是否中签。",
            event
                .ballot_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    if matches!(
        level,
        ReminderLevel::PaymentMorning | ReminderLevel::PaymentFollowUp
    ) {
        let phase = if level == ReminderLevel::PaymentMorning {
            "今天是公开数据标记的缴款日，请尽早检查"
        } else {
            "缴款日已到下午，请再次确认"
        };
        return format!(
            "{phase}\n缴款日期：{}\n股票代码：{}\n请登录券商客户端核对是否中签，并按券商规则确保资金账户足额。具体到账要求以正式公告和券商为准；本程序不会读取账户或确认缴款结果。",
            event
                .payment_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    if level == ReminderLevel::ListingMorning {
        return format!(
            "公开数据标记今天为上市日：{}\n股票代码：{}\n请在开盘前核对交易所公告和行情软件。本提醒不读取持仓、不跟踪收益，也不代表证券已经可以正常交易。",
            event
                .listing_date
                .map(|value| value.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.security_code,
        );
    }
    let level_text = reminder_level_text(level);
    format!(
        "{level_text}\n申购代码：{}\n发行价：{}\n请在券商客户端完成后点击“确认已申购”。",
        event.display_code(),
        event
            .issue_price
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "待核验".into())
    )
}

pub(crate) fn reminder_title_text(level: ReminderLevel) -> &'static str {
    match level {
        ReminderLevel::BallotCheck => "中签查询提醒",
        ReminderLevel::PaymentMorning | ReminderLevel::PaymentFollowUp => "缴款资金提醒",
        ReminderLevel::ListingMorning => "上市日提醒",
        _ => "打新提醒",
    }
}

pub(crate) fn reminder_level_text(level: ReminderLevel) -> &'static str {
    match level {
        ReminderLevel::Advance => "明日申购预告",
        ReminderLevel::Morning => "今日申购提醒",
        ReminderLevel::BrokerOpening | ReminderLevel::MarketOpening => "申购通道即将开放",
        ReminderLevel::FifteenMinutes
        | ReminderLevel::FiveMinutes
        | ReminderLevel::TwoMinutes
        | ReminderLevel::Final => "接近安全截止时间",
        ReminderLevel::DataChanged => "申购任务信息有变化",
        ReminderLevel::BallotCheck => "请查询中签结果",
        ReminderLevel::PaymentMorning => "缴款日，请尽早核对中签与资金",
        ReminderLevel::PaymentFollowUp => "缴款日下午，请再次确认资金状态",
        ReminderLevel::ListingMorning => "公开数据标记今天为上市日，请核对正式公告",
        _ => "申购任务尚未确认",
    }
}

pub(crate) fn lifecycle_text(status: LifecycleStatus) -> &'static str {
    match status {
        LifecycleStatus::Scheduled => "待申购日",
        LifecycleStatus::ActiveUnconfirmed => "今日待确认",
        LifecycleStatus::Acknowledged => "已确认",
        LifecycleStatus::AcknowledgedNeedsReview => "已确认但需复核",
        LifecycleStatus::SuspendedOrCancelled => "暂停或终止",
        LifecycleStatus::ExpiredUnconfirmed => "已过截止时间",
        _ => "已发现",
    }
}

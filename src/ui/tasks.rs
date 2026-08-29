use super::*;

/// 刷新主窗口运行时状态与任务列表。返回 `false` 表示数据库读取失败、
/// 刷新未完成；调用方不得据此推进已消费的 UI revision。
pub(crate) fn refresh_ui(ui: &MainWindow, runtime: &RuntimeHandle) -> bool {
    let snapshot = runtime.snapshot();
    ui.set_is_synchronizing(snapshot.is_synchronizing);
    ui.set_status_text(snapshot.status_text.clone().into());
    ui.set_sync_text(if snapshot.last_sync_text == "尚未同步" {
        "尚未完成同步".into()
    } else {
        format!("最近同步 {}", snapshot.last_sync_text).into()
    });
    ui.set_health_text(snapshot.health_text.clone().into());
    ui.set_clock_text(snapshot.clock_text.clone().into());
    let level = if snapshot.last_sync_succeeded == Some(false)
        || snapshot.health_state == HealthState::Failed
        || snapshot.clock_state == HealthState::Failed
    {
        2
    } else if snapshot.last_sync_succeeded == Some(true)
        && snapshot.health_state == HealthState::Healthy
        && snapshot.clock_state != HealthState::Failed
    {
        1
    } else {
        0
    };
    ui.set_runtime_level(level);

    if !runtime.is_ready() {
        return false;
    }

    let settings = match runtime.settings() {
        Ok(settings) => settings,
        Err(error) => {
            ui.set_status_text(format!("读取应用设置失败：{error:#}").into());
            return false;
        }
    };
    let today = match runtime.today_events() {
        Ok(events) => events,
        Err(error) => {
            ui.set_status_text(format!("读取今日任务失败：{error:#}").into());
            return false;
        }
    };
    let future = match runtime.future_events() {
        Ok(events) => events,
        Err(error) => {
            ui.set_status_text(format!("读取未来任务失败：{error:#}").into());
            return false;
        }
    };
    let health = match runtime.health_details() {
        Ok(health) => health,
        Err(error) => {
            ui.set_status_text(format!("读取健康详情失败：{error:#}").into());
            return false;
        }
    };
    let filter_text = ui.get_task_filter_text().to_string();
    let market_filter = ui.get_task_market_filter_index();
    let status_filter = ui.get_task_status_filter_index();
    let visible_today = today
        .iter()
        .filter(|event| task_matches_filter(event, &filter_text, market_filter, status_filter))
        .collect::<Vec<_>>();
    let visible_future = future
        .iter()
        .filter(|event| task_matches_filter(event, &filter_text, market_filter, status_filter))
        .collect::<Vec<_>>();
    ui.set_today_count(today.len() as i32);
    ui.set_today_visible_count(visible_today.len() as i32);
    ui.set_future_count(future.len() as i32);
    ui.set_future_visible_count(visible_future.len() as i32);
    ui.set_pending_count(today.iter().filter(|event| is_pending(event)).count() as i32);
    ui.set_acknowledged_count(
        today
            .iter()
            .filter(|event| event.lifecycle_status == LifecycleStatus::Acknowledged)
            .count() as i32,
    );
    ui.set_issue_count(
        today
            .iter()
            .filter(|event| event_needs_review(event))
            .count() as i32,
    );
    ui.set_today_tasks(ModelRc::from(Rc::new(VecModel::from(
        visible_today
            .into_iter()
            .map(|event| task_row(event, &settings))
            .collect::<Vec<_>>(),
    ))));
    ui.set_future_tasks(ModelRc::from(Rc::new(VecModel::from(
        visible_future
            .into_iter()
            .map(|event| task_row(event, &settings))
            .collect::<Vec<_>>(),
    ))));

    ui.set_health_title(match health.overall_state {
        HealthState::Healthy => "程序与数据源运行正常".into(),
        HealthState::Warning => "存在待核验任务或异常数据源".into(),
        HealthState::Failed => "提醒系统需要立即检查".into(),
        _ => "健康状态尚未建立".into(),
    });
    ui.set_health_summary(
        format!(
            "今日任务 {} 只，待确认 {} 只，来源冲突 {} 只，待人工核验 {} 只，本地投递重试 {} 条。{}",
            health.today_task_count,
            health.pending_confirmation_count,
            health.conflict_count,
            health.manual_review_count,
            health.delivery_retry_count,
            health
                .latest_delivery_error
                .as_deref()
                .map(|error| format!(" 最近错误：{}", operations::redact(error)))
                .unwrap_or_default(),
        )
        .into(),
    );
    ui.set_heartbeat_text(snapshot.next_wake_text.clone().into());
    let sources = health
        .sources
        .into_iter()
        .map(|source| SourceHealthRow {
            source: source.source.into(),
            state: health_state_text(source.state).into(),
            record_text: format!("记录 {}", source.last_record_count).into(),
            last_success_text: format!(
                "最近成功 {} · 连续失败 {}",
                format_timestamp(source.last_success_at),
                source.consecutive_failures
            )
            .into(),
            error_text: source.last_error.unwrap_or_default().into(),
            state_level: health_state_level(source.state),
        })
        .collect::<Vec<_>>();
    ui.set_source_health(ModelRc::from(Rc::new(VecModel::from(sources))));
    true
}

pub(crate) fn task_matches_filter(
    event: &IpoEvent,
    text: &str,
    market_filter: i32,
    status_filter: i32,
) -> bool {
    let market_matches = match market_filter {
        1 => event.exchange == Exchange::Shanghai,
        2 => event.exchange == Exchange::Shenzhen,
        3 => event.exchange == Exchange::Beijing,
        _ => true,
    };
    let status_matches = match status_filter {
        1 => is_pending(event),
        2 => event.lifecycle_status == LifecycleStatus::Acknowledged,
        3 => event_needs_review(event),
        _ => true,
    };
    if !market_matches || !status_matches {
        return false;
    }
    let needle = text.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    [
        event.name.as_str(),
        event.security_code.as_str(),
        event.apply_code.as_deref().unwrap_or_default(),
        event.legacy_code.as_deref().unwrap_or_default(),
        lifecycle_text(event.lifecycle_status),
        quality_text(event.data_quality_status),
    ]
    .iter()
    .any(|value| value.to_lowercase().contains(&needle))
}

pub(crate) fn task_row(event: &IpoEvent, settings: &AppSettings) -> TaskRow {
    let cutoff = crate::core::effective_cutoff(event, settings);
    let price = event
        .issue_price
        .map(|value| format!("{value:.2} 元"))
        .unwrap_or_else(|| "价格待公布".into());
    let max = event
        .max_apply_quantity
        .map(|value| format!("上限 {value} 股"))
        .unwrap_or_else(|| "上限待公布".into());
    let lot = event
        .lot_size
        .map(|value| format!("单位 {value} 股"))
        .unwrap_or_else(|| "单位待公布".into());
    let session = if event.sessions.is_empty() {
        default_session_text(event.exchange).to_owned()
    } else {
        event
            .sessions
            .iter()
            .map(|session| {
                format!(
                    "{}–{}",
                    session.official_start.format("%H:%M"),
                    session.official_end.format("%H:%M")
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    };
    let mut warnings = Vec::new();
    if event.exchange == Exchange::Beijing {
        warnings.push(
            "北交所通常需全额缴付申购资金；不足 100 股余股顺序可能受提交时间影响。".to_owned(),
        );
    }
    if matches!(
        event.data_quality_status,
        DataQualityStatus::DataConflict
            | DataQualityStatus::Stale
            | DataQualityStatus::ManualReviewRequired
    ) {
        warnings.push(format!(
            "数据状态：{}，请核对正式公告。",
            quality_text(event.data_quality_status)
        ));
    }
    if event.lifecycle_status == LifecycleStatus::AcknowledgedNeedsReview {
        warnings.push("关键申购信息已变化，旧确认已失效，请核对后重新确认。".to_owned());
    }
    TaskRow {
        event_id: event.id.clone().into(),
        event_version: event.event_version,
        name: event.name.clone().into(),
        market_and_codes: format!(
            "{} · 股票 {} · 申购 {}",
            market_name(event.exchange, event.board),
            event.security_code,
            event.apply_code.as_deref().unwrap_or("待核验")
        )
        .into(),
        date_and_cutoff: format!(
            "{} / {}",
            event
                .apply_date
                .map(|date| date.to_string())
                .unwrap_or_else(|| "待公告确认".into()),
            cutoff.format("%H:%M")
        )
        .into(),
        numbers: format!("{price} · {max} · {lot}").into(),
        session: session.into(),
        status: lifecycle_text(event.lifecycle_status).into(),
        quality: quality_text(event.data_quality_status).into(),
        updated: format!("最后更新 {}", event.updated_at.format("%m-%d %H:%M")).into(),
        warning: warnings.join("\n").into(),
        needs_review: event_needs_review(event),
        can_acknowledge: is_pending(event),
        can_revoke: event.lifecycle_status == LifecycleStatus::Acknowledged,
        confirm_label: if event.lifecycle_status == LifecycleStatus::AcknowledgedNeedsReview {
            "重新确认"
        } else {
            "确认已申购"
        }
        .into(),
    }
}

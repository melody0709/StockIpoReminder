use chrono::{NaiveDate, NaiveTime};

use super::*;
use crate::finance::{
    CreditCard, RepoSettings, cycle_key, due_on_for_cycle, first_tracked_cycle,
    normalize_lead_days, status_for_card, undo_window_open, validate_card,
};

pub(crate) fn refresh_finance_ui(ui: &MainWindow, runtime: &RuntimeHandle) -> bool {
    if !runtime.is_ready() {
        return false;
    }
    let now = crate::core::now_china();
    let settings = match runtime.repo_settings() {
        Ok(value) => value,
        Err(error) => {
            ui.set_repo_error(format!("读取逆回购设置失败：{error:#}").into());
            return false;
        }
    };
    let calendar = crate::finance::TradingCalendar::builtin();
    let today = now.date_naive();
    let acked = runtime.repo_day_acked(today).unwrap_or(false);
    // 只在不处于「已修改未保存」时把数据库值回填到表单：这个函数也会被后台状态刷新调用，
    // 无条件回填会把用户正在改的开关和时间覆盖成旧值。
    if !ui.get_repo_form_dirty() {
        ui.set_repo_enabled(settings.enabled);
        ui.set_repo_primary_enabled(settings.primary.is_some());
        ui.set_repo_primary_time(
            settings
                .primary
                .unwrap_or(crate::finance::REPO_PRIMARY_DEFAULT)
                .format("%H:%M")
                .to_string()
                .into(),
        );
        ui.set_repo_fallback_enabled(settings.fallback.is_some());
        ui.set_repo_fallback_time(
            settings
                .fallback
                .unwrap_or(crate::finance::REPO_FALLBACK_DEFAULT)
                .format("%H:%M")
                .to_string()
                .into(),
        );
    }
    ui.set_repo_acked(acked);
    let status = crate::finance::status_text(now, &settings, calendar, |date| {
        runtime.repo_day_acked(date).unwrap_or(false)
    });
    ui.set_repo_status(status.into());
    ui.set_repo_status_detail(
        format!(
            "主提醒 {}；兜底 {}；今天{}。",
            settings
                .primary
                .map(|value| value.format("%H:%M").to_string())
                .unwrap_or_else(|| "关闭".into()),
            settings
                .fallback
                .map(|value| value.format("%H:%M").to_string())
                .unwrap_or_else(|| "关闭".into()),
            if calendar.is_trading_day(today) == Some(true) {
                "是交易日"
            } else if calendar.is_trading_day(today) == Some(false) {
                "休市"
            } else {
                "不在日历覆盖范围"
            }
        )
        .into(),
    );
    let days_left = calendar.days_until_coverage_end(today);
    ui.set_repo_calendar_warning(days_left < 60 || calendar.is_trading_day(today).is_none());
    ui.set_repo_calendar_text(
        format!(
            "{}；{}。覆盖剩余 {} 天。",
            calendar.coverage_text(),
            calendar.source(),
            days_left.max(0)
        )
        .into(),
    );

    let cards = match runtime.credit_cards() {
        Ok(value) => value,
        Err(error) => {
            ui.set_credit_error(format!("读取信用卡失败：{error:#}").into());
            return false;
        }
    };
    let mut rows = Vec::new();
    let mut pending = 0usize;
    let mut overdue = 0usize;
    let mut next: Option<NaiveDate> = None;
    for card in &cards {
        let settled = runtime.credit_card_settled(&card.id).unwrap_or(None);
        let status = status_for_card(card, settled.as_deref(), today);
        if card.enabled {
            pending = pending.saturating_add(1);
            if status.overdue_days > 0 {
                overdue = overdue.saturating_add(1);
            }
            if next.is_none_or(|date| status.next_due < date) {
                next = Some(status.next_due);
            }
        }
        let cycle = cycle_key(status.next_due);
        rows.push(CreditCardRow {
            id: card.id.clone().into(),
            alias: card.alias.clone().into(),
            bank: card.bank.clone().unwrap_or_default().into(),
            due_text: format!("还款日 {}", status.next_due.format("%Y-%m-%d")).into(),
            status_text: status.label.clone().into(),
            status_tone: status.tone,
            lead_text: format!(
                "提前 {}",
                card.lead_days
                    .iter()
                    .map(|days| if *days == 0 {
                        "当天".to_owned()
                    } else {
                        format!("{days} 天")
                    })
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
            .into(),
            remind_text: format!(
                "{} 提醒{}",
                card.remind_time.format("%H:%M"),
                if card.overdue_enabled {
                    " · 逾期次日再提醒"
                } else {
                    ""
                }
            )
            .into(),
            cycle_key: cycle.into(),
            enabled: card.enabled,
            can_undo: undo_window_open(settled.as_deref(), today),
        });
    }
    rows.sort_by(|left, right| {
        right
            .status_tone
            .cmp(&left.status_tone)
            .then_with(|| left.due_text.cmp(&right.due_text))
    });
    ui.set_credit_cards(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_credit_summary(
        if cards.is_empty() {
            "暂无信用卡提醒".to_owned()
        } else if overdue > 0 {
            format!("{overdue} 张已逾期 · 共 {pending} 张启用")
        } else if let Some(next) = next {
            format!("{pending} 张启用 · 最近 {}", next.format("%Y-%m-%d"))
        } else {
            "所有信用卡提醒已暂停".to_owned()
        }
        .into(),
    );
    true
}

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .map_err(|_| anyhow::anyhow!("时间格式应为 HH:mm"))
}

fn parse_due_day(value: &str) -> Result<u32> {
    let value: u32 = value.trim().parse().context("还款日必须是 1–31 的整数")?;
    if !(1..=31).contains(&value) {
        anyhow::bail!("还款日必须是 1–31");
    }
    Ok(value)
}

fn parse_lead_days(value: &str) -> Result<Vec<u32>> {
    let mut values = Vec::new();
    for item in value.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let days: u32 = item.parse().context("提前天数必须是整数")?;
        values.push(days);
    }
    normalize_lead_days(values)
}

pub(crate) fn wire_finance_callbacks(ui: &MainWindow, runtime: RuntimeHandle) {
    let ui_save_repo = ui.as_weak();
    let runtime_save_repo = runtime.clone();
    ui.on_save_repo_settings(
        move |enabled, primary_enabled, primary_time, fallback_enabled, fallback_time| {
            let Some(ui) = ui_save_repo.upgrade() else {
                return;
            };
            let result = (|| -> Result<()> {
                let primary = primary_enabled
                    .then(|| parse_time(&primary_time))
                    .transpose()?;
                let fallback = fallback_enabled
                    .then(|| parse_time(&fallback_time))
                    .transpose()?;
                let settings = RepoSettings {
                    enabled: enabled && (primary.is_some() || fallback.is_some()),
                    primary,
                    fallback,
                    rules_changed_at: crate::core::now_china(),
                    updated_at: crate::core::now_china(),
                };
                runtime_save_repo.save_repo_settings(&settings)
            })();
            match result {
                Ok(()) => {
                    ui.set_repo_error("".into());
                    // 保存成功后表单与数据库一致，解除「已修改」标记并让刷新回填一次。
                    ui.set_repo_form_dirty(false);
                    refresh_finance_ui(&ui, &runtime_save_repo);
                }
                Err(error) => ui.set_repo_error(format!("{error:#}").into()),
            }
        },
    );

    let ui_ack = ui.as_weak();
    let runtime_ack = runtime.clone();
    ui.on_set_repo_acked(move |acked| {
        let Some(ui) = ui_ack.upgrade() else {
            return;
        };
        let result = runtime_ack.set_repo_day_acked(crate::core::now_china().date_naive(), acked);
        match result {
            Ok(()) => {
                ui.set_repo_error("".into());
                refresh_finance_ui(&ui, &runtime_ack);
            }
            Err(error) => ui.set_repo_error(format!("{error:#}").into()),
        }
    });

    let ui_save_card = ui.as_weak();
    let runtime_save_card = runtime.clone();
    ui.on_save_credit_card(
        move |id, alias, bank, due_day, remind_time, lead_days, overdue_enabled, enabled| {
            let Some(ui) = ui_save_card.upgrade() else {
                return;
            };
            let result = (|| -> Result<()> {
                let now = crate::core::now_china();
                let due_day = parse_due_day(&due_day)?;
                let remind_time = parse_time(&remind_time)?;
                let lead_days = parse_lead_days(&lead_days)?;
                let existing = if id.trim().is_empty() {
                    None
                } else {
                    // 编辑的卡若已被删掉，必须报错而不是继续往下走：后面会以这个 id 落库，
                    // 等于把刚删掉的卡（连同一个旧 uuid）重新造出来。
                    let found = runtime_save_card
                        .credit_cards()?
                        .into_iter()
                        .find(|card| card.id == id.as_str());
                    Some(found.ok_or_else(|| {
                        anyhow::anyhow!("这张信用卡已不存在，请重新选择或点「新增」")
                    })?)
                };
                let card = CreditCard {
                    id: if id.trim().is_empty() {
                        uuid::Uuid::new_v4().simple().to_string()
                    } else {
                        id.to_string()
                    },
                    alias: alias.trim().to_owned(),
                    bank: (!bank.trim().is_empty()).then(|| bank.trim().to_owned()),
                    due_day,
                    remind_time,
                    lead_days,
                    overdue_enabled,
                    enabled,
                    first_tracked_cycle: existing
                        .as_ref()
                        .map(|card| card.first_tracked_cycle.clone())
                        .unwrap_or_else(|| first_tracked_cycle(now.date_naive(), due_day)),
                    rules_changed_at: existing
                        .as_ref()
                        .map(|card| card.rules_changed_at)
                        .unwrap_or(now),
                    created_at: existing.as_ref().map(|card| card.created_at).unwrap_or(now),
                    updated_at: now,
                };
                validate_card(&card)?;
                runtime_save_card.save_credit_card(&card)?;
                let _ = due_on_for_cycle(&card.first_tracked_cycle, card.due_day)?;
                Ok(())
            })();
            match result {
                Ok(()) => {
                    ui.set_credit_error("".into());
                    clear_credit_editor(&ui);
                    refresh_finance_ui(&ui, &runtime_save_card);
                }
                Err(error) => ui.set_credit_error(format!("{error:#}").into()),
            }
        },
    );

    let ui_delete = ui.as_weak();
    let runtime_delete = runtime.clone();
    ui.on_delete_credit_card(move |id| {
        let Some(ui) = ui_delete.upgrade() else {
            return;
        };
        match runtime_delete.delete_credit_card(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                // 只在被删的正是编辑中的那张卡时清空表单：删别的卡不应丢掉正在填的内容。
                if editor_holds_card(ui.get_credit_form_id().as_str(), id.as_str()) {
                    clear_credit_editor(&ui);
                }
                refresh_finance_ui(&ui, &runtime_delete);
            }
            Err(error) => ui.set_credit_error(format!("{error:#}").into()),
        }
    });

    let ui_edit = ui.as_weak();
    let runtime_edit = runtime.clone();
    ui.on_edit_credit_card(move |id| {
        let Some(ui) = ui_edit.upgrade() else {
            return;
        };
        let Ok(cards) = runtime_edit.credit_cards() else {
            ui.set_credit_error("读取信用卡列表失败，请稍后重试".into());
            return;
        };
        let Some(card) = cards.into_iter().find(|card| card.id == id.as_str()) else {
            // 卡片已被删除时不能静默什么都不做，否则点击「编辑」看起来像失效。
            ui.set_credit_error("这张信用卡已不存在，请刷新后重试".into());
            refresh_finance_ui(&ui, &runtime_edit);
            return;
        };
        ui.set_credit_form_id(card.id.into());
        ui.set_credit_form_alias(card.alias.into());
        ui.set_credit_form_bank(card.bank.unwrap_or_default().into());
        ui.set_credit_form_due_day(card.due_day.to_string().into());
        ui.set_credit_form_remind_time(card.remind_time.format("%H:%M").to_string().into());
        ui.set_credit_form_lead_days(
            card.lead_days
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        );
        ui.set_credit_form_overdue(card.overdue_enabled);
        ui.set_credit_form_enabled(card.enabled);
        ui.set_credit_form_editing(true);
        ui.set_credit_error("".into());
    });

    let ui_clear = ui.as_weak();
    ui.on_clear_credit_editor(move || {
        if let Some(ui) = ui_clear.upgrade() {
            clear_credit_editor(&ui);
        }
    });

    let ui_paid = ui.as_weak();
    let runtime_paid = runtime.clone();
    ui.on_mark_credit_paid(move |id, cycle| {
        let Some(ui) = ui_paid.upgrade() else {
            return;
        };
        match runtime_paid.mark_credit_card_paid(&id, &cycle) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_paid);
            }
            Err(error) => ui.set_credit_error(format!("{error:#}").into()),
        }
    });

    let ui_undo = ui.as_weak();
    let runtime_undo = runtime;
    ui.on_undo_credit_action(move |id| {
        let Some(ui) = ui_undo.upgrade() else {
            return;
        };
        match runtime_undo.undo_credit_card_action(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_undo);
            }
            Err(error) => ui.set_credit_error(format!("{error:#}").into()),
        }
    });
}

/// 理财 Toast 的点击目标分类。
///
/// `runtime_bridge::finance_target` 会发出 `finance:repo` / `finance:card:<id>`，
/// 通知回调必须先把它们认出来，否则会被当成 IPO 事件 ID 去查详情，报「任务已不存在」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinanceToastTarget {
    ReverseRepo,
    CreditCard,
}

pub(crate) fn finance_toast_target(event_id: &str) -> Option<FinanceToastTarget> {
    if event_id == "finance:repo" {
        return Some(FinanceToastTarget::ReverseRepo);
    }
    if event_id == "finance:card" || event_id.starts_with("finance:card:") {
        return Some(FinanceToastTarget::CreditCard);
    }
    None
}

/// 逆回购「今日已处理」实际写入的日期。
///
/// 提醒窗口可以跨日保留，动作必须落在提醒自己的 `event_date` 上；用点击当天会出现
/// 「周五的弹窗周一才点，把周一整天标成已处理」的漏提醒。日期无法识别或落在未来时
/// 一律拒绝，宁可不动状态。
pub(crate) fn repo_ack_date(event_date: &str, today: NaiveDate) -> Result<NaiveDate> {
    let date = NaiveDate::parse_from_str(event_date.trim(), "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("提醒日期无法识别，未修改已处理状态"))?;
    if date > today {
        anyhow::bail!("提醒日期在未来，未修改已处理状态");
    }
    Ok(date)
}

/// 表单里正在编辑的是否就是这张卡。
///
/// 删除卡片后只应在命中编辑目标时清空表单：删的是别的卡时保留用户正在填的内容，
/// 删的正是编辑中的卡时清空，避免用残留 id 再保存一次把卡片「复活」。
pub(crate) fn editor_holds_card(form_id: &str, card_id: &str) -> bool {
    !form_id.trim().is_empty() && form_id == card_id
}

pub(crate) fn clear_credit_editor(ui: &MainWindow) {
    ui.set_credit_form_id("".into());
    ui.set_credit_form_alias("".into());
    ui.set_credit_form_bank("".into());
    ui.set_credit_form_due_day("5".into());
    ui.set_credit_form_remind_time("09:00".into());
    ui.set_credit_form_lead_days("3,0".into());
    ui.set_credit_form_overdue(true);
    ui.set_credit_form_enabled(true);
    ui.set_credit_form_editing(false);
    ui.set_credit_error("".into());
}

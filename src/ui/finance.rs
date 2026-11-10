use chrono::{NaiveDate, NaiveTime};
use slint::SharedString;

use super::*;
use crate::finance::{
    BANK_REGISTRY, PaymentAccount, PaymentAccountKind, RepoSettings, bank_definition, bank_icon,
    due_on_for_cycle, first_tracked_cycle, normalize_lead_days, parse_cycle, status_for_account,
    undo_window_open, validate_payment_account,
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

    let accounts = match runtime.payment_accounts() {
        Ok(value) => value,
        Err(error) => {
            ui.set_credit_error(format!("读取还款账户失败：{error:#}").into());
            ui.set_status_text(format!("读取还款账户失败：{error:#}").into());
            return false;
        }
    };
    install_form_models(ui, editing_unknown_bank(ui, &accounts));

    let mut entries = Vec::new();
    let mut pending = 0usize;
    let mut overdue = 0usize;
    let mut next: Option<NaiveDate> = None;
    for account in &accounts {
        let settled = runtime.payment_account_settled(&account.id).unwrap_or(None);
        let status = status_for_account(account, settled.as_deref(), today);
        let active = account.enabled && account.closed_at.is_none();
        if active {
            pending = pending.saturating_add(1);
            if status.overdue_days > 0 {
                overdue = overdue.saturating_add(1);
            }
            if let Some(due) = status.next_due
                && next.is_none_or(|current| due < current)
            {
                next = Some(due);
            }
        }
        let can_pay = active && status.next_cycle.is_some();
        let can_undo = undo_window_open(settled.as_deref(), today);
        let closed = account.closed_at.is_some();
        let action_rows = if account.kind.is_loan() && !closed {
            3
        } else if can_pay || can_undo || closed {
            2
        } else {
            1
        };
        let final_cycle_text = if account.kind.is_loan() {
            account
                .final_cycle
                .as_deref()
                .map(|cycle| format!("结束还款月 {cycle}（含）"))
                .unwrap_or_else(|| "未设置结束月".to_owned())
        } else {
            String::new()
        };
        entries.push((
            status.next_due,
            closed,
            CreditCardRow {
                id: account.id.clone().into(),
                alias: account.alias.clone().into(),
                account_kind: account.kind.label().into(),
                bank: account.bank_display_name().into(),
                bank_icon: bank_icon(account.bank_id.as_deref()),
                due_text: status
                    .next_due
                    .map(|due| due.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "无待还款日期".to_owned())
                    .into(),
                status_text: status.label.clone().into(),
                status_tone: status.tone,
                lead_text: format!(
                    "提前 {}",
                    account
                        .lead_days
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
                    account.remind_time.format("%H:%M"),
                    if account.overdue_enabled {
                        " · 逾期次日再提醒"
                    } else {
                        ""
                    }
                )
                .into(),
                final_cycle_text: final_cycle_text.into(),
                cycle_key: status.next_cycle.unwrap_or_default().into(),
                enabled: account.enabled,
                can_pay,
                can_undo,
                is_loan: account.kind.is_loan(),
                closed,
                action_rows,
            },
        ));
    }
    entries.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.2.alias.cmp(&right.2.alias))
    });
    let rows = entries
        .into_iter()
        .map(|(_, _, row)| row)
        .collect::<Vec<_>>();
    ui.set_credit_cards(ModelRc::from(Rc::new(VecModel::from(rows))));
    ui.set_credit_summary(
        if accounts.is_empty() {
            "暂无还款提醒".to_owned()
        } else if overdue > 0 {
            format!("{overdue} 项已逾期 · 共 {pending} 项启用")
        } else if let Some(next) = next {
            format!("{pending} 项启用 · 最近还款 {}", next.format("%Y-%m-%d"))
        } else {
            "所有还款提醒已暂停或结束".to_owned()
        }
        .into(),
    );
    true
}

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M")
        .map_err(|_| anyhow::anyhow!("时间格式应为 HH:mm"))
}

fn lead_mode(days: &[u32]) -> i32 {
    match days {
        [0] => 0,
        [1, 0] => 1,
        [3, 0] => 2,
        [7, 3, 0] => 3,
        _ => 4,
    }
}

fn parse_lead_days(mode: i32, custom: &str) -> Result<Vec<u32>> {
    let raw = match mode {
        0 => "0".to_owned(),
        1 => "1,0".to_owned(),
        2 => "3,0".to_owned(),
        3 => "7,3,0".to_owned(),
        4 => custom.to_owned(),
        _ => anyhow::bail!("提前提醒预设无效"),
    };
    let mut values = Vec::new();
    for item in raw.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        values.push(item.parse::<u32>().context("提前天数必须是整数")?);
    }
    normalize_lead_days(values)
}

fn account_kind(index: i32) -> Result<PaymentAccountKind> {
    match index {
        0 => Ok(PaymentAccountKind::CreditCard),
        1 => Ok(PaymentAccountKind::Mortgage),
        2 => Ok(PaymentAccountKind::AutoLoan),
        _ => anyhow::bail!("还款账户类型无效"),
    }
}

fn model_strings(values: Vec<String>) -> ModelRc<SharedString> {
    ModelRc::from(Rc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )))
}

fn unknown_bank(account: &PaymentAccount) -> Option<(String, String)> {
    let id = account.bank_id.as_deref()?;
    if id == "other" || bank_definition(id).is_some() {
        None
    } else {
        Some((
            id.to_owned(),
            format!("未知银行（{}，保留）", account.bank_display_name()),
        ))
    }
}

fn editing_unknown_bank(ui: &MainWindow, accounts: &[PaymentAccount]) -> Option<(String, String)> {
    let id = ui.get_credit_form_id();
    accounts
        .iter()
        .find(|account| account.id == id.as_str())
        .and_then(unknown_bank)
}

fn install_form_models(ui: &MainWindow, unknown: Option<(String, String)>) {
    let mut labels = vec!["未指定银行".to_owned()];
    let mut ids = vec![String::new()];
    labels.extend(
        BANK_REGISTRY
            .iter()
            .map(|bank| bank.display_name.to_owned()),
    );
    ids.extend(BANK_REGISTRY.iter().map(|bank| bank.id.to_owned()));
    if let Some((id, label)) = unknown {
        ids.push(id);
        labels.push(label);
    }
    ui.set_credit_bank_options(model_strings(labels));
    ui.set_credit_bank_ids(model_strings(ids));
    ui.set_credit_due_day_options(model_strings((1..=31).map(|day| day.to_string()).collect()));
}

fn bank_index_for(ui: &MainWindow, account: Option<&PaymentAccount>) -> i32 {
    let Some(account) = account else {
        return 0;
    };
    let Some(id) = account.bank_id.as_deref() else {
        return 0;
    };
    ui.get_credit_bank_ids()
        .iter()
        .position(|candidate| candidate.as_str() == id)
        .unwrap_or(0) as i32
}

fn selected_bank_id(ui: &MainWindow, index: i32) -> Option<String> {
    ui.get_credit_bank_ids()
        .row_data(usize::try_from(index).ok()?)
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
}

fn update_form_bank_icon(ui: &MainWindow) {
    let bank_id = selected_bank_id(ui, ui.get_credit_form_bank_index());
    ui.set_credit_form_bank_icon(bank_icon(bank_id.as_deref()));
    ui.set_credit_form_bank_custom(bank_id.as_deref() == Some("other"));
}

fn update_form_preview(ui: &MainWindow, runtime: &RuntimeHandle) {
    update_form_bank_icon(ui);
    let preview = (|| -> Result<String> {
        let due_day = (ui.get_credit_form_due_day_index() + 1) as u32;
        let final_cycle = ui.get_credit_form_final_cycle();
        let final_cycle = final_cycle.trim();
        if account_kind(ui.get_credit_form_kind_index())?.is_loan() && !final_cycle.is_empty() {
            parse_cycle(final_cycle).context("结束还款月格式必须是 YYYY-MM")?;
        }
        let form_id = ui.get_credit_form_id();
        let existing = (!form_id.trim().is_empty())
            .then(|| {
                runtime
                    .payment_accounts()
                    .ok()?
                    .into_iter()
                    .find(|account| account.id == form_id.as_str())
            })
            .flatten();
        let first = existing
            .as_ref()
            .map(|account| account.first_tracked_cycle.clone())
            .unwrap_or_else(|| first_tracked_cycle(crate::core::now_china().date_naive(), due_day));
        let due = due_on_for_cycle(&first, due_day)?;
        Ok(format!(
            "下一期预览：{} 账期，实际还款日 {}",
            first,
            due.format("%Y-%m-%d")
        ))
    })();
    ui.set_credit_form_preview(match preview {
        Ok(value) => value.into(),
        Err(_) => "".into(),
    });
}

fn account_from_form(
    ui: &MainWindow,
    runtime: &RuntimeHandle,
    id: &str,
    kind_index: i32,
    alias: &str,
    bank_index: i32,
    custom_bank_name: &str,
    due_day_index: i32,
    remind_time: &str,
    lead_mode_index: i32,
    custom_lead_days: &str,
    final_cycle: &str,
    overdue_enabled: bool,
    enabled: bool,
) -> Result<PaymentAccount> {
    let now = crate::core::now_china();
    let existing =
        if id.trim().is_empty() {
            None
        } else {
            let found = runtime
                .payment_accounts()?
                .into_iter()
                .find(|account| account.id == id);
            Some(found.ok_or_else(|| {
                anyhow::anyhow!("这个还款账户已不存在，请重新选择或点「新增还款」")
            })?)
        };
    let kind = if let Some(existing) = existing.as_ref() {
        existing.kind
    } else {
        account_kind(kind_index)?
    };
    let bank_id = if bank_index == 0 {
        None
    } else {
        Some(selected_bank_id(ui, bank_index).context("银行选项已变化，请重新选择后再保存")?)
    };
    let bank_name = match bank_id.as_deref() {
        None => None,
        Some("other") => Some(custom_bank_name.trim().to_owned()),
        Some(id) if bank_definition(id).is_some() => {
            bank_definition(id).map(|bank| bank.display_name.to_owned())
        }
        Some(_) => existing
            .as_ref()
            .and_then(|account| account.bank_name.clone()),
    };
    let due_day = (due_day_index + 1) as u32;
    let account = PaymentAccount {
        id: if id.trim().is_empty() {
            uuid::Uuid::new_v4().simple().to_string()
        } else {
            id.to_owned()
        },
        kind,
        alias: alias.trim().to_owned(),
        bank_id,
        bank_name,
        due_day,
        remind_time: parse_time(remind_time)?,
        lead_days: parse_lead_days(lead_mode_index, custom_lead_days)?,
        overdue_enabled,
        enabled,
        first_tracked_cycle: existing
            .as_ref()
            .map(|account| account.first_tracked_cycle.clone())
            .unwrap_or_else(|| first_tracked_cycle(now.date_naive(), due_day)),
        final_cycle: if kind.is_loan() {
            let final_cycle = final_cycle.trim();
            (!final_cycle.is_empty()).then(|| final_cycle.to_owned())
        } else {
            None
        },
        closed_at: existing.as_ref().and_then(|account| account.closed_at),
        rules_changed_at: existing
            .as_ref()
            .map(|account| account.rules_changed_at)
            .unwrap_or(now),
        created_at: existing
            .as_ref()
            .map(|account| account.created_at)
            .unwrap_or(now),
        updated_at: now,
    };
    validate_payment_account(&account)?;
    Ok(account)
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

    let ui_preview = ui.as_weak();
    let runtime_preview = runtime.clone();
    ui.on_preview_credit_form(move || {
        if let Some(ui) = ui_preview.upgrade() {
            update_form_preview(&ui, &runtime_preview);
        }
    });

    let ui_save_account = ui.as_weak();
    let runtime_save_account = runtime.clone();
    ui.on_save_credit_card(
        move |id,
              kind_index,
              alias,
              bank_index,
              custom_bank_name,
              due_day_index,
              remind_time,
              lead_mode_index,
              custom_lead_days,
              final_cycle,
              overdue_enabled,
              enabled| {
            let Some(ui) = ui_save_account.upgrade() else {
                return;
            };
            let result = account_from_form(
                &ui,
                &runtime_save_account,
                &id,
                kind_index,
                &alias,
                bank_index,
                &custom_bank_name,
                due_day_index,
                &remind_time,
                lead_mode_index,
                &custom_lead_days,
                &final_cycle,
                overdue_enabled,
                enabled,
            )
            .and_then(|account| runtime_save_account.save_payment_account(&account));
            match result {
                Ok(()) => {
                    ui.set_credit_error("".into());
                    clear_credit_editor(&ui);
                    refresh_finance_ui(&ui, &runtime_save_account);
                }
                Err(error) => {
                    ui.set_credit_error(format!("{error:#}").into());
                    update_form_preview(&ui, &runtime_save_account);
                }
            }
        },
    );

    let ui_delete = ui.as_weak();
    let runtime_delete = runtime.clone();
    ui.on_delete_credit_card(move |id| {
        let Some(ui) = ui_delete.upgrade() else {
            return;
        };
        match runtime_delete.delete_payment_account(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                if editor_holds_card(ui.get_credit_form_id().as_str(), id.as_str()) {
                    clear_credit_editor(&ui);
                }
                refresh_finance_ui(&ui, &runtime_delete);
            }
            Err(error) => {
                ui.set_credit_error(format!("{error:#}").into());
                ui.set_status_text(format!("删除还款账户失败：{error:#}").into());
            }
        }
    });

    let ui_edit = ui.as_weak();
    let runtime_edit = runtime.clone();
    ui.on_edit_credit_card(move |id| {
        let Some(ui) = ui_edit.upgrade() else {
            return;
        };
        let Ok(accounts) = runtime_edit.payment_accounts() else {
            ui.set_credit_error("读取还款账户列表失败，请稍后重试".into());
            ui.set_status_text("读取还款账户列表失败，请稍后重试".into());
            return;
        };
        let Some(account) = accounts.iter().find(|account| account.id == id.as_str()) else {
            ui.set_credit_error("这个还款账户已不存在，请刷新后重试".into());
            ui.set_status_text("这个还款账户已不存在，请刷新后重试".into());
            refresh_finance_ui(&ui, &runtime_edit);
            return;
        };
        install_form_models(&ui, unknown_bank(account));
        ui.set_credit_form_id(account.id.clone().into());
        ui.set_credit_form_kind_index(match account.kind {
            PaymentAccountKind::CreditCard => 0,
            PaymentAccountKind::Mortgage => 1,
            PaymentAccountKind::AutoLoan => 2,
        });
        ui.set_credit_form_alias(account.alias.clone().into());
        let bank_index = bank_index_for(&ui, Some(account));
        ui.set_credit_form_bank_index(bank_index);
        ui.set_credit_form_bank_name(account.bank_name.clone().unwrap_or_default().into());
        ui.set_credit_form_due_day_index(account.due_day.saturating_sub(1) as i32);
        ui.set_credit_form_remind_time(account.remind_time.format("%H:%M").to_string().into());
        ui.set_credit_form_lead_mode_index(lead_mode(&account.lead_days));
        ui.set_credit_form_lead_days(
            account
                .lead_days
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
                .into(),
        );
        ui.set_credit_form_final_cycle(account.final_cycle.clone().unwrap_or_default().into());
        ui.set_credit_form_overdue(account.overdue_enabled);
        ui.set_credit_form_enabled(account.enabled);
        ui.set_credit_form_editing(true);
        ui.set_credit_form_visible(true);
        ui.set_credit_highlight_id(account.id.clone().into());
        ui.set_credit_error("".into());
        update_form_preview(&ui, &runtime_edit);
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
        match runtime_paid.mark_payment_account_paid(&id, &cycle) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_paid);
            }
            Err(error) => {
                ui.set_credit_error(format!("{error:#}").into());
                ui.set_status_text(format!("标记本期已还失败：{error:#}").into());
            }
        }
    });

    let ui_undo = ui.as_weak();
    let runtime_undo = runtime.clone();
    ui.on_undo_credit_action(move |id| {
        let Some(ui) = ui_undo.upgrade() else {
            return;
        };
        match runtime_undo.undo_payment_account_action(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_undo);
            }
            Err(error) => {
                ui.set_credit_error(format!("{error:#}").into());
                ui.set_status_text(format!("撤销已还失败：{error:#}").into());
            }
        }
    });

    let ui_close = ui.as_weak();
    let runtime_close = runtime.clone();
    ui.on_close_credit_account(move |id| {
        let Some(ui) = ui_close.upgrade() else {
            return;
        };
        match runtime_close.close_payment_account(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_close);
            }
            Err(error) => {
                ui.set_credit_error(format!("{error:#}").into());
                ui.set_status_text(format!("标记已结清失败：{error:#}").into());
            }
        }
    });

    let ui_resume = ui.as_weak();
    let runtime_resume = runtime;
    ui.on_resume_credit_account(move |id| {
        let Some(ui) = ui_resume.upgrade() else {
            return;
        };
        match runtime_resume.resume_payment_account(&id) {
            Ok(()) => {
                ui.set_credit_error("".into());
                refresh_finance_ui(&ui, &runtime_resume);
            }
            Err(error) => {
                ui.set_credit_error(format!("{error:#}").into());
                ui.set_status_text(format!("恢复还款账户失败：{error:#}").into());
            }
        }
    });
}

/// 理财 Toast 的点击目标分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinanceToastTarget {
    ReverseRepo,
    PaymentAccount { account_id: Option<String> },
}

pub(crate) fn finance_toast_target(event_id: &str) -> Option<FinanceToastTarget> {
    if event_id == "finance:repo" {
        return Some(FinanceToastTarget::ReverseRepo);
    }
    if let Some(id) = event_id.strip_prefix("finance:payment:") {
        return Some(FinanceToastTarget::PaymentAccount {
            account_id: (!id.is_empty()).then(|| id.to_owned()),
        });
    }
    if event_id == "finance:payment" || event_id == "finance:card" {
        return Some(FinanceToastTarget::PaymentAccount { account_id: None });
    }
    if let Some(id) = event_id.strip_prefix("finance:card:") {
        return Some(FinanceToastTarget::PaymentAccount {
            account_id: (!id.is_empty()).then(|| id.to_owned()),
        });
    }
    None
}

/// 逆回购「今日已处理」实际写入的日期。
pub(crate) fn repo_ack_date(event_date: &str, today: NaiveDate) -> Result<NaiveDate> {
    let date = NaiveDate::parse_from_str(event_date.trim(), "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("提醒日期无法识别，未修改已处理状态"))?;
    if date > today {
        anyhow::bail!("提醒日期在未来，未修改已处理状态");
    }
    Ok(date)
}

/// 表单里正在编辑的是否就是这个账户。
pub(crate) fn editor_holds_card(form_id: &str, account_id: &str) -> bool {
    !form_id.trim().is_empty() && form_id == account_id
}

pub(crate) fn clear_credit_editor(ui: &MainWindow) {
    install_form_models(ui, None);
    ui.set_credit_form_id("".into());
    ui.set_credit_form_kind_index(0);
    ui.set_credit_form_alias("".into());
    ui.set_credit_form_bank_index(0);
    ui.set_credit_form_bank_icon(bank_icon(None));
    ui.set_credit_form_bank_custom(false);
    ui.set_credit_form_bank_name("".into());
    ui.set_credit_form_due_day_index(4);
    ui.set_credit_form_remind_time("09:00".into());
    ui.set_credit_form_lead_mode_index(2);
    ui.set_credit_form_lead_days("3,0".into());
    ui.set_credit_form_final_cycle("".into());
    ui.set_credit_form_overdue(true);
    ui.set_credit_form_enabled(true);
    ui.set_credit_form_editing(false);
    ui.set_credit_form_visible(false);
    ui.set_credit_form_preview("".into());
    ui.set_credit_error("".into());
}

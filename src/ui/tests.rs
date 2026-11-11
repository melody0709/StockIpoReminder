use chrono::NaiveDate;

use super::*;

fn filter_event(exchange: Exchange, status: LifecycleStatus) -> IpoEvent {
    let now = crate::core::now_china();
    IpoEvent {
        id: "fixture-filter".into(),
        exchange,
        board: Board::Main,
        security_code: "601001".into(),
        apply_code: Some("780001".into()),
        legacy_code: Some("730001".into()),
        name: "筛选测试股份".into(),
        apply_date: Some(now.date_naive()),
        issue_price: None,
        lot_size: None,
        max_apply_quantity: None,
        required_market_value: None,
        required_cash: None,
        ballot_date: None,
        payment_date: None,
        listing_date: None,
        status: model::IssueStatus::Active,
        lifecycle_status: status,
        event_version: 1,
        announcement_url: None,
        data_quality_status: DataQualityStatus::MultiSourceVerified,
        data_conflict: false,
        manual_override_fields: Vec::new(),
        sessions: Vec::new(),
        first_seen_at: now,
        updated_at: now,
    }
}

#[test]
fn reminder_alerts_follow_settings() {
    let mut settings = AppSettings::default();
    let alerts = ReminderAlerts::from_settings(&settings);
    assert!(alerts.sound && alerts.toast);

    settings.sound_enabled = false;
    settings.toast_enabled = false;
    let alerts = ReminderAlerts::from_settings(&settings);
    assert!(!alerts.sound && !alerts.toast);
}

#[test]
fn reminder_alerts_fail_closed_without_settings() {
    let alerts = ReminderAlerts::fail_closed();
    assert_eq!(alerts, ReminderAlerts::default());
    assert!(!alerts.sound && !alerts.toast);
}

#[test]
fn acknowledgement_is_available_only_on_the_apply_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();

    assert!(can_acknowledge_on(
        Some(today),
        LifecycleStatus::Scheduled,
        today,
    ));
    assert!(!can_acknowledge_on(
        Some(today + chrono::Duration::days(1)),
        LifecycleStatus::Scheduled,
        today,
    ));
    assert!(!can_acknowledge_on(
        Some(today),
        LifecycleStatus::ExpiredUnconfirmed,
        today,
    ));
}

#[test]
fn sync_interval_parser_supports_minutes_and_hours() {
    assert_eq!(
        parse_sync_interval("30", 0, "普通日期自动同步间隔").unwrap(),
        30
    );
    assert_eq!(
        parse_sync_interval("2", 1, "普通日期自动同步间隔").unwrap(),
        120
    );
    assert!(parse_sync_interval("4", 0, "申购日自动同步间隔").is_err());
    assert_eq!(sync_interval_display(120), ("2".into(), 1));
    assert_eq!(sync_interval_display(20), ("20".into(), 0));
}

#[test]
fn settings_reset_uses_all_application_defaults() {
    let stored = AppSettings {
        active_day_sync_minutes: 5,
        normal_sync_minutes: 120,
        sound_enabled: false,
        notification_self_test_completed: true,
        ..AppSettings::default()
    };

    assert_eq!(
        settings_base_for_save(stored.clone(), false).normal_sync_minutes,
        120
    );
    assert_eq!(settings_base_for_save(stored, true).normal_sync_minutes, 30);

    let defaults = settings_base_for_save(AppSettings::default(), true);
    assert_eq!(defaults.active_day_sync_minutes, 20);
    assert!(defaults.sound_enabled);
    assert!(!defaults.notification_self_test_completed);
}

#[test]
fn notification_self_test_requires_each_enabled_channel() {
    let mut settings = AppSettings::default();
    settings.notification_window_test_passed = Some(true);
    settings.notification_balloon_test_passed = Some(true);
    settings.notification_sound_test_passed = Some(true);
    assert!(settings.notification_tests_complete());

    settings.notification_balloon_test_passed = Some(false);
    settings.notification_toast_test_passed = Some(true);
    assert!(settings.notification_tests_complete());

    settings.notification_toast_test_passed = Some(false);
    settings.toast_enabled = false;
    assert!(settings.notification_tests_complete());
}

#[test]
fn legacy_taskbar_settings_are_ignored_without_blocking_notification_setup() {
    let mut settings = AppSettings::default();
    settings.notification_window_test_passed = Some(true);
    settings.notification_balloon_test_passed = Some(true);
    settings.notification_sound_test_passed = Some(true);
    let mut json = serde_json::to_value(settings).unwrap();
    let object = json.as_object_mut().unwrap();
    object.insert("flashTaskbar".into(), true.into());
    object.insert("notificationFlashTestPassed".into(), true.into());

    let restored: AppSettings = serde_json::from_value(json).unwrap();
    assert!(restored.notification_tests_complete());
    let saved = serde_json::to_value(restored).unwrap();
    assert!(saved.get("flashTaskbar").is_none());
    assert!(saved.get("notificationFlashTestPassed").is_none());
}

#[test]
fn task_filter_combines_text_market_and_status() {
    let event = filter_event(Exchange::Shanghai, LifecycleStatus::ActiveUnconfirmed);
    assert!(task_matches_filter(&event, "测试", 0, 0));
    assert!(task_matches_filter(&event, "780001", 1, 1));
    assert!(task_matches_filter(&event, "730001", 0, 0));
    assert!(!task_matches_filter(&event, "不存在", 0, 0));
    assert!(!task_matches_filter(&event, "", 2, 0));
    assert!(!task_matches_filter(&event, "", 0, 2));

    let mut review = event;
    review.data_quality_status = DataQualityStatus::ManualReviewRequired;
    assert!(task_matches_filter(&review, "人工核验", 1, 3));
}

#[test]
fn task_filter_handles_thousands_of_fixed_rows_without_losing_matches() {
    let events = (0..2_000)
        .map(|index| {
            let mut event = filter_event(
                if index % 3 == 0 {
                    Exchange::Shanghai
                } else if index % 3 == 1 {
                    Exchange::Shenzhen
                } else {
                    Exchange::Beijing
                },
                LifecycleStatus::ActiveUnconfirmed,
            );
            event.id = format!("fixture-filter-{index}");
            event.security_code = format!("{:06}", 600_000 + index);
            event.apply_code = Some(format!("{:06}", 700_000 + index));
            event.name = format!("固定压力样本{index}");
            event
        })
        .collect::<Vec<_>>();

    let exact = events
        .iter()
        .filter(|event| task_matches_filter(event, "固定压力样本1999", 0, 0))
        .count();
    assert_eq!(exact, 1);
    let shanghai = events
        .iter()
        .filter(|event| task_matches_filter(event, "固定压力样本", 1, 1))
        .count();
    assert_eq!(shanghai, 667);
}

#[test]
fn final_reminder_wins_over_data_changed_for_the_same_event() {
    let event = filter_event(Exchange::Shanghai, LifecycleStatus::ActiveUnconfirmed);
    let now = crate::core::now_china();
    let deliveries = vec![
        ReminderDelivery {
            outbox_id: 1,
            event: event.clone(),
            due_at: now,
            level: ReminderLevel::Final,
            dedupe_key: "final".into(),
            attempt_count: 1,
            message: None,
        },
        ReminderDelivery {
            outbox_id: 2,
            event,
            due_at: now,
            level: ReminderLevel::DataChanged,
            dedupe_key: "changed".into(),
            attempt_count: 1,
            message: Some("发行信息变化".into()),
        },
    ];
    let batch = reminder_batch(&deliveries, None);
    assert!(
        reminder_display_priority(ReminderLevel::Final)
            > reminder_display_priority(ReminderLevel::DataChanged)
    );
    assert!(
        batch
            .body
            .contains(reminder_level_text(ReminderLevel::Final))
    );
    assert!(
        !batch
            .body
            .contains(reminder_level_text(ReminderLevel::DataChanged))
    );
}

#[test]
fn operation_gate_allows_only_one_owner_and_releases_on_drop() {
    let flag = Arc::new(AtomicBool::new(false));
    let first = OperationGate::acquire(Arc::clone(&flag)).unwrap();
    assert!(OperationGate::acquire(Arc::clone(&flag)).is_none());
    drop(first);
    assert!(OperationGate::acquire(flag).is_some());
}

#[test]
fn editor_is_cleared_only_when_the_deleted_card_is_the_edit_target() {
    // 删的正是编辑中的卡：清空表单，避免残留 id 再保存把卡片复活。
    assert!(editor_holds_card("card-a", "card-a"));
    // 删的是别的卡：保留正在填的内容。
    assert!(!editor_holds_card("card-a", "card-b"));
    // 新增模式（表单没有 id）任何时候都不该被清空。
    assert!(!editor_holds_card("", "card-a"));
    assert!(!editor_holds_card("   ", "card-a"));
}

#[test]
fn repo_ack_uses_the_reminder_date_not_the_click_date() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
    // 周五的弹窗拖到周一才点：写入的必须是周五，不能把周一整天标成已处理。
    assert_eq!(
        repo_ack_date("2026-10-30", today).unwrap(),
        NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
    );
    // 当天正常点击。
    assert_eq!(repo_ack_date("2026-11-02", today).unwrap(), today);
    // 未来日期与无法识别的日期一律拒绝，宁可不动状态。
    assert!(repo_ack_date("2026-11-03", today).is_err());
    assert!(repo_ack_date("", today).is_err());
    assert!(repo_ack_date("2026/11/02", today).is_err());
}

#[test]
fn finance_toast_targets_are_classified_before_ipo_lookup() {
    assert_eq!(
        finance_toast_target("finance:repo"),
        Some(FinanceToastTarget::ReverseRepo)
    );
    assert_eq!(
        finance_toast_target("finance:card:0d1f"),
        Some(FinanceToastTarget::PaymentAccount {
            account_id: Some("0d1f".into())
        })
    );
    assert_eq!(
        finance_toast_target("finance:payment:0d1f"),
        Some(FinanceToastTarget::PaymentAccount {
            account_id: Some("0d1f".into())
        })
    );
    assert_eq!(
        finance_toast_target("finance:payment"),
        Some(FinanceToastTarget::PaymentAccount { account_id: None })
    );
    assert_eq!(
        finance_toast_target("finance:card"),
        Some(FinanceToastTarget::PaymentAccount { account_id: None })
    );
    // IPO 事件 ID 与更新目标不能被误判成理财。
    assert_eq!(finance_toast_target("sh-601001-2026-11-05"), None);
    assert_eq!(finance_toast_target("update:0.5.0"), None);
}

#[test]
fn slint_source_is_registered_as_a_build_dependency() {
    // 曾经出现「改了 ui/main.slint，产物仍是旧界面」：Slint 编译产物在 OUT_DIR，
    // build script 不重跑就不会更新。这条断言保证重跑条件一直在。
    let build_script =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs")).unwrap();
    assert!(
        build_script.contains("cargo:rerun-if-changed=ui/main.slint"),
        "build.rs 必须声明 ui/main.slint 为重跑条件"
    );
}

#[test]
fn repayment_form_and_account_list_never_share_the_vertical_viewport() {
    // 表单展开时若仍与账户列表并列，ListView 会被压成一条卡片高，账户多了无法滚动。
    // 两者改为互斥：表单占满可用高度并在内部滚动，收起后才显示占满剩余高度的账户列表。
    let ui =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/main.slint")).unwrap();
    let start = ui
        .find("if root.active-page == 5 :")
        .expect("必须存在还款提醒页");
    let end = ui[start..]
        .find("if root.active-page == 2 :")
        .map(|offset| start + offset)
        .expect("还款提醒页之后必须还有健康页");
    let page = &ui[start..end];
    assert!(page.contains("if !root.credit-form-visible : Rectangle"));
    assert!(page.contains("height: root.repayment-page-height"));
    assert!(page.contains("ListView"));
    assert!(page.contains("for card in root.credit-cards"));
}

#[test]
fn main_window_declares_window_level_max_size() {
    // 页面布局汇总出的 max 会被 Slint 当成窗口尺寸上限：逆回购页「提醒时段」的时点行
    // 四个控件全部固定宽（显式 width 同时是 min 和 max），曾把根布局 max-width 钉在
    // 约 654px，切页时后端直接把窗口钳进 654–800px 并禁止拉宽。根元素显式 max 是
    // 唯一稳定的覆盖点，删掉这两行就会复发，所以在这里钉住。
    let ui =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/main.slint")).unwrap();
    let window_start = ui
        .find("export component MainWindow inherits Window")
        .expect("ui/main.slint 必须定义 MainWindow");
    // 窗口属性都声明在第一个子元素之前，用第一个布局元素作为窗口头部的边界。
    let header_end = window_start
        + ui[window_start..]
            .find("VerticalBox {")
            .expect("MainWindow 必须包含垂直布局");
    let header = &ui[window_start..header_end];
    for property in ["max-width", "max-height"] {
        assert!(
            header.contains(&format!("{property}: 10000px;")),
            "MainWindow 必须声明窗口级 {property}: 10000px（与 window_state.rs 的 MAX_LOGICAL_DIMENSION 一致），否则页面布局的 max 会泄漏成窗口上限"
        );
    }
}

#[test]
fn repayment_account_icons_and_semantic_fallbacks_are_correctly_registered() {
    use crate::finance::{PaymentAccountKind, account_icon_svg};

    for kind in [
        PaymentAccountKind::CreditCard,
        PaymentAccountKind::Mortgage,
        PaymentAccountKind::AutoLoan,
        PaymentAccountKind::CreditLoan,
    ] {
        let svg = account_icon_svg(None, kind, None);
        assert!(svg.starts_with(b"<svg"), "语义降级必须生成有效 SVG");
        assert!(svg.len() > 100, "SVG 资源不能过短");
    }

    let custom = account_icon_svg(
        Some("other"),
        PaymentAccountKind::CreditCard,
        Some("测试银行"),
    );
    assert!(String::from_utf8_lossy(&custom).contains("测试"));
}

#[test]
fn test_parse_and_format_amount_in_ui() {
    use crate::ui::finance::{format_amount_for_input, parse_optional_amount};

    // 空白输入
    assert_eq!(parse_optional_amount("").unwrap(), None);
    assert_eq!(parse_optional_amount("   ").unwrap(), None);

    // 纯数字
    assert_eq!(parse_optional_amount("5800").unwrap(), Some(5800.0));
    assert_eq!(parse_optional_amount("5800.50").unwrap(), Some(5800.50));

    // 带货币前缀与千分位逗号
    assert_eq!(parse_optional_amount("¥5,800").unwrap(), Some(5800.0));
    assert_eq!(
        parse_optional_amount("￥ 12,345.67").unwrap(),
        Some(12345.67)
    );
    assert_eq!(parse_optional_amount("$1000").unwrap(), Some(1000.0));

    // 带中文「元」单位后缀
    assert_eq!(parse_optional_amount("5800元").unwrap(), Some(5800.0));
    assert_eq!(parse_optional_amount("¥ 5,800 元").unwrap(), Some(5800.0));
    assert_eq!(
        parse_optional_amount("12,345.50元").unwrap(),
        Some(12345.50)
    );

    // 边界值
    assert_eq!(parse_optional_amount("0").unwrap(), Some(0.0));
    assert_eq!(
        parse_optional_amount("100000000").unwrap(),
        Some(100_000_000.0)
    );

    // 异常输入
    assert!(parse_optional_amount("-10").is_err());
    assert!(parse_optional_amount("100000001").is_err());
    assert!(parse_optional_amount("abc").is_err());

    // 格式化回填与浮点微漂移容错
    assert_eq!(format_amount_for_input(5800.0), "5800");
    assert_eq!(format_amount_for_input(5800.50), "5800.50");
    assert_eq!(format_amount_for_input(5800.000000000001), "5800");
    assert_eq!(format_amount_for_input(0.0), "0");
}

#[test]
fn payment_reminder_kinds_cover_all_four_account_types() {
    // 弹窗「打开还款提醒」和「本期已还」都必须覆盖信用贷，否则会跳错页面或报不支持。
    for kind in ["credit_card", "mortgage", "auto_loan", "credit_loan"] {
        assert!(is_payment_reminder_kind(kind), "{kind}");
        assert!(opens_payment_page(kind), "{kind}");
    }
    assert!(opens_payment_page("payment"));
    assert!(!is_payment_reminder_kind("payment"));
    assert!(!opens_payment_page("reverse_repo"));
    assert!(!is_payment_reminder_kind("reverse_repo"));
}

#[test]
fn test_monthly_summary_formatting() {
    use crate::ui::finance::format_monthly_summary;

    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();

    // 1. 无任何账户
    assert_eq!(
        format_monthly_summary(0, 0, 0, 0, 0, 0, 0, 0.0, 0.0, None),
        "暂无还款提醒"
    );

    // 2. 有账户但无金额，且无逾期、有下次还款日
    assert_eq!(
        format_monthly_summary(2, 2, 0, 2, 2, 0, 0, 0.0, 0.0, Some(day)),
        "2 项启用 · 最近还款 2026-11-20"
    );

    // 3. 有账户但无金额，且有逾期
    assert_eq!(
        format_monthly_summary(3, 3, 0, 3, 3, 0, 1, 0.0, 0.0, Some(day)),
        "1 项已逾期 · 共 3 项启用"
    );

    // 4. 有账户但全结清/暂停（无下次还款日）
    assert_eq!(
        format_monthly_summary(2, 0, 0, 0, 0, 0, 0, 0.0, 0.0, None),
        "所有还款提醒已暂停或结束"
    );

    // 5. 全部已还清（无未填金额）
    assert_eq!(
        format_monthly_summary(3, 3, 3, 0, 0, 3, 0, 0.0, 18300.0, None),
        "本月已全部结清 ¥18,300.00 · 3 项已还"
    );

    // 6. 全部已还清（含部分未填金额）
    assert_eq!(
        format_monthly_summary(3, 3, 2, 1, 0, 3, 0, 0.0, 12500.0, None),
        "本月已全部结清 ¥12,500.00 (另 1 项未填金额) · 3 项已还"
    );

    // 7. 有待还，无逾期，全部有金额
    assert_eq!(
        format_monthly_summary(2, 2, 2, 0, 1, 1, 0, 5800.0, 12500.0, Some(day)),
        "本月待还 ¥5,800.00 · 已还 ¥12,500.00 (本月应还 ¥18,300.00 · 2 项启用)"
    );

    // 8. 有待还，无逾期，含未填金额
    assert_eq!(
        format_monthly_summary(3, 3, 2, 1, 1, 1, 0, 5800.0, 12500.0, Some(day)),
        "本月待还 ¥5,800.00 (另 1 项未填金额) · 已还 ¥12,500.00 · 共 2 项启用"
    );

    // 9. 有待还且有逾期（全部有金额）
    assert_eq!(
        format_monthly_summary(2, 2, 2, 0, 2, 0, 1, 10000.0, 0.0, Some(day)),
        "1 项已逾期 · 本月待还 ¥10,000.00 · 已还 ¥0.00 (本月应还 ¥10,000.00)"
    );

    // 10. 有待还且有逾期（含未填金额）
    assert_eq!(
        format_monthly_summary(3, 3, 2, 1, 2, 0, 1, 10000.0, 0.0, Some(day)),
        "1 项已逾期 · 本月待还 ¥10,000.00 (另 1 项未填金额) · 已还 ¥0.00"
    );

    // 11. 浮点分位累加精度（如 12500.55 + 3499.45 = 16000.00）
    let p1 = 12500.55;
    let p2 = 3499.45;
    let sum = p1 + p2;
    assert_eq!(
        format_monthly_summary(2, 2, 2, 0, 2, 0, 0, sum, 0.0, Some(day)),
        "本月待还 ¥16,000.00 · 已还 ¥0.00 (本月应还 ¥16,000.00 · 2 项启用)"
    );

    // 12. 本月范围内已还清、但仍有旧账逾期：不能显示「本月已全部结清」盖掉逾期。
    assert_eq!(
        format_monthly_summary(2, 2, 1, 0, 0, 1, 1, 0.0, 3200.0, None),
        "1 项已逾期 · 本月已还 ¥3,200.00 · 1 项已还"
    );

    // 13. 同上但本月已还项未填金额。
    assert_eq!(
        format_monthly_summary(3, 3, 1, 1, 0, 1, 1, 0.0, 3200.0, None),
        "1 项已逾期 · 本月已还 ¥3,200.00 (另 1 项未填金额)"
    );
}

#[test]
fn repayment_editor_preserves_paid_summary_and_confirms_unknown_amounts() {
    use crate::finance::{PaymentAccount, PaymentAccountKind, cycle_key, shift_cycle};

    struct HeadlessPlatform;
    impl slint::platform::Platform for HeadlessPlatform {
        fn create_window_adapter(
            &self,
        ) -> std::result::Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError>
        {
            Ok(
                slint::platform::software_renderer::MinimalSoftwareWindow::new(
                    slint::platform::software_renderer::RepaintBufferType::ReusedBuffer,
                ),
            )
        }
    }

    fn save_form(ui: &MainWindow) {
        ui.invoke_save_credit_card(
            ui.get_credit_form_id(),
            ui.get_credit_form_kind_index(),
            ui.get_credit_form_alias(),
            ui.get_credit_form_bank_index(),
            ui.get_credit_form_bank_name(),
            ui.get_credit_form_due_day_index(),
            ui.get_credit_form_remind_time(),
            ui.get_credit_form_lead_mode_index(),
            ui.get_credit_form_lead_days(),
            ui.get_credit_form_final_cycle(),
            ui.get_credit_form_overdue(),
            ui.get_credit_form_enabled(),
            ui.get_credit_form_amount(),
            ui.get_credit_form_total_amount(),
        );
        assert_eq!(ui.get_credit_error(), "");
    }

    slint::platform::set_platform(Box::new(HeadlessPlatform)).unwrap();
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-repayment-ui-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let database = crate::storage::Database::new(&root);
    database.initialize().unwrap();
    let runtime = RuntimeHandle::for_test(database.clone());
    let ui = MainWindow::new().unwrap();
    wire_finance_callbacks(&ui, runtime.clone());
    let now = crate::core::now_china();
    let current = cycle_key(now.date_naive());
    let next = shift_cycle(&current, 1).unwrap();
    let mut account = PaymentAccount {
        id: "summary-card".into(),
        kind: PaymentAccountKind::CreditCard,
        alias: "账单测试".into(),
        bank_id: None,
        bank_name: None,
        due_day: 20,
        remind_time: model::time(9, 0),
        lead_days: vec![0],
        overdue_enabled: false,
        enabled: true,
        first_tracked_cycle: current.clone(),
        final_cycle: None,
        closed_at: None,
        rules_changed_at: now,
        created_at: now,
        updated_at: now,
        amount: Some(3200.0),
        total_amount: None,
        amount_cycle: Some(current.clone()),
    };
    database.save_payment_account(&account).unwrap();
    database
        .mark_payment_account_paid(&account.id, &current)
        .unwrap();
    assert!(refresh_finance_ui(&ui, &runtime));
    assert!(ui.get_credit_summary().contains("3,200.00"));

    ui.invoke_edit_credit_card(account.id.clone().into());
    assert!(ui.get_credit_form_preview().contains(&next));
    assert!(ui.get_credit_form_preview().contains("重新填写"));
    assert!(!ui.get_credit_form_preview().contains("还款金额 ¥3,200.00"));
    ui.set_credit_form_amount("3300".into());
    ui.set_credit_form_amount_touched(true);
    ui.invoke_preview_credit_form();
    assert!(ui.get_credit_form_preview().contains("3,300.00"));
    assert!(!ui.get_credit_form_preview().contains("重新填写"));
    save_form(&ui);
    assert!(ui.get_credit_summary().contains("3,200.00"));
    assert!(
        ui.get_credit_cards()
            .row_data(0)
            .unwrap()
            .amount_text
            .contains("3,200.00")
    );
    assert_eq!(
        database
            .payment_account(&account.id)
            .unwrap()
            .unwrap()
            .amount_cycle,
        Some(next)
    );

    // 迁移后账期待确认：只改别名不能确认，重新输入相同金额可以确认。
    database.delete_payment_account(&account.id).unwrap();
    account.id = "unconfirmed-card".into();
    account.amount_cycle = None;
    database.save_payment_account(&account).unwrap();
    assert!(refresh_finance_ui(&ui, &runtime));
    assert!(
        ui.get_credit_cards()
            .row_data(0)
            .unwrap()
            .amount_text
            .contains("账期待确认")
    );
    assert!(!ui.get_credit_summary().contains("3,200.00"));
    ui.invoke_edit_credit_card(account.id.clone().into());
    ui.set_credit_form_alias("改名后仍待确认".into());
    save_form(&ui);
    assert_eq!(
        database
            .payment_account(&account.id)
            .unwrap()
            .unwrap()
            .amount_cycle,
        None
    );
    ui.invoke_edit_credit_card(account.id.clone().into());
    ui.set_credit_form_amount_touched(true);
    ui.invoke_preview_credit_form();
    assert!(!ui.get_credit_form_preview().contains("重新填写"));
    save_form(&ui);
    assert_eq!(
        database
            .payment_account(&account.id)
            .unwrap()
            .unwrap()
            .amount_cycle,
        Some(current)
    );
    assert!(ui.get_credit_summary().contains("3,200.00"));
    let previous_summary = ui.get_credit_summary();
    rusqlite::Connection::open(database.path())
        .unwrap()
        .execute("ALTER TABLE credit_card_actions DROP COLUMN amount", [])
        .unwrap();
    assert!(!refresh_finance_ui(&ui, &runtime));
    assert_eq!(ui.get_credit_summary(), previous_summary);
    assert!(ui.get_status_text().contains("读取还款记录失败"));
    let _ = std::fs::remove_dir_all(root);
}

use super::*;

pub(crate) struct EventDetailsData {
    event: IpoEvent,
    field_sources: Vec<FieldSourceEntry>,
    announcements: Vec<AnnouncementDocument>,
    overrides: Vec<ManualOverrideEntry>,
    settings: AppSettings,
}

pub(crate) fn load_event_details(
    runtime: &RuntimeHandle,
    event_id: &str,
) -> Result<Option<EventDetailsData>> {
    let Some(event) = runtime.event(event_id)? else {
        return Ok(None);
    };
    let field_sources = runtime.field_sources(event_id)?;
    let announcements = runtime.announcements(event_id)?;
    let overrides = runtime.manual_overrides(event_id, event.event_version)?;
    let settings = runtime.settings()?;
    Ok(Some(EventDetailsData {
        event,
        field_sources,
        announcements,
        overrides,
        settings,
    }))
}

/// 任务详情加载移到后台线程：数据库读取不再阻塞事件循环。
/// 生成号保证旧请求的结果不会覆盖用户后来选择的任务。
pub(crate) fn show_event_details(ui: &MainWindow, runtime: &RuntimeHandle, event_id: &str) {
    static DETAILS_GENERATION: AtomicU64 = AtomicU64::new(0);
    let generation = DETAILS_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let details_runtime = runtime.clone();
    let details_id = event_id.to_owned();
    let details_window = ui.as_weak();
    let spawned = std::thread::Builder::new()
        .name("event-details".into())
        .spawn(move || {
            let data = load_event_details(&details_runtime, &details_id);
            let _ = slint::invoke_from_event_loop(move || {
                if DETAILS_GENERATION.load(Ordering::Acquire) != generation {
                    return;
                }
                let Some(ui) = details_window.upgrade() else {
                    return;
                };
                match data {
                    Ok(Some(data)) => apply_event_details(&ui, data),
                    Ok(None) => ui.set_status_text("任务已不存在，请刷新列表".into()),
                    Err(error) => ui.set_status_text(format!("读取详情失败：{error:#}").into()),
                }
            });
        });
    if let Err(error) = spawned {
        DETAILS_GENERATION.fetch_sub(1, Ordering::AcqRel);
        operations::log("ERROR", &format!("无法启动详情加载线程：{error}"));
        ui.set_status_text("读取详情失败：无法启动后台加载线程".into());
    }
}

pub(crate) fn apply_event_details(ui: &MainWindow, data: EventDetailsData) {
    let EventDetailsData {
        event,
        field_sources,
        announcements,
        overrides,
        settings,
    } = data;
    ui.set_selected_event_id(event.id.clone().into());
    ui.set_selected_event_version(event.event_version);
    ui.set_selected_title(format!("{} · {}", event.name, event.display_code()).into());
    ui.set_selected_summary(
        format!(
            "{} · 股票代码 {} · 申购日 {} · 数据版本 {}",
            market_name(event.exchange, event.board),
            event.security_code,
            event
                .apply_date
                .map(|date| date.to_string())
                .unwrap_or_else(|| "待核验".into()),
            event.event_version,
        )
        .into(),
    );
    ui.set_selected_quality(quality_text(event.data_quality_status).into());
    ui.set_selected_quality_alert(matches!(
        event.data_quality_status,
        DataQualityStatus::DataConflict
            | DataQualityStatus::ManualReviewRequired
            | DataQualityStatus::Stale
    ));
    ui.set_selected_warning(if !event.manual_override_fields.is_empty() {
        format!(
            "当前有效人工覆盖：{}。所有原始来源仍保留在“字段来源”中。",
            event
                .manual_override_fields
                .iter()
                .map(|field| field_display_name(field))
                .collect::<Vec<_>>()
                .join("、"),
        )
        .into()
    } else if event.data_conflict {
        "关键字段存在来源冲突，请以最新正式发行公告为准。".into()
    } else {
        "".into()
    });
    let announcement_titles = announcements
        .iter()
        .map(|document| document.reference.title.clone())
        .collect::<Vec<_>>();
    ui.set_selected_detail(format_event_details(&event, &announcement_titles, &settings).into());
    ui.set_details_active_tab(0);
    ui.set_override_field_index(0);
    ui.set_override_announcement_index(0);
    ui.set_override_value("".into());
    ui.set_override_reason("".into());
    ui.set_override_status("".into());

    let field_rows = field_sources
        .into_iter()
        .map(|source| FieldSourceRow {
            field_name: field_display_name(&source.field_name).into(),
            normalized_value: source.normalized_value.unwrap_or_else(|| "—".into()).into(),
            raw_value: source.raw_value.unwrap_or_else(|| "—".into()).into(),
            source_text: format!("{} · 优先级 {}", source.source, source.priority).into(),
            fetched_text: format!("抓取 {}", source.fetched_at.format("%Y-%m-%d %H:%M")).into(),
        })
        .collect::<Vec<_>>();
    ui.set_field_source_rows(ModelRc::from(Rc::new(VecModel::from(field_rows))));

    let announcement_rows = announcements
        .iter()
        .map(|document| {
            let metadata_only = document.local_path.is_empty()
                && document.parser_version == "announcement-metadata-v1";
            let evidence = if metadata_only {
                "程序只保存公告标题和在线链接，不下载或解析正文；请按需打开官方原文核对。".into()
            } else if document.fields.is_empty() {
                "未提取到高置信度字段，请人工查看原文。".into()
            } else {
                document
                    .fields
                    .iter()
                    .take(6)
                    .map(|field| {
                        format!(
                            "{}={}（{:.0}%）",
                            field_display_name(&field.name),
                            field.value,
                            field.confidence * 100.0,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("；")
            };
            let metadata = if metadata_only {
                format!(
                    "{} · {} · 在线公告链接",
                    document.reference.provider,
                    document
                        .reference
                        .published_at
                        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "发布时间未知".into()),
                )
            } else {
                let hash_preview = document.file_hash.chars().take(12).collect::<String>();
                format!(
                    "{} · {} · {} · 历史文件 SHA-256 {}…",
                    document.reference.provider,
                    document
                        .reference
                        .published_at
                        .map(|value| value.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "发布时间未知".into()),
                    extraction_text(document.status),
                    hash_preview,
                )
            };
            AnnouncementRow {
                id: document.id.clone().into(),
                title: document.reference.title.clone().into(),
                metadata: metadata.into(),
                evidence: evidence.into(),
                source_url: document.reference.url.clone().into(),
                local_path: document.local_path.clone().into(),
                local_available: std::path::Path::new(&document.local_path).is_file(),
            }
        })
        .collect::<Vec<_>>();
    ui.set_announcement_rows(ModelRc::from(Rc::new(VecModel::from(announcement_rows))));
    let mut announcement_choices = vec![slint::SharedString::from("不指定公告")];
    announcement_choices.extend(
        announcements
            .iter()
            .map(|document| slint::SharedString::from(document.reference.title.as_str())),
    );
    ui.set_announcement_choices(ModelRc::from(Rc::new(VecModel::from(announcement_choices))));

    let override_rows = overrides
        .into_iter()
        .map(|entry| {
            let announcement_title = entry.announcement_document_id.as_deref().and_then(|id| {
                announcements
                    .iter()
                    .find(|document| document.id == id)
                    .map(|document| document.reference.title.as_str())
            });
            OverrideRow {
                id: entry.id.to_string().into(),
                summary: format!(
                    "{} = {}",
                    field_display_name(&entry.field_name),
                    entry.override_value
                )
                .into(),
                metadata: format!(
                    "理由：{} · {}{}{}",
                    entry.reason,
                    entry.created_at.format("%Y-%m-%d %H:%M"),
                    announcement_title
                        .map(|title| format!(" · 依据公告：{title}"))
                        .unwrap_or_else(|| " · 未关联依据公告".into()),
                    entry
                        .revoked_at
                        .map(|value| format!(" · 已于 {} 撤销", value.format("%Y-%m-%d %H:%M")))
                        .unwrap_or_else(|| " · 当前有效".into()),
                )
                .into(),
                can_revoke: entry.revoked_at.is_none(),
            }
        })
        .collect::<Vec<_>>();
    ui.set_override_rows(ModelRc::from(Rc::new(VecModel::from(override_rows))));
    ui.set_show_details(true);
}

pub(crate) fn override_field_name(index: i32) -> &'static str {
    match index {
        0 => "ApplyCode",
        1 => "ApplyDate",
        2 => "IssuePrice",
        3 => "MaxApplyQuantity",
        4 => "LotSize",
        5 => "OfficialSessions",
        6 => "IssueStatus",
        _ => "",
    }
}

pub(crate) fn field_display_name(name: &str) -> &str {
    match name {
        "SecurityCode" => "股票代码",
        "ApplyCode" => "申购代码",
        "LegacyCode" => "历史代码",
        "Name" => "股票简称",
        "ApplyDate" => "申购日期",
        "IssuePrice" => "发行价格",
        "LotSize" => "申购单位",
        "MaxApplyQuantity" => "申购上限",
        "RequiredMarketValue" => "所需市值",
        "RequiredCash" => "所需现金",
        "BallotDate" => "中签日期",
        "PaymentDate" => "缴款日期",
        "ListingDate" => "上市日期",
        "IssueStatus" | "Status" => "发行状态",
        "OfficialSessions" | "Sessions" => "官方申购时段",
        _ => name,
    }
}

pub(crate) fn extraction_text(status: model::ExtractionStatus) -> &'static str {
    match status {
        model::ExtractionStatus::Extracted => "文本已解析",
        model::ExtractionStatus::LowConfidence => "低置信度",
        model::ExtractionStatus::Failed => "解析失败",
        model::ExtractionStatus::Unsupported => "仅保存在线链接",
        _ => "待解析",
    }
}

pub(crate) fn format_event_details(
    event: &IpoEvent,
    announcements: &[String],
    settings: &AppSettings,
) -> String {
    let sessions = if event.sessions.is_empty() {
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
    let announcement_text = if announcements.is_empty() {
        "暂无已保存的正式公告链接".into()
    } else {
        announcements
            .iter()
            .take(8)
            .map(|title| format!("• {title}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "市场：{}\n证券代码：{}\n申购代码：{}\n申购日期：{}\n发行价格：{}\n申购单位：{} 股\n申购上限：{} 股\n所需市值：{}\n所需现金：{}\n中签日期：{}\n缴款日期：{}\n上市日期：{}\n官方申购时段：{}\n安全截止：{}\n任务状态：{}\n数据质量：{}\n事件版本：{}\n最后更新：{}\n\n正式公告链接\n{}",
        market_name(event.exchange, event.board),
        event.security_code,
        event.apply_code.as_deref().unwrap_or("待核验"),
        event
            .apply_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .issue_price
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "待核验".into()),
        event
            .lot_size
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .max_apply_quantity
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .required_market_value
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "不适用或待核验".into()),
        event
            .required_cash
            .map(|value| format!("{value:.2} 元"))
            .unwrap_or_else(|| "不适用或待核验".into()),
        event
            .ballot_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .payment_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        event
            .listing_date
            .map(|value| value.to_string())
            .unwrap_or_else(|| "待核验".into()),
        sessions,
        crate::core::effective_cutoff(event, settings).format("%H:%M"),
        lifecycle_text(event.lifecycle_status),
        quality_text(event.data_quality_status),
        event.event_version,
        event.updated_at.format("%Y-%m-%d %H:%M:%S"),
        announcement_text,
    )
}

use super::*;

#[derive(Debug)]
pub(crate) struct AnnouncementRunStats {
    pub(crate) started: ChinaDateTime,
    pub(crate) attempted_events: usize,
    pub(crate) successful_searches: usize,
    pub(crate) references_found: usize,
    pub(crate) metadata_records: usize,
    pub(crate) mirror_events: usize,
    pub(crate) issue_count: usize,
    pub(crate) issues: Vec<String>,
    pub(crate) retry_after: Option<ChinaDateTime>,
}

impl AnnouncementRunStats {
    pub(crate) fn new(started: ChinaDateTime) -> Self {
        Self {
            started,
            attempted_events: 0,
            successful_searches: 0,
            references_found: 0,
            metadata_records: 0,
            mirror_events: 0,
            issue_count: 0,
            issues: Vec::new(),
            retry_after: None,
        }
    }

    pub(crate) fn record_issue(&mut self, message: impl Into<String>) {
        self.issue_count += 1;
        if self.issues.len() < 3 {
            self.issues
                .push(message.into().chars().take(700).collect::<String>());
        }
    }

    pub(crate) fn observe_retry_after(&mut self, value: Option<ChinaDateTime>) {
        let Some(value) = value else { return };
        self.retry_after = Some(match self.retry_after {
            Some(current) => current.max(value),
            None => value,
        });
    }

    pub(crate) fn state(&self) -> HealthState {
        if self.successful_searches == 0 {
            HealthState::Failed
        } else if self.issue_count > 0 {
            HealthState::Warning
        } else {
            HealthState::Healthy
        }
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "attemptedEvents={}, searchSuccesses={}, references={}, metadataRecords={}, mirrorEvents={}, issues={}",
            self.attempted_events,
            self.successful_searches,
            self.references_found,
            self.metadata_records,
            self.mirror_events,
            self.issue_count
        )
    }

    pub(crate) fn error_summary(&self) -> Option<String> {
        (self.issue_count > 0).then(|| {
            format!(
                "公告元数据源部分或全部失败：{}；{}",
                self.summary(),
                self.issues.join("；")
            )
        })
    }
}

pub(crate) fn synchronize(
    database: &Database,
    client: &reqwest::blocking::Client,
    ui_state: &RuntimeUiState,
    reason: &str,
    stop_requested: &AtomicBool,
) -> Result<()> {
    ensure_not_stopping(stop_requested)?;
    update_snapshot(ui_state, |value| {
        value.is_synchronizing = true;
        value.status_text = format!("正在同步：{reason}");
        value.last_error = None;
    });
    let started = now_china();
    let settings = database.settings()?;
    let mut candidates = Vec::<Candidate>::new();
    let collectors: [(
        &str,
        fn(&reqwest::blocking::Client, &dyn Fn() -> bool) -> Result<CollectorOutput>,
    ); 4] = [
        ("eastmoney", network::collect_eastmoney),
        ("sse", network::collect_sse),
        ("cninfo", network::collect_cninfo),
        ("bse", network::collect_bse),
    ];
    let mut successful_sources = 0usize;
    let mut failed_sources = 0usize;
    let mut successful_source_names = HashSet::<&'static str>::new();
    let cancelled = || stop_requested.load(Ordering::Acquire);
    for (source, collect) in collectors {
        ensure_not_stopping(stop_requested)?;
        let now = now_china();
        if !database.source_can_attempt(source, now)?.0 {
            probe_backed_off_source(database, client, source, now, stop_requested)?;
            continue;
        }
        let collected = collect(client, &cancelled);
        ensure_not_stopping(stop_requested)?;
        match collected {
            Ok(output) => {
                let record_count = output.candidates.len();
                let audit_state = output.audit.state();
                let audit_summary = output.audit.summary();
                database.save_source_run(
                    output.source,
                    output.started,
                    audit_state,
                    record_count,
                    Some(&output.raw),
                    Some(&output.hash),
                    Some(&output.schema),
                    audit_summary.as_deref(),
                )?;
                candidates.extend(output.candidates);
                successful_sources += 1;
                if audit_state == HealthState::Healthy {
                    successful_source_names.insert(output.source);
                    operations::log(
                        "INFO",
                        &format!("数据源 {} 同步成功，{} 条记录", output.source, record_count),
                    );
                } else {
                    operations::log(
                        "WARN",
                        &format!(
                            "数据源 {} 响应可解析但覆盖不完整，{} 条记录：{}",
                            output.source,
                            record_count,
                            audit_summary.as_deref().unwrap_or("计数/明细核验异常")
                        ),
                    );
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                database.save_source_run_with_retry_after(
                    source,
                    now,
                    HealthState::Failed,
                    0,
                    None,
                    None,
                    None,
                    Some(&message),
                    network::retry_after_from_error(&error),
                )?;
                failed_sources += 1;
                operations::log("ERROR", &format!("数据源 {source} 同步失败：{message}"));
            }
        }
    }
    if successful_sources == 0 && candidates.is_empty() {
        let reason_text = if failed_sources == 0 {
            "所有来源都在退避期内"
        } else {
            "所有数据源同步失败"
        };
        let today_count = database
            .today_events()
            .map(|events| {
                events
                    .into_iter()
                    .filter(|event| settings.exchange_enabled(event.exchange))
                    .count()
            })
            .unwrap_or_default();
        let missing_sources = missing_required_sources(&settings, &successful_source_names);
        let conclusion = sync_conclusion(
            started,
            now_china(),
            today_count,
            0,
            0,
            &successful_source_names,
            &missing_sources,
        );
        let message = format!("{reason_text}；{}", conclusion.summary);
        update_snapshot(ui_state, |value| {
            value.is_synchronizing = false;
            value.status_text = format!("{message}，继续使用 SQLite 缓存");
            value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
            value.last_sync_succeeded = Some(false);
            value.last_error = Some(message.clone());
        });
        database.save_sync_conclusion(&conclusion)?;
        return Ok(());
    }

    let today = now_china().date_naive();
    candidates.retain(|candidate| {
        settings.exchange_enabled(candidate.exchange) && candidate.stable_identity().is_some()
    });
    let groups: Vec<Vec<Candidate>> = group_candidates(candidates)
        .into_values()
        .filter(|group| {
            group.iter().any(|candidate| {
                candidate
                    .apply_date
                    .is_some_and(|date| date >= today - chrono::Duration::days(30))
                    || matches!(
                        candidate.status,
                        crate::model::IssueStatus::Upcoming | crate::model::IssueStatus::Active
                    )
            })
        })
        .collect();
    let candidate_count: usize = groups.iter().map(Vec::len).sum();
    operations::log(
        "INFO",
        &format!(
            "近期候选过滤完成：candidates={candidate_count}, groups={}",
            groups.len()
        ),
    );
    let mut event_count = 0usize;
    let mut announcement_count = 0usize;
    let mut announcement_attempts = HashMap::<&'static str, bool>::new();
    let mut announcement_runs = HashMap::<&'static str, AnnouncementRunStats>::new();
    for group in groups {
        ensure_not_stopping(stop_requested)?;
        let identity = group.first().and_then(Candidate::stable_identity);
        let existing = identity
            .as_deref()
            .and_then(|id| database.event(id).ok().flatten());
        let Some(provisional) =
            reconcile_candidates(&group, existing.as_ref(), &settings, now_china())
        else {
            continue;
        };
        let combined = group;
        let mut documents = Vec::new();
        if should_check_announcements(&provisional) {
            let provider = match provisional.exchange {
                Exchange::Shanghai => "sse-announcement",
                Exchange::Shenzhen => "cninfo-announcement",
                Exchange::Beijing => "bse-announcement",
                _ => "announcement",
            };
            let now = now_china();
            let can_attempt = if let Some(can_attempt) = announcement_attempts.get(provider) {
                *can_attempt
            } else {
                let can_attempt = database.source_can_attempt(provider, now)?.0;
                if !can_attempt {
                    probe_backed_off_source(database, client, provider, now, stop_requested)?;
                }
                announcement_attempts.insert(provider, can_attempt);
                can_attempt
            };
            if can_attempt {
                let stats = announcement_runs
                    .entry(provider)
                    .or_insert_with(|| AnnouncementRunStats::new(now));
                stats.attempted_events += 1;
                let from =
                    provisional.apply_date.unwrap_or(now.date_naive()) - chrono::Duration::days(14);
                let to =
                    provisional.apply_date.unwrap_or(now.date_naive()) + chrono::Duration::days(7);
                let search = announcement::search(client, &provisional, from, to, &cancelled);
                ensure_not_stopping(stop_requested)?;
                match search {
                    Ok(output) => {
                        stats.successful_searches += 1;
                        stats.references_found += output.references.len();
                        if output.used_mirror {
                            stats.mirror_events += 1;
                        }
                        if let Some(warning) = output.warning {
                            stats.record_issue(format!("event={}：{warning}", provisional.id));
                            operations::log(
                                "WARN",
                                &format!(
                                    "公告来源降级：event={}, provider={provider}, warning={warning}",
                                    provisional.id
                                ),
                            );
                        }
                        for reference in output.references {
                            documents
                                .push(announcement::metadata_document(&provisional, reference));
                            stats.metadata_records += 1;
                        }
                    }
                    Err(error) => {
                        stats.observe_retry_after(network::retry_after_from_error(&error));
                        stats.record_issue(format!("event={}：{error:#}", provisional.id));
                        operations::log(
                            "ERROR",
                            &format!(
                                "公告元数据检索失败：event={}, provider={provider}, error={error:#}",
                                provisional.id
                            ),
                        );
                    }
                }
            }
        }
        let mut resolved =
            reconcile_candidates(&combined, existing.as_ref(), &settings, now_china())
                .unwrap_or(provisional);
        if resolved.announcement_url.is_none() {
            resolved.announcement_url = documents
                .first()
                .map(|document| document.reference.url.clone());
        }
        persist_reconciled_group(database, resolved, &combined, &documents)?;
        announcement_count += documents.len();
        event_count += 1;
    }
    for provider in [
        "sse-announcement",
        "cninfo-announcement",
        "bse-announcement",
    ] {
        ensure_not_stopping(stop_requested)?;
        let Some(stats) = announcement_runs.remove(provider) else {
            continue;
        };
        let state = stats.state();
        let summary = stats.summary();
        let error = stats.error_summary();
        database.save_source_run_with_retry_after(
            provider,
            stats.started,
            state,
            stats.metadata_records,
            Some(&summary),
            None,
            Some("announcement-metadata-run-v3"),
            error.as_deref(),
            stats.retry_after,
        )?;
        operations::log(
            if state == HealthState::Failed {
                "ERROR"
            } else if state == HealthState::Warning {
                "WARN"
            } else {
                "INFO"
            },
            &format!("公告元数据源 {provider} 本轮状态 {state:?}：{summary}"),
        );
    }
    database.touch_heartbeat("synchronization", now_china())?;
    let today_count = database
        .today_events()?
        .into_iter()
        .filter(|event| settings.exchange_enabled(event.exchange))
        .count();
    let missing_sources = missing_required_sources(&settings, &successful_source_names);
    let conclusion = sync_conclusion(
        started,
        now_china(),
        today_count,
        event_count,
        announcement_count,
        &successful_source_names,
        &missing_sources,
    );
    database.save_sync_conclusion(&conclusion)?;
    update_snapshot(ui_state, |value| {
        value.is_synchronizing = false;
        value.status_text = conclusion.summary.clone();
        value.last_sync_text = now_china().format("%Y-%m-%d %H:%M").to_string();
        value.last_sync_succeeded = Some(conclusion.kind.is_healthy());
        value.last_error = (!conclusion.kind.is_healthy())
            .then(|| format!("启用市场来源覆盖不完整：{}", missing_sources.join("、")));
    });
    operations::log(
        match conclusion.kind.health_state() {
            HealthState::Healthy => "INFO",
            HealthState::Warning => "WARN",
            _ => "ERROR",
        },
        &format!(
            "{}；conclusion={:?}, candidates={candidate_count}, events={event_count}, announcements={announcement_count}, sources={successful_sources}, failed={failed_sources}",
            conclusion.summary, conclusion.kind
        ),
    );
    Ok(())
}

pub(crate) fn ensure_not_stopping(stop_requested: &AtomicBool) -> Result<()> {
    if stop_requested.load(Ordering::Acquire) {
        bail!("同步已取消")
    }
    Ok(())
}

pub(crate) fn missing_required_sources(
    settings: &AppSettings,
    successful_sources: &HashSet<&'static str>,
) -> Vec<&'static str> {
    let mut required = vec!["eastmoney"];
    if settings.shanghai_enabled {
        required.push("sse");
    }
    if settings.shenzhen_enabled {
        required.push("cninfo");
    }
    if settings.beijing_enabled {
        required.push("bse");
    }
    required
        .into_iter()
        .filter(|source| !successful_sources.contains(source))
        .collect()
}

pub(crate) fn probe_backed_off_source(
    database: &Database,
    client: &reqwest::blocking::Client,
    source: &str,
    now: ChinaDateTime,
    stop_requested: &AtomicBool,
) -> Result<()> {
    ensure_not_stopping(stop_requested)?;
    if !database.try_claim_source_probe(source, now)? {
        return Ok(());
    }
    let started_at = now_china();
    let result = network::probe_source(client, source);
    ensure_not_stopping(stop_requested)?;
    match result {
        Ok(()) => {
            database.save_source_probe_run(source, started_at, true, None)?;
            operations::log(
                "INFO",
                &format!("退避期低频健康探测成功：source={source}；保留原 API 退避"),
            );
        }
        Err(error) => {
            let message = format!("{error:#}");
            database.save_source_probe_run(source, started_at, false, Some(&message))?;
            operations::log(
                "WARN",
                &format!("退避期低频健康探测失败：source={source}，error={message}"),
            );
        }
    }
    Ok(())
}

pub(crate) fn sync_conclusion(
    started_at: ChinaDateTime,
    finished_at: ChinaDateTime,
    today_count: usize,
    event_count: usize,
    announcement_count: usize,
    successful_sources: &HashSet<&'static str>,
    missing_sources: &[&str],
) -> SyncConclusion {
    let kind = if missing_sources.is_empty() {
        if today_count == 0 {
            SyncConclusionKind::HealthyEmpty
        } else {
            SyncConclusionKind::HealthyNonempty
        }
    } else if today_count == 0 {
        SyncConclusionKind::Unknown
    } else {
        SyncConclusionKind::DegradedCached
    };
    let summary = match kind {
        SyncConclusionKind::HealthyEmpty => {
            format!(
                "同步完成：今日无新股（启用市场来源覆盖正常）；本轮更新 {event_count} 个任务、{announcement_count} 条公告链接"
            )
        }
        SyncConclusionKind::HealthyNonempty => {
            format!(
                "同步完成：今日任务 {today_count} 只；本轮更新 {event_count} 个任务、{announcement_count} 条公告链接"
            )
        }
        SyncConclusionKind::DegradedCached => format!(
            "已保留现有今日任务：来源覆盖不完整（{}）；本轮更新 {event_count} 个任务、{announcement_count} 条公告链接",
            missing_sources.join("、")
        ),
        _ => format!(
            "暂未获取到今日任务：来源覆盖不完整（{}）；本轮更新 {event_count} 个任务、{announcement_count} 条公告链接",
            missing_sources.join("、")
        ),
    };
    let mut successful_sources = successful_sources
        .iter()
        .map(|source| (*source).to_owned())
        .collect::<Vec<_>>();
    successful_sources.sort();
    SyncConclusion {
        kind,
        started_at,
        finished_at,
        today_count,
        event_count,
        announcement_count,
        successful_sources,
        missing_sources: missing_sources
            .iter()
            .map(|source| (*source).to_owned())
            .collect(),
        summary,
    }
}

pub(crate) fn persist_reconciled_group(
    database: &Database,
    resolved: IpoEvent,
    candidates: &[Candidate],
    documents: &[AnnouncementDocument],
) -> Result<IpoEvent> {
    // announcement_documents.ipo_event_id has a foreign key to ipo_events.id,
    // so a newly discovered event must be committed before its documents.
    let saved = database.upsert_event(resolved)?;
    database.replace_field_sources(&saved.id, candidates)?;
    for document in documents {
        database.save_announcement(document)?;
    }
    Ok(saved)
}

pub(crate) fn should_check_announcements(event: &IpoEvent) -> bool {
    let today = now_china().date_naive();
    event.apply_date.is_some_and(|date| {
        date >= today - chrono::Duration::days(7) && date <= today + chrono::Duration::days(45)
    })
}

use super::*;

impl Database {
    pub fn manual_overrides(
        &self,
        event_id: &str,
        version: i32,
    ) -> Result<Vec<ManualOverrideEntry>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id,field_name,override_value,reason,announcement_document_id,created_at,revoked_at FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 ORDER BY created_at DESC,id DESC",
        )?;
        let rows = statement.query_map(params![event_id, version], |row| {
            let created_at: String = row.get(5)?;
            let revoked_at: Option<String> = row.get(6)?;
            Ok(ManualOverrideEntry {
                id: row.get(0)?,
                field_name: row.get(1)?,
                override_value: row.get(2)?,
                reason: row.get(3)?,
                announcement_document_id: row.get(4)?,
                created_at: parse_dt(&created_at).map_err(to_sql_error)?,
                revoked_at: revoked_at.as_deref().and_then(|value| parse_dt(value).ok()),
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn apply_manual_override(
        &self,
        event_id: &str,
        version: i32,
        field: &str,
        value: &str,
        reason: &str,
        announcement_id: Option<&str>,
    ) -> Result<()> {
        let value = normalize_manual_override(field, value)?;
        let reason = reason.trim();
        if reason.is_empty() {
            bail!("人工覆盖必须填写核验理由");
        }
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut event = event_from_connection(&transaction, event_id)?.context("发行任务不存在")?;
        if event.event_version != version {
            bail!("发行任务版本已变化，请刷新后重试");
        }
        if let Some(announcement_id) = announcement_id.filter(|value| !value.trim().is_empty()) {
            let belongs_to_event = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM announcement_documents WHERE id=?1 AND ipo_event_id=?2)",
                params![announcement_id, event_id],
                |row| row.get::<_, i32>(0),
            )? != 0;
            if !belongs_to_event {
                bail!("所选依据公告不存在或不属于当前发行任务");
            }
        }
        transaction.execute(
            "INSERT INTO manual_overrides(ipo_event_id,event_version,field_name,override_value,reason,announcement_document_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![event_id, version, field, limit(&value, 200), limit(reason, 500), announcement_id, format_dt(now)],
        )?;
        apply_manual_overrides(&transaction, &mut event)?;
        let settings = settings_from_connection(&transaction)?;
        reconcile_schedule_tx(&transaction, &event, &settings, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn revoke_manual_override(
        &self,
        event_id: &str,
        version: i32,
        override_id: i64,
    ) -> Result<()> {
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event = event_from_connection(&transaction, event_id)?.context("发行任务不存在")?;
        if event.event_version != version {
            bail!("发行任务版本已变化，请刷新后重试");
        }
        let changed = transaction.execute(
            "UPDATE manual_overrides SET revoked_at=?1 WHERE id=?2 AND ipo_event_id=?3 AND event_version=?4 AND revoked_at IS NULL",
            params![format_dt(now), override_id, event_id, version],
        )?;
        if changed == 0 {
            bail!("人工覆盖记录不存在、已经撤销或属于旧的数据版本");
        }
        // 撤销后必须重载事件：此前读出的事件带着被撤销字段的覆盖值，
        // 直接重规划会让旧覆盖（如 Postponed）残留到提醒计划中。
        let mut event = event_from_connection(&transaction, event_id)?.context("发行任务不存在")?;
        if event.event_version != version {
            bail!("发行任务版本已变化，请刷新后重试");
        }
        apply_manual_overrides(&transaction, &mut event)?;
        let settings = settings_from_connection(&transaction)?;
        reconcile_schedule_tx(&transaction, &event, &settings, now)?;
        transaction.commit()?;
        Ok(())
    }
}

pub(super) fn apply_manual_overrides(connection: &Connection, event: &mut IpoEvent) -> Result<()> {
    let mut statement = connection.prepare("SELECT field_name,override_value FROM manual_overrides WHERE ipo_event_id=?1 AND event_version=?2 AND revoked_at IS NULL ORDER BY id")?;
    let rows = statement.query_map(params![event.id, event.event_version], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    event.manual_override_fields.clear();
    for row in rows {
        let (field, value) = row?;
        if !event.manual_override_fields.contains(&field) {
            event.manual_override_fields.push(field.clone());
        }
        match field.as_str() {
            "ApplyCode" => event.apply_code = Some(value),
            "ApplyDate" => event.apply_date = parse_date_value(&value),
            "IssuePrice" => event.issue_price = value.parse().ok(),
            "LotSize" => event.lot_size = value.parse().ok(),
            "MaxApplyQuantity" => event.max_apply_quantity = value.parse().ok(),
            "OfficialSessions" => event.sessions = parse_override_sessions(&value, event.exchange)?,
            "IssueStatus" => {
                event.status =
                    parse_issue_status_override(&value).context("人工覆盖的发行状态无效")?
            }
            _ => {}
        }
    }
    Ok(())
}

/// 发行状态人工覆盖必须跨关键数据版本保持生效，直到用户显式修改或撤销。
/// 只继承最后一条活跃 IssueStatus 覆盖；其他字段仍沿用“关键版本变化后重新核验”的
/// 既有语义。
pub(super) fn carry_forward_issue_status_override(
    connection: &Connection,
    event_id: &str,
    previous_version: i32,
    current_version: i32,
) -> Result<()> {
    connection.execute(
        "INSERT INTO manual_overrides(ipo_event_id,event_version,field_name,override_value,reason,announcement_document_id,created_at)
         SELECT ipo_event_id,?1,field_name,override_value,reason,announcement_document_id,created_at
         FROM manual_overrides
         WHERE id=(
             SELECT id FROM manual_overrides
             WHERE ipo_event_id=?2 AND event_version=?3 AND field_name='IssueStatus' AND revoked_at IS NULL
             ORDER BY id DESC LIMIT 1
         )",
        params![current_version, event_id, previous_version],
    )?;
    Ok(())
}

pub(super) fn normalize_manual_override(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    match field {
        "ApplyCode"
            if value.chars().count() == 6
                && value.chars().all(|character| character.is_ascii_digit()) =>
        {
            Ok(value.to_owned())
        }
        "ApplyDate" => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(format_date)
            .context("申购日期格式无效，请使用 yyyy-MM-dd"),
        "IssuePrice" => {
            let number: f64 = value.parse().context("发行价格必须是大于 0 的数字")?;
            if !number.is_finite() || number <= 0.0 {
                bail!("发行价格必须是大于 0 的数字");
            }
            Ok(number.to_string())
        }
        "LotSize" => normalize_positive_integer(value, "申购单位必须是大于 0 的整数股数"),
        "MaxApplyQuantity" => normalize_positive_integer(value, "申购上限必须是大于 0 的整数股数"),
        "OfficialSessions" => normalize_session_text(value),
        "IssueStatus" => parse_issue_status_override(value)
            .map(issue_status_override_text)
            .context("发行状态无效"),
        "ApplyCode" => bail!("申购代码必须是 6 位数字"),
        _ => bail!("该字段不允许人工覆盖：{field}"),
    }
}

pub(super) fn normalize_positive_integer(value: &str, error: &str) -> Result<String> {
    let number: i64 = value.parse().with_context(|| error.to_owned())?;
    if number <= 0 {
        bail!(error.to_owned());
    }
    Ok(number.to_string())
}

pub(super) fn normalize_session_text(value: &str) -> Result<String> {
    let normalized = value
        .replace('，', ",")
        .replace('；', ",")
        .replace(';', ",");
    let pairs = normalized
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !(1..=3).contains(&pairs.len()) {
        bail!("官方时段格式无效，例如 09:30-11:30,13:00-15:00");
    }
    let mut result = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let pair = pair.replace('—', "-").replace('–', "-").replace("至", "-");
        let (start, end) = pair
            .split_once('-')
            .context("官方时段格式无效，例如 09:30-11:30,13:00-15:00")?;
        let start = parse_time_value(start.trim()).context("官方时段开始时间无效")?;
        let end = parse_time_value(end.trim()).context("官方时段结束时间无效")?;
        if start >= end {
            bail!("官方时段开始时间必须早于结束时间");
        }
        result.push(format!("{}-{}", start.format("%H:%M"), end.format("%H:%M")));
    }
    Ok(result.join(","))
}

pub(super) fn parse_override_sessions(
    value: &str,
    exchange: Exchange,
) -> Result<Vec<SubscriptionSession>> {
    let normalized = normalize_session_text(value)?;
    normalized
        .split(',')
        .enumerate()
        .map(|(index, pair)| {
            let (start, end) = pair.split_once('-').context("人工覆盖的官方时段无效")?;
            Ok(SubscriptionSession {
                session_number: index as i32 + 1,
                official_start: parse_time_value(start)
                    .context("人工覆盖的官方时段开始时间无效")?,
                official_end: parse_time_value(end).context("人工覆盖的官方时段结束时间无效")?,
                broker_accept_start: None,
                safety_cutoff: None,
                funding_mode: if exchange == Exchange::Beijing {
                    FundingMode::FullCash
                } else {
                    FundingMode::MarketValue
                },
                allocation_time_sensitive: exchange == Exchange::Beijing,
                source: "manual-override".into(),
                source_published_at: None,
            })
        })
        .collect()
}

pub(super) fn parse_time_value(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(value, "%H:%M:%S"))
        .ok()
}

pub(super) fn parse_issue_status_override(value: &str) -> Option<IssueStatus> {
    match value.trim() {
        "即将发行" | "正常发行" | "Upcoming" => Some(IssueStatus::Upcoming),
        "申购中" | "Active" => Some(IssueStatus::Active),
        // 「延期发行」是可恢复状态；「暂缓发行/暂停发行」与网络采集口径一致，视为中止。
        "延期发行" | "Postponed" => Some(IssueStatus::Postponed),
        "暂缓发行" | "暂停发行" | "中止发行" | "Suspended" => {
            Some(IssueStatus::Suspended)
        }
        "终止发行" | "Terminated" => Some(IssueStatus::Terminated),
        "发行完成" | "Completed" => Some(IssueStatus::Completed),
        _ => None,
    }
}

pub(super) fn issue_status_override_text(status: IssueStatus) -> String {
    match status {
        IssueStatus::Upcoming => "Upcoming",
        IssueStatus::Active => "Active",
        IssueStatus::Postponed => "Postponed",
        IssueStatus::Suspended => "Suspended",
        IssueStatus::Terminated => "Terminated",
        IssueStatus::Completed => "Completed",
        IssueStatus::Unknown => "Unknown",
    }
    .to_owned()
}

pub(super) fn parse_date_value(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

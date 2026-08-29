use super::*;

pub(super) fn map_event(row: &Row<'_>) -> rusqlite::Result<IpoEvent> {
    let sessions_json: String = row.get("sessions_json")?;
    let sessions = serde_json::from_str(&sessions_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            row.as_ref().column_index("sessions_json").unwrap_or(22),
            rusqlite::types::Type::Text,
            error.into(),
        )
    })?;
    Ok(IpoEvent {
        id: row.get("id")?,
        exchange: Exchange::from_i32_tracked("exchange", row.get("exchange")?),
        board: Board::from_i32_tracked("board", row.get("board")?),
        security_code: row.get("security_code")?,
        apply_code: row.get("apply_code")?,
        legacy_code: row.get("legacy_code")?,
        name: row.get("name")?,
        apply_date: parse_optional_date(row.get("apply_date")?),
        issue_price: row.get("issue_price")?,
        lot_size: row.get("lot_size")?,
        max_apply_quantity: row.get("max_apply_quantity")?,
        required_market_value: row.get("required_market_value")?,
        required_cash: row.get("required_cash")?,
        ballot_date: parse_optional_date(row.get("ballot_date")?),
        payment_date: parse_optional_date(row.get("payment_date")?),
        listing_date: parse_optional_date(row.get("listing_date")?),
        status: IssueStatus::from_i32_tracked("issue_status", row.get("issue_status")?),
        lifecycle_status: LifecycleStatus::from_i32_tracked(
            "lifecycle_status",
            row.get("lifecycle_status")?,
        ),
        event_version: row.get("event_version")?,
        announcement_url: row.get("announcement_url")?,
        data_quality_status: DataQualityStatus::from_i32_tracked(
            "data_quality_status",
            row.get("data_quality_status")?,
        ),
        data_conflict: row.get::<_, i32>("data_conflict")? != 0,
        manual_override_fields: Vec::new(),
        sessions,
        first_seen_at: parse_dt(&row.get::<_, String>("first_seen_at")?).map_err(to_sql_error)?,
        updated_at: parse_dt(&row.get::<_, String>("updated_at")?).map_err(to_sql_error)?,
    })
}

pub(super) fn event_from_connection(connection: &Connection, id: &str) -> Result<Option<IpoEvent>> {
    let mut event = connection
        .query_row("SELECT * FROM ipo_events WHERE id=?1", [id], map_event)
        .optional()?;
    if let Some(value) = &mut event {
        apply_manual_overrides(connection, value)?;
    }
    Ok(event)
}

pub(super) fn map_delivery(row: &Row<'_>) -> rusqlite::Result<ReminderDelivery> {
    let event = map_event_prefixed(row)?;
    Ok(ReminderDelivery {
        outbox_id: row.get(0)?,
        due_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
        level: ReminderLevel::from_i32_tracked("level", row.get(2)?),
        dedupe_key: row.get(3)?,
        attempt_count: row.get(4)?,
        message: row.get(5)?,
        event,
    })
}

pub(super) fn map_secondary_delivery(
    row: &Row<'_>,
) -> rusqlite::Result<SecondaryNotificationDelivery> {
    let event = map_event_prefixed(row)?;
    Ok(SecondaryNotificationDelivery {
        id: row.get(0)?,
        reminder_outbox_id: row.get(1)?,
        request_attempt_id: 0,
        provider: SecondaryNotificationProvider::from_i32_tracked("provider", row.get(2)?),
        due_at: parse_dt(&row.get::<_, String>(3)?).map_err(to_sql_error)?,
        level: ReminderLevel::from_i32_tracked("level", row.get(4)?),
        attempt_count: row.get(5)?,
        message: row.get(6)?,
        event,
    })
}

/// joined 查询的事件列按显式别名读取（M7）：查询里存在 o.id/r.id 等同名列，
/// 裸 row.get("id") 会串列；投影见 EVENT_COLUMNS 中 `event_*` 别名。
pub(super) fn map_event_prefixed(row: &Row<'_>) -> rusqlite::Result<IpoEvent> {
    let sessions: String = row.get("event_sessions_json")?;
    let sessions = serde_json::from_str(&sessions).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            row.as_ref()
                .column_index("event_sessions_json")
                .unwrap_or(0),
            rusqlite::types::Type::Text,
            error.into(),
        )
    })?;
    Ok(IpoEvent {
        id: row.get("event_id")?,
        exchange: Exchange::from_i32_tracked("exchange", row.get("event_exchange")?),
        board: Board::from_i32_tracked("board", row.get("event_board")?),
        security_code: row.get("event_security_code")?,
        apply_code: row.get("event_apply_code")?,
        legacy_code: row.get("event_legacy_code")?,
        name: row.get("event_name")?,
        apply_date: parse_optional_date(row.get("event_apply_date")?),
        issue_price: row.get("event_issue_price")?,
        lot_size: row.get("event_lot_size")?,
        max_apply_quantity: row.get("event_max_apply_quantity")?,
        required_market_value: row.get("event_required_market_value")?,
        required_cash: row.get("event_required_cash")?,
        ballot_date: parse_optional_date(row.get("event_ballot_date")?),
        payment_date: parse_optional_date(row.get("event_payment_date")?),
        listing_date: parse_optional_date(row.get("event_listing_date")?),
        status: IssueStatus::from_i32_tracked("issue_status", row.get("event_issue_status")?),
        lifecycle_status: LifecycleStatus::from_i32_tracked(
            "lifecycle_status",
            row.get("event_lifecycle_status")?,
        ),
        event_version: row.get("event_event_version")?,
        announcement_url: row.get("event_announcement_url")?,
        data_quality_status: DataQualityStatus::from_i32_tracked(
            "data_quality_status",
            row.get("event_data_quality_status")?,
        ),
        data_conflict: row.get::<_, i32>("event_data_conflict")? != 0,
        manual_override_fields: Vec::new(),
        sessions,
        first_seen_at: parse_dt(&row.get::<_, String>("event_first_seen_at")?)
            .map_err(to_sql_error)?,
        updated_at: parse_dt(&row.get::<_, String>("event_updated_at")?).map_err(to_sql_error)?,
    })
}

pub(super) fn parse_optional_date(value: Option<String>) -> Option<NaiveDate> {
    value.and_then(|v| NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok())
}
pub(super) fn format_date(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}
pub(super) fn format_dt(value: ChinaDateTime) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}
pub(super) fn parse_dt(value: &str) -> Result<ChinaDateTime> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&crate::core::china_offset()))
}
pub(super) fn to_sql_error(error: anyhow::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error.into())
}
pub(super) fn limit(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

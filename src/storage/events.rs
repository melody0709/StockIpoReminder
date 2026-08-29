use super::*;

impl Database {
    pub fn event(&self, id: &str) -> Result<Option<IpoEvent>> {
        let connection = self.open()?;
        event_from_connection(&connection, id)
    }

    pub fn events(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<IpoEvent>> {
        let connection = self.open()?;
        let mut statement = connection.prepare("SELECT * FROM ipo_events WHERE apply_date BETWEEN ?1 AND ?2 ORDER BY apply_date,exchange,security_code")?;
        let rows = statement.query_map(params![format_date(from), format_date(to)], map_event)?;
        let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        for event in &mut events {
            apply_manual_overrides(&connection, event)?;
        }
        Ok(events)
    }

    pub fn today_events(&self) -> Result<Vec<IpoEvent>> {
        let today = now_china().date_naive();
        self.events(today, today)
    }

    pub fn future_events(&self, days: i64) -> Result<Vec<IpoEvent>> {
        let today = now_china().date_naive();
        self.events(
            today + chrono::Duration::days(1),
            today + chrono::Duration::days(days),
        )
    }

    pub fn upsert_event(&self, mut event: IpoEvent) -> Result<IpoEvent> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut change_notification = None::<(bool, String, ChinaDateTime)>;
        let existing = transaction
            .query_row(
                "SELECT * FROM ipo_events WHERE id=?1",
                [&event.id],
                map_event,
            )
            .optional()?;
        if let Some(previous) = &existing {
            retain_known_optional_fields(previous, &mut event);
            let critical_reason = critical_change_reason(previous, &event);
            let noncritical_reason = noncritical_change_reason(previous, &event);
            let critical_changed = critical_reason.is_some();
            event.event_version = previous.event_version + i32::from(critical_changed);
            event.first_seen_at = previous.first_seen_at;
            change_notification = critical_reason
                .as_ref()
                .map(|reason| (true, reason.clone(), previous.updated_at))
                .or_else(|| noncritical_reason.map(|reason| (false, reason, previous.updated_at)));
            if critical_changed && previous.lifecycle_status == LifecycleStatus::Acknowledged {
                event.lifecycle_status = LifecycleStatus::AcknowledgedNeedsReview;
                transaction.execute("UPDATE acknowledgements SET needs_review_at=?1,review_reason=?2 WHERE ipo_event_id=?3 AND event_version=?4 AND revoked_at IS NULL", params![format_dt(event.updated_at), critical_reason.as_deref(), event.id, previous.event_version])?;
            } else if previous.lifecycle_status == LifecycleStatus::Acknowledged {
                event.lifecycle_status = LifecycleStatus::Acknowledged;
            }
        }
        if let Some(previous) = &existing
            && persisted_event_fields_equal(previous, &event)
        {
            return Ok(previous.clone());
        }
        let sessions = serde_json::to_string(&event.sessions)?;
        transaction.execute(
            UPSERT_EVENT_SQL,
            params![
                event.id,
                event.exchange as i32,
                event.board as i32,
                event.security_code,
                event.apply_code,
                event.legacy_code,
                event.name,
                event.apply_date.map(format_date),
                event.issue_price,
                event.lot_size,
                event.max_apply_quantity,
                event.required_market_value,
                event.required_cash,
                event.ballot_date.map(format_date),
                event.payment_date.map(format_date),
                event.listing_date.map(format_date),
                event.status as i32,
                event.lifecycle_status as i32,
                event.event_version,
                event.announcement_url,
                event.data_quality_status as i32,
                i32::from(event.data_conflict),
                sessions,
                format_dt(event.first_seen_at),
                format_dt(event.updated_at)
            ],
        )?;
        if let Some(previous) = &existing
            && previous.event_version != event.event_version
        {
            carry_forward_issue_status_override(
                &transaction,
                &event.id,
                previous.event_version,
                event.event_version,
            )?;
        }
        let settings = settings_from_connection(&transaction)?;
        transaction.execute(
            "UPDATE reminder_outbox SET delivery_state=?1,lease_until=NULL,updated_at=?2 WHERE ipo_event_id=?3 AND event_version<>?4 AND delivery_state IN (?5,?6,?7)",
            params![
                DeliveryState::Cancelled as i32,
                format_dt(event.updated_at),
                event.id,
                event.event_version,
                DeliveryState::Pending as i32,
                DeliveryState::Leased as i32,
                DeliveryState::Failed as i32,
            ],
        )?;
        // 重规划必须使用人工覆盖生效后的事件视图：否则活跃的 Postponed 等
        // IssueStatus 覆盖会被下一次同步的源数据重规划悄悄撤销。
        let mut planning = event.clone();
        apply_manual_overrides(&transaction, &mut planning)?;
        reconcile_schedule_tx(&transaction, &planning, &settings, now_china())?;
        if let Some((critical, reason, previous_updated_at)) = change_notification {
            enqueue_change_notification_tx(
                &transaction,
                &event,
                critical,
                &reason,
                previous_updated_at,
            )?;
        }
        transaction.commit()?;
        Ok(event)
    }

    pub fn refresh_lifecycle(&self) -> Result<bool> {
        let now = now_china();
        let settings = self.settings()?;
        let mut changed = false;
        for event in self.future_events(60)? {
            if event.lifecycle_status == LifecycleStatus::Acknowledged {
                self.revoke_acknowledgement_at(&event.id, event.event_version, now)?;
                changed = true;
            }
        }
        for mut event in self.today_events()? {
            let next = if event.lifecycle_status == LifecycleStatus::Scheduled {
                Some(LifecycleStatus::ActiveUnconfirmed)
            } else if matches!(
                event.lifecycle_status,
                LifecycleStatus::ActiveUnconfirmed | LifecycleStatus::AcknowledgedNeedsReview
            ) && now
                >= crate::core::at(
                    now.date_naive(),
                    crate::core::effective_cutoff(&event, &settings),
                )
            {
                Some(LifecycleStatus::ExpiredUnconfirmed)
            } else {
                None
            };
            if let Some(status) = next {
                event.lifecycle_status = status;
                event.updated_at = now;
                self.open()?.execute("UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4",params![status as i32,format_dt(now),event.id,event.event_version])?;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn next_lifecycle_transition_at(
        &self,
        now: ChinaDateTime,
    ) -> Result<Option<ChinaDateTime>> {
        let today = now.date_naive();
        let settings = self.settings()?;
        let mut next = None;
        for event in self.events(today, today)? {
            if event.lifecycle_status == LifecycleStatus::Scheduled {
                return Ok(Some(now));
            }
            if matches!(
                event.lifecycle_status,
                LifecycleStatus::ActiveUnconfirmed | LifecycleStatus::AcknowledgedNeedsReview
            ) {
                let cutoff =
                    crate::core::at(today, crate::core::effective_cutoff(&event, &settings));
                if cutoff <= now {
                    return Ok(Some(now));
                }
                next = Some(next.map_or(cutoff, |current: ChinaDateTime| current.min(cutoff)));
            }
        }

        let connection = self.open()?;
        let has_invalid_future_acknowledgement = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM ipo_events
                WHERE apply_date>?1 AND apply_date<=?2 AND lifecycle_status=?3
            )",
            params![
                format_date(today),
                format_date(today + chrono::Duration::days(60)),
                LifecycleStatus::Acknowledged as i32,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if has_invalid_future_acknowledgement {
            return Ok(Some(now));
        }

        let next_date: Option<String> = connection.query_row(
            "SELECT MIN(apply_date) FROM ipo_events
             WHERE apply_date>?1 AND lifecycle_status IN (?2,?3,?4)",
            params![
                format_date(today),
                LifecycleStatus::Scheduled as i32,
                LifecycleStatus::ActiveUnconfirmed as i32,
                LifecycleStatus::AcknowledgedNeedsReview as i32,
            ],
            |row| row.get(0),
        )?;
        if let Some(date) = next_date.and_then(|value| parse_date_value(&value)) {
            let boundary = crate::core::at(date, crate::model::time(0, 0));
            next = Some(next.map_or(boundary, |current| current.min(boundary)));
        }
        Ok(next)
    }
}

pub(super) fn persisted_event_fields_equal(previous: &IpoEvent, current: &IpoEvent) -> bool {
    previous.id == current.id
        && previous.exchange == current.exchange
        && previous.board == current.board
        && previous.security_code == current.security_code
        && previous.apply_code == current.apply_code
        && previous.legacy_code == current.legacy_code
        && previous.name == current.name
        && previous.apply_date == current.apply_date
        && previous.issue_price == current.issue_price
        && previous.lot_size == current.lot_size
        && previous.max_apply_quantity == current.max_apply_quantity
        && previous.required_market_value == current.required_market_value
        && previous.required_cash == current.required_cash
        && previous.ballot_date == current.ballot_date
        && previous.payment_date == current.payment_date
        && previous.listing_date == current.listing_date
        && previous.status == current.status
        && previous.lifecycle_status == current.lifecycle_status
        && previous.event_version == current.event_version
        && previous.announcement_url == current.announcement_url
        && previous.data_quality_status == current.data_quality_status
        && previous.data_conflict == current.data_conflict
        && previous.sessions == current.sessions
        && previous.first_seen_at == current.first_seen_at
}

pub(super) fn retain_known_optional_fields(previous: &IpoEvent, current: &mut IpoEvent) {
    if current.apply_code.is_none() {
        current.apply_code.clone_from(&previous.apply_code);
    }
    if current.legacy_code.is_none() {
        current.legacy_code.clone_from(&previous.legacy_code);
    }
    if current.apply_date.is_none() {
        current.apply_date = previous.apply_date;
    }
    if current.issue_price.is_none() {
        current.issue_price = previous.issue_price;
    }
    if current.lot_size.is_none() {
        current.lot_size = previous.lot_size;
    }
    if current.max_apply_quantity.is_none() {
        current.max_apply_quantity = previous.max_apply_quantity;
    }
    if current.required_market_value.is_none() {
        current.required_market_value = previous.required_market_value;
    }
    if current.required_cash.is_none() {
        current.required_cash = previous.required_cash;
    }
    if current.ballot_date.is_none() {
        current.ballot_date = previous.ballot_date;
    }
    if current.payment_date.is_none() {
        current.payment_date = previous.payment_date;
    }
    if current.listing_date.is_none() {
        current.listing_date = previous.listing_date;
    }
    if current.announcement_url.is_none() {
        current
            .announcement_url
            .clone_from(&previous.announcement_url);
    }
}

pub(super) fn enqueue_change_notification_tx(
    transaction: &rusqlite::Transaction<'_>,
    event: &IpoEvent,
    critical: bool,
    reason: &str,
    previous_updated_at: ChinaDateTime,
) -> Result<()> {
    let message = if critical {
        format!("{reason}。请重新核对任务详情；若此前已确认，必须重新确认。")
    } else {
        format!("{reason}。关键申购条件未变化，本次变更仅提醒一次。")
    };
    let fingerprint = sha256(format!(
        "{}|{}|{}|{}|{}",
        event.id,
        event.event_version,
        previous_updated_at.timestamp_micros(),
        event.updated_at.timestamp_micros(),
        reason
    ));
    let dedupe_key = format!(
        "{}:{}:change:{}",
        event.id,
        event.event_version,
        &fingerprint[..24]
    );
    transaction.execute(
        "INSERT OR IGNORE INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,delivery_state,message,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?3,?3)",
        params![
            event.id,
            event.event_version,
            format_dt(event.updated_at),
            ReminderLevel::DataChanged as i32,
            dedupe_key,
            DeliveryState::Pending as i32,
            limit(&message, 1000),
        ],
    )?;
    Ok(())
}

/// 更新残留清理的保守年龄阈值：与临时目录清理一致取 24 小时。
pub(super) fn reconcile_schedule_tx(
    tx: &rusqlite::Transaction<'_>,
    event: &IpoEvent,
    settings: &AppSettings,
    now: ChinaDateTime,
) -> Result<()> {
    let planned = plan_reminders(event, settings, now);
    tx.execute("UPDATE reminder_outbox SET delivery_state=4,updated_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND reminder_level<>?4 AND delivery_state IN (0,1,5)",params![format_dt(now),event.id,event.event_version,ReminderLevel::DataChanged as i32])?;
    for item in planned {
        tx.execute("INSERT INTO reminder_outbox(ipo_event_id,event_version,due_at,reminder_level,dedupe_key,delivery_state,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,0,?6,?6) ON CONFLICT(dedupe_key) DO UPDATE SET delivery_state=CASE WHEN reminder_outbox.delivery_state IN (2,3) THEN reminder_outbox.delivery_state ELSE 0 END,due_at=excluded.due_at,reminder_level=excluded.reminder_level,updated_at=excluded.updated_at",params![item.event_id,item.event_version,format_dt(item.due_at),item.level as i32,item.dedupe_key,format_dt(now)])?;
    }
    Ok(())
}

pub(super) const UPSERT_EVENT_SQL: &str = "INSERT INTO ipo_events(id,exchange,board,security_code,apply_code,legacy_code,name,apply_date,issue_price,lot_size,max_apply_quantity,required_market_value,required_cash,ballot_date,payment_date,listing_date,issue_status,lifecycle_status,event_version,announcement_url,data_quality_status,data_conflict,sessions_json,first_seen_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25) ON CONFLICT(id) DO UPDATE SET exchange=excluded.exchange,board=excluded.board,security_code=excluded.security_code,apply_code=COALESCE(excluded.apply_code,ipo_events.apply_code),legacy_code=COALESCE(excluded.legacy_code,ipo_events.legacy_code),name=excluded.name,apply_date=COALESCE(excluded.apply_date,ipo_events.apply_date),issue_price=COALESCE(excluded.issue_price,ipo_events.issue_price),lot_size=COALESCE(excluded.lot_size,ipo_events.lot_size),max_apply_quantity=COALESCE(excluded.max_apply_quantity,ipo_events.max_apply_quantity),required_market_value=COALESCE(excluded.required_market_value,ipo_events.required_market_value),required_cash=COALESCE(excluded.required_cash,ipo_events.required_cash),ballot_date=COALESCE(excluded.ballot_date,ipo_events.ballot_date),payment_date=COALESCE(excluded.payment_date,ipo_events.payment_date),listing_date=COALESCE(excluded.listing_date,ipo_events.listing_date),issue_status=excluded.issue_status,lifecycle_status=excluded.lifecycle_status,event_version=excluded.event_version,announcement_url=COALESCE(excluded.announcement_url,ipo_events.announcement_url),data_quality_status=excluded.data_quality_status,data_conflict=excluded.data_conflict,sessions_json=excluded.sessions_json,updated_at=excluded.updated_at";

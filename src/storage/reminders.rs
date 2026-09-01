use super::*;

impl Database {
    pub fn next_local_delivery_at(&self) -> Result<Option<ChinaDateTime>> {
        let value: Option<String> = self.open()?.query_row(
            "SELECT MIN(
                CASE
                    WHEN o.delivery_state=?1 THEN COALESCE(o.lease_until,o.due_at)
                    WHEN o.lease_until IS NULL OR o.lease_until<o.due_at THEN o.due_at
                    ELSE o.lease_until
                END
             )
             FROM reminder_outbox o
             JOIN ipo_events e
               ON e.id=o.ipo_event_id AND e.event_version=o.event_version
             WHERE o.delivery_state IN (?1,?2,?3)",
            params![
                DeliveryState::Leased as i32,
                DeliveryState::Pending as i32,
                DeliveryState::Failed as i32,
            ],
            |row| row.get(0),
        )?;
        value.map(|value| parse_dt(&value)).transpose()
    }

    pub fn claim_due(&self, limit: usize) -> Result<Vec<ReminderDelivery>> {
        self.claim_due_at(limit, now_china())
    }

    pub(super) fn claim_due_at(
        &self,
        limit: usize,
        now: ChinaDateTime,
    ) -> Result<Vec<ReminderDelivery>> {
        let mut connection = self.open()?;
        let formatted_now = format_dt(now);
        let has_due_work = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM reminder_outbox
                WHERE (delivery_state=?1 AND lease_until<=?2)
                   OR (delivery_state IN (?3,?4) AND due_at<=?2 AND (lease_until IS NULL OR lease_until<=?2))
            )",
            params![
                DeliveryState::Leased as i32,
                &formatted_now,
                DeliveryState::Pending as i32,
                DeliveryState::Failed as i32,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if !has_due_work {
            return Ok(Vec::new());
        }
        let lease = now + chrono::Duration::minutes(2);
        let tx = connection.transaction()?;
        tx.execute("UPDATE reminder_outbox SET delivery_state=0,lease_until=NULL,updated_at=?1 WHERE delivery_state=1 AND lease_until<=?1",[&formatted_now])?;
        tx.execute(
            "UPDATE reminder_outbox AS stale
             SET delivery_state=?1,lease_until=NULL,updated_at=?2
             WHERE stale.delivery_state IN (?3,?4)
               AND stale.due_at<=?2
               AND stale.reminder_level BETWEEN ?5 AND ?6
               AND EXISTS(
                   SELECT 1 FROM reminder_outbox AS newer
                   WHERE newer.ipo_event_id=stale.ipo_event_id
                     AND newer.event_version=stale.event_version
                     AND newer.delivery_state IN (?3,?4,?7)
                     AND newer.due_at<=?2
                     AND newer.reminder_level BETWEEN ?5 AND ?6
                     AND (newer.due_at>stale.due_at OR (newer.due_at=stale.due_at AND newer.reminder_level>stale.reminder_level))
               )",
            params![
                DeliveryState::Collapsed as i32,
                formatted_now,
                DeliveryState::Pending as i32,
                DeliveryState::Failed as i32,
                ReminderLevel::Advance as i32,
                ReminderLevel::Final as i32,
                DeliveryState::Delivered as i32,
            ],
        )?;
        let ids: Vec<i64> = {
            let mut s=tx.prepare("SELECT o.id FROM reminder_outbox o JOIN ipo_events e ON e.id=o.ipo_event_id AND e.event_version=o.event_version WHERE o.delivery_state IN (0,5) AND o.due_at<=?1 AND (o.lease_until IS NULL OR o.lease_until<=?1) ORDER BY o.due_at,o.id LIMIT ?2")?;
            s.query_map(params![formatted_now, limit as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let settings = settings_from_connection(&tx)?;
        let mut deliveries = Vec::new();
        for id in ids {
            let mut delivery = tx.query_row("SELECT o.id,o.due_at,o.reminder_level,o.dedupe_key,o.attempt_count,o.message,e.id AS event_id,e.exchange AS event_exchange,e.board AS event_board,e.security_code AS event_security_code,e.apply_code AS event_apply_code,e.legacy_code AS event_legacy_code,e.name AS event_name,e.apply_date AS event_apply_date,e.issue_price AS event_issue_price,e.lot_size AS event_lot_size,e.max_apply_quantity AS event_max_apply_quantity,e.required_market_value AS event_required_market_value,e.required_cash AS event_required_cash,e.ballot_date AS event_ballot_date,e.payment_date AS event_payment_date,e.listing_date AS event_listing_date,e.issue_status AS event_issue_status,e.lifecycle_status AS event_lifecycle_status,e.event_version AS event_event_version,e.announcement_url AS event_announcement_url,e.data_quality_status AS event_data_quality_status,e.data_conflict AS event_data_conflict,e.sessions_json AS event_sessions_json,e.first_seen_at AS event_first_seen_at,e.updated_at AS event_updated_at FROM reminder_outbox o JOIN ipo_events e ON e.id=o.ipo_event_id AND e.event_version=o.event_version WHERE o.id=?1",[id],map_delivery)?;
            let subscription_interruption = delivery.level as i32 >= ReminderLevel::Advance as i32
                && delivery.level as i32 <= ReminderLevel::DataChanged as i32;
            if subscription_interruption
                && !subscription_reminder_allowed_now(&delivery.event, &settings, now)
            {
                tx.execute(
                    "UPDATE reminder_outbox SET delivery_state=?1,lease_until=NULL,updated_at=?2 WHERE id=?3",
                    params![DeliveryState::Cancelled as i32, format_dt(now), id],
                )?;
                tx.execute(
                    "UPDATE secondary_notification_outbox
                     SET state=?1,lease_until=NULL,updated_at=?2
                     WHERE reminder_outbox_id=?3 AND state IN (?4,?5,?6)",
                    params![
                        SECONDARY_CANCELLED,
                        format_dt(now),
                        id,
                        SECONDARY_PENDING,
                        SECONDARY_LEASED,
                        SECONDARY_RETRYING,
                    ],
                )?;
                continue;
            }
            tx.execute("UPDATE reminder_outbox SET delivery_state=1,lease_until=?1,attempt_count=attempt_count+1,updated_at=?2 WHERE id=?3",params![format_dt(lease),format_dt(now),id])?;
            delivery.attempt_count += 1;
            deliveries.push(delivery);
        }
        if settings.secondary_notification_enabled
            && !matches!(
                settings.secondary_notification_provider,
                SecondaryNotificationProvider::Disabled | SecondaryNotificationProvider::Unknown
            )
        {
            for delivery in &deliveries {
                tx.execute(
                    "INSERT INTO secondary_notification_outbox(
                        reminder_outbox_id,provider,state,attempt_count,next_attempt_at,created_at,updated_at
                     ) VALUES(?1,?2,?3,0,?4,?4,?4)
                     ON CONFLICT(reminder_outbox_id,provider) DO UPDATE SET
                        state=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                   THEN excluded.state ELSE secondary_notification_outbox.state END,
                        attempt_count=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                           THEN 0 ELSE secondary_notification_outbox.attempt_count END,
                        next_attempt_at=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                             THEN excluded.next_attempt_at ELSE secondary_notification_outbox.next_attempt_at END,
                        lease_until=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                         THEN NULL ELSE secondary_notification_outbox.lease_until END,
                        last_error=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                        THEN NULL ELSE secondary_notification_outbox.last_error END,
                        delivered_at=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                          THEN NULL ELSE secondary_notification_outbox.delivered_at END,
                        updated_at=CASE WHEN secondary_notification_outbox.state IN (?5,?6)
                                        THEN excluded.updated_at ELSE secondary_notification_outbox.updated_at END",
                    params![
                        delivery.outbox_id,
                        settings.secondary_notification_provider as i32,
                        SECONDARY_PENDING,
                        format_dt(now),
                        SECONDARY_CANCELLED,
                        SECONDARY_EXHAUSTED,
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(deliveries)
    }

    pub fn complete_delivery(&self, delivery: &ReminderDelivery, channel: &str) -> Result<()> {
        let now = now_china();
        let mut c = self.open()?;
        let tx = c.transaction()?;
        let changed = tx.execute("UPDATE reminder_outbox SET delivery_state=2,delivered_at=?1,lease_until=NULL,updated_at=?1 WHERE id=?2 AND delivery_state=1",params![format_dt(now),delivery.outbox_id])?;
        if changed != 1 {
            bail!("提醒投递租约已失效或已完成")
        }
        tx.execute("INSERT INTO reminder_log(ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,dedupe_key,result) VALUES(?1,?2,?3,?4,?5,?6,'shown')",params![delivery.event.id,format_dt(delivery.due_at),format_dt(now),delivery.level as i32,channel,delivery.dedupe_key])?;
        tx.commit()?;
        Ok(())
    }
    pub fn fail_delivery(&self, id: i64, error: &str) -> Result<()> {
        self.fail_delivery_at(id, error, now_china())
    }

    pub(super) fn fail_delivery_at(&self, id: i64, error: &str, now: ChinaDateTime) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempt_count: i32 = transaction
            .query_row(
                "SELECT attempt_count FROM reminder_outbox WHERE id=?1 AND delivery_state=?2",
                params![id, DeliveryState::Leased as i32],
                |row| row.get(0),
            )
            .optional()?
            .context("提醒投递租约已失效或已完成")?;
        let retry = now + chrono::Duration::minutes(local_delivery_retry_minutes(attempt_count));
        let changed = transaction.execute("UPDATE reminder_outbox SET delivery_state=?1,lease_until=?2,last_error=?3,updated_at=?4 WHERE id=?5 AND delivery_state=?6",params![DeliveryState::Failed as i32,format_dt(retry),limit(error,1000),format_dt(now),id,DeliveryState::Leased as i32])?;
        if changed != 1 {
            bail!("提醒投递租约已失效或已完成")
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn recent_reminder_log(&self, limit: usize) -> Result<Vec<ReminderLogSummary>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,result FROM reminder_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.clamp(1, 200) as i64], |row| {
            Ok(ReminderLogSummary {
                event_id: row.get(0)?,
                scheduled_at: parse_dt(&row.get::<_, String>(1)?).map_err(to_sql_error)?,
                shown_at: parse_dt(&row.get::<_, String>(2)?).map_err(to_sql_error)?,
                reminder_level: ReminderLevel::from_i32_tracked("reminder_level", row.get(3)?),
                delivery_channel: row.get(4)?,
                result: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn reminder_state_summary(&self) -> Result<ReminderStateSummary> {
        let connection = self.open()?;
        let (pending, leased, delivered, collapsed, cancelled, failed) = connection.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN delivery_state=0 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=1 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=2 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=3 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=4 THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN delivery_state=5 THEN 1 ELSE 0 END),0)
             FROM reminder_outbox",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        let shown_last_seven_days = connection.query_row(
            "SELECT COUNT(*) FROM reminder_log WHERE shown_at>=?1",
            [format_dt(now_china() - chrono::Duration::days(7))],
            |row| row.get(0),
        )?;
        let latest_shown: Option<String> = connection
            .query_row(
                "SELECT shown_at FROM reminder_log ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let oldest_failed: Option<String> = connection.query_row(
            "SELECT MIN(updated_at) FROM reminder_outbox WHERE delivery_state=?1",
            [DeliveryState::Failed as i32],
            |row| row.get(0),
        )?;
        let latest_error: Option<String> = connection
            .query_row(
                "SELECT last_error FROM reminder_outbox WHERE delivery_state=?1 AND last_error IS NOT NULL ORDER BY updated_at DESC,id DESC LIMIT 1",
                [DeliveryState::Failed as i32],
                |row| row.get(0),
            )
            .optional()?;
        Ok(ReminderStateSummary {
            pending,
            leased,
            delivered,
            collapsed,
            cancelled,
            failed,
            oldest_failed_at: oldest_failed
                .as_deref()
                .and_then(|value| parse_dt(value).ok()),
            latest_error,
            shown_last_seven_days,
            latest_shown_at: latest_shown
                .as_deref()
                .and_then(|value| parse_dt(value).ok()),
        })
    }

    pub fn pending_count(&self) -> Result<i64> {
        Ok(self.open()?.query_row(
            "SELECT COUNT(*) FROM ipo_events WHERE apply_date=?1 AND lifecycle_status IN (?2,?3)",
            params![
                format_date(now_china().date_naive()),
                LifecycleStatus::ActiveUnconfirmed as i32,
                LifecycleStatus::AcknowledgedNeedsReview as i32
            ],
            |row| row.get(0),
        )?)
    }
}

pub(super) fn local_delivery_retry_minutes(attempt_count: i32) -> i64 {
    match attempt_count {
        i32::MIN..=1 => 1,
        2 => 5,
        3 => 15,
        _ => 30,
    }
}

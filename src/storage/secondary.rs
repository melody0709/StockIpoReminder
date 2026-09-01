use super::*;

impl Database {
    pub fn next_secondary_delivery_at(&self, now: ChinaDateTime) -> Result<Option<ChinaDateTime>> {
        let connection = self.open()?;
        let value: Option<String> = connection.query_row(
            "SELECT MIN(
                CASE WHEN state=?1 THEN COALESCE(lease_until,next_attempt_at)
                     ELSE next_attempt_at END
             )
             FROM secondary_notification_outbox
             WHERE state=?1 OR (state IN (?2,?3) AND attempt_count<?4)",
            params![
                SECONDARY_LEASED,
                SECONDARY_PENDING,
                SECONDARY_RETRYING,
                SECONDARY_MAX_ATTEMPTS,
            ],
            |row| row.get(0),
        )?;
        let Some(mut due_at) = value.map(|value| parse_dt(&value)).transpose()? else {
            return Ok(None);
        };

        let window_start = format_dt(now - chrono::Duration::hours(1));
        let attempts: i64 = connection.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [&window_start],
            |row| row.get(0),
        )?;
        if attempts >= SECONDARY_REQUESTS_PER_HOUR {
            let offset = attempts - SECONDARY_REQUESTS_PER_HOUR;
            let limiting_attempt: Option<String> = connection
                .query_row(
                    "SELECT attempted_at FROM secondary_notification_attempts
                     WHERE attempted_at>=?1
                     ORDER BY attempted_at,id
                     LIMIT 1 OFFSET ?2",
                    params![window_start, offset],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(limiting_attempt) = limiting_attempt {
                due_at = due_at.max(parse_dt(&limiting_attempt)? + chrono::Duration::hours(1));
            }
        }
        Ok(Some(due_at))
    }

    pub fn claim_secondary_due(&self, limit: usize) -> Result<Vec<SecondaryNotificationDelivery>> {
        self.claim_secondary_due_at(limit, now_china())
    }

    pub(super) fn claim_secondary_due_at(
        &self,
        limit: usize,
        now: ChinaDateTime,
    ) -> Result<Vec<SecondaryNotificationDelivery>> {
        let mut connection = self.open()?;
        let formatted_now = format_dt(now);
        let has_due_work = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM secondary_notification_outbox
                WHERE (state=?1 AND lease_until<=?2)
                   OR (state IN (?3,?4) AND attempt_count<?5 AND next_attempt_at<=?2)
            )",
            params![
                SECONDARY_LEASED,
                &formatted_now,
                SECONDARY_PENDING,
                SECONDARY_RETRYING,
                SECONDARY_MAX_ATTEMPTS,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if !has_due_work {
            return Ok(Vec::new());
        }
        let transaction = connection.transaction()?;
        prune_secondary_notification_history(&transaction, now)?;
        transaction.execute(
            "UPDATE secondary_notification_outbox SET state=?1,lease_until=NULL,next_attempt_at=?2,updated_at=?2 WHERE state=?3 AND lease_until<=?2",
            params![SECONDARY_RETRYING, formatted_now, SECONDARY_LEASED],
        )?;
        transaction.execute(
            "UPDATE secondary_notification_outbox SET state=?1,lease_until=NULL,updated_at=?2 WHERE state IN (?3,?4) AND NOT EXISTS(SELECT 1 FROM reminder_outbox r JOIN ipo_events e ON e.id=r.ipo_event_id AND e.event_version=r.event_version WHERE r.id=secondary_notification_outbox.reminder_outbox_id AND r.delivery_state NOT IN (?5,?6))",
            params![
                SECONDARY_CANCELLED,
                formatted_now,
                SECONDARY_PENDING,
                SECONDARY_RETRYING,
                DeliveryState::Cancelled as i32,
                DeliveryState::Collapsed as i32,
            ],
        )?;
        let requests_last_hour: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        if requests_last_hour >= SECONDARY_REQUESTS_PER_HOUR {
            transaction.commit()?;
            return Ok(Vec::new());
        }
        let provider: Option<i32> = transaction
            .query_row(
                "SELECT provider FROM secondary_notification_outbox WHERE state IN (?1,?2) AND attempt_count<?3 AND next_attempt_at<=?4 ORDER BY next_attempt_at,id LIMIT 1",
                params![SECONDARY_PENDING, SECONDARY_RETRYING, SECONDARY_MAX_ATTEMPTS, formatted_now],
                |row| row.get(0),
            )
            .optional()?;
        let Some(provider) = provider else {
            transaction.commit()?;
            return Ok(Vec::new());
        };
        let ids: Vec<i64> = {
            let mut statement = transaction.prepare(
                "SELECT id FROM secondary_notification_outbox WHERE provider=?1 AND state IN (?2,?3) AND attempt_count<?4 AND next_attempt_at<=?5 ORDER BY next_attempt_at,id LIMIT ?6",
            )?;
            statement
                .query_map(
                    params![
                        provider,
                        SECONDARY_PENDING,
                        SECONDARY_RETRYING,
                        SECONDARY_MAX_ATTEMPTS,
                        formatted_now,
                        limit.max(1) as i64,
                    ],
                    |row| row.get(0),
                )?
                .collect::<rusqlite::Result<_>>()?
        };
        let settings = settings_from_connection(&transaction)?;
        let mut eligible_ids = Vec::new();
        for id in ids {
            let delivery = transaction.query_row(
                "SELECT s.id,s.reminder_outbox_id,s.provider,r.due_at,r.reminder_level,s.attempt_count,r.message,e.id AS event_id,e.exchange AS event_exchange,e.board AS event_board,e.security_code AS event_security_code,e.apply_code AS event_apply_code,e.legacy_code AS event_legacy_code,e.name AS event_name,e.apply_date AS event_apply_date,e.issue_price AS event_issue_price,e.lot_size AS event_lot_size,e.max_apply_quantity AS event_max_apply_quantity,e.required_market_value AS event_required_market_value,e.required_cash AS event_required_cash,e.ballot_date AS event_ballot_date,e.payment_date AS event_payment_date,e.listing_date AS event_listing_date,e.issue_status AS event_issue_status,e.lifecycle_status AS event_lifecycle_status,e.event_version AS event_event_version,e.announcement_url AS event_announcement_url,e.data_quality_status AS event_data_quality_status,e.data_conflict AS event_data_conflict,e.sessions_json AS event_sessions_json,e.first_seen_at AS event_first_seen_at,e.updated_at AS event_updated_at FROM secondary_notification_outbox s JOIN reminder_outbox r ON r.id=s.reminder_outbox_id JOIN ipo_events e ON e.id=r.ipo_event_id AND e.event_version=r.event_version WHERE s.id=?1",
                [id],
                map_secondary_delivery,
            )?;
            let subscription_interruption = delivery.level as i32 >= ReminderLevel::Advance as i32
                && delivery.level as i32 <= ReminderLevel::DataChanged as i32;
            if subscription_interruption
                && !subscription_reminder_allowed_now(&delivery.event, &settings, now)
            {
                transaction.execute(
                    "UPDATE secondary_notification_outbox
                     SET state=?1,lease_until=NULL,updated_at=?2 WHERE id=?3",
                    params![SECONDARY_CANCELLED, formatted_now, id],
                )?;
            } else {
                eligible_ids.push(id);
            }
        }
        if eligible_ids.is_empty() {
            transaction.commit()?;
            return Ok(Vec::new());
        }
        transaction.execute(
            "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,-1,?3,NULL)",
            params![formatted_now, provider, eligible_ids.len() as i64],
        )?;
        let request_attempt_id = transaction.last_insert_rowid();
        let lease = format_dt(now + chrono::Duration::minutes(2));
        for id in &eligible_ids {
            transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,lease_until=?2,attempt_count=attempt_count+1,updated_at=?3 WHERE id=?4 AND state IN (?5,?6)",
                params![
                    SECONDARY_LEASED,
                    lease,
                    formatted_now,
                    id,
                    SECONDARY_PENDING,
                    SECONDARY_RETRYING,
                ],
            )?;
        }
        let mut deliveries = Vec::new();
        for id in eligible_ids {
            let mut delivery = transaction.query_row(
                "SELECT s.id,s.reminder_outbox_id,s.provider,r.due_at,r.reminder_level,s.attempt_count,r.message,e.id AS event_id,e.exchange AS event_exchange,e.board AS event_board,e.security_code AS event_security_code,e.apply_code AS event_apply_code,e.legacy_code AS event_legacy_code,e.name AS event_name,e.apply_date AS event_apply_date,e.issue_price AS event_issue_price,e.lot_size AS event_lot_size,e.max_apply_quantity AS event_max_apply_quantity,e.required_market_value AS event_required_market_value,e.required_cash AS event_required_cash,e.ballot_date AS event_ballot_date,e.payment_date AS event_payment_date,e.listing_date AS event_listing_date,e.issue_status AS event_issue_status,e.lifecycle_status AS event_lifecycle_status,e.event_version AS event_event_version,e.announcement_url AS event_announcement_url,e.data_quality_status AS event_data_quality_status,e.data_conflict AS event_data_conflict,e.sessions_json AS event_sessions_json,e.first_seen_at AS event_first_seen_at,e.updated_at AS event_updated_at FROM secondary_notification_outbox s JOIN reminder_outbox r ON r.id=s.reminder_outbox_id JOIN ipo_events e ON e.id=r.ipo_event_id AND e.event_version=r.event_version WHERE s.id=?1 AND s.state=?2",
                params![id, SECONDARY_LEASED],
                map_secondary_delivery,
            )?;
            delivery.request_attempt_id = request_attempt_id;
            deliveries.push(delivery);
        }
        transaction.commit()?;
        Ok(deliveries)
    }

    pub fn complete_secondary_deliveries(
        &self,
        deliveries: &[SecondaryNotificationDelivery],
        channel: &str,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        prune_secondary_notification_history(&transaction, now)?;
        for delivery in deliveries {
            let changed = transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,delivered_at=?2,lease_until=NULL,last_error=NULL,updated_at=?2 WHERE id=?3 AND state=?4",
                params![SECONDARY_DELIVERED, format_dt(now), delivery.id, SECONDARY_LEASED],
            )?;
            if changed != 1 {
                bail!("第二通知通道租约已失效或已完成");
            }
            transaction.execute(
                "INSERT INTO reminder_log(ipo_event_id,scheduled_at,shown_at,reminder_level,delivery_channel,dedupe_key,result) SELECT ipo_event_id,due_at,?1,reminder_level,?2,dedupe_key,'sent' FROM reminder_outbox WHERE id=?3",
                params![format_dt(now), channel, delivery.reminder_outbox_id],
            )?;
        }
        let attempt_id = common_secondary_attempt_id(deliveries)?;
        let changed = transaction.execute(
            "UPDATE secondary_notification_attempts SET success=1,error=NULL WHERE id=?1 AND success=-1",
            [attempt_id],
        )?;
        if changed != 1 {
            bail!("第二通知通道批次配额记录已失效");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn fail_secondary_deliveries(
        &self,
        deliveries: &[SecondaryNotificationDelivery],
        error: &str,
    ) -> Result<()> {
        self.fail_secondary_deliveries_at(deliveries, error, now_china())
    }

    pub(super) fn fail_secondary_deliveries_at(
        &self,
        deliveries: &[SecondaryNotificationDelivery],
        error: &str,
        now: ChinaDateTime,
    ) -> Result<()> {
        if deliveries.is_empty() {
            return Ok(());
        }
        let mut connection = self.open()?;
        let transaction = connection.transaction()?;
        for delivery in deliveries {
            let exhausted = delivery.attempt_count >= SECONDARY_MAX_ATTEMPTS;
            let delay_minutes = match delivery.attempt_count {
                0 | 1 => 1,
                2 => 5,
                3 => 15,
                _ => 30,
            };
            let changed = transaction.execute(
                "UPDATE secondary_notification_outbox SET state=?1,next_attempt_at=?2,lease_until=NULL,last_error=?3,updated_at=?4 WHERE id=?5 AND state=?6",
                params![
                    if exhausted { SECONDARY_EXHAUSTED } else { SECONDARY_RETRYING },
                    format_dt(now + chrono::Duration::minutes(delay_minutes)),
                    limit(error, 1000),
                    format_dt(now),
                    delivery.id,
                    SECONDARY_LEASED,
                ],
            )?;
            if changed != 1 {
                bail!("第二通知通道失败记录的租约已失效");
            }
        }
        let attempt_id = common_secondary_attempt_id(deliveries)?;
        let changed = transaction.execute(
            "UPDATE secondary_notification_attempts SET success=0,error=?1 WHERE id=?2 AND success=-1",
            params![limit(error, 1000), attempt_id],
        )?;
        if changed != 1 {
            bail!("第二通知通道批次失败配额记录已失效");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn secondary_notification_summary(&self) -> Result<SecondaryNotificationSummary> {
        let connection = self.open()?;
        let count = |state: i32| -> Result<i64> {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM secondary_notification_outbox WHERE state=?1",
                [state],
                |row| row.get(0),
            )?)
        };
        let requests_last_hour = connection.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now_china() - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        let latest_success_at = connection
            .query_row(
                "SELECT MAX(attempted_at) FROM secondary_notification_attempts WHERE success=1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )?
            .and_then(|value| parse_dt(&value).ok());
        let latest_error = connection
            .query_row(
                "SELECT error FROM secondary_notification_attempts WHERE success=0 ORDER BY attempted_at DESC,id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(SecondaryNotificationSummary {
            pending: count(SECONDARY_PENDING)?,
            leased: count(SECONDARY_LEASED)?,
            delivered: count(SECONDARY_DELIVERED)?,
            retrying: count(SECONDARY_RETRYING)?,
            exhausted: count(SECONDARY_EXHAUSTED)?,
            cancelled: count(SECONDARY_CANCELLED)?,
            requests_last_hour,
            latest_success_at,
            latest_error,
        })
    }

    pub fn reserve_secondary_notification_test(
        &self,
        provider: SecondaryNotificationProvider,
    ) -> Result<Option<i64>> {
        let now = now_china();
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM secondary_notification_attempts WHERE attempted_at>=?1",
            [format_dt(now - chrono::Duration::hours(1))],
            |row| row.get(0),
        )?;
        if count >= SECONDARY_REQUESTS_PER_HOUR {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO secondary_notification_attempts(attempted_at,provider,success,batch_size,error) VALUES(?1,?2,-1,0,NULL)",
            params![format_dt(now), provider as i32],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(Some(id))
    }

    pub fn finish_secondary_notification_test(
        &self,
        attempt_id: i64,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = self.open()?.execute(
            "UPDATE secondary_notification_attempts SET success=?1,error=?2 WHERE id=?3 AND success=-1",
            params![
                i32::from(error.is_none()),
                error.map(|value| limit(value, 1000)),
                attempt_id,
            ],
        )?;
        if changed != 1 {
            bail!("第二通知通道测试配额记录已失效");
        }
        Ok(())
    }
}

pub(super) fn common_secondary_attempt_id(
    deliveries: &[SecondaryNotificationDelivery],
) -> Result<i64> {
    let first = deliveries
        .first()
        .context("第二通知通道批次为空")?
        .request_attempt_id;
    if first <= 0
        || deliveries
            .iter()
            .any(|delivery| delivery.request_attempt_id != first)
    {
        bail!("第二通知通道批次配额记录不一致");
    }
    Ok(first)
}

pub(super) fn prune_secondary_notification_history(
    connection: &Connection,
    now: ChinaDateTime,
) -> Result<()> {
    connection.execute(
        "DELETE FROM secondary_notification_attempts WHERE attempted_at<?1",
        [format_dt(
            now - chrono::Duration::days(SECONDARY_ATTEMPT_RETENTION_DAYS),
        )],
    )?;
    connection.execute(
        "DELETE FROM secondary_notification_attempts WHERE id NOT IN (SELECT id FROM secondary_notification_attempts ORDER BY attempted_at DESC,id DESC LIMIT ?1)",
        [SECONDARY_MAX_ATTEMPT_RECORDS],
    )?;
    connection.execute(
        "DELETE FROM secondary_notification_outbox WHERE state IN (?1,?2,?3) AND updated_at<?4",
        params![
            SECONDARY_DELIVERED,
            SECONDARY_EXHAUSTED,
            SECONDARY_CANCELLED,
            format_dt(now - chrono::Duration::days(SECONDARY_OUTBOX_RETENTION_DAYS)),
        ],
    )?;
    Ok(())
}

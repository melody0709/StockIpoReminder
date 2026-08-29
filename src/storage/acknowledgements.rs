use super::*;

impl Database {
    pub fn acknowledge(&self, event_id: &str, version: i32) -> Result<()> {
        self.acknowledge_at(event_id, version, now_china())
    }

    pub(super) fn acknowledge_at(
        &self,
        event_id: &str,
        version: i32,
        now: ChinaDateTime,
    ) -> Result<()> {
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut event = event_from_connection(&tx, event_id)?.context("申购任务不存在")?;
        if event.event_version != version {
            bail!("申购数据已更新，请刷新后确认")
        }
        let apply_date = event
            .apply_date
            .context("任务缺少申购日期，不能确认已申购")?;
        if apply_date != now.date_naive() {
            bail!("只能在申购日当天确认已申购")
        }
        if !matches!(
            event.lifecycle_status,
            LifecycleStatus::Scheduled
                | LifecycleStatus::ActiveUnconfirmed
                | LifecycleStatus::Acknowledged
                | LifecycleStatus::AcknowledgedNeedsReview
        ) {
            bail!("当前任务状态不能确认已申购")
        }
        tx.execute("INSERT INTO acknowledgements(ipo_event_id,event_version,confirmed_at,confirmed_data_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(ipo_event_id,event_version) DO UPDATE SET confirmed_at=excluded.confirmed_at,confirmed_data_hash=excluded.confirmed_data_hash,reconfirmed_at=excluded.confirmed_at,revoked_at=NULL,needs_review_at=NULL,review_reason=NULL",params![event_id,version,format_dt(now),event_hash(&event)])?;
        let event_changed = tx.execute("UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4 AND lifecycle_status IN (?5,?6,?7,?8)",params![LifecycleStatus::Acknowledged as i32,format_dt(now),event_id,version,LifecycleStatus::Scheduled as i32,LifecycleStatus::ActiveUnconfirmed as i32,LifecycleStatus::Acknowledged as i32,LifecycleStatus::AcknowledgedNeedsReview as i32])?;
        if event_changed != 1 {
            bail!("申购任务状态已变化，请刷新后重试");
        }
        tx.execute("UPDATE reminder_outbox SET delivery_state=?1,acknowledged_at=?2,updated_at=?2 WHERE ipo_event_id=?3 AND event_version=?4 AND delivery_state IN (0,1,5)",params![DeliveryState::Cancelled as i32,format_dt(now),event_id,version])?;
        event.lifecycle_status = LifecycleStatus::Acknowledged;
        event.updated_at = now;
        let settings = settings_from_connection(&tx)?;
        reconcile_schedule_tx(&tx, &event, &settings, now)?;
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_acknowledgement(&self, event_id: &str, version: i32) -> Result<()> {
        self.revoke_acknowledgement_at(event_id, version, now_china())
    }

    pub(super) fn revoke_acknowledgement_at(
        &self,
        event_id: &str,
        version: i32,
        now: ChinaDateTime,
    ) -> Result<()> {
        let mut connection = self.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut event = event_from_connection(&transaction, event_id)?.context("申购任务不存在")?;
        if event.event_version != version || event.lifecycle_status != LifecycleStatus::Acknowledged
        {
            bail!("当前没有可撤销的有效确认");
        }
        let settings = settings_from_connection(&transaction)?;
        let date = event.apply_date.context("任务缺少申购日期")?;
        if now >= crate::core::at(date, crate::core::effective_cutoff(&event, &settings)) {
            bail!("已超过安全截止时间，不能撤销确认");
        }

        let restored_status = if date > now.date_naive() {
            LifecycleStatus::Scheduled
        } else {
            LifecycleStatus::ActiveUnconfirmed
        };
        event.lifecycle_status = restored_status;
        event.updated_at = now;
        let acknowledgement_changed = transaction.execute(
            "UPDATE acknowledgements SET revoked_at=?1 WHERE ipo_event_id=?2 AND event_version=?3 AND revoked_at IS NULL",
            params![format_dt(now), event_id, version],
        )?;
        if acknowledgement_changed != 1 {
            bail!("当前没有可撤销的有效确认");
        }
        let event_changed = transaction.execute(
            "UPDATE ipo_events SET lifecycle_status=?1,updated_at=?2 WHERE id=?3 AND event_version=?4 AND lifecycle_status=?5",
            params![
                restored_status as i32,
                format_dt(now),
                event_id,
                version,
                LifecycleStatus::Acknowledged as i32,
            ],
        )?;
        if event_changed != 1 {
            bail!("申购任务状态已变化，请刷新后重试");
        }
        reconcile_schedule_tx(&transaction, &event, &settings, now)?;
        transaction.commit()?;
        Ok(())
    }
}

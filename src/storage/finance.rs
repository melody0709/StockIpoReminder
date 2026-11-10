use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveTime};
use rusqlite::{Connection, OptionalExtension, params};

use super::*;
use crate::finance::{
    PaymentAccount, RepoSettings, cycle_after, cycle_before, due_on_for_cycle, first_tracked_cycle,
    legacy_bank_id, normalize_lead_days, parse_cycle, undo_window_open,
    validate_payment_account_history,
};

pub(super) fn migrate_v12(connection: &Connection) -> Result<()> {
    let applied: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=12)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if applied {
        return Ok(());
    }
    let now = now_china();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS reverse_repo_settings(
            id INTEGER PRIMARY KEY CHECK(id=1),
            enabled INTEGER NOT NULL DEFAULT 0,
            primary_time TEXT NULL,
            fallback_time TEXT NULL,
            rules_changed_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS reverse_repo_day_acks(
            trade_date TEXT PRIMARY KEY,
            acted_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS credit_cards(
            id TEXT PRIMARY KEY,
            alias TEXT NOT NULL,
            bank TEXT NULL,
            due_day INTEGER NOT NULL,
            remind_time TEXT NOT NULL,
            lead_days_json TEXT NOT NULL,
            overdue_enabled INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1,
            first_tracked_cycle TEXT NOT NULL,
            rules_changed_at TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS credit_card_actions(
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            card_id TEXT NOT NULL REFERENCES credit_cards(id) ON DELETE CASCADE,
            cycle_key TEXT NOT NULL,
            due_on TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action='paid'),
            acted_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS ux_card_action_cycle
            ON credit_card_actions(card_id,cycle_key);
        CREATE TABLE IF NOT EXISTS finance_shown(
            kind TEXT NOT NULL,
            source_id TEXT NOT NULL,
            occurrence_key TEXT NOT NULL,
            event_date TEXT NOT NULL,
            shown_at TEXT NOT NULL,
            PRIMARY KEY(kind,source_id,occurrence_key)
        );
        CREATE INDEX IF NOT EXISTS ix_finance_shown_date
            ON finance_shown(event_date);
        "#,
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO reverse_repo_settings(id,enabled,primary_time,fallback_time,rules_changed_at,updated_at)
         VALUES(1,0,'14:50',NULL,?1,?1)",
        [format_dt(now)],
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO schema_migrations(version,applied_at) VALUES(12,?1)",
        [format_dt(now)],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_v13(connection: &Connection) -> Result<()> {
    let applied: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=13)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if applied {
        return Ok(());
    }
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        r#"
        ALTER TABLE credit_cards ADD COLUMN account_type TEXT NOT NULL DEFAULT 'credit_card';
        ALTER TABLE credit_cards ADD COLUMN bank_id TEXT NULL;
        ALTER TABLE credit_cards ADD COLUMN final_cycle TEXT NULL;
        ALTER TABLE credit_cards ADD COLUMN closed_at TEXT NULL;
        "#,
    )?;
    let legacy_accounts = {
        let mut statement = transaction.prepare("SELECT id,bank FROM credit_cards")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, bank) in legacy_accounts {
        transaction.execute(
            "UPDATE credit_cards SET account_type='credit_card',bank_id=?2 WHERE id=?1",
            params![id, legacy_bank_id(bank.as_deref())],
        )?;
    }
    transaction.execute(
        "INSERT INTO schema_migrations(version,applied_at) VALUES(13,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) fn migrate_v14(connection: &Connection) -> Result<()> {
    let applied: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=14)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if applied {
        return Ok(());
    }
    let transaction = connection.unchecked_transaction()?;
    transaction.execute_batch(
        r#"
        ALTER TABLE credit_cards ADD COLUMN amount REAL NULL;
        ALTER TABLE credit_cards ADD COLUMN total_amount REAL NULL;
        "#,
    )?;
    transaction.execute(
        "INSERT INTO schema_migrations(version,applied_at) VALUES(14,?1)",
        [format_dt(now_china())],
    )?;
    transaction.commit()?;
    Ok(())
}

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .with_context(|| format!("数据库中的时间格式无效：{value}"))
}

fn parse_primary(value: Option<String>) -> Result<Option<NaiveTime>> {
    value.map(|value| parse_time(&value)).transpose()
}

fn payment_accounts_from_connection(connection: &Connection) -> Result<Vec<PaymentAccount>> {
    let mut statement = connection.prepare(
        "SELECT id,account_type,alias,bank_id,bank,due_day,remind_time,lead_days_json,
                overdue_enabled,enabled,first_tracked_cycle,final_cycle,closed_at,
                rules_changed_at,created_at,updated_at,amount,total_amount
         FROM credit_cards ORDER BY alias COLLATE NOCASE",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, String>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, Option<f64>>(16)?,
            row.get::<_, Option<f64>>(17)?,
        ))
    })?;
    let mut accounts = Vec::new();
    for row in rows {
        let (
            id,
            kind,
            alias,
            bank_id,
            bank_name,
            due_day,
            remind_time,
            lead_days,
            overdue_enabled,
            enabled,
            first_tracked_cycle,
            final_cycle,
            closed_at,
            rules_changed_at,
            created_at,
            updated_at,
            amount,
            total_amount,
        ) = row?;
        accounts.push(PaymentAccount {
            id,
            kind: kind.parse()?,
            alias,
            bank_id,
            bank_name,
            due_day: due_day as u32,
            remind_time: parse_time(&remind_time)?,
            lead_days: serde_json::from_str(&lead_days).unwrap_or_else(|_| vec![3, 0]),
            overdue_enabled: overdue_enabled != 0,
            enabled: enabled != 0,
            first_tracked_cycle,
            final_cycle,
            closed_at: closed_at.map(|value| parse_dt(&value)).transpose()?,
            rules_changed_at: parse_dt(&rules_changed_at)?,
            created_at: parse_dt(&created_at)?,
            updated_at: parse_dt(&updated_at)?,
            amount,
            total_amount,
        });
    }
    Ok(accounts)
}

fn payment_account_from_connection(
    connection: &Connection,
    id: &str,
) -> Result<Option<PaymentAccount>> {
    Ok(payment_accounts_from_connection(connection)?
        .into_iter()
        .find(|account| account.id == id))
}

fn settled_from_connection(connection: &Connection, account_id: &str) -> Result<Option<String>> {
    Ok(connection.query_row(
        "SELECT MAX(cycle_key) FROM credit_card_actions WHERE card_id=?1",
        [account_id],
        |row| row.get(0),
    )?)
}

impl Database {
    pub(crate) fn repo_settings(&self) -> Result<RepoSettings> {
        let connection = self.open()?;
        let row = connection
            .query_row(
                "SELECT enabled,primary_time,fallback_time,rules_changed_at,updated_at
                 FROM reverse_repo_settings WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((enabled, primary, fallback, rules, updated)) = row else {
            return Ok(RepoSettings::default_at(now_china()));
        };
        Ok(RepoSettings {
            enabled: enabled != 0,
            primary: parse_primary(primary)?,
            fallback: parse_primary(fallback)?,
            rules_changed_at: parse_dt(&rules)?,
            updated_at: parse_dt(&updated)?,
        })
    }

    pub(crate) fn save_repo_settings(&self, settings: &RepoSettings) -> Result<()> {
        settings.validate()?;
        let previous = self.repo_settings()?;
        let now = now_china();
        let rules_changed = previous.primary != settings.primary
            || previous.fallback != settings.fallback
            || (!previous.enabled && settings.enabled);
        let rules_changed_at = if rules_changed {
            now
        } else {
            previous.rules_changed_at
        };
        let connection = self.open()?;
        connection.execute(
            "INSERT INTO reverse_repo_settings(id,enabled,primary_time,fallback_time,rules_changed_at,updated_at)
             VALUES(1,?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET
               enabled=excluded.enabled,
               primary_time=excluded.primary_time,
               fallback_time=excluded.fallback_time,
               rules_changed_at=excluded.rules_changed_at,
               updated_at=excluded.updated_at",
            params![
                i64::from(settings.enabled),
                settings.primary.map(|value| value.format("%H:%M").to_string()),
                settings.fallback.map(|value| value.format("%H:%M").to_string()),
                format_dt(rules_changed_at),
                format_dt(now),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn repo_day_acked(&self, date: NaiveDate) -> Result<bool> {
        Ok(self.open()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM reverse_repo_day_acks WHERE trade_date=?1)",
            [format_date(date)],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn set_repo_day_acked(&self, date: NaiveDate, acked: bool) -> Result<()> {
        let connection = self.open()?;
        if acked {
            connection.execute(
                "INSERT OR REPLACE INTO reverse_repo_day_acks(trade_date,acted_at) VALUES(?1,?2)",
                params![format_date(date), format_dt(now_china())],
            )?;
        } else {
            connection.execute(
                "DELETE FROM reverse_repo_day_acks WHERE trade_date=?1",
                [format_date(date)],
            )?;
        }
        Ok(())
    }

    pub(crate) fn payment_accounts(&self) -> Result<Vec<PaymentAccount>> {
        payment_accounts_from_connection(&self.open()?)
    }

    #[cfg(test)]
    pub(crate) fn payment_account(&self, id: &str) -> Result<Option<PaymentAccount>> {
        payment_account_from_connection(&self.open()?, id)
    }

    pub(crate) fn save_payment_account(&self, account: &PaymentAccount) -> Result<()> {
        let mut account = account.clone();
        let now = now_china();
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        let previous = payment_account_from_connection(&transaction, &account.id)?;
        if account.first_tracked_cycle.trim().is_empty() {
            account.first_tracked_cycle = previous
                .as_ref()
                .map(|previous| previous.first_tracked_cycle.clone())
                .unwrap_or_else(|| first_tracked_cycle(now.date_naive(), account.due_day));
        }
        if let Some(previous) = previous.as_ref()
            && previous.kind != account.kind
        {
            anyhow::bail!("还款账户类型不可修改，请新建账户后删除旧账户");
        }
        if previous.as_ref().is_some_and(|previous| {
            previous.closed_at.is_some() && account.closed_at.is_none() && !account.enabled
        }) {
            anyhow::bail!("恢复为进行中必须同时启用账户");
        }
        let settled = settled_from_connection(&transaction, &account.id)?;
        validate_payment_account_history(&account, settled.as_deref())?;
        let rules_changed = previous.as_ref().is_none_or(|previous| {
            previous.due_day != account.due_day
                || previous.remind_time != account.remind_time
                || normalize_lead_days(previous.lead_days.iter().copied()).ok()
                    != normalize_lead_days(account.lead_days.iter().copied()).ok()
                || previous.overdue_enabled != account.overdue_enabled
                || (!previous.enabled && account.enabled)
                || previous.final_cycle != account.final_cycle
                || previous.closed_at.is_some() != account.closed_at.is_some()
                || previous.amount != account.amount
                || previous.total_amount != account.total_amount
        });
        let rules_changed_at = if rules_changed {
            now
        } else {
            previous
                .as_ref()
                .map(|previous| previous.rules_changed_at)
                .unwrap_or(now)
        };
        let leads = normalize_lead_days(account.lead_days.iter().copied())?;
        let created_at = previous
            .as_ref()
            .map(|previous| previous.created_at)
            .unwrap_or(now);
        let closed_at = account.closed_at.as_ref().map(|value| format_dt(*value));
        transaction.execute(
            "INSERT INTO credit_cards(
                id,account_type,alias,bank_id,bank,due_day,remind_time,lead_days_json,
                overdue_enabled,enabled,first_tracked_cycle,final_cycle,closed_at,
                rules_changed_at,created_at,updated_at,amount,total_amount
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(id) DO UPDATE SET
                account_type=excluded.account_type,
                alias=excluded.alias,
                bank_id=excluded.bank_id,
                bank=excluded.bank,
                due_day=excluded.due_day,
                remind_time=excluded.remind_time,
                lead_days_json=excluded.lead_days_json,
                overdue_enabled=excluded.overdue_enabled,
                enabled=excluded.enabled,
                first_tracked_cycle=excluded.first_tracked_cycle,
                final_cycle=excluded.final_cycle,
                closed_at=excluded.closed_at,
                rules_changed_at=excluded.rules_changed_at,
                updated_at=excluded.updated_at,
                amount=excluded.amount,
                total_amount=excluded.total_amount",
            params![
                account.id,
                account.kind.as_str(),
                account.alias.trim(),
                account.bank_id,
                account.bank_name.as_deref().map(str::trim),
                account.due_day as i64,
                account.remind_time.format("%H:%M").to_string(),
                serde_json::to_string(&leads)?,
                i64::from(account.overdue_enabled),
                i64::from(account.enabled),
                account.first_tracked_cycle,
                account.final_cycle,
                closed_at,
                format_dt(rules_changed_at),
                format_dt(created_at),
                format_dt(now),
                account.amount,
                account.total_amount,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn delete_payment_account(&self, id: &str) -> Result<()> {
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM credit_card_actions WHERE card_id=?1", [id])?;
        transaction.execute(
            "DELETE FROM finance_shown
             WHERE source_id=?1 AND kind IN ('credit_card','mortgage','auto_loan','credit_loan')",
            [id],
        )?;
        transaction.execute("DELETE FROM credit_cards WHERE id=?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn payment_account_settled(&self, account_id: &str) -> Result<Option<String>> {
        settled_from_connection(&self.open()?, account_id)
    }

    pub(crate) fn mark_payment_account_paid(&self, account_id: &str, cycle: &str) -> Result<()> {
        parse_cycle(cycle).context("已还账期格式必须是 YYYY-MM")?;
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        let Some(account) = payment_account_from_connection(&transaction, account_id)? else {
            anyhow::bail!("还款账户不存在");
        };
        if account.closed_at.is_some() {
            anyhow::bail!("已结清账户不能新增已还记录，请先恢复为进行中");
        }
        if cycle_before(cycle, &account.first_tracked_cycle) {
            anyhow::bail!("已还账期不能早于首个跟踪账期");
        }
        if account
            .final_cycle
            .as_deref()
            .is_some_and(|final_cycle| cycle_after(cycle, final_cycle))
        {
            anyhow::bail!("已还账期不能晚于结束还款月");
        }
        let due = due_on_for_cycle(cycle, account.due_day)?;
        transaction.execute(
            "INSERT OR IGNORE INTO credit_card_actions(card_id,cycle_key,due_on,action,acted_at)
             VALUES(?1,?2,?3,'paid',?4)",
            params![account_id, cycle, format_date(due), format_dt(now_china())],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn undo_payment_account_action(&self, account_id: &str) -> Result<()> {
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        if payment_account_from_connection(&transaction, account_id)?.is_none() {
            anyhow::bail!("还款账户不存在");
        }
        let settled = settled_from_connection(&transaction, account_id)?;
        let Some(settled) = settled else {
            anyhow::bail!("没有可撤销的已还记录");
        };
        if !undo_window_open(Some(&settled), now_china().date_naive()) {
            anyhow::bail!("只能撤销上一账期、当前账期或未来账期的最高已还记录");
        }
        transaction.execute(
            "DELETE FROM credit_card_actions WHERE card_id=?1 AND cycle_key=?2",
            params![account_id, settled],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn close_payment_account(&self, account_id: &str) -> Result<()> {
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        let Some(account) = payment_account_from_connection(&transaction, account_id)? else {
            anyhow::bail!("还款账户不存在");
        };
        if !account.kind.is_loan() {
            anyhow::bail!("只有贷款类账户（房贷、车贷或信用贷）可以标记已结清");
        }
        if account.closed_at.is_none() {
            let now = format_dt(now_china());
            transaction.execute(
                "UPDATE credit_cards
                 SET closed_at=?2,rules_changed_at=?2,updated_at=?2 WHERE id=?1",
                params![account_id, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn resume_payment_account(&self, account_id: &str) -> Result<()> {
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        let Some(account) = payment_account_from_connection(&transaction, account_id)? else {
            anyhow::bail!("还款账户不存在");
        };
        if !account.kind.is_loan() {
            anyhow::bail!("信用卡没有已结清状态");
        }
        let now = format_dt(now_china());
        transaction.execute(
            "UPDATE credit_cards
             SET closed_at=NULL,enabled=1,rules_changed_at=?2,updated_at=?2 WHERE id=?1",
            params![account_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_payment_account_rules_changed_at_for_test(
        &self,
        id: &str,
        rules_changed_at: ChinaDateTime,
    ) -> Result<()> {
        self.open()?.execute(
            "UPDATE credit_cards SET rules_changed_at=?2 WHERE id=?1",
            params![id, format_dt(rules_changed_at)],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn credit_cards(&self) -> Result<Vec<PaymentAccount>> {
        self.payment_accounts()
    }

    #[cfg(test)]
    pub(crate) fn save_credit_card(&self, card: &PaymentAccount) -> Result<()> {
        self.save_payment_account(card)
    }

    #[cfg(test)]
    pub(crate) fn delete_credit_card(&self, id: &str) -> Result<()> {
        self.delete_payment_account(id)
    }

    #[cfg(test)]
    pub(crate) fn credit_card_settled(&self, card_id: &str) -> Result<Option<String>> {
        self.payment_account_settled(card_id)
    }

    #[cfg(test)]
    pub(crate) fn mark_credit_card_paid(
        &self,
        card_id: &str,
        cycle: &str,
        _due: NaiveDate,
    ) -> Result<()> {
        self.mark_payment_account_paid(card_id, cycle)
    }

    #[cfg(test)]
    pub(crate) fn undo_credit_card_action(&self, card_id: &str) -> Result<()> {
        self.undo_payment_account_action(card_id)
    }

    /// 已展示查询必须按主键三元组走：表的主键是 (kind, source_id, occurrence_key)，
    /// 只按 occurrence_key 查会把别的来源（例如另一张信用卡）的记录当成本条已展示。
    pub(crate) fn finance_is_shown(&self, kind: &str, source_id: &str, key: &str) -> Result<bool> {
        Ok(self.open()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM finance_shown
                           WHERE kind=?1 AND source_id=?2 AND occurrence_key=?3)",
            params![kind, source_id, key],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn mark_finance_shown(
        &self,
        kind: &str,
        source_id: &str,
        key: &str,
        date: NaiveDate,
    ) -> Result<()> {
        self.open()?.execute(
            "INSERT OR IGNORE INTO finance_shown(kind,source_id,occurrence_key,event_date,shown_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                kind,
                source_id,
                key,
                format_date(date),
                format_dt(now_china())
            ],
        )?;
        Ok(())
    }

    pub(crate) fn cleanup_finance_shown(&self, before: NaiveDate) -> Result<()> {
        self.open()?.execute(
            "DELETE FROM finance_shown WHERE event_date<?1",
            [format_date(before)],
        )?;
        Ok(())
    }

    pub(crate) fn finance_counts(&self) -> Result<(i64, i64)> {
        let connection = self.open()?;
        let cards: i64 = connection.query_row(
            "SELECT COUNT(*) FROM credit_cards WHERE enabled=1 AND closed_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let shown: i64 = connection.query_row(
            "SELECT COUNT(*) FROM finance_shown WHERE event_date=?1",
            [format_date(now_china().date_naive())],
            |row| row.get(0),
        )?;
        Ok((cards, shown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        finance::{PaymentAccountKind, due_reminders},
        model::time,
    };
    use uuid::Uuid;

    fn database() -> (Database, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-finance-test-{}",
            Uuid::new_v4().simple()
        ));
        let database = Database::new(&root);
        database.initialize().unwrap();
        (database, root)
    }

    fn account(id: &str, kind: PaymentAccountKind) -> PaymentAccount {
        let now = now_china();
        PaymentAccount {
            id: id.into(),
            kind,
            alias: format!("账户 {id}"),
            bank_id: Some("cmb".into()),
            bank_name: Some("招商银行".into()),
            due_day: 5,
            remind_time: time(9, 0),
            lead_days: vec![3, 0],
            overdue_enabled: true,
            enabled: true,
            first_tracked_cycle: "2026-01".into(),
            final_cycle: None,
            closed_at: None,
            rules_changed_at: now,
            created_at: now,
            updated_at: now,
            amount: None,
            total_amount: None,
        }
    }

    fn create_real_v12_database() -> (Database, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-finance-v12-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let database = Database::new(&root);
        let connection = database.open().unwrap();
        connection.execute_batch(MIGRATION_SQL).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE reverse_repo_settings(
                    id INTEGER PRIMARY KEY CHECK(id=1), enabled INTEGER NOT NULL DEFAULT 0,
                    primary_time TEXT NULL, fallback_time TEXT NULL,
                    rules_changed_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                CREATE TABLE reverse_repo_day_acks(trade_date TEXT PRIMARY KEY, acted_at TEXT NOT NULL);
                CREATE TABLE credit_cards(
                    id TEXT PRIMARY KEY, alias TEXT NOT NULL, bank TEXT NULL,
                    due_day INTEGER NOT NULL, remind_time TEXT NOT NULL,
                    lead_days_json TEXT NOT NULL, overdue_enabled INTEGER NOT NULL DEFAULT 1,
                    enabled INTEGER NOT NULL DEFAULT 1, first_tracked_cycle TEXT NOT NULL,
                    rules_changed_at TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );
                CREATE TABLE credit_card_actions(
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    card_id TEXT NOT NULL REFERENCES credit_cards(id) ON DELETE CASCADE,
                    cycle_key TEXT NOT NULL, due_on TEXT NOT NULL,
                    action TEXT NOT NULL CHECK(action='paid'), acted_at TEXT NOT NULL
                );
                CREATE UNIQUE INDEX ux_card_action_cycle ON credit_card_actions(card_id,cycle_key);
                CREATE TABLE finance_shown(
                    kind TEXT NOT NULL, source_id TEXT NOT NULL, occurrence_key TEXT NOT NULL,
                    event_date TEXT NOT NULL, shown_at TEXT NOT NULL,
                    PRIMARY KEY(kind,source_id,occurrence_key)
                );
                CREATE INDEX ix_finance_shown_date ON finance_shown(event_date);
                INSERT INTO schema_migrations(version,applied_at) VALUES(12,'2026-10-01T00:00:00+08:00');
                "#,
            )
            .unwrap();
        for (id, alias, bank) in [
            ("legacy-card", "旧招行卡", Some("招行")),
            ("unknown-bank-card", "旧地方卡", Some("未知地方银行")),
        ] {
            connection
                .execute(
                    "INSERT INTO credit_cards(
                        id,alias,bank,due_day,remind_time,lead_days_json,overdue_enabled,enabled,
                        first_tracked_cycle,rules_changed_at,created_at,updated_at
                     ) VALUES(?1,?2,?3,5,'09:00','[3,0]',1,1,'2026-09',
                              '2026-09-01T00:00:00+08:00','2026-09-01T00:00:00+08:00',
                              '2026-09-01T00:00:00+08:00')",
                    params![id, alias, bank],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO credit_card_actions(card_id,cycle_key,due_on,action,acted_at)
                 VALUES('legacy-card','2026-09','2026-09-05','paid','2026-09-05T09:00:00+08:00')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO finance_shown(kind,source_id,occurrence_key,event_date,shown_at)
                 VALUES('credit_card','legacy-card','legacy-card:2026-10:3:2026-10-05',
                        '2026-10-02','2026-10-02T09:00:00+08:00')",
                [],
            )
            .unwrap();
        drop(connection);
        (database, root)
    }

    #[test]
    fn v13_creates_default_off_repo_and_payment_account_tables() {
        let (database, root) = database();
        assert_eq!(database.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let repo = database.repo_settings().unwrap();
        assert!(!repo.enabled);
        assert_eq!(repo.primary, Some(time(14, 50)));
        assert_eq!(repo.fallback, None);
        let columns = database
            .open()
            .unwrap()
            .prepare("PRAGMA table_info(credit_cards)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for column in ["account_type", "bank_id", "final_cycle", "closed_at"] {
            assert!(columns.iter().any(|value| value == column));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn real_v12_upgrade_preserves_accounts_actions_bank_text_and_shown_ledger() {
        let (database, root) = create_real_v12_database();
        database.initialize().unwrap();
        assert_eq!(database.schema_version().unwrap(), LATEST_SCHEMA_VERSION);

        let known = database.payment_account("legacy-card").unwrap().unwrap();
        assert_eq!(known.kind, PaymentAccountKind::CreditCard);
        assert_eq!(known.alias, "旧招行卡");
        assert_eq!(known.bank_id.as_deref(), Some("cmb"));
        assert_eq!(known.bank_name.as_deref(), Some("招行"));
        assert_eq!(known.first_tracked_cycle, "2026-09");
        assert_eq!(
            known.rules_changed_at,
            parse_dt("2026-09-01T00:00:00+08:00").unwrap()
        );
        assert_eq!(known.final_cycle, None);
        assert_eq!(known.closed_at, None);
        let unknown = database
            .payment_account("unknown-bank-card")
            .unwrap()
            .unwrap();
        assert_eq!(unknown.bank_id.as_deref(), Some("other"));
        assert_eq!(unknown.bank_name.as_deref(), Some("未知地方银行"));
        assert_eq!(
            database
                .payment_account_settled("legacy-card")
                .unwrap()
                .as_deref(),
            Some("2026-09")
        );
        let key = "legacy-card:2026-10:3:2026-10-05";
        assert!(
            database
                .finance_is_shown("credit_card", "legacy-card", key)
                .unwrap()
        );

        let now = "2026-10-02T09:00:00+08:00".parse().unwrap();
        let unfiltered = due_reminders(now, &known, Some("2026-09"), |_| false);
        assert!(unfiltered.iter().any(|item| item.occurrence_key == key));
        let filtered = due_reminders(now, &known, Some("2026-09"), |key| {
            database
                .finance_is_shown("credit_card", "legacy-card", key)
                .unwrap()
        });
        assert!(!filtered.iter().any(|item| item.occurrence_key == key));

        database.initialize().unwrap();
        let again = database.payment_account("legacy-card").unwrap().unwrap();
        assert_eq!(again, known);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn v13_failure_rolls_back_columns_and_migration_marker() {
        let (database, root) = create_real_v12_database();
        database
            .open()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_v13_bank_mapping BEFORE UPDATE ON credit_cards
                 BEGIN SELECT RAISE(ABORT,'fixture v13 failure'); END;",
            )
            .unwrap();
        assert!(database.initialize().is_err());
        let connection = database.open().unwrap();
        let new_columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('credit_cards')
                 WHERE name IN ('account_type','bank_id','final_cycle','closed_at')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_columns, 0);
        let migrated: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version=13",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated, 0);
        connection
            .execute("DROP TRIGGER fail_v13_bank_mapping", [])
            .unwrap();
        drop(connection);
        database.initialize().unwrap();
        assert_eq!(database.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn v14_migration_adds_columns_to_existing_v13_database() {
        let (database, root) = database();
        let connection = database.open().unwrap();
        // 模拟 v13 状态：删除 version=14，删除 amount 与 total_amount 列
        connection
            .execute("DELETE FROM schema_migrations WHERE version=14", [])
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE credit_cards DROP COLUMN amount;
                 ALTER TABLE credit_cards DROP COLUMN total_amount;",
            )
            .unwrap();
        drop(connection);

        // 重新初始化，执行 migrate_v14
        database.initialize().unwrap();
        assert_eq!(database.schema_version().unwrap(), 14);

        let connection = database.open().unwrap();
        let amount_col: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('credit_cards') WHERE name IN ('amount','total_amount')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(amount_col, 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repo_save_refreshes_rules_only_when_needed() {
        let (database, root) = database();
        let mut settings = database.repo_settings().unwrap();
        let first_rules = settings.rules_changed_at;
        settings.enabled = true;
        database.save_repo_settings(&settings).unwrap();
        let enabled = database.repo_settings().unwrap();
        assert!(enabled.enabled);
        assert!(enabled.rules_changed_at >= first_rules);
        settings = enabled;
        settings.primary = Some(time(14, 55));
        database.save_repo_settings(&settings).unwrap();
        assert_eq!(
            database.repo_settings().unwrap().primary,
            Some(time(14, 55))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn account_actions_and_shown_ledger_are_local_and_persistent() {
        let (database, root) = database();
        let now = now_china();
        let today = now.date_naive();
        let cycle = crate::finance::cycle_key(today);
        let mut card = account("card-1", PaymentAccountKind::CreditCard);
        card.due_day = 31;
        card.first_tracked_cycle = cycle.clone();
        database.save_credit_card(&card).unwrap();
        let due = due_on_for_cycle(&cycle, card.due_day).unwrap();
        database
            .mark_credit_card_paid("card-1", &cycle, due)
            .unwrap();
        assert_eq!(
            database.credit_card_settled("card-1").unwrap().as_deref(),
            Some(cycle.as_str())
        );
        let key = format!("card-1:{cycle}:3:{due}");
        database
            .mark_finance_shown("credit_card", "card-1", &key, today)
            .unwrap();
        assert!(
            database
                .finance_is_shown("credit_card", "card-1", &key)
                .unwrap()
        );
        database.undo_credit_card_action("card-1").unwrap();
        assert_eq!(database.credit_card_settled("card-1").unwrap(), None);
        database.delete_credit_card("card-1").unwrap();
        assert!(database.credit_cards().unwrap().is_empty());
        assert!(
            !database
                .finance_is_shown("credit_card", "card-1", &key)
                .unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multiple_new_payment_accounts_persist_independently() {
        let (database, root) = database();
        for (id, kind) in [
            ("multi-card", PaymentAccountKind::CreditCard),
            ("multi-mortgage", PaymentAccountKind::Mortgage),
            ("multi-auto", PaymentAccountKind::AutoLoan),
            ("multi-credit-loan", PaymentAccountKind::CreditLoan),
        ] {
            database.save_payment_account(&account(id, kind)).unwrap();
        }
        let mut accounts = database.payment_accounts().unwrap();
        accounts.sort_by(|left, right| left.id.cmp(&right.id));
        assert_eq!(accounts.len(), 4);
        assert_eq!(accounts[0].id, "multi-auto");
        assert_eq!(accounts[1].id, "multi-card");
        assert_eq!(accounts[2].id, "multi-credit-loan");
        assert_eq!(accounts[3].id, "multi-mortgage");

        let mut edited = database.payment_account("multi-card").unwrap().unwrap();
        edited.alias = "只改这一条".into();
        database.save_payment_account(&edited).unwrap();
        assert_eq!(
            database
                .payment_account("multi-card")
                .unwrap()
                .unwrap()
                .alias,
            "只改这一条"
        );
        assert_ne!(
            database
                .payment_account("multi-mortgage")
                .unwrap()
                .unwrap()
                .alias,
            "只改这一条"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shown_ledger_is_scoped_by_kind_and_source_not_just_by_key() {
        let (database, root) = database();
        let today = now_china().date_naive();
        for id in ["card-a", "card-b"] {
            database
                .save_payment_account(&account(id, PaymentAccountKind::CreditCard))
                .unwrap();
        }
        let key = "shared-key";
        database
            .mark_finance_shown("credit_card", "card-a", key, today)
            .unwrap();
        assert!(
            database
                .finance_is_shown("credit_card", "card-a", key)
                .unwrap()
        );
        assert!(
            !database
                .finance_is_shown("credit_card", "card-b", key)
                .unwrap()
        );
        assert!(
            !database
                .finance_is_shown("mortgage", "card-a", key)
                .unwrap()
        );
        assert!(
            !database
                .finance_is_shown("reverse_repo", "repo", key)
                .unwrap()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_bank_id_is_preserved_on_read_and_edit() {
        let (database, root) = database();
        let mut account = account("future-bank", PaymentAccountKind::CreditCard);
        account.bank_id = Some("future_bank_v9".into());
        account.bank_name = Some("未来银行".into());
        database.save_payment_account(&account).unwrap();
        let mut loaded = database.payment_account(&account.id).unwrap().unwrap();
        assert_eq!(loaded.bank_id.as_deref(), Some("future_bank_v9"));
        assert_eq!(loaded.bank_name.as_deref(), Some("未来银行"));
        loaded.alias = "修改别名".into();
        database.save_payment_account(&loaded).unwrap();
        let loaded = database.payment_account(&account.id).unwrap().unwrap();
        assert_eq!(loaded.bank_id.as_deref(), Some("future_bank_v9"));
        assert_eq!(loaded.bank_name.as_deref(), Some("未来银行"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn account_type_is_immutable_and_final_cycle_cannot_precede_paid_history() {
        let (database, root) = database();
        let mut loan = account("loan-history", PaymentAccountKind::Mortgage);
        loan.first_tracked_cycle = "2026-10".into();
        loan.final_cycle = Some("2026-12".into());
        database.save_payment_account(&loan).unwrap();
        database
            .mark_payment_account_paid(&loan.id, "2026-12")
            .unwrap();
        let mut edited = database.payment_account(&loan.id).unwrap().unwrap();
        edited.final_cycle = Some("2026-11".into());
        assert!(database.save_payment_account(&edited).is_err());
        assert_eq!(
            database
                .payment_account_settled(&loan.id)
                .unwrap()
                .as_deref(),
            Some("2026-12")
        );
        edited.final_cycle = Some("2026-12".into());
        edited.kind = PaymentAccountKind::AutoLoan;
        assert!(database.save_payment_account(&edited).is_err());
        assert_eq!(
            database.payment_account(&loan.id).unwrap().unwrap().kind,
            PaymentAccountKind::Mortgage
        );
        for cycle in ["2026-1", "2026-09", "2027-01"] {
            assert!(
                database.mark_payment_account_paid(&loan.id, cycle).is_err(),
                "{cycle}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn undo_storage_accepts_highest_future_cycle_and_rejects_old_or_missing_actions() {
        let (database, root) = database();
        let today = now_china().date_naive();
        let current = crate::finance::cycle_key(today);
        let future = crate::finance::shift_cycle(&current, 2).unwrap();
        let mut card = account("future-paid", PaymentAccountKind::CreditCard);
        card.first_tracked_cycle = current;
        database.save_payment_account(&card).unwrap();
        database
            .mark_payment_account_paid(&card.id, &future)
            .unwrap();
        database.undo_payment_account_action(&card.id).unwrap();
        assert_eq!(database.payment_account_settled(&card.id).unwrap(), None);
        assert!(database.undo_payment_account_action(&card.id).is_err());

        let old = crate::finance::shift_cycle(&crate::finance::cycle_key(today), -2).unwrap();
        database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO credit_card_actions(card_id,cycle_key,due_on,action,acted_at)
                 VALUES(?1,?2,'2026-01-05','paid',?3)",
                params![card.id, old, format_dt(now_china())],
            )
            .unwrap();
        assert!(database.undo_payment_account_action(&card.id).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn final_cycle_changes_and_resume_refresh_rule_timestamps() {
        let (database, root) = database();
        let mut loan = account("loan-close", PaymentAccountKind::AutoLoan);
        loan.first_tracked_cycle = "2026-01".into();
        loan.final_cycle = Some("2026-12".into());
        database.save_payment_account(&loan).unwrap();
        let old = "2020-01-01T00:00:00+08:00";
        database
            .open()
            .unwrap()
            .execute(
                "UPDATE credit_cards SET rules_changed_at=?2,updated_at=?2 WHERE id=?1",
                params![loan.id, old],
            )
            .unwrap();
        let mut edited = database.payment_account(&loan.id).unwrap().unwrap();
        edited.final_cycle = Some("2027-01".into());
        database.save_payment_account(&edited).unwrap();
        let edited = database.payment_account(&loan.id).unwrap().unwrap();
        assert!(edited.rules_changed_at > parse_dt(old).unwrap());

        database.close_payment_account(&loan.id).unwrap();
        let closed = database.payment_account(&loan.id).unwrap().unwrap();
        assert!(closed.closed_at.is_some());
        assert!(
            database
                .mark_payment_account_paid(&loan.id, "2026-12")
                .is_err()
        );
        database
            .open()
            .unwrap()
            .execute(
                "UPDATE credit_cards SET enabled=0,rules_changed_at=?2,updated_at=?2 WHERE id=?1",
                params![loan.id, old],
            )
            .unwrap();
        database.resume_payment_account(&loan.id).unwrap();
        let resumed = database.payment_account(&loan.id).unwrap().unwrap();
        assert!(resumed.enabled);
        assert_eq!(resumed.closed_at, None);
        assert!(resumed.rules_changed_at > parse_dt(old).unwrap());
        assert!(resumed.updated_at > parse_dt(old).unwrap());
        database.delete_payment_account(&loan.id).unwrap();
        assert!(database.payment_account(&loan.id).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn credit_card_cannot_be_closed_or_resumed() {
        let (database, root) = database();
        let card = account("card-close", PaymentAccountKind::CreditCard);
        database.save_payment_account(&card).unwrap();
        assert!(database.close_payment_account(&card.id).is_err());
        assert!(database.resume_payment_account(&card.id).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn v14_persists_amount_and_total_amount_and_tracks_changes() {
        let (database, root) = database();
        let mut loan = account("credit-loan-1", PaymentAccountKind::CreditLoan);
        loan.amount = Some(4500.5);
        loan.total_amount = Some(150000.0);
        database.save_payment_account(&loan).unwrap();

        let loaded = database.payment_account(&loan.id).unwrap().unwrap();
        assert_eq!(loaded.kind, PaymentAccountKind::CreditLoan);
        assert_eq!(loaded.amount, Some(4500.5));
        assert_eq!(loaded.total_amount, Some(150000.0));

        let old = "2026-09-01T00:00:00+08:00";
        database
            .open()
            .unwrap()
            .execute(
                "UPDATE credit_cards SET rules_changed_at=?2,updated_at=?2 WHERE id=?1",
                params![loan.id, old],
            )
            .unwrap();

        let mut updated = loaded.clone();
        updated.amount = Some(4200.0);
        database.save_payment_account(&updated).unwrap();
        let reloaded = database.payment_account(&loan.id).unwrap().unwrap();
        assert_eq!(reloaded.amount, Some(4200.0));
        assert!(reloaded.rules_changed_at > parse_dt(old).unwrap());

        // 结清和恢复保持金额不变
        database.close_payment_account(&loan.id).unwrap();
        let closed = database.payment_account(&loan.id).unwrap().unwrap();
        assert!(closed.closed_at.is_some());
        assert_eq!(closed.amount, Some(4200.0));
        assert_eq!(closed.total_amount, Some(150000.0));

        database.resume_payment_account(&loan.id).unwrap();
        let resumed = database.payment_account(&loan.id).unwrap().unwrap();
        assert!(resumed.closed_at.is_none());
        assert_eq!(resumed.amount, Some(4200.0));
        assert_eq!(resumed.total_amount, Some(150000.0));

        database.delete_payment_account(&loan.id).unwrap();
        assert!(database.payment_account(&loan.id).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}

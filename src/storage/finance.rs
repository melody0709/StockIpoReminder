use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveTime};
use rusqlite::{Connection, OptionalExtension, params};

use super::*;
use crate::finance::{
    CreditCard, RepoSettings, due_on_for_cycle, first_tracked_cycle, normalize_lead_days,
    validate_card,
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

fn parse_time(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .with_context(|| format!("数据库中的时间格式无效：{value}"))
}

fn parse_primary(value: Option<String>) -> Result<Option<NaiveTime>> {
    value.map(|value| parse_time(&value)).transpose()
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

    pub(crate) fn credit_cards(&self) -> Result<Vec<CreditCard>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id,alias,bank,due_day,remind_time,lead_days_json,overdue_enabled,enabled,
                    first_tracked_cycle,rules_changed_at,created_at,updated_at
             FROM credit_cards ORDER BY alias COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| {
            let lead_days: String = row.get(5)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                lead_days,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
            ))
        })?;
        let mut cards = Vec::new();
        for row in rows {
            let (
                id,
                alias,
                bank,
                due_day,
                remind_time,
                lead_days,
                overdue_enabled,
                enabled,
                first_cycle,
                rules,
                created,
                updated,
            ) = row?;
            cards.push(CreditCard {
                id,
                alias,
                bank,
                due_day: due_day as u32,
                remind_time: parse_time(&remind_time)?,
                lead_days: serde_json::from_str(&lead_days).unwrap_or_else(|_| vec![3, 0]),
                overdue_enabled: overdue_enabled != 0,
                enabled: enabled != 0,
                first_tracked_cycle: first_cycle,
                rules_changed_at: parse_dt(&rules)?,
                created_at: parse_dt(&created)?,
                updated_at: parse_dt(&updated)?,
            });
        }
        Ok(cards)
    }

    pub(crate) fn save_credit_card(&self, card: &CreditCard) -> Result<()> {
        validate_card(card)?;
        let now = now_china();
        let previous = self.credit_card(&card.id)?;
        let rules_changed = previous.as_ref().is_none_or(|previous| {
            previous.due_day != card.due_day
                || previous.remind_time != card.remind_time
                || normalize_lead_days(previous.lead_days.iter().copied()).ok()
                    != normalize_lead_days(card.lead_days.iter().copied()).ok()
                || previous.overdue_enabled != card.overdue_enabled
                || (!previous.enabled && card.enabled)
        });
        let rules_changed_at = if rules_changed {
            now
        } else {
            previous
                .as_ref()
                .map(|previous| previous.rules_changed_at)
                .unwrap_or(now)
        };
        let first_cycle = if card.first_tracked_cycle.is_empty() {
            first_tracked_cycle(now.date_naive(), card.due_day)
        } else {
            card.first_tracked_cycle.clone()
        };
        let leads = normalize_lead_days(card.lead_days.iter().copied())?;
        let created_at = previous
            .as_ref()
            .map(|previous| previous.created_at)
            .unwrap_or(now);
        let connection = self.open()?;
        connection.execute(
            "INSERT INTO credit_cards(
                id,alias,bank,due_day,remind_time,lead_days_json,overdue_enabled,enabled,
                first_tracked_cycle,rules_changed_at,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(id) DO UPDATE SET
                alias=excluded.alias,
                bank=excluded.bank,
                due_day=excluded.due_day,
                remind_time=excluded.remind_time,
                lead_days_json=excluded.lead_days_json,
                overdue_enabled=excluded.overdue_enabled,
                enabled=excluded.enabled,
                first_tracked_cycle=excluded.first_tracked_cycle,
                rules_changed_at=excluded.rules_changed_at,
                updated_at=excluded.updated_at",
            params![
                card.id,
                card.alias.trim(),
                card.bank.as_ref().map(|value| value.trim()),
                card.due_day as i64,
                card.remind_time.format("%H:%M").to_string(),
                serde_json::to_string(&leads)?,
                i64::from(card.overdue_enabled),
                i64::from(card.enabled),
                first_cycle,
                format_dt(rules_changed_at),
                format_dt(created_at),
                format_dt(now),
            ],
        )?;
        Ok(())
    }

    pub(crate) fn credit_card(&self, id: &str) -> Result<Option<CreditCard>> {
        Ok(self.credit_cards()?.into_iter().find(|card| card.id == id))
    }

    pub(crate) fn delete_credit_card(&self, id: &str) -> Result<()> {
        let connection = self.open()?;
        let transaction = connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM credit_card_actions WHERE card_id=?1", [id])?;
        transaction.execute(
            "DELETE FROM finance_shown WHERE kind='credit_card' AND source_id=?1",
            [id],
        )?;
        transaction.execute("DELETE FROM credit_cards WHERE id=?1", [id])?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn credit_card_settled(&self, card_id: &str) -> Result<Option<String>> {
        Ok(self.open()?.query_row(
            "SELECT MAX(cycle_key) FROM credit_card_actions WHERE card_id=?1",
            [card_id],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn mark_credit_card_paid(
        &self,
        card_id: &str,
        cycle: &str,
        due: NaiveDate,
    ) -> Result<()> {
        let Some(card) = self.credit_card(card_id)? else {
            anyhow::bail!("信用卡不存在");
        };
        let due = due_on_for_cycle(cycle, card.due_day).unwrap_or(due);
        self.open()?.execute(
            "INSERT OR IGNORE INTO credit_card_actions(card_id,cycle_key,due_on,action,acted_at)
             VALUES(?1,?2,?3,'paid',?4)",
            params![card_id, cycle, format_date(due), format_dt(now_china())],
        )?;
        Ok(())
    }

    pub(crate) fn undo_credit_card_action(&self, card_id: &str) -> Result<()> {
        self.open()?.execute(
            "DELETE FROM credit_card_actions WHERE id=(
                SELECT id FROM credit_card_actions WHERE card_id=?1 ORDER BY cycle_key DESC,id DESC LIMIT 1
             )",
            [card_id],
        )?;
        Ok(())
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
            "SELECT COUNT(*) FROM credit_cards WHERE enabled=1",
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
    use crate::model::time;
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

    #[test]
    fn v12_creates_default_off_repo_and_card_tables() {
        let (database, root) = database();
        assert_eq!(database.schema_version().unwrap(), 12);
        let repo = database.repo_settings().unwrap();
        assert!(!repo.enabled);
        assert_eq!(repo.primary, Some(time(14, 50)));
        assert_eq!(repo.fallback, None);
        database
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM finance_shown", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
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
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn card_actions_and_shown_ledger_are_local_and_persistent() {
        let (database, root) = database();
        let now = now_china();
        let today = now.date_naive();
        let card = CreditCard {
            id: "card-1".into(),
            alias: "招行 Visa".into(),
            bank: Some("招商银行".into()),
            due_day: 31,
            remind_time: time(9, 0),
            lead_days: vec![3, 0],
            overdue_enabled: true,
            enabled: true,
            first_tracked_cycle: "2026-02".into(),
            rules_changed_at: now,
            created_at: now,
            updated_at: now,
        };
        database.save_credit_card(&card).unwrap();
        database
            .mark_credit_card_paid(
                "card-1",
                "2026-02",
                NaiveDate::from_ymd_opt(2026, 2, 28).unwrap(),
            )
            .unwrap();
        assert_eq!(
            database.credit_card_settled("card-1").unwrap().as_deref(),
            Some("2026-02")
        );
        database
            .mark_finance_shown("credit_card", "card-1", "2026-02:3:2026-02-28", today)
            .unwrap();
        assert!(
            database
                .finance_is_shown("credit_card", "card-1", "2026-02:3:2026-02-28")
                .unwrap()
        );
        database.undo_credit_card_action("card-1").unwrap();
        assert_eq!(database.credit_card_settled("card-1").unwrap(), None);
        database.delete_credit_card("card-1").unwrap();
        assert!(database.credit_cards().unwrap().is_empty());
        assert!(
            !database
                .finance_is_shown("credit_card", "card-1", "2026-02:3:2026-02-28")
                .unwrap()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shown_ledger_is_scoped_by_source_not_just_by_key() {
        // 两张同还款日、同档位的卡会产生同样的账期/档位/日期，只有卡片 ID 不同。
        // 台账必须按 (kind, source_id, occurrence_key) 判定，否则一张展示过就会压掉另一张。
        let (database, root) = database();
        let now = now_china();
        let today = now.date_naive();
        for id in ["card-a", "card-b"] {
            database
                .save_credit_card(&CreditCard {
                    id: id.into(),
                    alias: format!("卡 {id}"),
                    bank: None,
                    due_day: 5,
                    remind_time: time(9, 0),
                    lead_days: vec![3, 0],
                    overdue_enabled: true,
                    enabled: true,
                    first_tracked_cycle: "2026-01".into(),
                    rules_changed_at: now,
                    created_at: now,
                    updated_at: now,
                })
                .unwrap();
        }
        let key = "2026-10:3:2026-10-05";
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
                .unwrap(),
            "另一张卡不应被这张卡的展示记录压掉"
        );
        assert!(
            !database
                .finance_is_shown("reverse_repo", "repo", key)
                .unwrap(),
            "不同 kind 的同名 key 不应互相影响"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

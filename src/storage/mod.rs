use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveTime};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use crate::{
    core::{
        critical_change_reason, event_hash, now_china, plan_reminders, sha256,
        subscription_reminder_allowed_now,
    },
    model::*,
};

const SECONDARY_PENDING: i32 = 0;
const SECONDARY_LEASED: i32 = 1;
const SECONDARY_DELIVERED: i32 = 2;
const SECONDARY_RETRYING: i32 = 3;
const SECONDARY_EXHAUSTED: i32 = 4;
const SECONDARY_CANCELLED: i32 = 5;
const SECONDARY_MAX_ATTEMPTS: i32 = 5;
const SECONDARY_REQUESTS_PER_HOUR: i64 = 20;
const SECONDARY_ATTEMPT_RETENTION_DAYS: i64 = 30;
const SECONDARY_OUTBOX_RETENTION_DAYS: i64 = 90;
const SECONDARY_MAX_ATTEMPT_RECORDS: i64 = 2000;
const LOCAL_DELIVERY_PERSISTENT_FAILURE_MINUTES: i64 = 15;
const RUNTIME_HEARTBEAT_WARNING_MINUTES: i64 = 3;
const RUNTIME_HEARTBEAT_FAILED_MINUTES: i64 = 15;
const BACKUP_PAGES_PER_STEP: i32 = 1024;
const BACKUP_STEP_PAUSE: StdDuration = StdDuration::from_millis(1);
pub const LATEST_SCHEMA_VERSION: i64 = 11;

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}

mod acknowledgements;
mod backup;
mod events;
mod evidence;
mod health;
mod maintenance;
mod mapping;
mod overrides;
mod reminders;
mod schema;
mod secondary;
mod settings;
mod sync;

#[allow(unused_imports)]
use {
    backup::*, events::*, health::*, maintenance::*, mapping::*, overrides::*, reminders::*,
    schema::*, secondary::*, settings::*,
};

#[cfg(test)]
mod tests;

impl Database {
    pub fn new(data_root: &Path) -> Self {
        Self {
            path: data_root.join("stock-ipo-reminder.db"),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn open(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(StdDuration::from_secs(10))?;
        connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        Ok(connection)
    }
}

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};

use crate::{
    announcement,
    core::{at, group_candidates, now_china, reconcile_candidates, sha256},
    model::{
        AnnouncementDocument, AppSettings, Candidate, ChinaDateTime, Exchange, FieldSourceEntry,
        HealthState, IpoEvent, LifecycleStatus, ManualOverrideEntry, ReminderDelivery,
        SyncConclusion, SyncConclusionKind,
    },
    network::{self, CollectorOutput},
    operations, secondary_notification,
    storage::Database,
    windows_integration,
};

const MINIMUM_SYNC_MINUTES: i32 = 5;
const MAXIMUM_SYNC_MINUTES: i32 = 7 * 24 * 60;
const SYNC_WINDOW_START_HOUR: u32 = 6;
const SYNC_WINDOW_END_HOUR: u32 = 22;
const DAILY_DISCOVERY_HOUR: u32 = 8;
const DAILY_MAINTENANCE_HOUR: u32 = 5;
const DAILY_MAINTENANCE_MINUTE: u32 = 30;
const MANAGED_BACKUP_LIMIT: usize = 7;
const MANAGED_BACKUP_MAX_BYTES: u64 = 512 * 1024 * 1024;
const RUNTIME_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
const RUNTIME_FAILURE_RESET_AFTER: Duration = Duration::from_secs(10 * 60);
const RUNTIME_RESTART_DELAYS: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

mod delivery;
mod handle;
mod health;
mod maintenance;
mod run_loop;
mod scheduler;
mod snapshot;
mod synchronization;

#[allow(unused_imports)]
pub(crate) use {
    delivery::*, handle::*, health::*, maintenance::*, run_loop::*, scheduler::*, snapshot::*,
    synchronization::*,
};

#[cfg(test)]
mod tests;

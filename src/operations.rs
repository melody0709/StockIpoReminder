use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use regex::Regex;
use serde_json::json;
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::{core::now_china, storage::Database};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const LOG_SEGMENTS_PER_DAY: usize = 5;
const LOG_RETENTION_DAYS: i64 = 14;
const DIAGNOSTIC_LOG_FILES_TO_INCLUDE: usize = 10;
const APPLICATION_VERSION_MARKER: &str = "application-version.txt";
static LOG_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static LOG_GATE: Mutex<()> = Mutex::new(());
static LAST_LOG_CLEANUP_DATE: Mutex<Option<NaiveDate>> = Mutex::new(None);

pub fn initialize(data_root: &Path) -> Result<()> {
    let directory = data_root.join("logs");
    fs::create_dir_all(&directory)?;
    let _ = LOG_DIRECTORY.set(directory);
    maintain_logs(data_root)?;
    log("INFO", "Rust 正式运行时启动");
    Ok(())
}

pub fn try_run_self_test(arguments: &[String], data_root: &Path) -> Result<Option<i32>> {
    let Some(report_path) = arguments
        .windows(2)
        .find(|pair| pair[0] == "--self-test-report")
        .map(|pair| PathBuf::from(&pair[1]))
    else {
        return Ok(None);
    };
    let result = (|| -> Result<serde_json::Value> {
        let database = Database::new(data_root);
        database.initialize()?;
        database.integrity_check()?;
        let settings = database.settings()?;
        database.save_settings(&settings)?;
        let secondary_notification = database.secondary_notification_summary()?;
        let backup = database.backup(&data_root.join("backups"))?;
        let diagnostic = create_diagnostic_bundle(data_root, &database)?;
        let windows_time_service = match crate::windows_integration::windows_time_service_running()
        {
            Ok(Some(running)) => json!({
                "supported": true,
                "querySucceeded": true,
                "running": running
            }),
            Ok(None) => json!({
                "supported": false,
                "querySucceeded": false,
                "running": null
            }),
            Err(error) => json!({
                "supported": true,
                "querySucceeded": false,
                "running": null,
                "error": redact(&format!("{error:#}"))
            }),
        };
        let mut toast_diagnostics = crate::windows_integration::toast_diagnostics();
        toast_diagnostics.error = toast_diagnostics.error.as_deref().map(redact);
        toast_diagnostics.shortcut_error = toast_diagnostics.shortcut_error.as_deref().map(redact);
        Ok(json!({
            "success": true,
            "implementation": "rust",
            "architecture": std::env::consts::ARCH,
            "pointerWidth": std::mem::size_of::<usize>() * 8,
            "databaseIntegrity": "ok",
            "schemaMigrationVersion": database.schema_version()?,
            "settingsRoundtrip": true,
            "backupCreated": backup.exists(),
            "diagnosticCreated": diagnostic.exists(),
            "windowsTimeService": windows_time_service,
            "windowsToast": toast_diagnostics,
            "secondaryNotification": secondary_notification,
            "dataRoot": "<isolated-data-root>"
        }))
    })();
    let report = match &result {
        Ok(value) => value.clone(),
        Err(error) => {
            json!({"success": false, "implementation": "rust", "error": redact(&format!("{error:#}"))})
        }
    };
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    Ok(Some(if result.is_ok() { 0 } else { 2 }))
}

pub fn prepare_database_upgrade(
    data_root: &Path,
    current_version: &str,
) -> Result<Option<PathBuf>> {
    let database = Database::new(data_root);
    if !database.path().is_file() {
        return Ok(None);
    }
    let marker = data_root.join(APPLICATION_VERSION_MARKER);
    let previous_version = fs::read_to_string(&marker)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if previous_version.as_deref() == Some(current_version) {
        return Ok(None);
    }

    let backup = database
        .backup(&data_root.join("backups"))
        .with_context(|| {
            format!(
                "应用版本从 {} 变更为 {current_version} 前无法创建数据库备份",
                previous_version.as_deref().unwrap_or("未知版本")
            )
        })?;
    log(
        "INFO",
        &format!(
            "应用版本从 {} 变更为 {current_version}，数据库迁移前备份已创建：{}",
            previous_version.as_deref().unwrap_or("未知版本"),
            backup.display()
        ),
    );
    Ok(Some(backup))
}

pub fn mark_database_version(data_root: &Path, current_version: &str) -> Result<()> {
    let marker = data_root.join(APPLICATION_VERSION_MARKER);
    let temporary = marker.with_extension(format!("txt.{}.tmp", uuid::Uuid::new_v4().simple()));
    fs::write(&temporary, format!("{current_version}\n"))?;
    if let Err(error) = atomic_replace_file(&temporary, &marker) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交应用版本标记");
    }
    Ok(())
}

pub fn atomic_replace_file(temporary: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            },
            core::PCWSTR,
        };

        let temporary_wide = temporary
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let target_wide = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        unsafe {
            MoveFileExW(
                PCWSTR(temporary_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )?;
        }
        return Ok(());
    }

    #[cfg(not(windows))]
    {
        if target.exists() {
            fs::remove_file(target)?;
        }
        fs::rename(temporary, target)?;
        Ok(())
    }
}

pub fn log(level: &str, message: &str) {
    let Some(directory) = LOG_DIRECTORY.get() else {
        return;
    };
    let Ok(_guard) = LOG_GATE.lock() else { return };
    let now = now_china();
    if let Ok(mut last_cleanup) = LAST_LOG_CLEANUP_DATE.lock()
        && *last_cleanup != Some(now.date_naive())
    {
        let _ = cleanup_old_logs(directory, now.date_naive());
        *last_cleanup = Some(now.date_naive());
    }
    let path = daily_log_path(directory, now.date_naive());
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
    {
        rotate_daily_log(&path);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} [{}] {}", now.to_rfc3339(), level, redact(message));
    }
}

fn daily_log_path(directory: &Path, date: NaiveDate) -> PathBuf {
    directory.join(format!("stock-ipo-reminder-{}.log", date.format("%Y%m%d")))
}

fn rotate_daily_log(current: &Path) {
    if LOG_SEGMENTS_PER_DAY <= 1 {
        let _ = fs::remove_file(current);
        return;
    }
    let oldest = PathBuf::from(format!(
        "{}.{}",
        current.display(),
        LOG_SEGMENTS_PER_DAY - 1
    ));
    let _ = fs::remove_file(oldest);
    for index in (1..LOG_SEGMENTS_PER_DAY - 1).rev() {
        let source = PathBuf::from(format!("{}.{}", current.display(), index));
        let target = PathBuf::from(format!("{}.{}", current.display(), index + 1));
        if source.exists() {
            let _ = fs::rename(source, target);
        }
    }
    if current.exists() {
        let _ = fs::rename(current, PathBuf::from(format!("{}.1", current.display())));
    }
}

pub fn maintain_logs(data_root: &Path) -> Result<()> {
    let directory = data_root.join("logs");
    fs::create_dir_all(&directory)?;
    cleanup_old_logs(&directory, now_china().date_naive())
}

fn cleanup_old_logs(directory: &Path, today: NaiveDate) -> Result<()> {
    let oldest_kept = today - chrono::Duration::days(LOG_RETENTION_DAYS - 1);
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.path().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if log_date_from_name(&name).is_some_and(|date| date < oldest_kept) {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn log_date_from_name(name: &str) -> Option<NaiveDate> {
    const PREFIX: &str = "stock-ipo-reminder-";
    let suffix = name.strip_prefix(PREFIX)?;
    let date = suffix.get(..8)?;
    suffix
        .get(8..)?
        .starts_with(".log")
        .then(|| NaiveDate::parse_from_str(date, "%Y%m%d").ok())
        .flatten()
}

pub fn redact(value: &str) -> String {
    let authorization =
        Regex::new(r"(?i)(authorization|cookie|set-cookie)\s*[:=]\s*[^\s,;]+(?:[;,][^\s]+)*")
            .unwrap();
    let query = Regex::new(r"(https://[^\s?]+)\?[^\s]+").unwrap();
    let windows_path = Regex::new(r"(?i)[A-Z]:\\(?:[^\\\s]+\\)+[^\s]+").unwrap();
    let value = authorization.replace_all(value, "$1:<redacted>");
    let value = query.replace_all(&value, "$1?<redacted>");
    windows_path
        .replace_all(&value, "<local-path>")
        .into_owned()
}

pub fn create_diagnostic_bundle(data_root: &Path, database: &Database) -> Result<PathBuf> {
    let directory = data_root.join("diagnostics");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!(
        "stock-ipo-reminder-diagnostic-{}.zip",
        now_china().format("%Y%m%d-%H%M%S")
    ));
    let file = File::create(&path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let integrity = database
        .integrity_check()
        .map(|_| "ok".to_owned())
        .unwrap_or_else(|error| redact(&format!("{error:#}")));
    let (health_state, health_text) = database
        .health_text()
        .unwrap_or((crate::model::HealthState::Unknown, "unavailable".into()));
    let settings = database.settings().unwrap_or_default();
    let mut toast_diagnostics = crate::windows_integration::toast_diagnostics();
    toast_diagnostics.error = toast_diagnostics.error.as_deref().map(redact);
    toast_diagnostics.shortcut_error = toast_diagnostics.shortcut_error.as_deref().map(redact);
    let mut health_details = database.health_details().ok();
    if let Some(details) = &mut health_details {
        for source in &mut details.sources {
            source.last_error = source.last_error.as_deref().map(redact);
        }
        for operation in &mut details.operations {
            operation.last_error = operation.last_error.as_deref().map(redact);
        }
    }
    let mut recent_sync_runs = database.recent_sync_runs(50).unwrap_or_default();
    for run in &mut recent_sync_runs {
        run.error = run.error.as_deref().map(redact);
    }
    let mut secondary_notification = database.secondary_notification_summary().ok();
    if let Some(summary) = &mut secondary_notification {
        summary.latest_error = summary.latest_error.as_deref().map(redact);
    }
    let crash_directory = data_root.join("diagnostics").join("crashes");
    let mut crash_reports: Vec<_> = fs::read_dir(&crash_directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .collect();
    crash_reports.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    crash_reports.truncate(10);
    let summary = json!({
        "schemaVersion": "4",
        "generatedAt": now_china().to_rfc3339(),
        "product": "StockIpoReminder-Rust",
        "applicationVersion": env!("CARGO_PKG_VERSION"),
        "schemaMigrationVersion": database.schema_version().unwrap_or_default(),
        "databaseIntegrity": integrity,
        "healthState": health_state as i32,
        "healthText": health_text,
        "healthDetails": health_details,
        "latestSyncConclusion": database.latest_sync_conclusion().ok().flatten(),
        "recentSyncRuns": recent_sync_runs,
        "reminderState": database.reminder_state_summary().ok(),
        "secondaryNotification": secondary_notification,
        "recentReminderLog": database.recent_reminder_log(50).unwrap_or_default(),
        "recentCrashReportCount": crash_reports.len(),
        "todayEventCount": database.today_events().map(|events| events.len()).unwrap_or_default(),
        "pendingCount": database.pending_count().unwrap_or_default(),
        "settings": settings,
        "windowsToast": toast_diagnostics,
        "note": "诊断包不包含 SQLite 数据库、公告原文、Cookie、Authorization、第二通知通道凭据或绝对路径"
    });
    zip.start_file("summary.json", options)?;
    zip.write_all(serde_json::to_string_pretty(&summary)?.as_bytes())?;

    let log_directory = data_root.join("logs");
    if log_directory.exists() {
        let mut entries: Vec<_> = fs::read_dir(log_directory)?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
        for entry in entries.into_iter().take(DIAGNOSTIC_LOG_FILES_TO_INCLUDE) {
            let mut bytes = Vec::new();
            File::open(entry.path())?
                .take(2 * 1024 * 1024)
                .read_to_end(&mut bytes)?;
            let text = redact(&String::from_utf8_lossy(&bytes));
            let name = entry.file_name().to_string_lossy().into_owned();
            zip.start_file(format!("logs/{name}"), options)?;
            zip.write_all(text.as_bytes())?;
        }
    }
    for entry in crash_reports {
        let bytes = fs::read(entry.path())?;
        let text = redact(&String::from_utf8_lossy(&bytes));
        let name = entry.file_name().to_string_lossy().into_owned();
        zip.start_file(format!("crashes/{name}"), options)?;
        zip.write_all(text.as_bytes())?;
    }
    zip.finish().context("无法完成诊断 ZIP")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn redacts_headers_queries_and_paths() {
        let value = redact(
            "Authorization: Bearer-secret https://example.com/a?token=secret C:\\Users\\name\\file.txt",
        );
        assert!(!value.contains("Bearer-secret"));
        assert!(!value.contains("token=secret"));
        assert!(!value.contains("Users\\name"));
    }

    #[test]
    fn daily_log_retention_keeps_fourteen_calendar_days() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-log-retention-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 8, 26).unwrap();
        let oldest_kept = daily_log_path(&root, today - chrono::Duration::days(13));
        let expired = daily_log_path(&root, today - chrono::Duration::days(14));
        let unrelated = root.join("keep-me.txt");
        fs::write(&oldest_kept, "keep").unwrap();
        fs::write(&expired, "remove").unwrap();
        fs::write(&unrelated, "keep").unwrap();

        cleanup_old_logs(&root, today).unwrap();

        assert!(oldest_kept.exists());
        assert!(!expired.exists());
        assert!(unrelated.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn atomic_replace_never_requires_deleting_the_committed_file_first() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-atomic-replace-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("state.json");
        let first = root.join("first.tmp");
        let second = root.join("second.tmp");
        fs::write(&first, "first").unwrap();
        atomic_replace_file(&first, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "first");

        fs::write(&second, "second").unwrap();
        atomic_replace_file(&second, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "second");
        assert!(!first.exists());
        assert!(!second.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn version_change_creates_one_integrity_checked_pre_migration_backup() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-upgrade-backup-test-{}",
            Uuid::new_v4().simple()
        ));
        let database = Database::new(&root);
        database.initialize().unwrap();
        mark_database_version(&root, "0.2.4").unwrap();

        assert!(prepare_database_upgrade(&root, "0.2.4").unwrap().is_none());
        let backup = prepare_database_upgrade(&root, "0.2.5")
            .unwrap()
            .expect("version change should create a backup");
        assert!(backup.is_file());
        let integrity: String = rusqlite::Connection::open(backup)
            .unwrap()
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");

        mark_database_version(&root, "0.2.5").unwrap();
        assert!(prepare_database_upgrade(&root, "0.2.5").unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::json;
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::{core::now_china, storage::Database};

const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
const LOG_FILES_TO_KEEP: usize = 5;
static LOG_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static LOG_GATE: Mutex<()> = Mutex::new(());

pub fn initialize(data_root: &Path) -> Result<()> {
    let directory = data_root.join("logs");
    fs::create_dir_all(&directory)?;
    let _ = LOG_DIRECTORY.set(directory);
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
        let backup = database.backup(&data_root.join("backups"))?;
        let diagnostic = create_diagnostic_bundle(data_root, &database)?;
        Ok(json!({
            "success": true,
            "implementation": "rust",
            "architecture": std::env::consts::ARCH,
            "pointerWidth": std::mem::size_of::<usize>() * 8,
            "databaseIntegrity": "ok",
            "settingsRoundtrip": true,
            "backupCreated": backup.exists(),
            "diagnosticCreated": diagnostic.exists(),
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

pub fn log(level: &str, message: &str) {
    let Some(directory) = LOG_DIRECTORY.get() else {
        return;
    };
    let Ok(_guard) = LOG_GATE.lock() else { return };
    let path = directory.join("stock-ipo-reminder.log");
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
    {
        rotate_logs(directory);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "{} [{}] {}",
            now_china().to_rfc3339(),
            level,
            redact(message)
        );
    }
}

fn rotate_logs(directory: &Path) {
    for index in (1..LOG_FILES_TO_KEEP).rev() {
        let source = directory.join(format!("stock-ipo-reminder.log.{index}"));
        let target = directory.join(format!("stock-ipo-reminder.log.{}", index + 1));
        if source.exists() {
            let _ = fs::rename(source, target);
        }
    }
    let current = directory.join("stock-ipo-reminder.log");
    if current.exists() {
        let _ = fs::rename(current, directory.join("stock-ipo-reminder.log.1"));
    }
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
    let summary = json!({
        "schemaVersion": "1",
        "generatedAt": now_china().to_rfc3339(),
        "product": "StockIpoReminder-Rust",
        "databaseIntegrity": integrity,
        "healthState": health_state as i32,
        "healthText": health_text,
        "todayEventCount": database.today_events().map(|events| events.len()).unwrap_or_default(),
        "pendingCount": database.pending_count().unwrap_or_default(),
        "settings": settings,
        "note": "诊断包不包含 SQLite 数据库、公告原文、Cookie、Authorization 或绝对路径"
    });
    zip.start_file("summary.json", options)?;
    zip.write_all(serde_json::to_string_pretty(&summary)?.as_bytes())?;

    let log_directory = data_root.join("logs");
    if log_directory.exists() {
        for entry in fs::read_dir(log_directory)?
            .filter_map(Result::ok)
            .take(LOG_FILES_TO_KEEP)
        {
            if !entry.path().is_file() {
                continue;
            }
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
    zip.finish().context("无法完成诊断 ZIP")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_headers_queries_and_paths() {
        let value = redact(
            "Authorization: Bearer-secret https://example.com/a?token=secret C:\\Users\\name\\file.txt",
        );
        assert!(!value.contains("Bearer-secret"));
        assert!(!value.contains("token=secret"));
        assert!(!value.contains("Users\\name"));
    }
}

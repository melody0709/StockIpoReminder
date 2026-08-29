use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeDelta, Utc};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::operations;

const REPORT_LIMIT: u64 = 128 * 1024;
const MAX_ATTEMPTS_PER_DAY: usize = 3;
const MAX_STATE_RECORDS: usize = 100;
const MAX_UPLOADED_HASHES: usize = 500;

pub const CRASH_REPORT_URL: &str = match option_env!("STOCK_IPO_CRASH_REPORT_URL") {
    Some(value) => value,
    None => "",
};
pub const CRASH_REPORT_PRIVACY_URL: &str = match option_env!("STOCK_IPO_CRASH_REPORT_PRIVACY_URL") {
    Some(value) => value,
    None => "",
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadAttempt {
    attempted_at_utc: String,
    report_sha256: String,
    success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UploadState {
    schema_version: u32,
    attempts: Vec<UploadAttempt>,
    uploaded_report_sha256: Vec<String>,
}

impl Default for UploadState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            attempts: Vec::new(),
            uploaded_report_sha256: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum UploadOutcome {
    NoReport,
    Uploaded(String),
}

impl UploadOutcome {
    pub fn message(&self) -> String {
        match self {
            Self::NoReport => "没有尚未发送的本地崩溃报告".into(),
            Self::Uploaded(name) => format!("已发送脱敏崩溃报告：{name}"),
        }
    }
}

pub fn configured() -> bool {
    configured_values(CRASH_REPORT_URL, CRASH_REPORT_PRIVACY_URL)
}

pub fn privacy_url() -> Option<&'static str> {
    configured().then_some(CRASH_REPORT_PRIVACY_URL)
}

pub fn configuration_status() -> String {
    if configured() {
        "崩溃报告服务已配置；默认不发送，只有你启用自动发送或明确点击发送时才会上传。".into()
    } else {
        "当前构建未同时嵌入 HTTPS 崩溃报告地址和隐私政策地址；上传保持关闭，本地报告不会离开电脑。"
            .into()
    }
}

pub fn last_result(data_root: &Path) -> Option<String> {
    let value: Value = serde_json::from_slice(
        &fs::read(
            data_root
                .join("diagnostics")
                .join("crash-upload-last-result.json"),
        )
        .ok()?,
    )
    .ok()?;
    value
        .get("detail")
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub fn upload_next(data_root: &Path) -> Result<UploadOutcome> {
    let result = upload_next_inner(data_root);
    if let Err(error) = write_last_result(data_root, &result) {
        operations::log("WARN", &format!("无法写入崩溃报告上传结果：{error:#}"));
    }
    result
}

fn upload_next_inner(data_root: &Path) -> Result<UploadOutcome> {
    if !configured() {
        bail!("当前构建未配置崩溃报告接收服务和隐私政策");
    }
    let endpoint = validated_https_url(CRASH_REPORT_URL, "崩溃报告接收地址")?;
    validated_https_url(CRASH_REPORT_PRIVACY_URL, "崩溃报告隐私政策")?;

    let now = Utc::now();
    let mut state = load_state(data_root);
    prune_state(&mut state, now);
    if attempts_in_window(&state, now) >= MAX_ATTEMPTS_PER_DAY {
        save_state(data_root, &state)?;
        bail!("过去 24 小时已尝试发送 3 次，请稍后再试");
    }

    let Some((path, report_sha256)) = next_report(data_root, &state)? else {
        save_state(data_root, &state)?;
        return Ok(UploadOutcome::NoReport);
    };
    let report_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("崩溃报告文件名无效")?
        .to_owned();
    let report = sanitized_report(&path)?;
    let attempt = UploadAttempt {
        attempted_at_utc: now.to_rfc3339(),
        report_sha256: report_sha256.clone(),
        success: false,
    };
    state.attempts.push(attempt);
    save_state(data_root, &state)?;

    let payload = json!({
        "schemaVersion": 1,
        "product": "StockIpoReminder",
        "applicationVersion": env!("CARGO_PKG_VERSION"),
        "platform": "windows-x64",
        "sentAtUtc": now.to_rfc3339(),
        "reportSha256": report_sha256,
        "report": report,
        "privacy": "No database, logs, announcement body, command line, data path, username, device identifier, account, holding or credential is included."
    });
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(Policy::none())
        .user_agent(format!(
            "StockIpoReminder/{}/CrashReport",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("无法创建崩溃报告上传客户端")?;
    let response = client
        .post(endpoint)
        .json(&payload)
        .send()
        .context("无法连接崩溃报告接收服务")?;
    validated_https_url(response.url().as_str(), "崩溃报告最终地址")?;
    if !response.status().is_success() {
        bail!("崩溃报告接收服务返回 HTTP {}", response.status());
    }

    if let Some(last) = state.attempts.last_mut() {
        last.success = true;
    }
    if !state
        .uploaded_report_sha256
        .iter()
        .any(|value| value == &report_sha256)
    {
        state.uploaded_report_sha256.push(report_sha256);
    }
    if state.uploaded_report_sha256.len() > MAX_UPLOADED_HASHES {
        let remove = state.uploaded_report_sha256.len() - MAX_UPLOADED_HASHES;
        state.uploaded_report_sha256.drain(..remove);
    }
    save_state(data_root, &state)?;
    Ok(UploadOutcome::Uploaded(report_name))
}

fn configured_values(endpoint: &str, privacy: &str) -> bool {
    validated_https_url(endpoint, "崩溃报告接收地址").is_ok()
        && validated_https_url(privacy, "崩溃报告隐私政策").is_ok()
}

fn validated_https_url(value: &str, label: &str) -> Result<Url> {
    let url = Url::parse(value).with_context(|| format!("{label} URL 无效"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("{label}必须使用不含凭据和片段的 HTTPS URL");
    }
    Ok(url)
}

fn state_path(data_root: &Path) -> PathBuf {
    data_root
        .join("diagnostics")
        .join("crash-upload-state.json")
}

fn load_state(data_root: &Path) -> UploadState {
    fs::read(state_path(data_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_state(data_root: &Path, state: &UploadState) -> Result<()> {
    let path = state_path(data_root);
    let directory = path.parent().context("崩溃报告状态目录无效")?;
    fs::create_dir_all(directory)?;
    let temporary = directory.join(format!(
        "crash-upload-state-{}.tmp",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, serde_json::to_vec_pretty(state)?)?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交崩溃报告上传状态");
    }
    Ok(())
}

fn write_last_result(data_root: &Path, result: &Result<UploadOutcome>) -> Result<()> {
    let directory = data_root.join("diagnostics");
    fs::create_dir_all(&directory)?;
    let value = match result {
        Ok(outcome) => json!({
            "success": true,
            "generatedAtUtc": Utc::now().to_rfc3339(),
            "detail": outcome.message()
        }),
        Err(error) => json!({
            "success": false,
            "generatedAtUtc": Utc::now().to_rfc3339(),
            "error": operations::redact(&format!("{error:#}"))
        }),
    };
    let path = directory.join("crash-upload-last-result.json");
    // 与 save_state 一致：临时文件 + 原子替换，避免进程中断留下半写 JSON。
    let temporary = directory.join(format!(
        "crash-upload-last-result-{}.tmp",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交崩溃报告上传结果");
    }
    Ok(())
}

fn prune_state(state: &mut UploadState, now: DateTime<Utc>) {
    let oldest = now - TimeDelta::days(30);
    state.attempts.retain(|attempt| {
        DateTime::parse_from_rfc3339(&attempt.attempted_at_utc)
            .map(|value| value.with_timezone(&Utc) >= oldest)
            .unwrap_or(false)
    });
    if state.attempts.len() > MAX_STATE_RECORDS {
        let remove = state.attempts.len() - MAX_STATE_RECORDS;
        state.attempts.drain(..remove);
    }
}

fn attempts_in_window(state: &UploadState, now: DateTime<Utc>) -> usize {
    let cutoff = now - TimeDelta::hours(24);
    state
        .attempts
        .iter()
        .filter(|attempt| {
            DateTime::parse_from_rfc3339(&attempt.attempted_at_utc)
                .map(|value| value.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
        })
        .count()
}

fn next_report(data_root: &Path, state: &UploadState) -> Result<Option<(PathBuf, String)>> {
    let directory = data_root.join("diagnostics").join("crashes");
    let mut candidates = fs::read_dir(&directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("crash-recovery-") && name.ends_with(".json")
        })
        .filter(|entry| {
            entry
                .metadata()
                .is_ok_and(|metadata| metadata.len() > 0 && metadata.len() <= REPORT_LIMIT)
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| std::cmp::Reverse(path.file_name().map(ToOwned::to_owned)));
    for path in candidates {
        let hash = sha256_file(&path)?;
        if !state
            .uploaded_report_sha256
            .iter()
            .any(|value| value == &hash)
        {
            return Ok(Some((path, hash)));
        }
    }
    Ok(None)
}

fn sanitized_report(path: &Path) -> Result<Value> {
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(REPORT_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > REPORT_LIMIT {
        bail!("崩溃报告为空或超过 128 KiB 上限");
    }
    let value: Value = serde_json::from_slice(&bytes).context("崩溃报告不是有效 JSON")?;
    if !value.is_object() {
        bail!("崩溃报告必须是 JSON 对象");
    }
    Ok(sanitize_value(value))
}

fn sanitize_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut sanitized = Map::new();
            for (key, value) in object {
                if !sensitive_key(&key) {
                    sanitized.insert(key, sanitize_value(value));
                }
            }
            Value::Object(sanitized)
        }
        Value::Array(values) => {
            Value::Array(values.into_iter().take(100).map(sanitize_value).collect())
        }
        Value::String(value) => Value::String(truncate_text(&operations::redact(&value), 4096)),
        other => other,
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
        "command",
        "argument",
        "path",
        "directory",
        "username",
        "hostname",
        "device",
        "account",
        "holding",
        "position",
        "credential",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn truncate_text(value: &str, maximum: usize) -> String {
    let mut result = value.chars().take(maximum).collect::<String>();
    if value.chars().count() > maximum {
        result.push('…');
    }
    result
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_https_url_rejects_password_only_and_fragment() {
        assert!(
            validated_https_url(
                "https://:secret@reports.example.invalid/v1/crashes",
                "崩溃报告接收地址"
            )
            .is_err()
        );
        assert!(
            validated_https_url(
                "https://reports.example.invalid/v1/crashes#frag",
                "崩溃报告接收地址"
            )
            .is_err()
        );
        assert!(
            validated_https_url(
                "https://reports.example.invalid/v1/crashes",
                "崩溃报告接收地址"
            )
            .is_ok()
        );
    }

    #[test]
    fn configuration_requires_two_credential_free_https_urls() {
        assert!(configured_values(
            "https://reports.example.invalid/v1/crashes",
            "https://reports.example.invalid/privacy"
        ));
        assert!(!configured_values(
            "http://reports.example.invalid/v1/crashes",
            "https://reports.example.invalid/privacy"
        ));
        assert!(!configured_values(
            "https://user:secret@reports.example.invalid/v1/crashes",
            "https://reports.example.invalid/privacy"
        ));
        assert!(!configured_values(
            "https://reports.example.invalid/v1/crashes",
            ""
        ));
    }

    #[test]
    fn sanitization_removes_sensitive_keys_and_text_paths() {
        let value = json!({
            "schemaVersion": "1",
            "exitCode": 2,
            "commandLine": "--data-root C:\\Users\\name\\StockIpoReminder",
            "nested": {
                "token": "secret",
                "note": "Authorization: bearer-secret C:\\Users\\name\\file.txt"
            }
        });
        let sanitized = sanitize_value(value);
        assert!(sanitized.get("commandLine").is_none());
        assert!(sanitized["nested"].get("token").is_none());
        let note = sanitized["nested"]["note"].as_str().unwrap();
        assert!(!note.contains("bearer-secret"));
        assert!(!note.contains("Users\\name"));
        assert_eq!(sanitized["exitCode"], 2);
    }

    #[test]
    fn report_selection_is_scoped_deduplicated_and_size_bounded() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-crash-upload-test-{}",
            Uuid::new_v4().simple()
        ));
        let directory = root.join("diagnostics").join("crashes");
        fs::create_dir_all(&directory).unwrap();
        let eligible = directory.join("crash-recovery-20260826-120000-000-1.json");
        fs::write(&eligible, br#"{"schemaVersion":"1","exitCode":2}"#).unwrap();
        fs::write(directory.join("unrelated.json"), b"{}").unwrap();
        fs::write(
            directory.join("crash-recovery-20260826-120001-000-2.json"),
            vec![b'x'; REPORT_LIMIT as usize + 1],
        )
        .unwrap();
        let selected = next_report(&root, &UploadState::default())
            .unwrap()
            .unwrap();
        assert_eq!(selected.0, eligible);
        let state = UploadState {
            uploaded_report_sha256: vec![selected.1],
            ..Default::default()
        };
        assert!(next_report(&root, &state).unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rate_limit_counts_failures_and_successes_in_rolling_day() {
        let now = Utc::now();
        let mut state = UploadState::default();
        for hours in [1, 2, 25] {
            state.attempts.push(UploadAttempt {
                attempted_at_utc: (now - TimeDelta::hours(hours)).to_rfc3339(),
                report_sha256: format!("{hours:064x}"),
                success: hours == 1,
            });
        }
        assert_eq!(attempts_in_window(&state, now), 2);
        state.attempts.push(UploadAttempt {
            attempted_at_utc: now.to_rfc3339(),
            report_sha256: "ff".repeat(32),
            success: false,
        });
        assert_eq!(attempts_in_window(&state, now), MAX_ATTEMPTS_PER_DAY);
    }
}

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    model::{
        AppSettings, ReminderLevel, SecondaryNotificationDelivery, SecondaryNotificationProvider,
    },
    operations,
};

const MAX_SECRET_BYTES: usize = 4096;
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 3500;
const SECRET_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretEnvelope {
    schema_version: u32,
    provider: SecondaryNotificationProvider,
    protected_hex: String,
    updated_at_utc: String,
}

struct SecretMaterial {
    provider: SecondaryNotificationProvider,
    value: String,
}

#[derive(Debug, Clone)]
pub struct SendReceipt {
    pub provider: SecondaryNotificationProvider,
    pub batch_size: usize,
}

impl SendReceipt {
    pub fn message(&self) -> String {
        format!(
            "{} 已发送 {} 条提醒",
            provider_label(self.provider),
            self.batch_size
        )
    }
}

pub fn provider_label(provider: SecondaryNotificationProvider) -> &'static str {
    match provider {
        SecondaryNotificationProvider::WeCom => "企业微信机器人",
        SecondaryNotificationProvider::DingTalk => "钉钉机器人",
        SecondaryNotificationProvider::Feishu => "飞书机器人",
        SecondaryNotificationProvider::PushPlus => "PushPlus",
        _ => "未配置第二通知通道",
    }
}

pub fn configured(data_root: &Path, settings: &AppSettings) -> bool {
    settings.secondary_notification_provider != SecondaryNotificationProvider::Disabled
        && load_secret(data_root)
            .is_ok_and(|secret| secret.provider == settings.secondary_notification_provider)
}

pub fn configuration_status(data_root: &Path, settings: &AppSettings) -> String {
    let provider = settings.secondary_notification_provider;
    if provider == SecondaryNotificationProvider::Disabled {
        return "第二通知通道未配置；提醒只在本机显示。".into();
    }
    match load_secret(data_root) {
        Ok(secret) if secret.provider == provider => format!(
            "{}凭据已由 Windows 当前用户加密保存；不会写入 SQLite、日志或诊断 ZIP。",
            provider_label(provider)
        ),
        Ok(_) => format!(
            "已切换到{}，请填写并保存对应凭据。",
            provider_label(provider)
        ),
        Err(_) => format!(
            "{}尚无有效凭据；请填写后保存设置。",
            provider_label(provider)
        ),
    }
}

pub fn save_secret(
    data_root: &Path,
    provider: SecondaryNotificationProvider,
    value: &str,
) -> Result<()> {
    validate_secret(provider, value)?;
    let protected = protect_current_user(value.as_bytes())?;
    let envelope = SecretEnvelope {
        schema_version: SECRET_SCHEMA_VERSION,
        provider,
        protected_hex: hex::encode(protected),
        updated_at_utc: Utc::now().to_rfc3339(),
    };
    let path = secret_path(data_root);
    let parent = path.parent().context("第二通知通道凭据目录无效")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        "secondary-notification-{}.tmp",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, serde_json::to_vec_pretty(&envelope)?)?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交第二通知通道加密凭据");
    }
    Ok(())
}

pub fn clear_secret(data_root: &Path) -> Result<()> {
    let path = secret_path(data_root);
    if path.exists() {
        fs::remove_file(path).context("无法清除第二通知通道凭据")?;
    }
    Ok(())
}

pub struct SecretSnapshot(Option<Vec<u8>>);

pub fn snapshot_secret(data_root: &Path) -> Result<SecretSnapshot> {
    let path = secret_path(data_root);
    let bytes = if path.exists() {
        Some(fs::read(&path).context("无法读取现有第二通知通道凭据以准备回滚")?)
    } else {
        None
    };
    Ok(SecretSnapshot(bytes))
}

pub fn restore_secret(data_root: &Path, snapshot: &SecretSnapshot) -> Result<()> {
    let path = secret_path(data_root);
    let Some(bytes) = snapshot.0.as_deref() else {
        return clear_secret(data_root);
    };
    let parent = path.parent().context("第二通知通道凭据目录无效")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        "secondary-notification-restore-{}.tmp",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, bytes)?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法恢复第二通知通道加密凭据");
    }
    Ok(())
}

pub fn send_test(data_root: &Path, provider: SecondaryNotificationProvider) -> Result<SendReceipt> {
    let secret = load_matching_secret(data_root, provider)?;
    send_message(
        &secret,
        "Stock IPO Reminder 第二通知通道测试",
        "这是一条用户主动发送的测试消息。收到它表示 HTTPS、加密凭据和服务端响应校验均已通过。",
    )?;
    Ok(SendReceipt {
        provider,
        batch_size: 1,
    })
}

pub fn send_batch(
    data_root: &Path,
    deliveries: &[SecondaryNotificationDelivery],
) -> Result<SendReceipt> {
    let first = deliveries.first().context("第二通知通道批次为空")?;
    if deliveries
        .iter()
        .any(|delivery| delivery.provider != first.provider)
    {
        bail!("第二通知通道批次包含多个服务商");
    }
    let secret = load_matching_secret(data_root, first.provider)?;
    let (title, body) = batch_message(deliveries);
    send_message(&secret, &title, &body)?;
    Ok(SendReceipt {
        provider: first.provider,
        batch_size: deliveries.len(),
    })
}

fn send_message(secret: &SecretMaterial, title: &str, body: &str) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(Policy::none())
        .user_agent(format!(
            "StockIpoReminder/{}/SecondaryNotification",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("无法创建第二通知通道 HTTPS 客户端")?;
    let (endpoint, payload) = provider_request(secret, title, body)?;
    let response = client
        .post(endpoint)
        .json(&payload)
        .send()
        .context("第二通知通道连接失败")?;
    if !response.status().is_success() {
        bail!("第二通知通道返回 HTTP {}", response.status());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        bail!("第二通知通道响应超过 64 KiB 上限");
    }
    let value: Value = serde_json::from_slice(&bytes).context("第二通知通道响应不是有效 JSON")?;
    validate_provider_response(secret.provider, &value)
}

fn provider_request(secret: &SecretMaterial, title: &str, body: &str) -> Result<(Url, Value)> {
    let content = truncate_text(&format!("{title}\n\n{body}"), MAX_MESSAGE_CHARS);
    match secret.provider {
        SecondaryNotificationProvider::WeCom => Ok((
            validate_webhook_url(secret.provider, &secret.value)?,
            json!({"msgtype": "text", "text": {"content": content}}),
        )),
        SecondaryNotificationProvider::DingTalk => Ok((
            validate_webhook_url(secret.provider, &secret.value)?,
            json!({"msgtype": "text", "text": {"content": content}, "at": {"isAtAll": false}}),
        )),
        SecondaryNotificationProvider::Feishu => Ok((
            validate_webhook_url(secret.provider, &secret.value)?,
            json!({"msg_type": "text", "content": {"text": content}}),
        )),
        SecondaryNotificationProvider::PushPlus => Ok((
            Url::parse("https://www.pushplus.plus/send")?,
            json!({
                "token": secret.value,
                "title": truncate_text(title, 128),
                "content": truncate_text(body, MAX_MESSAGE_CHARS),
                "template": "txt"
            }),
        )),
        _ => bail!("第二通知通道服务商无效"),
    }
}

fn validate_provider_response(
    provider: SecondaryNotificationProvider,
    value: &Value,
) -> Result<()> {
    let accepted = match provider {
        SecondaryNotificationProvider::WeCom | SecondaryNotificationProvider::DingTalk => {
            value.get("errcode").and_then(Value::as_i64) == Some(0)
        }
        SecondaryNotificationProvider::Feishu => {
            value.get("code").and_then(Value::as_i64) == Some(0)
        }
        SecondaryNotificationProvider::PushPlus => {
            value.get("code").and_then(Value::as_i64) == Some(200)
        }
        _ => false,
    };
    if accepted {
        return Ok(());
    }
    let code = value
        .get("errcode")
        .or_else(|| value.get("code"))
        .map(Value::to_string)
        .unwrap_or_else(|| "missing".into());
    bail!(
        "{}拒绝消息，响应代码 {}",
        provider_label(provider),
        operations::redact(&code)
    )
}

fn validate_secret(provider: SecondaryNotificationProvider, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_SECRET_BYTES {
        bail!("第二通知通道凭据为空或超过 4 KiB 上限");
    }
    match provider {
        SecondaryNotificationProvider::WeCom
        | SecondaryNotificationProvider::DingTalk
        | SecondaryNotificationProvider::Feishu => {
            validate_webhook_url(provider, value)?;
        }
        SecondaryNotificationProvider::PushPlus => {
            if value.chars().any(char::is_whitespace) || value.chars().count() < 8 {
                bail!("PushPlus token 格式无效");
            }
        }
        _ => bail!("请先选择第二通知通道服务商"),
    }
    Ok(())
}

fn validate_webhook_url(provider: SecondaryNotificationProvider, value: &str) -> Result<Url> {
    let url = Url::parse(value).context("Webhook URL 无效")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Webhook 必须是无凭据、无片段的 HTTPS URL");
    }
    let host = url.host_str().unwrap_or_default();
    let path = url.path();
    let has_query_key = |name: &str| {
        url.query_pairs()
            .any(|(key, value)| key == name && !value.is_empty())
    };
    let valid = match provider {
        SecondaryNotificationProvider::WeCom => {
            host.eq_ignore_ascii_case("qyapi.weixin.qq.com")
                && path == "/cgi-bin/webhook/send"
                && has_query_key("key")
        }
        SecondaryNotificationProvider::DingTalk => {
            host.eq_ignore_ascii_case("oapi.dingtalk.com")
                && path == "/robot/send"
                && has_query_key("access_token")
        }
        SecondaryNotificationProvider::Feishu => {
            (host.eq_ignore_ascii_case("open.feishu.cn")
                || host.eq_ignore_ascii_case("open.larksuite.com"))
                && path.starts_with("/open-apis/bot/v2/hook/")
                && path.len() > "/open-apis/bot/v2/hook/".len()
        }
        _ => false,
    };
    if !valid {
        bail!("Webhook 与所选服务商的官方 HTTPS 地址格式不匹配");
    }
    Ok(url)
}

fn batch_message(deliveries: &[SecondaryNotificationDelivery]) -> (String, String) {
    let title = format!("Stock IPO Reminder：{} 条新股提醒", deliveries.len());
    let mut lines = deliveries
        .iter()
        .take(20)
        .map(|delivery| {
            let message = delivery
                .message
                .as_deref()
                .and_then(|value| value.lines().next())
                .map(operations::redact)
                .unwrap_or_else(|| reminder_level_text(delivery.level).into());
            format!(
                "• {}（{}）{}，到期 {}：{}",
                delivery.event.name,
                delivery.event.display_code(),
                reminder_level_text(delivery.level),
                delivery.due_at.format("%m-%d %H:%M"),
                truncate_text(&message, 180)
            )
        })
        .collect::<Vec<_>>();
    if deliveries.len() > 20 {
        lines.push(format!(
            "• 另有 {} 条提醒，请打开桌面应用查看。",
            deliveries.len() - 20
        ));
    }
    lines.push("请逐只登录券商核对；本程序不读取账户、不判断委托结果，也不会自动下单。".into());
    (title, truncate_text(&lines.join("\n"), MAX_MESSAGE_CHARS))
}

fn reminder_level_text(level: ReminderLevel) -> &'static str {
    match level {
        ReminderLevel::Advance => "申购日前提醒",
        ReminderLevel::Morning => "申购日早间提醒",
        ReminderLevel::BrokerOpening => "券商受理开始提醒",
        ReminderLevel::MarketOpening => "开盘提醒",
        ReminderLevel::Hourly => "申购日持续提醒",
        ReminderLevel::NoonBoundary => "午间边界提醒",
        ReminderLevel::AfternoonOpening => "午后开盘提醒",
        ReminderLevel::FifteenMinutes => "截止前 15 分钟提醒",
        ReminderLevel::FiveMinutes => "截止前 5 分钟提醒",
        ReminderLevel::TwoMinutes => "截止前 2 分钟提醒",
        ReminderLevel::Final => "最终安全截止提醒",
        ReminderLevel::DataChanged => "发行信息变化提醒",
        ReminderLevel::HealthWarning => "数据健康警告",
        ReminderLevel::BallotCheck => "中签查询提醒",
        ReminderLevel::PaymentMorning => "缴款资金早间提醒",
        ReminderLevel::PaymentFollowUp => "缴款资金复核提醒",
        ReminderLevel::ListingMorning => "上市日提醒",
        _ => "新股提醒",
    }
}

fn truncate_text(value: &str, maximum: usize) -> String {
    let mut result = value.chars().take(maximum).collect::<String>();
    if value.chars().count() > maximum {
        result.push('…');
    }
    result
}

fn secret_path(data_root: &Path) -> PathBuf {
    data_root
        .join("secrets")
        .join("secondary-notification.dpapi.json")
}

fn load_matching_secret(
    data_root: &Path,
    provider: SecondaryNotificationProvider,
) -> Result<SecretMaterial> {
    let secret = load_secret(data_root)?;
    if secret.provider != provider {
        bail!("加密凭据与当前第二通知通道服务商不匹配");
    }
    validate_secret(provider, &secret.value)?;
    Ok(secret)
}

fn load_secret(data_root: &Path) -> Result<SecretMaterial> {
    let bytes = fs::read(secret_path(data_root)).context("未找到第二通知通道加密凭据")?;
    if bytes.len() > 16 * 1024 {
        bail!("第二通知通道凭据文件超过大小上限");
    }
    let envelope: SecretEnvelope = serde_json::from_slice(&bytes)?;
    if envelope.schema_version != SECRET_SCHEMA_VERSION {
        bail!("第二通知通道凭据版本不受支持");
    }
    let protected = hex::decode(envelope.protected_hex)?;
    let plaintext = unprotect_current_user(&protected)?;
    if plaintext.len() > MAX_SECRET_BYTES {
        bail!("解密后的第二通知通道凭据超过大小上限");
    }
    let value = String::from_utf8(plaintext).context("第二通知通道凭据不是有效 UTF-8")?;
    Ok(SecretMaterial {
        provider: envelope.provider,
        value,
    })
}

#[cfg(windows)]
fn protect_current_user(value: &[u8]) -> Result<Vec<u8>> {
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            Security::Cryptography::{
                CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
            },
        },
        core::w,
    };

    let entropy_bytes = b"StockIpoReminder/SecondaryNotification/v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len().try_into().context("凭据过长")?,
        pbData: value.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len().try_into().unwrap(),
        pbData: entropy_bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            w!("Stock IPO Reminder secondary notification"),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        let protected = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(protected)
    }
}

#[cfg(windows)]
fn unprotect_current_user(value: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
        },
    };

    let entropy_bytes = b"StockIpoReminder/SecondaryNotification/v1";
    let input = CRYPT_INTEGER_BLOB {
        cbData: value.len().try_into().context("加密凭据过长")?,
        pbData: value.as_ptr() as *mut u8,
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len().try_into().unwrap(),
        pbData: entropy_bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
        let plaintext = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(plaintext)
    }
}

#[cfg(not(windows))]
fn protect_current_user(_value: &[u8]) -> Result<Vec<u8>> {
    bail!("第二通知通道凭据加密仅支持 Windows")
}

#[cfg(not(windows))]
fn unprotect_current_user(_value: &[u8]) -> Result<Vec<u8>> {
    bail!("第二通知通道凭据解密仅支持 Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SecondaryNotificationProvider;

    #[test]
    fn webhook_validation_accepts_only_official_credential_free_https_urls() {
        assert!(
            validate_secret(
                SecondaryNotificationProvider::WeCom,
                "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=test-key"
            )
            .is_ok()
        );
        assert!(
            validate_secret(
                SecondaryNotificationProvider::DingTalk,
                "https://oapi.dingtalk.com/robot/send?access_token=test-token"
            )
            .is_ok()
        );
        assert!(
            validate_secret(
                SecondaryNotificationProvider::Feishu,
                "https://open.feishu.cn/open-apis/bot/v2/hook/test-hook"
            )
            .is_ok()
        );
        assert!(
            validate_secret(
                SecondaryNotificationProvider::WeCom,
                "https://user:secret@qyapi.weixin.qq.com/cgi-bin/webhook/send?key=test"
            )
            .is_err()
        );
        assert!(
            validate_secret(
                SecondaryNotificationProvider::WeCom,
                "https://example.invalid/cgi-bin/webhook/send?key=test"
            )
            .is_err()
        );
    }

    #[test]
    fn provider_payload_and_response_contracts_are_explicit() {
        let secret = SecretMaterial {
            provider: SecondaryNotificationProvider::PushPlus,
            value: "12345678-test-token".into(),
        };
        let (url, payload) = provider_request(&secret, "title", "body").unwrap();
        assert_eq!(url.as_str(), "https://www.pushplus.plus/send");
        assert_eq!(payload["template"], "txt");
        assert!(
            validate_provider_response(
                SecondaryNotificationProvider::WeCom,
                &json!({"errcode": 0})
            )
            .is_ok()
        );
        assert!(
            validate_provider_response(
                SecondaryNotificationProvider::PushPlus,
                &json!({"code": 500})
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_secret_roundtrip_is_bound_to_current_windows_user() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-secondary-secret-test-{}",
            Uuid::new_v4().simple()
        ));
        save_secret(
            &root,
            SecondaryNotificationProvider::PushPlus,
            "12345678-test-token",
        )
        .unwrap();
        let raw = fs::read(secret_path(&root)).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("12345678-test-token"));
        let loaded = load_secret(&root).unwrap();
        assert_eq!(loaded.provider, SecondaryNotificationProvider::PushPlus);
        assert_eq!(loaded.value, "12345678-test-token");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_secret_rotation_atomically_replaces_provider_and_value() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-secondary-rotation-test-{}",
            Uuid::new_v4().simple()
        ));
        save_secret(
            &root,
            SecondaryNotificationProvider::PushPlus,
            "12345678-old-token",
        )
        .unwrap();
        save_secret(
            &root,
            SecondaryNotificationProvider::Feishu,
            "https://open.feishu.cn/open-apis/bot/v2/hook/new-hook",
        )
        .unwrap();

        let loaded = load_secret(&root).unwrap();
        assert_eq!(loaded.provider, SecondaryNotificationProvider::Feishu);
        assert_eq!(
            loaded.value,
            "https://open.feishu.cn/open-apis/bot/v2/hook/new-hook"
        );
        let secret_directory = secret_path(&root).parent().unwrap().to_owned();
        assert!(
            fs::read_dir(secret_directory)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| entry.path().extension().is_none_or(|value| value != "tmp"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn encrypted_secret_is_excluded_from_diagnostic_bundle() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-secondary-diagnostic-test-{}",
            Uuid::new_v4().simple()
        ));
        let secret = "12345678-private-diagnostic-token";
        let database = crate::storage::Database::new(&root);
        database.initialize().unwrap();
        save_secret(&root, SecondaryNotificationProvider::PushPlus, secret).unwrap();
        let bundle = crate::operations::create_diagnostic_bundle(&root, &database).unwrap();
        let file = fs::File::open(bundle).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            assert!(!entry.name().contains("secondary-notification.dpapi"));
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains(secret));
        }
        let _ = fs::remove_dir_all(root);
    }
}

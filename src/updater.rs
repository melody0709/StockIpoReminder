use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{operations, windows_integration};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows::{
    Wdk::System::SystemServices::RtlGetVersion,
    Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, WAIT_OBJECT_0},
        Storage::FileSystem::FILE_SHARE_READ,
        System::{
            Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
            SystemInformation::OSVERSIONINFOW,
            Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS, WaitForSingleObject},
        },
    },
    core::PCWSTR,
};

const MANIFEST_LIMIT: u64 = 256 * 1024;
const SIGNATURE_LIMIT: u64 = 256 * 1024;
const INSTALLER_LIMIT: u64 = 200 * 1024 * 1024;
const UPDATE_STATE_FILE: &str = "update-state.json";
const PENDING_MANIFEST_FILE: &str = "update-manifest.json";
const PENDING_SIGNATURE_FILE: &str = "update-manifest.json.minisig";
/// 自动检查的最小间隔（小时）；手动检查不受此限制。
/// 与进程内成功缓存同周期：每天最多 4 次真实网络检查。
const AUTOMATIC_CHECK_INTERVAL_HOURS: i64 = 6;
/// 清单校验成功结果的进程内缓存时长；命中缓存不发网络请求。
const CHECK_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// 等待 Watchdog 释放 supervisor 锁的上限；超时取消安装而不是依赖固定睡眠。
const SUPERVISOR_WAIT_LIMIT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const PROCESS_SYNCHRONIZE: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0010_0000);

/// 稳定版更新 feed 固定编译进客户端，避免漏设环境变量导致正式包缺少更新能力。
pub const UPDATE_FEED_URL: &str =
    "https://github.com/melody0709/StockIpoReminder/releases/latest/download/update-manifest.json";
/// 信任根：仓库内置的 Minisign 公钥（私钥保存在仓库外并加密）。
const UPDATE_PUBLIC_KEY: &str = include_str!("../assets/update-signing/stock-ipo-update.pub");

/// 更新通知的带类型激活参数；不得与股票事件 ID 混用。
pub const ACTIVATION_UPDATE_AVAILABLE: &str = "update:available";
pub const ACTIVATION_UPDATE_READY: &str = "update:ready";

pub fn is_update_activation(value: &str) -> bool {
    value == ACTIVATION_UPDATE_AVAILABLE || value == ACTIVATION_UPDATE_READY
}

/// 把清单中的发布说明文件名解析为与更新源同目录的安全 HTTPS URL。
/// 只接受不含协议、主机和路径分隔符的相对文件名，避免清单把用户导向任意站点。
pub fn release_notes_url(manifest: &UpdateManifest) -> Option<String> {
    let value = manifest.release_notes_url.as_deref()?.trim();
    if value.is_empty()
        || value.contains(['/', '\\'])
        || value.contains("://")
        || !value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '.' | '_' | '-'))
    {
        return None;
    }
    let base = validated_https_url(UPDATE_FEED_URL, "更新清单").ok()?;
    let joined = base.join(value).ok()?;
    validated_https_url(joined.as_str(), "发布说明").ok()?;
    Some(joined.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstaller {
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub product: String,
    pub channel: String,
    pub version: String,
    pub published_at_utc: String,
    pub minimum_windows_build: u32,
    pub release_notes_url: Option<String>,
    pub installer: UpdateInstaller,
}

#[derive(Debug, Clone)]
pub struct AvailableUpdate {
    pub manifest: UpdateManifest,
    manifest_bytes: Vec<u8>,
    signature_bytes: Vec<u8>,
    installer_url: Url,
}

#[derive(Debug, Clone)]
pub struct PendingUpdate {
    pub manifest: UpdateManifest,
    pub installer_path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum UpdateCheck {
    UpToDate,
    Available(AvailableUpdate),
}

/// 运行状态（不进入 AppSettings 或 SQLite）：记录自动检查节流、
/// 每版本一次性提醒和待安装版本。Option 字段缺失时 serde 自动回填
/// None；schemaVersion 缺失或非 1 视为文件损坏，由读取方回退默认值。
/// 未来新增非 Option 字段时在该字段上单独标注 `#[serde(default)]`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub schema_version: u32,
    pub last_automatic_check_at_utc: Option<String>,
    pub pending_version: Option<String>,
    /// 用户点过「稍后」的版本；该版本不再占用界面，出现更高版本时重新展示。
    pub banner_dismissed_version: Option<String>,
}

impl Default for UpdateState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            last_automatic_check_at_utc: None,
            pending_version: None,
            banner_dismissed_version: None,
        }
    }
}

static UPDATE_STATE_MUTEX: Mutex<()> = Mutex::new(());

/// 只缓存成功结果（含「已是最新」）；失败不写缓存，因此下一次自动检查仍会重试。
struct CachedCheck {
    checked_at: Instant,
    check: UpdateCheck,
}

static CHECK_CACHE: Mutex<Option<CachedCheck>> = Mutex::new(None);

pub fn configured() -> bool {
    trusted_public_key().is_ok()
}

pub fn configuration_status() -> String {
    if configured() {
        "安全自动更新已配置：内置 Minisign 公钥验证签名清单，安装前复核大小与 SHA-256。".into()
    } else {
        "当前构建未正确嵌入更新签名公钥；自动更新保持关闭。".into()
    }
}

pub fn trusted_public_key() -> Result<minisign_verify::PublicKey> {
    minisign_verify::PublicKey::decode(UPDATE_PUBLIC_KEY).context("无法解析内置更新签名公钥")
}

/// 验证清单原始字节的 Minisign 预哈希签名；成功前调用方不得解析清单字段。
pub fn verify_minisign_signature(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    public_key: &minisign_verify::PublicKey,
) -> Result<()> {
    let signature_text =
        std::str::from_utf8(signature_bytes).context("更新清单签名不是有效的 UTF-8 文本")?;
    let signature = minisign_verify::Signature::decode(signature_text)
        .context("无法解析更新清单 Minisign 签名")?;
    public_key
        .verify(manifest_bytes, &signature, false)
        .map_err(|error| anyhow::anyhow!("更新清单 Minisign 签名验证失败：{error}"))
}

/// 检查更新；`force=false` 时命中 6 小时内的成功缓存直接返回，不发网络请求。
pub fn check_for_update_with_cache(force: bool) -> Result<UpdateCheck> {
    if !force
        && let Some(cached) = CHECK_CACHE
            .lock()
            .ok()
            .and_then(|slot| {
                slot.as_ref()
                    .map(|entry| (entry.checked_at, entry.check.clone()))
            })
            .filter(|(checked_at, _)| checked_at.elapsed() < CHECK_CACHE_TTL)
    {
        return Ok(cached.1);
    }
    let result = fetch_update_check()?;
    if let Ok(mut slot) = CHECK_CACHE.lock() {
        *slot = Some(CachedCheck {
            checked_at: Instant::now(),
            check: result.clone(),
        });
    }
    Ok(result)
}

fn fetch_update_check() -> Result<UpdateCheck> {
    let manifest_url = validated_https_url(UPDATE_FEED_URL, "更新清单")?;
    let signature_url = minisign_signature_url(&manifest_url)?;
    let client = update_client()?;
    let manifest_bytes = fetch_limited(&client, &manifest_url, MANIFEST_LIMIT, "更新清单")?;
    let signature_bytes = fetch_limited(&client, &signature_url, SIGNATURE_LIMIT, "更新清单签名")?;
    let public_key = trusted_public_key()?;
    verify_minisign_signature(&manifest_bytes, &signature_bytes, &public_key)?;
    let manifest: UpdateManifest =
        serde_json::from_slice(&manifest_bytes).context("无法解析签名更新清单")?;
    let installer_url = validate_manifest(&manifest, &manifest_url)?;
    if compare_versions(&manifest.version, env!("CARGO_PKG_VERSION"))?
        != std::cmp::Ordering::Greater
    {
        return Ok(UpdateCheck::UpToDate);
    }
    Ok(UpdateCheck::Available(AvailableUpdate {
        manifest,
        manifest_bytes,
        signature_bytes,
        installer_url,
    }))
}

/// 待安装更新的受控目录；其中的文件名全部由程序按清单版本生成。
pub fn pending_directory(data_root: &Path) -> PathBuf {
    data_root.join("updates").join("pending")
}

pub fn pending_installer_file_name(version: &str) -> String {
    format!("StockIpoReminder-{version}-win-x64.msi")
}

/// 下载/验签过程的清理守卫：pending 提交成功前任何失败都尽力删除已生成的
/// 更新文件（.part 与提交后的 .msi），避免失败残留长期占用磁盘。
struct PendingUpdateFile {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl PendingUpdateFile {
    fn new(path: PathBuf) -> Self {
        Self {
            paths: vec![path],
            armed: true,
        }
    }

    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingUpdateFile {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for path in &self.paths {
            if let Err(error) = fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                operations::log("WARN", &format!("清理未完成的更新文件失败：{error}"));
            }
        }
    }
}

/// 下载 MSI 并把清单、签名与安装包一起提交到受控 pending 目录。
/// 不退出应用；安装由 [`request_install`] 单独触发。
pub fn download_and_verify_update(
    data_root: &Path,
    update: &AvailableUpdate,
    progress: Option<&dyn Fn(u64, u64)>,
) -> Result<PendingUpdate> {
    if crate::deployment::installed_msi_product_code()?.is_none() {
        bail!("自动更新只支持由 Windows Installer 管理的安装版");
    }
    let client = update_client()?;
    let temporary = data_root.join("temp").join("updates");
    fs::create_dir_all(&temporary).context("无法创建更新下载目录")?;
    let partial = partial_download_path(&temporary, &update.manifest.version, Uuid::new_v4());
    let mut guard = PendingUpdateFile::new(partial.clone());
    let result = (|| -> Result<PendingUpdate> {
        download_installer(&client, update, &partial, progress)?;
        let pending_root = pending_directory(data_root);
        fs::create_dir_all(&pending_root).context("无法创建待安装更新目录")?;
        let installer = pending_root.join(pending_installer_file_name(&update.manifest.version));
        fs::rename(&partial, &installer).context("无法提交已验证的更新安装包")?;
        guard.track(installer.clone());
        write_pending_file(&pending_root, PENDING_MANIFEST_FILE, &update.manifest_bytes)?;
        write_pending_file(
            &pending_root,
            PENDING_SIGNATURE_FILE,
            &update.signature_bytes,
        )?;
        remove_stale_pending_files(&pending_root, &update.manifest.installer.url)?;
        mutate_update_state(data_root, |state| {
            state.pending_version = Some(update.manifest.version.clone());
        })?;
        Ok(PendingUpdate {
            manifest: update.manifest.clone(),
            installer_path: installer,
        })
    })();
    if result.is_ok() {
        // pending 已完整提交，由安装流程或下次启动的恢复逻辑接管。
        guard.disarm();
    }
    result
}

fn partial_download_path(directory: &Path, version: &str, operation_id: Uuid) -> PathBuf {
    directory.join(format!(
        ".StockIpoReminder-{version}-win-x64-{}.msi.part",
        operation_id.simple()
    ))
}

fn write_pending_file(pending_root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let target = pending_root.join(name);
    let temporary = pending_root.join(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary, bytes).context("无法写入待安装更新文件")?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交待安装更新文件");
    }
    Ok(())
}

/// pending 目录中只保留当前清单对应的三个文件；其余旧版本残留和崩溃
/// 遗留的 .tmp 中间文件一律删除，避免长期占用磁盘。
fn remove_stale_pending_files(pending_root: &Path, keep_installer: &str) -> Result<()> {
    for entry in fs::read_dir(pending_root).context("无法读取待安装更新目录")? {
        let entry = entry.context("无法读取待安装更新目录条目")?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stale =
            (name.ends_with(".msi") || name.ends_with(".minisig") || name.ends_with(".tmp"))
                && name != keep_installer
                && name != PENDING_MANIFEST_FILE
                && name != PENDING_SIGNATURE_FILE;
        if stale
            && let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(anyhow::anyhow!("清理过期待安装更新失败：{error}"));
        }
    }
    Ok(())
}

/// 从受控 pending 目录重新验证全部材料：清单签名、schema、版本、
/// Windows Build、安装包大小与 SHA-256。主进程与 helper 共用此入口。
pub fn verify_pending(data_root: &Path) -> Result<PendingUpdate> {
    let directory = pending_directory(data_root);
    let manifest_bytes =
        fs::read(directory.join(PENDING_MANIFEST_FILE)).context("缺少待安装更新清单")?;
    let signature_bytes =
        fs::read(directory.join(PENDING_SIGNATURE_FILE)).context("缺少待安装更新清单签名")?;
    let public_key = trusted_public_key()?;
    verify_minisign_signature(&manifest_bytes, &signature_bytes, &public_key)?;
    let manifest: UpdateManifest =
        serde_json::from_slice(&manifest_bytes).context("无法解析待安装更新清单")?;
    let base = validated_https_url(UPDATE_FEED_URL, "更新清单")?;
    validate_manifest(&manifest, &base)?;
    if compare_versions(&manifest.version, env!("CARGO_PKG_VERSION"))?
        != std::cmp::Ordering::Greater
    {
        bail!("待安装更新版本不高于当前版本，已拒绝");
    }
    let installer_path = directory.join(&manifest.installer.url);
    let metadata = fs::metadata(&installer_path).context("缺少待安装更新安装包")?;
    if metadata.len() != manifest.installer.size_bytes {
        bail!("待安装更新安装包大小与清单不一致");
    }
    if sha256_file(&installer_path)? != normalize_sha256(&manifest.installer.sha256)? {
        bail!("待安装更新安装包 SHA-256 与清单不一致");
    }
    Ok(PendingUpdate {
        manifest,
        installer_path,
    })
}

/// 删除 pending 目录中的全部受控文件并清空状态中的待安装版本。
pub fn cleanup_pending(data_root: &Path) {
    let directory = pending_directory(data_root);
    if let Ok(entries) = fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Err(error) = fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                operations::log("WARN", &format!("清理待安装更新文件失败：{error}"));
            }
        }
    }
    let _ = mutate_update_state(data_root, |state| state.pending_version = None);
}

/// pending 目录中是否确实存在清单材料；用于区分“没有待安装更新”和“待安装更新损坏”。
pub fn pending_manifest_present(data_root: &Path) -> bool {
    pending_directory(data_root)
        .join(PENDING_MANIFEST_FILE)
        .is_file()
}

/// 确认 pending 完整后启动安装 helper；helper 只接收 data_root 和父进程 PID，
/// 全部材料由它自己从受控目录重新读取并验证。
pub fn request_install(data_root: &Path) -> Result<String> {
    verify_pending(data_root)?;
    dispatch_install_helper(data_root)?;
    Ok("待安装更新已重新验证；程序退出后将启动 Windows Installer 完成升级".into())
}

pub fn try_handle(arguments: &[String]) -> Result<Option<i32>> {
    if arguments
        .iter()
        .any(|value| value == "--update-bundle-self-test")
    {
        return run_bundle_self_test(arguments).map(Some);
    }
    if !arguments
        .iter()
        .any(|value| value == "--update-install-helper")
    {
        return Ok(None);
    }
    let result = (|| -> Result<InstallOutcome> {
        let data_root = argument_path(arguments, "--data-root").context("缺少数据目录")?;
        let parent_pid = argument_value(arguments, "--parent-pid")
            .context("缺少父进程编号")?
            .parse::<u32>()
            .context("父进程编号无效")?;
        run_install_helper(&data_root, parent_pid)
    })();
    if let Some(data_root) = argument_path(arguments, "--data-root") {
        let _ = write_update_result(&data_root, &result);
    }
    Ok(Some(if result.is_ok() { 0 } else { 2 }))
}

/// helper 写入的安装结果；`version` 只在成功路径上存在，用于新版启动后的一次性回执。
struct InstallOutcome {
    detail: String,
    version: String,
}

pub fn last_result(data_root: &Path) -> Option<String> {
    let path = data_root
        .join("diagnostics")
        .join("update-last-result.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    value
        .get("detail")
        .or_else(|| value.get("error"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

/// 新版首次启动时读取一次安装成功回执：只在成功、版本与当前运行版本一致
/// 且尚未消费时返回版本号，并立即把该回执标记为已消费，避免每次启动重复提示。
pub fn consume_upgrade_receipt(data_root: &Path) -> Option<String> {
    let path = data_root
        .join("diagnostics")
        .join("update-last-result.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
    if value.get("consumed").and_then(|flag| flag.as_bool()) == Some(true)
        || value.get("success").and_then(|flag| flag.as_bool()) != Some(true)
    {
        return None;
    }
    let version = value
        .get("version")
        .and_then(|item| item.as_str())?
        .to_owned();
    if version != env!("CARGO_PKG_VERSION") {
        return None;
    }
    value["consumed"] = serde_json::Value::Bool(true);
    if let Ok(bytes) = serde_json::to_vec_pretty(&value) {
        let _ = fs::write(&path, bytes);
    }
    Some(version)
}

fn update_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .redirect(Policy::limited(3))
        .user_agent(format!(
            "StockIpoReminder/{}/Windows",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("无法创建自动更新网络客户端")
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

fn minisign_signature_url(manifest_url: &Url) -> Result<Url> {
    let mut signature = manifest_url.clone();
    signature.set_fragment(None);
    signature.set_path(&format!("{}.minisig", manifest_url.path()));
    Ok(signature)
}

fn fetch_limited(client: &Client, url: &Url, limit: u64, label: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url.clone())
        .send()
        .with_context(|| format!("无法下载{label}"))?
        .error_for_status()
        .with_context(|| format!("{label}返回错误状态"))?;
    validated_https_url(response.url().as_str(), label)?;
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("{label}超过大小上限");
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("无法读取{label}"))?;
    if bytes.len() as u64 > limit {
        bail!("{label}超过大小上限");
    }
    Ok(bytes)
}

fn validate_manifest(manifest: &UpdateManifest, base_url: &Url) -> Result<Url> {
    if manifest.schema_version != 2
        || manifest.product != "StockIpoReminder"
        || manifest.channel != "stable"
    {
        bail!("更新清单产品、通道或 schema 不受支持");
    }
    parse_version(&manifest.version)?;
    DateTime::parse_from_rfc3339(&manifest.published_at_utc)
        .context("更新清单发布时间不是有效的 RFC 3339 时间")?;
    normalize_sha256(&manifest.installer.sha256)?;
    if manifest.installer.size_bytes == 0 || manifest.installer.size_bytes > INSTALLER_LIMIT {
        bail!("更新安装包大小超出允许范围");
    }
    if manifest.installer.url != pending_installer_file_name(&manifest.version) {
        bail!("更新安装包文件名与清单版本不匹配");
    }
    if manifest.minimum_windows_build > current_windows_build()? {
        bail!(
            "更新要求 Windows Build {}，当前系统不满足",
            manifest.minimum_windows_build
        );
    }
    let installer_url = base_url
        .join(&manifest.installer.url)
        .context("更新安装包 URL 无效")?;
    validated_https_url(installer_url.as_str(), "更新安装包")
}

fn download_installer(
    client: &Client,
    update: &AvailableUpdate,
    target: &Path,
    progress: Option<&dyn Fn(u64, u64)>,
) -> Result<()> {
    let mut response = client
        .get(update.installer_url.clone())
        .send()
        .context("无法下载更新安装包")?
        .error_for_status()
        .context("更新安装包返回错误状态")?;
    validated_https_url(response.url().as_str(), "更新安装包最终地址")?;
    if response
        .content_length()
        .is_some_and(|length| length != update.manifest.installer.size_bytes)
    {
        bail!("更新安装包 Content-Length 与签名清单不一致");
    }
    let mut file = fs::File::create(target).context("无法创建更新安装包临时文件")?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = response.read(&mut buffer).context("读取更新安装包失败")?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > update.manifest.installer.size_bytes || total > INSTALLER_LIMIT {
            bail!("更新安装包超过签名清单声明的大小");
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])?;
        if let Some(progress) = progress {
            progress(total, update.manifest.installer.size_bytes);
        }
    }
    file.sync_all()?;
    if total != update.manifest.installer.size_bytes {
        bail!("更新安装包大小与签名清单不一致");
    }
    let actual = hex::encode(hasher.finalize());
    if actual != normalize_sha256(&update.manifest.installer.sha256)? {
        bail!("更新安装包 SHA-256 与签名清单不一致");
    }
    Ok(())
}

fn dispatch_install_helper(data_root: &Path) -> Result<()> {
    let current_executable = env::current_exe()?;
    let helper = env::temp_dir().join(format!(
        "StockIpoReminder-Update-{}.exe",
        Uuid::new_v4().simple()
    ));
    fs::copy(&current_executable, &helper).context("无法创建更新安装助手")?;
    let parent_pid = std::process::id().to_string();
    let mut command = Command::new(&helper);
    command.args([
        "--update-install-helper",
        "--parent-pid",
        &parent_pid,
        "--data-root",
        data_root.to_string_lossy().as_ref(),
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn().context("无法启动更新安装助手")?;
    windows_integration::delete_after_reboot(&helper);
    Ok(())
}

fn run_install_helper(data_root: &Path, parent_pid: u32) -> Result<InstallOutcome> {
    // 只信任受控 pending 目录中的材料；命令行不携带哈希或安装包路径。
    let pending = verify_pending(data_root)?;
    let version = pending.manifest.version.clone();
    let installer = pending.installer_path;
    // 以禁止写入和删除共享的方式持有安装包句柄直到 msiexec 结束。
    let installer_lock = open_installer_read_locked(&installer)?;
    // 锁定后在锁保护下重新验证内容：关闭“验证完成后、取得锁之前”
    // 安装包被替换的 TOCTOU 窗口。
    verify_locked_installer(&installer, &pending.manifest.installer)?;
    wait_for_parent_exit(parent_pid)?;
    // 轮询取得 supervisor 锁即证明 Watchdog 已观察到正常退出并释放旧 EXE。
    let supervisor = wait_for_supervisor_mutex(data_root)?;
    let exit_code = run_msiexec(&installer)?;
    drop(installer_lock);
    if exit_code != 0 && exit_code != 3010 {
        // 安装失败或用户取消 UAC：保留可重试 pending，并恢复启动当前版本。
        drop(supervisor);
        if let Err(error) = relaunch_installed_app() {
            operations::log(
                "WARN",
                &format!("更新失败后恢复启动当前版本失败：{error:#}"),
            );
        }
        bail!("Windows Installer 更新失败：exit={exit_code}");
    }
    cleanup_pending(data_root);
    if exit_code == 3010 {
        drop(supervisor);
        return Ok(InstallOutcome {
            detail: "更新安装成功，需要重新启动 Windows 后完成".into(),
            version,
        });
    }
    drop(supervisor);
    relaunch_installed_app().context("更新安装成功，但自动启动新版本失败")?;
    Ok(InstallOutcome {
        detail: "更新安装成功，已启动新版本".into(),
        version,
    })
}

#[cfg(windows)]
fn open_installer_read_locked(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .with_context(|| format!("无法锁定待安装更新安装包：{}", path.display()))
}

/// 在只读锁保护下复核安装包大小与 SHA-256；与清单声明一致才继续安装。
fn verify_locked_installer(path: &Path, expected: &UpdateInstaller) -> Result<()> {
    let metadata = fs::metadata(path).context("无法读取锁定的更新安装包")?;
    if metadata.len() != expected.size_bytes {
        bail!("锁定后的更新安装包大小与清单不一致");
    }
    if sha256_file(path)? != normalize_sha256(&expected.sha256)? {
        bail!("锁定后的更新安装包 SHA-256 与清单不一致");
    }
    Ok(())
}

#[cfg(not(windows))]
fn open_installer_read_locked(_path: &Path) -> Result<fs::File> {
    bail!("当前平台不支持 MSI 自动更新")
}

#[cfg(windows)]
fn wait_for_supervisor_mutex(data_root: &Path) -> Result<windows_integration::SingleInstance> {
    let deadline = Instant::now() + SUPERVISOR_WAIT_LIMIT;
    loop {
        if let Some(instance) =
            windows_integration::SingleInstance::try_acquire_supervisor(data_root)?
        {
            return Ok(instance);
        }
        if Instant::now() >= deadline {
            bail!("等待 Watchdog 释放单实例锁超时，已取消更新安装");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(windows))]
fn wait_for_supervisor_mutex(_data_root: &Path) -> Result<()> {
    bail!("当前平台不支持 MSI 自动更新")
}

#[cfg(windows)]
fn run_msiexec(installer: &Path) -> Result<i32> {
    let msiexec = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("msiexec.exe");
    let mut command = Command::new(msiexec);
    command
        .args([
            "/i",
            installer.to_string_lossy().as_ref(),
            "/passive",
            "/norestart",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.creation_flags(CREATE_NO_WINDOW);
    let status = command
        .status()
        .context("无法启动 Windows Installer 更新")?;
    Ok(status.code().unwrap_or(-1))
}

#[cfg(not(windows))]
fn run_msiexec(_installer: &Path) -> Result<i32> {
    bail!("当前平台不支持 MSI 自动更新")
}

/// 从 HKLM\Software\StockIpoReminder\InstallFolder 读取安装目录并启动应用。
fn relaunch_installed_app() -> Result<()> {
    let folder = installed_folder_from_registry()?;
    let executable = folder.join("StockIpoReminder.exe");
    if !executable.is_file() {
        bail!("安装目录中没有找到可执行文件：{}", executable.display());
    }
    let mut command = Command::new(&executable);
    command
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn().context("无法启动已安装的应用")?;
    Ok(())
}

#[cfg(windows)]
fn installed_folder_from_registry() -> Result<PathBuf> {
    let sub_key = wide_null(r"Software\StockIpoReminder");
    let value_name = wide_null("InstallFolder");
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * std::mem::size_of::<u16>()) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub_key.as_ptr()),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        bail!("无法读取安装目录注册表值：error=0x{:08x}", status.0 as u32);
    }
    let length = (size as usize) / std::mem::size_of::<u16>();
    let text = String::from_utf16_lossy(&buffer[..length]);
    let path = PathBuf::from(text.trim_end_matches('\0'));
    if !path.is_absolute() {
        bail!("注册表安装目录不是绝对路径");
    }
    Ok(path)
}

#[cfg(not(windows))]
fn installed_folder_from_registry() -> Result<PathBuf> {
    bail!("当前平台不支持读取 Windows 安装目录")
}

fn write_update_result(data_root: &Path, result: &Result<InstallOutcome>) -> Result<()> {
    let directory = data_root.join("diagnostics");
    fs::create_dir_all(&directory)?;
    let value = match result {
        Ok(outcome) => json!({
            "success": true,
            "detail": outcome.detail,
            "version": outcome.version,
            "consumed": false,
        }),
        Err(error) => json!({"success": false, "error": format!("{error:#}"), "consumed": false}),
    };
    fs::write(
        directory.join("update-last-result.json"),
        serde_json::to_vec_pretty(&value)?,
    )?;
    Ok(())
}

fn run_bundle_self_test(arguments: &[String]) -> Result<i32> {
    let manifest_path = argument_path(arguments, "--manifest").context("缺少更新清单")?;
    let signature_path = argument_path(arguments, "--signature").context("缺少清单签名")?;
    let installer_path = argument_path(arguments, "--installer").context("缺少安装包")?;
    let public_key_path = argument_path(arguments, "--public-key").context("缺少验签公钥")?;
    let report_path = argument_path(arguments, "--report").context("缺少自测试报告")?;
    let result = (|| -> Result<UpdateManifest> {
        let manifest_bytes = fs::read(&manifest_path)?;
        let signature_bytes = fs::read(&signature_path)?;
        let public_key =
            minisign_verify::PublicKey::from_file(&public_key_path).context("无法读取验签公钥")?;
        verify_minisign_signature(&manifest_bytes, &signature_bytes, &public_key)?;
        let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)?;
        let base = Url::parse("https://updates.example.invalid/update-manifest.json")?;
        validate_manifest(&manifest, &base)?;
        if installer_path.file_name().and_then(|value| value.to_str())
            != Some(manifest.installer.url.as_str())
        {
            bail!("安装包文件名与更新清单不一致");
        }
        let metadata = fs::metadata(&installer_path)?;
        if metadata.len() != manifest.installer.size_bytes {
            bail!("安装包大小与更新清单不一致");
        }
        if sha256_file(&installer_path)? != normalize_sha256(&manifest.installer.sha256)? {
            bail!("安装包 SHA-256 与更新清单不一致");
        }
        Ok(manifest)
    })();
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let report = match &result {
        Ok(manifest) => json!({"success": true, "version": manifest.version}),
        Err(error) => json!({"success": false, "error": format!("{error:#}")}),
    };
    fs::write(report_path, serde_json::to_vec_pretty(&report)?)?;
    Ok(if result.is_ok() { 0 } else { 2 })
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn normalize_sha256(value: &str) -> Result<String> {
    let normalized = value
        .chars()
        .filter(|value| !value.is_ascii_whitespace() && *value != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|value| value.is_ascii_hexdigit()) {
        bail!("SHA-256 指纹必须是 64 位十六进制字符串");
    }
    Ok(normalized)
}

fn parse_version(value: &str) -> Result<[u64; 3]> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        bail!("更新版本必须是 x.y.z");
    }
    Ok([
        parts[0].parse().context("更新主版本号无效")?,
        parts[1].parse().context("更新次版本号无效")?,
        parts[2].parse().context("更新修订版本号无效")?,
    ])
}

fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

fn argument_path(arguments: &[String], name: &str) -> Option<PathBuf> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| PathBuf::from(&pair[1]))
}

fn argument_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

pub fn load_update_state(data_root: &Path) -> UpdateState {
    let path = data_root.join(UPDATE_STATE_FILE);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<UpdateState>(&bytes) {
            Ok(state) if state.schema_version == 1 => state,
            Ok(_) => {
                operations::log("WARN", "更新状态 schema 不受支持，已重置为默认值");
                UpdateState::default()
            }
            Err(error) => {
                operations::log("WARN", &format!("更新状态文件已损坏，已重置：{error}"));
                UpdateState::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => UpdateState::default(),
        Err(error) => {
            operations::log("WARN", &format!("读取更新状态失败，按默认值处理：{error}"));
            UpdateState::default()
        }
    }
}

/// 进程内互斥地读取-修改-写回更新状态，避免并发读改写丢失字段。
pub fn mutate_update_state<T>(
    data_root: &Path,
    mutate: impl FnOnce(&mut UpdateState) -> T,
) -> Result<T> {
    let _guard = UPDATE_STATE_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut state = load_update_state(data_root);
    let result = mutate(&mut state);
    let path = data_root.join(UPDATE_STATE_FILE);
    let temporary = data_root.join(format!(
        ".{}.{}.tmp",
        UPDATE_STATE_FILE,
        Uuid::new_v4().simple()
    ));
    fs::create_dir_all(data_root)?;
    fs::write(&temporary, serde_json::to_vec_pretty(&state)?)?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交更新状态文件");
    }
    Ok(result)
}

/// 自动检查开始时记录本次尝试时间（无论随后成败）。
pub fn record_automatic_check(data_root: &Path) {
    let _ = mutate_update_state(data_root, |state| {
        state.last_automatic_check_at_utc = Some(Utc::now().to_rfc3339());
    });
}

pub fn automatic_check_due(last_check_utc: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last_check_utc {
        None => true,
        Some(last) => {
            now.signed_duration_since(last)
                >= chrono::Duration::hours(AUTOMATIC_CHECK_INTERVAL_HOURS)
        }
    }
}

pub fn automatic_check_due_from_state(data_root: &Path, now: DateTime<Utc>) -> bool {
    let last = load_update_state(data_root)
        .last_automatic_check_at_utc
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    automatic_check_due(last, now)
}

/// 该版本是否已被用户「稍后」隐藏；更高版本出现时应重新展示。
pub fn banner_dismissed_for(data_root: &Path, version: &str) -> bool {
    load_update_state(data_root)
        .banner_dismissed_version
        .as_deref()
        == Some(version)
}

pub fn dismiss_banner_for(data_root: &Path, version: &str) {
    let _ = mutate_update_state(data_root, |state| {
        state.banner_dismissed_version = Some(version.to_owned());
    });
}

/// 只读兜底：清单不可用时解析 `releases/latest` 的 302 `Location` 取 tag。
/// 结果**只用于界面提示**，永远不参与下载或安装判断。
pub fn probe_latest_release_tag() -> Result<String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .user_agent(format!(
            "StockIpoReminder/{}/Windows",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("无法创建发布页探测客户端")?;
    let response = client
        .get(release_page_url()?)
        .send()
        .context("无法访问发布页")?;
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .context("发布页没有返回跳转地址")?;
    if !location.contains("/releases/tag/") {
        bail!("发布页跳转地址不是版本标签");
    }
    let tag = location
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if tag.is_empty() {
        bail!("无法解析发布版本标签");
    }
    Ok(tag)
}

/// 把 `v0.4.0` 之类的 tag 转成高于当前版本的版本号；不高于当前版本返回 `None`。
pub fn higher_release_version(tag: &str) -> Option<String> {
    let version = tag.trim().trim_start_matches('v');
    let ordering = compare_versions(version, env!("CARGO_PKG_VERSION")).ok()?;
    (ordering == std::cmp::Ordering::Greater).then(|| version.to_owned())
}

/// 固定发布页地址（托盘菜单与手动下载入口共用）。
pub fn release_page_url() -> Result<Url> {
    let feed = validated_https_url(UPDATE_FEED_URL, "更新清单")?;
    let mut segments = feed
        .path_segments()
        .context("更新源地址缺少仓库信息")?
        .filter(|segment| !segment.is_empty());
    let owner = segments.next().context("更新源地址缺少仓库所有者")?;
    let repository = segments.next().context("更新源地址缺少仓库名称")?;
    Url::parse(&format!(
        "https://github.com/{owner}/{repository}/releases/latest"
    ))
    .context("发布页地址无效")
}

#[cfg(windows)]
fn current_windows_build() -> Result<u32> {
    let mut version = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    let status = unsafe { RtlGetVersion(&mut version) };
    if !status.is_ok() {
        bail!(
            "无法读取 Windows 版本（NTSTATUS 0x{:08X}）",
            status.0 as u32
        );
    }
    Ok(version.dwBuildNumber)
}

#[cfg(not(windows))]
fn current_windows_build() -> Result<u32> {
    Ok(u32::MAX)
}

#[cfg(windows)]
fn wait_for_parent_exit(parent_pid: u32) -> Result<()> {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }
        .context("无法打开主程序进程，已取消更新安装")?;
    let wait = unsafe { WaitForSingleObject(handle, 30_000) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if wait != WAIT_OBJECT_0 {
        bail!("主程序未在 30 秒内退出，已取消更新安装");
    }
    Ok(())
}

#[cfg(not(windows))]
fn wait_for_parent_exit(_parent_pid: u32) -> Result<()> {
    bail!("当前平台不支持 MSI 自动更新")
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PUBLIC_KEY: &str = include_str!("../tests/fixtures/minisign/test-only.pub");
    const TEST_MANIFEST: &[u8] = include_bytes!("../tests/fixtures/minisign/update-manifest.json");
    const TEST_SIGNATURE: &[u8] =
        include_bytes!("../tests/fixtures/minisign/update-manifest.json.minisig");
    const TEST_LEGACY_SIGNATURE: &[u8] =
        include_bytes!("../tests/fixtures/minisign/update-manifest.json.legacy.minisig");

    fn test_public_key() -> minisign_verify::PublicKey {
        minisign_verify::PublicKey::decode(TEST_PUBLIC_KEY).expect("test public key must parse")
    }

    fn temp_data_root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "stock-ipo-updater-test-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).expect("create temp data root");
        root
    }

    #[test]
    fn validated_https_url_rejects_password_only_and_fragment() {
        assert!(
            validated_https_url(
                "https://:secret@updates.example.invalid/manifest.json",
                "更新清单"
            )
            .is_err()
        );
        assert!(
            validated_https_url(
                "https://updates.example.invalid/manifest.json#frag",
                "更新清单"
            )
            .is_err()
        );
        assert!(
            validated_https_url("https://updates.example.invalid/manifest.json", "更新清单")
                .is_ok()
        );
    }

    #[test]
    fn trusted_public_key_parses_embedded_root() {
        assert!(trusted_public_key().is_ok());
    }

    #[test]
    fn valid_minisign_signature_accepted() {
        let key = test_public_key();
        assert!(verify_minisign_signature(TEST_MANIFEST, TEST_SIGNATURE, &key).is_ok());
    }

    #[test]
    fn production_key_rejects_test_signature() {
        let key = trusted_public_key().expect("embedded public key");
        assert!(verify_minisign_signature(TEST_MANIFEST, TEST_SIGNATURE, &key).is_err());
    }

    #[test]
    fn tampered_manifest_rejected() {
        let key = test_public_key();
        let mut tampered = TEST_MANIFEST.to_vec();
        let last = tampered.len() - 1;
        tampered[last] = tampered[last].wrapping_add(1);
        assert!(verify_minisign_signature(&tampered, TEST_SIGNATURE, &key).is_err());
    }

    #[test]
    fn tampered_signature_rejected() {
        let key = test_public_key();
        let mut tampered = TEST_SIGNATURE.to_vec();
        let last = tampered.len() - 1;
        tampered[last] = tampered[last].wrapping_add(1);
        assert!(verify_minisign_signature(TEST_MANIFEST, &tampered, &key).is_err());
    }

    #[test]
    fn legacy_signature_rejected() {
        let key = test_public_key();
        assert!(verify_minisign_signature(TEST_MANIFEST, TEST_LEGACY_SIGNATURE, &key).is_err());
    }

    fn manifest() -> UpdateManifest {
        serde_json::from_slice(TEST_MANIFEST).expect("fixture manifest must parse")
    }

    #[test]
    fn versions_are_strict_and_numeric() {
        assert_eq!(
            compare_versions("0.2.8", "0.2.7").unwrap(),
            std::cmp::Ordering::Greater
        );
        assert!(parse_version("0.2").is_err());
        assert!(parse_version("0.2.8-beta").is_err());
    }

    #[test]
    fn update_feed_is_fixed_github_stable_https() {
        let url = validated_https_url(UPDATE_FEED_URL, "更新清单").expect("feed must be valid");
        assert_eq!(
            url.as_str(),
            "https://github.com/melody0709/StockIpoReminder/releases/latest/download/update-manifest.json"
        );
    }

    #[test]
    fn manifest_fixture_passes_validation() {
        let value = manifest();
        let base = Url::parse("https://updates.example.invalid/update-manifest.json").unwrap();
        assert!(validate_manifest(&value, &base).is_ok());
        let insecure = Url::parse("http://updates.example.invalid/update-manifest.json").unwrap();
        assert!(validate_manifest(&value, &insecure).is_err());
    }

    #[test]
    fn manifest_rejects_wrong_schema_product_channel_and_fields() {
        let base = Url::parse("https://updates.example.invalid/update-manifest.json").unwrap();
        let mut value = manifest();
        value.schema_version = 1;
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.product = "Other".into();
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.channel = "beta".into();
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.published_at_utc = "2026-09-09 00:00:00".into();
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.installer.url = "StockIpoReminder-0.0.1-win-x64.msi".into();
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.installer.url = "../escape/StockIpoReminder-9.9.9-win-x64.msi".into();
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.installer.sha256 = "zz".repeat(32);
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.installer.size_bytes = 0;
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.installer.size_bytes = INSTALLER_LIMIT + 1;
        assert!(validate_manifest(&value, &base).is_err());
        let mut value = manifest();
        value.minimum_windows_build = u32::MAX;
        assert!(validate_manifest(&value, &base).is_err());
    }

    #[test]
    fn pending_verification_requires_complete_and_matching_installer() {
        let root = temp_data_root();
        let pending = pending_directory(&root);
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join(PENDING_MANIFEST_FILE), TEST_MANIFEST).unwrap();
        fs::write(pending.join(PENDING_SIGNATURE_FILE), TEST_SIGNATURE).unwrap();
        // 清单版本 9.9.9 高于当前版本；缺少安装包时必须失败。
        assert!(verify_pending(&root).is_err());
        let value = manifest();
        let msi = pending.join(&value.installer.url);
        // 大小与清单不一致：必须失败。
        fs::write(&msi, vec![0u8; 16]).unwrap();
        assert!(verify_pending(&root).is_err());
        // 大小一致但 SHA-256 与清单不一致：必须失败。
        fs::write(&msi, vec![0u8; value.installer.size_bytes as usize]).unwrap();
        assert!(verify_pending(&root).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pending_directory_is_scoped_under_updates() {
        let root = PathBuf::from(r"C:\Data\StockIpoReminder");
        assert_eq!(
            pending_directory(&root),
            root.join("updates").join("pending")
        );
        assert_eq!(
            pending_installer_file_name("0.3.8"),
            "StockIpoReminder-0.3.8-win-x64.msi"
        );
    }

    #[test]
    fn each_update_operation_uses_distinct_partial_path() {
        let directory = PathBuf::from(r"C:\Data\temp\updates");
        let first = partial_download_path(
            &directory,
            "0.3.8",
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        );
        let second = partial_download_path(
            &directory,
            "0.3.8",
            Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        );
        assert_ne!(first, second);
        assert_eq!(
            first.extension().and_then(|value| value.to_str()),
            Some("part")
        );
    }

    #[test]
    fn update_state_roundtrip_and_mutation() {
        let root = temp_data_root();
        assert_eq!(load_update_state(&root).pending_version, None);
        mutate_update_state(&root, |state| {
            state.pending_version = Some("0.3.8".into());
            state.banner_dismissed_version = Some("0.3.7".into());
            state.last_automatic_check_at_utc = Some("2026-09-09T00:00:00Z".into());
        })
        .unwrap();
        let state = load_update_state(&root);
        assert_eq!(state.schema_version, 1);
        assert_eq!(state.pending_version.as_deref(), Some("0.3.8"));
        assert_eq!(state.banner_dismissed_version.as_deref(), Some("0.3.7"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn automatic_check_throttles_to_the_configured_interval() {
        let now = Utc::now();
        assert!(automatic_check_due(None, now));
        assert!(!automatic_check_due(
            Some(now - chrono::Duration::hours(5)),
            now
        ));
        assert!(automatic_check_due(
            Some(now - chrono::Duration::hours(6)),
            now
        ));
        assert!(automatic_check_due(
            Some(now - chrono::Duration::hours(72)),
            now
        ));
    }

    #[test]
    fn banner_dismissal_only_hides_the_dismissed_version() {
        let root = temp_data_root();
        assert!(!banner_dismissed_for(&root, "0.4.0"));
        dismiss_banner_for(&root, "0.4.0");
        assert!(banner_dismissed_for(&root, "0.4.0"));
        // 更高版本必须重新展示。
        assert!(!banner_dismissed_for(&root, "0.4.1"));
        // 隐藏状态必须持久化，重启后仍然生效。
        assert_eq!(
            load_update_state(&root).banner_dismissed_version.as_deref(),
            Some("0.4.0")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn release_page_url_is_derived_from_the_fixed_feed() {
        assert_eq!(
            release_page_url().expect("release page").as_str(),
            "https://github.com/melody0709/StockIpoReminder/releases/latest"
        );
    }

    #[test]
    fn higher_release_version_only_accepts_newer_tags() {
        assert_eq!(higher_release_version("v9.9.9").as_deref(), Some("9.9.9"));
        assert_eq!(higher_release_version("9.9.9").as_deref(), Some("9.9.9"));
        assert!(higher_release_version("v0.0.1").is_none());
        assert!(higher_release_version(env!("CARGO_PKG_VERSION")).is_none());
        assert!(higher_release_version("not-a-version").is_none());
    }

    #[test]
    fn upgrade_receipt_is_consumed_once_for_the_running_version() {
        let root = temp_data_root();
        let diagnostics = root.join("diagnostics");
        fs::create_dir_all(&diagnostics).unwrap();
        let path = diagnostics.join("update-last-result.json");

        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "success": true,
                "detail": "更新安装成功，已启动新版本",
                "version": env!("CARGO_PKG_VERSION"),
                "consumed": false,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            consume_upgrade_receipt(&root).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // 第二次启动不再重复提示。
        assert!(consume_upgrade_receipt(&root).is_none());

        // 成功但版本不匹配（例如回滚到旧版）不提示。
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "success": true,
                "detail": "更新安装成功，已启动新版本",
                "version": "9.9.9",
                "consumed": false,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(consume_upgrade_receipt(&root).is_none());

        // 失败回执不提示。
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "success": false,
                "error": "Windows Installer 更新失败：exit=1620",
                "consumed": false,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(consume_upgrade_receipt(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn update_activation_values_are_typed() {
        assert!(is_update_activation(ACTIVATION_UPDATE_AVAILABLE));
        assert!(is_update_activation(ACTIVATION_UPDATE_READY));
        assert!(!is_update_activation("event:abc"));
        assert!(!is_update_activation(""));
        assert!(!is_update_activation("update:"));
    }

    #[test]
    fn release_notes_url_only_accepts_relative_release_asset() {
        let mut value = manifest();
        value.release_notes_url = Some("RELEASE_NOTES.md".into());
        assert_eq!(
            release_notes_url(&value).as_deref(),
            Some(
                "https://github.com/melody0709/StockIpoReminder/releases/latest/download/RELEASE_NOTES.md"
            )
        );
        let mut value = manifest();
        value.release_notes_url = Some("https://evil.example.invalid/notes.md".into());
        assert!(release_notes_url(&value).is_none());
        let mut value = manifest();
        value.release_notes_url = Some("../escape.md".into());
        assert!(release_notes_url(&value).is_none());
        let mut value = manifest();
        value.release_notes_url = Some("".into());
        assert!(release_notes_url(&value).is_none());
        let mut value = manifest();
        value.release_notes_url = None;
        assert!(release_notes_url(&value).is_none());
    }

    #[test]
    fn update_state_tolerates_missing_optional_fields_but_requires_schema() {
        // Option 字段缺失时 serde 回填 None（未来新增可选字段不破坏旧文件）。
        let partial: UpdateState = serde_json::from_str(r#"{"schemaVersion":1}"#).unwrap();
        assert_eq!(partial.schema_version, 1);
        assert_eq!(partial.last_automatic_check_at_utc, None);
        assert_eq!(partial.pending_version, None);
        assert_eq!(partial.banner_dismissed_version, None);
        // schemaVersion 缺失视为损坏文件，解析必须失败。
        assert!(serde_json::from_str::<UpdateState>(r#"{"pendingVersion":"0.3.8"}"#).is_err());
        // 未知 schema 版本可被 serde 解析，但 load_update_state 必须回退默认值。
        let root = temp_data_root();
        fs::write(
            root.join(UPDATE_STATE_FILE),
            r#"{"schemaVersion":2,"pendingVersion":"9.9.9"}"#,
        )
        .unwrap();
        let state = load_update_state(&root);
        assert_eq!(state.schema_version, 1);
        assert_eq!(state.pending_version, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_pending_files_including_tmp_residue_are_removed() {
        let root = temp_data_root();
        let pending = pending_directory(&root);
        fs::create_dir_all(&pending).unwrap();
        let keep = pending_installer_file_name("9.9.9");
        fs::write(pending.join(&keep), b"keep").unwrap();
        fs::write(pending.join(PENDING_MANIFEST_FILE), b"manifest").unwrap();
        fs::write(pending.join(PENDING_SIGNATURE_FILE), b"signature").unwrap();
        let stale_msi = pending.join(pending_installer_file_name("9.8.7"));
        fs::write(&stale_msi, b"stale").unwrap();
        let stale_tmp = pending.join(".update-manifest.json.abc123.tmp");
        fs::write(&stale_tmp, b"residue").unwrap();
        remove_stale_pending_files(&pending, &keep).unwrap();
        assert!(pending.join(&keep).is_file());
        assert!(pending.join(PENDING_MANIFEST_FILE).is_file());
        assert!(pending.join(PENDING_SIGNATURE_FILE).is_file());
        assert!(!stale_msi.exists());
        assert!(!stale_tmp.exists());
        let _ = fs::remove_dir_all(&root);
    }
}

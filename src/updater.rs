use std::{
    env, fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
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
        Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0},
        Security::{
            Cryptography::{
                CERT_CONTEXT, CERT_SHA256_HASH_PROP_ID, CRYPT_VERIFY_MESSAGE_PARA,
                CertFreeCertificateContext, CertGetCertificateContextProperty,
                CryptVerifyDetachedMessageSignature, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
            },
            WinTrust::{
                WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
                WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_NONE,
                WTD_SAFER_FLAG, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
                WTD_UICONTEXT_INSTALL, WTHelperGetProvCertFromChain,
                WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
            },
        },
        System::{
            SystemInformation::OSVERSIONINFOW,
            Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS, WaitForSingleObject},
        },
    },
    core::{PCWSTR, PWSTR},
};

const MANIFEST_LIMIT: u64 = 256 * 1024;
const SIGNATURE_LIMIT: u64 = 256 * 1024;
const INSTALLER_LIMIT: u64 = 200 * 1024 * 1024;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const PROCESS_SYNCHRONIZE: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0010_0000);

pub const UPDATE_FEED_URL: &str = match option_env!("STOCK_IPO_UPDATE_FEED_URL") {
    Some(value) => value,
    None => "",
};
pub const TRUSTED_UPDATE_SIGNER_SHA256: &str = match option_env!("STOCK_IPO_UPDATE_SIGNER_SHA256") {
    Some(value) => value,
    None => "",
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInstaller {
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub signer_sha256: String,
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
    installer_url: Url,
}

#[derive(Debug, Clone)]
pub enum UpdateCheck {
    UpToDate,
    Available(AvailableUpdate),
}

pub fn configured() -> bool {
    !UPDATE_FEED_URL.trim().is_empty() && normalize_sha256(TRUSTED_UPDATE_SIGNER_SHA256).is_ok()
}

pub fn configuration_status() -> String {
    if configured() {
        "安全自动更新已配置：启动时检查签名清单，安装前复核哈希与 Authenticode。".into()
    } else {
        "当前构建未嵌入可信更新源和签名证书指纹；自动更新保持关闭。".into()
    }
}

pub fn check_for_update() -> Result<UpdateCheck> {
    if !configured() {
        bail!("当前构建未配置可信更新源或签名证书指纹");
    }
    let manifest_url = validated_https_url(UPDATE_FEED_URL, "更新清单")?;
    let signature_url = detached_signature_url(&manifest_url)?;
    let client = update_client()?;
    let manifest_bytes = fetch_limited(&client, &manifest_url, MANIFEST_LIMIT, "更新清单")?;
    let signature = fetch_limited(&client, &signature_url, SIGNATURE_LIMIT, "更新清单签名")?;
    verify_detached_signature(&manifest_bytes, &signature, TRUSTED_UPDATE_SIGNER_SHA256)?;
    let manifest: UpdateManifest =
        serde_json::from_slice(&manifest_bytes).context("无法解析签名更新清单")?;
    let installer_url = validate_manifest(&manifest, &manifest_url, TRUSTED_UPDATE_SIGNER_SHA256)?;
    if compare_versions(&manifest.version, env!("CARGO_PKG_VERSION"))?
        != std::cmp::Ordering::Greater
    {
        return Ok(UpdateCheck::UpToDate);
    }
    Ok(UpdateCheck::Available(AvailableUpdate {
        manifest,
        installer_url,
    }))
}

/// 下载/验签过程的清理守卫：helper 成功启动前任何失败都尽力删除已生成的
/// 更新文件（.part 与改名后的 .msi），避免失败残留长期占用磁盘。
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

pub fn download_and_request_install(data_root: &Path, update: &AvailableUpdate) -> Result<String> {
    if crate::deployment::installed_msi_product_code()?.is_none() {
        bail!("自动更新只支持由 Windows Installer 管理的安装版");
    }
    let client = update_client()?;
    let directory = data_root.join("temp").join("updates");
    fs::create_dir_all(&directory).context("无法创建更新下载目录")?;
    let (partial, installer) =
        update_download_paths(&directory, &update.manifest.version, Uuid::new_v4());
    let mut guard = PendingUpdateFile::new(partial.clone());
    let result = (|| -> Result<()> {
        download_installer(&client, update, &partial)?;
        fs::rename(&partial, &installer).context("无法提交已验证的更新安装包")?;
        guard.track(installer.clone());
        verify_authenticode(&installer, TRUSTED_UPDATE_SIGNER_SHA256)?;
        dispatch_install_helper(data_root, &installer, &update.manifest.installer.sha256)?;
        Ok(())
    })();
    if result.is_ok() {
        // helper 成功启动后由安装流程接管安装包；失败路径由守卫清理。
        guard.disarm();
    }
    result.map(|()| {
        format!(
            "{} 已下载并通过签名校验；程序退出后将启动 Windows Installer",
            update.manifest.version
        )
    })
}

fn update_download_paths(
    directory: &Path,
    version: &str,
    operation_id: Uuid,
) -> (PathBuf, PathBuf) {
    let operation_id = operation_id.simple();
    (
        directory.join(format!(
            ".StockIpoReminder-{version}-win-x64-{operation_id}.msi.part"
        )),
        directory.join(format!(
            "StockIpoReminder-{version}-win-x64-{operation_id}.msi"
        )),
    )
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
    let result = (|| -> Result<String> {
        let data_root = argument_path(arguments, "--data-root").context("缺少数据目录")?;
        let installer = argument_path(arguments, "--installer").context("缺少更新安装包")?;
        let expected_sha256 =
            argument_value(arguments, "--sha256").context("缺少更新安装包哈希")?;
        let parent_pid = argument_value(arguments, "--parent-pid")
            .context("缺少父进程编号")?
            .parse::<u32>()
            .context("父进程编号无效")?;
        run_install_helper(&data_root, &installer, &expected_sha256, parent_pid)
    })();
    if let Some(data_root) = argument_path(arguments, "--data-root") {
        let _ = write_update_result(&data_root, &result);
    }
    Ok(Some(if result.is_ok() { 0 } else { 2 }))
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

fn detached_signature_url(manifest_url: &Url) -> Result<Url> {
    let mut signature = manifest_url.clone();
    signature.set_fragment(None);
    signature.set_path(&format!("{}.p7s", manifest_url.path()));
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

fn validate_manifest(manifest: &UpdateManifest, base_url: &Url, signer: &str) -> Result<Url> {
    if manifest.schema_version != 1
        || manifest.product != "StockIpoReminder"
        || manifest.channel != "stable"
    {
        bail!("更新清单产品、通道或 schema 不受支持");
    }
    parse_version(&manifest.version)?;
    let expected_signer = normalize_sha256(signer)?;
    if normalize_sha256(&manifest.installer.signer_sha256)? != expected_signer {
        bail!("更新清单中的安装包签名证书与当前应用固定证书不一致");
    }
    normalize_sha256(&manifest.installer.sha256)?;
    if manifest.installer.size_bytes == 0 || manifest.installer.size_bytes > INSTALLER_LIMIT {
        bail!("更新安装包大小超出允许范围");
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

fn download_installer(client: &Client, update: &AvailableUpdate, target: &Path) -> Result<()> {
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

fn dispatch_install_helper(data_root: &Path, installer: &Path, sha256: &str) -> Result<()> {
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
        "--installer",
        installer.to_string_lossy().as_ref(),
        "--sha256",
        sha256,
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

fn run_install_helper(
    data_root: &Path,
    installer: &Path,
    expected_sha256: &str,
    parent_pid: u32,
) -> Result<String> {
    if !configured() {
        bail!("当前构建未固定可信更新签名证书");
    }
    validate_installer_path(data_root, installer)?;
    let actual = sha256_file(installer)?;
    if actual != normalize_sha256(expected_sha256)? {
        bail!("安装助手复核更新安装包 SHA-256 失败");
    }
    verify_authenticode(installer, TRUSTED_UPDATE_SIGNER_SHA256)?;
    wait_for_parent_exit(parent_pid)?;
    thread::sleep(Duration::from_millis(1200));
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
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let status = command
        .status()
        .context("无法启动 Windows Installer 更新")?;
    let exit_code = status.code().unwrap_or(-1);
    if exit_code != 0 && exit_code != 3010 {
        bail!("Windows Installer 更新失败：exit={exit_code}");
    }
    let _ = fs::remove_file(installer);
    Ok(if exit_code == 3010 {
        "更新安装成功，需要重新启动 Windows 后完成".into()
    } else {
        "更新安装成功，请重新启动应用".into()
    })
}

fn validate_installer_path(data_root: &Path, installer: &Path) -> Result<()> {
    if installer
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("更新安装包路径包含非法上级目录跳转");
    }
    let expected = data_root.join("temp").join("updates");
    let full = absolute(installer)?;
    let root = absolute(&expected)?;
    if !full.starts_with(&root)
        || !full
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("msi"))
    {
        bail!("更新安装包不在当前数据目录的受控更新目录中");
    }
    Ok(())
}

fn write_update_result(data_root: &Path, result: &Result<String>) -> Result<()> {
    let directory = data_root.join("diagnostics");
    fs::create_dir_all(&directory)?;
    let value = match result {
        Ok(detail) => json!({"success": true, "detail": detail}),
        Err(error) => json!({"success": false, "error": format!("{error:#}")}),
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
    let signer = argument_value(arguments, "--signer").context("缺少签名证书指纹")?;
    let report_path = argument_path(arguments, "--report").context("缺少自测试报告")?;
    let allow_untrusted_test_root = arguments
        .iter()
        .any(|value| value == "--allow-untrusted-test-root");
    let result = (|| -> Result<UpdateManifest> {
        let manifest_bytes = fs::read(&manifest_path)?;
        let signature = fs::read(&signature_path)?;
        verify_detached_signature(&manifest_bytes, &signature, &signer)?;
        let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)?;
        let base = Url::parse("https://updates.example.invalid/update-manifest.json")?;
        validate_manifest(&manifest, &base, &signer)?;
        if sha256_file(&installer_path)? != normalize_sha256(&manifest.installer.sha256)? {
            bail!("安装包 SHA-256 与清单不一致");
        }
        verify_authenticode_with_policy(&installer_path, &signer, allow_untrusted_test_root)?;
        Ok(manifest)
    })();
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let report = match &result {
        Ok(manifest) => {
            json!({"success": true, "version": manifest.version, "signerSha256": normalize_sha256(&signer)?})
        }
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

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(env::current_dir()?.join(path))
    }
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

#[cfg(windows)]
fn verify_detached_signature(content: &[u8], signature: &[u8], expected: &str) -> Result<()> {
    let mut parameters = CRYPT_VERIFY_MESSAGE_PARA {
        cbSize: std::mem::size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32,
        dwMsgAndCertEncodingType: (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING).0,
        ..Default::default()
    };
    let content_pointers = [content.as_ptr()];
    let content_lengths = [u32::try_from(content.len()).context("更新清单过大")?];
    let mut signer_certificate: *mut CERT_CONTEXT = std::ptr::null_mut();
    unsafe {
        CryptVerifyDetachedMessageSignature(
            &mut parameters,
            0,
            signature,
            1,
            content_pointers.as_ptr(),
            content_lengths.as_ptr(),
            Some(&mut signer_certificate),
        )
    }
    .context("更新清单 CMS/PKCS#7 签名无效")?;
    if signer_certificate.is_null() {
        bail!("更新清单签名没有返回签名证书");
    }
    let actual = certificate_sha256(signer_certificate);
    unsafe {
        let _ = CertFreeCertificateContext(Some(signer_certificate));
    }
    if actual? != normalize_sha256(expected)? {
        bail!("更新清单签名证书与应用固定证书不一致");
    }
    Ok(())
}

#[cfg(not(windows))]
fn verify_detached_signature(_content: &[u8], _signature: &[u8], _expected: &str) -> Result<()> {
    bail!("当前平台不支持 Windows 更新签名验证")
}

#[cfg(windows)]
fn certificate_sha256(certificate: *const CERT_CONTEXT) -> Result<String> {
    let mut length = 0u32;
    unsafe {
        CertGetCertificateContextProperty(certificate, CERT_SHA256_HASH_PROP_ID, None, &mut length)
    }
    .context("无法读取签名证书 SHA-256 指纹长度")?;
    let mut bytes = vec![0u8; length as usize];
    unsafe {
        CertGetCertificateContextProperty(
            certificate,
            CERT_SHA256_HASH_PROP_ID,
            Some(bytes.as_mut_ptr().cast()),
            &mut length,
        )
    }
    .context("无法读取签名证书 SHA-256 指纹")?;
    bytes.truncate(length as usize);
    Ok(hex::encode(bytes))
}

#[cfg(windows)]
fn verify_authenticode(path: &Path, expected_signer: &str) -> Result<()> {
    verify_authenticode_with_policy(path, expected_signer, false)
}

#[cfg(windows)]
fn verify_authenticode_with_policy(
    path: &Path,
    expected_signer: &str,
    allow_untrusted_test_root: bool,
) -> Result<()> {
    let wide = wide_null(path.to_string_lossy().as_ref());
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide.as_ptr()),
        hFile: HANDLE::default(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        pwszURLReference: PWSTR::null(),
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_SAFER_FLAG,
        dwUIContext: WTD_UICONTEXT_INSTALL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    const CERT_E_UNTRUSTEDROOT: u32 = 0x800B_0109;
    let accepted_test_root = allow_untrusted_test_root && status as u32 == CERT_E_UNTRUSTEDROOT;
    let signer = if status == 0 || accepted_test_root {
        let provider = unsafe { WTHelperProvDataFromStateData(data.hWVTStateData) };
        if provider.is_null() {
            Err(anyhow::anyhow!("无法读取 Authenticode 提供者数据"))
        } else {
            let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
            if signer.is_null() {
                Err(anyhow::anyhow!("无法读取 Authenticode 签名者"))
            } else {
                let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0) };
                if certificate.is_null() || unsafe { (*certificate).pCert }.is_null() {
                    Err(anyhow::anyhow!("无法读取 Authenticode 签名证书"))
                } else {
                    certificate_sha256(unsafe { (*certificate).pCert })
                }
            }
        }
    } else {
        Err(anyhow::anyhow!(
            "Windows Authenticode 验证失败：status=0x{:08x}",
            status as u32
        ))
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
    }
    if signer? != normalize_sha256(expected_signer)? {
        bail!("安装包 Authenticode 证书与签名更新清单不一致");
    }
    Ok(())
}

#[cfg(not(windows))]
fn verify_authenticode(_path: &Path, _expected_signer: &str) -> Result<()> {
    bail!("当前平台不支持 Authenticode 验证")
}

#[cfg(not(windows))]
fn verify_authenticode_with_policy(
    _path: &Path,
    _expected_signer: &str,
    _allow_untrusted_test_root: bool,
) -> Result<()> {
    bail!("当前平台不支持 Authenticode 验证")
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

    fn manifest() -> UpdateManifest {
        UpdateManifest {
            schema_version: 1,
            product: "StockIpoReminder".into(),
            channel: "stable".into(),
            version: "9.8.7".into(),
            published_at_utc: "2026-08-26T00:00:00Z".into(),
            minimum_windows_build: 19041,
            release_notes_url: Some("RELEASE_NOTES.md".into()),
            installer: UpdateInstaller {
                url: "StockIpoReminder-9.8.7-win-x64.msi".into(),
                sha256: "11".repeat(32),
                size_bytes: 1024,
                signer_sha256: "22".repeat(32),
            },
        }
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
    fn manifest_rejects_signer_mismatch_and_insecure_url() {
        let value = manifest();
        let base = Url::parse("https://updates.example.invalid/update-manifest.json").unwrap();
        assert!(validate_manifest(&value, &base, &"22".repeat(32)).is_ok());
        assert!(validate_manifest(&value, &base, &"33".repeat(32)).is_err());
        let insecure = Url::parse("http://updates.example.invalid/update-manifest.json").unwrap();
        assert!(validate_manifest(&value, &insecure, &"22".repeat(32)).is_err());
    }

    #[test]
    fn installer_path_is_scoped_to_update_directory() {
        let root = PathBuf::from(r"C:\Users\example\AppData\Local\StockIpoReminder");
        assert!(
            validate_installer_path(
                &root,
                &root
                    .join("temp")
                    .join("updates")
                    .join("StockIpoReminder-0.2.8-win-x64.msi")
            )
            .is_ok()
        );
        assert!(
            validate_installer_path(&root, &root.join("temp").join("..").join("malicious.msi"))
                .is_err()
        );
    }

    #[test]
    fn each_update_operation_uses_distinct_partial_and_committed_paths() {
        let directory = PathBuf::from(r"C:\Data\temp\updates");
        let first = update_download_paths(
            &directory,
            "0.3.1",
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        );
        let second = update_download_paths(
            &directory,
            "0.3.1",
            Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
        );
        assert_ne!(first, second);
        assert_eq!(
            first.0.extension().and_then(|value| value.to_str()),
            Some("part")
        );
        assert_eq!(
            first.1.extension().and_then(|value| value.to_str()),
            Some("msi")
        );
    }
}

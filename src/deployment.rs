use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::json;
use uuid::Uuid;

use crate::{storage::Database, windows_integration};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, WAIT_OBJECT_0},
        System::{
            ApplicationInstallationAndServicing::{
                INSTALLSTATE_DEFAULT, MsiEnumRelatedProductsW, MsiQueryProductStateW,
            },
            Threading::{OpenProcess, PROCESS_ACCESS_RIGHTS, WaitForSingleObject},
        },
    },
    core::{PCWSTR, PWSTR},
};

const MSI_UPGRADE_CODE: &str = "{A007780A-97A8-483E-A532-F336649EC5BB}";
pub const DATA_PURGE_CONFIRMATION: &str = "删除当前用户数据";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const PROCESS_SYNCHRONIZE: PROCESS_ACCESS_RIGHTS = PROCESS_ACCESS_RIGHTS(0x0010_0000);

/// 部署模式：互斥，一次只允许一种操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeploymentMode {
    Install,
    Uninstall,
    LegacyUninstallHelper,
    MsiUninstallHelper,
}

impl DeploymentMode {
    fn label(self) -> &'static str {
        match self {
            DeploymentMode::Install => "install",
            DeploymentMode::Uninstall => "uninstall",
            DeploymentMode::LegacyUninstallHelper => "uninstall-helper",
            DeploymentMode::MsiUninstallHelper => "msi-uninstall-helper",
        }
    }
}

fn allowed_mode_arguments(
    mode: DeploymentMode,
) -> (&'static [&'static str], &'static [&'static str]) {
    match mode {
        DeploymentMode::Install => (
            &["--install", "--no-launch"],
            &["--data-root", "--install-root", "--report"],
        ),
        DeploymentMode::Uninstall => (
            &["--uninstall", "--purge-data"],
            &[
                "--data-root",
                "--install-root",
                "--report",
                "--purge-confirmation",
            ],
        ),
        DeploymentMode::LegacyUninstallHelper => (
            &["--uninstall-helper", "--purge-data"],
            &[
                "--data-root",
                "--install-root",
                "--report",
                "--purge-confirmation",
            ],
        ),
        DeploymentMode::MsiUninstallHelper => (
            &["--msi-uninstall-helper", "--purge-data"],
            &[
                "--data-root",
                "--report",
                "--purge-confirmation",
                "--product-code",
                "--parent-pid",
            ],
        ),
    }
}

fn validate_mode_arguments(arguments: &[String], mode: DeploymentMode) -> Result<()> {
    let (flags, value_arguments) = allowed_mode_arguments(mode);
    let mut seen = HashSet::new();
    let mut index = 0usize;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if flags.contains(&argument) {
            if !seen.insert(argument) {
                bail!("部署参数重复：{argument}");
            }
            index += 1;
            continue;
        }
        if value_arguments.contains(&argument) {
            if !seen.insert(argument) {
                bail!("部署参数重复：{argument}");
            }
            let value = arguments
                .get(index + 1)
                .filter(|value| !value.is_empty() && !value.starts_with("--"))
                .with_context(|| format!("部署参数缺少值：{argument}"))?;
            let _ = value;
            index += 2;
            continue;
        }
        bail!("部署参数不适用于当前模式（{}）：{argument}", mode.label());
    }
    if seen.contains("--purge-confirmation") && !seen.contains("--purge-data") {
        bail!("--purge-confirmation 只能与 --purge-data 同时使用");
    }
    Ok(())
}

fn deployment_mode(arguments: &[String], stem: &str) -> Result<Option<DeploymentMode>> {
    let stem = stem.to_ascii_lowercase();
    let implicit_install = stem.contains("setup");
    let implicit_uninstall = stem.contains("uninstaller");
    let install = implicit_install || arguments.iter().any(|value| value == "--install");
    let uninstall = implicit_uninstall || arguments.iter().any(|value| value == "--uninstall");
    let legacy_helper = arguments.iter().any(|value| value == "--uninstall-helper");
    let msi_helper = arguments
        .iter()
        .any(|value| value == "--msi-uninstall-helper");
    if !install && !uninstall && !legacy_helper && !msi_helper {
        return Ok(None);
    }
    let mut modes = Vec::new();
    if install {
        modes.push(DeploymentMode::Install);
    }
    if uninstall {
        modes.push(DeploymentMode::Uninstall);
    }
    if legacy_helper {
        modes.push(DeploymentMode::LegacyUninstallHelper);
    }
    if msi_helper {
        modes.push(DeploymentMode::MsiUninstallHelper);
    }
    if modes.len() > 1 {
        let labels = modes
            .iter()
            .map(|mode| mode.label())
            .collect::<Vec<_>>()
            .join("、");
        bail!("部署模式互斥，检测到多个模式同时请求：{labels}");
    }
    Ok(modes.into_iter().next())
}

pub fn try_handle(arguments: &[String]) -> Result<Option<i32>> {
    let executable = env::current_exe()?;
    let stem = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned();
    let Some(mode) = deployment_mode(arguments, &stem)? else {
        return Ok(None);
    };
    validate_mode_arguments(arguments, mode)?;

    let data_root = argument_path(arguments, "--data-root").unwrap_or_else(default_data_root);
    let install_root =
        argument_path(arguments, "--install-root").unwrap_or_else(default_install_root);
    let report_path = argument_path(arguments, "--report");
    let purge_data = arguments.iter().any(|value| value == "--purge-data");
    let purge_confirmation = argument_value(arguments, "--purge-confirmation").unwrap_or_default();
    let no_launch = arguments.iter().any(|value| value == "--no-launch");
    let result = match mode {
        DeploymentMode::Install => {
            install_application(&executable, &install_root, &data_root, no_launch)
        }
        DeploymentMode::MsiUninstallHelper => {
            let product_code = argument_value(arguments, "--product-code")
                .context("卸载助手缺少 MSI ProductCode")?;
            let parent_pid = argument_value(arguments, "--parent-pid")
                .context("卸载助手缺少父进程编号")?
                .parse::<u32>()
                .context("卸载助手父进程编号无效")?;
            run_msi_uninstall_helper(
                &product_code,
                parent_pid,
                &data_root,
                purge_data,
                &purge_confirmation,
            )
        }
        DeploymentMode::LegacyUninstallHelper => {
            thread::sleep(Duration::from_millis(1200));
            uninstall_application(&install_root, &data_root, purge_data, &purge_confirmation)
        }
        DeploymentMode::Uninstall => dispatch_uninstall_helper(
            &executable,
            &install_root,
            &data_root,
            purge_data,
            &purge_confirmation,
        ),
    };
    let action = if mode == DeploymentMode::Install {
        "install"
    } else {
        "uninstall"
    };
    let report = match &result {
        Ok(detail) => {
            json!({"success": true, "action": action, "detail": detail})
        }
        Err(error) => {
            json!({"success": false, "action": action, "error": format!("{error:#}")})
        }
    };
    if let Some(path) = report_path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    match result {
        Ok(_) => Ok(Some(0)),
        Err(error) => {
            crate::operations::log("ERROR", &format!("部署操作失败：{error:#}"));
            Ok(Some(2))
        }
    }
}

pub fn installed_msi_product_code() -> Result<Option<String>> {
    #[cfg(windows)]
    {
        let upgrade_code = wide_null(MSI_UPGRADE_CODE);
        let mut index = 0u32;
        loop {
            let mut buffer = [0u16; 39];
            let status = unsafe {
                MsiEnumRelatedProductsW(
                    PCWSTR(upgrade_code.as_ptr()),
                    None,
                    index,
                    PWSTR(buffer.as_mut_ptr()),
                )
            };
            if status == ERROR_NO_MORE_ITEMS.0 {
                return Ok(None);
            }
            if status != ERROR_SUCCESS.0 {
                bail!("无法查询当前 MSI 安装：error={status}");
            }
            let length = buffer
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(buffer.len());
            let product_code = String::from_utf16(&buffer[..length])
                .context("Windows Installer 返回了无效 ProductCode")?;
            if unsafe { MsiQueryProductStateW(PCWSTR(buffer.as_ptr())) } == INSTALLSTATE_DEFAULT {
                return Ok(Some(normalize_product_code(&product_code)?));
            }
            index = index.saturating_add(1);
        }
    }
    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

pub fn request_msi_uninstall(
    data_root: &Path,
    purge_data: bool,
    purge_confirmation: &str,
) -> Result<String> {
    let product_code = installed_msi_product_code()?
        .context("当前程序不是由已注册的 Stock IPO Reminder MSI 管理，无法从应用内启动卸载")?;
    if purge_data {
        validate_current_user_data_root(data_root)?;
        validate_purge_confirmation(purge_confirmation)?;
    }

    let current_executable = env::current_exe()?;
    let helper = env::temp_dir().join(format!(
        "StockIpoReminder-MsiUninstall-{}.exe",
        Uuid::new_v4().simple()
    ));
    fs::copy(&current_executable, &helper).context("无法创建当前用户卸载助手")?;
    let parent_pid = std::process::id().to_string();
    let mut command = Command::new(&helper);
    command.args([
        "--msi-uninstall-helper",
        "--product-code",
        &product_code,
        "--parent-pid",
        &parent_pid,
        "--data-root",
        data_root.to_string_lossy().as_ref(),
    ]);
    if purge_data {
        command.args(["--purge-data", "--purge-confirmation", purge_confirmation]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.spawn().context("无法启动当前用户卸载助手")?;
    windows_integration::delete_after_reboot(&helper);
    Ok(if purge_data {
        "卸载助手已启动；Windows Installer 成功卸载后会删除当前用户数据".into()
    } else {
        "卸载助手已启动；当前用户数据将继续保留".into()
    })
}

fn run_msi_uninstall_helper(
    product_code: &str,
    parent_pid: u32,
    data_root: &Path,
    purge_data: bool,
    purge_confirmation: &str,
) -> Result<String> {
    let product_code = normalize_product_code(product_code)?;
    let installed =
        installed_msi_product_code()?.context("没有找到可卸载的 Stock IPO Reminder MSI")?;
    if !installed.eq_ignore_ascii_case(&product_code) {
        bail!("拒绝卸载与当前产品 UpgradeCode 无关的 MSI");
    }
    if purge_data {
        validate_current_user_data_root(data_root)?;
        validate_purge_confirmation(purge_confirmation)?;
    }

    wait_for_parent_exit(parent_pid)?;
    // The normal application is a child of the same-EXE watchdog. Give that
    // supervisor time to observe the clean exit and release the installed EXE
    // before Windows Installer removes the per-machine payload.
    thread::sleep(Duration::from_millis(1200));
    let msiexec = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("msiexec.exe");
    let mut command = Command::new(msiexec);
    command
        .args(["/x", &product_code, "/passive", "/norestart"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let status = command.status().context("无法启动 Windows Installer")?;
    let exit_code = status.code().unwrap_or(-1);
    if exit_code != 0 && exit_code != 3010 {
        bail!("Windows Installer 卸载失败：exit={exit_code}；用户数据未删除");
    }

    let helper_executable = env::current_exe()?;
    let _ = windows_integration::set_auto_start(false, &helper_executable, data_root);
    if purge_data && data_root.exists() {
        fs::remove_dir_all(data_root).context("MSI 已卸载，但无法删除当前用户数据目录")?;
    }
    Ok(if purge_data {
        "MSI 和当前用户数据已删除".into()
    } else {
        format!("MSI 已卸载；用户数据继续保留在 {}", data_root.display())
    })
}

fn wait_for_parent_exit(parent_pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }
            .context("无法打开主程序进程，已取消卸载")?;
        let wait = unsafe { WaitForSingleObject(handle, 30_000) };
        unsafe {
            let _ = CloseHandle(handle);
        }
        if wait != WAIT_OBJECT_0 {
            bail!("主程序未在 30 秒内退出，已取消卸载");
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = parent_pid;
        bail!("当前平台不支持 MSI 卸载")
    }
}

fn normalize_product_code(value: &str) -> Result<String> {
    let parsed = Uuid::parse_str(value.trim().trim_matches(['{', '}']))
        .context("MSI ProductCode 格式无效")?;
    Ok(format!("{{{}}}", parsed.hyphenated()).to_ascii_uppercase())
}

fn validate_purge_confirmation(value: &str) -> Result<()> {
    if value != DATA_PURGE_CONFIRMATION {
        bail!("删除用户数据必须准确输入确认短语：{DATA_PURGE_CONFIRMATION}");
    }
    Ok(())
}

fn install_application(
    source_executable: &Path,
    install_root: &Path,
    data_root: &Path,
    no_launch: bool,
) -> Result<String> {
    validate_install_root(install_root)?;
    let _instance = windows_integration::SingleInstance::acquire(data_root)
        .context("升级前请先从托盘安全退出正在运行的程序")?;
    fs::create_dir_all(data_root)?;
    let database = Database::new(data_root);
    if database.path().exists() {
        database.initialize()?;
        let backup = database.backup(&data_root.join("backups"))?;
        crate::operations::log(
            "INFO",
            &format!("安装/升级前已备份 SQLite：{}", backup.display()),
        );
    }

    let parent = install_root.parent().context("安装目录缺少父目录")?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".StockIpoReminder-install-{}",
        Uuid::new_v4().simple()
    ));
    let previous = parent.join(format!(
        ".StockIpoReminder-previous-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&staging)?;
    let installed_executable = staging.join("StockIpoReminder.exe");
    fs::copy(source_executable, &installed_executable)?;
    fs::copy(
        source_executable,
        staging.join("StockIpoReminder.Uninstaller.exe"),
    )?;
    copy_optional_neighbor(source_executable, &staging, "README.md")?;
    copy_optional_neighbor(source_executable, &staging, "RELEASE_NOTES.md")?;

    let had_previous = install_root.exists();
    if had_previous {
        fs::rename(install_root, &previous).context("无法替换现有安装目录；请确认程序已退出")?;
    }
    if let Err(error) = fs::rename(&staging, install_root) {
        if had_previous {
            let _ = fs::rename(&previous, install_root);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error).context("无法提交新版本，已尝试回滚旧版本");
    }
    if previous.exists() {
        let _ = fs::remove_dir_all(&previous);
    }
    let final_executable = install_root.join("StockIpoReminder.exe");
    windows_integration::set_auto_start(true, &final_executable, data_root)?;
    if !no_launch {
        Command::new(&final_executable)
            .args([
                "--background",
                "--data-root",
                data_root.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }
    Ok(format!(
        "已安装到 {}；用户数据保存在 {}",
        install_root.display(),
        data_root.display()
    ))
}

fn dispatch_uninstall_helper(
    current_executable: &Path,
    install_root: &Path,
    data_root: &Path,
    purge_data: bool,
    purge_confirmation: &str,
) -> Result<String> {
    validate_install_root(install_root)?;
    let current = current_executable
        .canonicalize()
        .unwrap_or_else(|_| current_executable.to_owned());
    let target = install_root
        .canonicalize()
        .unwrap_or_else(|_| install_root.to_owned());
    if !current.starts_with(&target) {
        return uninstall_application(install_root, data_root, purge_data, purge_confirmation);
    }
    let helper = env::temp_dir().join(format!(
        "StockIpoReminder-Uninstall-{}.exe",
        Uuid::new_v4().simple()
    ));
    fs::copy(current_executable, &helper)?;
    let mut command = Command::new(&helper);
    command.args([
        "--uninstall-helper",
        "--install-root",
        install_root.to_string_lossy().as_ref(),
        "--data-root",
        data_root.to_string_lossy().as_ref(),
    ]);
    if purge_data {
        command.args(["--purge-data", "--purge-confirmation", purge_confirmation]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    windows_integration::delete_after_reboot(&helper);
    Ok("卸载助手已启动".into())
}

fn uninstall_application(
    install_root: &Path,
    data_root: &Path,
    purge_data: bool,
    purge_confirmation: &str,
) -> Result<String> {
    validate_install_root(install_root)?;
    let executable = install_root.join("StockIpoReminder.exe");
    let _ = windows_integration::set_auto_start(false, &executable, data_root);
    if install_root.exists() {
        fs::remove_dir_all(install_root).context("无法删除安装目录")?;
    }
    if purge_data {
        validate_data_root(data_root)?;
        validate_purge_confirmation(purge_confirmation)?;
        if data_root.exists() {
            fs::remove_dir_all(data_root).context("无法删除用户数据目录")?;
        }
        Ok("程序和用户数据已删除".into())
    } else {
        Ok(format!(
            "程序已删除；用户数据继续保留在 {}",
            data_root.display()
        ))
    }
}

fn validate_install_root(path: &Path) -> Result<()> {
    let full = absolute(path)?;
    if full.parent().is_none()
        || !full.file_name().is_some_and(|value| {
            value
                .to_string_lossy()
                .eq_ignore_ascii_case("StockIpoReminder")
        })
    {
        bail!("安装目录必须以 StockIpoReminder 结尾");
    }
    if full.components().count() < 3 {
        bail!("拒绝使用过于宽泛的安装目录");
    }
    Ok(())
}

fn validate_data_root(path: &Path) -> Result<()> {
    validate_current_user_data_root(path)
}

fn validate_current_user_data_root(path: &Path) -> Result<()> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("无法确定当前 Windows 用户的 LocalAppData 目录")?;
    validate_current_user_data_root_for(path, &local_app_data)
}

fn validate_current_user_data_root_for(path: &Path, local_app_data: &Path) -> Result<()> {
    let full = absolute_without_parent_segments(path)?;
    let expected = absolute_without_parent_segments(&local_app_data.join("StockIpoReminder"))?;
    if !full
        .to_string_lossy()
        .eq_ignore_ascii_case(&expected.to_string_lossy())
    {
        bail!("只允许删除当前用户的默认数据目录：{}", expected.display());
    }
    Ok(())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn absolute_without_parent_segments(path: &Path) -> Result<PathBuf> {
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("拒绝包含上级目录跳转的数据路径");
    }
    absolute(path)
}

fn copy_optional_neighbor(executable: &Path, target: &Path, name: &str) -> Result<()> {
    let source = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name);
    if source.exists() {
        fs::copy(source, target.join(name))?;
    }
    Ok(())
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
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn default_data_root() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("StockIpoReminder")
}
fn default_install_root() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Programs")
        .join("StockIpoReminder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_modes_are_mutually_exclusive() {
        assert_eq!(
            deployment_mode(&[], "StockIpoReminder-Setup").unwrap(),
            Some(DeploymentMode::Install)
        );
        assert_eq!(
            deployment_mode(&[], "StockIpoReminder-Uninstaller").unwrap(),
            Some(DeploymentMode::Uninstall)
        );
        assert_eq!(
            deployment_mode(&["--install".to_owned()], "StockIpoReminder").unwrap(),
            Some(DeploymentMode::Install)
        );
        assert_eq!(
            deployment_mode(&["--msi-uninstall-helper".to_owned()], "x").unwrap(),
            Some(DeploymentMode::MsiUninstallHelper)
        );
        assert_eq!(
            deployment_mode(&["--unrelated".to_owned()], "StockIpoReminder").unwrap(),
            None
        );
        let mode_flags = [
            "--install",
            "--uninstall",
            "--uninstall-helper",
            "--msi-uninstall-helper",
        ];
        for first in 0..mode_flags.len() {
            for second in (first + 1)..mode_flags.len() {
                assert!(
                    deployment_mode(
                        &[mode_flags[first].to_owned(), mode_flags[second].to_owned(),],
                        "StockIpoReminder"
                    )
                    .is_err(),
                    "{} 与 {} 应互斥",
                    mode_flags[first],
                    mode_flags[second]
                );
            }
        }
        assert!(deployment_mode(&["--uninstall".to_owned()], "StockIpoReminder-Setup").is_err());
        assert!(
            deployment_mode(&["--install".to_owned()], "StockIpoReminder-Uninstaller").is_err()
        );
    }

    #[test]
    fn deployment_accepts_only_well_formed_arguments_for_the_selected_mode() {
        assert!(
            validate_mode_arguments(
                &["--install".to_owned(), "--no-launch".to_owned()],
                DeploymentMode::Install
            )
            .is_ok()
        );
        assert!(
            validate_mode_arguments(
                &[
                    "--uninstall".to_owned(),
                    "--data-root".to_owned(),
                    "C:\\d".to_owned(),
                    "--purge-data".to_owned(),
                    "--purge-confirmation".to_owned(),
                    DATA_PURGE_CONFIRMATION.to_owned(),
                ],
                DeploymentMode::Uninstall
            )
            .is_ok()
        );
        assert!(
            validate_mode_arguments(
                &[
                    "--msi-uninstall-helper".to_owned(),
                    "--product-code".to_owned(),
                    "{PRODUCT-CODE}".to_owned(),
                    "--parent-pid".to_owned(),
                    "123".to_owned(),
                ],
                DeploymentMode::MsiUninstallHelper
            )
            .is_ok()
        );
        assert!(
            validate_mode_arguments(&["--product-code".to_owned()], DeploymentMode::Install)
                .is_err()
        );
        assert!(
            validate_mode_arguments(&["--no-launch".to_owned()], DeploymentMode::Uninstall)
                .is_err()
        );
        assert!(
            validate_mode_arguments(
                &["--install".to_owned(), "--unknown".to_owned()],
                DeploymentMode::Install
            )
            .is_err()
        );
        assert!(
            validate_mode_arguments(
                &["--install".to_owned(), "--data-root".to_owned()],
                DeploymentMode::Install
            )
            .is_err()
        );
        assert!(
            validate_mode_arguments(
                &["--install".to_owned(), "--install".to_owned()],
                DeploymentMode::Install
            )
            .is_err()
        );
        assert!(
            validate_mode_arguments(
                &[
                    "--uninstall".to_owned(),
                    "--purge-confirmation".to_owned(),
                    DATA_PURGE_CONFIRMATION.to_owned(),
                ],
                DeploymentMode::Uninstall
            )
            .is_err()
        );
    }

    #[test]
    fn purge_requires_exact_confirmation_phrase() {
        assert!(validate_purge_confirmation(DATA_PURGE_CONFIRMATION).is_ok());
        assert!(validate_purge_confirmation("删除用户数据").is_err());
        assert!(validate_purge_confirmation(" 删除当前用户数据 ").is_err());
    }

    #[test]
    fn purge_only_accepts_current_users_default_data_root() {
        let local_app_data = PathBuf::from(r"C:\Users\example\AppData\Local");
        assert!(
            validate_current_user_data_root_for(
                &local_app_data.join("StockIpoReminder"),
                &local_app_data,
            )
            .is_ok()
        );
        assert!(
            validate_current_user_data_root_for(
                &local_app_data.join("OtherApp").join("StockIpoReminder"),
                &local_app_data,
            )
            .is_err()
        );
        assert!(
            validate_current_user_data_root_for(
                Path::new(r"C:\Users\other\AppData\Local\StockIpoReminder"),
                &local_app_data,
            )
            .is_err()
        );
        assert!(
            validate_current_user_data_root_for(
                &local_app_data
                    .join("temp")
                    .join("..")
                    .join("StockIpoReminder"),
                &local_app_data,
            )
            .is_err()
        );
    }

    #[test]
    fn product_code_is_strictly_normalized() {
        assert_eq!(
            normalize_product_code("ccc1062c-adb0-40b5-b0a7-48cb366f42d7").unwrap(),
            "{CCC1062C-ADB0-40B5-B0A7-48CB366F42D7}"
        );
        assert!(normalize_product_code("not-a-guid").is_err());
    }
}

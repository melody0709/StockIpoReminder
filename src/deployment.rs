use std::{
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

pub fn try_handle(arguments: &[String]) -> Result<Option<i32>> {
    let executable = env::current_exe()?;
    let stem = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let implicit_install = arguments.is_empty() && stem.contains("setup");
    let implicit_uninstall = arguments.is_empty() && stem.contains("uninstaller");
    let install = implicit_install || arguments.iter().any(|value| value == "--install");
    let uninstall = implicit_uninstall || arguments.iter().any(|value| value == "--uninstall");
    let helper = arguments.iter().any(|value| value == "--uninstall-helper");
    if !install && !uninstall && !helper {
        return Ok(None);
    }

    let data_root = argument_path(arguments, "--data-root").unwrap_or_else(default_data_root);
    let install_root =
        argument_path(arguments, "--install-root").unwrap_or_else(default_install_root);
    let report_path = argument_path(arguments, "--report");
    let purge_data = arguments.iter().any(|value| value == "--purge-data");
    let no_launch = arguments.iter().any(|value| value == "--no-launch");
    let result = if install {
        install_application(&executable, &install_root, &data_root, no_launch)
    } else if helper {
        thread::sleep(Duration::from_millis(1200));
        uninstall_application(&install_root, &data_root, purge_data)
    } else {
        dispatch_uninstall_helper(&executable, &install_root, &data_root, purge_data)
    };
    let report = match &result {
        Ok(detail) => {
            json!({"success": true, "action": if install {"install"} else {"uninstall"}, "detail": detail})
        }
        Err(error) => {
            json!({"success": false, "action": if install {"install"} else {"uninstall"}, "error": format!("{error:#}")})
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
) -> Result<String> {
    validate_install_root(install_root)?;
    let current = current_executable
        .canonicalize()
        .unwrap_or_else(|_| current_executable.to_owned());
    let target = install_root
        .canonicalize()
        .unwrap_or_else(|_| install_root.to_owned());
    if !current.starts_with(&target) {
        return uninstall_application(install_root, data_root, purge_data);
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
        command.arg("--purge-data");
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
) -> Result<String> {
    validate_install_root(install_root)?;
    let executable = install_root.join("StockIpoReminder.exe");
    let _ = windows_integration::set_auto_start(false, &executable, data_root);
    if install_root.exists() {
        fs::remove_dir_all(install_root).context("无法删除安装目录")?;
    }
    if purge_data {
        validate_data_root(data_root)?;
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
    let full = absolute(path)?;
    if !full.file_name().is_some_and(|value| {
        value
            .to_string_lossy()
            .eq_ignore_ascii_case("StockIpoReminder")
    }) || full.components().count() < 3
    {
        bail!("拒绝删除未经确认的数据目录");
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

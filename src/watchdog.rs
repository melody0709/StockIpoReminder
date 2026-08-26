use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::{core::now_china, operations};

const CRASH_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_RESTARTS_IN_WINDOW: usize = 3;
const RESTART_DELAYS: [Duration; MAX_RESTARTS_IN_WINDOW] = [
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

pub fn should_supervise(arguments: &[String]) -> bool {
    cfg!(windows)
        && !arguments
            .iter()
            .any(|value| matches!(value.as_str(), "--watchdog-child" | "--no-watchdog"))
}

pub fn supervise(arguments: &[String], data_root: &Path) -> Result<()> {
    #[cfg(not(windows))]
    {
        let _ = (arguments, data_root);
        return Ok(());
    }

    #[cfg(windows)]
    {
        let executable = std::env::current_exe().context("无法定位 Watchdog 子进程程序")?;
        let child_arguments = child_arguments(arguments);
        let mut crashes = VecDeque::<Instant>::new();

        loop {
            let status = run_child(&executable, &child_arguments)?;
            if status.success() {
                operations::log("INFO", "主程序正常退出，Watchdog 同步退出");
                return Ok(());
            }

            let now = Instant::now();
            while crashes
                .front()
                .is_some_and(|timestamp| now.duration_since(*timestamp) > CRASH_WINDOW)
            {
                crashes.pop_front();
            }
            crashes.push_back(now);

            let restart_index = crashes.len().saturating_sub(1);
            let restart_scheduled = crashes.len() <= MAX_RESTARTS_IN_WINDOW;
            let delay = restart_scheduled.then(|| RESTART_DELAYS[restart_index]);
            let report =
                write_crash_report(data_root, &status, crashes.len(), restart_scheduled, delay)?;
            operations::log(
                "ERROR",
                &format!(
                    "检测到主程序异常退出（exit={}），恢复报告：{}",
                    exit_code(&status),
                    report.display()
                ),
            );

            let Some(delay) = delay else {
                bail!(
                    "主程序在 10 分钟内异常退出超过 {MAX_RESTARTS_IN_WINDOW} 次，Watchdog 已停止重启"
                );
            };
            thread::sleep(delay);
        }
    }
}

fn child_arguments(arguments: &[String]) -> Vec<String> {
    let mut values: Vec<String> = arguments
        .iter()
        .filter(|value| !matches!(value.as_str(), "--watchdog-child" | "--no-watchdog"))
        .cloned()
        .collect();
    values.push("--watchdog-child".into());
    values
}

fn run_child(executable: &Path, arguments: &[String]) -> Result<ExitStatus> {
    Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Watchdog 无法启动或等待主程序")
}

fn write_crash_report(
    data_root: &Path,
    status: &ExitStatus,
    crashes_in_window: usize,
    restart_scheduled: bool,
    delay: Option<Duration>,
) -> Result<PathBuf> {
    let directory = data_root.join("diagnostics").join("crashes");
    fs::create_dir_all(&directory)?;
    let generated_at = now_china();
    let path = directory.join(format!(
        "crash-recovery-{}-{}.json",
        generated_at.format("%Y%m%d-%H%M%S-%3f"),
        crashes_in_window
    ));
    let report = json!({
        "schemaVersion": "1",
        "generatedAt": generated_at.to_rfc3339(),
        "applicationVersion": env!("CARGO_PKG_VERSION"),
        "exitCode": exit_code(status),
        "crashesInTenMinuteWindow": crashes_in_window,
        "maximumRestartsInWindow": MAX_RESTARTS_IN_WINDOW,
        "restartScheduled": restart_scheduled,
        "restartDelaySeconds": delay.map(|value| value.as_secs()),
        "note": "报告不包含命令行、数据目录或用户数据"
    });
    fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    Ok(path)
}

fn exit_code(status: &ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_arguments_strip_control_flags() {
        let arguments = vec![
            "--background".into(),
            "--no-watchdog".into(),
            "--watchdog-child".into(),
        ];
        assert_eq!(
            child_arguments(&arguments),
            vec!["--background".to_owned(), "--watchdog-child".to_owned()]
        );
    }
}

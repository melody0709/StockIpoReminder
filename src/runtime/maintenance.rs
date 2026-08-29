use super::*;

pub(crate) fn run_daily_maintenance(database: &Database, data_root: &Path) {
    let backup_directory = data_root.join("backups");
    let today = now_china().date_naive();
    let fingerprint = database.business_state_fingerprint();
    match fingerprint {
        Ok(Some(fingerprint)) if fs_needs_daily_backup(&backup_directory, today) => {
            match latest_managed_backup_fingerprint(&backup_directory) {
                Ok(Some(previous)) if previous == fingerprint => {}
                Ok(_) => match database.backup(&backup_directory) {
                    Ok(path) => {
                        if let Err(error) = write_backup_fingerprint(&path, &fingerprint) {
                            operations::log("WARN", &format!("SQLite 备份指纹写入失败：{error:#}"));
                        }
                        retain_latest_backups(
                            &backup_directory,
                            MANAGED_BACKUP_LIMIT,
                            MANAGED_BACKUP_MAX_BYTES,
                            Some(&path),
                        );
                        if let Err(error) = database.save_operation_health(
                            "database-backup",
                            HealthState::Healthy,
                            None,
                        ) {
                            operations::log(
                                "ERROR",
                                &format!("SQLite 备份健康状态写入失败：{error:#}"),
                            );
                        }
                        operations::log(
                            "INFO",
                            &format!("业务数据变化后的 SQLite 备份完成：{}", path.display()),
                        );
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        let _ = database.save_operation_health(
                            "database-backup",
                            HealthState::Failed,
                            Some(&message),
                        );
                        operations::log("ERROR", &format!("SQLite 备份失败：{message}"));
                    }
                },
                Err(error) => operations::log(
                    "WARN",
                    &format!("读取 SQLite 备份指纹失败，将跳过自动备份：{error:#}"),
                ),
            }
        }
        Ok(_) => {}
        Err(error) => operations::log(
            "WARN",
            &format!("计算 SQLite 业务数据指纹失败，将跳过自动备份：{error:#}"),
        ),
    }
    match database.maintenance(data_root) {
        Ok(changed) => {
            if let Err(error) =
                database.save_operation_health("database-maintenance", HealthState::Healthy, None)
            {
                operations::log("ERROR", &format!("数据库维护健康状态写入失败：{error:#}"));
            }
            if changed {
                operations::log("INFO", "本地数据库与临时文件保留策略已完成清理");
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = database.save_operation_health(
                "database-maintenance",
                HealthState::Failed,
                Some(&message),
            );
            operations::log("ERROR", &format!("本地数据维护失败：{message}"));
        }
    }
    match operations::maintain_logs(data_root) {
        Ok(()) => {
            if let Err(error) =
                database.save_operation_health("log-retention", HealthState::Healthy, None)
            {
                operations::log("ERROR", &format!("日志保留健康状态写入失败：{error:#}"));
            }
        }
        Err(error) => {
            let message = format!("{error:#}");
            let _ = database.save_operation_health(
                "log-retention",
                HealthState::Failed,
                Some(&message),
            );
            operations::log("ERROR", &format!("日志保留清理失败：{message}"));
        }
    }
}

pub(crate) fn fs_needs_daily_backup(directory: &Path, date: chrono::NaiveDate) -> bool {
    let prefix = format!("stock-ipo-reminder-{}", date.format("%Y%m%d"));
    std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
}

pub(crate) fn retain_latest_backups(
    directory: &Path,
    count: usize,
    maximum_bytes: u64,
    preserve: Option<&Path>,
) {
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_managed_daily_backup(path))
        .collect();
    paths.sort();
    let mut total_bytes = paths
        .iter()
        .filter_map(|path| path.metadata().ok().map(|metadata| metadata.len()))
        .sum::<u64>();
    while paths.len() > count || total_bytes > maximum_bytes {
        let Some(index) = paths
            .iter()
            .position(|path| preserve != Some(path.as_path()))
        else {
            break;
        };
        let path = paths.remove(index);
        let length = path
            .metadata()
            .ok()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            total_bytes = total_bytes.saturating_sub(length);
            let _ = std::fs::remove_file(backup_fingerprint_path(&path));
        }
    }
}

pub(crate) fn is_managed_daily_backup(path: &Path) -> bool {
    path.extension().is_some_and(|value| value == "db")
        && path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                value
                    .strip_prefix("stock-ipo-reminder-")
                    .is_some_and(|suffix| suffix.len() == 19 && suffix.as_bytes()[8] == b'-')
            })
        && backup_fingerprint_path(path).is_file()
}

pub(crate) fn backup_fingerprint_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.fingerprint", path.display()))
}

pub(crate) fn write_backup_fingerprint(path: &Path, fingerprint: &str) -> Result<()> {
    let target = backup_fingerprint_path(path);
    let temporary = PathBuf::from(format!("{}.tmp", target.display()));
    std::fs::write(&temporary, fingerprint.as_bytes())?;
    operations::atomic_replace_file(&temporary, &target)
}

pub(crate) fn latest_managed_backup_fingerprint(directory: &Path) -> Result<Option<String>> {
    let latest = std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_managed_daily_backup(path))
        .max();
    let Some(latest) = latest else {
        return Ok(None);
    };
    let fingerprint_path = backup_fingerprint_path(&latest);
    if !fingerprint_path.is_file() {
        return Ok(None);
    }
    Ok(Some(
        std::fs::read_to_string(fingerprint_path)?.trim().to_owned(),
    ))
}

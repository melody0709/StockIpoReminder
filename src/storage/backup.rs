use super::*;

impl Database {
    pub fn backup(&self, backup_dir: &Path) -> Result<PathBuf> {
        self.backup_with_commit_hook(backup_dir, |_| Ok(()))
    }

    pub(super) fn backup_with_commit_hook(
        &self,
        backup_dir: &Path,
        before_commit: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<PathBuf> {
        fs::create_dir_all(backup_dir)?;
        let timestamp = now_china();
        // 追加短随机后缀：毫秒时间戳在同一毫秒内碰撞时 rename 会失败；
        // 唯一命名保证「不覆盖已有备份」。
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let target = backup_dir.join(format!(
            "stock-ipo-reminder-{}-{}.db",
            timestamp.format("%Y%m%d-%H%M%S-%3f"),
            suffix.get(..8).unwrap_or_default()
        ));
        let temporary = backup_dir.join(format!(
            ".stock-ipo-reminder-backup-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let source = self.open()?;
        let result = (|| -> Result<()> {
            let mut destination = Connection::open(&temporary)?;
            {
                let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
                backup.run_to_completion(BACKUP_PAGES_PER_STEP, BACKUP_STEP_PAUSE, None)?;
            }
            let integrity: String =
                destination.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            if integrity != "ok" {
                bail!("备份完整性检查失败：{integrity}")
            }
            drop(destination);
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&temporary)?
                .sync_all()?;
            before_commit(&temporary)?;
            fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        remove_sqlite_sidecars(&temporary);
        result?;
        Ok(target)
    }
}

pub(super) fn remove_sqlite_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(PathBuf::from(format!("{}{suffix}", path.display())));
    }
}

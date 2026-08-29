use super::*;

#[test]
fn update_residue_names_are_matched_strictly() {
    assert!(is_update_residue_name(
        ".StockIpoReminder-0.3.1-win-x64-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi.part"
    ));
    assert!(is_update_residue_name(
        "StockIpoReminder-0.3.1-win-x64-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi"
    ));
    assert!(!is_update_residue_name("stock-ipo-reminder-20260828.db"));
    assert!(!is_update_residue_name("notes.msi"));
    assert!(!is_update_residue_name(
        "StockIpoReminder-0.3.1-win-x64-notauuid.msi"
    ));
    assert!(is_update_helper_residue_name(
        "StockIpoReminder-Update-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe"
    ));
    assert!(!is_update_helper_residue_name(
        "StockIpoReminder-Uninstall-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe"
    ));
    assert!(!is_update_helper_residue_name(
        "StockIpoReminder-MsiUninstall-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe"
    ));
    assert!(!is_update_helper_residue_name(
        "StockIpoReminder-Other-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe"
    ));
    assert!(!is_update_helper_residue_name("unrelated.exe"));
}

#[test]
fn cleanup_update_residue_deletes_only_old_matching_plain_files() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-residue-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let updates = root.join("updates");
    fs::create_dir_all(&updates).unwrap();
    let fresh_msi =
        updates.join("StockIpoReminder-0.3.1-win-x64-8f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi");
    fs::write(&fresh_msi, b"x").unwrap();
    let unrelated = updates.join("unrelated.txt");
    fs::write(&unrelated, b"x").unwrap();
    make_file_old(&unrelated);
    let old_msi =
        updates.join("StockIpoReminder-0.3.2-win-x64-7f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi");
    fs::write(&old_msi, b"x").unwrap();
    make_file_old(&old_msi);
    let old_partial =
        updates.join(".StockIpoReminder-0.3.2-win-x64-6f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi.part");
    fs::write(&old_partial, b"x").unwrap();
    make_file_old(&old_partial);
    let matching_directory =
        updates.join("StockIpoReminder-0.3.2-win-x64-5f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi");
    fs::create_dir(&matching_directory).unwrap();

    assert!(cleanup_update_residue(&updates));
    assert!(!old_msi.exists());
    assert!(!old_partial.exists());
    assert!(fresh_msi.is_file());
    assert!(unrelated.is_file());
    assert!(matching_directory.is_dir());

    #[cfg(windows)]
    {
        let target = root.join("outside-target.msi");
        fs::write(&target, b"x").unwrap();
        let link =
            updates.join("StockIpoReminder-0.3.2-win-x64-4f2f7b0ac2e94a2c8de4a91f1d0e55b2.msi");
        if std::os::windows::fs::symlink_file(&target, &link).is_ok() {
            assert!(plain_file_metadata(&link).unwrap().is_none());
            assert!(!cleanup_update_residue(&updates));
            assert!(target.is_file());
        }
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cleanup_helper_residue_is_scoped_to_old_update_helpers() {
    let root = std::env::temp_dir().join(format!(
        "stock-ipo-helper-residue-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&root).unwrap();
    let old_update = root.join("StockIpoReminder-Update-3f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe");
    fs::write(&old_update, b"x").unwrap();
    make_file_old(&old_update);
    let fresh_update = root.join("StockIpoReminder-Update-2f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe");
    fs::write(&fresh_update, b"x").unwrap();
    let old_uninstall =
        root.join("StockIpoReminder-Uninstall-1f2f7b0ac2e94a2c8de4a91f1d0e55b2.exe");
    fs::write(&old_uninstall, b"x").unwrap();
    make_file_old(&old_uninstall);

    assert!(cleanup_helper_residue(&root));
    assert!(!old_update.exists());
    assert!(fresh_update.is_file());
    assert!(old_uninstall.is_file());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn idle_maintenance_and_unchanged_operation_health_do_not_write() {
    let test = TestDatabase::new();
    assert!(!test.database.maintenance(&test.root).unwrap());

    let old = crate::core::at(
        NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    );
    test.database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO operation_health(component,last_attempt_at,last_success_at,health_state,last_error) VALUES('fixture',?1,?1,?2,NULL)",
                params![format_dt(old), HealthState::Healthy as i32],
            )
            .unwrap();
    test.database
        .save_operation_health("fixture", HealthState::Healthy, None)
        .unwrap();
    let unchanged: String = test
        .database
        .open()
        .unwrap()
        .query_row(
            "SELECT last_attempt_at FROM operation_health WHERE component='fixture'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unchanged, format_dt(old));
}

#[test]
fn business_fingerprint_ignores_heartbeats_and_health_audits() {
    let test = TestDatabase::new();
    test.database
        .save_settings(&AppSettings::default())
        .unwrap();
    let before = test.database.business_state_fingerprint().unwrap();

    test.database
        .touch_heartbeat("synchronization", now_china())
        .unwrap();
    test.database
        .save_operation_health("fixture", HealthState::Healthy, None)
        .unwrap();
    assert_eq!(test.database.business_state_fingerprint().unwrap(), before);

    let mut settings = test.database.settings().unwrap();
    settings.sound_enabled = !settings.sound_enabled;
    test.database.save_settings(&settings).unwrap();
    assert_ne!(test.database.business_state_fingerprint().unwrap(), before);
}

#[test]
fn backup_is_integrity_checked_and_leaves_no_temporary_file() {
    let test = TestDatabase::new();
    test.database.upsert_event(test.event()).unwrap();
    let backup_directory = test.root.join("backups");

    let path = test.database.backup(&backup_directory).unwrap();

    assert!(path.exists());
    let integrity: String = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(
        fs::read_dir(&backup_directory)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
    );
}

#[test]
fn interrupted_backup_commit_preserves_existing_backups_and_cleans_temporary_file() {
    let test = TestDatabase::new();
    test.database.upsert_event(test.event()).unwrap();
    let backup_directory = test.root.join("backups");
    let existing = test.database.backup(&backup_directory).unwrap();

    let error = test
        .database
        .backup_with_commit_hook(&backup_directory, |_| {
            bail!("simulated interruption before atomic commit")
        })
        .unwrap_err();
    assert!(error.to_string().contains("simulated interruption"));
    assert!(existing.exists());
    assert!(
        fs::read_dir(&backup_directory)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| entry.path().extension().is_none_or(|value| value != "tmp"))
    );
}

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

#[test]
fn business_fingerprint_includes_v13_account_fields_and_actions() {
    use crate::finance::{PaymentAccount, PaymentAccountKind};

    let test = TestDatabase::new();
    let now = now_china();
    let account = PaymentAccount {
        id: "fingerprint-loan".into(),
        kind: PaymentAccountKind::Mortgage,
        alias: "指纹贷款".into(),
        bank_id: Some("cmb".into()),
        bank_name: Some("招商银行".into()),
        due_day: 5,
        remind_time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        lead_days: vec![3, 0],
        overdue_enabled: true,
        enabled: true,
        first_tracked_cycle: "2026-01".into(),
        final_cycle: Some("2026-12".into()),
        closed_at: None,
        rules_changed_at: now,
        created_at: now,
        updated_at: now,
    };
    test.database.save_payment_account(&account).unwrap();
    let mut previous = test.database.business_state_fingerprint().unwrap();
    for sql in [
        "UPDATE credit_cards SET account_type='auto_loan' WHERE id='fingerprint-loan'",
        "UPDATE credit_cards SET bank_id='ccb' WHERE id='fingerprint-loan'",
        "UPDATE credit_cards SET final_cycle='2027-01' WHERE id='fingerprint-loan'",
    ] {
        test.database.open().unwrap().execute_batch(sql).unwrap();
        let current = test.database.business_state_fingerprint().unwrap();
        assert_ne!(current, previous, "指纹未覆盖字段变更：{sql}");
        previous = current;
    }
    test.database
        .mark_payment_account_paid(&account.id, "2027-01")
        .unwrap();
    let current = test.database.business_state_fingerprint().unwrap();
    assert_ne!(current, previous, "指纹未覆盖已还动作");
    previous = current;
    test.database
        .open()
        .unwrap()
        .execute(
            "UPDATE credit_cards SET closed_at=?2 WHERE id=?1",
            params![account.id, format_dt(now)],
        )
        .unwrap();
    assert_ne!(
        test.database.business_state_fingerprint().unwrap(),
        previous
    );
}

#[test]
fn backup_preserves_loan_end_close_and_paid_action() {
    use crate::finance::{PaymentAccount, PaymentAccountKind};

    let test = TestDatabase::new();
    let now = now_china();
    let account = PaymentAccount {
        id: "backup-loan".into(),
        kind: PaymentAccountKind::Mortgage,
        alias: "备份贷款".into(),
        bank_id: Some("other".into()),
        bank_name: Some("地方银行".into()),
        due_day: 5,
        remind_time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        lead_days: vec![0],
        overdue_enabled: true,
        enabled: true,
        first_tracked_cycle: "2026-01".into(),
        final_cycle: Some("2026-12".into()),
        closed_at: None,
        rules_changed_at: now,
        created_at: now,
        updated_at: now,
    };
    test.database.save_payment_account(&account).unwrap();
    test.database
        .mark_payment_account_paid(&account.id, "2026-12")
        .unwrap();
    test.database.close_payment_account(&account.id).unwrap();
    let path = test.database.backup(&test.root.join("backups")).unwrap();
    let connection = Connection::open(path).unwrap();
    let row = connection
        .query_row(
            "SELECT account_type,bank_id,bank,final_cycle,closed_at IS NOT NULL,
                    (SELECT COUNT(*) FROM credit_card_actions WHERE card_id='backup-loan')
             FROM credit_cards WHERE id='backup-loan'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(row.0, "mortgage");
    assert_eq!(row.1, "other");
    assert_eq!(row.2, "地方银行");
    assert_eq!(row.3, "2026-12");
    assert_eq!(row.4, 1);
    assert_eq!(row.5, 1);
}

use super::*;

impl Database {
    pub fn maintenance(&self, data_root: &Path) -> Result<bool> {
        let connection = self.open()?;
        let now = now_china();
        let raw_cutoff = format_dt(now - chrono::Duration::days(14));
        let sync_cutoff = format_dt(now - chrono::Duration::days(90));
        let reminder_cutoff = format_dt(now - chrono::Duration::days(180));
        let secondary_attempt_cutoff =
            format_dt(now - chrono::Duration::days(SECONDARY_ATTEMPT_RETENTION_DAYS));
        let secondary_outbox_cutoff =
            format_dt(now - chrono::Duration::days(SECONDARY_OUTBOX_RETENTION_DAYS));
        let needs_database_cleanup = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM raw_payloads WHERE fetched_at<?1)
                 OR EXISTS(SELECT 1 FROM sync_runs WHERE finished_at<?2)
                 OR EXISTS(SELECT 1 FROM reminder_log WHERE shown_at<?3)
                 OR EXISTS(SELECT 1 FROM secondary_notification_attempts WHERE attempted_at<?4)
                 OR (SELECT COUNT(*) FROM secondary_notification_attempts)>?5
                 OR EXISTS(
                     SELECT 1 FROM secondary_notification_outbox
                     WHERE state IN (?6,?7,?8) AND updated_at<?9
                 )",
            params![
                raw_cutoff,
                sync_cutoff,
                reminder_cutoff,
                secondary_attempt_cutoff,
                SECONDARY_MAX_ATTEMPT_RECORDS,
                SECONDARY_DELIVERED,
                SECONDARY_EXHAUSTED,
                SECONDARY_CANCELLED,
                secondary_outbox_cutoff,
            ],
            |row| row.get::<_, i32>(0),
        )? != 0;
        if needs_database_cleanup {
            connection.execute(
                "DELETE FROM raw_payloads WHERE fetched_at < ?1",
                [&raw_cutoff],
            )?;
            connection.execute(
                "DELETE FROM sync_runs WHERE finished_at < ?1",
                [&sync_cutoff],
            )?;
            connection.execute(
                "DELETE FROM reminder_log WHERE shown_at < ?1",
                [&reminder_cutoff],
            )?;
            prune_secondary_notification_history(&connection, now)?;
        }
        let mut changed = needs_database_cleanup;
        let temporary = data_root.join("temp");
        if temporary.exists() {
            for entry in fs::read_dir(temporary)? {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        crate::operations::log(
                            "WARN",
                            &format!("维护：读取临时目录条目失败：{error}"),
                        );
                        continue;
                    }
                };
                let path = entry.path();
                let metadata = match plain_file_metadata(&path) {
                    Ok(Some(metadata)) => metadata,
                    Ok(None) => continue,
                    Err(error) => {
                        crate::operations::log(
                            "WARN",
                            &format!("维护：读取临时文件元数据失败：{error}"),
                        );
                        continue;
                    }
                };
                if metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.elapsed().ok())
                    .is_some_and(|age| age > StdDuration::from_secs(24 * 3600))
                {
                    match fs::remove_file(path) {
                        Ok(()) => changed = true,
                        Err(error) => crate::operations::log(
                            "WARN",
                            &format!("维护：删除临时文件失败（将继续处理其余条目）：{error}"),
                        ),
                    }
                }
            }
        }
        changed |= cleanup_update_residue(&data_root.join("temp").join("updates"));
        changed |= cleanup_helper_residue(&std::env::temp_dir());
        Ok(changed)
    }

    pub fn business_state_fingerprint(&self) -> Result<Option<String>> {
        let connection = self.open()?;
        let mut state = String::new();
        let mut row_count = 0usize;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "events",
            "SELECT quote(id)||','||quote(exchange)||','||quote(board)||','||quote(security_code)||','||quote(apply_code)||','||quote(legacy_code)||','||quote(name)||','||quote(apply_date)||','||quote(issue_price)||','||quote(lot_size)||','||quote(max_apply_quantity)||','||quote(required_market_value)||','||quote(required_cash)||','||quote(ballot_date)||','||quote(payment_date)||','||quote(listing_date)||','||quote(issue_status)||','||quote(lifecycle_status)||','||quote(event_version)||','||quote(announcement_url)||','||quote(data_quality_status)||','||quote(data_conflict)||','||quote(sessions_json) FROM ipo_events ORDER BY id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "settings",
            "SELECT quote(json_value) FROM app_settings ORDER BY id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "acknowledgements",
            "SELECT quote(ipo_event_id)||','||quote(event_version)||','||quote(confirmed_data_hash)||','||quote(needs_review_at IS NOT NULL)||','||quote(review_reason)||','||quote(reconfirmed_at IS NOT NULL)||','||quote(revoked_at IS NOT NULL) FROM acknowledgements ORDER BY ipo_event_id,event_version",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "outbox",
            "SELECT quote(ipo_event_id)||','||quote(event_version)||','||quote(due_at)||','||quote(reminder_level)||','||quote(dedupe_key)||','||quote(lease_until)||','||quote(delivery_state)||','||quote(attempt_count)||','||quote(last_error)||','||quote(delivered_at)||','||quote(acknowledged_at)||','||quote(message) FROM reminder_outbox ORDER BY id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "secondary-outbox",
            "SELECT quote(reminder_outbox_id)||','||quote(provider)||','||quote(state)||','||quote(attempt_count)||','||quote(next_attempt_at)||','||quote(lease_until)||','||quote(last_error)||','||quote(delivered_at) FROM secondary_notification_outbox ORDER BY id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "overrides",
            "SELECT quote(ipo_event_id)||','||quote(event_version)||','||quote(field_name)||','||quote(override_value)||','||quote(reason)||','||quote(announcement_document_id)||','||quote(revoked_at IS NOT NULL) FROM manual_overrides ORDER BY id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "field-sources",
            "SELECT quote(ipo_event_id)||','||quote(field_name)||','||quote(normalized_value)||','||quote(raw_value)||','||quote(source)||','||quote(source_published_at)||','||quote(raw_hash)||','||quote(priority) FROM ipo_field_sources ORDER BY ipo_event_id,field_name,source,priority,id",
        )?;
        append_fingerprint_rows(
            &connection,
            &mut state,
            &mut row_count,
            "announcements",
            "SELECT quote(id)||','||quote(ipo_event_id)||','||quote(provider)||','||quote(announcement_id)||','||quote(announcement_type)||','||quote(title)||','||quote(published_at)||','||quote(source_url)||','||quote(local_path)||','||quote(file_hash)||','||quote(extraction_status)||','||quote(extracted_text_hash)||','||quote(parser_version)||','||quote(parsed_fields_json) FROM announcement_documents ORDER BY id",
        )?;
        Ok((row_count > 0).then(|| sha256(state)))
    }

    pub fn compact_if_needed(&self) -> Result<bool> {
        let connection = self.open()?;
        let page_count: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let free_pages: i64 =
            connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
        if page_count < 256 || free_pages.saturating_mul(4) < page_count {
            return Ok(false);
        }
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(true)
    }
}

pub(super) fn append_fingerprint_rows(
    connection: &Connection,
    state: &mut String,
    row_count: &mut usize,
    label: &str,
    sql: &str,
) -> Result<()> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        state.push_str(label);
        state.push('\0');
        state.push_str(&row?);
        state.push('\n');
        *row_count += 1;
    }
    Ok(())
}

pub(super) const UPDATE_RESIDUE_MIN_AGE: StdDuration = StdDuration::from_secs(24 * 3600);

pub(super) fn residue_age(metadata: &fs::Metadata) -> Option<StdDuration> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.elapsed().ok())
}

/// 检查目录项本身，只允许非 reparse 的普通文件；不得跟随链接离开扫描根目录。
pub(super) fn plain_file_metadata(path: &Path) -> std::io::Result<Option<fs::Metadata>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(windows)]
    if metadata.file_attributes() & 0x0400 != 0 {
        // FILE_ATTRIBUTE_REPARSE_POINT
        return Ok(None);
    }
    Ok(Some(metadata))
}

/// 严格匹配应用生成的更新文件名：
/// `.StockIpoReminder-<version>-win-x64-<uuid>.msi.part` 与
/// `StockIpoReminder-<version>-win-x64-<uuid>.msi`。
pub(super) fn is_update_residue_name(name: &str) -> bool {
    let stem = match name.strip_suffix(".msi.part") {
        Some(stem) => stem,
        None => match name.strip_suffix(".msi") {
            Some(stem) => stem,
            None => return false,
        },
    };
    let stem = stem.strip_prefix('.').unwrap_or(stem);
    let Some(rest) = stem.strip_prefix("StockIpoReminder-") else {
        return false;
    };
    let Some((version, operation_id)) = rest.rsplit_once("-win-x64-") else {
        return false;
    };
    !version.is_empty()
        && version
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
        && uuid::Uuid::parse_str(operation_id).is_ok()
}

/// 严格匹配应用更新 helper 在系统 %TEMP% 的残留名：
/// `StockIpoReminder-Update-<uuid>.exe`。
pub(super) fn is_update_helper_residue_name(name: &str) -> bool {
    name.strip_prefix("StockIpoReminder-Update-")
        .and_then(|rest| rest.strip_suffix(".exe"))
        .is_some_and(|operation_id| uuid::Uuid::parse_str(operation_id).is_ok())
}

/// 扫描受控更新目录（仅当前一层，不递归、不跟随符号链接/junction），
/// 清理超龄的下载/安装残留。
pub(super) fn cleanup_update_residue(update_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(update_dir) else {
        return false;
    };
    let mut changed = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                crate::operations::log("WARN", &format!("维护：读取更新目录条目失败：{error}"));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match plain_file_metadata(&path) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => continue,
            Err(error) => {
                crate::operations::log("WARN", &format!("维护：读取更新残留元数据失败：{error}"));
                continue;
            }
        };
        if residue_age(&metadata).is_none_or(|age| age <= UPDATE_RESIDUE_MIN_AGE) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_update_residue_name(name) {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => changed = true,
            Err(error) => crate::operations::log(
                "WARN",
                &format!("维护：删除更新残留失败（文件可能被占用，将继续处理其余条目）：{error}"),
            ),
        }
    }
    changed
}

/// 清理系统 %TEMP% 中更新 helper 的超龄残留；文件被占用
/// （sharing violation，例如 helper 仍在运行）时记录并跳过。
pub(super) fn cleanup_helper_residue(temp_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(temp_dir) else {
        return false;
    };
    let mut changed = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                crate::operations::log("WARN", &format!("维护：读取系统临时目录条目失败：{error}"));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match plain_file_metadata(&path) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => continue,
            Err(error) => {
                crate::operations::log(
                    "WARN",
                    &format!("维护：读取更新 helper 残留元数据失败：{error}"),
                );
                continue;
            }
        };
        if residue_age(&metadata).is_none_or(|age| age <= UPDATE_RESIDUE_MIN_AGE) {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_update_helper_residue_name(name) {
            continue;
        }
        match fs::remove_file(path) {
            Ok(()) => changed = true,
            Err(error) => crate::operations::log(
                "WARN",
                &format!("维护：删除更新 helper 残留失败（文件可能正在运行，已跳过）：{error}"),
            ),
        }
    }
    changed
}

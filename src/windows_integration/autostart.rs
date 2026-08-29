use super::*;

pub fn set_auto_start(enabled: bool, executable: &Path, data_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
        let value_name = auto_start_value_name(data_root);
        let run_key = wide_null(RUN_KEY);
        let value_name_wide = wide_null(&value_name);
        let mut key = HKEY::default();

        if enabled {
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(run_key.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                )
            };
            if status != ERROR_SUCCESS {
                bail!("无法打开 Windows 开机自启动注册表项：error={}", status.0);
            }
            let command = auto_start_command(executable, data_root);
            let bytes = unsafe {
                std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2)
            };
            let write_status = unsafe {
                RegSetValueExW(
                    key,
                    PCWSTR(value_name_wide.as_ptr()),
                    None,
                    REG_SZ,
                    Some(bytes),
                )
            };
            unsafe {
                let _ = RegCloseKey(key);
            }
            if write_status != ERROR_SUCCESS {
                bail!(
                    "无法写入 Windows 开机自启动注册表项：error={}",
                    write_status.0
                );
            }
        } else {
            let open_status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(run_key.as_ptr()),
                    None,
                    KEY_SET_VALUE,
                    &mut key,
                )
            };
            if open_status != ERROR_FILE_NOT_FOUND && open_status != ERROR_SUCCESS {
                bail!(
                    "无法打开 Windows 开机自启动注册表项：error={}",
                    open_status.0
                );
            }
            if open_status == ERROR_SUCCESS {
                let delete_status =
                    unsafe { RegDeleteValueW(key, PCWSTR(value_name_wide.as_ptr())) };
                unsafe {
                    let _ = RegCloseKey(key);
                }
                if delete_status != ERROR_SUCCESS && delete_status != ERROR_FILE_NOT_FOUND {
                    bail!(
                        "无法删除 Windows 开机自启动注册表项：error={}",
                        delete_status.0
                    );
                }
            }
        }

        remove_legacy_auto_start_task(data_root);
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, executable, data_root);
        Ok(())
    }
}

pub fn auto_start_registered(data_root: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
        let run_key = wide_null(RUN_KEY);
        let value_name = wide_null(&auto_start_value_name(data_root));
        let mut size = 0u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(run_key.as_ptr()),
                PCWSTR(value_name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut size),
            )
        };
        if status == ERROR_SUCCESS {
            return Ok(true);
        }
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(false);
        }
        bail!("无法读取 Windows 开机自启动注册表状态：error={}", status.0)
    }
    #[cfg(not(windows))]
    {
        let _ = data_root;
        Ok(false)
    }
}

pub(crate) fn auto_start_value_name(data_root: &Path) -> String {
    let identity = data_root.to_string_lossy().to_ascii_lowercase();
    format!("StockIpoReminder-{}", &sha256(identity.as_bytes())[..12])
}

#[cfg(windows)]
pub(crate) fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
pub(crate) fn auto_start_command(executable: &Path, data_root: &Path) -> Vec<u16> {
    let mut command = Vec::new();
    append_quoted_argument(&mut command, executable.as_os_str());
    command.extend(" --background --data-root ".encode_utf16());
    append_quoted_argument(&mut command, data_root.as_os_str());
    command.push(0);
    command
}

#[cfg(windows)]
pub(crate) fn append_quoted_argument(target: &mut Vec<u16>, value: &std::ffi::OsStr) {
    target.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in value.encode_wide() {
        if unit == b'\\' as u16 {
            backslashes += 1;
            continue;
        }
        if unit == b'"' as u16 {
            target.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
        } else {
            target.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
        }
        backslashes = 0;
        target.push(unit);
    }
    target.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    target.push(b'"' as u16);
}

#[cfg(windows)]
pub(crate) fn remove_legacy_auto_start_task(data_root: &Path) {
    let identity = data_root.to_string_lossy().to_ascii_lowercase();
    let task_name = format!("StockIpoReminder-{}", &sha256(identity.as_bytes())[..12]);
    let _ = Command::new("schtasks.exe")
        .args(["/Delete", "/F", "/TN", &task_name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

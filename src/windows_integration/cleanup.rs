use super::*;

pub fn delete_after_reboot(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let result = MoveFileExW(
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );
            if result.is_err() {
                // 调度删除失败不能静默：残留文件依赖维护清理兜底。
                crate::operations::log(
                    "WARN",
                    &format!(
                        "注册重启后删除失败：{}（Windows 错误码 {}）",
                        crate::operations::redact(&path.display().to_string()),
                        std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
                    ),
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

use super::*;

pub struct SingleInstance {
    #[cfg(windows)]
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire(data_root: &Path) -> Result<Self> {
        Self::acquire_named(data_root, "")
    }

    pub fn try_acquire_supervisor(data_root: &Path) -> Result<Option<Self>> {
        Self::try_acquire_named(data_root, "-Watchdog")
    }

    fn acquire_named(data_root: &Path, suffix: &str) -> Result<Self> {
        Self::try_acquire_named(data_root, suffix)?.context("同一数据目录已有一个实例正在运行")
    }

    fn try_acquire_named(data_root: &Path, suffix: &str) -> Result<Option<Self>> {
        #[cfg(windows)]
        {
            let name = instance_mutex_name(data_root, suffix);
            let wide: Vec<u16> = std::ffi::OsStr::new(&name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let handle = unsafe { CreateMutexW(None, true, PCWSTR(wide.as_ptr())) }
                .context("无法创建单实例 Mutex")?;
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Ok(None);
            }
            return Ok(Some(Self { handle }));
        }
        #[cfg(not(windows))]
        {
            let _ = (data_root, suffix);
            Ok(Some(Self {}))
        }
    }
}

pub(crate) fn instance_mutex_name(data_root: &Path, suffix: &str) -> String {
    let identity = data_root.to_string_lossy().to_ascii_lowercase();
    format!(
        "Local\\StockIpoReminder-{}{}",
        &sha256(identity.as_bytes())[..20],
        suffix
    )
}

pub fn activation_message_name(data_root: &Path) -> String {
    let identity = data_root.to_string_lossy().to_ascii_lowercase();
    format!(
        "StockIpoReminder.Activate.{}",
        &sha256(identity.as_bytes())[..20]
    )
}

pub fn request_activate_existing(data_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let name = wide_null(&activation_message_name(data_root));
        let message = unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) };
        if message == 0 {
            bail!("无法注册现有实例唤醒消息");
        }
        unsafe { PostMessageW(Some(HWND_BROADCAST), message, WPARAM(0), LPARAM(0)) }
            .context("无法广播现有实例唤醒消息")?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = data_root;
        Ok(())
    }
}

pub fn application_instance_running(data_root: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let name = instance_mutex_name(data_root, "");
        let wide: Vec<u16> = std::ffi::OsStr::new(&name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }
            .context("无法探测主程序单实例 Mutex")?;
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Ok(existed);
    }
    #[cfg(not(windows))]
    {
        let _ = data_root;
        Ok(false)
    }
}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

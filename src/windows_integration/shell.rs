use super::*;

pub fn open_folder(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(windows)]
    {
        Command::new("explorer.exe").arg(path).spawn()?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

pub fn open_external(target: &str) -> Result<()> {
    let parsed = url::Url::parse(target).context("公告原文地址无效")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("只允许打开 http/https 公告地址");
    }
    shell_open(std::ffi::OsStr::new(target))
}

pub fn open_local_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("本地公告文件不存在：{}", path.display());
    }
    shell_open(path.as_os_str())
}

pub(crate) fn shell_open(target: &std::ffi::OsStr) -> Result<()> {
    #[cfg(windows)]
    {
        let wide: Vec<u16> = target.encode_wide().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR::null(),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            bail!("Windows 无法打开目标，错误代码 {}", result.0 as isize);
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        bail!("当前平台不支持打开外部目标")
    }
}

pub fn copy_text(text: &str) -> Result<()> {
    #[cfg(windows)]
    unsafe {
        OpenClipboard(None).context("无法打开 Windows 剪贴板")?;
        let result = (|| -> Result<()> {
            EmptyClipboard().context("无法清空 Windows 剪贴板")?;
            let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * size_of::<u16>())
                .context("无法分配剪贴板内存")?;
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                let _ = GlobalFree(Some(memory));
                bail!("无法锁定剪贴板内存");
            }
            pointer.copy_from_nonoverlapping(wide.as_ptr(), wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) = SetClipboardData(13, Some(HANDLE(memory.0))) {
                let _ = GlobalFree(Some(memory));
                return Err(error).context("无法写入 Windows 剪贴板");
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        return result;
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        bail!("当前平台不支持复制到剪贴板")
    }
}

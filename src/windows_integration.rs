use std::{
    env, fs,
    mem::size_of,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::core::sha256;

#[cfg(windows)]
use std::os::windows::{ffi::OsStrExt, process::CommandExt};
#[cfg(windows)]
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, GlobalFree, HANDLE, HINSTANCE, HWND,
            LPARAM, WPARAM,
        },
        Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Diagnostics::Debug::MessageBeep,
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            ProcessStatus::EmptyWorkingSet,
            Threading::{CreateMutexW, GetCurrentProcess},
        },
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, HICON, ICON_BIG,
                ICON_SMALL, LoadIconW, MB_ICONEXCLAMATION, SW_SHOWNORMAL, SendMessageW, WM_SETICON,
            },
        },
    },
    core::PCWSTR,
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
pub const APP_ICON_RESOURCE_ID: u16 = 1;

pub struct SingleInstance {
    #[cfg(windows)]
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire(data_root: &Path) -> Result<Self> {
        #[cfg(windows)]
        {
            let identity = data_root.to_string_lossy().to_ascii_lowercase();
            let name = format!(
                "Local\\StockIpoReminder-{}",
                &sha256(identity.as_bytes())[..20]
            );
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
                bail!("同一数据目录已有一个实例正在运行");
            }
            return Ok(Self { handle });
        }
        #[cfg(not(windows))]
        {
            let _ = data_root;
            Ok(Self {})
        }
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

pub fn trim_working_set() {
    #[cfg(windows)]
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

pub fn play_alert() {
    #[cfg(windows)]
    unsafe {
        let _ = MessageBeep(MB_ICONEXCLAMATION);
    }
}

#[cfg(windows)]
pub fn application_icon() -> Result<HICON> {
    let module = unsafe { GetModuleHandleW(None) }.context("无法读取当前程序模块")?;
    let resource = PCWSTR(APP_ICON_RESOURCE_ID as usize as *const u16);
    unsafe { LoadIconW(Some(HINSTANCE(module.0)), resource) }.context("无法加载程序图标资源")
}

pub fn install_window_icon(window: &slint::Window) -> Result<()> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().context("无法读取主窗口句柄")?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            bail!("当前主窗口不是 Win32 窗口");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let icon = application_icon()?;
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(())
    }
}

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

fn shell_open(target: &std::ffi::OsStr) -> Result<()> {
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

pub fn flash_window(window: &slint::Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = window.window_handle();
        let Ok(raw) = handle.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            return;
        };
        let info = FLASHWINFO {
            cbSize: size_of::<FLASHWINFO>() as u32,
            hwnd: HWND(raw.hwnd.get() as *mut _),
            dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
            uCount: 5,
            dwTimeout: 0,
        };
        unsafe {
            let _ = FlashWindowEx(&info);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

pub fn set_auto_start(enabled: bool, executable: &Path, data_root: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let identity = data_root.to_string_lossy().to_ascii_lowercase();
        let task_name = format!("StockIpoReminder-{}", &sha256(identity.as_bytes())[..12]);
        if !enabled {
            let status = Command::new("schtasks.exe")
                .args(["/Delete", "/F", "/TN", &task_name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !matches!(status.code(), Some(0 | 1)) {
                bail!("Windows 计划任务删除失败：exit={:?}", status.code());
            }
            return Ok(());
        }

        let user = format!(
            "{}\\{}",
            env::var("USERDOMAIN").unwrap_or_default(),
            env::var("USERNAME").unwrap_or_default()
        );
        let working_directory = executable.parent().unwrap_or_else(|| Path::new("."));
        let arguments = format!("--background --data-root \"{}\"", data_root.display());
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Stock IPO Reminder background startup</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><Hidden>false</Hidden><RunOnlyIfIdle>false</RunOnlyIfIdle><WakeToRun>false</WakeToRun><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Priority>7</Priority><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings>
  <Actions Context="Author"><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>
</Task>"#,
            xml_escape(&user),
            xml_escape(&user),
            xml_escape(&executable.to_string_lossy()),
            xml_escape(&arguments),
            xml_escape(&working_directory.to_string_lossy())
        );
        let path = env::temp_dir().join(format!(
            "stock-ipo-reminder-{}.xml",
            uuid::Uuid::new_v4().simple()
        ));
        let mut bytes = Vec::with_capacity(2 + xml.len() * 2);
        bytes.extend_from_slice(&[0xff, 0xfe]);
        for unit in xml.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&path, bytes)?;
        let status = Command::new("schtasks.exe")
            .args([
                "/Create",
                "/F",
                "/TN",
                &task_name,
                "/XML",
                path.to_string_lossy().as_ref(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        let _ = fs::remove_file(&path);
        let status = status?;
        if !status.success() {
            bail!("Windows 计划任务更新失败：exit={:?}", status.code());
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, executable, data_root);
        Ok(())
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn delete_after_reboot(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let _ = MoveFileExW(
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

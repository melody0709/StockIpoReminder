use std::{
    fs,
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
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError,
            GlobalFree, HANDLE, HINSTANCE, HWND, LPARAM, RECT, WPARAM,
        },
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Diagnostics::Debug::MessageBeep,
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            ProcessStatus::EmptyWorkingSet,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
            },
            Threading::{CreateMutexW, GetCurrentProcess},
        },
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{
                FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GetClientRect,
                GetWindowRect, HICON, ICON_BIG, ICON_SMALL, LoadIconW, MB_ICONEXCLAMATION,
                SW_SHOWNORMAL, SendMessageW, WM_SETICON,
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

pub fn fit_window_to_work_area(window: &slint::Window) -> Result<()> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().context("无法读取主窗口句柄")?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            bail!("当前主窗口不是 Win32 窗口");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            bail!("无法读取主窗口所在显示器的工作区");
        }

        let mut window_rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut window_rect) }.context("无法读取主窗口外框")?;
        let mut client_rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client_rect) }.context("无法读取主窗口客户区")?;

        let work_width = (monitor_info.rcWork.right - monitor_info.rcWork.left).max(1);
        let work_height = (monitor_info.rcWork.bottom - monitor_info.rcWork.top).max(1);
        let outer_width = (window_rect.right - window_rect.left).max(1);
        let outer_height = (window_rect.bottom - window_rect.top).max(1);
        let client_width = (client_rect.right - client_rect.left).max(1);
        let client_height = (client_rect.bottom - client_rect.top).max(1);
        let frame_width = (outer_width - client_width).max(0);
        let frame_height = (outer_height - client_height).max(0);
        let margin = (8.0 * window.scale_factor()).round().max(1.0) as i32;
        let available_width = (work_width - frame_width - margin * 2).max(1) as u32;
        let available_height = (work_height - frame_height - margin * 2).max(1) as u32;
        let current_size = window.size();
        let scale_factor = window.scale_factor();
        let minimum_width = (760.0 * scale_factor).round().max(1.0) as u32;
        let minimum_height = (460.0 * scale_factor).round().max(1.0) as u32;
        let target_width =
            clamp_window_dimension(current_size.width, minimum_width, available_width);
        let target_height =
            clamp_window_dimension(current_size.height, minimum_height, available_height);

        if target_width != current_size.width || target_height != current_size.height {
            window.set_size(slint::PhysicalSize::new(target_width, target_height));
        }

        let target_outer_width = target_width as i32 + frame_width;
        let target_outer_height = target_height as i32 + frame_height;
        let x = monitor_info.rcWork.left + (work_width - target_outer_width).max(0) / 2;
        let y = monitor_info.rcWork.top + (work_height - target_outer_height).max(0) / 2;
        window.set_position(slint::PhysicalPosition::new(x, y));
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(())
    }
}

fn clamp_window_dimension(current: u32, minimum: u32, available: u32) -> u32 {
    current.min(available).max(minimum.min(available))
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

fn auto_start_value_name(data_root: &Path) -> String {
    let identity = data_root.to_string_lossy().to_ascii_lowercase();
    format!("StockIpoReminder-{}", &sha256(identity.as_bytes())[..12])
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn auto_start_command(executable: &Path, data_root: &Path) -> Vec<u16> {
    let mut command = Vec::new();
    append_quoted_argument(&mut command, executable.as_os_str());
    command.extend(" --background --data-root ".encode_utf16());
    append_quoted_argument(&mut command, data_root.as_os_str());
    command.push(0);
    command
}

#[cfg(windows)]
fn append_quoted_argument(target: &mut Vec<u16>, value: &std::ffi::OsStr) {
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
fn remove_legacy_auto_start_task(data_root: &Path) {
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

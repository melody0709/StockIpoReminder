use std::{
    cell::Cell,
    fs,
    mem::size_of,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::core::sha256;

#[cfg(windows)]
use std::os::windows::{
    ffi::{OsStrExt, OsStringExt},
    process::CommandExt,
};
#[cfg(windows)]
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager},
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError,
            GlobalFree, HANDLE, HINSTANCE, HWND, LPARAM, PROPERTYKEY, RECT, WPARAM,
        },
        Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW},
        System::{
            Com::StructuredStorage::{PropVariantClear, PropVariantToString},
            Com::{CoTaskMemFree, IBindCtx},
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Diagnostics::Debug::MessageBeep,
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            ProcessStatus::EmptyWorkingSet,
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
                RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW,
                RegOpenKeyExW, RegSetValueExW,
            },
            Services::{
                CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
                SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
            },
            Threading::{CreateMutexW, GetCurrentProcess},
            WinRT::{RO_INIT_SINGLETHREADED, RoInitialize},
        },
        UI::{
            Shell::{
                FOLDERID_CommonPrograms, KF_FLAG_DEFAULT,
                PropertiesSystem::{
                    GPS_DEFAULT, IPropertyStore, SHGetPropertyStoreFromParsingName,
                },
                QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP, QUNS_BUSY, QUNS_NOT_PRESENT,
                QUNS_PRESENTATION_MODE, QUNS_QUIET_TIME, QUNS_RUNNING_D3D_FULL_SCREEN,
                SHGetKnownFolderPath, SHQueryUserNotificationState,
                SetCurrentProcessExplicitAppUserModelID, ShellExecuteW,
            },
            WindowsAndMessaging::{
                FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWL_EXSTYLE,
                GetClientRect, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HICON,
                HWND_BROADCAST, HWND_TOPMOST, ICON_BIG, ICON_SMALL, IsIconic, IsWindowVisible,
                LoadIconW, MB_ICONEXCLAMATION, PostMessageW, RegisterWindowMessageW, SW_SHOWNORMAL,
                SWP_NOACTIVATE, SWP_SHOWWINDOW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
                WM_SETICON, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{GUID, HSTRING, IInspectable, PCWSTR, w},
};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
pub const APP_ICON_RESOURCE_ID: u16 = 1;
pub const APP_USER_MODEL_ID: &str = "StockIpoReminder.Desktop";

#[cfg(windows)]
const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

#[cfg(windows)]
static PROCESS_APP_IDENTITY: OnceLock<std::result::Result<(), String>> = OnceLock::new();
type ToastActivationHandler = Arc<dyn Fn(Option<String>) + Send + Sync + 'static>;
static TOAST_ACTIVATION_HANDLER: OnceLock<Mutex<Option<ToastActivationHandler>>> = OnceLock::new();

#[cfg(windows)]
thread_local! {
    static WINRT_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToastDiagnostics {
    pub supported: bool,
    pub app_user_model_id: String,
    pub process_identity_set: bool,
    pub notifier_created: bool,
    pub notification_setting: Option<String>,
    pub notifications_enabled: bool,
    pub common_start_menu_shortcut_present: bool,
    pub shortcut_aumid_matches: bool,
    pub user_notification_state: Option<String>,
    pub accepts_notifications_now: Option<bool>,
    pub error: Option<String>,
    pub shortcut_error: Option<String>,
}

impl Default for ToastDiagnostics {
    fn default() -> Self {
        Self {
            supported: cfg!(windows),
            app_user_model_id: APP_USER_MODEL_ID.to_owned(),
            process_identity_set: false,
            notifier_created: false,
            notification_setting: None,
            notifications_enabled: false,
            common_start_menu_shortcut_present: false,
            shortcut_aumid_matches: false,
            user_notification_state: None,
            accepts_notifications_now: None,
            error: None,
            shortcut_error: None,
        }
    }
}

pub fn initialize_notification_platform() -> Result<()> {
    #[cfg(windows)]
    {
        ensure_process_app_identity()?;
        ensure_winrt_for_current_thread()?;
    }
    Ok(())
}

pub fn set_toast_activation_handler(handler: ToastActivationHandler) {
    if let Ok(mut target) = TOAST_ACTIVATION_HANDLER
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        *target = Some(handler);
    }
}

pub fn show_windows_toast(title: &str, body: &str, event_id: Option<&str>) -> Result<()> {
    #[cfg(windows)]
    {
        initialize_notification_platform()?;
        let notifier =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_USER_MODEL_ID))
                .context("无法创建 Windows Toast 通知器；开始菜单快捷方式可能尚未注册 AUMID")?;
        let setting = notifier
            .Setting()
            .context("无法读取 Windows Toast 权限状态")?;
        if setting != NotificationSetting::Enabled {
            bail!(
                "Windows Toast 当前不可用：{}",
                notification_setting_name(setting)
            );
        }

        let document = XmlDocument::new().context("无法创建 Windows Toast XML 文档")?;
        document
            .LoadXml(&HSTRING::from(toast_xml(title, body, event_id)))
            .context("无法解析 Windows Toast 内容")?;
        let notification = ToastNotification::CreateToastNotification(&document)
            .context("无法创建 Windows Toast 通知")?;
        let activated_event_id = event_id.map(str::to_owned);
        let activated = TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, _| {
            let callback = TOAST_ACTIVATION_HANDLER
                .get()
                .and_then(|target| target.lock().ok())
                .and_then(|target| target.as_ref().cloned());
            if let Some(callback) = callback {
                callback(activated_event_id.clone());
            }
            Ok(())
        });
        notification
            .Activated(&activated)
            .context("无法注册 Windows Toast 点击处理器")?;
        notifier
            .Show(&notification)
            .context("Windows 拒绝显示 Toast 通知")?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body, event_id);
        bail!("当前平台不支持 Windows Toast")
    }
}

pub fn toast_diagnostics() -> ToastDiagnostics {
    let mut diagnostics = ToastDiagnostics::default();
    #[cfg(windows)]
    {
        match initialize_notification_platform() {
            Ok(()) => diagnostics.process_identity_set = true,
            Err(error) => diagnostics.error = Some(format!("{error:#}")),
        }

        match common_start_menu_shortcut_registration() {
            Ok((present, matches)) => {
                diagnostics.common_start_menu_shortcut_present = present;
                diagnostics.shortcut_aumid_matches = matches;
            }
            Err(error) => diagnostics.shortcut_error = Some(format!("{error:#}")),
        }

        match unsafe { SHQueryUserNotificationState() } {
            Ok(state) => {
                diagnostics.user_notification_state = Some(user_notification_state_name(state));
                diagnostics.accepts_notifications_now = Some(state == QUNS_ACCEPTS_NOTIFICATIONS);
            }
            Err(error) => {
                if diagnostics.error.is_none() {
                    diagnostics.error = Some(format!("无法读取 Windows 当前通知呈现状态：{error}"));
                }
            }
        }

        if diagnostics.process_identity_set {
            match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
                APP_USER_MODEL_ID,
            )) {
                Ok(notifier) => {
                    diagnostics.notifier_created = true;
                    match notifier.Setting() {
                        Ok(setting) => {
                            diagnostics.notification_setting =
                                Some(notification_setting_name(setting).to_owned());
                            diagnostics.notifications_enabled =
                                setting == NotificationSetting::Enabled;
                        }
                        Err(error) => {
                            diagnostics.error =
                                Some(format!("无法读取 Windows Toast 权限状态：{error}"));
                        }
                    }
                }
                Err(error) => {
                    diagnostics.error = Some(format!(
                        "无法创建 Windows Toast 通知器；安装快捷方式可能尚未注册：{error}"
                    ));
                }
            }
        }
    }
    diagnostics
}

#[cfg(windows)]
fn ensure_process_app_identity() -> Result<()> {
    match PROCESS_APP_IDENTITY.get_or_init(|| {
        let app_id = wide_null(APP_USER_MODEL_ID);
        unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr())) }
            .map_err(|error| format!("无法设置进程 AppUserModelID：{error}"))
    }) {
        Ok(()) => Ok(()),
        Err(error) => bail!(error.clone()),
    }
}

#[cfg(windows)]
fn ensure_winrt_for_current_thread() -> Result<()> {
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106_u32 as i32;
    WINRT_INITIALIZED.with(|initialized| {
        if initialized.get() {
            return Ok(());
        }
        match unsafe { RoInitialize(RO_INIT_SINGLETHREADED) } {
            Ok(()) => initialized.set(true),
            Err(error) if error.code().0 == RPC_E_CHANGED_MODE => {
                // The UI host already selected a COM apartment; WinRT remains available in it.
                initialized.set(true);
            }
            Err(error) => return Err(error).context("无法初始化 Windows Runtime 通知线程"),
        }
        Ok(())
    })
}

#[cfg(windows)]
fn common_start_menu_shortcut_registration() -> Result<(bool, bool)> {
    ensure_winrt_for_current_thread()?;
    let common_programs =
        unsafe { SHGetKnownFolderPath(&FOLDERID_CommonPrograms, KF_FLAG_DEFAULT, None) }
            .context("无法定位公共开始菜单")?;
    let common_programs_path = unsafe { path_buf_from_allocated_wide(common_programs.0) }?;
    let shortcut = common_programs_path
        .join("A 股新股申购提醒")
        .join("A 股新股申购提醒.lnk");
    if !shortcut.is_file() {
        return Ok((false, false));
    }

    let wide = wide_null(&shortcut.to_string_lossy());
    let store: IPropertyStore = unsafe {
        SHGetPropertyStoreFromParsingName::<_, Option<&IBindCtx>, IPropertyStore>(
            PCWSTR(wide.as_ptr()),
            None,
            GPS_DEFAULT,
        )
    }
    .context("无法读取开始菜单快捷方式属性")?;
    let mut value = unsafe { store.GetValue(&PKEY_APP_USER_MODEL_ID) }
        .context("无法读取开始菜单快捷方式 AppUserModelID")?;
    let mut text = [0u16; 129];
    let conversion = unsafe { PropVariantToString(&value, &mut text) };
    let clear = unsafe { PropVariantClear(&mut value) };
    conversion.context("开始菜单快捷方式 AppUserModelID 格式无效")?;
    clear.context("无法释放开始菜单快捷方式属性")?;
    let end = text
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(text.len());
    Ok((
        true,
        String::from_utf16_lossy(&text[..end]) == APP_USER_MODEL_ID,
    ))
}

#[cfg(windows)]
unsafe fn path_buf_from_allocated_wide(pointer: *mut u16) -> Result<PathBuf> {
    if pointer.is_null() {
        bail!("Windows 返回了空的已知文件夹路径");
    }
    let mut length = 0usize;
    while unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(unsafe {
        std::slice::from_raw_parts(pointer, length)
    }));
    unsafe { CoTaskMemFree(Some(pointer.cast())) };
    Ok(path)
}

#[cfg(windows)]
fn notification_setting_name(setting: NotificationSetting) -> &'static str {
    match setting {
        NotificationSetting::Enabled => "enabled",
        NotificationSetting::DisabledForApplication => "disabledForApplication",
        NotificationSetting::DisabledForUser => "disabledForUser",
        NotificationSetting::DisabledByGroupPolicy => "disabledByGroupPolicy",
        NotificationSetting::DisabledByManifest => "disabledByManifestOrRegistration",
        _ => "unknown",
    }
}

#[cfg(windows)]
fn user_notification_state_name(
    state: windows::Win32::UI::Shell::QUERY_USER_NOTIFICATION_STATE,
) -> String {
    match state {
        QUNS_NOT_PRESENT => "notPresent",
        QUNS_BUSY => "busy",
        QUNS_RUNNING_D3D_FULL_SCREEN => "fullScreen",
        QUNS_PRESENTATION_MODE => "presentationMode",
        QUNS_ACCEPTS_NOTIFICATIONS => "acceptsNotifications",
        QUNS_QUIET_TIME => "quietTime",
        QUNS_APP => "app",
        _ => "unknown",
    }
    .to_owned()
}

fn toast_xml(title: &str, body: &str, event_id: Option<&str>) -> String {
    let launch = event_id
        .filter(|value| !value.is_empty())
        .map(|value| {
            format!(
                " launch=\"eventId={}\"",
                xml_escape(&truncate_chars(value, 256))
            )
        })
        .unwrap_or_default();
    format!(
        "<toast duration=\"long\"{launch}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(&truncate_chars(title.trim(), 96)),
        xml_escape(&truncate_chars(body.trim(), 512)),
    )
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    let mut characters = value.chars();
    let prefix: String = characters.by_ref().take(maximum).collect();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        // XML 1.0 Char 合法范围：#x9 | #xA | #xD | [#x20-#xD7FF] |
        // [#xE000-#xFFFD] | [#x10000-#x10FFFF]。TAB/LF/CR 合法必须保留，
        // 其余 C0 控制字符会导致 XmlDocument::LoadXml 失败。
        let is_legal = matches!(character, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{D7FF}').contains(&character)
            || ('\u{E000}'..='\u{FFFD}').contains(&character)
            || ('\u{10000}'..='\u{10FFFF}').contains(&character);
        if !is_legal {
            continue;
        }
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

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

fn instance_mutex_name(data_root: &Path, suffix: &str) -> String {
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

pub fn windows_time_service_running() -> Result<Option<bool>> {
    #[cfg(windows)]
    {
        let manager = unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }
            .context("无法打开 Windows 服务控制管理器")?;
        let service = match unsafe { OpenServiceW(manager, w!("W32Time"), SERVICE_QUERY_STATUS) } {
            Ok(service) => service,
            Err(error) => {
                unsafe {
                    let _ = CloseServiceHandle(manager);
                }
                return Err(error).context("无法打开 Windows Time 服务");
            }
        };
        let mut status = SERVICE_STATUS::default();
        let query = unsafe { QueryServiceStatus(service, &mut status) };
        unsafe {
            let _ = CloseServiceHandle(service);
            let _ = CloseServiceHandle(manager);
        }
        query.context("无法读取 Windows Time 服务状态")?;
        return Ok(Some(status.dwCurrentState == SERVICE_RUNNING));
    }
    #[cfg(not(windows))]
    {
        Ok(None)
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
        let minimum_width = (800.0 * scale_factor).round().max(1.0) as u32;
        let minimum_height = (500.0 * scale_factor).round().max(1.0) as u32;
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

pub fn show_reminder_window(window: &slint::Window) -> Result<()> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = match handle.window_handle() {
            Ok(raw) => raw,
            Err(_) => {
                window.show().context("无法创建专用提醒窗口")?;
                handle.window_handle().context("无法读取提醒窗口句柄")?
            }
        };
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            bail!("当前提醒窗口不是 Win32 窗口");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let existing_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let no_activate_style =
            existing_style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, no_activate_style);
        }
        window.show().context("无法显示专用提醒窗口")?;

        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            bail!("无法读取提醒窗口所在显示器工作区");
        }
        let size = window.size();
        let margin = (16.0 * window.scale_factor()).round().max(1.0) as i32;
        let width = size.width.max(1) as i32;
        let height = size.height.max(1) as i32;
        let x = (monitor_info.rcWork.right - width - margin).max(monitor_info.rcWork.left);
        let y = (monitor_info.rcWork.bottom - height - margin).max(monitor_info.rcWork.top);
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .context("无法无激活显示专用提醒窗口")?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        window.show().context("无法显示专用提醒窗口")?;
        Ok(())
    }
}

pub fn confirm_window_visible(window: &slint::Window) -> Result<()> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().context("无法读取窗口句柄")?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            bail!("当前窗口不是 Win32 窗口");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            bail!("窗口尚未进入可见状态");
        }
        if unsafe { IsIconic(hwnd) }.as_bool() {
            bail!("窗口处于最小化状态");
        }
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.context("无法读取窗口位置")?;
        if rect.right <= rect.left || rect.bottom <= rect.top {
            bail!("窗口外框尺寸无效");
        }
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut monitor_info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
            bail!("无法读取窗口所在显示器工作区");
        }
        let intersects = rect.left < monitor_info.rcWork.right
            && rect.right > monitor_info.rcWork.left
            && rect.top < monitor_info.rcWork.bottom
            && rect.bottom > monitor_info.rcWork.top;
        if !intersects {
            bail!("窗口未与可用工作区相交");
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        if window.is_visible() {
            Ok(())
        } else {
            bail!("窗口尚未进入可见状态")
        }
    }
}

pub fn window_is_foreground(window: &slint::Window) -> Result<bool> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().context("无法读取窗口句柄")?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            bail!("当前窗口不是 Win32 窗口");
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        return Ok(unsafe { GetForegroundWindow() } == hwnd);
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_keeps_xml_10_chars_and_filters_illegal_control_bytes() {
        assert_eq!(xml_escape("a\tb\nc\rd"), "a\tb\nc\rd");
        assert_eq!(
            xml_escape("<a>&\"'</a>"),
            "&lt;a&gt;&amp;&quot;&apos;&lt;/a&gt;"
        );
        assert_eq!(xml_escape("中\u{0}文\u{8}"), "中文");
        assert_eq!(xml_escape("emoji \u{1F600}"), "emoji \u{1F600}");
        assert_eq!(xml_escape(""), "");
    }

    #[test]
    fn activation_message_is_stable_and_scoped_to_the_data_root() {
        let first = activation_message_name(Path::new("C:\\Data\\StockIpoReminder"));
        let same = activation_message_name(Path::new("c:\\data\\stockiporeminder"));
        let other = activation_message_name(Path::new("D:\\Data\\StockIpoReminder"));
        assert_eq!(first, same);
        assert_ne!(first, other);
        assert!(!first.contains("C:\\Data"));
    }

    #[test]
    fn toast_xml_escapes_untrusted_text_and_limits_payload_size() {
        let xml = toast_xml(
            "A&B <测试>",
            &format!("'\"{}", "字".repeat(600)),
            Some("shanghai:601001&version=2"),
        );
        assert!(xml.contains("A&amp;B &lt;测试&gt;"));
        assert!(xml.contains("&apos;&quot;"));
        assert!(!xml.contains("A&B"));
        assert!(xml.chars().count() < 800);
        assert!(xml.contains('…'));
        assert!(xml.contains("launch=\"eventId=shanghai:601001&amp;version=2\""));
    }
}

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

mod autostart;
mod cleanup;
mod instance;
mod shell;
mod system;
mod toast;
mod window;

#[allow(unused_imports)]
pub(crate) use {autostart::*, cleanup::*, instance::*, shell::*, system::*, toast::*, window::*};

#[cfg(test)]
mod tests;

use std::{
    mem::size_of,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use slint::{ComponentHandle, Timer, Weak};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
                WTSUnRegisterSessionNotification,
            },
        },
        UI::{
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_RESPECT_QUIET_TIME, NIIF_WARNING,
                NIM_ADD, NIM_DELETE, NIM_MODIFY, NIN_BALLOONUSERCLICK, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DispatchMessageW, GetCursorPos, GetMessageW, MF_SEPARATOR, MF_STRING, MSG,
                PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMECRITICAL, PBT_APMRESUMESTANDBY,
                PBT_APMRESUMESUSPEND, PostMessageW, PostQuitMessage, RegisterClassW,
                RegisterWindowMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
                TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
                WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_POWERBROADCAST,
                WM_RBUTTONUP, WM_TIMECHANGE, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_UNLOCK,
            },
        },
    },
    core::w,
};

use crate::{MainWindow, runtime::RuntimeHandle, windows_integration};

const TRAY_MESSAGE: u32 = WM_APP + 41;
const SHOW_COMMAND: usize = 1001;
const SYNC_COMMAND: usize = 1002;
const SETTINGS_COMMAND: usize = 1003;
const EXIT_COMMAND: usize = 1004;
const TODAY_COMMAND: usize = 1005;
const FUTURE_COMMAND: usize = 1006;
const LOGS_COMMAND: usize = 1007;
const ICON_ID: u32 = 1;

struct Callbacks {
    show: Box<dyn Fn() + Send + Sync>,
    activate: Box<dyn Fn() + Send + Sync>,
    today: Box<dyn Fn() + Send + Sync>,
    future: Box<dyn Fn() + Send + Sync>,
    logs: Box<dyn Fn() + Send + Sync>,
    notification: Box<dyn Fn(Option<String>) + Send + Sync>,
    sync: Box<dyn Fn() + Send + Sync>,
    settings: Box<dyn Fn() + Send + Sync>,
    exit: Box<dyn Fn() + Send + Sync>,
    recovery: Box<dyn Fn() + Send + Sync>,
}

static CALLBACKS: OnceLock<Callbacks> = OnceLock::new();
static LAST_NOTIFICATION_EVENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static LAST_RECOVERY: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
static TOAST_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED_MESSAGE: AtomicU32 = AtomicU32::new(0);
static ACTIVATE_INSTANCE_MESSAGE: AtomicU32 = AtomicU32::new(0);
static TASKBAR_READD_SUCCEEDED: AtomicU32 = AtomicU32::new(0);
static TASKBAR_READD_FAILED: AtomicU32 = AtomicU32::new(0);
static RECOVERY_POWER_MESSAGES: AtomicU32 = AtomicU32::new(0);
static RECOVERY_UNLOCK_MESSAGES: AtomicU32 = AtomicU32::new(0);
static RECOVERY_TIME_MESSAGES: AtomicU32 = AtomicU32::new(0);
static RECOVERY_ACCEPTED: AtomicU32 = AtomicU32::new(0);
static RECOVERY_SUPPRESSED: AtomicU32 = AtomicU32::new(0);
static RECOVERY_CALLBACKS: AtomicU32 = AtomicU32::new(0);

pub struct NativeTray {
    hwnd: Arc<AtomicIsize>,
    thread: Option<JoinHandle<()>>,
}

impl NativeTray {
    pub fn start(
        window: Weak<MainWindow>,
        runtime: RuntimeHandle,
        data_root: std::path::PathBuf,
        _recovery_smoke_mode: bool,
    ) -> Result<Self> {
        let activation_message_name = windows_integration::activation_message_name(&data_root);
        let show_window = window.clone();
        let activation_window = window.clone();
        let today_window = window.clone();
        let future_window = window.clone();
        let notification_window = window.clone();
        let settings_window = window.clone();
        let exit_window = window;
        let show_runtime = runtime.clone();
        let activation_runtime = runtime.clone();
        let today_runtime = runtime.clone();
        let future_runtime = runtime.clone();
        let settings_runtime = runtime.clone();
        let sync_runtime = runtime.clone();
        let notification_runtime = runtime.clone();
        let recovery_runtime = runtime;
        let _ = CALLBACKS.set(Callbacks {
            show: Box::new(move || {
                let weak = show_window.clone();
                let runtime = show_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        super::show_and_repaint(&window);
                    }
                });
            }),
            activate: Box::new(move || {
                let weak = activation_window.clone();
                let runtime = activation_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        super::show_and_repaint(&window);
                        let verify = window.as_weak();
                        Timer::single_shot(Duration::from_millis(150), move || {
                            let Some(window) = verify.upgrade() else {
                                crate::operations::log(
                                    "ERROR",
                                    "第二次启动唤醒后主窗口对象已销毁",
                                );
                                return;
                            };
                            match windows_integration::confirm_window_visible(window.window()) {
                                Ok(()) => crate::operations::log(
                                    "INFO",
                                    "第二次启动唤醒：主窗口已可见 event=second_launch_window_visible",
                                ),
                                Err(error) => crate::operations::log(
                                    "ERROR",
                                    &format!("第二次启动唤醒后主窗口可见性确认失败：{error:#}"),
                                ),
                            }
                        });
                    }
                });
            }),
            today: Box::new(move || {
                let weak = today_window.clone();
                let runtime = today_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        window.set_active_page(0);
                        super::show_and_repaint(&window);
                    }
                });
            }),
            future: Box::new(move || {
                let weak = future_window.clone();
                let runtime = future_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        window.set_active_page(1);
                        super::show_and_repaint(&window);
                    }
                });
            }),
            logs: Box::new(move || {
                if let Err(error) = windows_integration::open_folder(&data_root.join("logs")) {
                    crate::operations::log(
                        "ERROR",
                        &format!("从托盘打开日志目录失败：{error:#}"),
                    );
                }
            }),
            notification: Box::new(move |event_id| {
                let weak = notification_window.clone();
                let runtime = notification_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        super::show_and_repaint(&window);
                        if let Some(event_id) = event_id.filter(|value| !value.is_empty()) {
                            super::show_event_details(&window, &runtime, &event_id);
                        }
                    }
                });
            }),
            sync: Box::new(move || sync_runtime.request_sync("托盘手动同步")),
            settings: Box::new(move || {
                let weak = settings_window.clone();
                let runtime = settings_runtime.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        super::refresh_ui(&window, &runtime);
                        window.set_active_page(3);
                        super::show_and_repaint(&window);
                    }
                });
            }),
            exit: Box::new(move || {
                let weak = exit_window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = weak.upgrade() {
                        if window.get_pending_count() > 0 {
                            window.set_show_exit_confirmation(true);
                            super::show_and_repaint(&window);
                            return;
                        }
                        let _ = window.hide();
                    }
                    let _ = slint::quit_event_loop();
                });
            }),
            recovery: Box::new(move || {
                RECOVERY_CALLBACKS.fetch_add(1, Ordering::AcqRel);
                recovery_runtime.recovery();
            }),
        });
        windows_integration::set_toast_activation_handler(Arc::new(|event_id| {
            if let Some(callbacks) = CALLBACKS.get() {
                (callbacks.notification)(event_id);
            }
        }));
        let hwnd = Arc::new(AtomicIsize::new(0));
        let thread_hwnd = Arc::clone(&hwnd);
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("stock-ipo-native-tray".into())
            .spawn(move || unsafe {
                let _ = run_tray(thread_hwnd, sender, activation_message_name);
            })
            .context("无法启动托盘线程")?;
        match receiver.recv().context("托盘线程未返回初始化结果")? {
            Ok(()) => Ok(Self {
                hwnd,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                bail!(error)
            }
        }
    }

    pub fn notify(&self, title: &str, body: &str, event_id: Option<&str>) {
        match windows_integration::show_windows_toast(title, body, event_id) {
            Ok(()) => {
                TOAST_FALLBACK_LOGGED.store(false, Ordering::Release);
            }
            Err(error) => {
                if !TOAST_FALLBACK_LOGGED.swap(true, Ordering::AcqRel) {
                    crate::operations::log(
                        "WARN",
                        &format!("Windows Toast 不可用，后续系统通知已回退托盘气泡：{error:#}"),
                    );
                }
                self.notify_balloon(title, body, event_id);
            }
        }
    }

    pub fn notify_balloon(&self, title: &str, body: &str, event_id: Option<&str>) {
        let raw = self.hwnd.load(Ordering::Acquire);
        if raw == 0 {
            return;
        }
        if let Ok(mut target) = LAST_NOTIFICATION_EVENT
            .get_or_init(|| Mutex::new(None))
            .lock()
        {
            *target = event_id.map(str::to_owned);
        }
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(raw as *mut _),
            uID: ICON_ID,
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_WARNING | NIIF_RESPECT_QUIET_TIME,
            ..Default::default()
        };
        copy_wide(title, &mut data.szInfoTitle);
        copy_wide(body, &mut data.szInfo);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn set_status(&self, pending_count: i64, unhealthy: bool) {
        let raw = self.hwnd.load(Ordering::Acquire);
        if raw == 0 {
            return;
        }
        let text = if unhealthy {
            format!("A 股新股申购提醒 · 状态异常 · 待确认 {pending_count}")
        } else {
            format!("A 股新股申购提醒 · 待确认 {pending_count}")
        };
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(raw as *mut _),
            uID: ICON_ID,
            uFlags: NIF_TIP,
            ..Default::default()
        };
        copy_wide(&text, &mut data.szTip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    pub fn schedule_recovery_smoke(&self, report_path: std::path::PathBuf) -> Result<()> {
        let raw = self.hwnd.load(Ordering::Acquire);
        if raw == 0 {
            bail!("托盘消息窗口尚未创建");
        }
        TASKBAR_READD_SUCCEEDED.store(0, Ordering::Release);
        TASKBAR_READD_FAILED.store(0, Ordering::Release);
        RECOVERY_POWER_MESSAGES.store(0, Ordering::Release);
        RECOVERY_UNLOCK_MESSAGES.store(0, Ordering::Release);
        RECOVERY_TIME_MESSAGES.store(0, Ordering::Release);
        RECOVERY_ACCEPTED.store(0, Ordering::Release);
        RECOVERY_SUPPRESSED.store(0, Ordering::Release);
        RECOVERY_CALLBACKS.store(0, Ordering::Release);
        if let Ok(mut last) = LAST_RECOVERY.get_or_init(|| Mutex::new(None)).lock() {
            *last = None;
        }

        let hwnd = HWND(raw as *mut _);
        let icon_data = create_icon_data(hwnd)?;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data);
        }
        thread::Builder::new()
            .name("stock-ipo-recovery-smoke".into())
            .spawn(move || {
                let smoke_hwnd = HWND(raw as *mut _);
                let taskbar_message = TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire);
                unsafe {
                    let _ = PostMessageW(Some(smoke_hwnd), taskbar_message, WPARAM(0), LPARAM(0));
                    let _ = PostMessageW(Some(smoke_hwnd), WM_TIMECHANGE, WPARAM(0), LPARAM(0));
                    let _ = PostMessageW(
                        Some(smoke_hwnd),
                        WM_POWERBROADCAST,
                        WPARAM(PBT_APMRESUMEAUTOMATIC as usize),
                        LPARAM(0),
                    );
                    let _ = PostMessageW(
                        Some(smoke_hwnd),
                        WM_WTSSESSION_CHANGE,
                        WPARAM(WTS_SESSION_UNLOCK as usize),
                        LPARAM(0),
                    );
                }
                thread::sleep(Duration::from_millis(5_300));
                unsafe {
                    let _ = PostMessageW(Some(smoke_hwnd), WM_TIMECHANGE, WPARAM(0), LPARAM(0));
                }
                thread::sleep(Duration::from_millis(700));

                let taskbar_succeeded = TASKBAR_READD_SUCCEEDED.load(Ordering::Acquire);
                let taskbar_failed = TASKBAR_READD_FAILED.load(Ordering::Acquire);
                let power_messages = RECOVERY_POWER_MESSAGES.load(Ordering::Acquire);
                let unlock_messages = RECOVERY_UNLOCK_MESSAGES.load(Ordering::Acquire);
                let time_messages = RECOVERY_TIME_MESSAGES.load(Ordering::Acquire);
                let accepted = RECOVERY_ACCEPTED.load(Ordering::Acquire);
                let suppressed = RECOVERY_SUPPRESSED.load(Ordering::Acquire);
                let callbacks = RECOVERY_CALLBACKS.load(Ordering::Acquire);
                let success = taskbar_succeeded == 1
                    && taskbar_failed == 0
                    && power_messages == 1
                    && unlock_messages == 1
                    && time_messages == 2
                    && accepted == 2
                    && suppressed == 2
                    && callbacks == 2;
                let report = serde_json::json!({
                    "schemaVersion": "1",
                    "success": success,
                    "version": env!("CARGO_PKG_VERSION"),
                    "generatedAtUtc": chrono::Utc::now().to_rfc3339(),
                    "taskbarCreated": {
                        "iconRemovedBeforeSimulation": true,
                        "reRegistrationSucceeded": taskbar_succeeded,
                        "reRegistrationFailed": taskbar_failed
                    },
                    "recoveryMessages": {
                        "powerResume": power_messages,
                        "sessionUnlock": unlock_messages,
                        "timeChange": time_messages,
                        "acceptedAfterDebounce": accepted,
                        "suppressedByFiveSecondDebounce": suppressed,
                        "runtimeCallbacks": callbacks
                    }
                });
                let write_result = report_path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .transpose()
                    .and_then(|_| {
                        serde_json::to_vec_pretty(&report)
                            .map_err(std::io::Error::other)
                            .and_then(|bytes| std::fs::write(&report_path, bytes))
                    });
                if let Err(error) = write_result {
                    crate::operations::log(
                        "ERROR",
                        &format!("写入 Windows 恢复 smoke 报告失败：{error}"),
                    );
                }
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            })
            .context("无法启动 Windows 恢复 smoke 线程")?;
        Ok(())
    }
}

impl Drop for NativeTray {
    fn drop(&mut self) {
        let raw = self.hwnd.load(Ordering::Acquire);
        if raw != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(raw as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

unsafe fn run_tray(
    hwnd_slot: Arc<AtomicIsize>,
    initialized: mpsc::SyncSender<std::result::Result<(), String>>,
    activation_message_name: String,
) -> Result<()> {
    let setup = (|| -> Result<(HWND, NOTIFYICONDATAW)> {
        let module = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW 失败")?;
        let instance = HINSTANCE(module.0);
        let class_name = w!("StockIpoReminderTrayWindow");
        let window_class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&window_class) } == 0 {
            bail!("RegisterClassW 失败");
        }
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_created == 0 {
            bail!("RegisterWindowMessageW(TaskbarCreated) 失败");
        }
        TASKBAR_CREATED_MESSAGE.store(taskbar_created, Ordering::Release);
        let activation_name: Vec<u16> = activation_message_name
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let activation_message =
            unsafe { RegisterWindowMessageW(windows::core::PCWSTR(activation_name.as_ptr())) };
        if activation_message == 0 {
            bail!("RegisterWindowMessageW(ActivateInstance) 失败");
        }
        ACTIVATE_INSTANCE_MESSAGE.store(activation_message, Ordering::Release);
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                w!("Stock IPO Reminder"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .context("CreateWindowExW 失败")?;
        hwnd_slot.store(hwnd.0 as isize, Ordering::Release);
        unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
            .context("无法注册 Windows 会话恢复通知")?;
        let icon_data = create_icon_data(hwnd)?;
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &icon_data) }.as_bool() {
            bail!("Shell_NotifyIconW(NIM_ADD) 失败");
        }
        Ok((hwnd, icon_data))
    })();
    let (_hwnd, icon_data) = match setup {
        Ok(value) => {
            let _ = initialized.send(Ok(()));
            value
        }
        Err(error) => {
            let _ = initialized.send(Err(format!("{error:#}")));
            return Err(error);
        }
    };
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let _ = unsafe { WTSUnRegisterSessionNotification(_hwnd) };
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data) };
    hwnd_slot.store(0, Ordering::Release);
    Ok(())
}

fn create_icon_data(hwnd: HWND) -> Result<NOTIFYICONDATAW> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: windows_integration::application_icon()?,
        ..Default::default()
    };
    copy_wide("A 股新股申购提醒", &mut data.szTip);
    Ok(data)
}

fn copy_wide(value: &str, target: &mut [u16]) {
    let source: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let count = source.len().min(target.len());
    target[..count].copy_from_slice(&source[..count]);
    if count == target.len() {
        target[target.len() - 1] = 0;
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        message if message != 0 && message == ACTIVATE_INSTANCE_MESSAGE.load(Ordering::Acquire) => {
            if let Some(callbacks) = CALLBACKS.get() {
                (callbacks.activate)();
            }
            LRESULT(0)
        }
        message if message != 0 && message == TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire) => {
            match create_icon_data(hwnd) {
                Ok(icon_data) => {
                    if unsafe { Shell_NotifyIconW(NIM_ADD, &icon_data) }.as_bool() {
                        TASKBAR_READD_SUCCEEDED.fetch_add(1, Ordering::AcqRel);
                        crate::operations::log(
                            "INFO",
                            "检测到 Explorer 任务栏重建，托盘图标已重新注册",
                        );
                    } else {
                        TASKBAR_READD_FAILED.fetch_add(1, Ordering::AcqRel);
                        crate::operations::log(
                            "ERROR",
                            "检测到 Explorer 任务栏重建，但托盘图标重新注册失败",
                        );
                    }
                }
                Err(error) => crate::operations::log(
                    "ERROR",
                    &format!("Explorer 重启后创建托盘图标数据失败：{error:#}"),
                ),
            }
            LRESULT(0)
        }
        TRAY_MESSAGE if lparam.0 as u32 == WM_LBUTTONDBLCLK => {
            if let Some(callbacks) = CALLBACKS.get() {
                (callbacks.show)();
            }
            LRESULT(0)
        }
        TRAY_MESSAGE if lparam.0 as u32 == NIN_BALLOONUSERCLICK => {
            if let Some(callbacks) = CALLBACKS.get() {
                let event_id = LAST_NOTIFICATION_EVENT
                    .get_or_init(|| Mutex::new(None))
                    .lock()
                    .ok()
                    .and_then(|target| target.clone());
                (callbacks.notification)(event_id);
            }
            LRESULT(0)
        }
        TRAY_MESSAGE if lparam.0 as u32 == WM_RBUTTONUP => {
            unsafe { show_menu(hwnd) };
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                SHOW_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.show)();
                    }
                }
                TODAY_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.today)();
                    }
                }
                FUTURE_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.future)();
                    }
                }
                LOGS_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.logs)();
                    }
                }
                SYNC_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.sync)();
                    }
                }
                SETTINGS_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.settings)();
                    }
                }
                EXIT_COMMAND => {
                    if let Some(callbacks) = CALLBACKS.get() {
                        (callbacks.exit)();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST
            if matches!(
                wparam.0 as u32,
                PBT_APMRESUMEAUTOMATIC
                    | PBT_APMRESUMECRITICAL
                    | PBT_APMRESUMESTANDBY
                    | PBT_APMRESUMESUSPEND
            ) =>
        {
            RECOVERY_POWER_MESSAGES.fetch_add(1, Ordering::AcqRel);
            dispatch_recovery();
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => {
            RECOVERY_UNLOCK_MESSAGES.fetch_add(1, Ordering::AcqRel);
            dispatch_recovery();
            LRESULT(0)
        }
        WM_TIMECHANGE => {
            RECOVERY_TIME_MESSAGES.fetch_add(1, Ordering::AcqRel);
            dispatch_recovery();
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn dispatch_recovery() {
    let now = Instant::now();
    let gate = LAST_RECOVERY.get_or_init(|| Mutex::new(None));
    let Ok(mut last) = gate.lock() else { return };
    if last.is_some_and(|previous| now.duration_since(previous) < Duration::from_secs(5)) {
        RECOVERY_SUPPRESSED.fetch_add(1, Ordering::AcqRel);
        return;
    }
    RECOVERY_ACCEPTED.fetch_add(1, Ordering::AcqRel);
    *last = Some(now);
    drop(last);
    if let Some(callbacks) = CALLBACKS.get() {
        (callbacks.recovery)();
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, SHOW_COMMAND, w!("打开主窗口"));
        let _ = AppendMenuW(menu, MF_STRING, TODAY_COMMAND, w!("今日任务"));
        let _ = AppendMenuW(menu, MF_STRING, FUTURE_COMMAND, w!("未来 60 天"));
        let _ = AppendMenuW(menu, MF_STRING, LOGS_COMMAND, w!("打开日志目录"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, w!(""));
        let _ = AppendMenuW(menu, MF_STRING, SYNC_COMMAND, w!("立即同步"));
        let _ = AppendMenuW(menu, MF_STRING, SETTINGS_COMMAND, w!("提醒设置"));
        let _ = AppendMenuW(menu, MF_STRING, EXIT_COMMAND, w!("安全退出"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

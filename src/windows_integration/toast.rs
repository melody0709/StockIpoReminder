use super::*;

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
pub(crate) fn ensure_process_app_identity() -> Result<()> {
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
pub(crate) fn ensure_winrt_for_current_thread() -> Result<()> {
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
pub(crate) fn common_start_menu_shortcut_registration() -> Result<(bool, bool)> {
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
pub(crate) fn notification_setting_name(setting: NotificationSetting) -> &'static str {
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
pub(crate) fn user_notification_state_name(
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

pub(crate) fn toast_xml(title: &str, body: &str, event_id: Option<&str>) -> String {
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

pub(crate) fn truncate_chars(value: &str, maximum: usize) -> String {
    let mut characters = value.chars();
    let prefix: String = characters.by_ref().take(maximum).collect();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

pub(crate) fn xml_escape(value: &str) -> String {
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

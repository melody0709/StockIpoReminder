use super::*;

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

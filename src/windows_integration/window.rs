use super::*;

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
        // 以原生客户区为准：程序化改尺寸后 Slint 的 window.size() 可能仍是一帧之前的旧值，
        // 用它做钳制/居中会让窗口底边落到工作区之外，而屏幕外那一段永远不会被绘制。
        let current_size = slint::PhysicalSize::new(client_width as u32, client_height as u32);
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

/// Slint 缓存的窗口尺寸是否已经追上原生客户区。
///
/// 程序化改尺寸后，原生窗口和 Slint 会分几步才对齐；在对齐之前提交的重绘只覆盖旧区域，
/// 屏幕外或新增的那一段会停在未绘制状态，表现为窗口底部直接透出桌面，只有再次改尺寸才会恢复。
pub fn window_size_settled(window: &slint::Window) -> bool {
    #[cfg(windows)]
    {
        let Some(client) = window_client_size(window) else {
            return false;
        };
        let cached = window.size();
        client.width.abs_diff(cached.width) <= 1 && client.height.abs_diff(cached.height) <= 1
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        true
    }
}

/// 原生客户区尺寸（物理像素），读取失败时返回 `None`。
pub fn window_client_size(window: &slint::Window) -> Option<slint::PhysicalSize> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().ok()?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            return None;
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let mut client_rect = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut client_rect) }.is_err() {
            return None;
        }
        Some(slint::PhysicalSize::new(
            (client_rect.right - client_rect.left).max(0) as u32,
            (client_rect.bottom - client_rect.top).max(0) as u32,
        ))
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
    }
}

/// 窗口自身 DPI 与所在显示器 DPI 不一致时返回 `(窗口 DPI, 显示器 DPI)`。
///
/// 常驻托盘的应用在隐藏期间遇到显示缩放变化时，窗口缓存的 DPI 不会刷新，
/// 于是 UI 会一直按旧缩放渲染，直到窗口被系统重新计算 DPI 为止。
pub fn window_dpi_mismatch(window: &slint::Window) -> Option<(u32, u32)> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = window.window_handle();
        let raw = handle.window_handle().ok()?;
        let RawWindowHandle::Win32(raw) = raw.as_raw() else {
            return None;
        };
        let hwnd = HWND(raw.hwnd.get() as *mut _);
        let window_dpi = unsafe { GetDpiForWindow(hwnd) };
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut monitor_dpi_x = 0u32;
        let mut monitor_dpi_y = 0u32;
        unsafe {
            GetDpiForMonitor(
                monitor,
                MDT_EFFECTIVE_DPI,
                &mut monitor_dpi_x,
                &mut monitor_dpi_y,
            )
        }
        .ok()?;
        (window_dpi.abs_diff(monitor_dpi_x) >= 2).then_some((window_dpi, monitor_dpi_x))
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        None
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

pub(crate) fn clamp_window_dimension(current: u32, minimum: u32, available: u32) -> u32 {
    current.min(available).max(minimum.min(available))
}

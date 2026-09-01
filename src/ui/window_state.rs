use std::{
    cell::RefCell,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, LogicalSize, PhysicalSize, Timer, TimerMode};

use crate::{MainWindow, operations};

const WINDOW_STATE_FILE: &str = "window-state.json";
const WINDOW_STATE_SCHEMA_VERSION: u32 = 1;
const MIN_LOGICAL_WIDTH: u32 = 800;
const MIN_LOGICAL_HEIGHT: u32 = 500;
const MAX_LOGICAL_DIMENSION: u32 = 10_000;
const OBSERVATION_INTERVAL: Duration = Duration::from_millis(500);
const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
static PENDING_MAIN_WINDOW_SIZE: Mutex<Option<MainWindowSize>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MainWindowSize {
    width: u32,
    height: u32,
}

impl MainWindowSize {
    fn new(width: u32, height: u32) -> Result<Self> {
        if !(MIN_LOGICAL_WIDTH..=MAX_LOGICAL_DIMENSION).contains(&width)
            || !(MIN_LOGICAL_HEIGHT..=MAX_LOGICAL_DIMENSION).contains(&height)
        {
            bail!("主窗口尺寸超出允许范围：{width}x{height}");
        }
        Ok(Self { width, height })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredWindowState {
    schema_version: u32,
    width: u32,
    height: u32,
}

struct WindowSizeTracker {
    observed: Option<MainWindowSize>,
    observed_since: Instant,
    saved: Option<MainWindowSize>,
}

pub(crate) fn prepare_main_window_size_restore(data_root: &Path) -> Result<Option<MainWindowSize>> {
    let size = read_main_window_size(data_root)?;
    if let Ok(mut pending) = PENDING_MAIN_WINDOW_SIZE.lock() {
        *pending = size;
    }
    Ok(size)
}

pub(crate) fn apply_restored_main_window_size(window: &MainWindow) {
    let size = PENDING_MAIN_WINDOW_SIZE
        .lock()
        .ok()
        .and_then(|mut pending| pending.take());
    if let Some(size) = size {
        window
            .window()
            .set_size(LogicalSize::new(size.width as f32, size.height as f32));
        let weak = window.as_weak();
        Timer::single_shot(Duration::from_millis(150), move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            match logical_size_from_physical(window.window().size(), window.window().scale_factor())
            {
                Some(actual) => operations::log(
                    "INFO",
                    &format!(
                        "主窗口尺寸已恢复：logicalWidth={} logicalHeight={} requestedWidth={} requestedHeight={} event=main_window_size_restored",
                        actual.width, actual.height, size.width, size.height
                    ),
                ),
                None => operations::log("WARN", "主窗口尺寸恢复后无法读取有效逻辑尺寸"),
            }
        });
    }
}

pub(crate) fn persist_main_window_size(window: &MainWindow, data_root: &Path) {
    if let Err(error) = save_main_window_size(window, data_root) {
        operations::log("WARN", &format!("保存主窗口尺寸失败：{error:#}"));
    }
}

pub(crate) fn start_main_window_size_persistence(
    window: slint::Weak<MainWindow>,
    data_root: PathBuf,
    restored: Option<MainWindowSize>,
) -> Timer {
    let tracker = Rc::new(RefCell::new(WindowSizeTracker {
        observed: restored,
        observed_since: Instant::now(),
        saved: restored,
    }));
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, OBSERVATION_INTERVAL, move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        if !window.window().is_visible() {
            return;
        }
        let Some(size) = capture_main_window_size(window.window()) else {
            return;
        };

        let now = Instant::now();
        let mut tracker = tracker.borrow_mut();
        if tracker.observed != Some(size) {
            tracker.observed = Some(size);
            tracker.observed_since = now;
            return;
        }
        if tracker.saved == Some(size)
            || now.saturating_duration_since(tracker.observed_since) < SAVE_DEBOUNCE
        {
            return;
        }

        match write_main_window_size(&data_root, size) {
            Ok(()) => tracker.saved = Some(size),
            Err(error) => {
                // 延后下一次重试，避免每 500ms 重复写盘和刷日志；
                // 隐藏窗口或安全退出时仍会立即再尝试同步保存。
                tracker.observed_since = now;
                operations::log("WARN", &format!("后台保存主窗口尺寸失败：{error:#}"));
            }
        }
    });
    timer
}

fn save_main_window_size(window: &MainWindow, data_root: &Path) -> Result<()> {
    let Some(size) = capture_main_window_size(window.window()) else {
        return Ok(());
    };
    write_main_window_size(data_root, size)
}

fn capture_main_window_size(window: &slint::Window) -> Option<MainWindowSize> {
    if window.is_maximized() || window.is_minimized() || window.is_fullscreen() {
        return None;
    }
    logical_size_from_physical(window.size(), window.scale_factor())
}

fn logical_size_from_physical(size: PhysicalSize, scale_factor: f32) -> Option<MainWindowSize> {
    if !scale_factor.is_finite() || scale_factor <= 0.0 || size.width == 0 || size.height == 0 {
        return None;
    }
    let logical = size.to_logical(scale_factor);
    MainWindowSize::new(logical.width.round() as u32, logical.height.round() as u32).ok()
}

fn read_main_window_size(data_root: &Path) -> Result<Option<MainWindowSize>> {
    let path = data_root.join(WINDOW_STATE_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("无法读取主窗口状态文件"),
    };
    let state: StoredWindowState =
        serde_json::from_slice(&bytes).context("主窗口状态 JSON 已损坏")?;
    if state.schema_version != WINDOW_STATE_SCHEMA_VERSION {
        bail!("不支持的主窗口状态版本：{}", state.schema_version);
    }
    MainWindowSize::new(state.width, state.height).map(Some)
}

fn write_main_window_size(data_root: &Path, size: MainWindowSize) -> Result<()> {
    let path = data_root.join(WINDOW_STATE_FILE);
    let state = StoredWindowState {
        schema_version: WINDOW_STATE_SCHEMA_VERSION,
        width: size.width,
        height: size.height,
    };
    let mut bytes = serde_json::to_vec_pretty(&state)?;
    bytes.push(b'\n');
    if fs::read(&path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }

    let temporary = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().simple()));
    fs::write(&temporary, bytes).context("无法写入临时主窗口状态文件")?;
    if let Err(error) = operations::atomic_replace_file(&temporary, &path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("无法提交主窗口状态文件");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_window_size_is_dpi_independent() {
        assert_eq!(
            logical_size_from_physical(PhysicalSize::new(1770, 1170), 1.5),
            Some(MainWindowSize {
                width: 1180,
                height: 780,
            })
        );
    }

    #[test]
    fn invalid_or_unsafe_window_sizes_are_rejected() {
        assert!(MainWindowSize::new(799, 780).is_err());
        assert!(MainWindowSize::new(1180, 499).is_err());
        assert!(MainWindowSize::new(10_001, 780).is_err());
        assert_eq!(
            logical_size_from_physical(PhysicalSize::new(1180, 780), 0.0),
            None
        );
    }

    #[test]
    fn window_state_round_trips_through_atomic_file() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-window-state-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        let size = MainWindowSize::new(1420, 910).unwrap();

        write_main_window_size(&root, size).unwrap();
        assert_eq!(read_main_window_size(&root).unwrap(), Some(size));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_window_state_schema_is_reported() {
        let root = std::env::temp_dir().join(format!(
            "stock-ipo-window-state-schema-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(WINDOW_STATE_FILE),
            br#"{"schemaVersion":99,"width":1180,"height":780}"#,
        )
        .unwrap();

        assert!(read_main_window_size(&root).is_err());

        fs::remove_dir_all(root).unwrap();
    }
}

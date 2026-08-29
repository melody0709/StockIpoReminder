use super::*;

/// UI 发起的后台工作线程数量；退出时等待其收尾，避免数据库写入被硬中断。
static UI_WORKER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Drop 时机覆盖正常结束与 panic 展开，保证计数在两条路径下都会归还。
pub(crate) struct UiWorkerGuard;

impl Drop for UiWorkerGuard {
    fn drop(&mut self) {
        UI_WORKER_COUNT.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(crate) fn spawn_ui_worker(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> Result<(), std::io::Error> {
    UI_WORKER_COUNT.fetch_add(1, Ordering::AcqRel);
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _worker_guard = UiWorkerGuard;
            body();
        });
    if spawned.is_err() {
        UI_WORKER_COUNT.fetch_sub(1, Ordering::AcqRel);
    }
    spawned.map(|_| ())
}

pub(crate) fn wait_for_ui_workers() {
    while UI_WORKER_COUNT.load(Ordering::Acquire) > 0 {
        std::thread::sleep(Duration::from_millis(20));
    }
}

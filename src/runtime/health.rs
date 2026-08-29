use super::*;

pub(crate) fn check_clock(
    client: &reqwest::blocking::Client,
    reason: &str,
    stop_requested: Option<&AtomicBool>,
) -> (HealthState, String) {
    let windows_time = windows_integration::windows_time_service_running();
    let endpoints = ["https://www.microsoft.com/", "https://www.cloudflare.com/"];
    let mut offsets = Vec::new();
    for endpoint in endpoints {
        if stop_requested.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            break;
        }
        let start = Utc::now();
        let url = format!("{endpoint}?clock_probe={}", start.timestamp_millis());
        let response = client
            .get(url)
            .header(reqwest::header::CACHE_CONTROL, "no-cache, no-store")
            .send();
        let end = Utc::now();
        let Ok(response) = response else { continue };
        let Some(raw) = response
            .headers()
            .get(reqwest::header::DATE)
            .and_then(|value| value.to_str().ok())
        else {
            continue;
        };
        let Ok(server) = DateTime::parse_from_rfc2822(raw) else {
            continue;
        };
        let midpoint = start + (end - start) / 2;
        offsets.push((server.with_timezone(&Utc) - midpoint).num_milliseconds());
    }
    evaluate_clock_offsets(offsets, reason, windows_time)
}

pub(crate) fn evaluate_clock_offsets(
    mut offsets: Vec<i64>,
    reason: &str,
    windows_time: Result<Option<bool>>,
) -> (HealthState, String) {
    offsets.sort_unstable();
    if offsets.is_empty() {
        return add_windows_time_status(
            HealthState::Unknown,
            format!("无法取得独立网络时间样本（0/2，{reason}），未据此修改任务状态"),
            windows_time,
        );
    }
    let offset = if offsets.len() % 2 == 1 {
        offsets[offsets.len() / 2]
    } else {
        (offsets[offsets.len() / 2 - 1] + offsets[offsets.len() / 2]) / 2
    };
    let absolute = offset.unsigned_abs();
    let state = if offsets.len() < 2 || absolute > 2 * 60 * 1000 {
        if absolute > 5 * 60 * 1000 {
            HealthState::Failed
        } else {
            HealthState::Warning
        }
    } else {
        HealthState::Healthy
    };
    let prefix = match state {
        HealthState::Healthy => "系统时间正常",
        HealthState::Warning if offsets.len() < 2 => "系统时间样本不足",
        HealthState::Warning => "系统时间可能有偏差",
        HealthState::Failed => "系统时间偏差过大",
        _ => "系统时间状态未知",
    };
    add_windows_time_status(
        state,
        format!(
            "{prefix}：估算偏差 {:+.0} 秒，有效样本 {}/2（{reason}）",
            offset as f64 / 1000.0,
            offsets.len()
        ),
        windows_time,
    )
}

pub(crate) fn add_windows_time_status(
    state: HealthState,
    text: String,
    service_status: Result<Option<bool>>,
) -> (HealthState, String) {
    match service_status {
        Ok(Some(true)) => (state, format!("{text}；Windows Time 服务正在运行")),
        Ok(Some(false)) => (
            if state == HealthState::Failed {
                state
            } else {
                HealthState::Warning
            },
            format!("{text}；Windows Time 服务未运行，请检查 W32Time 配置"),
        ),
        Ok(None) => (state, text),
        Err(error) => (
            if state == HealthState::Failed {
                state
            } else {
                HealthState::Warning
            },
            format!(
                "{text}；Windows Time 服务状态读取失败：{}",
                operations::redact(&format!("{error:#}"))
            ),
        ),
    }
}

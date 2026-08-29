use super::*;

pub fn client() -> Result<Client> {
    build_client(business_redirect_allowed)
}

pub fn time_client() -> Result<Client> {
    build_client(time_redirect_allowed)
}

pub(crate) fn build_client(redirect_allowed: fn(&url::Url) -> bool) -> Result<Client> {
    Ok(ClientBuilder::new()
        .timeout(Duration::from_secs(45))
        .connect_timeout(Duration::from_secs(10))
        .redirect(Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("重定向次数超过上限")
            } else if redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("重定向目标不在 HTTPS 白名单内")
            }
        }))
        .cookie_store(true)
        .user_agent(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) StockIpoReminder-Rust/",
            env!("CARGO_PKG_VERSION")
        ))
        .brotli(true)
        .gzip(true)
        .deflate(true)
        .build()?)
}

pub fn probe_source(client: &Client, source: &str) -> Result<()> {
    let url = source_probe_url(source).context("未知数据源，无法执行低频健康探测")?;
    ensure_allowed(url, false)?;
    let response = client
        .get(url)
        .header(RANGE, "bytes=0-0")
        .timeout(Duration::from_secs(10))
        .send()?;
    let _ = checked_response(response, false)?;
    Ok(())
}

pub(crate) fn source_probe_url(source: &str) -> Option<&'static str> {
    match source {
        "eastmoney" => Some("https://www.eastmoney.com/"),
        "sse" | "sse-announcement" => Some("https://www.sse.com.cn/"),
        "cninfo" | "cninfo-announcement" => Some("https://www.cninfo.com.cn/"),
        "bse" | "bse-announcement" => Some("https://www.bseinfo.net/newshare/listofissues.html"),
        _ => None,
    }
}

pub fn get_text(
    client: &Client,
    url: &str,
    referer: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    ensure_allowed(url, false)?;
    let mut request = client.get(url);
    if let Some(referer) = referer {
        request = request.header("Referer", referer);
    }
    response_text(request.send()?, false, cancelled)
}

pub fn checked_response(response: Response, announcement: bool) -> Result<Response> {
    ensure_allowed(response.url().as_str(), announcement)?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let host = response.url().host_str().unwrap_or_default().to_owned();
    let retry_after = if matches!(status, 429 | 503) {
        parse_retry_after(response.headers().get(RETRY_AFTER), now_china())
    } else {
        None
    };
    Err(HttpStatusError {
        status,
        host,
        retry_after,
    }
    .into())
}

pub fn retry_after_from_error(error: &anyhow::Error) -> Option<ChinaDateTime> {
    error
        .downcast_ref::<HttpStatusError>()
        .and_then(|failure| failure.retry_after)
}

pub(crate) fn response_text(
    response: Response,
    announcement: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    let mut response = checked_response(response, announcement)?;
    let limit = if announcement {
        MAX_ANNOUNCEMENT_RESPONSE_BYTES
    } else {
        MAX_DATA_RESPONSE_BYTES
    };
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        bail!("远端响应超过 {} MiB 大小上限", limit / 1024 / 1024);
    }
    let charset = declared_charset(response.headers());
    let bytes = read_limited(&mut response, limit, Some(cancelled))?;
    decode_response_bytes(&bytes, charset)
}

/// 从 Content-Type 提取 charset 参数（小写化，去掉引号）。
pub(crate) fn declared_charset(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|content_type| {
            content_type.split(';').skip(1).find_map(|parameter| {
                let parameter = parameter.trim();
                let (name, value) = parameter.split_once('=')?;
                name.trim()
                    .eq_ignore_ascii_case("charset")
                    .then(|| value.trim().trim_matches('"').to_ascii_lowercase())
            })
        })
}

/// 按声明字符集解码响应体。仅接受 UTF-8 与显式声明的 GBK/GB2312/GB18030；
/// 对无声明的非法 UTF-8 不做猜测式兜底，只报错并附有限长度 hex 诊断。
pub(crate) fn decode_response_bytes(bytes: &[u8], charset: Option<String>) -> Result<String> {
    const HEX_PREVIEW_BYTES: usize = 48;
    let hex_preview = |data: &[u8]| -> String {
        data.iter()
            .take(HEX_PREVIEW_BYTES)
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let declared = charset.as_deref().unwrap_or_default();
    if matches!(declared, "gbk" | "gb2312" | "gb18030") {
        // GB18030 是 GBK/GB2312 的官方超集，统一按 GB18030 解码。
        let (decoded, _, had_errors) = encoding_rs::GB18030.decode(bytes);
        if had_errors {
            bail!(
                "远端响应按 GB18030 解码存在无法映射的字节，前 {HEX_PREVIEW_BYTES} 字节 hex：{}",
                hex_preview(bytes)
            );
        }
        Ok(decoded.into_owned())
    } else {
        match String::from_utf8(bytes.to_vec()) {
            Ok(text) => Ok(text),
            Err(error) => {
                let bytes = error.into_bytes();
                bail!(
                    "远端响应不是有效 UTF-8（charset 声明：{}），前 {HEX_PREVIEW_BYTES} 字节 hex：{}",
                    if declared.is_empty() {
                        "未声明"
                    } else {
                        declared
                    },
                    hex_preview(&bytes)
                );
            }
        }
    }
}

/// 分块读取响应体：每次成功 read 后检查取消标志，退出请求不再需要等待
/// 整段响应读完。逐次 read 的 stall 超时与体积上限保持不变。
pub(crate) fn read_limited(
    reader: &mut impl Read,
    limit: u64,
    cancelled: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    const CHUNK_SIZE: usize = 64 * 1024;
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; CHUNK_SIZE];
    loop {
        if cancelled.is_some_and(|check| check()) {
            bail!("同步已取消：读取响应体时检测到退出请求");
        }
        let read = reader.read(&mut chunk).context("无法读取远端响应")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() as u64 > limit {
            bail!("远端响应超过 {} MiB 大小上限", limit / 1024 / 1024);
        }
    }
    Ok(bytes)
}

pub(crate) fn parse_retry_after(
    value: Option<&reqwest::header::HeaderValue>,
    now: ChinaDateTime,
) -> Option<ChinaDateTime> {
    let value = value?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<i64>() {
        if !(0..=MAX_RETRY_AFTER_SECONDS).contains(&seconds) {
            return None;
        }
        return chrono::Duration::try_seconds(seconds)
            .and_then(|delay| now.checked_add_signed(delay));
    }
    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|value| value.with_timezone(&china_offset()))?;
    let maximum = now.checked_add_signed(chrono::Duration::seconds(MAX_RETRY_AFTER_SECONDS))?;
    (retry_at >= now && retry_at <= maximum).then_some(retry_at)
}
pub fn ensure_allowed(value: &str, announcement: bool) -> Result<()> {
    let url = url::Url::parse(value)?;
    let allowed = if announcement {
        ANNOUNCEMENT_HOSTS
    } else {
        DATA_HOSTS
    };
    if url.scheme() != "https" || !allowed.contains(&url.host_str().unwrap_or_default()) {
        bail!(
            "拒绝访问白名单外地址：{}",
            url.host_str().unwrap_or_default()
        )
    }
    Ok(())
}
pub(crate) const DATA_HOSTS: &[&str] = &[
    "www.eastmoney.com",
    "datacenter-web.eastmoney.com",
    "query.sse.com.cn",
    "www.sse.com.cn",
    "www.cninfo.com.cn",
    "static.cninfo.com.cn",
    "disc.static.szse.cn",
    "www.bseinfo.net",
    "www.bse.cn",
];
pub(crate) const TIME_HOSTS: &[&str] = &["www.microsoft.com", "www.cloudflare.com"];
pub(crate) const ANNOUNCEMENT_HOSTS: &[&str] = &[
    "query.sse.com.cn",
    "www.sse.com.cn",
    "static.sse.com.cn",
    "www.cninfo.com.cn",
    "static.cninfo.com.cn",
    "disc.static.szse.cn",
    "www.bseinfo.net",
    "bseinfo.net",
    "www.bse.cn",
    "bse.cn",
];

pub(crate) fn business_redirect_allowed(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| DATA_HOSTS.contains(&host) || ANNOUNCEMENT_HOSTS.contains(&host))
}

pub(crate) fn time_redirect_allowed(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| TIME_HOSTS.contains(&host))
}

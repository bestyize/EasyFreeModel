//! 出站 HTTP 客户端。
//!
//! 红线：
//! - `User-Agent` 固定为 `RouterDesktop/<版本>`，调用方无法覆盖。
//! - 不禁用 TLS 证书校验，不跟随重定向（避免把自定义密钥头带到其它主机）。
//! - 错误只暴露分类，不携带 URL / 请求头 / 正文。

use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use url::Url;

use super::UpstreamError;

pub const USER_AGENT_PREFIX: &str = "RouterDesktop";

pub fn user_agent() -> String {
    format!("{USER_AGENT_PREFIX}/{}", env!("CARGO_PKG_VERSION"))
}

#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub connect_timeout: Duration,
    /// 忽略系统 / 环境变量代理（测试访问回环地址时使用）。
    pub no_proxy: bool,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self { connect_timeout: Duration::from_secs(15), no_proxy: false }
    }
}

#[derive(Clone)]
pub struct UpstreamClient {
    http: reqwest::Client,
}

impl std::fmt::Debug for UpstreamClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UpstreamClient")
    }
}

/// 把 reqwest 错误归类成不含 URL 的 [`UpstreamError`]。
pub(crate) fn classify_error(e: &reqwest::Error) -> UpstreamError {
    if e.is_timeout() {
        UpstreamError::Timeout
    } else if e.is_connect() {
        UpstreamError::Connect
    } else if e.is_body() || e.is_decode() {
        UpstreamError::Network("the connection was closed or reset while receiving the response".into())
    } else if e.is_redirect() {
        UpstreamError::Network("the upstream attempted a redirect, which is not followed".into())
    } else {
        UpstreamError::Network("the request could not be completed".into())
    }
}

impl UpstreamClient {
    pub fn new() -> Result<Self, UpstreamError> {
        Self::with_options(ClientOptions::default())
    }

    pub fn with_options(opts: ClientOptions) -> Result<Self, UpstreamError> {
        let mut b = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(opts.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(Duration::from_secs(60));
        if opts.no_proxy {
            b = b.no_proxy();
        }
        let http = b.build().map_err(|_| UpstreamError::Config("failed to build the HTTP client".into()))?;
        Ok(Self { http })
    }

    pub(crate) async fn post_json(
        &self,
        url: Url,
        mut headers: HeaderMap,
        body: Bytes,
        total_timeout: Option<Duration>,
        header_timeout: Duration,
    ) -> Result<reqwest::Response, UpstreamError> {
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent()).expect("static user agent"));
        let mut rb = self.http.post(url).headers(headers).body(body);
        if let Some(t) = total_timeout {
            rb = rb.timeout(t);
        }
        match tokio::time::timeout(header_timeout, rb.send()).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(classify_error(&e)),
            Err(_) => Err(UpstreamError::Timeout),
        }
    }

    pub(crate) async fn get(&self, url: Url, mut headers: HeaderMap, timeout: Duration) -> Result<reqwest::Response, UpstreamError> {
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent()).expect("static user agent"));
        self.http.get(url).headers(headers).timeout(timeout).send().await.map_err(|e| classify_error(&e))
    }
}

/// 读取响应体。`truncate = true` 时超过 `limit` 的部分被丢弃（用于错误响应）；否则超限返回错误。
pub(crate) async fn read_body(resp: reqwest::Response, limit: usize, truncate: bool) -> Result<Bytes, UpstreamError> {
    let mut stream = resp.bytes_stream();
    let mut buf = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| classify_error(&e))?;
        if buf.len() + chunk.len() > limit {
            if truncate {
                let room = limit - buf.len();
                buf.extend_from_slice(&chunk[..room]);
                break;
            }
            return Err(UpstreamError::Malformed("the upstream response is too large".into()));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

/// `Retry-After`：整数秒或 HTTP-date。
pub(crate) fn parse_retry_after(v: &HeaderValue) -> Option<Duration> {
    let s = v.to_str().ok()?.trim();
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = chrono::DateTime::parse_from_rfc2822(s).ok()?;
    let delta = when.with_timezone(&chrono::Utc) - chrono::Utc::now();
    Some(Duration::from_secs(delta.num_seconds().max(0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_fixed() {
        assert_eq!(user_agent(), format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn retry_after_forms() {
        assert_eq!(parse_retry_after(&HeaderValue::from_static("7")), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after(&HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT")), Some(Duration::ZERO));
        assert_eq!(parse_retry_after(&HeaderValue::from_static("soon")), None);
        let future = (chrono::Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
        let d = parse_retry_after(&HeaderValue::from_str(&future).unwrap()).unwrap();
        assert!(d.as_secs() >= 88 && d.as_secs() <= 90, "{d:?}");
    }
}

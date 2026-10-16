//! Shared per-request routing. Only connection failures before a response, and
//! proxy authentication rejection, may move a replayable request to another node.
use std::time::Duration;

use reqwest::{RequestBuilder, Response, Url};

use super::{ProxyPool, Route, GLOBAL};

pub fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback()),
        None => false,
    }
}

#[derive(thiserror::Error)]
pub enum SendError {
    #[error("代理池已开启但没有可用节点，已阻止直连")]
    Blocked,
    #[error("代理认证失败（HTTP 407），请检查代理用户名和密码")]
    ProxyAuth,
    #[error("网络请求超时")]
    Timeout,
    // Never display reqwest's URL: it may contain credentials or signed queries.
    #[error("网络请求失败")]
    Network(#[from] reqwest::Error),
}

impl std::fmt::Debug for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl SendError {
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout) || matches!(self, Self::Network(e) if e.is_timeout())
    }
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Blocked | Self::ProxyAuth) || matches!(self, Self::Network(e) if e.is_connect())
    }
}

pub async fn send(provider: &str, request: RequestBuilder, timeout: Duration) -> Result<Response, SendError> {
    send_with_pool(GLOBAL.get().map(|pool| pool.as_ref()), provider, request, timeout).await
}

pub(super) async fn send_with_pool(
    pool: Option<&ProxyPool>, provider: &str, builder: RequestBuilder, timeout: Duration,
) -> Result<Response, SendError> {
    let (direct, request) = builder.build_split();
    let request = request?;
    let routes = match pool {
        Some(pool) => pool.plan_for_url(provider, request.url()),
        None => vec![Route::Direct],
    };
    if routes.is_empty() { return Err(SendError::Blocked); }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut original = Some(request);
    let mut last = SendError::Blocked;
    for route in routes {
        let Some(source) = original.as_ref() else { break };
        let mut request = match source.try_clone() {
            Some(request) => request,
            // Streaming multipart bodies cannot be cloned: send at most once.
            None => original.take().expect("request is present"),
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { return Err(SendError::Timeout); }
        let (client, node) = match route {
            Route::Direct => (direct.clone(), None),
            Route::Proxy { node_id, client } => {
                let limit = request.timeout().copied().unwrap_or(remaining).min(remaining);
                *request.timeout_mut() = Some(limit);
                (client, Some(node_id))
            }
        };
        let result = tokio::time::timeout(remaining, client.execute(request)).await;
        match result {
            Ok(Ok(response)) if node.is_some() && response.status().as_u16() == 407 => {
                if let (Some(pool), Some(id)) = (pool, node.as_deref()) {
                    pool.report_failure(id, "代理认证失败（HTTP 407）");
                }
                last = SendError::ProxyAuth;
            }
            Ok(Ok(response)) => {
                if let (Some(pool), Some(id)) = (pool, node.as_deref()) { pool.report_success(id); }
                return Ok(response);
            }
            Ok(Err(error)) if node.is_some() && error.is_connect() => {
                if let (Some(pool), Some(id)) = (pool, node.as_deref()) {
                    pool.report_failure(id, "无法连接到代理或上游");
                }
                last = SendError::Network(error);
            }
            Ok(Err(error)) => return Err(SendError::Network(error)),
            Err(_) => return Err(SendError::Timeout),
        }
    }
    Err(last)
}

#[async_trait::async_trait]
pub trait RoutedRequest {
    async fn send_routed(self, provider: &str, timeout: Duration) -> Result<Response, SendError>;
}

#[async_trait::async_trait]
impl RoutedRequest for RequestBuilder {
    async fn send_routed(self, provider: &str, timeout: Duration) -> Result<Response, SendError> {
        send(provider, self, timeout).await
    }
}

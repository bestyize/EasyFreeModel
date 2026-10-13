//! 回环回调服务器。
//!
//! - 只绑定 127.0.0.1（随机端口，或配置的固定端口）。
//! - 只接受一次回调：收到第一个匹配路径的请求后即给出结果，后续请求一律拒绝。
//! - 在服务器内校验 state（常量时间比较），不匹配则整次授权作废。
//! - 校验 Host 头，抵御 DNS 重绑定。
//! - 响应页面是固定文本，不回显任何查询参数。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::error::{sanitize_code, sanitize_text, OAuthError};
use super::secret::Secret;

#[derive(Debug)]
pub enum CallbackOutcome {
    Code(Secret),
    ProviderError { code: String, description: Option<String> },
    StateMismatch,
    MissingCode,
}

struct Shared {
    path: String,
    expected_state: String,
    allowed_hosts: [String; 2],
    tx: Mutex<Option<oneshot::Sender<CallbackOutcome>>>,
}

pub struct LoopbackServer {
    pub port: u16,
    pub rx: oneshot::Receiver<CallbackOutcome>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    /// 绑定 127.0.0.1:`port`（0 = 随机）。端口被占用时返回 `ListenFailed`。
    pub async fn bind(port: u16, path: &str, expected_state: &str) -> Result<Self, OAuthError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|e| OAuthError::ListenFailed { port, message: e.kind().to_string() })?;
        let port = listener
            .local_addr()
            .map_err(|e| OAuthError::ListenFailed { port, message: e.kind().to_string() })?
            .port();

        let (tx, rx) = oneshot::channel();
        let shared = Arc::new(Shared {
            path: path.to_owned(),
            expected_state: expected_state.to_owned(),
            allowed_hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            tx: Mutex::new(Some(tx)),
        });
        let app = Router::new().fallback(handle).with_state(shared);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Ok(LoopbackServer { port, rx, shutdown: Some(shutdown_tx), task: Some(task) })
    }

    /// 立即停止监听并释放端口。
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let abort = task.abort_handle();
            if tokio::time::timeout(Duration::from_secs(1), task).await.is_err() {
                abort.abort();
            }
        }
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn evaluate(expected_state: &str, params: &HashMap<String, String>) -> CallbackOutcome {
    let state = params.get("state").map(String::as_str).unwrap_or("");
    let state_ok = !expected_state.is_empty() && bool::from(state.as_bytes().ct_eq(expected_state.as_bytes()));
    if !state_ok {
        return CallbackOutcome::StateMismatch;
    }
    if let Some(error) = params.get("error") {
        return CallbackOutcome::ProviderError {
            code: sanitize_code(error),
            description: params
                .get("error_description")
                .map(|d| sanitize_text(d, &[]))
                .filter(|d| !d.is_empty()),
        };
    }
    match params.get("code").filter(|c| !c.is_empty()) {
        Some(code) => CallbackOutcome::Code(Secret::new(code.as_str())),
        None => CallbackOutcome::MissingCode,
    }
}

fn page(status: StatusCode, message: &'static str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\"><title>Router Desktop</title></head>\
         <body style=\"font-family:system-ui,sans-serif;margin:3rem\"><p>{message}</p></body></html>"
    );
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'"),
    );
    response
}

async fn handle(State(shared): State<Arc<Shared>>, method: Method, headers: HeaderMap, uri: Uri) -> Response {
    if uri.path() != shared.path {
        return page(StatusCode::NOT_FOUND, "Not found.");
    }
    if method != Method::GET {
        return page(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed.");
    }
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| shared.allowed_hosts.iter().any(|allowed| allowed.eq_ignore_ascii_case(h)));
    if !host_ok {
        return page(StatusCode::BAD_REQUEST, "Unexpected host.");
    }

    let params: HashMap<String, String> = uri
        .query()
        .map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let outcome = evaluate(&shared.expected_state, &params);
    let (status, message) = match &outcome {
        CallbackOutcome::Code(_) => (StatusCode::OK, "授权完成，可以关闭此页面并返回 Router Desktop。 Authorization complete; you can close this tab."),
        CallbackOutcome::StateMismatch => (StatusCode::BAD_REQUEST, "校验失败，授权已被拒绝。 Authorization rejected (state mismatch)."),
        CallbackOutcome::ProviderError { .. } => (StatusCode::BAD_REQUEST, "授权未完成。 Authorization was not completed."),
        CallbackOutcome::MissingCode => (StatusCode::BAD_REQUEST, "回调缺少授权码。 The callback had no authorization code."),
    };
    let first = shared.tx.lock().unwrap_or_else(|e| e.into_inner()).take();
    match first {
        Some(tx) => {
            let _ = tx.send(outcome);
            page(status, message)
        }
        None => page(StatusCode::CONFLICT, "This authorization was already handled."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn evaluate_checks_state_first() {
        assert!(matches!(evaluate("s", &params(&[("code", "c"), ("state", "x")])), CallbackOutcome::StateMismatch));
        assert!(matches!(evaluate("s", &params(&[("code", "c")])), CallbackOutcome::StateMismatch));
        // 即便带 error，state 不对也先判定为不匹配
        assert!(matches!(evaluate("s", &params(&[("error", "access_denied"), ("state", "x")])), CallbackOutcome::StateMismatch));
        assert!(matches!(evaluate("", &params(&[("code", "c"), ("state", "")])), CallbackOutcome::StateMismatch));
    }

    #[test]
    fn evaluate_outcomes() {
        assert!(matches!(evaluate("s", &params(&[("code", "c"), ("state", "s")])), CallbackOutcome::Code(_)));
        assert!(matches!(evaluate("s", &params(&[("state", "s")])), CallbackOutcome::MissingCode));
        assert!(matches!(
            evaluate("s", &params(&[("error", "access_denied"), ("state", "s")])),
            CallbackOutcome::ProviderError { .. }
        ));
    }
}

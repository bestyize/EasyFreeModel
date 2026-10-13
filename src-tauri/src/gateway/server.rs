//! axum 路由、中间件栈与占位处理器。
//!
//! 中间件（由外到内）：访问日志 → 请求超时 → [路由匹配] → 认证/授权/限流 → 请求体大小检查 → 处理器。
//!
//! 入口端点：`/v1/chat/completions`、`/v1/messages`、`/v1/responses`、Gemini
//! （`/v1beta/models/{model}:generateContent`、`:streamGenerateContent`、`/v1beta/models`）、Ollama（`/api/chat`、`/api/tags`）、
//! 以及各协议共用的 `/v1/models`。
//!
//! 取消语义：处理器内部不得 `tokio::spawn` 脱离请求生命周期的任务；客户端断开时 hyper 丢弃整个请求
//! future，处理器（以及它持有的上游请求 / 流）随之被 drop，即「drop 即取消」。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, Request, State};
use axum::http::{HeaderMap, Uri};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde_json::{json, Value};

use super::auth::{self, AuthenticatedKey, KeyIdForLog};
use super::forward;
use super::rate_limit::RateLimiter;
use super::response::{error_response, error_style, gemini_method, split_gemini_target, GeminiMethod};
use super::KeyVerifier;
use crate::protocols::{ApiError, ApiErrorKind, ProtocolKind};
use crate::routing::Runtime;

/// 请求体上限默认 32 MiB（多模态图片 base64 场景）。
pub const DEFAULT_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// 单次请求（到响应头产出为止）的默认超时。流式响应体不受此限制，由客户端断开 / 上游结束决定。
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// 停止网关时等待在途请求结束的时长，超时后强制终止。
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub max_body_bytes: usize,
    pub request_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub verifier: Arc<dyn KeyVerifier>,
    pub limiter: Arc<RateLimiter>,
    pub config: GatewayConfig,
    /// 真实上游转发运行时；None 时协议端点返回 501。
    pub runtime: Option<Arc<Runtime>>,
}

impl AppState {
    pub fn new(verifier: Arc<dyn KeyVerifier>, config: GatewayConfig) -> Self {
        Self { verifier, limiter: Arc::new(RateLimiter::per_minute()), config, runtime: None }
    }
}

pub(crate) fn build_router(state: AppState) -> Router {
    build_router_with(state, Router::new())
}

/// `extra` 中的路由与内置受保护路由共享同一套认证 / 大小检查 / 超时 / 日志（测试用于挂载慢处理器等）。
pub(crate) fn build_router_with(state: AppState, extra: Router<AppState>) -> Router {
    let protected = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/responses", post(responses))
        // Gemini：模型名与方法在同一个路径段里（`gemini-2.5-flash:generateContent`），且别名可含 `/`，
        // 因此用通配符整体捕获，再按「最后一个冒号」手工拆分（见 `response::split_gemini_target`）。
        .route("/v1beta/models", get(gemini_list_models))
        .route("/v1beta/models/{*rest}", post(gemini_generate).get(gemini_get_model))
        .route("/v1/models/{*rest}", post(gemini_generate))
        .route("/api/chat", post(ollama_chat))
        .route("/api/tags", get(ollama_tags))
        .merge(extra)
        // 后加的 route_layer 在外层：先认证，再检查大小。
        .route_layer(middleware::from_fn_with_state(state.clone(), enforce_content_length))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::authenticate));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        // 对 `Bytes` / `Json` 等提取器生效，覆盖没有 Content-Length 的分块请求体。
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), with_timeout))
        .layer(middleware::from_fn(access_log))
        .with_state(state)
}

// ───────────── 处理器 ─────────────

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}))
}

/// 启用且适配器就绪的提供商的启用模型（经密钥白名单过滤）；未挂接转发运行时时为空列表。
async fn list_models(State(state): State<AppState>, Extension(key): Extension<AuthenticatedKey>, headers: HeaderMap) -> Response {
    match &state.runtime {
        Some(rt) => forward::list_models(rt, &key.0, error_style("/v1/models", &headers)).await,
        None => Json(json!({"object": "list", "data": []})).into_response(),
    }
}

/// Gemini 风格的模型列表。
async fn gemini_list_models(State(state): State<AppState>, Extension(key): Extension<AuthenticatedKey>) -> Response {
    match &state.runtime {
        Some(rt) => forward::list_models_gemini(rt, &key.0).await,
        None => Json(json!({"models": []})).into_response(),
    }
}

async fn gemini_get_model(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(rest): Path<String>,
) -> Response {
    match &state.runtime {
        Some(rt) => forward::get_model_gemini(rt, &key.0, &rest).await,
        None => adapter_missing(ProtocolKind::Gemini),
    }
}

/// Ollama 风格的模型列表。
async fn ollama_tags(State(state): State<AppState>, Extension(key): Extension<AuthenticatedKey>) -> Response {
    match &state.runtime {
        Some(rt) => forward::list_models_ollama(rt, &key.0).await,
        None => Json(json!({"models": []})).into_response(),
    }
}

async fn responses(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    proxy(state, key, ProtocolKind::OpenAiResponses, headers, body).await
}

async fn ollama_chat(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    proxy(state, key, ProtocolKind::Ollama, headers, body).await
}

/// `?alt=sse`
fn wants_sse(uri: &Uri) -> bool {
    uri.query().is_some_and(|q| url::form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "alt" && v.eq_ignore_ascii_case("sse")))
}

/// `POST /v1beta/models/{model}:generateContent` 与 `:streamGenerateContent`。
async fn gemini_generate(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    Path(rest): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let protocol = ProtocolKind::Gemini;
    let Some((model, method)) = split_gemini_target(&rest) else {
        let err = ApiError::new(ApiErrorKind::NotFound, "Expected /models/{model}:generateContent or /models/{model}:streamGenerateContent.")
            .with_code("not_found");
        return error_response(protocol, &err);
    };
    let Some(m) = gemini_method(method) else {
        let err = ApiError::new(
            ApiErrorKind::NotImplemented,
            format!("The Gemini method `{}` is not supported by this gateway (only generateContent and streamGenerateContent).", method.chars().take(64).collect::<String>()),
        )
        .with_code("method_not_supported");
        return error_response(protocol, &err);
    };
    let model = model.strip_prefix("models/").unwrap_or(model).to_string();
    let gemini = GeminiCall { model, stream: m == GeminiMethod::StreamGenerate, sse: wants_sse(&uri) };
    proxy_with(state, key, protocol, headers, body, Some(gemini)).await
}

/// Gemini URL 里携带的调用参数。
struct GeminiCall {
    model: String,
    stream: bool,
    sse: bool,
}

async fn chat_completions(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    proxy(state, key, ProtocolKind::OpenAiChat, headers, body).await
}

async fn anthropic_messages(
    State(state): State<AppState>,
    Extension(key): Extension<AuthenticatedKey>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    proxy(state, key, ProtocolKind::AnthropicMessages, headers, body).await
}

async fn proxy(
    state: AppState,
    key: AuthenticatedKey,
    protocol: ProtocolKind,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    proxy_with(state, key, protocol, headers, body, None).await
}

async fn proxy_with(
    state: AppState,
    key: AuthenticatedKey,
    protocol: ProtocolKind,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
    gemini: Option<GeminiCall>,
) -> Response {
    let Some(rt) = state.runtime.clone() else {
        return adapter_missing(protocol);
    };
    match body {
        Ok(bytes) => {
            let inbound = forward::Inbound {
                protocol,
                headers,
                body: bytes,
                path_model: gemini.as_ref().map(|g| g.model.clone()),
                path_stream: gemini.as_ref().map(|g| g.stream),
                gemini_sse: gemini.as_ref().is_some_and(|g| g.sse),
            };
            forward::proxy_inbound(rt, key.0, inbound).await
        }
        Err(rej) => {
            let too_large = rej.status() == StatusCode::PAYLOAD_TOO_LARGE;
            let err = if too_large {
                ApiError::new(ApiErrorKind::PayloadTooLarge, "Request body exceeds the size limit.").with_code("request_too_large")
            } else {
                ApiError::new(ApiErrorKind::InvalidRequest, "The request body could not be read.").with_code("invalid_body")
            };
            error_response(protocol, &err)
        }
    }
}

/// 尚无真实上游适配器：明确返回 501，绝不伪造成功响应。
fn adapter_missing(protocol: ProtocolKind) -> Response {
    let err = ApiError::new(
        ApiErrorKind::NotImplemented,
        format!("{}：该协议的上游适配器尚未接入，网关暂时无法处理该请求。", protocol.display_name()),
    )
    .with_code("upstream_adapter_not_implemented");
    error_response(protocol, &err)
}

async fn not_found(req: Request) -> Response {
    let style = error_style(req.uri().path(), req.headers());
    error_response(style, &ApiError::new(ApiErrorKind::NotFound, "Unknown route.").with_code("not_found"))
}

async fn method_not_allowed(req: Request) -> Response {
    let style = error_style(req.uri().path(), req.headers());
    error_response(style, &ApiError::new(ApiErrorKind::MethodNotAllowed, "Method not allowed.").with_code("method_not_allowed"))
}

// ───────────── 中间件 ─────────────

/// 声明的 `Content-Length` 超限时立即拒绝，不读取正文。
async fn enforce_content_length(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > state.config.max_body_bytes as u64) {
        let style = error_style(req.uri().path(), req.headers());
        let mib = state.config.max_body_bytes as f64 / (1024.0 * 1024.0);
        let err = ApiError::new(ApiErrorKind::PayloadTooLarge, format!("Request body exceeds the {mib:.0} MiB limit."))
            .with_code("request_too_large");
        return error_response(style, &err);
    }
    next.run(req).await
}

async fn with_timeout(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let style = error_style(req.uri().path(), req.headers());
    match tokio::time::timeout(state.config.request_timeout, next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => {
            let err = ApiError::new(ApiErrorKind::Timeout, "The gateway timed out while processing the request.")
                .with_code("gateway_timeout");
            error_response(style, &err)
        }
    }
}

/// 请求 future 被丢弃（客户端断开）时记录一条 debug 日志。
struct CancelGuard {
    request_id: String,
    path: String,
    done: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.done {
            tracing::debug!(request_id = %self.request_id, path = %self.path, "client disconnected; request cancelled");
        }
    }
}

/// 访问日志：只记录请求 id、方法、路径（不含查询串）、状态码、耗时与密钥 id。
/// 不读取也不记录任何请求头 / 请求正文。
async fn access_log(req: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().simple().to_string();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();
    let mut guard = CancelGuard { request_id: request_id.clone(), path: path.clone(), done: false };

    let mut resp = next.run(req).await;
    guard.done = true;

    if let Ok(v) = HeaderValue::from_str(&request_id) {
        resp.headers_mut().insert("x-request-id", v);
    }
    let key_id = resp.extensions().get::<KeyIdForLog>().map(|k| k.0.clone());
    let status = resp.status();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if path == "/health" {
        tracing::debug!(request_id = %request_id, %method, %path, status = status.as_u16(), elapsed_ms, "gateway request");
    } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        tracing::warn!(request_id = %request_id, %method, %path, status = status.as_u16(), elapsed_ms, key_id = key_id.as_deref().unwrap_or("-"), "gateway request");
    } else {
        tracing::info!(request_id = %request_id, %method, %path, status = status.as_u16(), elapsed_ms, key_id = key_id.as_deref().unwrap_or("-"), "gateway request");
    }
    resp.into_response()
}

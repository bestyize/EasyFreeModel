//! 所有入口协议（OpenAI Chat / Responses、Anthropic Messages、Gemini、Ollama）的真实转发管线，以及各协议的模型列表。
//!
//! 流程：解析请求 → 模型解析（别名冲突 400 / 白名单 403）→ 构造出站请求（同协议透传，异协议经规范化转换）
//! → 路由层逐账号尝试（并发限制、失败分类、回退）→ 响应转换 / 流式转发 → 写用量记录。
//!
//! Gemini 入口的模型名与是否流式来自 URL（见 [`Inbound`]），其余协议来自请求体。
//!
//! 取消语义：本模块不 `spawn` 脱离请求生命周期的任务（用量写库除外，它只持有已完成的结果）。
//! 客户端断开时 hyper 丢弃请求 future / 响应体，其中的上游请求、`UsageGuard` 与并发名额随之释放。
//!
//! 日志纪律：只记录 provider / connection / 模型别名 / 状态码 / 次数，绝不记录密钥、请求头、请求或响应正文。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use serde_json::{json, Value};

use super::convert::{build_outbound, convert_response, upstream_error_body, wants_stream, warnings_header, ConvertedResponse};
use super::response::error_response;
use super::stream::{client_encoder, prime, PrimeOutcome, Pump, PumpConfig, WireFormat};
use super::VerifiedKey;
use crate::models::UsageStatus;
use crate::protocols::canonical::Usage;
use crate::protocols::{ApiError, ApiErrorKind, ProtocolKind};
use crate::routing::dispatch::{Attempt, DispatchError, DispatchFailure, NoConnection};
use crate::routing::repo::ProviderCfg;
use crate::routing::{describe_upstream_error, Runtime};
use crate::upstream::{AdapterKind, StreamOpen, UpstreamRequest, NOT_READY_MARKER};
use crate::usage::recorder::UsageGuard;

/// 以指定 HTTP 状态码返回协议风格的错误体（`ApiError::status()` 只覆盖固定映射）。
fn error_with_status(protocol: ProtocolKind, status: u16, err: &ApiError) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (status, Json(err.to_body(protocol))).into_response()
}

fn set_retry_after(resp: &mut Response, d: Duration) {
    let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
}

/// 一次入口请求。Gemini 的模型名与「是否流式」在 URL 里（`/v1beta/models/{model}:streamGenerateContent`），
/// 不在请求体里；其它协议这两项都在请求体里。
pub(crate) struct Inbound {
    pub protocol: ProtocolKind,
    pub headers: axum::http::HeaderMap,
    pub body: Bytes,
    /// Gemini：URL 里的模型名。
    pub path_model: Option<String>,
    /// Gemini：URL 里的方法是否为 `streamGenerateContent`。
    pub path_stream: Option<bool>,
    /// Gemini：流式时是否带 `?alt=sse`（不带则回复 JSON 数组流）。
    pub gemini_sse: bool,
}

/// 估算请求的输入 token（上游没有给出用量时才用）：只数文本，跳过 base64 图片 / 文件负载与 data URL。
/// 覆盖各协议的二进制载体：Anthropic `source.data`、Gemini `inlineData.data`、Responses `file_data`、Ollama `images`。
fn estimate_input_tokens(body: &Value) -> u32 {
    fn walk(v: &Value, acc: &mut (u64, u64)) {
        match v {
            Value::String(s) => {
                if !s.starts_with("data:") {
                    for c in s.chars() {
                        if c.is_ascii() {
                            acc.0 += 1;
                        } else {
                            acc.1 += 1;
                        }
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, acc)),
            Value::Object(o) => {
                let is_media = ["media_type", "mimeType", "mime_type"].iter().any(|k| o.contains_key(*k));
                for (k, x) in o {
                    if (is_media && k == "data") || k == "file_data" || (k == "images" && x.is_array()) {
                        continue;
                    }
                    walk(x, acc);
                }
            }
            _ => {}
        }
    }
    let mut acc = (0u64, 0u64);
    walk(body, &mut acc);
    (acc.0.div_ceil(4) + acc.1).min(u64::from(u32::MAX)) as u32
}

pub(crate) async fn proxy_inbound(rt: Arc<Runtime>, key: VerifiedKey, inbound: Inbound) -> Response {
    let Inbound { protocol, headers, body, path_model, path_stream, gemini_sse } = inbound;
    let invalid = |msg: &str, code: &str| error_response(protocol, &ApiError::new(ApiErrorKind::InvalidRequest, msg).with_code(code));

    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return invalid("The request body is not valid JSON.", "invalid_json"),
    };
    let requested = match protocol {
        ProtocolKind::Gemini => path_model.filter(|m| !m.trim().is_empty()),
        _ => value.get("model").and_then(Value::as_str).filter(|m| !m.trim().is_empty()).map(str::to_string),
    };
    let Some(requested) = requested else {
        return invalid("`model` is required.", "missing_model");
    };

    let mut resolved = match rt.resolve(&requested, &key).await {
        Ok(r) => r,
        Err(e) => return error_response(protocol, &e),
    };
    if rt.stream_usage_disabled(&resolved.provider.id) {
        resolved.endpoint.stream_usage = false;
    }
    let kind = resolved.endpoint.kind;
    let adapter = kind.adapter();
    let upstream_protocol = kind.native_protocol();
    let estimated_input = estimate_input_tokens(&value);
    let public_name = if resolved.model.public_alias.trim().is_empty() {
        format!("{}/{}", resolved.provider.id, resolved.model.upstream_model_id)
    } else {
        resolved.model.public_alias.clone()
    };

    let want_stream = wants_stream(protocol, &value, path_stream);
    let outbound = match build_outbound(protocol, upstream_protocol, value, &resolved.model.upstream_model_id, &headers, want_stream) {
        Ok(o) => o,
        Err(e) => return error_response(protocol, &e),
    };
    let warnings = warnings_header(&outbound.warnings);
    if !outbound.warnings.is_empty() {
        tracing::debug!(count = outbound.warnings.len(), "request converted with warnings");
    }

    let mut guard = UsageGuard::new(rt.db().clone(), Some(key.id.clone()), &resolved.provider.id, &public_name, protocol.id(), estimated_input);
    let passthrough = outbound.passthrough;
    let stream = outbound.stream;
    let client_wants_usage = outbound.client_wants_usage;
    let tool_names = outbound.tool_names;
    let req = UpstreamRequest {
        body: outbound.body,
        model: resolved.model.upstream_model_id.clone(),
        stream,
        passthrough,
        extra_headers: outbound.extra_headers,
    };
    let http = rt.client.clone();

    if !stream {
        let result = rt
            .dispatch(&resolved.provider, &resolved.endpoint, |target| {
                let (http, req, names) = (http.clone(), req.clone(), tool_names.clone());
                async move {
                    match adapter.invoke(&http, &target, req).await {
                        Ok(reply) if reply.is_success() => match convert_response(protocol, upstream_protocol, passthrough, &reply.body, &names) {
                            Ok(c) => Attempt::Ok(c),
                            Err(_) => Attempt::Failed(crate::upstream::UpstreamError::Malformed("the upstream response could not be parsed".into())),
                        },
                        Ok(reply) => Attempt::Rejected(reply),
                        Err(e) => Attempt::Failed(e),
                    }
                }
            })
            .await;
        return match result {
            Ok(d) => {
                let ConvertedResponse { body, usage, output_bytes } = d.value;
                guard.set_connection(&d.connection.id);
                guard.finish(UsageStatus::Success, &usage, output_bytes);
                tracing::info!(provider = %resolved.provider.id, connection = %d.connection.id, attempts = d.attempts, passthrough, "upstream request completed");
                let mut resp = Response::new(Body::from(body));
                resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                if let Some(w) = warnings {
                    resp.headers_mut().insert("x-router-warnings", w);
                }
                resp
            }
            Err(f) => failure_response(protocol, upstream_protocol, &resolved.provider, *f, guard),
        };
    }

    let idle = rt.request_timeout().await;
    let result = rt
        .dispatch(&resolved.provider, &resolved.endpoint, |target| {
            let (http, req, rt) = (http.clone(), req.clone(), rt.clone());
            async move {
                match open_stream(&rt, adapter, &http, target, req).await {
                    Err(e) => Attempt::Failed(e),
                    Ok(StreamOpen::Rejected(reply)) => Attempt::Rejected(reply),
                    Ok(StreamOpen::Stream(us)) => match prime(us.body, kind, idle).await {
                        PrimeOutcome::Ready(p) => Attempt::Ok(p),
                        PrimeOutcome::InBandError(reply) => Attempt::Rejected(reply),
                        PrimeOutcome::Failed(e) => Attempt::Failed(e),
                    },
                }
            }
        })
        .await;

    match result {
        Ok(d) => {
            guard.set_connection(&d.connection.id);
            tracing::info!(provider = %resolved.provider.id, connection = %d.connection.id, attempts = d.attempts, passthrough, "upstream stream started");
            let encoder = if passthrough { None } else { Some(client_encoder(protocol, client_wants_usage)) };
            let wire = WireFormat::for_client(protocol, gemini_sse);
            let cfg = PumpConfig {
                runtime: rt.clone(),
                client_protocol: protocol,
                upstream_kind: kind,
                encoder,
                wire,
                tool_names,
                drop_injected_usage_chunk: passthrough
                    && kind == AdapterKind::OpenAiCompatible
                    && resolved.endpoint.stream_usage
                    && !client_wants_usage,
                connection_id: d.connection.id.clone(),
                secrets: d.secrets.0,
                idle_timeout: idle,
                _permit: d.permit,
            };
            let pump = Pump::new(d.value, cfg, guard);
            let mut resp = Response::new(Body::from_stream(pump.into_stream()));
            let h = resp.headers_mut();
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(wire.content_type()));
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            h.insert("x-accel-buffering", HeaderValue::from_static("no"));
            if let Some(w) = warnings {
                h.insert("x-router-warnings", w);
            }
            resp
        }
        Err(f) => failure_response(protocol, upstream_protocol, &resolved.provider, *f, guard),
    }
}

/// 打开上游流。个别 OpenAI 兼容上游不认识 `stream_options` 会回 400/422：此时记住这家并去掉该字段重试一次，
/// 之后对它不再注入（用量退回估算）。只针对我们自己注入的字段；客户端自己带的 `stream_options` 原样保留。
async fn open_stream(
    rt: &Runtime,
    adapter: &'static dyn crate::upstream::Adapter,
    http: &crate::upstream::UpstreamClient,
    mut target: crate::upstream::UpstreamTarget,
    req: UpstreamRequest,
) -> Result<StreamOpen, crate::upstream::UpstreamError> {
    let first = adapter.stream(http, &target, req.clone()).await?;
    if let StreamOpen::Rejected(reply) = &first {
        let mentions = String::from_utf8_lossy(&reply.body).to_ascii_lowercase().contains("stream_options");
        if target.endpoint.stream_usage
            && target.endpoint.kind == AdapterKind::OpenAiCompatible
            && matches!(reply.status, 400 | 422)
            && mentions
        {
            tracing::info!(provider = %target.provider_id, "upstream rejected stream_options; retrying without it");
            rt.disable_stream_usage(&target.provider_id);
            target.endpoint.stream_usage = false;
            return adapter.stream(http, &target, req).await;
        }
    }
    Ok(first)
}

fn no_connection_message(provider: &ProviderCfg, n: &NoConnection) -> String {
    let name = &provider.display_name;
    if n.total == 0 {
        return format!("Provider `{name}` has no connections configured.");
    }
    if n.cooling > 0 {
        let wait = n.shortest_cooldown.map(|d| d.as_secs() + 1).unwrap_or(0);
        return format!(
            "All usable connections of provider `{name}` are cooling down after upstream errors; retry in about {wait} seconds."
        );
    }
    if n.missing_credential > 0 {
        return format!("No connection of provider `{name}` has a stored credential.");
    }
    if n.needs_reauth > 0 {
        return format!(
            "Every usable connection of provider `{name}` needs to be re-authorized in Router Desktop (its OAuth token is no longer valid)."
        );
    }
    format!("All connections of provider `{name}` are disabled or paused.")
}

/// 把调度失败如实翻译给客户端，并写入失败的用量记录。
fn failure_response(
    protocol: ProtocolKind,
    upstream_protocol: ProtocolKind,
    provider: &ProviderCfg,
    failure: DispatchFailure,
    mut guard: UsageGuard,
) -> Response {
    if let Some(c) = &failure.connection_id {
        guard.set_connection(c);
    }
    guard.finish(UsageStatus::Error, &Usage::default(), 0);
    tracing::warn!(provider = %provider.id, attempts = failure.attempts, "request failed");
    match failure.error {
        DispatchError::Busy => {
            let err = ApiError::new(
                ApiErrorKind::BadGateway,
                format!("Provider `{}` is at its concurrency limit; retry shortly.", provider.display_name),
            )
            .with_code("provider_busy");
            let mut resp = error_with_status(protocol, 503, &err);
            set_retry_after(&mut resp, Duration::from_secs(1));
            resp
        }
        DispatchError::NoConnection(n) => {
            let err = ApiError::new(ApiErrorKind::BadGateway, no_connection_message(provider, &n)).with_code("no_available_connection");
            let mut resp = error_with_status(protocol, 503, &err);
            if let Some(d) = n.shortest_cooldown {
                set_retry_after(&mut resp, d);
            }
            resp
        }
        DispatchError::Upstream { reply, secrets } => {
            let (status, body) = upstream_error_body(protocol, upstream_protocol, reply.status, &reply.body, &secrets.0);
            let mut resp = Response::new(Body::from(body));
            *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
            resp.headers_mut().insert("x-router-source", HeaderValue::from_static("upstream"));
            if let Some(d) = reply.retry_after.filter(|_| status == 429) {
                set_retry_after(&mut resp, d);
            }
            resp
        }
        DispatchError::Transport(e) => {
            let (kind, msg) = describe_upstream_error(&e);
            let err = ApiError::new(kind, msg).with_code("upstream_unreachable");
            let status = if kind == ApiErrorKind::Timeout { 504 } else { 502 };
            error_with_status(protocol, status, &err)
        }
        DispatchError::Local(msg) => {
            // 配置合法但对应的接入方式尚未实现（例如 Vertex 的服务账号 / OAuth 令牌）：如实报告 501
            if let Some((_, reason)) = msg.split_once(NOT_READY_MARKER) {
                let err = ApiError::new(ApiErrorKind::NotImplemented, reason.trim().to_string()).with_code("upstream_adapter_not_implemented");
                return error_with_status(protocol, 501, &err);
            }
            let err = ApiError::new(ApiErrorKind::Internal, msg).with_code("upstream_misconfigured");
            error_with_status(protocol, 500, &err)
        }
        DispatchError::NotImplemented(msg) => {
            let err = ApiError::new(ApiErrorKind::NotImplemented, msg).with_code("token_exchange_not_implemented");
            error_with_status(protocol, 501, &err)
        }
        DispatchError::TokenUnavailable(msg) => {
            let err = ApiError::new(ApiErrorKind::BadGateway, msg).with_code("oauth_token_unavailable");
            error_with_status(protocol, 503, &err)
        }
    }
}

fn catalog_error(protocol: ProtocolKind) -> Response {
    error_response(
        protocol,
        &ApiError::new(ApiErrorKind::Internal, "Failed to read the model catalog.").with_code("catalog_unavailable"),
    )
}

/// `GET /v1/models`：OpenAI 风格列表。
pub(crate) async fn list_models(rt: &Runtime, key: &VerifiedKey, protocol: ProtocolKind) -> Response {
    match rt.list_public_models(key).await {
        Ok(models) => {
            let data: Vec<Value> = models
                .into_iter()
                .map(|m| json!({"id": m.id, "object": "model", "created": 0, "owned_by": m.provider_id}))
                .collect();
            Json(json!({"object": "list", "data": data})).into_response()
        }
        Err(_) => catalog_error(protocol),
    }
}

fn gemini_model_entry(id: &str) -> Value {
    json!({
        "name": format!("models/{id}"),
        "displayName": id,
        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
    })
}

/// `GET /v1beta/models`：Gemini 风格列表（`name` 为 `models/<对外模型名>`）。
pub(crate) async fn list_models_gemini(rt: &Runtime, key: &VerifiedKey) -> Response {
    match rt.list_public_models(key).await {
        Ok(models) => {
            let data: Vec<Value> = models.iter().map(|m| gemini_model_entry(&m.id)).collect();
            Json(json!({"models": data})).into_response()
        }
        Err(_) => catalog_error(ProtocolKind::Gemini),
    }
}

/// `GET /v1beta/models/{name}`：单个模型（Gemini SDK 的 `models.get`）。
pub(crate) async fn get_model_gemini(rt: &Runtime, key: &VerifiedKey, name: &str) -> Response {
    let wanted = name.strip_prefix("models/").unwrap_or(name);
    match rt.list_public_models(key).await {
        Ok(models) => match models.iter().find(|m| m.id == wanted) {
            Some(m) => Json(gemini_model_entry(&m.id)).into_response(),
            None => error_response(
                ProtocolKind::Gemini,
                &ApiError::new(ApiErrorKind::NotFound, format!("The model `{}` does not exist or is not enabled.", name.chars().take(120).collect::<String>()))
                    .with_code("model_not_found"),
            ),
        },
        Err(_) => catalog_error(ProtocolKind::Gemini),
    }
}

/// `GET /api/tags`：Ollama 风格列表。
pub(crate) async fn list_models_ollama(rt: &Runtime, key: &VerifiedKey) -> Response {
    match rt.list_public_models(key).await {
        Ok(models) => {
            let data: Vec<Value> = models.into_iter().map(|m| json!({"name": m.id, "model": m.id})).collect();
            Json(json!({"models": data})).into_response()
        }
        Err(_) => catalog_error(ProtocolKind::Ollama),
    }
}

//! 认证、协议授权与限流中间件。
//!
//! 顺序：取凭据 → `KeyVerifier::verify`（401）→ 协议是否允许（403）→ RPM 限流（429）→ 放行。
//! **日志纪律**：凭据明文只在局部变量里短暂存在并交给 `verify`，不进入任何日志、错误信息或响应体；
//! 日志中只出现 `VerifiedKey::id`。

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

use super::response::{classify, error_response, error_style, extract_credential, protocol_allowed, RouteClass};
use super::server::AppState;
use super::VerifiedKey;
use crate::protocols::{ApiError, ApiErrorKind};

/// 放入请求扩展，供下游处理器读取当前密钥（模型白名单、统计归属等）。
#[derive(Debug, Clone)]
pub struct AuthenticatedKey(pub VerifiedKey);

/// 放入**响应**扩展，供最外层访问日志记录是哪个密钥发起的请求（仅 id）。
#[derive(Debug, Clone)]
pub(crate) struct KeyIdForLog(pub String);

fn tag(mut resp: Response, key_id: &str) -> Response {
    resp.extensions_mut().insert(KeyIdForLog(key_id.to_string()));
    resp
}

pub(crate) async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let style = error_style(&path, req.headers());

    let Some(credential) = extract_credential(req.headers()) else {
        let err = ApiError::new(
            ApiErrorKind::Authentication,
            "You didn't provide an API key. Send it as `Authorization: Bearer <key>`, or in the `x-api-key` / `x-goog-api-key` header.",
        )
        .with_code("missing_api_key");
        return with_www_authenticate(error_response(style, &err));
    };

    let Some(key) = state.verifier.verify(credential).await else {
        let err = ApiError::new(ApiErrorKind::Authentication, "Invalid API key provided.").with_code("invalid_api_key");
        return with_www_authenticate(error_response(style, &err));
    };

    if let RouteClass::Protocol(protocol) = classify(&path) {
        if !protocol_allowed(&key.allowed_protocols, protocol) {
            let err = ApiError::new(
                ApiErrorKind::PermissionDenied,
                format!("This API key is not allowed to use the {} protocol.", protocol.display_name()),
            )
            .with_code("protocol_not_allowed");
            return tag(error_response(style, &err), &key.id);
        }
    }

    if let Some(rpm) = key.rpm_limit {
        if let Err(limited) = state.limiter.check(&key.id, rpm) {
            let err = ApiError::new(
                ApiErrorKind::RateLimit,
                format!("Rate limit exceeded: at most {rpm} requests per minute. Retry in {} seconds.", limited.retry_after_secs()),
            )
            .with_code("rate_limit_exceeded");
            let mut resp = error_response(style, &err);
            resp.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(limited.retry_after_secs()));
            return tag(resp, &key.id);
        }
    }

    let key_id = key.id.clone();
    req.extensions_mut().insert(AuthenticatedKey(key));
    tag(next.run(req).await, &key_id)
}

fn with_www_authenticate(mut resp: Response) -> Response {
    resp.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    resp
}

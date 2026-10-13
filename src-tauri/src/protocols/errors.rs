//! 协议风格的错误体。网关与协议层共用，保证 401/403/429/501 等错误在各协议下形状正确。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ProtocolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorKind {
    InvalidRequest,
    Authentication,
    PermissionDenied,
    NotFound,
    MethodNotAllowed,
    PayloadTooLarge,
    RateLimit,
    Internal,
    NotImplemented,
    BadGateway,
    Timeout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub kind: ApiErrorKind,
    pub message: String,
    /// 机器可读的细分代码（OpenAI 风格 `error.code`）。
    pub code: Option<String>,
}

impl ApiError {
    pub fn new(kind: ApiErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), code: None }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn status(&self) -> u16 {
        match self.kind {
            ApiErrorKind::InvalidRequest => 400,
            ApiErrorKind::Authentication => 401,
            ApiErrorKind::PermissionDenied => 403,
            ApiErrorKind::NotFound => 404,
            ApiErrorKind::MethodNotAllowed => 405,
            ApiErrorKind::PayloadTooLarge => 413,
            ApiErrorKind::RateLimit => 429,
            ApiErrorKind::Internal => 500,
            ApiErrorKind::NotImplemented => 501,
            ApiErrorKind::BadGateway => 502,
            ApiErrorKind::Timeout => 504,
        }
    }

    fn openai_type(&self) -> &'static str {
        match self.kind {
            ApiErrorKind::InvalidRequest | ApiErrorKind::NotFound | ApiErrorKind::MethodNotAllowed => {
                "invalid_request_error"
            }
            ApiErrorKind::PayloadTooLarge => "invalid_request_error",
            ApiErrorKind::Authentication => "authentication_error",
            ApiErrorKind::PermissionDenied => "permission_error",
            ApiErrorKind::RateLimit => "rate_limit_error",
            ApiErrorKind::NotImplemented => "not_implemented_error",
            ApiErrorKind::Internal | ApiErrorKind::BadGateway | ApiErrorKind::Timeout => "server_error",
        }
    }

    fn anthropic_type(&self) -> &'static str {
        match self.kind {
            ApiErrorKind::InvalidRequest | ApiErrorKind::MethodNotAllowed => "invalid_request_error",
            ApiErrorKind::Authentication => "authentication_error",
            ApiErrorKind::PermissionDenied => "permission_error",
            ApiErrorKind::NotFound => "not_found_error",
            ApiErrorKind::PayloadTooLarge => "request_too_large",
            ApiErrorKind::RateLimit => "rate_limit_error",
            // Anthropic 没有「未实现」类型，使用 api_error，message 中写明原因。
            ApiErrorKind::NotImplemented | ApiErrorKind::Internal | ApiErrorKind::BadGateway | ApiErrorKind::Timeout => {
                "api_error"
            }
        }
    }

    fn gemini_status(&self) -> &'static str {
        match self.kind {
            ApiErrorKind::InvalidRequest | ApiErrorKind::MethodNotAllowed | ApiErrorKind::PayloadTooLarge => {
                "INVALID_ARGUMENT"
            }
            ApiErrorKind::Authentication => "UNAUTHENTICATED",
            ApiErrorKind::PermissionDenied => "PERMISSION_DENIED",
            ApiErrorKind::NotFound => "NOT_FOUND",
            ApiErrorKind::RateLimit => "RESOURCE_EXHAUSTED",
            ApiErrorKind::NotImplemented => "UNIMPLEMENTED",
            ApiErrorKind::Internal => "INTERNAL",
            ApiErrorKind::BadGateway => "UNAVAILABLE",
            ApiErrorKind::Timeout => "DEADLINE_EXCEEDED",
        }
    }

    /// 渲染为指定协议风格的 JSON 错误体。
    pub fn to_body(&self, protocol: ProtocolKind) -> Value {
        match protocol {
            ProtocolKind::OpenAiChat | ProtocolKind::OpenAiResponses => json!({
                "error": {
                    "message": self.message,
                    "type": self.openai_type(),
                    "code": self.code,
                }
            }),
            ProtocolKind::AnthropicMessages => json!({
                "type": "error",
                "error": { "type": self.anthropic_type(), "message": self.message }
            }),
            // Vertex 的错误体与 Gemini 相同
            ProtocolKind::Gemini | ProtocolKind::Vertex => json!({
                "error": { "code": self.status(), "message": self.message, "status": self.gemini_status() }
            }),
            // Ollama：`{"error":"message"}`，状态码由 HTTP 层承载
            ProtocolKind::Ollama => json!({ "error": self.message }),
        }
    }

    /// 流式响应中途出错时的 SSE 帧（OpenAI 无 event 名，Anthropic 为 `event: error`）。
    /// Ollama 的流是 NDJSON：这里的 `data` 是那一行的 JSON，由调用方按 NDJSON 写出（不带 `data:` 前缀）。
    pub fn to_sse_frame(&self, protocol: ProtocolKind) -> super::SseFrame {
        let data = self.to_body(protocol).to_string();
        match protocol {
            ProtocolKind::AnthropicMessages => super::SseFrame::event("error", data),
            _ => super::SseFrame::data(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_style_body() {
        let e = ApiError::new(ApiErrorKind::Authentication, "bad key").with_code("invalid_api_key");
        assert_eq!(e.status(), 401);
        assert_eq!(
            e.to_body(ProtocolKind::OpenAiChat),
            json!({"error": {"message": "bad key", "type": "authentication_error", "code": "invalid_api_key"}})
        );
    }

    #[test]
    fn anthropic_style_body() {
        let e = ApiError::new(ApiErrorKind::NotImplemented, "todo");
        assert_eq!(e.status(), 501);
        assert_eq!(
            e.to_body(ProtocolKind::AnthropicMessages),
            json!({"type": "error", "error": {"type": "api_error", "message": "todo"}})
        );
        assert_eq!(e.to_sse_frame(ProtocolKind::AnthropicMessages).event.as_deref(), Some("error"));
    }

    #[test]
    fn ollama_and_vertex_bodies() {
        let e = ApiError::new(ApiErrorKind::RateLimit, "slow down");
        assert_eq!(e.to_body(ProtocolKind::Ollama), json!({"error": "slow down"}));
        assert_eq!(e.to_body(ProtocolKind::Vertex), e.to_body(ProtocolKind::Gemini));
        assert_eq!(e.to_sse_frame(ProtocolKind::Ollama).data, r#"{"error":"slow down"}"#);
    }

    #[test]
    fn gemini_style_body() {
        let e = ApiError::new(ApiErrorKind::RateLimit, "slow down");
        assert_eq!(
            e.to_body(ProtocolKind::Gemini),
            json!({"error": {"code": 429, "message": "slow down", "status": "RESOURCE_EXHAUSTED"}})
        );
    }
}

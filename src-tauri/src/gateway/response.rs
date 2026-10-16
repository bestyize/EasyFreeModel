//! 协议风格的错误响应与请求分类。

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::protocols::{ApiError, ProtocolKind};

/// 路径对应的协议归类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteClass {
    /// 某协议的推理端点，需要校验密钥的 `allowed_protocols`。
    Protocol(ProtocolKind),
    /// 模型列表：任何有效密钥都可访问。
    Models,
    Other,
}

/// Gemini 的 URL 方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeminiMethod {
    Generate,
    StreamGenerate,
}

/// 把 `{model}:{method}`（`models/` 之后的部分，已解码）拆开。模型名里可以有 `/`（对外别名如 `p1/gemini`）和 `:`
/// （如 Ollama 风格的 `llama3:8b`）：方法永远是**最后一个** `:` 之后的部分。
/// 返回 `(model, method)`；`method` 原样返回，是否受支持由调用方判断。
pub(crate) fn split_gemini_target(rest: &str) -> Option<(&str, &str)> {
    let (model, method) = rest.rsplit_once(':')?;
    (!model.is_empty() && !method.is_empty()).then_some((model, method))
}

pub(crate) fn gemini_method(method: &str) -> Option<GeminiMethod> {
    match method {
        "generateContent" => Some(GeminiMethod::Generate),
        "streamGenerateContent" => Some(GeminiMethod::StreamGenerate),
        _ => None,
    }
}

/// `/v1beta/models/<model>:<method>`（或 `/v1/models/...`）里受支持的 Gemini 推理方法。
fn gemini_inference_path(path: &str) -> bool {
    // 有的客户端把 `:` 编码成 %3A
    let decoded = path.replace("%3A", ":").replace("%3a", ":");
    let Some(rest) = decoded.strip_prefix("/v1beta/models/").or_else(|| decoded.strip_prefix("/v1/models/")) else {
        return false;
    };
    split_gemini_target(rest).is_some_and(|(_, m)| gemini_method(m).is_some())
}

pub(crate) fn classify(path: &str) -> RouteClass {
    match path {
        "/v1/chat/completions" => RouteClass::Protocol(ProtocolKind::OpenAiChat),
        "/v1/responses" => RouteClass::Protocol(ProtocolKind::OpenAiResponses),
        "/v1/responses/compact" => RouteClass::Protocol(ProtocolKind::OpenAiResponses),
        "/v1/messages" => RouteClass::Protocol(ProtocolKind::AnthropicMessages),
        "/v1/messages/count_tokens" => RouteClass::Protocol(ProtocolKind::AnthropicMessages),
        "/api/chat" => RouteClass::Protocol(ProtocolKind::Ollama),
        "/v1/models" | "/v1beta/models" | "/api/tags" => RouteClass::Models,
        p if gemini_inference_path(p) => RouteClass::Protocol(ProtocolKind::Gemini),
        p if p.starts_with("/v1beta/models/") => RouteClass::Models,
        _ => RouteClass::Other,
    }
}

/// 密钥是否被允许使用该协议。
///
/// `Ollama` 不在前端 / 密钥可选的四种协议里（`models::Protocol`）。「不限制协议」的密钥会被展开成这四种全选，
/// 因此：显式带了 `ollama` 的放行；四种全部具备（等价于不限制协议）的也放行；只勾选了部分协议的不放行。
pub(crate) fn protocol_allowed(allowed: &[String], protocol: ProtocolKind) -> bool {
    if allowed.iter().any(|p| p == protocol.id()) {
        return true;
    }
    protocol == ProtocolKind::Ollama && ProtocolKind::ALL.iter().all(|p| allowed.iter().any(|a| a == p.id()))
}

/// 决定错误体采用哪种协议风格：已知端点按端点；否则看请求头特征；默认 OpenAI。
pub(crate) fn error_style(path: &str, headers: &HeaderMap) -> ProtocolKind {
    if let RouteClass::Protocol(p) = classify(path) {
        return p;
    }
    if path.starts_with("/api/") {
        return ProtocolKind::Ollama;
    }
    if path.starts_with("/v1beta/") || headers.contains_key("x-goog-api-key") {
        return ProtocolKind::Gemini;
    }
    if headers.contains_key("anthropic-version")
        || (headers.contains_key("x-api-key") && !headers.contains_key(header::AUTHORIZATION))
    {
        return ProtocolKind::AnthropicMessages;
    }
    ProtocolKind::OpenAiChat
}

pub(crate) fn error_response(style: ProtocolKind, err: &ApiError) -> Response {
    let status = StatusCode::from_u16(err.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(err.to_body(style))).into_response()
}

/// 从请求头取出明文密钥：优先 `Authorization: Bearer`，其次 Anthropic `x-api-key`、Gemini `x-goog-api-key`。
/// 返回值是敏感数据，调用方不得写入日志。
pub(crate) fn extract_credential(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some((scheme, rest)) = v.trim().split_once(char::is_whitespace) {
            let token = rest.trim();
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    for name in ["x-api-key", "x-goog-api-key"] {
        if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
            let token = v.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn credential_extraction() {
        assert_eq!(extract_credential(&headers(&[("authorization", "Bearer abc")])).as_deref(), Some("abc"));
        assert_eq!(extract_credential(&headers(&[("authorization", "bearer   abc ")])).as_deref(), Some("abc"));
        assert_eq!(extract_credential(&headers(&[("x-api-key", " abc ")])).as_deref(), Some("abc"));
        assert_eq!(extract_credential(&headers(&[("x-goog-api-key", "abc")])).as_deref(), Some("abc"));
        // Authorization 不是 Bearer 时回落到 x-api-key
        assert_eq!(
            extract_credential(&headers(&[("authorization", "Basic Zm9v"), ("x-api-key", "abc")])).as_deref(),
            Some("abc")
        );
        assert_eq!(extract_credential(&headers(&[("authorization", "Bearer ")])), None);
        assert_eq!(extract_credential(&headers(&[("authorization", "abc")])), None);
        assert_eq!(extract_credential(&headers(&[])), None);
    }

    #[test]
    fn style_detection() {
        let none = headers(&[]);
        assert_eq!(error_style("/v1/messages", &none), ProtocolKind::AnthropicMessages);
        assert_eq!(error_style("/v1/chat/completions", &headers(&[("x-api-key", "k")])), ProtocolKind::OpenAiChat);
        assert_eq!(error_style("/v1/models", &headers(&[("x-api-key", "k")])), ProtocolKind::AnthropicMessages);
        assert_eq!(error_style("/v1/models", &headers(&[("anthropic-version", "2023-06-01")])), ProtocolKind::AnthropicMessages);
        assert_eq!(error_style("/v1/models", &headers(&[("authorization", "Bearer k")])), ProtocolKind::OpenAiChat);
        assert_eq!(error_style("/v1beta/models/x:generateContent", &none), ProtocolKind::Gemini);
        assert_eq!(error_style("/v1beta/models", &none), ProtocolKind::Gemini);
        assert_eq!(error_style("/api/chat", &headers(&[("x-api-key", "k")])), ProtocolKind::Ollama);
        assert_eq!(error_style("/api/tags", &none), ProtocolKind::Ollama);
        assert_eq!(error_style("/nope", &none), ProtocolKind::OpenAiChat);
    }

    #[test]
    fn gemini_target_splitting_uses_the_last_colon() {
        assert_eq!(split_gemini_target("gemini-2.5-flash:generateContent"), Some(("gemini-2.5-flash", "generateContent")));
        assert_eq!(split_gemini_target("p1/gemini-x:streamGenerateContent"), Some(("p1/gemini-x", "streamGenerateContent")));
        assert_eq!(split_gemini_target("llama3:8b:generateContent"), Some(("llama3:8b", "generateContent")));
        assert_eq!(split_gemini_target("no-method"), None);
        assert_eq!(split_gemini_target(":generateContent"), None);
        assert_eq!(split_gemini_target("model:"), None);
        assert_eq!(gemini_method("generateContent"), Some(GeminiMethod::Generate));
        assert_eq!(gemini_method("streamGenerateContent"), Some(GeminiMethod::StreamGenerate));
        assert_eq!(gemini_method("countTokens"), None);
    }

    #[test]
    fn route_classification_for_gemini_and_ollama() {
        let gem = RouteClass::Protocol(ProtocolKind::Gemini);
        for p in [
            "/v1beta/models/gemini-2.5-flash:generateContent",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent",
            "/v1beta/models/p1/gem:generateContent",
            "/v1beta/models/gemini-2.5-flash%3AgenerateContent",
            "/v1/models/gemini-2.5-flash:generateContent",
        ] {
            assert_eq!(classify(p), gem, "{p}");
        }
        assert_eq!(classify("/v1beta/models"), RouteClass::Models);
        assert_eq!(classify("/v1beta/models/gemini-2.5-flash"), RouteClass::Models);
        assert_eq!(classify("/v1beta/models/gemini-2.5-flash:countTokens"), RouteClass::Models, "unsupported methods carry no protocol");
        assert_eq!(classify("/api/chat"), RouteClass::Protocol(ProtocolKind::Ollama));
        assert_eq!(classify("/api/tags"), RouteClass::Models);
        assert_eq!(classify("/v1/messages/count_tokens"), RouteClass::Protocol(ProtocolKind::AnthropicMessages));
        assert_eq!(classify("/v1/responses/compact"), RouteClass::Protocol(ProtocolKind::OpenAiResponses));
        assert_eq!(classify("/v1/models/gpt-4o"), RouteClass::Other);
    }

    #[test]
    fn ollama_is_allowed_for_unrestricted_keys_only() {
        let all: Vec<String> = ProtocolKind::ALL.iter().map(|p| p.id().to_string()).collect();
        assert!(protocol_allowed(&all, ProtocolKind::Ollama), "a key without a protocol restriction is expanded to the four known protocols");
        assert!(protocol_allowed(&["ollama".to_string()], ProtocolKind::Ollama));
        assert!(!protocol_allowed(&["openai-chat".to_string(), "gemini".to_string()], ProtocolKind::Ollama));
        assert!(!protocol_allowed(&[], ProtocolKind::Ollama));
        assert!(!protocol_allowed(&["ollama".to_string()], ProtocolKind::Gemini));
        assert!(protocol_allowed(&["gemini".to_string()], ProtocolKind::Gemini));
    }
}

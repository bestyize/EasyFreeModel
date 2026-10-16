//! 请求 / 响应转换：透传 vs 经规范化模型转换，以及上游错误 → 客户端协议风格的错误体。
//!
//! - **透传**（入口协议 == 上游协议）：只把 `model` 改成上游真实 ID，其余字段原样保留（含未知字段），
//!   不经规范化重编码；非流式响应体原样返回。
//! - **转换**（协议不同）：`decode_request → CanonicalRequest → encode_request`，响应反向转换；
//!   无法转换的字段放进 `warnings`，以响应头 `x-router-warnings`（只含字段名）暴露。
//! - 上游错误：同协议且形状合法时保留上游错误体（擦除我们发出的密钥）；否则重新包装成客户端协议风格。

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use bytes::Bytes;
use serde_json::Value;

use crate::protocols::canonical::{CanonicalResponse, ContentPart, Usage};
use crate::protocols::gemini::{self, ToolNameMap};
use crate::protocols::{anthropic, ollama, openai_chat, openai_responses, vertex, ApiError, ApiErrorKind, ProtocolKind};

pub(crate) struct Outbound {
    /// 已是上游原生协议的请求体（model 已改写；Gemini / Vertex 的模型在 URL 里，body 里没有）。
    pub body: Value,
    pub passthrough: bool,
    pub warnings: Vec<String>,
    #[allow(dead_code)]
    pub stream: bool,
    /// 客户端是否要求在流里带 usage（OpenAI `stream_options.include_usage`）。
    pub client_wants_usage: bool,
    pub extra_headers: Vec<(HeaderName, HeaderValue)>,
    /// 编码到 Gemini / Vertex 时被改写过的工具名（贯穿整个请求生命周期，响应解码时还原）。其它协议为空映射。
    pub tool_names: ToolNameMap,
}

/// Claude's mid-conversation system messages must become user content. Keep
/// the top-level system prompt stable so a changing reminder does not bust
/// the cached conversation prefix on every request (same policy as 9Router).
fn fold_anthropic_system_messages(body: &mut Value) -> Result<(), ApiError> {
    let Some(obj) = body.as_object_mut() else { return Ok(()) };
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else { return Ok(()) };
    if !messages.iter().any(|m| m.get("role").and_then(Value::as_str) == Some("system")) {
        return Ok(());
    }
    let mut rest = Vec::with_capacity(messages.len());
    for message in std::mem::take(messages) {
        if message.get("role").and_then(Value::as_str) != Some("system") {
            rest.push(message);
            continue;
        }
        let text = match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) if parts.iter().all(|p| p.get("type").and_then(Value::as_str) == Some("text") && p.get("text").and_then(Value::as_str).is_some()) =>
                parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
            _ => return Err(ApiError::new(ApiErrorKind::InvalidRequest, "System messages must contain text.")),
        };
        if text.trim().is_empty() { continue }
        let block = serde_json::json!({"type":"text", "text":text});
        if rest.last().is_some_and(|m: &Value| m.get("role").and_then(Value::as_str) == Some("user")) {
            let previous = rest.last_mut().expect("checked above");
            if let Some(obj) = previous.as_object_mut() {
                let content = obj.entry("content").or_insert(Value::Array(vec![]));
                match content {
                    Value::String(s) => {
                        let text = std::mem::take(s);
                        *content = Value::Array(vec![serde_json::json!({"type":"text", "text":text}), block]);
                    }
                    Value::Array(parts) => parts.push(block),
                    _ => return Err(ApiError::new(ApiErrorKind::InvalidRequest, "User message content must be text or blocks.")),
                }
            }
        } else {
            rest.push(serde_json::json!({"role":"user", "content":[block]}));
        }
    }
    *messages = rest;
    Ok(())
}

/// 请求是否为流式：Gemini 由 URL 方法决定；Ollama 缺省为流式；其余看 body 里的 `stream`（缺省非流式）。
pub(crate) fn wants_stream(protocol: ProtocolKind, body: &Value, path_stream: Option<bool>) -> bool {
    match protocol {
        ProtocolKind::Gemini | ProtocolKind::Vertex => path_stream.unwrap_or(false),
        ProtocolKind::Ollama => body.get("stream").and_then(Value::as_bool).unwrap_or(true),
        _ => body.get("stream").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn decode_client_request(
    protocol: ProtocolKind,
    value: &Value,
    model: &str,
    stream: bool,
) -> Result<crate::protocols::CanonicalRequest, ApiError> {
    let r = match protocol {
        ProtocolKind::OpenAiChat => openai_chat::decode_request(value),
        ProtocolKind::AnthropicMessages => anthropic::decode_request(value),
        ProtocolKind::OpenAiResponses => openai_responses::decode_request(value),
        ProtocolKind::Gemini => gemini::decode_request_for(model, stream, value),
        ProtocolKind::Ollama => ollama::decode_request(value),
        ProtocolKind::Vertex => {
            return Err(ApiError::new(ApiErrorKind::NotImplemented, "Vertex AI is an upstream-only protocol."));
        }
    };
    r.map_err(|e| e.to_api_error())
}

fn encode_upstream_request(
    protocol: ProtocolKind,
    req: &crate::protocols::CanonicalRequest,
) -> Result<(crate::protocols::Encoded, ToolNameMap), ApiError> {
    let r = match protocol {
        ProtocolKind::OpenAiChat => openai_chat::encode_request(req).map(|e| (e, ToolNameMap::default())),
        ProtocolKind::AnthropicMessages => anthropic::encode_request(req).map(|e| (e, ToolNameMap::default())),
        ProtocolKind::OpenAiResponses => openai_responses::encode_request(req).map(|e| (e, ToolNameMap::default())),
        ProtocolKind::Gemini => gemini::encode_request_with(req, &gemini::Dialect::AI_STUDIO),
        // Vertex：历史里缺签名的 functionCall 补占位签名（见 protocols::vertex 的文档）
        ProtocolKind::Vertex => vertex::encode_request_mapped(req, true),
        ProtocolKind::Ollama => ollama::encode_request(req).map(|e| (e, ToolNameMap::default())),
    };
    r.map_err(|e| e.to_api_error())
}

/// 构造发往上游的请求。`stream` 由 [`wants_stream`] 得出；`upstream_model` 是上游真实模型 ID。
pub(crate) fn build_outbound(
    client: ProtocolKind,
    upstream: ProtocolKind,
    mut value: Value,
    upstream_model: &str,
    headers: &HeaderMap,
    stream: bool,
) -> Result<Outbound, ApiError> {
    if client == ProtocolKind::AnthropicMessages {
        fold_anthropic_system_messages(&mut value)?;
    }
    if client == upstream {
        let mut body = value;
        let Some(obj) = body.as_object_mut() else {
            return Err(ApiError::new(ApiErrorKind::InvalidRequest, "Request body must be a JSON object."));
        };
        match client {
            // Gemini 的模型在 URL 路径里；body 里即使带了 `model` 也必须与 URL 一致
            ProtocolKind::Gemini => {
                if obj.contains_key("model") {
                    obj.insert("model".into(), Value::String(format!("models/{upstream_model}")));
                }
            }
            ProtocolKind::Ollama => {
                obj.insert("model".into(), Value::String(upstream_model.to_string()));
                // Ollama 缺省即流式；显式写出，保证上游与我们对响应形态的理解一致
                obj.insert("stream".into(), Value::Bool(stream));
            }
            _ => {
                obj.insert("model".into(), Value::String(upstream_model.to_string()));
            }
        }
        let client_wants_usage = body
            .get("stream_options")
            .and_then(|s| s.get("include_usage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut extra_headers = Vec::new();
        if client == ProtocolKind::AnthropicMessages {
            for name in ["anthropic-version", "anthropic-beta"] {
                if let Some(v) = headers.get(name) {
                    extra_headers.push((HeaderName::from_static(name), v.clone()));
                }
            }
        }
        return Ok(Outbound {
            body,
            passthrough: true,
            warnings: vec![],
            stream,
            client_wants_usage,
            extra_headers,
            tool_names: ToolNameMap::default(),
        });
    }

    let mut canon = decode_client_request(client, &value, upstream_model, stream)?;
    canon.model = upstream_model.to_string();
    let (encoded, tool_names) = encode_upstream_request(upstream, &canon)?;
    let mut warnings = canon.warnings.clone();
    warnings.extend(encoded.warnings);
    Ok(Outbound {
        body: encoded.body,
        passthrough: false,
        warnings,
        stream: canon.stream,
        client_wants_usage: canon.include_usage,
        extra_headers: vec![],
        tool_names,
    })
}

/// `x-router-warnings` 的值：只含字段名（从 warning 文本里提取形如 `max_tokens`、`tool_result.is_error` 的标识符），
/// 去重、限长，保证是合法的头值。没有可暴露的字段名时返回 None。
pub(crate) fn warnings_header(warnings: &[String]) -> Option<HeaderValue> {
    let mut names: Vec<String> = Vec::new();
    for w in warnings {
        let tail = w.strip_prefix("unsupported field ignored:").map(str::trim);
        let candidates: Vec<&str> = match tail {
            Some(t) => vec![t],
            None => w.split_whitespace().collect(),
        };
        for tok in candidates {
            let tok = tok.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.' | '(' | ')' | '`' | '"' | '\''));
            let looks_like_field = !tok.is_empty()
                && tok.len() <= 64
                && tok.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-' | '[' | ']'))
                && (tail.is_some() || tok.contains('_'));
            if looks_like_field && !names.iter().any(|n| n == tok) {
                names.push(tok.to_string());
            }
        }
    }
    names.truncate(20);
    if names.is_empty() {
        return None;
    }
    HeaderValue::from_str(&names.join(",")).ok()
}

pub(crate) struct ConvertedResponse {
    pub body: Bytes,
    pub usage: Usage,
    pub output_bytes: usize,
}

/// 上游只支持流式，而客户端请求非流式时，把已完整解码的结果编码成客户端协议的 JSON。
pub(crate) fn encode_collected_response(client: ProtocolKind, canonical: &CanonicalResponse) -> ConvertedResponse {
    let value = match client {
        ProtocolKind::OpenAiChat => openai_chat::encode_response(canonical),
        ProtocolKind::AnthropicMessages => anthropic::encode_response(canonical),
        ProtocolKind::OpenAiResponses => openai_responses::encode_response(canonical),
        ProtocolKind::Gemini => gemini::encode_response(canonical),
        ProtocolKind::Ollama => ollama::encode_response(canonical),
        ProtocolKind::Vertex => unreachable!("Vertex is upstream-only"),
    };
    ConvertedResponse {
        body: Bytes::from(value.to_string()),
        usage: canonical.usage.clone(),
        output_bytes: canonical.content.iter().map(ContentPart::text_bytes).sum(),
    }
}

/// 非流式 2xx 响应：解析（用于记账）并按需转换。解析失败返回 502 风格的错误。
/// `names` 是编码请求时得到的 Gemini / Vertex 工具名映射。
pub(crate) fn convert_response(
    client: ProtocolKind,
    upstream: ProtocolKind,
    passthrough: bool,
    raw: &Bytes,
    names: &ToolNameMap,
) -> Result<ConvertedResponse, ApiError> {
    let malformed = || ApiError::new(ApiErrorKind::BadGateway, "The upstream provider returned a response that could not be parsed.").with_code("bad_upstream_response");
    let mut value: Value = serde_json::from_slice(raw).map_err(|_| malformed())?;
    let restored = crate::upstream::opencode_compat::restore_tool_names(&mut value, names);
    let restored_raw = if restored { Bytes::from(value.to_string()) } else { raw.clone() };
    let canonical = match upstream {
        ProtocolKind::OpenAiChat => openai_chat::decode_response(&value),
        ProtocolKind::AnthropicMessages => anthropic::decode_response(&value),
        ProtocolKind::OpenAiResponses => openai_responses::decode_response(&value),
        ProtocolKind::Gemini | ProtocolKind::Vertex => gemini::decode_response_mapped(&value, names),
        ProtocolKind::Ollama => ollama::decode_response(&value),
    };
    if passthrough {
        // 透传：响应体原样返回；解析只用于用量。无法按协议解析时仍然透传，用量退回估算。
        return Ok(match canonical {
            Ok(c) => ConvertedResponse {
                body: restored_raw,
                output_bytes: c.content.iter().map(ContentPart::text_bytes).sum(),
                usage: c.usage,
            },
            Err(_) => ConvertedResponse { body: restored_raw, usage: Usage::default(), output_bytes: raw.len() },
        });
    }
    let canonical = canonical.map_err(|_| malformed())?;
    let encoded = match client {
        ProtocolKind::OpenAiChat => openai_chat::encode_response(&canonical),
        ProtocolKind::AnthropicMessages => anthropic::encode_response(&canonical),
        ProtocolKind::OpenAiResponses => openai_responses::encode_response(&canonical),
        ProtocolKind::Gemini => gemini::encode_response(&canonical),
        ProtocolKind::Ollama => ollama::encode_response(&canonical),
        ProtocolKind::Vertex => return Err(malformed()),
    };
    Ok(ConvertedResponse {
        body: Bytes::from(serde_json::to_vec(&encoded).map_err(|_| malformed())?),
        output_bytes: canonical.content.iter().map(ContentPart::text_bytes).sum(),
        usage: canonical.usage,
    })
}

// ───────────────────────── 上游错误 → 客户端错误 ─────────────────────────

pub(crate) fn kind_for_status(status: u16) -> ApiErrorKind {
    match status {
        400 | 409 | 422 => ApiErrorKind::InvalidRequest,
        401 => ApiErrorKind::Authentication,
        402 | 403 => ApiErrorKind::PermissionDenied,
        404 => ApiErrorKind::NotFound,
        413 => ApiErrorKind::PayloadTooLarge,
        429 => ApiErrorKind::RateLimit,
        408 | 504 => ApiErrorKind::Timeout,
        s if (400..500).contains(&s) => ApiErrorKind::InvalidRequest,
        _ => ApiErrorKind::BadGateway,
    }
}

/// 擦除我们自己发出的秘密值（有些上游会在错误信息里回显密钥）。
pub(crate) fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets {
        if !s.is_empty() {
            out = out.replace(s.as_str(), "[redacted]");
        }
    }
    out
}

fn extract_message(body: &[u8]) -> (String, Option<String>) {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        let err = v.get("error");
        let message = err
            .and_then(|e| e.get("message"))
            .or_else(|| v.get("message"))
            .or_else(|| err.filter(|e| e.is_string()))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let code = err
            .and_then(|e| e.get("code").or_else(|| e.get("type")))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(m) = message {
            return (m, code);
        }
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        (String::new(), None)
    } else {
        (text.chars().take(500).collect(), None)
    }
}

/// 上游错误体在客户端协议下是否已是合法形状。
fn is_valid_error_shape(protocol: ProtocolKind, v: &Value) -> bool {
    match protocol {
        ProtocolKind::AnthropicMessages => {
            v.get("type").and_then(Value::as_str) == Some("error") && v.get("error").is_some_and(Value::is_object)
        }
        ProtocolKind::Gemini | ProtocolKind::Vertex => {
            v.get("error").is_some_and(|e| e.get("message").is_some_and(Value::is_string))
        }
        ProtocolKind::Ollama => v.get("error").is_some_and(Value::is_string),
        _ => v.get("error").is_some_and(|e| e.is_object() || e.is_string()),
    }
}

/// Gemini / Vertex / Ollama / Responses 上游的错误体由协议层的 `decode_error` 解码（Chat 与 Anthropic 沿用 [`extract_message`]）。
/// 非 JSON 文本只保留前 500 个字符，HTML 一律不回显。
fn decode_upstream_error(upstream: ProtocolKind, status: u16, body: &[u8]) -> Option<ApiError> {
    let parsed = match serde_json::from_slice::<Value>(body) {
        Ok(v) => v,
        Err(_) => {
            let text = String::from_utf8_lossy(body);
            let text = text.trim();
            if text.is_empty() || text.starts_with('<') {
                Value::Null
            } else {
                Value::String(text.chars().take(500).collect())
            }
        }
    };
    Some(match upstream {
        ProtocolKind::Gemini | ProtocolKind::Vertex => gemini::decode_error(status, &parsed),
        ProtocolKind::Ollama => ollama::decode_error(status, &parsed),
        ProtocolKind::OpenAiResponses => openai_responses::decode_error(status, &parsed),
        ProtocolKind::OpenAiChat | ProtocolKind::AnthropicMessages => return None,
    })
}

/// 把上游的非 2xx 响应转成要写给客户端的 `(状态码, 错误体字节)`。
pub(crate) fn upstream_error_body(
    client: ProtocolKind,
    upstream: ProtocolKind,
    status: u16,
    body: &[u8],
    secrets: &[String],
) -> (u16, Bytes) {
    if (200..400).contains(&status) || !(100..600).contains(&status) {
        // 重定向等：出站不跟随，base URL 多半配错了
        let err = ApiError::new(
            ApiErrorKind::BadGateway,
            format!("The upstream provider answered with an unexpected HTTP status ({status}). Check the provider's base URL."),
        )
        .with_code("unexpected_upstream_status");
        return (502, Bytes::from(err.to_body(client).to_string()));
    }
    if client == upstream {
        if let Ok(v) = serde_json::from_slice::<Value>(body) {
            if is_valid_error_shape(client, &v) {
                let text = scrub(&v.to_string(), secrets);
                return (status, Bytes::from(text));
            }
        }
    }
    if let Some(decoded) = decode_upstream_error(upstream, status, body) {
        let mut err = ApiError::new(kind_for_status(status), scrub(&decoded.message, secrets));
        if let Some(c) = decoded.code {
            err = err.with_code(scrub(&c, secrets));
        }
        return (status, Bytes::from(err.to_body(client).to_string()));
    }
    let (message, code) = extract_message(body);
    let message = if message.is_empty() { format!("The upstream provider returned HTTP {status}.") } else { message };
    let mut err = ApiError::new(kind_for_status(status), scrub(&message, secrets));
    if let Some(c) = code {
        err = err.with_code(scrub(&c, secrets));
    }
    (status, Bytes::from(err.to_body(client).to_string()))
}

/// 流内错误帧的消息：固定文案，不转述上游内容。
pub(crate) fn stream_error(kind: ApiErrorKind, message: &str) -> ApiError {
    ApiError::new(kind, message.to_string()).with_code("upstream_stream_error")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn passthrough_only_rewrites_model_and_keeps_unknown_fields() {
        let body = json!({
            "model": "alias/x", "messages": [{"role": "user", "content": "q"}],
            "frequency_penalty": 0.5, "vendor_extension": {"deep": [1, 2, 3]}, "stream": true,
            "stream_options": {"include_usage": true}
        });
        let mut h = HeaderMap::new();
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let out = build_outbound(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, body.clone(), "gpt-real", &h, true).unwrap();
        assert!(out.passthrough && out.stream && out.client_wants_usage && out.warnings.is_empty());
        assert_eq!(out.body["model"], "gpt-real");
        let mut expected = body;
        expected["model"] = json!("gpt-real");
        assert_eq!(out.body, expected);
        assert!(out.extra_headers.is_empty(), "OpenAI passthrough does not forward anthropic headers");
    }

    #[test]
    fn anthropic_passthrough_forwards_version_and_beta_headers() {
        let mut h = HeaderMap::new();
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        h.insert("anthropic-beta", HeaderValue::from_static("tools-2024-04-04"));
        h.insert("x-other", HeaderValue::from_static("nope"));
        let body = json!({"model": "a", "max_tokens": 5, "messages": [], "cache_control_hint": 1});
        let out = build_outbound(ProtocolKind::AnthropicMessages, ProtocolKind::AnthropicMessages, body, "claude-real", &h, false).unwrap();
        let names: Vec<&str> = out.extra_headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["anthropic-version", "anthropic-beta"]);
        assert_eq!(out.body["cache_control_hint"], 1);
    }

    #[test]
    fn mid_conversation_system_is_folded_into_nearest_user_turn() {
        let body = json!({"model":"alias", "max_tokens":64, "system":"stable prefix", "messages":[
            {"role":"user", "content":"question"},
            {"role":"system", "content":"changing reminder"},
            {"role":"assistant", "content":"answer"},
            {"role":"system", "content":[{"type":"text", "text":"next reminder"}]},
            {"role":"user", "content":"follow up"}
        ]});
        let out = build_outbound(ProtocolKind::AnthropicMessages, ProtocolKind::AnthropicMessages, body, "claude-real", &HeaderMap::new(), false).unwrap();
        assert_eq!(out.body["system"], "stable prefix");
        assert_eq!(out.body["messages"][0]["content"][1]["text"], "changing reminder");
        assert_eq!(out.body["messages"][2]["role"], "user");
        assert_eq!(out.body["messages"][2]["content"][0]["text"], "next reminder");
        assert!(out.body["messages"].as_array().unwrap().iter().all(|m| m["role"] != "system"));
        let converted = build_outbound(ProtocolKind::AnthropicMessages, ProtocolKind::OpenAiChat, out.body, "other", &HeaderMap::new(), false).unwrap();
        assert_eq!(converted.body["messages"][0]["role"], "system");
    }

    #[test]
    fn cross_protocol_goes_through_canonical_and_reports_warnings() {
        let body = json!({
            "model": "alias", "max_tokens": 64, "stream": true,
            "system": "be brief",
            "messages": [{"role": "user", "content": "hello there"}],
            "metadata": {"user_id": "u"}, "top_k": 5, "weird_field": 1
        });
        let out = build_outbound(ProtocolKind::AnthropicMessages, ProtocolKind::OpenAiChat, body, "gpt-real", &HeaderMap::new(), true).unwrap();
        assert!(!out.passthrough && out.stream);
        assert_eq!(out.body["model"], "gpt-real");
        assert_eq!(out.body["messages"][0]["role"], "system");
        assert!(out.body.get("weird_field").is_none(), "unknown fields are not blindly copied across protocols");
        let h = warnings_header(&out.warnings).expect("warnings expected");
        let s = h.to_str().unwrap();
        assert!(s.contains("weird_field") && s.contains("top_k"), "{s}");
        assert!(!s.contains("hello"), "never body text");
    }

    #[test]
    fn invalid_cross_protocol_request_is_400() {
        let e = build_outbound(ProtocolKind::AnthropicMessages, ProtocolKind::OpenAiChat, json!({"model": "m", "messages": []}), "x", &HeaderMap::new(), false)
            .err()
            .unwrap();
        assert_eq!(e.status(), 400);
    }

    #[test]
    fn warnings_header_contains_only_field_names() {
        let w = vec![
            "unsupported field ignored: logit_bias".to_string(),
            "max_tokens not provided; Anthropic requires it, defaulted to 4096".to_string(),
            "presence_penalty/frequency_penalty/seed are not supported by Anthropic Messages and were dropped".to_string(),
            "unsupported field ignored: logit_bias".to_string(),
        ];
        let h = warnings_header(&w).unwrap();
        assert_eq!(h.to_str().unwrap(), "logit_bias,max_tokens,presence_penalty/frequency_penalty/seed");
        assert!(warnings_header(&[]).is_none());
        assert!(warnings_header(&["images inside blocks were moved".to_string()]).is_none());
    }

    #[test]
    fn error_body_same_protocol_is_preserved_but_scrubbed() {
        let body = br#"{"error":{"message":"bad key sk-live-123456","type":"invalid_request_error","param":"model","code":"model_not_found"}}"#;
        let (status, out) = upstream_error_body(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, 404, body, &["sk-live-123456".into()]);
        assert_eq!(status, 404);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["error"]["param"], "model");
        assert_eq!(v["error"]["code"], "model_not_found");
        assert!(!v.to_string().contains("sk-live-123456"));
    }

    #[test]
    fn error_body_is_rewrapped_across_protocols_and_for_odd_shapes() {
        let anth = br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let (status, out) = upstream_error_body(ProtocolKind::OpenAiChat, ProtocolKind::AnthropicMessages, 429, anth, &[]);
        assert_eq!(status, 429);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["error"]["message"], "slow down");
        assert_eq!(v["error"]["type"], "rate_limit_error");
        assert_eq!(v["error"]["code"], "rate_limit_error");

        let oa = br#"{"error":{"message":"overloaded","type":"server_error"}}"#;
        let (status, out) = upstream_error_body(ProtocolKind::AnthropicMessages, ProtocolKind::OpenAiChat, 503, oa, &[]);
        assert_eq!(status, 503);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!((v["type"].as_str(), v["error"]["message"].as_str()), (Some("error"), Some("overloaded")));

        // 非 JSON / HTML：给出通用文案而不是回显 HTML
        let (status, out) = upstream_error_body(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, 502, b"<html>bad gateway</html>", &[]);
        assert_eq!(status, 502);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["error"]["message"], "The upstream provider returned HTTP 502.");

        // 同协议但形状不对
        let (_, out) = upstream_error_body(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, 422, br#"{"detail":"nope"}"#, &[]);
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["error"]["message"], "nope");
        assert_eq!(v["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn redirect_status_becomes_502() {
        let (status, out) = upstream_error_body(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, 302, b"", &[]);
        assert_eq!(status, 502);
        assert!(String::from_utf8_lossy(&out).contains("base URL"));
    }

    #[test]
    fn non_stream_conversion_both_directions() {
        let oa = json!({"id":"c1","model":"gpt","choices":[{"message":{"role":"assistant","content":"hi there","reasoning":"because"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":5,"completion_tokens":3}});
        let raw = Bytes::from(oa.to_string());
        let r = convert_response(ProtocolKind::AnthropicMessages, ProtocolKind::OpenAiChat, false, &raw, &ToolNameMap::default()).unwrap();
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["type"], "thinking");
        assert_eq!(v["content"][1]["text"], "hi there");
        assert_eq!(v["usage"]["input_tokens"], 5);
        assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (Some(5), Some(3)));

        let r = convert_response(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, true, &raw, &ToolNameMap::default()).unwrap();
        assert_eq!(r.body, raw, "passthrough returns the upstream bytes untouched");
        assert!(convert_response(ProtocolKind::OpenAiChat, ProtocolKind::OpenAiChat, true, &Bytes::from_static(b"<html>"), &ToolNameMap::default()).is_err());
    }
}

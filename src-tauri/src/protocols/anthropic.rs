//! Anthropic Messages（`POST /v1/messages`）编解码。
//!
//! - 请求：`decode_request`（客户端 → canonical，`max_tokens` 必填）、`encode_request`（canonical → Anthropic 上游）。
//! - 响应：`encode_response`、`decode_response`。
//! - 流式：`AnthropicStreamEncoder`，状态机保证
//!   `message_start → (content_block_start → content_block_delta* → content_block_stop)* → message_delta → message_stop`。
//!
//! 取舍：
//! - 顶层 `system` 可为字符串或 text block 数组；`cache_control` 不进入 canonical，解码时产生 warning。
//! - `input_tokens` 不含缓存读写：解码时 `canonical.input_tokens = input + cache_read + cache_creation`，
//!   编码时反向相减（见 `canonical.rs` 顶部说明）。
//! - 非 `custom` 类型的服务端工具（web_search、bash 等）、`document`、`redacted_thinking`、`server_tool_use`
//!   等返回 `Unsupported`，不静默丢弃。
//! - 从 canonical 编码请求时，相邻同角色消息会合并，且 `tool_result` 始终排在用户消息最前（Anthropic 的硬性要求）。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::canonical::*;
use super::common::*;
use super::{Encoded, ProtocolError, ProtocolResult, SseFrame, StreamEncoder};

/// canonical 未指定 `max_tokens` 而目标是 Anthropic 时的默认值（Anthropic 要求必填）。
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

const KNOWN_FIELDS: &[&str] = &[
    "model",
    "max_tokens",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "stream",
    "metadata",
];

// ───────────────────────── 请求解码 ─────────────────────────

pub fn decode_request(body: &Value) -> ProtocolResult<CanonicalRequest> {
    let obj = as_object(body, "request body")?;
    let mut req = CanonicalRequest { model: required_str(obj, "model", "request")?.to_string(), ..Default::default() };
    if req.model.is_empty() {
        return Err(invalid("request.model must not be empty"));
    }

    let max_tokens = match obj.get("max_tokens") {
        None | Some(Value::Null) => return Err(invalid("max_tokens: Field required")),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n >= 1)
            .ok_or_else(|| invalid("max_tokens: Input should be a positive integer"))?,
    };

    if let Some(sys) = obj.get("system").filter(|v| !v.is_null()) {
        req.system = decode_system(sys, &mut req.warnings)?;
    }

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("messages: Field required (array)"))?;
    if messages.is_empty() {
        return Err(invalid("messages: at least one message is required"));
    }
    for (i, m) in messages.iter().enumerate() {
        let what = format!("messages[{i}]");
        let mo = as_object(m, &what)?;
        let role = match required_str(mo, "role", &what)? {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            other => return Err(invalid(format!("{what}.role `{other}` must be `user` or `assistant`"))),
        };
        let content = decode_content(mo.get("content"), role, &what, &mut req.warnings)?;
        req.messages.push(Message { role, content });
    }

    req.stream = opt_bool(obj, "stream", "request")?.unwrap_or(false);
    req.params = GenerationParams {
        max_tokens: Some(max_tokens),
        temperature: opt_f64(obj, "temperature", "request")?,
        top_p: opt_f64(obj, "top_p", "request")?,
        top_k: opt_u32(obj, "top_k", "request")?,
        stop: match obj.get("stop_sequences") {
            None | Some(Value::Null) => vec![],
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string).ok_or_else(|| invalid("stop_sequences must contain only strings")))
                .collect::<ProtocolResult<_>>()?,
            Some(_) => return Err(invalid("stop_sequences must be an array of strings")),
        },
        ..Default::default()
    };
    req.user = obj.get("metadata").and_then(Value::as_object).and_then(|m| opt_str(m, "user_id"));

    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        req.tools = decode_tools(tools, &mut req.warnings)?;
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        let (choice, disable_parallel) = decode_tool_choice(tc)?;
        req.tool_choice = Some(choice);
        if disable_parallel == Some(true) {
            req.parallel_tool_calls = Some(false);
        }
    }

    warn_unknown_fields(obj, KNOWN_FIELDS, &mut req.warnings);
    Ok(req)
}

fn note_cache_control(o: &Map<String, Value>, warnings: &mut Vec<String>) {
    if o.contains_key("cache_control") {
        let w = "cache_control is not supported and was ignored";
        if !warnings.iter().any(|x| x == w) {
            warnings.push(w.to_string());
        }
    }
}

fn decode_system(v: &Value, warnings: &mut Vec<String>) -> ProtocolResult<Vec<String>> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(blocks) => blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let what = format!("system[{i}]");
                let o = as_object(b, &what)?;
                if o.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(ProtocolError::Unsupported(format!("{what}.type (only `text` blocks are supported)")));
                }
                note_cache_control(o, warnings);
                Ok(required_str(o, "text", &what)?.to_string())
            })
            .collect(),
        _ => Err(invalid("system must be a string or an array of text blocks")),
    }
}

fn decode_content(
    content: Option<&Value>,
    role: Role,
    what: &str,
    warnings: &mut Vec<String>,
) -> ProtocolResult<Vec<ContentPart>> {
    match content {
        Some(Value::String(s)) => Ok(vec![ContentPart::text(s.clone())]),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .enumerate()
            .map(|(i, b)| decode_block(b, role, &format!("{what}.content[{i}]"), warnings))
            .collect(),
        _ => Err(invalid(format!("{what}.content: Field required (string or array)"))),
    }
}

fn decode_image_source(v: Option<&Value>, what: &str) -> ProtocolResult<ImageSource> {
    let s = as_object(v.ok_or_else(|| invalid(format!("{what}.source is required")))?, &format!("{what}.source"))?;
    let sw = format!("{what}.source");
    match s.get("type").and_then(Value::as_str) {
        Some("base64") => Ok(ImageSource::Base64 {
            media_type: required_str(s, "media_type", &sw)?.to_string(),
            data: required_str(s, "data", &sw)?.to_string(),
        }),
        Some("url") => Ok(ImageSource::Url { url: required_str(s, "url", &sw)?.to_string() }),
        Some("file") => Ok(ImageSource::FileId { file_id: required_str(s, "file_id", &sw)?.to_string() }),
        other => Err(ProtocolError::Unsupported(format!("{sw}.type `{}`", other.unwrap_or("?")))),
    }
}

fn decode_block(b: &Value, role: Role, what: &str, warnings: &mut Vec<String>) -> ProtocolResult<ContentPart> {
    let o = as_object(b, what)?;
    let ty = o.get("type").and_then(Value::as_str).ok_or_else(|| invalid(format!("{what}.type is required")))?;
    note_cache_control(o, warnings);
    match ty {
        "text" => Ok(ContentPart::text(required_str(o, "text", what)?)),
        "image" => Ok(ContentPart::Image { source: decode_image_source(o.get("source"), what)?, detail: None }),
        "tool_use" => {
            if role != Role::Assistant {
                return Err(invalid(format!("{what}: tool_use blocks are only allowed in assistant messages")));
            }
            Ok(ContentPart::ToolUse {
                id: required_str(o, "id", what)?.to_string(),
                name: required_str(o, "name", what)?.to_string(),
                input: o.get("input").cloned().unwrap_or_else(|| json!({})),
            })
        }
        "tool_result" => {
            if role != Role::User {
                return Err(invalid(format!("{what}: tool_result blocks are only allowed in user messages")));
            }
            let content = match o.get("content") {
                None | Some(Value::Null) => vec![],
                Some(Value::String(s)) => vec![ContentPart::text(s.clone())],
                Some(Value::Array(inner)) => inner
                    .iter()
                    .enumerate()
                    .map(|(j, ib)| {
                        let iw = format!("{what}.content[{j}]");
                        match ib.get("type").and_then(Value::as_str) {
                            Some("text") | Some("image") => decode_block(ib, Role::User, &iw, warnings),
                            other => Err(ProtocolError::Unsupported(format!(
                                "{iw}.type `{}` inside tool_result",
                                other.unwrap_or("?")
                            ))),
                        }
                    })
                    .collect::<ProtocolResult<_>>()?,
                Some(_) => return Err(invalid(format!("{what}.content must be a string or an array"))),
            };
            Ok(ContentPart::ToolResult {
                tool_use_id: required_str(o, "tool_use_id", what)?.to_string(),
                content,
                is_error: opt_bool(o, "is_error", what)?.unwrap_or(false),
            })
        }
        "thinking" => Ok(ContentPart::Thinking {
            text: required_str(o, "thinking", what)?.to_string(),
            signature: opt_str(o, "signature"),
        }),
        other => Err(ProtocolError::Unsupported(format!("{what}.type `{other}`"))),
    }
}

fn decode_tools(v: &Value, warnings: &mut Vec<String>) -> ProtocolResult<Vec<ToolDefinition>> {
    let arr = v.as_array().ok_or_else(|| invalid("tools must be an array"))?;
    arr.iter()
        .enumerate()
        .map(|(i, t)| {
            let what = format!("tools[{i}]");
            let o = as_object(t, &what)?;
            match o.get("type").and_then(Value::as_str) {
                None | Some("custom") => {}
                Some(other) => {
                    return Err(ProtocolError::Unsupported(format!(
                        "{what}.type `{other}` (Anthropic server-side tools are not supported)"
                    )))
                }
            }
            note_cache_control(o, warnings);
            Ok(ToolDefinition {
                name: required_str(o, "name", &what)?.to_string(),
                description: opt_str(o, "description"),
                input_schema: o
                    .get("input_schema")
                    .cloned()
                    .ok_or_else(|| invalid(format!("{what}.input_schema is required")))?,
            })
        })
        .collect()
}

fn decode_tool_choice(v: &Value) -> ProtocolResult<(ToolChoice, Option<bool>)> {
    let o = as_object(v, "tool_choice")?;
    let disable_parallel = opt_bool(o, "disable_parallel_tool_use", "tool_choice")?;
    let choice = match required_str(o, "type", "tool_choice")? {
        "auto" => ToolChoice::Auto,
        "any" => ToolChoice::Any,
        "none" => ToolChoice::None,
        "tool" => ToolChoice::Tool { name: required_str(o, "name", "tool_choice")?.to_string() },
        other => return Err(invalid(format!("tool_choice.type `{other}` is not valid"))),
    };
    Ok((choice, disable_parallel))
}

// ───────────────────────── 请求编码（发往 Anthropic 上游） ─────────────────────────

fn image_block(source: &ImageSource) -> Value {
    match source {
        ImageSource::Base64 { media_type, data } => {
            json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}})
        }
        ImageSource::Url { url } => json!({"type": "image", "source": {"type": "url", "url": url}}),
        ImageSource::FileId { file_id } => json!({"type": "image", "source": {"type": "file", "file_id": file_id}}),
    }
}

fn encode_block(part: &ContentPart) -> Value {
    match part {
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::Image { source, .. } => image_block(source),
        ContentPart::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        ContentPart::ToolResult { tool_use_id, content, is_error } => {
            let mut o = Map::new();
            o.insert("type".into(), json!("tool_result"));
            o.insert("tool_use_id".into(), json!(tool_use_id));
            if !content.is_empty() {
                let inner: Vec<Value> = content.iter().map(encode_block).collect();
                o.insert("content".into(), Value::Array(inner));
            }
            if *is_error {
                o.insert("is_error".into(), json!(true));
            }
            Value::Object(o)
        }
        ContentPart::Thinking { text, signature } => {
            let mut o = Map::new();
            o.insert("type".into(), json!("thinking"));
            o.insert("thinking".into(), json!(text));
            if let Some(s) = signature {
                o.insert("signature".into(), json!(s));
            }
            Value::Object(o)
        }
    }
}

pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let mut warnings = Vec::new();

    // 合并相邻同角色消息；用户消息中 tool_result 排在最前（稳定排序）。
    // Gemini / Responses 产生的思考签名对 Anthropic 无效，发过去会被整个请求拒绝，因此丢弃这类思考块。
    let mut dropped_foreign_thinking = false;
    let mut merged: Vec<(Role, Vec<&ContentPart>)> = Vec::new();
    for m in &req.messages {
        let parts: Vec<&ContentPart> = m
            .content
            .iter()
            .filter(|p| match p {
                ContentPart::Thinking { signature: Some(s), .. } if signature::is_foreign(s) => {
                    dropped_foreign_thinking = true;
                    false
                }
                _ => true,
            })
            .collect();
        if parts.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((role, existing)) if *role == m.role => existing.extend(parts),
            _ => merged.push((m.role, parts)),
        }
    }
    if dropped_foreign_thinking {
        warnings.push("thinking blocks carrying another provider's signature were not sent to Anthropic".into());
    }
    let messages: Vec<Value> = merged
        .into_iter()
        .map(|(role, mut parts)| {
            if role == Role::User {
                parts.sort_by_key(|p| !matches!(p, ContentPart::ToolResult { .. }));
            }
            let blocks: Vec<Value> = parts.into_iter().map(encode_block).collect();
            json!({"role": if role == Role::User { "user" } else { "assistant" }, "content": blocks})
        })
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    let max_tokens = match req.params.max_tokens {
        Some(v) => v,
        None => {
            warnings.push(format!("max_tokens not provided; Anthropic requires it, defaulted to {DEFAULT_MAX_TOKENS}"));
            DEFAULT_MAX_TOKENS
        }
    };
    body.insert("max_tokens".into(), json!(max_tokens));
    body.insert("messages".into(), Value::Array(messages));
    match req.system.len() {
        0 => {}
        1 => {
            body.insert("system".into(), json!(req.system[0]));
        }
        _ => {
            let blocks: Vec<Value> = req.system.iter().map(|s| json!({"type": "text", "text": s})).collect();
            body.insert("system".into(), Value::Array(blocks));
        }
    }
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    let p = &req.params;
    if let Some(v) = p.temperature {
        body.insert("temperature".into(), json!(v));
    }
    if let Some(v) = p.top_p {
        body.insert("top_p".into(), json!(v));
    }
    if let Some(v) = p.top_k {
        body.insert("top_k".into(), json!(v));
    }
    if !p.stop.is_empty() {
        body.insert("stop_sequences".into(), json!(p.stop));
    }
    if p.presence_penalty.is_some() || p.frequency_penalty.is_some() || p.seed.is_some() {
        warnings.push("presence_penalty/frequency_penalty/seed are not supported by Anthropic Messages and were dropped".into());
    }
    if let Some(u) = &req.user {
        body.insert("metadata".into(), json!({"user_id": u}));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut o = Map::new();
                o.insert("name".into(), json!(t.name));
                if let Some(d) = &t.description {
                    o.insert("description".into(), json!(d));
                }
                o.insert("input_schema".into(), t.input_schema.clone());
                Value::Object(o)
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    let disable_parallel = req.parallel_tool_calls == Some(false) && !req.tools.is_empty();
    let choice = match &req.tool_choice {
        Some(ToolChoice::Auto) => Some(json!({"type": "auto"})),
        Some(ToolChoice::None) => Some(json!({"type": "none"})),
        Some(ToolChoice::Any) => Some(json!({"type": "any"})),
        Some(ToolChoice::Tool { name }) => Some(json!({"type": "tool", "name": name})),
        None if disable_parallel => Some(json!({"type": "auto"})),
        None => None,
    };
    if let Some(mut c) = choice {
        if disable_parallel && c["type"] != "none" {
            c["disable_parallel_tool_use"] = json!(true);
        }
        body.insert("tool_choice".into(), c);
    }
    Ok(Encoded { body: Value::Object(body), warnings })
}

// ───────────────────────── 响应编码 / 解码 ─────────────────────────

fn stop_reason_str(r: &StopReason) -> &str {
    match r {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::StopSequence => "stop_sequence",
        StopReason::ToolUse => "tool_use",
        StopReason::ContentFilter => "refusal",
        StopReason::Other(s) if s == "pause_turn" => "pause_turn",
        StopReason::Other(_) => "end_turn",
    }
}

pub fn decode_stop_reason(v: Option<&Value>) -> StopReason {
    match v.and_then(Value::as_str) {
        None | Some("end_turn") => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("tool_use") => StopReason::ToolUse,
        Some("refusal") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(other.to_string()),
    }
}

fn usage_value(u: &Usage, include_input: bool) -> Value {
    let mut o = Map::new();
    if include_input {
        let cached = u.cache_read_tokens.unwrap_or(0) + u.cache_creation_tokens.unwrap_or(0);
        o.insert("input_tokens".into(), json!(u.input_tokens.unwrap_or(0).saturating_sub(cached)));
    }
    o.insert("output_tokens".into(), json!(u.output_tokens.unwrap_or(0)));
    if include_input {
        if let Some(c) = u.cache_read_tokens {
            o.insert("cache_read_input_tokens".into(), json!(c));
        }
        if let Some(c) = u.cache_creation_tokens {
            o.insert("cache_creation_input_tokens".into(), json!(c));
        }
    }
    Value::Object(o)
}

pub fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(o) = v.and_then(Value::as_object) else {
        return Usage::default();
    };
    let get = |k: &str| o.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    let (input, read, create) = (get("input_tokens"), get("cache_read_input_tokens"), get("cache_creation_input_tokens"));
    let total = input.map(|i| i + read.unwrap_or(0) + create.unwrap_or(0));
    Usage {
        input_tokens: total,
        output_tokens: get("output_tokens"),
        cache_read_tokens: read,
        cache_creation_tokens: create,
    }
}

pub fn encode_response(resp: &CanonicalResponse) -> Value {
    let content: Vec<Value> = resp
        .content
        .iter()
        .filter(|p| match p {
            // 只携带外来签名、没有文本的思考块是其它协议的内部载体，Anthropic 客户端用不上
            ContentPart::Thinking { text, signature } => {
                !(text.is_empty() && signature.as_deref().is_some_and(signature::is_foreign))
            }
            ContentPart::Text { .. } | ContentPart::ToolUse { .. } => true,
            _ => false,
        })
        .map(encode_block)
        .collect();
    json!({
        "id": if resp.id.is_empty() { format!("msg_{}", uuid::Uuid::new_v4().simple()) } else { resp.id.clone() },
        "type": "message",
        "role": "assistant",
        "model": resp.model,
        "content": content,
        "stop_reason": stop_reason_str(&resp.stop_reason),
        "stop_sequence": resp.stop_sequence,
        "usage": usage_value(&resp.usage, true),
    })
}

pub fn decode_response(body: &Value) -> ProtocolResult<CanonicalResponse> {
    let obj = body.as_object().ok_or_else(|| invalid_resp("response body must be an object"))?;
    let blocks = obj
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_resp("response.content is missing"))?;
    let mut warnings = Vec::new();
    let content = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| decode_block(b, Role::Assistant, &format!("content[{i}]"), &mut warnings))
        .collect::<ProtocolResult<Vec<_>>>()
        .map_err(|e| match e {
            ProtocolError::InvalidRequest(m) => ProtocolError::InvalidResponse(m),
            other => other,
        })?;
    Ok(CanonicalResponse {
        id: opt_str(obj, "id").unwrap_or_default(),
        model: opt_str(obj, "model").unwrap_or_default(),
        created: None,
        content,
        stop_reason: decode_stop_reason(obj.get("stop_reason")),
        stop_sequence: opt_str(obj, "stop_sequence"),
        usage: decode_usage(obj.get("usage")),
    })
}

// ───────────────────────── 流式编码状态机 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
    Tool,
}

#[derive(Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Streaming,
    Done,
}

pub struct AnthropicStreamEncoder {
    phase: Phase,
    open: BTreeMap<u32, BlockKind>,
    seen: std::collections::HashSet<u32>,
    usage: Usage,
}

impl Default for AnthropicStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicStreamEncoder {
    pub fn new() -> Self {
        Self { phase: Phase::Idle, open: BTreeMap::new(), seen: Default::default(), usage: Usage::default() }
    }

    fn frame(event: &str, data: Value) -> SseFrame {
        SseFrame::event(event, data.to_string())
    }

    fn open_block(&self, index: u32, expected: Option<BlockKind>, what: &str) -> ProtocolResult<BlockKind> {
        let kind = *self
            .open
            .get(&index)
            .ok_or_else(|| invalid_resp(format!("{what} for block {index} which is not open")))?;
        if let Some(e) = expected {
            if e != kind {
                return Err(invalid_resp(format!("{what} does not match the type of block {index}")));
            }
        }
        Ok(kind)
    }
}

impl StreamEncoder for AnthropicStreamEncoder {
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>> {
        if self.phase == Phase::Done {
            return Err(invalid_resp("stream event received after the stream ended"));
        }
        if let StreamEvent::Error { kind, message } = &event {
            self.phase = Phase::Done;
            return Ok(vec![super::ApiError::new(*kind, message.clone()).to_sse_frame(super::ProtocolKind::AnthropicMessages)]);
        }
        if self.phase == Phase::Idle {
            let StreamEvent::MessageStart { id, model, usage } = event else {
                return Err(invalid_resp("first stream event must be message_start"));
            };
            self.phase = Phase::Streaming;
            if let Some(u) = &usage {
                self.usage.merge(u);
            }
            let mut start_usage = usage_value(&self.usage, true);
            start_usage["output_tokens"] = json!(0);
            return Ok(vec![Self::frame(
                "message_start",
                json!({"type": "message_start", "message": {
                    "id": if id.is_empty() { format!("msg_{}", uuid::Uuid::new_v4().simple()) } else { id },
                    "type": "message", "role": "assistant", "model": model,
                    "content": [], "stop_reason": null, "stop_sequence": null,
                    "usage": start_usage,
                }}),
            )]);
        }

        match event {
            StreamEvent::MessageStart { .. } => Err(invalid_resp("duplicate message_start")),
            StreamEvent::ContentBlockStart { index, block } => {
                if !self.seen.insert(index) {
                    return Err(invalid_resp(format!("content block index {index} was already used")));
                }
                let (kind, cb) = match block {
                    BlockStart::Text => (BlockKind::Text, json!({"type": "text", "text": ""})),
                    BlockStart::Thinking => (BlockKind::Thinking, json!({"type": "thinking", "thinking": ""})),
                    BlockStart::ToolUse { id, name } => {
                        (BlockKind::Tool, json!({"type": "tool_use", "id": id, "name": name, "input": {}}))
                    }
                };
                self.open.insert(index, kind);
                Ok(vec![Self::frame(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": index, "content_block": cb}),
                )])
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                if let BlockDelta::Signature { signature: sig } = &delta {
                    if signature::is_foreign(sig) {
                        self.open_block(index, Some(BlockKind::Thinking), "content_block_delta")?;
                        return Ok(vec![]);
                    }
                }
                let (expected, d) = match delta {
                    BlockDelta::Text { text } => (BlockKind::Text, json!({"type": "text_delta", "text": text})),
                    BlockDelta::Thinking { text } => (BlockKind::Thinking, json!({"type": "thinking_delta", "thinking": text})),
                    BlockDelta::Signature { signature } => {
                        (BlockKind::Thinking, json!({"type": "signature_delta", "signature": signature}))
                    }
                };
                self.open_block(index, Some(expected), "content_block_delta")?;
                Ok(vec![Self::frame("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": d}))])
            }
            StreamEvent::ToolCallDelta { index, partial_json } => {
                self.open_block(index, Some(BlockKind::Tool), "tool call delta")?;
                Ok(vec![Self::frame(
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": index,
                           "delta": {"type": "input_json_delta", "partial_json": partial_json}}),
                )])
            }
            StreamEvent::ContentBlockStop { index } => {
                self.open_block(index, None, "content_block_stop")?;
                self.open.remove(&index);
                Ok(vec![Self::frame("content_block_stop", json!({"type": "content_block_stop", "index": index}))])
            }
            StreamEvent::Usage(u) => {
                self.usage.merge(&u);
                Ok(vec![])
            }
            StreamEvent::MessageStop { stop_reason, stop_sequence } => {
                self.phase = Phase::Done;
                // 上游（Responses / Ollama / Chat 等）的输入 token 往往要到流末尾才知道，而 `message_start`
                // 在流开头发出、只能写 0。Anthropic 的 `message_delta.usage` 是累计值，因此在输入 token
                // 已知时把 `input_tokens` 与缓存字段也放进去，客户端以它为准。
                let mut frames: Vec<SseFrame> = std::mem::take(&mut self.open)
                    .into_keys()
                    .map(|index| Self::frame("content_block_stop", json!({"type": "content_block_stop", "index": index})))
                    .collect();
                frames.push(Self::frame(
                    "message_delta",
                    json!({"type": "message_delta",
                           "delta": {"stop_reason": stop_reason_str(&stop_reason), "stop_sequence": stop_sequence},
                           "usage": usage_value(&self.usage, self.usage.input_tokens.is_some())}),
                ));
                frames.push(Self::frame("message_stop", json!({"type": "message_stop"})));
                Ok(frames)
            }
            StreamEvent::Error { .. } => unreachable!("handled above"),
        }
    }
}

#[cfg(test)]
mod stream_usage_tests {
    use super::*;

    fn run(events: Vec<StreamEvent>) -> Vec<Value> {
        let mut enc = AnthropicStreamEncoder::new();
        events
            .into_iter()
            .flat_map(|e| enc.push(e).expect("valid sequence"))
            .map(|f| serde_json::from_str::<Value>(&f.data).unwrap())
            .collect()
    }

    fn start() -> StreamEvent {
        StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None }
    }

    fn stop() -> StreamEvent {
        StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None }
    }

    /// 上游（Responses / Ollama / Chat）的输入 token 只在流末尾出现：message_start 只能写 0，
    /// message_delta 必须带上最终的 input_tokens 与缓存字段（input_tokens 不含缓存）。
    #[test]
    fn message_delta_carries_input_usage_that_only_arrived_at_the_end() {
        let frames = run(vec![
            start(),
            StreamEvent::Usage(Usage {
                input_tokens: Some(120),
                output_tokens: Some(9),
                cache_read_tokens: Some(100),
                cache_creation_tokens: Some(5),
            }),
            stop(),
        ]);
        assert_eq!(frames[0]["message"]["usage"]["input_tokens"], 0, "unknown at message_start");
        let md = frames.iter().find(|f| f["type"] == "message_delta").unwrap();
        assert_eq!(md["usage"]["input_tokens"], 15, "120 total - 100 cache read - 5 cache creation");
        assert_eq!(md["usage"]["cache_read_input_tokens"], 100);
        assert_eq!(md["usage"]["cache_creation_input_tokens"], 5);
        assert_eq!(md["usage"]["output_tokens"], 9);
    }

    #[test]
    fn message_delta_without_known_input_usage_does_not_claim_zero() {
        let frames = run(vec![start(), StreamEvent::Usage(Usage { output_tokens: Some(4), ..Default::default() }), stop()]);
        let md = frames.iter().find(|f| f["type"] == "message_delta").unwrap();
        assert_eq!(md["usage"], json!({"output_tokens": 4}), "only what is known");
    }

    #[test]
    fn message_start_usage_is_unchanged_when_input_is_known_early() {
        let frames = run(vec![
            StreamEvent::MessageStart {
                id: "m".into(),
                model: "x".into(),
                usage: Some(Usage { input_tokens: Some(30), cache_read_tokens: Some(10), ..Default::default() }),
            },
            StreamEvent::Usage(Usage { output_tokens: Some(2), ..Default::default() }),
            stop(),
        ]);
        assert_eq!(frames[0]["message"]["usage"]["input_tokens"], 20);
        assert_eq!(frames[0]["message"]["usage"]["cache_read_input_tokens"], 10);
        assert_eq!(frames[0]["message"]["usage"]["output_tokens"], 0);
        let md = frames.iter().find(|f| f["type"] == "message_delta").unwrap();
        assert_eq!(md["usage"]["input_tokens"], 20);
        assert_eq!(md["usage"]["output_tokens"], 2);
    }
}

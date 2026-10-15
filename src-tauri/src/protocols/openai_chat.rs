//! OpenAI Chat Completions（`POST /v1/chat/completions`）编解码。
//!
//! - 请求：`decode_request`（客户端 → canonical）、`encode_request`（canonical → OpenAI 兼容上游）。
//! - 响应：`encode_response`（canonical → 客户端）、`decode_response`（上游 → canonical）。
//! - 流式：`OpenAiStreamEncoder`（canonical 事件 → chunk SSE，以 `data: [DONE]` 结束）。
//!
//! 取舍（均有测试覆盖）：
//! - `developer` 角色按 `system` 处理；出现在对话中间的 system/developer 消息被提升到 `system` 并产生 warning。
//! - 连续的 `role=tool` 消息合并为一条 `User` 消息里的多个 `ToolResult`。
//! - `max_completion_tokens` 优先于 `max_tokens`；编码到上游时统一使用 `max_tokens`（兼容面最广）。
//! - `n != 1`、音频 / 文件输入、非 function 类型的工具返回 `Unsupported`；`response_format`、`logprobs`
//!   等字段进入 warnings，不会静默消失。
//! - 思考内容映射到非标准字段 `reasoning_content`（DeepSeek / OpenRouter 惯例），向上游回传历史时丢弃并告警。

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::canonical::*;
use super::common::*;
use super::{Encoded, ProtocolError, ProtocolResult, SseFrame, StreamEncoder};

const KNOWN_FIELDS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "max_tokens",
    "max_completion_tokens",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "seed",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "user",
    "n",
];

// ───────────────────────── 请求解码 ─────────────────────────

pub fn decode_request(body: &Value) -> ProtocolResult<CanonicalRequest> {
    let obj = as_object(body, "request body")?;
    let mut req = CanonicalRequest { model: required_str(obj, "model", "request")?.to_string(), ..Default::default() };
    if req.model.is_empty() {
        return Err(invalid("request.model must not be empty"));
    }

    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("request.messages is required and must be an array"))?;
    if messages.is_empty() {
        return Err(invalid("request.messages must not be empty"));
    }
    decode_messages(messages, &mut req)?;

    if let Some(n) = obj.get("n").filter(|v| !v.is_null()) {
        if n.as_u64() != Some(1) {
            return Err(ProtocolError::Unsupported("n != 1 (multiple choices) is not supported".into()));
        }
    }

    req.stream = opt_bool(obj, "stream", "request")?.unwrap_or(false);
    if let Some(so) = obj.get("stream_options").and_then(Value::as_object) {
        req.include_usage = opt_bool(so, "include_usage", "stream_options")?.unwrap_or(false);
    }
    req.params = GenerationParams {
        max_tokens: match opt_u32(obj, "max_completion_tokens", "request")? {
            Some(v) => Some(v),
            None => opt_u32(obj, "max_tokens", "request")?,
        },
        temperature: opt_f64(obj, "temperature", "request")?,
        top_p: opt_f64(obj, "top_p", "request")?,
        top_k: None,
        stop: decode_stop(obj.get("stop"))?,
        presence_penalty: opt_f64(obj, "presence_penalty", "request")?,
        frequency_penalty: opt_f64(obj, "frequency_penalty", "request")?,
        seed: obj.get("seed").and_then(Value::as_i64),
    };
    req.parallel_tool_calls = opt_bool(obj, "parallel_tool_calls", "request")?;
    req.user = opt_str(obj, "user");

    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        req.tools = decode_tools(tools)?;
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        req.tool_choice = Some(decode_tool_choice(tc)?);
    }

    warn_unknown_fields(obj, KNOWN_FIELDS, &mut req.warnings);
    Ok(req)
}

fn decode_stop(v: Option<&Value>) -> ProtocolResult<Vec<String>> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().map(str::to_string).ok_or_else(|| invalid("request.stop must contain only strings")))
            .collect(),
        Some(_) => Err(invalid("request.stop must be a string or an array of strings")),
    }
}

fn decode_tools(v: &Value) -> ProtocolResult<Vec<ToolDefinition>> {
    let arr = v.as_array().ok_or_else(|| invalid("request.tools must be an array"))?;
    arr.iter()
        .enumerate()
        .map(|(i, t)| {
            let what = format!("tools[{i}]");
            let o = as_object(t, &what)?;
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("function");
            if ty != "function" {
                return Err(ProtocolError::Unsupported(format!("{what}.type `{ty}` (only `function` tools are supported)")));
            }
            let f = o.get("function").ok_or_else(|| invalid(format!("{what}.function is required")))?;
            let f = as_object(f, &format!("{what}.function"))?;
            Ok(ToolDefinition {
                name: required_str(f, "name", &format!("{what}.function"))?.to_string(),
                description: opt_str(f, "description"),
                input_schema: f
                    .get("parameters")
                    .filter(|p| !p.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect()
}

fn decode_tool_choice(v: &Value) -> ProtocolResult<ToolChoice> {
    match v {
        Value::String(s) => match s.as_str() {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" => Ok(ToolChoice::Any),
            other => Err(invalid(format!("request.tool_choice `{other}` is not valid"))),
        },
        Value::Object(o) => {
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
            if ty != "function" {
                return Err(ProtocolError::Unsupported(format!("tool_choice.type `{ty}`")));
            }
            let name = o
                .get("function")
                .and_then(Value::as_object)
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("tool_choice.function.name is required"))?;
            Ok(ToolChoice::Tool { name: name.to_string() })
        }
        _ => Err(invalid("request.tool_choice must be a string or an object")),
    }
}

/// 把 `content`（null / 字符串 / 文本片段数组）拆成文本片段。非文本片段报错。
fn text_parts(content: Option<&Value>, what: &str) -> ProtocolResult<Vec<String>> {
    match content {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let o = as_object(p, &format!("{what}.content[{i}]"))?;
                match o.get("type").and_then(Value::as_str) {
                    Some("text") => Ok(required_str(o, "text", &format!("{what}.content[{i}]"))?.to_string()),
                    Some("refusal") => Ok(required_str(o, "refusal", &format!("{what}.content[{i}]"))?.to_string()),
                    other => Err(ProtocolError::Unsupported(format!(
                        "{what}.content[{i}].type `{}` is not allowed in this role",
                        other.unwrap_or("?")
                    ))),
                }
            })
            .collect(),
        Some(_) => Err(invalid(format!("{what}.content must be a string, an array or null"))),
    }
}

fn decode_user_content(content: Option<&Value>, what: &str) -> ProtocolResult<Vec<ContentPart>> {
    match content {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![ContentPart::text(s.clone())]),
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let pw = format!("{what}.content[{i}]");
                let o = as_object(p, &pw)?;
                match o.get("type").and_then(Value::as_str) {
                    Some("text") => Ok(ContentPart::text(required_str(o, "text", &pw)?)),
                    Some("image_url") => {
                        let (url, detail) = match o.get("image_url") {
                            Some(Value::String(s)) => (s.as_str(), None),
                            Some(Value::Object(io)) => (required_str(io, "url", &format!("{pw}.image_url"))?, opt_str(io, "detail")),
                            _ => return Err(invalid(format!("{pw}.image_url is required"))),
                        };
                        Ok(ContentPart::Image { source: image_source_from_url(url)?, detail })
                    }
                    other => Err(ProtocolError::Unsupported(format!(
                        "{pw}.type `{}` (only text and image_url are supported)",
                        other.unwrap_or("?")
                    ))),
                }
            })
            .collect(),
        Some(_) => Err(invalid(format!("{what}.content must be a string or an array"))),
    }
}

fn decode_messages(messages: &[Value], req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let mut seen_non_system = false;
    let mut last_was_tool_group = false;
    for (i, m) in messages.iter().enumerate() {
        let what = format!("messages[{i}]");
        let mo = as_object(m, &what)?;
        let role = required_str(mo, "role", &what)?;
        let mut is_tool = false;
        match role {
            "system" | "developer" => {
                let text = text_parts(mo.get("content"), &what)?.join("\n");
                if seen_non_system {
                    req.warnings.push(format!(
                        "{what}: {role} message after conversation start was hoisted to the system prompt"
                    ));
                }
                req.system.push(text);
            }
            "user" => {
                seen_non_system = true;
                req.messages.push(Message::user(decode_user_content(mo.get("content"), &what)?));
            }
            "assistant" => {
                seen_non_system = true;
                let mut content: Vec<ContentPart> = text_parts(mo.get("content"), &what)?
                    .into_iter()
                    .filter(|t| !t.is_empty())
                    .map(ContentPart::text)
                    .collect();
                if let Some(rc) = mo.get("reasoning_content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    content.insert(0, ContentPart::Thinking { text: rc.to_string(), signature: None });
                }
                if let Some(calls) = mo.get("tool_calls").filter(|v| !v.is_null()) {
                    let calls = calls.as_array().ok_or_else(|| invalid(format!("{what}.tool_calls must be an array")))?;
                    for (j, c) in calls.iter().enumerate() {
                        content.push(decode_tool_call(c, &format!("{what}.tool_calls[{j}]"))?);
                    }
                }
                req.messages.push(Message::assistant(content));
            }
            "tool" => {
                seen_non_system = true;
                is_tool = true;
                let id = required_str(mo, "tool_call_id", &what)?.to_string();
                let content = text_parts(mo.get("content"), &what)?.into_iter().map(ContentPart::text).collect();
                let part = ContentPart::ToolResult { tool_use_id: id, content, is_error: false };
                match req.messages.last_mut() {
                    Some(last) if last_was_tool_group => last.content.push(part),
                    _ => req.messages.push(Message::user(vec![part])),
                }
            }
            other => {
                return Err(ProtocolError::Unsupported(format!("{what}.role `{other}`")));
            }
        }
        last_was_tool_group = is_tool;
    }
    Ok(())
}

fn decode_tool_call(c: &Value, what: &str) -> ProtocolResult<ContentPart> {
    let o = as_object(c, what)?;
    if let Some(ty) = o.get("type").and_then(Value::as_str) {
        if ty != "function" {
            return Err(ProtocolError::Unsupported(format!("{what}.type `{ty}`")));
        }
    }
    let id = required_str(o, "id", what)?.to_string();
    let f = as_object(o.get("function").ok_or_else(|| invalid(format!("{what}.function is required")))?, what)?;
    let name = required_str(f, "name", &format!("{what}.function"))?.to_string();
    let input = match f.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(Value::String(s)) => parse_json_arguments(s)
            .map_err(|e| invalid(format!("{what}.function.arguments is not valid JSON: {e}")))?,
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err(invalid(format!("{what}.function.arguments must be a JSON string"))),
    };
    Ok(ContentPart::ToolUse { id, name, input })
}

// ───────────────────────── 请求编码（发往 OpenAI 兼容上游） ─────────────────────────

pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let mut warnings = Vec::new();
    let mut messages: Vec<Value> = Vec::new();

    for s in &req.system {
        messages.push(json!({"role": "system", "content": s}));
    }
    for m in &req.messages {
        match m.role {
            Role::User => encode_user_message(&m.content, &mut messages, &mut warnings),
            Role::Assistant => encode_assistant_message(&m.content, &mut messages, &mut warnings),
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    if req.stream {
        body.insert("stream".into(), json!(true));
        // 上游始终要求 usage，便于统计；是否向客户端输出由 OpenAiStreamEncoder 决定。
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let p = &req.params;
    if let Some(v) = p.max_tokens {
        body.insert("max_tokens".into(), json!(v));
    }
    if let Some(v) = p.temperature {
        body.insert("temperature".into(), json!(v));
    }
    if let Some(v) = p.top_p {
        body.insert("top_p".into(), json!(v));
    }
    if !p.stop.is_empty() {
        body.insert("stop".into(), json!(p.stop));
    }
    if let Some(v) = p.presence_penalty {
        body.insert("presence_penalty".into(), json!(v));
    }
    if let Some(v) = p.frequency_penalty {
        body.insert("frequency_penalty".into(), json!(v));
    }
    if let Some(v) = p.seed {
        body.insert("seed".into(), json!(v));
    }
    if p.top_k.is_some() {
        warnings.push("top_k is not supported by OpenAI Chat Completions and was dropped".into());
    }
    if let Some(u) = &req.user {
        body.insert("user".into(), json!(u));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut f = Map::new();
                f.insert("name".into(), json!(t.name));
                if let Some(d) = &t.description {
                    f.insert("description".into(), json!(d));
                }
                f.insert("parameters".into(), t.input_schema.clone());
                json!({"type": "function", "function": f})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if let Some(pt) = req.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(pt));
        }
    }
    if let Some(tc) = &req.tool_choice {
        let v = match tc {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Any => json!("required"),
            ToolChoice::Tool { name } => json!({"type": "function", "function": {"name": name}}),
        };
        body.insert("tool_choice".into(), v);
    }
    Ok(Encoded { body: Value::Object(body), warnings })
}

fn flatten_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn image_url_value(source: &ImageSource, detail: &Option<String>, warnings: &mut Vec<String>) -> Option<Value> {
    let url = match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
        ImageSource::FileId { .. } => {
            warnings.push("image referenced by file_id cannot be sent to OpenAI Chat Completions and was dropped".into());
            return None;
        }
    };
    let mut iu = Map::new();
    iu.insert("url".into(), json!(url));
    if let Some(d) = detail {
        iu.insert("detail".into(), json!(d));
    }
    Some(json!({"type": "image_url", "image_url": iu}))
}

fn encode_user_message(content: &[ContentPart], out: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let mut rest: Vec<Value> = Vec::new();
    let mut deferred_images: Vec<Value> = Vec::new();
    let mut only_text: Vec<&str> = Vec::new();
    let mut has_image = false;
    let mut tool_messages: Vec<Value> = Vec::new();

    for part in content {
        match part {
            ContentPart::ToolResult { tool_use_id, content, is_error } => {
                if *is_error {
                    warnings.push("tool_result.is_error cannot be represented in OpenAI Chat and was dropped".into());
                }
                for c in content {
                    if let ContentPart::Image { source, detail } = c {
                        if let Some(v) = image_url_value(source, detail, warnings) {
                            deferred_images.push(v);
                        }
                    }
                }
                tool_messages.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": flatten_text(content)}));
            }
            ContentPart::Text { text } => {
                only_text.push(text);
                rest.push(json!({"type": "text", "text": text}));
            }
            ContentPart::Image { source, detail } => {
                if let Some(v) = image_url_value(source, detail, warnings) {
                    has_image = true;
                    rest.push(v);
                }
            }
            ContentPart::ToolUse { .. } | ContentPart::Thinking { .. } => {
                warnings.push("tool_use/thinking part inside a user message was dropped".into());
            }
        }
    }
    out.extend(tool_messages);
    if !deferred_images.is_empty() {
        warnings.push("images inside tool_result were moved to a following user message (OpenAI tool messages are text-only)".into());
        let mut parts = vec![json!({"type": "text", "text": "[images returned by the tool above]"})];
        parts.extend(deferred_images);
        out.push(json!({"role": "user", "content": parts}));
    }
    if rest.is_empty() {
        return;
    }
    if !has_image && only_text.len() == 1 {
        out.push(json!({"role": "user", "content": only_text[0]}));
    } else {
        out.push(json!({"role": "user", "content": rest}));
    }
}

fn encode_assistant_message(content: &[ContentPart], out: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let mut texts: Vec<&str> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    for part in content {
        match part {
            ContentPart::Text { text } => texts.push(text),
            ContentPart::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            ContentPart::Thinking { text, signature } => {
                // 仅携带签名的空思考块只是其它协议的内部载体，不值得告警
                let carrier_only = text.is_empty() && signature.as_deref().is_some_and(signature::is_foreign);
                if !carrier_only {
                    warnings.push("assistant thinking content was not sent to the OpenAI-compatible upstream".into());
                }
            }
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {
                warnings.push("image/tool_result part inside an assistant message was dropped".into());
            }
        }
    }
    let mut msg = Map::new();
    msg.insert("role".into(), json!("assistant"));
    if texts.is_empty() && !calls.is_empty() {
        msg.insert("content".into(), Value::Null);
    } else if texts.len() <= 1 {
        msg.insert("content".into(), json!(texts.first().copied().unwrap_or("")));
    } else {
        // Multiple assistant text parts can be stream fragments in a client
        // history. Keep their boundaries without inventing a newline between
        // every fragment (9Router retains the parts as an OpenAI content array).
        msg.insert("content".into(), json!(texts.into_iter().map(|text| json!({"type": "text", "text": text})).collect::<Vec<_>>()));
    }
    if !calls.is_empty() {
        msg.insert("tool_calls".into(), Value::Array(calls));
    }
    out.push(Value::Object(msg));
}

// ───────────────────────── 响应编码 / 解码 ─────────────────────────

fn finish_reason(r: &StopReason) -> &str {
    match r {
        StopReason::EndTurn | StopReason::StopSequence => "stop",
        StopReason::MaxTokens => "length",
        StopReason::ToolUse => "tool_calls",
        StopReason::ContentFilter => "content_filter",
        StopReason::Other(_) => "stop",
    }
}

fn usage_value(u: &Usage) -> Value {
    let prompt = u.input_tokens.unwrap_or(0);
    let completion = u.output_tokens.unwrap_or(0);
    let mut v = json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    });
    if let Some(c) = u.cache_read_tokens {
        v["prompt_tokens_details"] = json!({"cached_tokens": c});
    }
    v
}

fn new_completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

pub fn encode_response(resp: &CanonicalResponse) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<Value> = Vec::new();
    for part in &resp.content {
        match part {
            ContentPart::Text { text: t } => text.push_str(t),
            ContentPart::Thinking { text: t, .. } => reasoning.push_str(t),
            ContentPart::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {}
        }
    }
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) });
    if !reasoning.is_empty() {
        message.insert("reasoning_content".into(), json!(reasoning));
    }
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    json!({
        "id": if resp.id.is_empty() { new_completion_id() } else { resp.id.clone() },
        "object": "chat.completion",
        "created": resp.created.unwrap_or_else(|| chrono::Utc::now().timestamp()),
        "model": resp.model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason(&resp.stop_reason),
        }],
        "usage": usage_value(&resp.usage),
    })
}

pub fn decode_finish_reason(v: Option<&Value>) -> StopReason {
    match v.and_then(Value::as_str) {
        None | Some("stop") => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(other.to_string()),
    }
}

pub fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(o) = v.and_then(Value::as_object) else {
        return Usage::default();
    };
    let get = |k: &str| o.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Usage {
        input_tokens: get("prompt_tokens"),
        output_tokens: get("completion_tokens"),
        cache_read_tokens: o
            .get("prompt_tokens_details")
            .and_then(Value::as_object)
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            // DeepSeek 风格的缓存命中字段
            .or_else(|| get("prompt_cache_hit_tokens")),
        cache_creation_tokens: None,
    }
}

pub fn decode_response(body: &Value) -> ProtocolResult<CanonicalResponse> {
    let obj = body.as_object().ok_or_else(|| invalid_resp("response body must be an object"))?;
    let choice = obj
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_resp("response.choices is empty"))?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_resp("response.choices[0].message is missing"))?;

    let mut content = Vec::new();
    // 思考内容：DeepSeek / OpenRouter 惯例是 reasoning_content，部分上游使用 reasoning。
    let reasoning = ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|k| message.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()));
    if let Some(r) = reasoning {
        content.push(ContentPart::Thinking { text: r.to_string(), signature: None });
    }
    if let Some(t) = message.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        content.push(ContentPart::text(t));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (i, c) in calls.iter().enumerate() {
            content.push(decode_tool_call(c, &format!("tool_calls[{i}]")).map_err(|e| invalid_resp(e.to_string()))?);
        }
    }
    Ok(CanonicalResponse {
        id: opt_str(obj, "id").unwrap_or_default(),
        model: opt_str(obj, "model").unwrap_or_default(),
        created: obj.get("created").and_then(Value::as_i64),
        content,
        stop_reason: decode_finish_reason(choice.get("finish_reason")),
        stop_sequence: None,
        usage: decode_usage(obj.get("usage")),
    })
}

// ───────────────────────── 流式编码 ─────────────────────────

/// canonical 事件 → OpenAI `chat.completion.chunk` SSE。
///
/// 输出顺序：role chunk → 内容 / tool_calls chunk → finish chunk →（可选）usage chunk → `data: [DONE]`。
/// 出错时输出 `{"error":...}` 帧并终止（不再输出 `[DONE]`）。
pub struct OpenAiStreamEncoder {
    include_usage: bool,
    id: String,
    model: String,
    created: i64,
    started: bool,
    finished: bool,
    usage: Usage,
    /// canonical block index → OpenAI tool_calls[].index
    tool_slots: HashMap<u32, u32>,
    next_tool_slot: u32,
}

impl OpenAiStreamEncoder {
    pub fn new(include_usage: bool) -> Self {
        Self {
            include_usage,
            id: new_completion_id(),
            model: String::new(),
            created: chrono::Utc::now().timestamp(),
            started: false,
            finished: false,
            usage: Usage::default(),
            tool_slots: HashMap::new(),
            next_tool_slot: 0,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> SseFrame {
        SseFrame::data(
            json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            })
            .to_string(),
        )
    }

    fn require_started(&self) -> ProtocolResult<()> {
        if !self.started {
            return Err(invalid_resp("stream event received before message_start"));
        }
        Ok(())
    }
}

impl StreamEncoder for OpenAiStreamEncoder {
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>> {
        if self.finished {
            return Err(invalid_resp("stream event received after the stream ended"));
        }
        match event {
            StreamEvent::MessageStart { id, model, usage } => {
                if self.started {
                    return Err(invalid_resp("duplicate message_start"));
                }
                self.started = true;
                if !id.is_empty() {
                    self.id = id;
                }
                self.model = model;
                if let Some(u) = usage {
                    self.usage.merge(&u);
                }
                Ok(vec![self.chunk(json!({"role": "assistant", "content": ""}), None)])
            }
            StreamEvent::ContentBlockStart { index, block } => {
                self.require_started()?;
                match block {
                    BlockStart::Text | BlockStart::Thinking => Ok(vec![]),
                    BlockStart::ToolUse { id, name } => {
                        let slot = self.next_tool_slot;
                        self.next_tool_slot += 1;
                        self.tool_slots.insert(index, slot);
                        let delta = json!({"tool_calls": [{
                            "index": slot, "id": id, "type": "function",
                            "function": {"name": name, "arguments": ""},
                        }]});
                        Ok(vec![self.chunk(delta, None)])
                    }
                }
            }
            StreamEvent::ContentBlockDelta { delta, .. } => {
                self.require_started()?;
                match delta {
                    BlockDelta::Text { text } => Ok(vec![self.chunk(json!({"content": text}), None)]),
                    BlockDelta::Thinking { text } => Ok(vec![self.chunk(json!({"reasoning_content": text}), None)]),
                    // OpenAI Chat 没有思考签名字段。
                    BlockDelta::Signature { .. } => Ok(vec![]),
                }
            }
            StreamEvent::ToolCallDelta { index, partial_json } => {
                self.require_started()?;
                let slot = *self
                    .tool_slots
                    .get(&index)
                    .ok_or_else(|| invalid_resp(format!("tool call delta for unknown block index {index}")))?;
                let delta = json!({"tool_calls": [{"index": slot, "function": {"arguments": partial_json}}]});
                Ok(vec![self.chunk(delta, None)])
            }
            StreamEvent::ContentBlockStop { .. } => {
                self.require_started()?;
                Ok(vec![])
            }
            StreamEvent::Usage(u) => {
                self.require_started()?;
                self.usage.merge(&u);
                Ok(vec![])
            }
            StreamEvent::MessageStop { stop_reason, .. } => {
                self.require_started()?;
                self.finished = true;
                let mut frames = vec![self.chunk(json!({}), Some(finish_reason(&stop_reason)))];
                if self.include_usage {
                    frames.push(SseFrame::data(
                        json!({
                            "id": self.id,
                            "object": "chat.completion.chunk",
                            "created": self.created,
                            "model": self.model,
                            "choices": [],
                            "usage": usage_value(&self.usage),
                        })
                        .to_string(),
                    ));
                }
                frames.push(SseFrame::data("[DONE]"));
                Ok(frames)
            }
            StreamEvent::Error { kind, message } => {
                self.finished = true;
                Ok(vec![super::ApiError::new(kind, message).to_sse_frame(super::ProtocolKind::OpenAiChat)])
            }
        }
    }
}

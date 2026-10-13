//! OpenAI Responses（`POST /v1/responses`）编解码。独立实现，不经过 Chat Completions 的字段映射。
//!
//! # 入口侧
//! - [`decode_request`]：`input`（字符串或 item 数组）、`instructions`、`tools`、`tool_choice`、`reasoning`、
//!   `max_output_tokens` → canonical。
//! - [`encode_response`]：canonical → `response` 对象（`output` 为 item 列表）。
//! - [`ResponsesStreamEncoder`]：canonical 事件 → `response.created` ... `response.completed` 全套 SSE，带 `sequence_number`。
//! - [`encode_error`] / [`error_event`]：错误体与流内 `error` 事件。
//!
//! # 上游侧
//! - [`encode_request`]、[`decode_response`]、[`ResponsesStreamDecoder`]（增量 SSE，处理半包）、[`decode_error`]。
//!
//! # 约定与取舍（均有测试）
//! - **有状态能力不支持**：`previous_response_id`、`conversation`、`background`、`prompt`、`item_reference`
//!   返回 `Unsupported`（网关无服务端会话存储），不会静默丢掉上下文。`store` 被接受但忽略；编码到上游时恒为 `store:false`。
//! - **内置工具**（web_search / file_search / computer / code_interpreter / image_generation / mcp / local_shell ...）
//!   出现在 `tools` 或 `input` 历史中时进入 warnings，不会静默消失；`custom`（自由文本）工具被改写为带单个 `input`
//!   字符串参数的 function 工具，并告警，其 `custom_tool_call` 对应 `ToolUse.input = {"input": "..."}`。
//! - **reasoning item**：`summary` 文本 → `Thinking.text`；`encrypted_content` → `Thinking.signature`
//!   （前缀 `openai-responses:`）。编码到上游时只有带该前缀签名的 Thinking 才会作为 reasoning item 回传，
//!   并在请求了 reasoning 时追加 `include: ["reasoning.encrypted_content"]`（`store:false` 下多轮推理依赖它）。
//! - Responses 的函数工具默认 `strict:true`，而 canonical 的 schema 是任意 JSON Schema，因此编码时恒写 `strict:false`。
//! - `call_id` 超过 64 字符时用 SHA-256 缩短（`function_call` 与 `function_call_output` 使用同一映射）。
//! - **用量**：`input_tokens` 已含 `cached_tokens`；`output_tokens` 已含 `reasoning_tokens`，canonical 原样搬运。
//! - 不支持 `stop` / `top_k` / `seed` / 惩罚项，canonical 里有这些字段时编码产生 warning。

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::canonical::*;
use super::common::*;
use super::wire::{kind_from_status, SseParser};
use super::{ApiError, ApiErrorKind, Encoded, ProtocolError, ProtocolKind, ProtocolResult, SseFrame, StreamEncoder};

const KNOWN_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning",
    "max_output_tokens",
    "temperature",
    "top_p",
    "stream",
    "user",
    "store",
];

/// 这些内置工具 / 历史 item 类型无法在 canonical 中表达。
const BINARY_NOTE: &str = "non-image binary input is carried in a binary part";

fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
}

fn hex_prefix(input: &str, bytes: usize) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest.iter().take(bytes).map(|b| format!("{b:02x}")).collect()
}

fn clamp_call_id(id: &str) -> String {
    if id.len() <= 64 {
        id.to_string()
    } else {
        format!("call_{}", hex_prefix(id, 16))
    }
}

// ───────────────────────── 请求解码 ─────────────────────────

pub fn decode_request(body: &Value) -> ProtocolResult<CanonicalRequest> {
    let obj = as_object(body, "request body")?;
    let mut req = CanonicalRequest { model: required_str(obj, "model", "request")?.to_string(), ..Default::default() };
    if req.model.is_empty() {
        return Err(invalid("request.model must not be empty"));
    }
    for key in ["previous_response_id", "conversation", "prompt"] {
        if obj.get(key).is_some_and(|v| !v.is_null()) {
            return Err(ProtocolError::Unsupported(format!(
                "`{key}` requires server-side state, which this gateway does not keep; send the full conversation in `input`"
            )));
        }
    }
    if obj.get("background").and_then(Value::as_bool) == Some(true) {
        return Err(ProtocolError::Unsupported("background responses are not supported".into()));
    }

    if let Some(ins) = obj.get("instructions").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        req.system.push(ins.to_string());
    }

    match obj.get("input") {
        Some(Value::String(s)) => req.messages.push(Message::user(vec![ContentPart::text(s.clone())])),
        Some(Value::Array(items)) => decode_input_items(items, &mut req)?,
        _ => return Err(invalid("request.input is required and must be a string or an array")),
    }
    if req.messages.is_empty() {
        return Err(invalid("request.input must contain at least one user-visible item"));
    }

    req.stream = opt_bool(obj, "stream", "request")?.unwrap_or(false);
    req.include_usage = true;
    req.params = GenerationParams {
        max_tokens: opt_u32(obj, "max_output_tokens", "request")?,
        temperature: opt_f64(obj, "temperature", "request")?,
        top_p: opt_f64(obj, "top_p", "request")?,
        ..Default::default()
    };
    req.parallel_tool_calls = opt_bool(obj, "parallel_tool_calls", "request")?;
    req.user = opt_str(obj, "user");

    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        decode_tools(tools, &mut req)?;
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        req.tool_choice = decode_tool_choice(tc, &mut req.warnings)?;
    }
    if let Some(r) = obj.get("reasoning").filter(|v| !v.is_null()) {
        let ro = as_object(r, "reasoning")?;
        let effort = opt_str(ro, "effort");
        let summary = opt_str(ro, "summary").or_else(|| opt_str(ro, "generate_summary"));
        if effort.is_some() || summary.is_some() {
            req.reasoning =
                Some(ReasoningConfig { effort, budget_tokens: None, include_thoughts: summary.map(|_| true) });
        }
        warn_unknown_prefixed(ro, &["effort", "summary", "generate_summary"], "reasoning.", &mut req.warnings);
    }

    // 有状态字段此处必为 null / false（否则上面已报错），视为已识别。
    let known: Vec<&str> =
        KNOWN_FIELDS.iter().copied().chain(["previous_response_id", "conversation", "prompt", "background"]).collect();
    warn_unknown_fields(obj, &known, &mut req.warnings);
    Ok(req)
}

fn warn_unknown_prefixed(obj: &Map<String, Value>, known: &[&str], prefix: &str, warnings: &mut Vec<String>) {
    let mut unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !known.contains(k)).collect();
    unknown.sort_unstable();
    for k in unknown {
        warnings.push(format!("unsupported field ignored: {prefix}{k}"));
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Last {
    None,
    User,
    ToolResults,
    Assistant,
}

fn image_from_url(url: &str, detail: Option<String>) -> ProtocolResult<ContentPart> {
    Ok(ContentPart::Image { source: image_source_from_url(url)?, detail })
}

fn decode_parts(content: Option<&Value>, what: &str, warnings: &mut Vec<String>) -> ProtocolResult<Vec<ContentPart>> {
    match content {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(vec![ContentPart::text(s.clone())]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                let pw = format!("{what}.content[{i}]");
                let o = as_object(p, &pw)?;
                match o.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") | Some("text") => {
                        out.push(ContentPart::text(required_str(o, "text", &pw)?))
                    }
                    Some("refusal") => out.push(ContentPart::text(required_str(o, "refusal", &pw)?)),
                    Some("input_image") => {
                        let detail = opt_str(o, "detail");
                        match (o.get("image_url"), o.get("file_id")) {
                            (Some(Value::String(u)), _) => out.push(image_from_url(u, detail)?),
                            (Some(Value::Object(io)), _) => {
                                out.push(image_from_url(required_str(io, "url", &pw)?, detail)?)
                            }
                            (_, Some(Value::String(f))) => out.push(ContentPart::Image {
                                source: ImageSource::FileId { file_id: f.clone() },
                                detail,
                            }),
                            _ => return Err(invalid(format!("{pw}: input_image requires image_url or file_id"))),
                        }
                    }
                    Some("input_file") => {
                        let filename = opt_str(o, "filename");
                        if let Some(fd) = o.get("file_data").and_then(Value::as_str) {
                            let part = match parse_data_url(fd)? {
                                Some((media_type, data)) => ContentPart::Image {
                                    source: ImageSource::Base64 { media_type, data },
                                    detail: None,
                                },
                                None => {
                                    let media_type = filename
                                        .as_deref()
                                        .and_then(|f| f.rsplit('.').next())
                                        .map(|e| match e.to_ascii_lowercase().as_str() {
                                            "pdf" => "application/pdf",
                                            "png" => "image/png",
                                            "jpg" | "jpeg" => "image/jpeg",
                                            _ => "application/octet-stream",
                                        })
                                        .unwrap_or("application/octet-stream");
                                    ContentPart::Image {
                                        source: ImageSource::Base64 {
                                            media_type: media_type.to_string(),
                                            data: fd.to_string(),
                                        },
                                        detail: None,
                                    }
                                }
                            };
                            out.push(part);
                        } else if let Some(u) = o.get("file_url").and_then(Value::as_str) {
                            out.push(ContentPart::Image {
                                source: ImageSource::Url { url: u.to_string() },
                                detail: None,
                            });
                        } else if let Some(f) = o.get("file_id").and_then(Value::as_str) {
                            out.push(ContentPart::Image {
                                source: ImageSource::FileId { file_id: f.to_string() },
                                detail: None,
                            });
                        } else {
                            return Err(invalid(format!("{pw}: input_file requires file_data, file_url or file_id")));
                        }
                        let w = format!("{pw}: {BINARY_NOTE}");
                        if !warnings.contains(&w) {
                            warnings.push(w);
                        }
                    }
                    other => {
                        return Err(ProtocolError::Unsupported(format!(
                            "{pw}.type `{}` (supported: input_text, output_text, input_image, input_file, refusal)",
                            other.unwrap_or("?")
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid(format!("{what}.content must be a string or an array"))),
    }
}

fn decode_input_items(items: &[Value], req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let mut last = Last::None;
    let mut seen_non_system = false;
    for (i, item) in items.iter().enumerate() {
        let what = format!("input[{i}]");
        let o = as_object(item, &what)?;
        let ty = o.get("type").and_then(Value::as_str).or(if o.contains_key("role") { Some("message") } else { None });
        let Some(ty) = ty else {
            return Err(invalid(format!("{what}.type is required")));
        };
        match ty {
            "message" => {
                let role = required_str(o, "role", &what)?;
                match role {
                    "system" | "developer" => {
                        let text: Vec<String> = decode_parts(o.get("content"), &what, &mut req.warnings)?
                            .into_iter()
                            .filter_map(|p| if let ContentPart::Text { text } = p { Some(text) } else { None })
                            .collect();
                        if seen_non_system {
                            req.warnings.push(format!(
                                "{what}: {role} message after conversation start was hoisted to the system prompt"
                            ));
                        }
                        req.system.push(text.join("\n"));
                    }
                    "user" => {
                        seen_non_system = true;
                        let parts = decode_parts(o.get("content"), &what, &mut req.warnings)?;
                        req.messages.push(Message::user(parts));
                        last = Last::User;
                    }
                    "assistant" => {
                        seen_non_system = true;
                        let parts = decode_parts(o.get("content"), &what, &mut req.warnings)?;
                        push_assistant(req, &mut last, parts);
                    }
                    other => return Err(ProtocolError::Unsupported(format!("{what}.role `{other}`"))),
                }
            }
            "reasoning" => {
                seen_non_system = true;
                let texts = |key: &str| -> Vec<String> {
                    o.get(key)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.get("text").and_then(Value::as_str))
                                .filter(|t| !t.is_empty())
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut text = texts("summary");
                if text.is_empty() {
                    text = texts("content");
                }
                let sig = o
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| signature::tag(signature::OPENAI_RESPONSES, s));
                if !text.is_empty() || sig.is_some() {
                    push_assistant(
                        req,
                        &mut last,
                        vec![ContentPart::Thinking { text: text.join("\n\n"), signature: sig }],
                    );
                }
            }
            "function_call" | "custom_tool_call" => {
                seen_non_system = true;
                let id = opt_str(o, "call_id")
                    .or_else(|| opt_str(o, "id"))
                    .ok_or_else(|| invalid(format!("{what}.call_id is required")))?;
                let name = required_str(o, "name", &what)?.to_string();
                let input = if ty == "custom_tool_call" {
                    json!({"input": o.get("input").and_then(Value::as_str).unwrap_or("")})
                } else {
                    match o.get("arguments") {
                        None | Some(Value::Null) => json!({}),
                        Some(Value::String(s)) => parse_json_arguments(s)
                            .map_err(|e| invalid(format!("{what}.arguments is not valid JSON: {e}")))?,
                        Some(v @ Value::Object(_)) => v.clone(),
                        Some(_) => return Err(invalid(format!("{what}.arguments must be a JSON string"))),
                    }
                };
                push_assistant(req, &mut last, vec![ContentPart::ToolUse { id, name, input }]);
            }
            "function_call_output" | "custom_tool_call_output" => {
                seen_non_system = true;
                let id = opt_str(o, "call_id").ok_or_else(|| invalid(format!("{what}.call_id is required")))?;
                let content = match o.get("output") {
                    None | Some(Value::Null) => vec![],
                    Some(Value::String(s)) => vec![ContentPart::text(s.clone())],
                    Some(Value::Array(_)) => {
                        let wrapper = json!({"content": o.get("output")});
                        decode_parts(wrapper.get("content"), &what, &mut req.warnings)?
                    }
                    Some(other) => vec![ContentPart::text(other.to_string())],
                };
                let part = ContentPart::ToolResult { tool_use_id: id, content, is_error: false };
                match req.messages.last_mut() {
                    Some(m) if last == Last::ToolResults => m.content.push(part),
                    _ => req.messages.push(Message::user(vec![part])),
                }
                last = Last::ToolResults;
            }
            "item_reference" => {
                return Err(ProtocolError::Unsupported(format!("{what}: item_reference requires server-side state")));
            }
            other => {
                req.warnings.push(format!("{what}: item type `{other}` is not supported and was dropped"));
            }
        }
    }
    Ok(())
}

fn push_assistant(req: &mut CanonicalRequest, last: &mut Last, parts: Vec<ContentPart>) {
    match req.messages.last_mut() {
        Some(m) if *last == Last::Assistant => m.content.extend(parts),
        _ => req.messages.push(Message::assistant(parts)),
    }
    *last = Last::Assistant;
}

fn custom_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"input": {"type": "string", "description": "Raw freeform input for this custom tool"}},
        "required": ["input"],
    })
}

fn decode_tools(v: &Value, req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let arr = v.as_array().ok_or_else(|| invalid("request.tools must be an array"))?;
    for (i, t) in arr.iter().enumerate() {
        let what = format!("tools[{i}]");
        let o = as_object(t, &what)?;
        let ty =
            o.get("type").and_then(Value::as_str).unwrap_or(if o.contains_key("function") { "function" } else { "?" });
        match ty {
            "function" => {
                // 同时容忍 Chat Completions 形态 `{type:"function", function:{...}}`。
                let f = o.get("function").and_then(Value::as_object).unwrap_or(o);
                let name = required_str(f, "name", &what)?.to_string();
                if f.get("strict").and_then(Value::as_bool) == Some(true) {
                    req.warnings.push(format!("{what}: strict mode is not enforced for translated tools"));
                }
                req.tools.push(ToolDefinition {
                    name,
                    description: opt_str(f, "description"),
                    input_schema: f
                        .get("parameters")
                        .filter(|p| !p.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                });
            }
            "custom" => {
                let name = required_str(o, "name", &what)?.to_string();
                let fmt = o.get("format").and_then(Value::as_object);
                let hint = fmt
                    .map(|f| {
                        [opt_str(f, "syntax"), opt_str(f, "definition")]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let description = [opt_str(o, "description").unwrap_or_default(), hint]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                req.warnings.push(format!(
                    "{what}: custom tool `{name}` was exposed as a function with a single `input` string"
                ));
                req.tools.push(ToolDefinition {
                    name,
                    description: (!description.is_empty()).then_some(description),
                    input_schema: custom_tool_schema(),
                });
            }
            other => {
                req.warnings.push(format!("{what}: hosted tool `{other}` is not supported and was dropped"));
            }
        }
    }
    Ok(())
}

fn decode_tool_choice(v: &Value, warnings: &mut Vec<String>) -> ProtocolResult<Option<ToolChoice>> {
    match v {
        Value::String(s) => match s.as_str() {
            "auto" => Ok(Some(ToolChoice::Auto)),
            "none" => Ok(Some(ToolChoice::None)),
            "required" => Ok(Some(ToolChoice::Any)),
            other => Err(invalid(format!("request.tool_choice `{other}` is not valid"))),
        },
        Value::Object(o) => {
            match o.get("type").and_then(Value::as_str) {
                Some("function") | Some("custom") => {
                    let name = o
                        .get("name")
                        .or_else(|| o.get("function").and_then(|f| f.get("name")))
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("tool_choice.name is required"))?;
                    Ok(Some(ToolChoice::Tool { name: name.to_string() }))
                }
                Some("allowed_tools") => {
                    let names: Vec<&str> = o
                        .get("tools")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).collect())
                        .unwrap_or_default();
                    let required = o.get("mode").and_then(Value::as_str) == Some("required");
                    Ok(Some(match (required, names.as_slice()) {
                        (true, [one]) => ToolChoice::Tool { name: (*one).to_string() },
                        (true, _) => {
                            warnings.push("tool_choice.allowed_tools was reduced to `required`; the allowed list cannot be expressed".into());
                            ToolChoice::Any
                        }
                        (false, _) => {
                            warnings.push(
                                "tool_choice.allowed_tools was reduced to `auto`; the allowed list cannot be expressed"
                                    .into(),
                            );
                            ToolChoice::Auto
                        }
                    }))
                }
                Some(other) => {
                    warnings.push(format!("tool_choice.type `{other}` selects a hosted tool and was ignored"));
                    Ok(None)
                }
                None => Err(invalid("tool_choice.type is required")),
            }
        }
        _ => Err(invalid("request.tool_choice must be a string or an object")),
    }
}

// ───────────────────────── 请求编码（发往 Responses 上游） ─────────────────────────

fn data_url(media_type: &str, data: &str) -> String {
    format!("data:{media_type};base64,{data}")
}

fn input_part(p: &ContentPart, warnings: &mut Vec<String>) -> Option<Value> {
    match p {
        ContentPart::Text { text } => Some(json!({"type": "input_text", "text": text})),
        ContentPart::Image { source, detail } => {
            let mut part = match source {
                ImageSource::Base64 { media_type, data } if !media_type.starts_with("image/") => {
                    return Some(
                        json!({"type": "input_file", "filename": "attachment", "file_data": data_url(media_type, data)}),
                    );
                }
                ImageSource::Base64 { media_type, data } => {
                    json!({"type": "input_image", "image_url": data_url(media_type, data)})
                }
                ImageSource::Url { url } => json!({"type": "input_image", "image_url": url}),
                ImageSource::FileId { file_id } => json!({"type": "input_image", "file_id": file_id}),
            };
            if let Some(d) = detail {
                part["detail"] = json!(d);
            }
            Some(part)
        }
        ContentPart::ToolUse { .. } | ContentPart::Thinking { .. } | ContentPart::ToolResult { .. } => {
            warnings.push("tool/thinking part inside a user message was dropped".into());
            None
        }
    }
}

fn normalize_parameters(schema: &Value) -> Value {
    match schema {
        Value::Object(o)
            if o.get("type").and_then(Value::as_str) == Some("object") && !o.contains_key("properties") =>
        {
            let mut m = o.clone();
            m.insert("properties".into(), json!({}));
            Value::Object(m)
        }
        Value::Object(_) => schema.clone(),
        _ => json!({"type": "object", "properties": {}}),
    }
}

fn budget_to_effort(b: u32) -> Option<&'static str> {
    match b {
        0 => None,
        1..=2048 => Some("low"),
        2049..=12288 => Some("medium"),
        _ => Some("high"),
    }
}

pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let mut warnings = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for m in &req.messages {
        match m.role {
            Role::User => {
                let mut rest: Vec<Value> = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::ToolResult { tool_use_id, content, is_error } => {
                            if *is_error {
                                warnings.push(
                                    "tool_result.is_error cannot be represented in OpenAI Responses and was dropped"
                                        .into(),
                                );
                            }
                            let has_image = content.iter().any(|c| matches!(c, ContentPart::Image { .. }));
                            let output = if has_image {
                                Value::Array(content.iter().filter_map(|c| input_part(c, &mut warnings)).collect())
                            } else {
                                json!(content
                                    .iter()
                                    .filter_map(|c| if let ContentPart::Text { text } = c {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"))
                            };
                            input.push(json!({"type": "function_call_output", "call_id": clamp_call_id(tool_use_id), "output": output}));
                        }
                        other => {
                            if let Some(v) = input_part(other, &mut warnings) {
                                rest.push(v);
                            }
                        }
                    }
                }
                if !rest.is_empty() {
                    input.push(json!({"type": "message", "role": "user", "content": rest}));
                }
            }
            Role::Assistant => {
                let mut run: Vec<Value> = Vec::new();
                let flush = |run: &mut Vec<Value>, input: &mut Vec<Value>| {
                    if !run.is_empty() {
                        input.push(json!({"type": "message", "role": "assistant", "content": std::mem::take(run)}));
                    }
                };
                for p in &m.content {
                    match p {
                        ContentPart::Text { text } => {
                            if !text.is_empty() {
                                run.push(json!({"type": "output_text", "text": text}));
                            }
                        }
                        ContentPart::Thinking { text, signature } => {
                            flush(&mut run, &mut input);
                            match signature.as_deref().and_then(|s| signature::untag(signature::OPENAI_RESPONSES, s)) {
                                Some(enc) => {
                                    let summary: Vec<Value> = if text.is_empty() {
                                        vec![]
                                    } else {
                                        vec![json!({"type": "summary_text", "text": text})]
                                    };
                                    input.push(json!({
                                        "type": "reasoning",
                                        "id": format!("rs_{}", hex_prefix(enc, 12)),
                                        "summary": summary,
                                        "encrypted_content": enc,
                                    }));
                                }
                                None => {
                                    if !text.is_empty() || signature.is_some() {
                                        warnings.push("assistant thinking content without an OpenAI Responses signature was not sent upstream".into());
                                    }
                                }
                            }
                        }
                        ContentPart::ToolUse { id, name, input: args } => {
                            flush(&mut run, &mut input);
                            input.push(json!({
                                "type": "function_call",
                                "call_id": clamp_call_id(id),
                                "name": name,
                                "arguments": args.to_string(),
                            }));
                        }
                        ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {
                            warnings.push("image/tool_result part inside an assistant message was dropped".into());
                        }
                    }
                }
                flush(&mut run, &mut input);
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("input".into(), Value::Array(input));
    if !req.system.is_empty() {
        body.insert("instructions".into(), json!(req.system.join("\n\n")));
    }
    body.insert("store".into(), json!(false));
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    let p = &req.params;
    if let Some(v) = p.max_tokens {
        body.insert("max_output_tokens".into(), json!(v));
    }
    if let Some(v) = p.temperature {
        body.insert("temperature".into(), json!(v));
    }
    if let Some(v) = p.top_p {
        body.insert("top_p".into(), json!(v));
    }
    for (present, name) in [
        (!p.stop.is_empty(), "stop"),
        (p.top_k.is_some(), "top_k"),
        (p.seed.is_some(), "seed"),
        (p.presence_penalty.is_some(), "presence_penalty"),
        (p.frequency_penalty.is_some(), "frequency_penalty"),
    ] {
        if present {
            warnings.push(format!("{name} is not supported by OpenAI Responses and was dropped"));
        }
    }
    if let Some(u) = &req.user {
        body.insert("user".into(), json!(u));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut o = Map::new();
                o.insert("type".into(), json!("function"));
                o.insert("name".into(), json!(t.name));
                if let Some(d) = &t.description {
                    o.insert("description".into(), json!(d));
                }
                o.insert("parameters".into(), normalize_parameters(&t.input_schema));
                o.insert("strict".into(), json!(false));
                Value::Object(o)
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if let Some(pt) = req.parallel_tool_calls {
            body.insert("parallel_tool_calls".into(), json!(pt));
        }
    }
    if let Some(tc) = &req.tool_choice {
        body.insert(
            "tool_choice".into(),
            match tc {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::None => json!("none"),
                ToolChoice::Any => json!("required"),
                ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
            },
        );
    }
    if let Some(r) = &req.reasoning {
        let mut ro = Map::new();
        if let Some(e) = r.effort.clone().or_else(|| r.budget_tokens.and_then(budget_to_effort).map(str::to_string)) {
            ro.insert("effort".into(), json!(e));
        }
        if r.include_thoughts == Some(true) {
            ro.insert("summary".into(), json!("auto"));
        }
        if !ro.is_empty() {
            body.insert("reasoning".into(), Value::Object(ro));
            body.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }
    Ok(Encoded { body: Value::Object(body), warnings })
}

// ───────────────────────── 响应编码 / 解码 ─────────────────────────

fn usage_value(u: &Usage) -> Value {
    let (i, o) = (u.input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0));
    json!({
        "input_tokens": i,
        "input_tokens_details": {"cached_tokens": u.cache_read_tokens.unwrap_or(0)},
        "output_tokens": o,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": i + o,
    })
}

pub fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(o) = v.and_then(Value::as_object) else {
        return Usage::default();
    };
    let get = |k: &str| o.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Usage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: o
            .get("input_tokens_details")
            .and_then(Value::as_object)
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        cache_creation_tokens: None,
    }
}

/// (status, incomplete_details.reason)
fn status_for(r: &StopReason) -> (&'static str, Option<&'static str>) {
    match r {
        StopReason::MaxTokens => ("incomplete", Some("max_output_tokens")),
        StopReason::ContentFilter => ("incomplete", Some("content_filter")),
        _ => ("completed", None),
    }
}

/// `response` 对象中随状态变化的部分。
struct ResponseShape<'a> {
    status: &'a str,
    incomplete: Option<&'a str>,
    output: Vec<Value>,
    usage: Option<&'a Usage>,
    error: Option<Value>,
}

fn response_object(id: &str, model: &str, created: i64, shape: ResponseShape<'_>) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": created,
        "status": shape.status,
        "background": false,
        "error": shape.error,
        "incomplete_details": shape.incomplete.map(|r| json!({"reason": r})),
        "instructions": null,
        "max_output_tokens": null,
        "metadata": {},
        "model": model,
        "output": shape.output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "reasoning": {"effort": null, "summary": null},
        "store": false,
        "temperature": null,
        "text": {"format": {"type": "text"}},
        "tool_choice": "auto",
        "tools": [],
        "top_p": null,
        "truncation": "disabled",
        "usage": shape.usage.map(usage_value),
        "user": null,
    })
}

fn message_item(id: &str, texts: &[String], status: &str) -> Value {
    let content: Vec<Value> =
        texts.iter().map(|t| json!({"type": "output_text", "text": t, "annotations": []})).collect();
    json!({"id": id, "type": "message", "status": status, "role": "assistant", "content": content})
}

fn reasoning_item(id: &str, text: &str, sig: Option<&str>) -> Value {
    let summary: Vec<Value> =
        if text.is_empty() { vec![] } else { vec![json!({"type": "summary_text", "text": text})] };
    let mut v = json!({"id": id, "type": "reasoning", "summary": summary});
    if let Some(s) = sig {
        v["encrypted_content"] = json!(s);
    }
    v
}

fn function_call_item(id: &str, call_id: &str, name: &str, args: &str, status: &str) -> Value {
    json!({"id": id, "type": "function_call", "status": status, "call_id": call_id, "name": name, "arguments": args})
}

fn output_items(content: &[ContentPart]) -> Vec<Value> {
    let mut items = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    for p in content {
        if !matches!(p, ContentPart::Text { .. }) && !texts.is_empty() {
            items.push(message_item(&new_id("msg"), &std::mem::take(&mut texts), "completed"));
        }
        match p {
            ContentPart::Text { text } => texts.push(text.clone()),
            ContentPart::Thinking { text, signature } => {
                let sig = signature.as_deref().and_then(|s| signature::untag(signature::OPENAI_RESPONSES, s));
                if !text.is_empty() || sig.is_some() {
                    items.push(reasoning_item(&new_id("rs"), text, sig));
                }
            }
            ContentPart::ToolUse { id, name, input } => {
                items.push(function_call_item(
                    &new_id("fc"),
                    &clamp_call_id(id),
                    name,
                    &input.to_string(),
                    "completed",
                ));
            }
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {}
        }
    }
    if !texts.is_empty() {
        items.push(message_item(&new_id("msg"), &texts, "completed"));
    }
    items
}

pub fn encode_response(resp: &CanonicalResponse) -> Value {
    let (status, incomplete) = status_for(&resp.stop_reason);
    let id = if resp.id.is_empty() { new_id("resp") } else { resp.id.clone() };
    response_object(
        &id,
        &resp.model,
        resp.created.unwrap_or_else(|| chrono::Utc::now().timestamp()),
        ResponseShape {
            status,
            incomplete,
            output: output_items(&resp.content),
            usage: Some(&resp.usage),
            error: None,
        },
    )
}

fn decode_function_args(o: &Map<String, Value>, what: &str) -> ProtocolResult<Value> {
    match o.get("arguments") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::String(s)) => {
            parse_json_arguments(s).map_err(|e| invalid_resp(format!("{what}.arguments is not valid JSON: {e}")))
        }
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(_) => Err(invalid_resp(format!("{what}.arguments must be a JSON string"))),
    }
}

pub fn decode_response(body: &Value) -> ProtocolResult<CanonicalResponse> {
    let obj = body.as_object().ok_or_else(|| invalid_resp("response body must be an object"))?;
    if obj.get("status").and_then(Value::as_str) == Some("failed") {
        let e = obj.get("error").cloned().unwrap_or(Value::Null);
        return Err(invalid_resp(format!(
            "upstream response failed: {}",
            e.get("message").and_then(Value::as_str).unwrap_or("unknown error")
        )));
    }
    let items =
        obj.get("output").and_then(Value::as_array).ok_or_else(|| invalid_resp("response.output is missing"))?;
    let mut content = Vec::new();
    let mut saw_call = false;
    for (i, it) in items.iter().enumerate() {
        let what = format!("output[{i}]");
        let Some(o) = it.as_object() else { continue };
        match o.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in o.get("content").and_then(Value::as_array).into_iter().flatten() {
                    let t = match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => part.get("text").and_then(Value::as_str),
                        Some("refusal") => part.get("refusal").and_then(Value::as_str),
                        _ => None,
                    };
                    if let Some(t) = t.filter(|t| !t.is_empty()) {
                        content.push(ContentPart::text(t));
                    }
                }
            }
            Some("reasoning") => {
                let texts = |key: &str| -> Vec<String> {
                    o.get(key)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.get("text").and_then(Value::as_str))
                                .filter(|t| !t.is_empty())
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut text = texts("summary");
                if text.is_empty() {
                    text = texts("content");
                }
                let sig = o
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| signature::tag(signature::OPENAI_RESPONSES, s));
                if !text.is_empty() || sig.is_some() {
                    content.push(ContentPart::Thinking { text: text.join("\n\n"), signature: sig });
                }
            }
            Some("function_call") => {
                saw_call = true;
                content.push(ContentPart::ToolUse {
                    id: opt_str(o, "call_id")
                        .or_else(|| opt_str(o, "id"))
                        .ok_or_else(|| invalid_resp(format!("{what}.call_id is missing")))?,
                    name: required_str(o, "name", &what).map_err(|e| invalid_resp(e.to_string()))?.to_string(),
                    input: decode_function_args(o, &what)?,
                });
            }
            Some("custom_tool_call") => {
                saw_call = true;
                content.push(ContentPart::ToolUse {
                    id: opt_str(o, "call_id")
                        .or_else(|| opt_str(o, "id"))
                        .ok_or_else(|| invalid_resp(format!("{what}.call_id is missing")))?,
                    name: required_str(o, "name", &what).map_err(|e| invalid_resp(e.to_string()))?.to_string(),
                    input: json!({"input": o.get("input").and_then(Value::as_str).unwrap_or("")}),
                });
            }
            _ => {} // 上游执行的内置工具调用（web_search_call 等）不进入 canonical
        }
    }
    let stop_reason =
        stop_reason_of(obj.get("status").and_then(Value::as_str), obj.get("incomplete_details"), saw_call);
    Ok(CanonicalResponse {
        id: opt_str(obj, "id").unwrap_or_default(),
        model: opt_str(obj, "model").unwrap_or_default(),
        created: obj.get("created_at").and_then(Value::as_i64),
        content,
        stop_reason,
        stop_sequence: None,
        usage: decode_usage(obj.get("usage")),
    })
}

fn stop_reason_of(status: Option<&str>, incomplete: Option<&Value>, saw_call: bool) -> StopReason {
    if status == Some("incomplete") || incomplete.is_some_and(|v| !v.is_null()) {
        return match incomplete.and_then(|d| d.get("reason")).and_then(Value::as_str) {
            Some("max_output_tokens") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::ContentFilter,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::MaxTokens,
        };
    }
    if saw_call {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    }
}

// ───────────────────────── 错误 ─────────────────────────

fn kind_from_code(code: &str) -> ApiErrorKind {
    match code {
        "rate_limit_exceeded" | "insufficient_quota" => ApiErrorKind::RateLimit,
        "invalid_api_key" | "invalid_authentication" => ApiErrorKind::Authentication,
        "server_error" | "internal_error" => ApiErrorKind::Internal,
        c if c.starts_with("invalid_") || c == "context_length_exceeded" || c == "vector_store_timeout" => {
            ApiErrorKind::InvalidRequest
        }
        _ => ApiErrorKind::BadGateway,
    }
}

/// 解码 HTTP 错误体 `{"error":{"message","type","param","code"}}`（也用于 `response.failed` / `error` 事件里的错误对象）。
pub fn decode_error(status: u16, body: &Value) -> ApiError {
    let e = body.get("error").filter(|e| e.is_object()).unwrap_or(body);
    let message = e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| match body {
        Value::String(s) if !s.trim().is_empty() => s.clone(),
        _ => format!("upstream returned HTTP {status}"),
    });
    let code = e.get("code").and_then(Value::as_str).map(str::to_string);
    let kind = if status == 0 {
        code.as_deref().map(kind_from_code).unwrap_or(ApiErrorKind::BadGateway)
    } else {
        kind_from_status(status)
    };
    let mut err = ApiError::new(kind, message);
    if let Some(c) = code {
        err = err.with_code(c);
    }
    err
}

pub fn encode_error(e: &ApiError) -> Value {
    e.to_body(ProtocolKind::OpenAiResponses)
}

fn error_code(kind: ApiErrorKind) -> &'static str {
    match kind {
        ApiErrorKind::RateLimit => "rate_limit_exceeded",
        ApiErrorKind::InvalidRequest | ApiErrorKind::PayloadTooLarge => "invalid_request_error",
        ApiErrorKind::Authentication => "invalid_api_key",
        _ => "server_error",
    }
}

/// 流内 `error` 事件（响应尚未创建时使用）。
pub fn error_event(e: &ApiError, sequence_number: u64) -> SseFrame {
    let code = e.code.clone().unwrap_or_else(|| error_code(e.kind).to_string());
    SseFrame::event(
        "error",
        json!({"type": "error", "code": code, "message": e.message, "param": null, "sequence_number": sequence_number})
            .to_string(),
    )
}

// ───────────────────────── 流式编码 ─────────────────────────

enum OpenItem {
    Message {
        item_id: String,
        output_index: usize,
        text: String,
    },
    /// 惰性创建：只有出现非空摘要文本或 Responses 签名时才产生 output item（避免空的 reasoning item）。
    Reasoning {
        item: Option<(String, usize)>,
        text: String,
        sig: Option<String>,
    },
    Function {
        item_id: String,
        output_index: usize,
        call_id: String,
        name: String,
        args: String,
    },
}

/// canonical 事件 → Responses SSE。
///
/// 顺序：`response.created` → `response.in_progress` → 每个 item：`output_item.added` → (`content_part.added` /
/// `reasoning_summary_part.added`) → `*.delta`* → `*.done` → `output_item.done`；结束时 `response.completed`
/// （`MaxTokens` / `ContentFilter` 为 `response.incomplete`）。出错时：已创建响应则 `response.failed`，否则 `error`。
/// 所有事件都带递增的 `sequence_number`。
pub struct ResponsesStreamEncoder {
    seq: u64,
    started: bool,
    finished: bool,
    id: String,
    model: String,
    created: i64,
    usage: Usage,
    open: BTreeMap<u32, OpenItem>,
    seen: std::collections::HashSet<u32>,
    done: Vec<(usize, Value)>,
    next_output: usize,
}

impl Default for ResponsesStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesStreamEncoder {
    pub fn new() -> Self {
        Self {
            seq: 0,
            started: false,
            finished: false,
            id: new_id("resp"),
            model: String::new(),
            created: chrono::Utc::now().timestamp(),
            usage: Usage::default(),
            open: BTreeMap::new(),
            seen: Default::default(),
            done: Vec::new(),
            next_output: 0,
        }
    }

    fn emit(&mut self, ty: &str, mut payload: Value) -> SseFrame {
        payload["type"] = json!(ty);
        payload["sequence_number"] = json!(self.seq);
        self.seq += 1;
        SseFrame::event(ty, payload.to_string())
    }

    fn done_output(&mut self) -> Vec<Value> {
        let mut items = self.done.clone();
        items.sort_by_key(|(i, _)| *i);
        items.into_iter().map(|(_, v)| v).collect()
    }

    fn start_reasoning(&mut self, frames: &mut Vec<SseFrame>) -> (String, usize) {
        let output_index = self.next_output;
        self.next_output += 1;
        let item_id = new_id("rs");
        frames.push(self.emit(
            "response.output_item.added",
            json!({"output_index": output_index, "item": {"id": item_id, "type": "reasoning", "summary": []}}),
        ));
        frames.push(self.emit("response.reasoning_summary_part.added", json!({"item_id": item_id, "output_index": output_index, "summary_index": 0, "part": {"type": "summary_text", "text": ""}})));
        (item_id, output_index)
    }

    fn close(&mut self, item: OpenItem, frames: &mut Vec<SseFrame>) {
        match item {
            OpenItem::Message { item_id, output_index, text } => {
                let part = json!({"type": "output_text", "text": text, "annotations": []});
                frames.push(self.emit("response.output_text.done", json!({"item_id": item_id, "output_index": output_index, "content_index": 0, "text": text, "logprobs": []})));
                frames.push(self.emit(
                    "response.content_part.done",
                    json!({"item_id": item_id, "output_index": output_index, "content_index": 0, "part": part}),
                ));
                let full = message_item(&item_id, std::slice::from_ref(&text), "completed");
                frames.push(
                    self.emit("response.output_item.done", json!({"output_index": output_index, "item": full.clone()})),
                );
                self.done.push((output_index, full));
            }
            OpenItem::Reasoning { item, text, sig } => {
                let (item_id, output_index) = match item {
                    Some(x) => x,
                    None if sig.is_some() => self.start_reasoning(frames),
                    None => return,
                };
                frames.push(self.emit(
                    "response.reasoning_summary_text.done",
                    json!({"item_id": item_id, "output_index": output_index, "summary_index": 0, "text": text}),
                ));
                frames.push(self.emit("response.reasoning_summary_part.done", json!({"item_id": item_id, "output_index": output_index, "summary_index": 0, "part": {"type": "summary_text", "text": text}})));
                let full = reasoning_item(&item_id, &text, sig.as_deref());
                frames.push(
                    self.emit("response.output_item.done", json!({"output_index": output_index, "item": full.clone()})),
                );
                self.done.push((output_index, full));
            }
            OpenItem::Function { item_id, output_index, call_id, name, args } => {
                let args = if args.is_empty() { "{}".to_string() } else { args };
                frames.push(self.emit(
                    "response.function_call_arguments.done",
                    json!({"item_id": item_id, "output_index": output_index, "name": name, "arguments": args}),
                ));
                let full = function_call_item(&item_id, &call_id, &name, &args, "completed");
                frames.push(
                    self.emit("response.output_item.done", json!({"output_index": output_index, "item": full.clone()})),
                );
                self.done.push((output_index, full));
            }
        }
    }

    fn response_json(
        &mut self,
        status: &str,
        incomplete: Option<&str>,
        with_usage: bool,
        error: Option<Value>,
    ) -> Value {
        let output = self.done_output();
        let usage = with_usage.then(|| self.usage.clone());
        response_object(
            &self.id,
            &self.model,
            self.created,
            ResponseShape { status, incomplete, output, usage: usage.as_ref(), error },
        )
    }
}

impl StreamEncoder for ResponsesStreamEncoder {
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>> {
        if self.finished {
            return Err(invalid_resp("stream event received after the stream ended"));
        }
        if let StreamEvent::Error { kind, message } = &event {
            self.finished = true;
            let mut frames = Vec::new();
            let err = ApiError::new(*kind, message.clone());
            if self.started {
                self.open.clear(); // 未完成的 item 不再补全：失败响应只带已完成的 output
                let resp = self.response_json(
                    "failed",
                    None,
                    false,
                    Some(json!({"code": error_code(*kind), "message": message})),
                );
                frames.push(self.emit("response.failed", json!({"response": resp})));
            } else {
                frames.push(error_event(&err, self.seq));
                self.seq += 1;
            }
            return Ok(frames);
        }
        if !self.started {
            let StreamEvent::MessageStart { id, model, usage } = event else {
                return Err(invalid_resp("first stream event must be message_start"));
            };
            self.started = true;
            if !id.is_empty() {
                self.id = id;
            }
            self.model = model;
            if let Some(u) = usage {
                self.usage.merge(&u);
            }
            let created = self.response_json("in_progress", None, false, None);
            let mut frames = vec![self.emit("response.created", json!({"response": created.clone()}))];
            frames.push(self.emit("response.in_progress", json!({"response": created})));
            return Ok(frames);
        }
        match event {
            StreamEvent::MessageStart { .. } => Err(invalid_resp("duplicate message_start")),
            StreamEvent::ContentBlockStart { index, block } => {
                if !self.seen.insert(index) {
                    return Err(invalid_resp(format!("content block index {index} was already used")));
                }
                let mut frames = Vec::new();
                let item = match block {
                    BlockStart::Text => {
                        let output_index = self.next_output;
                        self.next_output += 1;
                        let item_id = new_id("msg");
                        frames.push(self.emit("response.output_item.added", json!({"output_index": output_index, "item": {"id": item_id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}})));
                        frames.push(self.emit("response.content_part.added", json!({"item_id": item_id, "output_index": output_index, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}})));
                        OpenItem::Message { item_id, output_index, text: String::new() }
                    }
                    BlockStart::Thinking => OpenItem::Reasoning { item: None, text: String::new(), sig: None },
                    BlockStart::ToolUse { id, name } => {
                        let output_index = self.next_output;
                        self.next_output += 1;
                        let item_id = new_id("fc");
                        let call_id = clamp_call_id(&id);
                        frames.push(self.emit("response.output_item.added", json!({"output_index": output_index, "item": function_call_item(&item_id, &call_id, &name, "", "in_progress")})));
                        OpenItem::Function { item_id, output_index, call_id, name, args: String::new() }
                    }
                };
                self.open.insert(index, item);
                Ok(frames)
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                let kind_ok = match (&delta, self.open.get(&index)) {
                    (_, None) => {
                        return Err(invalid_resp(format!("content_block_delta for block {index} which is not open")))
                    }
                    (BlockDelta::Text { .. }, Some(OpenItem::Message { .. })) => true,
                    (BlockDelta::Thinking { .. } | BlockDelta::Signature { .. }, Some(OpenItem::Reasoning { .. })) => {
                        true
                    }
                    _ => false,
                };
                if !kind_ok {
                    return Err(invalid_resp(format!("delta type does not match the type of block {index}")));
                }
                let mut frames = Vec::new();
                match delta {
                    BlockDelta::Text { text: t } => {
                        if let Some(OpenItem::Message { item_id, output_index, text }) = self.open.get_mut(&index) {
                            text.push_str(&t);
                            let (item_id, output_index) = (item_id.clone(), *output_index);
                            frames.push(self.emit("response.output_text.delta", json!({"item_id": item_id, "output_index": output_index, "content_index": 0, "delta": t, "logprobs": []})));
                        }
                    }
                    BlockDelta::Thinking { text: t } => {
                        if t.is_empty() {
                            return Ok(frames);
                        }
                        let started = matches!(self.open.get(&index), Some(OpenItem::Reasoning { item: Some(_), .. }));
                        let created = if started { None } else { Some(self.start_reasoning(&mut frames)) };
                        if let Some(OpenItem::Reasoning { item, text, .. }) = self.open.get_mut(&index) {
                            if let Some(c) = created {
                                *item = Some(c);
                            }
                            text.push_str(&t);
                            let (item_id, output_index) = item.clone().expect("reasoning item started above");
                            frames.push(self.emit("response.reasoning_summary_text.delta", json!({"item_id": item_id, "output_index": output_index, "summary_index": 0, "delta": t})));
                        }
                    }
                    BlockDelta::Signature { signature: s } => {
                        if let (Some(raw), Some(OpenItem::Reasoning { sig, .. })) =
                            (signature::untag(signature::OPENAI_RESPONSES, &s), self.open.get_mut(&index))
                        {
                            *sig = Some(raw.to_string());
                        }
                    }
                }
                Ok(frames)
            }
            StreamEvent::ToolCallDelta { index, partial_json } => match self.open.get_mut(&index) {
                Some(OpenItem::Function { item_id, output_index, args, .. }) => {
                    args.push_str(&partial_json);
                    let (item_id, output_index) = (item_id.clone(), *output_index);
                    Ok(vec![self.emit(
                        "response.function_call_arguments.delta",
                        json!({"item_id": item_id, "output_index": output_index, "delta": partial_json}),
                    )])
                }
                _ => Err(invalid_resp(format!("tool call delta for block {index} which is not an open tool call"))),
            },
            StreamEvent::ContentBlockStop { index } => {
                let item = self
                    .open
                    .remove(&index)
                    .ok_or_else(|| invalid_resp(format!("content_block_stop for block {index} which is not open")))?;
                let mut frames = Vec::new();
                self.close(item, &mut frames);
                Ok(frames)
            }
            StreamEvent::Usage(u) => {
                self.usage.merge(&u);
                Ok(vec![])
            }
            StreamEvent::MessageStop { stop_reason, .. } => {
                self.finished = true;
                let mut frames = Vec::new();
                for (_, item) in std::mem::take(&mut self.open) {
                    self.close(item, &mut frames);
                }
                let (status, incomplete) = status_for(&stop_reason);
                let resp = self.response_json(status, incomplete, true, None);
                let ty = if status == "completed" { "response.completed" } else { "response.incomplete" };
                frames.push(self.emit(ty, json!({"response": resp})));
                Ok(frames)
            }
            StreamEvent::Error { .. } => unreachable!("handled above"),
        }
    }
}

// ───────────────────────── 流式解码 ─────────────────────────

#[derive(Default)]
struct ItemTrack {
    kind: TrackKind,
    /// message：content_index → canonical block index
    blocks: HashMap<u32, (u32, bool)>,
    canonical: Option<u32>,
    got_args: bool,
    got_text: bool,
    summary_parts: u32,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum TrackKind {
    #[default]
    Ignored,
    Message,
    Reasoning,
    Function,
    Custom,
}

/// Responses 上游 SSE → [`StreamEvent`]。容忍缺失的 `content_part.added` / `output_item.added`（部分兼容服务不发），
/// 忽略未知事件类型；`finish()` 必须在上游 EOF 后调用。
pub struct ResponsesStreamDecoder {
    sse: SseParser,
    started: bool,
    finished: bool,
    next_index: u32,
    items: HashMap<u32, ItemTrack>,
    saw_call: bool,
}

impl Default for ResponsesStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn json_string_body(s: &str) -> String {
    let q = Value::String(s.to_string()).to_string();
    q[1..q.len() - 1].to_string()
}

impl ResponsesStreamDecoder {
    pub fn new() -> Self {
        Self {
            sse: SseParser::default(),
            started: false,
            finished: false,
            next_index: 0,
            items: HashMap::new(),
            saw_call: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        let frames = self.sse.push(bytes);
        let mut out = Vec::new();
        for f in frames {
            self.handle_frame(&f, &mut out)?;
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        let frames = self.sse.finish();
        let mut out = Vec::new();
        for f in frames {
            self.handle_frame(&f, &mut out)?;
        }
        if !self.finished {
            if !self.started {
                return Err(invalid_resp("upstream stream ended without any data"));
            }
            self.finished = true;
            out.push(StreamEvent::Error {
                kind: ApiErrorKind::BadGateway,
                message: "upstream stream ended before response.completed".into(),
            });
        }
        Ok(out)
    }

    fn alloc(&mut self) -> u32 {
        let i = self.next_index;
        self.next_index += 1;
        i
    }

    fn ensure_started(&mut self, resp: Option<&Value>, out: &mut Vec<StreamEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(StreamEvent::MessageStart {
            id: resp
                .and_then(|r| r.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| new_id("resp")),
            model: resp.and_then(|r| r.get("model")).and_then(Value::as_str).unwrap_or("").to_string(),
            usage: None,
        });
    }

    fn handle_frame(&mut self, f: &SseFrame, out: &mut Vec<StreamEvent>) -> ProtocolResult<()> {
        let data = f.data.trim();
        if data.is_empty() || data == "[DONE]" || self.finished {
            return Ok(());
        }
        let v: Value =
            serde_json::from_str(data).map_err(|e| invalid_resp(format!("malformed Responses stream event: {e}")))?;
        let ty =
            v.get("type").and_then(Value::as_str).map(str::to_string).or_else(|| f.event.clone()).unwrap_or_default();
        self.handle_event(&ty, &v, out)
    }

    fn track(&mut self, oi: u32) -> &mut ItemTrack {
        self.items.entry(oi).or_default()
    }

    /// 打开（或取回）message item 的某个 content part 对应的文本块。
    fn text_block(&mut self, oi: u32, ci: u32, out: &mut Vec<StreamEvent>) -> u32 {
        let t = self.track(oi);
        if t.kind == TrackKind::Ignored {
            t.kind = TrackKind::Message;
        }
        if let Some((idx, open)) = t.blocks.get(&ci).copied() {
            if open {
                return idx;
            }
        }
        let idx = self.alloc();
        self.track(oi).blocks.insert(ci, (idx, true));
        out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::Text });
        idx
    }

    fn thinking_block(&mut self, oi: u32, out: &mut Vec<StreamEvent>) -> u32 {
        if let Some(i) = self.items.get(&oi).and_then(|t| t.canonical) {
            return i;
        }
        let idx = self.alloc();
        let t = self.track(oi);
        t.kind = TrackKind::Reasoning;
        t.canonical = Some(idx);
        out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::Thinking });
        idx
    }

    fn handle_event(&mut self, ty: &str, v: &Value, out: &mut Vec<StreamEvent>) -> ProtocolResult<()> {
        let oi = v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let ci = v.get("content_index").and_then(Value::as_u64).unwrap_or(0) as u32;
        match ty {
            "response.created" | "response.in_progress" | "response.queued" => {
                self.ensure_started(v.get("response"), out);
            }
            "response.output_item.added" => {
                self.ensure_started(None, out);
                let item = v.get("item").unwrap_or(&Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => self.track(oi).kind = TrackKind::Message,
                    Some("reasoning") => {
                        self.thinking_block(oi, out);
                    }
                    Some(t @ ("function_call" | "custom_tool_call")) => {
                        let idx = self.alloc();
                        self.saw_call = true;
                        let tr = self.track(oi);
                        tr.kind = if t == "function_call" { TrackKind::Function } else { TrackKind::Custom };
                        tr.canonical = Some(idx);
                        out.push(StreamEvent::ContentBlockStart {
                            index: idx,
                            block: BlockStart::ToolUse {
                                id: item
                                    .get("call_id")
                                    .or_else(|| item.get("id"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                name: item.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                            },
                        });
                        if t == "custom_tool_call" {
                            out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: "{\"input\":\"".into() });
                        }
                    }
                    _ => {}
                }
            }
            "response.content_part.added" => {
                self.ensure_started(None, out);
                let part = v.get("part").unwrap_or(&Value::Null);
                if matches!(part.get("type").and_then(Value::as_str), Some("output_text") | Some("refusal")) {
                    let idx = self.text_block(oi, ci, out);
                    if let Some(t) = part.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                        self.track(oi).got_text = true;
                        out.push(StreamEvent::ContentBlockDelta {
                            index: idx,
                            delta: BlockDelta::Text { text: t.to_string() },
                        });
                    }
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                self.ensure_started(None, out);
                let idx = self.text_block(oi, ci, out);
                if let Some(d) = v.get("delta").and_then(Value::as_str).filter(|d| !d.is_empty()) {
                    self.track(oi).got_text = true;
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Text { text: d.to_string() },
                    });
                }
            }
            "response.output_text.done" | "response.refusal.done" => {
                self.ensure_started(None, out);
                let full = v.get("text").or_else(|| v.get("refusal")).and_then(Value::as_str).unwrap_or("");
                if !self.items.get(&oi).is_some_and(|t| t.got_text) && !full.is_empty() {
                    let idx = self.text_block(oi, ci, out);
                    self.track(oi).got_text = true;
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Text { text: full.to_string() },
                    });
                }
            }
            "response.content_part.done" => {
                if let Some((idx, true)) = self.items.get(&oi).and_then(|t| t.blocks.get(&ci)).copied() {
                    self.track(oi).blocks.insert(ci, (idx, false));
                    out.push(StreamEvent::ContentBlockStop { index: idx });
                }
            }
            "response.reasoning_summary_part.added" => {
                self.ensure_started(None, out);
                let idx = self.thinking_block(oi, out);
                let t = self.track(oi);
                let first = t.summary_parts == 0;
                t.summary_parts += 1;
                if !first {
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Thinking { text: "\n\n".into() },
                    });
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.ensure_started(None, out);
                let idx = self.thinking_block(oi, out);
                if let Some(d) = v.get("delta").and_then(Value::as_str).filter(|d| !d.is_empty()) {
                    self.track(oi).got_text = true;
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Thinking { text: d.to_string() },
                    });
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                let full = v.get("text").and_then(Value::as_str).unwrap_or("");
                if !self.items.get(&oi).is_some_and(|t| t.got_text) && !full.is_empty() {
                    let idx = self.thinking_block(oi, out);
                    self.track(oi).got_text = true;
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Thinking { text: full.to_string() },
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(idx), Some(d)) =
                    (self.items.get(&oi).and_then(|t| t.canonical), v.get("delta").and_then(Value::as_str))
                {
                    if !d.is_empty() {
                        self.track(oi).got_args = true;
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: d.to_string() });
                    }
                }
            }
            "response.function_call_arguments.done" => {
                let full = v.get("arguments").and_then(Value::as_str).unwrap_or("");
                if let Some(idx) = self.items.get(&oi).and_then(|t| t.canonical) {
                    if !self.items.get(&oi).is_some_and(|t| t.got_args) && !full.is_empty() {
                        self.track(oi).got_args = true;
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: full.to_string() });
                    }
                }
            }
            "response.custom_tool_call_input.delta" => {
                if let (Some(idx), Some(d)) =
                    (self.items.get(&oi).and_then(|t| t.canonical), v.get("delta").and_then(Value::as_str))
                {
                    if !d.is_empty() {
                        self.track(oi).got_args = true;
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: json_string_body(d) });
                    }
                }
            }
            "response.custom_tool_call_input.done" => {
                let full = v.get("input").and_then(Value::as_str).unwrap_or("");
                if let Some(idx) = self.items.get(&oi).and_then(|t| t.canonical) {
                    if !self.items.get(&oi).is_some_and(|t| t.got_args) && !full.is_empty() {
                        self.track(oi).got_args = true;
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: json_string_body(full) });
                    }
                }
            }
            "response.output_item.done" => {
                self.ensure_started(None, out);
                let item = v.get("item").unwrap_or(&Value::Null).clone();
                self.finish_item(oi, &item, out);
            }
            "response.completed" | "response.incomplete" => {
                self.ensure_started(v.get("response"), out);
                let resp = v.get("response").unwrap_or(&Value::Null);
                // 兜底：关闭仍未关闭的块（不规范的上游）
                let open: Vec<u32> = self.items.keys().copied().collect();
                for oi in open {
                    self.finish_item(oi, &Value::Null, out);
                }
                let usage = decode_usage(resp.get("usage"));
                if !usage.is_empty() {
                    out.push(StreamEvent::Usage(usage));
                }
                let status = if ty == "response.incomplete" {
                    Some("incomplete")
                } else {
                    resp.get("status").and_then(Value::as_str)
                };
                let stop_reason = stop_reason_of(status, resp.get("incomplete_details"), self.saw_call);
                out.push(StreamEvent::MessageStop { stop_reason, stop_sequence: None });
                self.finished = true;
            }
            "response.failed" => {
                let e = v.get("response").and_then(|r| r.get("error")).cloned().unwrap_or(Value::Null);
                let err = decode_error(0, &json!({"error": e}));
                out.push(StreamEvent::Error { kind: err.kind, message: err.message });
                self.finished = true;
            }
            "error" => {
                let err = decode_error(0, &json!({"error": {"message": v.get("message"), "code": v.get("code")}}));
                out.push(StreamEvent::Error { kind: err.kind, message: err.message });
                self.finished = true;
            }
            _ => {
                tracing::debug!(event = ty, "ignoring unknown Responses stream event");
            }
        }
        Ok(())
    }

    /// 收尾一个 output item：补发缺失的内容并关闭块。`item` 为 `Null` 时只做关闭。
    fn finish_item(&mut self, oi: u32, item: &Value, out: &mut Vec<StreamEvent>) {
        let Some(mut t) = self.items.remove(&oi) else {
            // 没有见过 added 事件的 item：用 done 里的完整内容合成。
            self.synthesize_item(item, out);
            return;
        };
        match t.kind {
            TrackKind::Message => {
                if t.blocks.is_empty() {
                    for part in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                        let text =
                            part.get("text").or_else(|| part.get("refusal")).and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            let idx = self.alloc();
                            out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::Text });
                            out.push(StreamEvent::ContentBlockDelta {
                                index: idx,
                                delta: BlockDelta::Text { text: text.to_string() },
                            });
                            out.push(StreamEvent::ContentBlockStop { index: idx });
                        }
                    }
                }
                let mut open: Vec<(u32, u32)> =
                    t.blocks.drain().filter(|(_, (_, o))| *o).map(|(c, (i, _))| (c, i)).collect();
                open.sort();
                for (_, idx) in open {
                    out.push(StreamEvent::ContentBlockStop { index: idx });
                }
            }
            TrackKind::Reasoning => {
                let idx = t.canonical.expect("reasoning track has a block");
                if !t.got_text {
                    let summary: Vec<&str> = item
                        .get("summary")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.get("text").and_then(Value::as_str))
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    if !summary.is_empty() {
                        out.push(StreamEvent::ContentBlockDelta {
                            index: idx,
                            delta: BlockDelta::Thinking { text: summary.join("\n\n") },
                        });
                    }
                }
                if let Some(enc) = item.get("encrypted_content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    out.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::Signature { signature: signature::tag(signature::OPENAI_RESPONSES, enc) },
                    });
                }
                out.push(StreamEvent::ContentBlockStop { index: idx });
            }
            TrackKind::Function => {
                let idx = t.canonical.expect("function track has a block");
                if !t.got_args {
                    if let Some(a) = item.get("arguments").and_then(Value::as_str).filter(|a| !a.is_empty()) {
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: a.to_string() });
                    }
                }
                out.push(StreamEvent::ContentBlockStop { index: idx });
            }
            TrackKind::Custom => {
                let idx = t.canonical.expect("custom track has a block");
                if !t.got_args {
                    if let Some(a) = item.get("input").and_then(Value::as_str).filter(|a| !a.is_empty()) {
                        out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: json_string_body(a) });
                    }
                }
                out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: "\"}".into() });
                out.push(StreamEvent::ContentBlockStop { index: idx });
            }
            TrackKind::Ignored => {}
        }
    }

    fn synthesize_item(&mut self, item: &Value, out: &mut Vec<StreamEvent>) {
        let Ok(resp) = decode_response(&json!({"output": [item], "status": "completed"})) else { return };
        for part in resp.content {
            let idx = self.alloc();
            match part {
                ContentPart::Text { text } => {
                    out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::Text });
                    out.push(StreamEvent::ContentBlockDelta { index: idx, delta: BlockDelta::Text { text } });
                }
                ContentPart::Thinking { text, signature } => {
                    out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::Thinking });
                    if !text.is_empty() {
                        out.push(StreamEvent::ContentBlockDelta { index: idx, delta: BlockDelta::Thinking { text } });
                    }
                    if let Some(signature) = signature {
                        out.push(StreamEvent::ContentBlockDelta {
                            index: idx,
                            delta: BlockDelta::Signature { signature },
                        });
                    }
                }
                ContentPart::ToolUse { id, name, input } => {
                    self.saw_call = true;
                    out.push(StreamEvent::ContentBlockStart { index: idx, block: BlockStart::ToolUse { id, name } });
                    out.push(StreamEvent::ToolCallDelta { index: idx, partial_json: input.to_string() });
                }
                _ => continue,
            }
            out.push(StreamEvent::ContentBlockStop { index: idx });
        }
    }
}

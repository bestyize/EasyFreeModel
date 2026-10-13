//! Ollama（`POST /api/chat`，NDJSON 流）编解码。
//!
//! # 入口侧
//! [`decode_request`]、[`encode_response`]、[`OllamaStreamEncoder`]（逐行 NDJSON）、[`encode_error`] / [`error_line`]。
//!
//! # 上游侧
//! [`encode_request`]、[`decode_response`]、[`OllamaStreamDecoder`]（增量，处理半行与分片）、[`decode_error`]。
//!
//! # 约定与取舍（均有测试）
//! - **`stream` 默认为 true**（Ollama 与 OpenAI 相反）：请求里缺省 `stream` 时解码为流式；编码到上游时总是显式写出。
//! - `messages[].images` 是裸 base64 数组（无 `data:` 前缀）；canonical 里只有 `Base64` 图片能发给 Ollama，
//!   URL / file_id 图片丢弃并告警（网关需要时应先下载）。解码时按魔数猜 MIME，猜不出用 `image/png` 并告警。
//! - `tool_calls[].function.arguments` 是**对象**而非 JSON 字符串；工具结果消息用 `tool_name`（而非 id）配对，
//!   解码时按「最近一条助手消息里尚未配对的同名调用」还原 `tool_use_id`，没有 `id` 的调用按序生成确定性 id（`call_N`）。
//! - Ollama 的 tool call 在流里整块到达，不会增量；编码流式响应时同样在 `ContentBlockStop` 时整块输出。
//! - `options` 里只映射 temperature / top_p / top_k / num_predict / stop / seed / presence_penalty / frequency_penalty；
//!   其余（num_ctx、repeat_penalty、min_p ...）进入 warnings。`num_predict <= 0`（无限 / 填满上下文）视为未设置。
//! - `think`：bool 或 `low/medium/high`（gpt-oss）。编码时 `gpt-oss*` 模型用档位字符串，其它模型用 bool。
//! - 没有 `tool_choice`：`none` 通过省略 `tools` 实现，`any` / 指定工具无法表达，产生 warning。
//! - `done_reason` 为 `stop` 时若本轮出现过 tool call，则 canonical `stop_reason = ToolUse`。
//! - **用量**：`prompt_eval_count` → `input_tokens`，`eval_count` → `output_tokens`；Ollama 没有缓存 / 推理 token 拆分。
//! - 流内错误是单独一行 `{"error":"..."}`，解码为 `StreamEvent::Error`。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::canonical::*;
use super::common::*;
use super::wire::{kind_from_status, NdjsonParser};
use super::{ApiError, ApiErrorKind, Encoded, ProtocolError, ProtocolResult};

const KNOWN_FIELDS: &[&str] = &["model", "messages", "stream", "tools", "options", "think"];
const KNOWN_OPTIONS: &[&str] =
    &["temperature", "top_p", "top_k", "num_predict", "stop", "seed", "presence_penalty", "frequency_penalty"];

fn new_call_id() -> String {
    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
}

fn sniff_media_type(b64: &str) -> Option<&'static str> {
    [
        ("iVBOR", "image/png"),
        ("/9j/", "image/jpeg"),
        ("R0lGOD", "image/gif"),
        ("UklGR", "image/webp"),
        ("Qk", "image/bmp"),
    ]
    .into_iter()
    .find(|(p, _)| b64.starts_with(p))
    .map(|(_, m)| m)
}

// ───────────────────────── 请求解码 ─────────────────────────

struct Pending {
    id: String,
    name: String,
    resolved: bool,
}

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

    req.stream = opt_bool(obj, "stream", "request")?.unwrap_or(true);
    req.include_usage = true;

    if let Some(opts) = obj.get("options").filter(|v| !v.is_null()) {
        let o = as_object(opts, "options")?;
        let num_predict =
            o.get("num_predict").and_then(Value::as_i64).filter(|n| *n > 0).and_then(|n| u32::try_from(n).ok());
        req.params = GenerationParams {
            max_tokens: num_predict,
            temperature: opt_f64(o, "temperature", "options")?,
            top_p: opt_f64(o, "top_p", "options")?,
            top_k: opt_u32(o, "top_k", "options")?,
            stop: match o.get("stop") {
                None | Some(Value::Null) => vec![],
                Some(Value::String(s)) => vec![s.clone()],
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|x| {
                        x.as_str().map(str::to_string).ok_or_else(|| invalid("options.stop must contain only strings"))
                    })
                    .collect::<ProtocolResult<_>>()?,
                Some(_) => return Err(invalid("options.stop must be a string or an array of strings")),
            },
            presence_penalty: opt_f64(o, "presence_penalty", "options")?,
            frequency_penalty: opt_f64(o, "frequency_penalty", "options")?,
            seed: o.get("seed").and_then(Value::as_i64),
        };
        let mut unknown: Vec<&str> = o.keys().map(String::as_str).filter(|k| !KNOWN_OPTIONS.contains(k)).collect();
        unknown.sort_unstable();
        for k in unknown {
            req.warnings.push(format!("unsupported field ignored: options.{k}"));
        }
    }

    match obj.get("think") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => {
            req.reasoning = Some(ReasoningConfig { include_thoughts: Some(*b), ..Default::default() })
        }
        Some(Value::String(s)) => {
            req.reasoning =
                Some(ReasoningConfig { effort: Some(s.clone()), include_thoughts: Some(true), budget_tokens: None })
        }
        Some(_) => return Err(invalid("request.think must be a boolean or a string")),
    }

    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        let arr = tools.as_array().ok_or_else(|| invalid("request.tools must be an array"))?;
        for (i, t) in arr.iter().enumerate() {
            let what = format!("tools[{i}]");
            let o = as_object(t, &what)?;
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("function");
            if ty != "function" {
                return Err(ProtocolError::Unsupported(format!(
                    "{what}.type `{ty}` (only `function` tools are supported)"
                )));
            }
            let f =
                as_object(o.get("function").ok_or_else(|| invalid(format!("{what}.function is required")))?, &what)?;
            req.tools.push(ToolDefinition {
                name: required_str(f, "name", &format!("{what}.function"))?.to_string(),
                description: opt_str(f, "description"),
                input_schema: f
                    .get("parameters")
                    .filter(|p| !p.is_null())
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            });
        }
    }

    warn_unknown_fields(obj, KNOWN_FIELDS, &mut req.warnings);
    Ok(req)
}

fn decode_images(m: &Map<String, Value>, what: &str, warnings: &mut Vec<String>) -> ProtocolResult<Vec<ContentPart>> {
    let Some(images) = m.get("images").filter(|v| !v.is_null()) else {
        return Ok(vec![]);
    };
    let arr = images.as_array().ok_or_else(|| invalid(format!("{what}.images must be an array")))?;
    let mut out = Vec::new();
    for (i, img) in arr.iter().enumerate() {
        let s = img.as_str().ok_or_else(|| invalid(format!("{what}.images[{i}] must be a base64 string")))?;
        let part = match parse_data_url(s)? {
            Some((media_type, data)) => {
                ContentPart::Image { source: ImageSource::Base64 { media_type, data }, detail: None }
            }
            None => {
                let media_type = match sniff_media_type(s) {
                    Some(m) => m,
                    None => {
                        warnings
                            .push(format!("{what}.images[{i}]: media type could not be detected; assumed image/png"));
                        "image/png"
                    }
                };
                ContentPart::Image {
                    source: ImageSource::Base64 { media_type: media_type.to_string(), data: s.to_string() },
                    detail: None,
                }
            }
        };
        out.push(part);
    }
    Ok(out)
}

fn text_content(m: &Map<String, Value>, what: &str) -> ProtocolResult<String> {
    match m.get("content") {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(invalid(format!("{what}.content must be a string"))),
    }
}

fn decode_messages(messages: &[Value], req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let mut seen_non_system = false;
    let mut pending: Vec<Pending> = Vec::new();
    let mut counter = 0usize;
    let mut last_was_tool = false;
    for (i, m) in messages.iter().enumerate() {
        let what = format!("messages[{i}]");
        let mo = as_object(m, &what)?;
        let role = required_str(mo, "role", &what)?;
        let mut is_tool = false;
        match role {
            "system" => {
                if seen_non_system {
                    req.warnings.push(format!(
                        "{what}: system message after conversation start was hoisted to the system prompt"
                    ));
                }
                req.system.push(text_content(mo, &what)?);
            }
            "user" => {
                seen_non_system = true;
                let text = text_content(mo, &what)?;
                let mut parts = Vec::new();
                if !text.is_empty() {
                    parts.push(ContentPart::text(text));
                }
                parts.extend(decode_images(mo, &what, &mut req.warnings)?);
                req.messages.push(Message::user(parts));
            }
            "assistant" => {
                seen_non_system = true;
                pending.clear();
                let mut parts = Vec::new();
                if let Some(t) = mo.get("thinking").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    parts.push(ContentPart::Thinking { text: t.to_string(), signature: None });
                }
                let text = text_content(mo, &what)?;
                if !text.is_empty() {
                    parts.push(ContentPart::text(text));
                }
                if mo.get("images").is_some_and(|v| !v.is_null()) {
                    req.warnings.push(format!("{what}: images on an assistant message were dropped"));
                }
                if let Some(calls) = mo.get("tool_calls").filter(|v| !v.is_null()) {
                    let calls =
                        calls.as_array().ok_or_else(|| invalid(format!("{what}.tool_calls must be an array")))?;
                    for (j, c) in calls.iter().enumerate() {
                        let cw = format!("{what}.tool_calls[{j}]");
                        let co = as_object(c, &cw)?;
                        let f = as_object(
                            co.get("function").ok_or_else(|| invalid(format!("{cw}.function is required")))?,
                            &cw,
                        )?;
                        let name = required_str(f, "name", &format!("{cw}.function"))?.to_string();
                        let input = match f.get("arguments") {
                            None | Some(Value::Null) => json!({}),
                            Some(v @ Value::Object(_)) => v.clone(),
                            Some(Value::String(s)) => parse_json_arguments(s)
                                .map_err(|e| invalid(format!("{cw}.function.arguments is not valid JSON: {e}")))?,
                            Some(_) => return Err(invalid(format!("{cw}.function.arguments must be an object"))),
                        };
                        let id =
                            opt_str(co, "id").filter(|s| !s.is_empty()).unwrap_or_else(|| format!("call_{counter}"));
                        counter += 1;
                        pending.push(Pending { id: id.clone(), name: name.clone(), resolved: false });
                        parts.push(ContentPart::ToolUse { id, name, input });
                    }
                }
                req.messages.push(Message::assistant(parts));
            }
            "tool" => {
                seen_non_system = true;
                is_tool = true;
                let name = opt_str(mo, "tool_name").or_else(|| opt_str(mo, "name"));
                let explicit = opt_str(mo, "tool_call_id").filter(|s| !s.is_empty());
                let idx = match (&explicit, &name) {
                    (Some(id), _) => pending.iter().position(|p| &p.id == id),
                    (None, Some(n)) => pending.iter().position(|p| !p.resolved && &p.name == n),
                    (None, None) => {
                        let unresolved: Vec<usize> =
                            pending.iter().enumerate().filter(|(_, p)| !p.resolved).map(|(i, _)| i).collect();
                        if unresolved.len() == 1 {
                            Some(unresolved[0])
                        } else {
                            None
                        }
                    }
                };
                let id = match idx {
                    Some(k) => {
                        pending[k].resolved = true;
                        pending[k].id.clone()
                    }
                    None => {
                        req.warnings.push(format!("{what}: tool message could not be matched to a tool call"));
                        let id = explicit.unwrap_or_else(|| format!("call_{counter}"));
                        counter += 1;
                        id
                    }
                };
                let part = ContentPart::ToolResult {
                    tool_use_id: id,
                    content: vec![ContentPart::text(text_content(mo, &what)?)],
                    is_error: false,
                };
                match req.messages.last_mut() {
                    Some(last) if last_was_tool => last.content.push(part),
                    _ => req.messages.push(Message::user(vec![part])),
                }
            }
            other => return Err(ProtocolError::Unsupported(format!("{what}.role `{other}`"))),
        }
        last_was_tool = is_tool;
    }
    Ok(())
}

// ───────────────────────── 请求编码（发往 Ollama 上游） ─────────────────────────

fn flatten_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| if let ContentPart::Text { text } = p { Some(text.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

fn image_base64(source: &ImageSource, warnings: &mut Vec<String>) -> Option<String> {
    match source {
        ImageSource::Base64 { data, .. } => Some(data.clone()),
        ImageSource::Url { .. } => {
            warnings.push("image URL cannot be sent to Ollama (base64 only) and was dropped".into());
            None
        }
        ImageSource::FileId { .. } => {
            warnings.push("image referenced by file_id cannot be sent to Ollama and was dropped".into());
            None
        }
    }
}

pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let mut warnings = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for s in &req.system {
        messages.push(json!({"role": "system", "content": s}));
    }
    let mut call_names: BTreeMap<String, String> = BTreeMap::new();
    for m in &req.messages {
        match m.role {
            Role::User => {
                let mut texts: Vec<&str> = Vec::new();
                let mut images: Vec<String> = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::ToolResult { tool_use_id, content, is_error } => {
                            if *is_error {
                                warnings.push(
                                    "tool_result.is_error cannot be represented in Ollama and was dropped".into(),
                                );
                            }
                            if content.iter().any(|c| matches!(c, ContentPart::Image { .. })) {
                                warnings
                                    .push("images inside tool_result cannot be sent to Ollama and were dropped".into());
                            }
                            let name = call_names.get(tool_use_id).cloned().unwrap_or_else(|| {
                                warnings.push(format!("tool_result `{tool_use_id}` has no matching tool_use; its id was used as tool_name"));
                                tool_use_id.clone()
                            });
                            messages.push(json!({"role": "tool", "tool_name": name, "content": flatten_text(content)}));
                        }
                        ContentPart::Text { text } => texts.push(text),
                        ContentPart::Image { source, .. } => {
                            if let Some(b) = image_base64(source, &mut warnings) {
                                images.push(b);
                            }
                        }
                        ContentPart::ToolUse { .. } | ContentPart::Thinking { .. } => {
                            warnings.push("tool_use/thinking part inside a user message was dropped".into());
                        }
                    }
                }
                if !texts.is_empty() || !images.is_empty() {
                    let mut msg = json!({"role": "user", "content": texts.join("\n")});
                    if !images.is_empty() {
                        msg["images"] = json!(images);
                    }
                    messages.push(msg);
                }
            }
            Role::Assistant => {
                let mut texts: Vec<&str> = Vec::new();
                let mut thinking: Vec<&str> = Vec::new();
                let mut calls: Vec<Value> = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::Text { text } => texts.push(text),
                        ContentPart::Thinking { text, .. } => {
                            if !text.is_empty() {
                                thinking.push(text);
                            }
                        }
                        ContentPart::ToolUse { id, name, input } => {
                            call_names.insert(id.clone(), name.clone());
                            // `index` 是该助手消息内的序号
                            calls.push(
                                json!({"id": id, "function": {"index": calls.len(), "name": name, "arguments": input}}),
                            );
                        }
                        ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {
                            warnings.push("image/tool_result part inside an assistant message was dropped".into());
                        }
                    }
                }
                let mut msg = json!({"role": "assistant", "content": texts.join("\n")});
                if !thinking.is_empty() {
                    msg["thinking"] = json!(thinking.join("\n"));
                }
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                messages.push(msg);
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(req.stream));

    let mut options = Map::new();
    let p = &req.params;
    if let Some(v) = p.temperature {
        options.insert("temperature".into(), json!(v));
    }
    if let Some(v) = p.top_p {
        options.insert("top_p".into(), json!(v));
    }
    if let Some(v) = p.top_k {
        options.insert("top_k".into(), json!(v));
    }
    if let Some(v) = p.max_tokens {
        options.insert("num_predict".into(), json!(v));
    }
    if !p.stop.is_empty() {
        options.insert("stop".into(), json!(p.stop));
    }
    if let Some(v) = p.seed {
        options.insert("seed".into(), json!(v));
    }
    if let Some(v) = p.presence_penalty {
        options.insert("presence_penalty".into(), json!(v));
    }
    if let Some(v) = p.frequency_penalty {
        options.insert("frequency_penalty".into(), json!(v));
    }
    if !options.is_empty() {
        body.insert("options".into(), Value::Object(options));
    }

    let drop_tools = matches!(req.tool_choice, Some(ToolChoice::None));
    if !req.tools.is_empty() && !drop_tools {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut f = Map::new();
                f.insert("name".into(), json!(t.name));
                f.insert("description".into(), json!(t.description.clone().unwrap_or_default()));
                f.insert("parameters".into(), t.input_schema.clone());
                json!({"type": "function", "function": f})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if matches!(req.tool_choice, Some(ToolChoice::Any) | Some(ToolChoice::Tool { .. })) {
            warnings.push("tool_choice any/specific tool cannot be forced on Ollama; the model decides freely".into());
        }
        if req.parallel_tool_calls == Some(false) {
            warnings.push("parallel_tool_calls=false cannot be expressed in Ollama and was ignored".into());
        }
    }
    if let Some(r) = &req.reasoning {
        let gpt_oss = req.model.to_ascii_lowercase().contains("gpt-oss");
        let effort = r.effort.as_deref();
        let off = effort == Some("none") || r.include_thoughts == Some(false);
        let think = if gpt_oss {
            match effort {
                Some("low") | Some("minimal") => Some(json!("low")),
                Some("medium") => Some(json!("medium")),
                Some("high") => Some(json!("high")),
                _ => (r.include_thoughts == Some(true)).then(|| json!("medium")),
            }
        } else if off {
            Some(json!(false))
        } else if effort.is_some() || r.include_thoughts == Some(true) || r.budget_tokens.is_some_and(|b| b > 0) {
            Some(json!(true))
        } else {
            None
        };
        if let Some(t) = think {
            body.insert("think".into(), t);
        }
    }
    if req.user.is_some() {
        warnings.push("user identifier is not supported by Ollama and was dropped".into());
    }
    Ok(Encoded { body: Value::Object(body), warnings })
}

// ───────────────────────── 响应编码 / 解码 ─────────────────────────

fn done_reason(r: &StopReason) -> &'static str {
    match r {
        StopReason::MaxTokens => "length",
        _ => "stop",
    }
}

fn rfc3339(ts: Option<i64>) -> String {
    let dt = ts.and_then(|t| chrono::DateTime::from_timestamp(t, 0)).unwrap_or_else(chrono::Utc::now);
    dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn counts(v: &mut Value, u: &Usage) {
    if let Some(n) = u.input_tokens {
        v["prompt_eval_count"] = json!(n);
    }
    if let Some(n) = u.output_tokens {
        v["eval_count"] = json!(n);
    }
}

pub fn encode_response(resp: &CanonicalResponse) -> Value {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Value> = Vec::new();
    for p in &resp.content {
        match p {
            ContentPart::Text { text: t } => text.push_str(t),
            ContentPart::Thinking { text: t, .. } => thinking.push_str(t),
            ContentPart::ToolUse { id, name, input } => {
                calls.push(json!({"id": id, "function": {"index": calls.len(), "name": name, "arguments": input}}))
            }
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {}
        }
    }
    let mut message = json!({"role": "assistant", "content": text});
    if !thinking.is_empty() {
        message["thinking"] = json!(thinking);
    }
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    let mut v = json!({
        "model": resp.model,
        "created_at": rfc3339(resp.created),
        "message": message,
        "done": true,
        "done_reason": done_reason(&resp.stop_reason),
    });
    counts(&mut v, &resp.usage);
    v
}

fn decode_usage(o: &Map<String, Value>) -> Usage {
    let get = |k: &str| o.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    Usage { input_tokens: get("prompt_eval_count"), output_tokens: get("eval_count"), ..Default::default() }
}

fn decode_stop(reason: Option<&str>, saw_tool: bool) -> StopReason {
    match reason {
        Some("length") => StopReason::MaxTokens,
        Some("stop") | None => {
            if saw_tool {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
        Some(other) => StopReason::Other(other.to_string()),
    }
}

fn decode_call(c: &Value, what: &str) -> ProtocolResult<(String, String, Value)> {
    let co = c.as_object().ok_or_else(|| invalid_resp(format!("{what} must be an object")))?;
    let f = co
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_resp(format!("{what}.function is missing")))?;
    let name = f
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_resp(format!("{what}.function.name is missing")))?;
    let input = match f.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(Value::String(s)) => {
            parse_json_arguments(s).map_err(|e| invalid_resp(format!("{what}.function.arguments: {e}")))?
        }
        Some(_) => return Err(invalid_resp(format!("{what}.function.arguments must be an object"))),
    };
    let id = opt_str(co, "id").filter(|s| !s.is_empty()).unwrap_or_else(new_call_id);
    Ok((id, name.to_string(), input))
}

pub fn decode_response(body: &Value) -> ProtocolResult<CanonicalResponse> {
    let obj = body.as_object().ok_or_else(|| invalid_resp("response body must be an object"))?;
    if let Some(e) = obj.get("error") {
        return Err(invalid_resp(format!("upstream returned an error: {}", error_text(e))));
    }
    let msg =
        obj.get("message").and_then(Value::as_object).ok_or_else(|| invalid_resp("response.message is missing"))?;
    let mut content = Vec::new();
    if let Some(t) = msg.get("thinking").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        content.push(ContentPart::Thinking { text: t.to_string(), signature: None });
    }
    if let Some(t) = msg.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        content.push(ContentPart::text(t));
    }
    let mut saw_tool = false;
    for (i, c) in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let (id, name, input) = decode_call(c, &format!("tool_calls[{i}]"))?;
        saw_tool = true;
        content.push(ContentPart::ToolUse { id, name, input });
    }
    let created = obj
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp());
    Ok(CanonicalResponse {
        id: format!("ollama-{}", uuid::Uuid::new_v4().simple()),
        model: opt_str(obj, "model").unwrap_or_default(),
        created,
        content,
        stop_reason: decode_stop(obj.get("done_reason").and_then(Value::as_str), saw_tool),
        stop_sequence: None,
        usage: decode_usage(obj),
    })
}

// ───────────────────────── 错误 ─────────────────────────

fn error_text(e: &Value) -> String {
    match e {
        Value::String(s) => s.clone(),
        Value::Object(o) => o.get("message").and_then(Value::as_str).unwrap_or("unknown upstream error").to_string(),
        other => other.to_string(),
    }
}

/// 解码上游错误体 `{"error":"..."}`（或非 JSON 文本，以字符串 Value 传入）。
pub fn decode_error(status: u16, body: &Value) -> ApiError {
    let message = match body.get("error") {
        Some(e) => error_text(e),
        None => match body {
            Value::String(s) if !s.trim().is_empty() => s.clone(),
            _ => format!("upstream returned HTTP {status}"),
        },
    };
    ApiError::new(kind_from_status(status), message)
}

/// Ollama 风格错误体：`{"error":"message"}`（状态码由 HTTP 层承载，见 [`ApiError::status`]）。
pub fn encode_error(e: &ApiError) -> Value {
    json!({"error": e.message})
}

/// 流内错误行（NDJSON，含换行）。
pub fn error_line(e: &ApiError) -> String {
    format!("{}\n", encode_error(e))
}

// ───────────────────────── 流式编码（canonical → NDJSON） ─────────────────────────

enum OpenBlock {
    Text,
    Thinking,
    Tool { id: String, name: String, args: String },
}

/// canonical 事件 → NDJSON 行（每个返回值是一行，已带结尾换行，可直接写入响应体）。
pub struct OllamaStreamEncoder {
    started: bool,
    finished: bool,
    model: String,
    created_at: String,
    usage: Usage,
    open: BTreeMap<u32, OpenBlock>,
    tool_count: usize,
}

impl Default for OllamaStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaStreamEncoder {
    pub fn new() -> Self {
        Self {
            started: false,
            finished: false,
            model: String::new(),
            created_at: rfc3339(None),
            usage: Usage::default(),
            open: BTreeMap::new(),
            tool_count: 0,
        }
    }

    fn line(&self, message: Value, done: Option<(&StopReason, &Usage)>) -> String {
        let mut v =
            json!({"model": self.model, "created_at": self.created_at, "message": message, "done": done.is_some()});
        if let Some((reason, usage)) = done {
            v["done_reason"] = json!(done_reason(reason));
            counts(&mut v, usage);
        }
        format!("{v}\n")
    }

    fn close_tool(&mut self, id: String, name: String, args: String) -> ProtocolResult<String> {
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&args)
                .map_err(|e| invalid_resp(format!("tool call `{name}` arguments are not valid JSON: {e}")))?
        };
        let idx = self.tool_count;
        self.tool_count += 1;
        Ok(self.line(
            json!({"role": "assistant", "content": "", "tool_calls": [{"id": id, "function": {"index": idx, "name": name, "arguments": input}}]}),
            None,
        ))
    }

    pub fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<String>> {
        if self.finished {
            return Err(invalid_resp("stream event received after the stream ended"));
        }
        if let StreamEvent::Error { kind, message } = &event {
            self.finished = true;
            return Ok(vec![error_line(&ApiError::new(*kind, message.clone()))]);
        }
        if !self.started {
            let StreamEvent::MessageStart { model, usage, .. } = event else {
                return Err(invalid_resp("first stream event must be message_start"));
            };
            self.started = true;
            self.model = model;
            if let Some(u) = usage {
                self.usage.merge(&u);
            }
            return Ok(vec![]);
        }
        match event {
            StreamEvent::MessageStart { .. } => Err(invalid_resp("duplicate message_start")),
            StreamEvent::ContentBlockStart { index, block } => {
                if self.open.contains_key(&index) {
                    return Err(invalid_resp(format!("content block index {index} is already open")));
                }
                self.open.insert(
                    index,
                    match block {
                        BlockStart::Text => OpenBlock::Text,
                        BlockStart::Thinking => OpenBlock::Thinking,
                        BlockStart::ToolUse { id, name } => OpenBlock::Tool { id, name, args: String::new() },
                    },
                );
                Ok(vec![])
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                let kind = self
                    .open
                    .get(&index)
                    .ok_or_else(|| invalid_resp(format!("content_block_delta for block {index} which is not open")))?;
                match (delta, kind) {
                    (BlockDelta::Text { text }, OpenBlock::Text) => {
                        Ok(vec![self.line(json!({"role": "assistant", "content": text}), None)])
                    }
                    (BlockDelta::Thinking { text }, OpenBlock::Thinking) => {
                        Ok(vec![self.line(json!({"role": "assistant", "content": "", "thinking": text}), None)])
                    }
                    // Ollama 没有思考签名字段。
                    (BlockDelta::Signature { .. }, OpenBlock::Thinking) => Ok(vec![]),
                    _ => Err(invalid_resp(format!("delta type does not match the type of block {index}"))),
                }
            }
            StreamEvent::ToolCallDelta { index, partial_json } => match self.open.get_mut(&index) {
                Some(OpenBlock::Tool { args, .. }) => {
                    args.push_str(&partial_json);
                    Ok(vec![])
                }
                _ => Err(invalid_resp(format!("tool call delta for block {index} which is not an open tool call"))),
            },
            StreamEvent::ContentBlockStop { index } => match self.open.remove(&index) {
                None => Err(invalid_resp(format!("content_block_stop for block {index} which is not open"))),
                Some(OpenBlock::Tool { id, name, args }) => Ok(vec![self.close_tool(id, name, args)?]),
                Some(_) => Ok(vec![]),
            },
            StreamEvent::Usage(u) => {
                self.usage.merge(&u);
                Ok(vec![])
            }
            StreamEvent::MessageStop { stop_reason, .. } => {
                self.finished = true;
                let mut lines = Vec::new();
                for (_, b) in std::mem::take(&mut self.open) {
                    if let OpenBlock::Tool { id, name, args } = b {
                        lines.push(self.close_tool(id, name, args)?);
                    }
                }
                let usage = self.usage.clone();
                lines.push(self.line(json!({"role": "assistant", "content": ""}), Some((&stop_reason, &usage))));
                Ok(lines)
            }
            StreamEvent::Error { .. } => unreachable!("handled above"),
        }
    }
}

// ───────────────────────── 流式解码（上游 NDJSON → canonical 事件） ─────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum OpenKind {
    Text,
    Thinking,
}

/// Ollama 上游 NDJSON → [`StreamEvent`]。`finish()` 必须在上游 EOF 后调用。
pub struct OllamaStreamDecoder {
    parser: NdjsonParser,
    started: bool,
    finished: bool,
    next_index: u32,
    open: Option<(u32, OpenKind)>,
    saw_tool: bool,
}

impl Default for OllamaStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl OllamaStreamDecoder {
    pub fn new() -> Self {
        Self {
            parser: NdjsonParser::default(),
            started: false,
            finished: false,
            next_index: 0,
            open: None,
            saw_tool: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        let values = self.parser.push(bytes)?;
        let mut out = Vec::new();
        for v in values {
            self.handle(&v, &mut out)?;
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        let values = self.parser.finish()?;
        let mut out = Vec::new();
        for v in values {
            self.handle(&v, &mut out)?;
        }
        if !self.finished {
            if !self.started {
                return Err(invalid_resp("upstream stream ended without any data"));
            }
            self.close_open(&mut out);
            self.finished = true;
            out.push(StreamEvent::Error {
                kind: ApiErrorKind::BadGateway,
                message: "upstream stream ended before `done`".into(),
            });
        }
        Ok(out)
    }

    fn close_open(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((index, _)) = self.open.take() {
            out.push(StreamEvent::ContentBlockStop { index });
        }
    }

    fn emit_text(&mut self, kind: OpenKind, text: &str, out: &mut Vec<StreamEvent>) {
        if text.is_empty() {
            return;
        }
        let index = match self.open {
            Some((i, k)) if k == kind => i,
            _ => {
                self.close_open(out);
                let i = self.next_index;
                self.next_index += 1;
                out.push(StreamEvent::ContentBlockStart {
                    index: i,
                    block: if kind == OpenKind::Thinking { BlockStart::Thinking } else { BlockStart::Text },
                });
                self.open = Some((i, kind));
                i
            }
        };
        let delta = if kind == OpenKind::Thinking {
            BlockDelta::Thinking { text: text.to_string() }
        } else {
            BlockDelta::Text { text: text.to_string() }
        };
        out.push(StreamEvent::ContentBlockDelta { index, delta });
    }

    fn handle(&mut self, v: &Value, out: &mut Vec<StreamEvent>) -> ProtocolResult<()> {
        if self.finished {
            return Ok(());
        }
        let obj = v.as_object().ok_or_else(|| invalid_resp("Ollama stream line must be an object"))?;
        if let Some(e) = obj.get("error") {
            self.close_open(out);
            self.finished = true;
            out.push(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message: error_text(e) });
            return Ok(());
        }
        if !self.started {
            self.started = true;
            out.push(StreamEvent::MessageStart {
                id: format!("ollama-{}", uuid::Uuid::new_v4().simple()),
                model: opt_str(obj, "model").unwrap_or_default(),
                usage: None,
            });
        }
        if let Some(msg) = obj.get("message").and_then(Value::as_object) {
            if let Some(t) = msg.get("thinking").and_then(Value::as_str) {
                self.emit_text(OpenKind::Thinking, t, out);
            }
            if let Some(t) = msg.get("content").and_then(Value::as_str) {
                self.emit_text(OpenKind::Text, t, out);
            }
            for (i, c) in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
                let (id, name, input) = decode_call(c, &format!("tool_calls[{i}]"))?;
                self.close_open(out);
                let index = self.next_index;
                self.next_index += 1;
                self.saw_tool = true;
                out.push(StreamEvent::ContentBlockStart { index, block: BlockStart::ToolUse { id, name } });
                out.push(StreamEvent::ToolCallDelta { index, partial_json: input.to_string() });
                out.push(StreamEvent::ContentBlockStop { index });
            }
        }
        if obj.get("done").and_then(Value::as_bool) == Some(true) {
            self.close_open(out);
            let usage = decode_usage(obj);
            if !usage.is_empty() {
                out.push(StreamEvent::Usage(usage));
            }
            out.push(StreamEvent::MessageStop {
                stop_reason: decode_stop(obj.get("done_reason").and_then(Value::as_str), self.saw_tool),
                stop_sequence: None,
            });
            self.finished = true;
        }
        Ok(())
    }
}

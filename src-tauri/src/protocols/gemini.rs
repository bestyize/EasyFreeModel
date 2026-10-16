//! Gemini（`generateContent` / `streamGenerateContent`）编解码；Vertex AI 的差异见 [`super::vertex`]。
//!
//! # 入口侧（我们对外暴露 Gemini 协议）
//! - [`decode_request`] / [`decode_request_for`]：请求 → canonical。Gemini 的模型名与是否流式来自 URL，
//!   不在 body 里，因此用 `decode_request_for(model, stream, body)`；`decode_request(body)` 留空由调用方补。
//! - [`encode_response`]：canonical → `GenerateContentResponse`；[`GeminiStreamEncoder`]：canonical 事件 → `?alt=sse` 帧。
//! - [`encode_error`]：Gemini 风格错误体。
//!
//! # 上游侧（我们调用 Gemini）
//! - [`encode_request`]（AI Studio 方言）/ [`encode_request_with`]（指定 [`Dialect`]，Vertex 用）；
//!   路径用 [`request_path`]。
//! - [`decode_response`] / [`decode_response_mapped`]、[`GeminiStreamDecoder`]（SSE 或 JSON 数组流，处理半包）、
//!   [`decode_error`]。
//!
//! # 约定与取舍（均有测试）
//! - 键名同时接受 camelCase 与 snake_case（官方 REST 与部分 SDK 都会发 `inline_data`、`system_instruction`）。
//! - 角色：`user` / `model`；`functionResponse` 位于 `user` 轮，canonical 里表示为 `ToolResult`。
//! - **用量**：`input_tokens = promptTokenCount + toolUsePromptTokenCount`（已含缓存），
//!   `cache_read_tokens = cachedContentTokenCount`；
//!   `output_tokens = candidatesTokenCount + thoughtsTokenCount`（Gemini 的候选 token 不含思考 token，这里折算为含推理）。
//!   编码回 Gemini 时无法再拆出思考 token，`candidatesTokenCount` 即 `output_tokens`，`thoughtsTokenCount` 省略。
//! - **思考签名**：`thoughtSignature` 以 `gemini:` 前缀存入 `Thinking.signature`；
//!   「文本为空、仅含签名」的 `Thinking` 附着在紧随其后的 Text/ToolUse 上。编码时只接受 `gemini:` 前缀的签名，
//!   其它来源的签名丢弃并告警；思考文本本身不回传（官方也不要求回传摘要）。
//! - **工具名**：Gemini 只接受 `[A-Za-z_][A-Za-z0-9_.:-]{0,63}`，其它字符会被替换；映射关系由 [`ToolNameMap`] 带回，
//!   解码上游响应时请使用 `*_mapped` / [`GeminiStreamDecoder::with_name_map`] 还原原名。
//! - **functionCall id**：Gemini 要求整个历史内 id 唯一，而 OpenAI 的 tool_call_id 只在单轮内唯一；编码时对重复 id 追加后缀，
//!   functionResponse 按出现顺序与 functionCall 配对。没有 id 的 functionCall 在请求解码时按序生成确定性 id（`call_N`），
//!   保证历史前缀稳定（利于上游缓存）。
//! - **工具 schema**：统一经 [`super::schema::sanitize_for_gemini`] 清理，其告警带上工具名传出。
//! - `safetySettings` 原样保存在 `CanonicalRequest::gemini_safety_settings`，只会发给 Gemini/Vertex，同时产生 warning。
//! - 内置工具（googleSearch、codeExecution、urlContext ...）、`responseSchema`、`candidateCount > 1` 之外的
//!   生成配置等无法表达的内容进入 warnings 或返回 `Unsupported`，不会静默消失。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde_json::{json, Map, Value};

use super::canonical::*;
use super::common::*;
use super::schema::sanitize_for_gemini;
use super::wire::{as_u32, JsonArrayStream, SseParser};
use super::{ApiError, ApiErrorKind, Encoded, ProtocolError, ProtocolKind, ProtocolResult, SseFrame, StreamEncoder};

/// Gemini 3 在历史里缺少签名的 functionCall 上接受的占位签名（官方文档给出的跳过校验值）。
pub const DUMMY_THOUGHT_SIGNATURE: &str = "skip_thought_signature_validator";

/// 编码方言：AI Studio（generativelanguage）与 Vertex AI 的请求体差异开关。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialect {
    /// 用于告警文案。
    pub name: &'static str,
    /// functionCall / functionResponse 是否带 `id`（Vertex 会拒绝未知字段 `id`）。
    pub send_call_ids: bool,
    /// `fileData.mimeType` 是否必填（Vertex 必填）。
    pub require_file_mime: bool,
    /// 为缺少签名的首个 functionCall 补占位签名（Gemini 3 历史里混入其它模型产出的工具调用时需要）。
    pub dummy_thought_signature: bool,
}

impl Dialect {
    pub const AI_STUDIO: Dialect =
        Dialect { name: "gemini", send_call_ids: true, require_file_mime: false, dummy_thought_signature: false };
}

// ───────────────────────── 通用小工具 ─────────────────────────

/// 取字段，同时接受 camelCase 与 snake_case；null 视为不存在。
fn g<'a>(o: &'a Map<String, Value>, camel: &str) -> Option<&'a Value> {
    o.get(camel).or_else(|| o.get(&to_snake(camel))).filter(|v| !v.is_null())
}

fn to_snake(camel: &str) -> String {
    let mut out = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut up = false;
    for c in snake.chars() {
        if c == '_' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn warn_unknown(obj: &Map<String, Value>, known: &[&str], prefix: &str, warnings: &mut Vec<String>) {
    let mut unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !known.contains(k) && !known.contains(&to_camel(k).as_str()))
        .collect();
    unknown.sort_unstable();
    for k in unknown {
        warnings.push(format!("unsupported field ignored: {prefix}{k}"));
    }
}

fn g_str(o: &Map<String, Value>, camel: &str) -> Option<String> {
    g(o, camel).and_then(Value::as_str).map(str::to_string)
}

fn strip_models_prefix(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// 构造请求路径（不含 `/v1beta/` 前缀与鉴权）：`models/<m>:generateContent` 或带 `?alt=sse` 的流式路径。
pub fn request_path(model: &str, stream: bool) -> String {
    let m = if model.contains('/') { model.to_string() } else { format!("models/{model}") };
    if stream {
        format!("{m}:streamGenerateContent?alt=sse")
    } else {
        format!("{m}:generateContent")
    }
}

fn guess_mime(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    let ext = path.rsplit('.').next()?;
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => return None,
    })
}

// ───────────────────────── 工具名映射 ─────────────────────────

/// 原始工具名 ↔ Gemini 合法名称。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolNameMap {
    to_wire: HashMap<String, String>,
    from_wire: HashMap<String, String>,
}

impl ToolNameMap {
    /// Register a reversible provider alias without applying Gemini's sanitization.
    pub(crate) fn insert_alias(&mut self, original: &str, wire: &str) {
        self.to_wire.insert(original.to_string(), wire.to_string());
        self.from_wire.insert(wire.to_string(), original.to_string());
    }
    pub fn is_identity(&self) -> bool {
        self.to_wire.iter().all(|(k, v)| k == v)
    }

    /// Gemini 返回的名称 → 原始名称（未登记的原样返回）。
    pub fn original(&self, wire: &str) -> String {
        self.from_wire.get(wire).cloned().unwrap_or_else(|| wire.to_string())
    }

    fn sanitize(name: &str) -> String {
        let mut s: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') { c } else { '_' })
            .collect();
        if !s.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            s.insert(0, '_');
        }
        s.truncate(64);
        s
    }

    fn wire(&mut self, original: &str) -> String {
        if let Some(w) = self.to_wire.get(original) {
            return w.clone();
        }
        let base = Self::sanitize(original);
        let mut candidate = base.clone();
        let mut n = 2;
        while self.from_wire.get(&candidate).is_some_and(|o| o != original) {
            let suffix = format!("_{n}");
            let mut b = base.clone();
            b.truncate(64 - suffix.len());
            candidate = format!("{b}{suffix}");
            n += 1;
        }
        self.to_wire.insert(original.to_string(), candidate.clone());
        self.from_wire.insert(candidate.clone(), original.to_string());
        candidate
    }
}

// ───────────────────────── schema 转换（Gemini → JSON Schema） ─────────────────────────

const NUMERIC_KEYS: &[&str] = &["minItems", "maxItems", "minLength", "maxLength", "minProperties", "maxProperties"];

/// Gemini 的 OpenAPI 风格 schema → 标准 JSON Schema：类型小写、`nullable` 改写成 `type: [t, "null"]`、
/// protobuf 序列化成字符串的整数还原为数字。
fn schema_from_gemini(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut out = Map::new();
            let mut nullable = false;
            for (k, val) in o {
                match k.as_str() {
                    "nullable" => nullable = val.as_bool().unwrap_or(false),
                    "propertyOrdering" => {}
                    "type" => {
                        if let Some(t) = val.as_str() {
                            if t != "TYPE_UNSPECIFIED" {
                                out.insert(k.clone(), json!(t.to_ascii_lowercase()));
                            }
                        } else {
                            out.insert(k.clone(), val.clone());
                        }
                    }
                    "properties" => {
                        if let Value::Object(p) = val {
                            out.insert(
                                k.clone(),
                                Value::Object(p.iter().map(|(pk, pv)| (pk.clone(), schema_from_gemini(pv))).collect()),
                            );
                        } else {
                            out.insert(k.clone(), val.clone());
                        }
                    }
                    "items" => {
                        out.insert(k.clone(), schema_from_gemini(val));
                    }
                    "anyOf" | "oneOf" | "allOf" => {
                        if let Value::Array(a) = val {
                            out.insert(k.clone(), Value::Array(a.iter().map(schema_from_gemini).collect()));
                        } else {
                            out.insert(k.clone(), val.clone());
                        }
                    }
                    key if NUMERIC_KEYS.contains(&key) => {
                        let n = as_u32(val).map(|n| json!(n)).unwrap_or_else(|| val.clone());
                        out.insert(k.clone(), n);
                    }
                    _ => {
                        out.insert(k.clone(), val.clone());
                    }
                }
            }
            if nullable {
                match out.get("type").cloned() {
                    Some(Value::String(t)) => {
                        out.insert("type".into(), json!([t, "null"]));
                    }
                    Some(Value::Array(mut a)) => {
                        if !a.iter().any(|x| x == "null") {
                            a.push(json!("null"));
                        }
                        out.insert("type".into(), Value::Array(a));
                    }
                    _ => {}
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// ───────────────────────── 请求解码 ─────────────────────────

const KNOWN_TOP: &[&str] =
    &["model", "contents", "systemInstruction", "generationConfig", "tools", "toolConfig", "safetySettings"];
const KNOWN_GEN: &[&str] = &[
    "temperature",
    "topP",
    "topK",
    "maxOutputTokens",
    "stopSequences",
    "presencePenalty",
    "frequencyPenalty",
    "seed",
    "candidateCount",
    "thinkingConfig",
];
const KNOWN_PART: &[&str] =
    &["text", "thought", "thoughtSignature", "inlineData", "fileData", "functionCall", "functionResponse"];

struct PendingCall {
    id: String,
    name: String,
    resolved: bool,
}

#[derive(Default)]
struct DecodeCtx {
    calls: Vec<PendingCall>,
    counter: usize,
}

/// 请求体 → canonical（`model` 为空、`stream=false`，由调用方根据 URL 补全）。
pub fn decode_request(body: &Value) -> ProtocolResult<CanonicalRequest> {
    decode_request_for("", false, body)
}

/// `model` 与 `stream` 来自 URL（`models/<model>:streamGenerateContent`）。
pub fn decode_request_for(model: &str, stream: bool, body: &Value) -> ProtocolResult<CanonicalRequest> {
    let obj = as_object(body, "request body")?;
    let mut req = CanonicalRequest { model: strip_models_prefix(model).to_string(), stream, ..Default::default() };
    if req.model.is_empty() {
        if let Some(m) = obj.get("model").and_then(Value::as_str) {
            req.model = strip_models_prefix(m).to_string();
        }
    }

    let contents = match g(obj, "contents") {
        Some(Value::Array(a)) => a.clone(),
        Some(v @ Value::Object(_)) => vec![v.clone()],
        _ => return Err(invalid("request.contents is required and must be an array")),
    };
    if contents.is_empty() {
        return Err(invalid("request.contents must not be empty"));
    }
    let mut ctx = DecodeCtx::default();
    for (i, c) in contents.iter().enumerate() {
        let what = format!("contents[{i}]");
        let co = as_object(c, &what)?;
        let role = match co.get("role").and_then(Value::as_str) {
            None | Some("user") | Some("function") => Role::User,
            Some("model") => Role::Assistant,
            Some(other) => return Err(invalid(format!("{what}.role `{other}` must be `user` or `model`"))),
        };
        let parts = g(co, "parts")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(format!("{what}.parts is required and must be an array")))?;
        let mut content = Vec::new();
        for (j, p) in parts.iter().enumerate() {
            decode_part(p, role, &format!("{what}.parts[{j}]"), &mut ctx, &mut content, &mut req.warnings)?;
        }
        req.messages.push(Message { role, content });
    }

    if let Some(si) = g(obj, "systemInstruction") {
        match si {
            Value::String(s) => req.system.push(s.clone()),
            Value::Object(so) => {
                let parts = g(so, "parts").and_then(Value::as_array).cloned().unwrap_or_default();
                for p in &parts {
                    match p.as_object().and_then(|po| po.get("text")).and_then(Value::as_str) {
                        Some(t) => req.system.push(t.to_string()),
                        None => req.warnings.push("systemInstruction: non-text part ignored".into()),
                    }
                }
            }
            _ => return Err(invalid("request.systemInstruction must be a Content object")),
        }
    }

    if let Some(gc) = g(obj, "generationConfig") {
        decode_generation_config(as_object(gc, "generationConfig")?, &mut req)?;
    }

    if let Some(tools) = g(obj, "tools") {
        decode_tools(tools, &mut req)?;
    }
    if let Some(tc) = g(obj, "toolConfig") {
        decode_tool_config(as_object(tc, "toolConfig")?, &mut req)?;
    }
    if let Some(ss) = g(obj, "safetySettings") {
        req.warnings.push(
            "safetySettings are only forwarded to Gemini/Vertex upstreams; other protocols cannot express them".into(),
        );
        req.gemini_safety_settings = Some(ss.clone());
    }

    warn_unknown(obj, KNOWN_TOP, "", &mut req.warnings);
    Ok(req)
}

fn decode_inline(o: &Map<String, Value>, what: &str, warnings: &mut Vec<String>) -> ProtocolResult<ContentPart> {
    let mime = required_str(o, "mimeType", what).or_else(|_| required_str(o, "mime_type", what))?;
    let data = required_str(o, "data", what)?;
    if !mime.starts_with("image/") {
        warnings.push(format!("{what}: non-image inlineData ({mime}) is carried in a binary part"));
    }
    Ok(ContentPart::Image {
        source: ImageSource::Base64 { media_type: mime.to_string(), data: data.to_string() },
        detail: None,
    })
}

fn response_to_text(resp: &Value) -> (String, bool) {
    if let Some(o) = resp.as_object() {
        if o.len() == 1 {
            let (k, v) = o.iter().next().expect("len checked");
            match (k.as_str(), v) {
                ("output" | "result" | "content", Value::String(s)) => return (s.clone(), false),
                ("error", Value::String(s)) => return (s.clone(), true),
                _ => {}
            }
        }
    }
    match resp {
        Value::String(s) => (s.clone(), false),
        other => (other.to_string(), false),
    }
}

fn decode_part(
    p: &Value,
    role: Role,
    what: &str,
    ctx: &mut DecodeCtx,
    out: &mut Vec<ContentPart>,
    warnings: &mut Vec<String>,
) -> ProtocolResult<()> {
    let o = as_object(p, what)?;
    // 无法表达的 part 字段（executableCode / codeExecutionResult / videoMetadata ...）：只记录字段名。
    let mut extra: Vec<&str> = o
        .keys()
        .map(String::as_str)
        .filter(|k| !KNOWN_PART.contains(k) && !KNOWN_PART.contains(&to_camel(k).as_str()))
        .collect();
    extra.sort_unstable();
    if !extra.is_empty() {
        warnings.push(format!("{what}: unsupported part ignored (fields: {})", extra.join(", ")));
    }
    let sig = g_str(o, "thoughtSignature").filter(|s| !s.is_empty()).map(|s| signature::tag(signature::GEMINI, &s));
    let is_thought = g(o, "thought").and_then(Value::as_bool).unwrap_or(false);

    if let Some(text) = o.get("text").and_then(Value::as_str) {
        if is_thought {
            out.push(ContentPart::Thinking { text: text.to_string(), signature: sig });
        } else {
            if let Some(s) = sig {
                out.push(ContentPart::Thinking { text: String::new(), signature: Some(s) });
            }
            if !text.is_empty() {
                out.push(ContentPart::text(text));
            }
        }
        return Ok(());
    }
    if let Some(s) = sig.clone() {
        if !is_thought {
            out.push(ContentPart::Thinking { text: String::new(), signature: Some(s) });
        }
    }
    if let Some(inline) = g(o, "inlineData") {
        out.push(decode_inline(as_object(inline, what)?, what, warnings)?);
    } else if let Some(fd) = g(o, "fileData") {
        let fo = as_object(fd, what)?;
        let uri = g_str(fo, "fileUri").ok_or_else(|| invalid(format!("{what}.fileData.fileUri is required")))?;
        if let Some(m) = g_str(fo, "mimeType") {
            if guess_mime(&uri) != Some(m.as_str()) {
                warnings.push(format!("{what}: fileData.mimeType `{m}` cannot be preserved in the canonical model"));
            }
        }
        out.push(ContentPart::Image { source: ImageSource::Url { url: uri }, detail: None });
    } else if let Some(fc) = g(o, "functionCall") {
        let fo = as_object(fc, what)?;
        let name = required_str(fo, "name", &format!("{what}.functionCall"))?.to_string();
        let id = g_str(fo, "id").filter(|s| !s.is_empty()).unwrap_or_else(|| format!("call_{}", ctx.counter));
        ctx.counter += 1;
        ctx.calls.push(PendingCall { id: id.clone(), name: name.clone(), resolved: false });
        out.push(ContentPart::ToolUse { id, name, input: g(fo, "args").cloned().unwrap_or_else(|| json!({})) });
    } else if let Some(fr) = g(o, "functionResponse") {
        let fo = as_object(fr, what)?;
        let name = required_str(fo, "name", &format!("{what}.functionResponse"))?.to_string();
        let explicit = g_str(fo, "id").filter(|s| !s.is_empty());
        let id = match explicit {
            Some(id) => {
                if let Some(c) = ctx.calls.iter_mut().find(|c| c.id == id) {
                    c.resolved = true;
                }
                id
            }
            None => match ctx.calls.iter_mut().find(|c| !c.resolved && c.name == name) {
                Some(c) => {
                    c.resolved = true;
                    c.id.clone()
                }
                None => {
                    warnings.push(format!("{what}: functionResponse `{name}` has no matching functionCall"));
                    let id = format!("call_{}", ctx.counter);
                    ctx.counter += 1;
                    id
                }
            },
        };
        let (text, is_error) = response_to_text(g(fo, "response").unwrap_or(&Value::Null));
        let mut content = vec![ContentPart::text(text)];
        if let Some(extra) = g(fo, "parts").and_then(Value::as_array) {
            for ep in extra {
                match ep.as_object().and_then(|e| g(e, "inlineData")).and_then(Value::as_object) {
                    Some(inline) => content.push(decode_inline(inline, what, warnings)?),
                    None => warnings.push(format!("{what}: unsupported functionResponse.parts entry ignored")),
                }
            }
        }
        if role == Role::Assistant {
            warnings.push(format!("{what}: functionResponse inside a model turn"));
        }
        out.push(ContentPart::ToolResult { tool_use_id: id, content, is_error });
    }
    Ok(())
}

fn decode_generation_config(gc: &Map<String, Value>, req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let num = |k: &str| -> ProtocolResult<Option<f64>> {
        match g(gc, k) {
            None => Ok(None),
            Some(v) => v.as_f64().map(Some).ok_or_else(|| invalid(format!("generationConfig.{k} must be a number"))),
        }
    };
    let uint = |k: &str| -> ProtocolResult<Option<u32>> {
        match g(gc, k) {
            None => Ok(None),
            Some(v) => as_u32(v)
                .map(Some)
                .ok_or_else(|| invalid(format!("generationConfig.{k} must be a non-negative integer"))),
        }
    };
    if let Some(n) = uint("candidateCount")? {
        if n != 1 {
            return Err(ProtocolError::Unsupported(
                "generationConfig.candidateCount != 1 (multiple candidates) is not supported".into(),
            ));
        }
    }
    req.params = GenerationParams {
        max_tokens: uint("maxOutputTokens")?,
        temperature: num("temperature")?,
        top_p: num("topP")?,
        top_k: uint("topK")?,
        stop: match g(gc, "stopSequences") {
            None => vec![],
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| invalid("generationConfig.stopSequences must contain only strings"))
                })
                .collect::<ProtocolResult<_>>()?,
            Some(_) => return Err(invalid("generationConfig.stopSequences must be an array of strings")),
        },
        presence_penalty: num("presencePenalty")?,
        frequency_penalty: num("frequencyPenalty")?,
        seed: g(gc, "seed").and_then(Value::as_i64),
    };
    if let Some(tc) = g(gc, "thinkingConfig") {
        let to = as_object(tc, "generationConfig.thinkingConfig")?;
        let budget = g(to, "thinkingBudget").and_then(Value::as_i64);
        req.reasoning = Some(ReasoningConfig {
            effort: g_str(to, "thinkingLevel").map(|l| l.to_ascii_lowercase()),
            budget_tokens: budget.filter(|b| *b >= 0).and_then(|b| u32::try_from(b).ok()),
            include_thoughts: g(to, "includeThoughts").and_then(Value::as_bool),
        });
        warn_unknown(
            to,
            &["thinkingBudget", "thinkingLevel", "includeThoughts"],
            "generationConfig.thinkingConfig.",
            &mut req.warnings,
        );
    }
    warn_unknown(gc, KNOWN_GEN, "generationConfig.", &mut req.warnings);
    Ok(())
}

fn decode_tools(v: &Value, req: &mut CanonicalRequest) -> ProtocolResult<()> {
    let arr = v.as_array().ok_or_else(|| invalid("request.tools must be an array"))?;
    for (i, t) in arr.iter().enumerate() {
        let what = format!("tools[{i}]");
        let to = as_object(t, &what)?;
        if let Some(decls) = g(to, "functionDeclarations") {
            let decls =
                decls.as_array().ok_or_else(|| invalid(format!("{what}.functionDeclarations must be an array")))?;
            for (j, d) in decls.iter().enumerate() {
                let dw = format!("{what}.functionDeclarations[{j}]");
                let d = as_object(d, &dw)?;
                let schema =
                    g(d, "parametersJsonSchema").cloned().or_else(|| g(d, "parameters").map(schema_from_gemini));
                req.tools.push(ToolDefinition {
                    name: required_str(d, "name", &dw)?.to_string(),
                    description: opt_str(d, "description"),
                    input_schema: schema.unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                });
                warn_unknown(
                    d,
                    &["name", "description", "parameters", "parametersJsonSchema"],
                    &format!("{dw}."),
                    &mut req.warnings,
                );
            }
        }
        let mut other: Vec<&str> = to
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "functionDeclarations" && *k != "function_declarations")
            .collect();
        other.sort_unstable();
        for k in other {
            req.warnings.push(format!("{what}.{k} is a built-in Gemini tool and is not supported; it was dropped"));
        }
    }
    Ok(())
}

fn decode_tool_config(tc: &Map<String, Value>, req: &mut CanonicalRequest) -> ProtocolResult<()> {
    if let Some(fc) = g(tc, "functionCallingConfig") {
        let fo = as_object(fc, "toolConfig.functionCallingConfig")?;
        let allowed: Vec<String> = g(fo, "allowedFunctionNames")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let mode = g_str(fo, "mode").unwrap_or_else(|| "AUTO".into());
        req.tool_choice = Some(match mode.as_str() {
            "AUTO" | "MODE_UNSPECIFIED" => ToolChoice::Auto,
            "NONE" => ToolChoice::None,
            "ANY" | "VALIDATED" => {
                match allowed.as_slice() {
                    [one] => ToolChoice::Tool { name: one.clone() },
                    [] => ToolChoice::Any,
                    _ => {
                        req.warnings.push("toolConfig.functionCallingConfig.allowedFunctionNames with several names was reduced to `any`".into());
                        ToolChoice::Any
                    }
                }
            }
            other => return Err(invalid(format!("toolConfig.functionCallingConfig.mode `{other}` is not valid"))),
        });
        if mode == "VALIDATED" {
            req.warnings.push("functionCallingConfig.mode VALIDATED was treated as ANY".into());
        }
        warn_unknown(fo, &["mode", "allowedFunctionNames"], "toolConfig.functionCallingConfig.", &mut req.warnings);
    }
    warn_unknown(tc, &["functionCallingConfig"], "toolConfig.", &mut req.warnings);
    Ok(())
}

// ───────────────────────── 请求编码（发往 Gemini 上游） ─────────────────────────

/// canonical → AI Studio 方言的 `generateContent` 请求体。工具名被改写时会产生 warning；
/// 需要还原响应里的工具名请改用 [`encode_request_with`]。
pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let (enc, map) = encode_request_with(req, &Dialect::AI_STUDIO)?;
    let mut enc = enc;
    if !map.is_identity() {
        enc.warnings
            .push("tool names were rewritten for Gemini; use encode_request_with to obtain the ToolNameMap".into());
    }
    Ok(enc)
}

fn image_part(source: &ImageSource, d: &Dialect, warnings: &mut Vec<String>) -> Option<Value> {
    match source {
        ImageSource::Base64 { media_type, data } => Some(json!({"inlineData": {"mimeType": media_type, "data": data}})),
        ImageSource::Url { url } => {
            let mut fd = Map::new();
            fd.insert("fileUri".into(), json!(url));
            match guess_mime(url) {
                Some(m) => {
                    fd.insert("mimeType".into(), json!(m));
                }
                None if d.require_file_mime => {
                    warnings.push(format!(
                        "{}: fileData.mimeType is required; guessed image/jpeg for a URL without a known extension",
                        d.name
                    ));
                    fd.insert("mimeType".into(), json!("image/jpeg"));
                }
                None => {}
            }
            Some(json!({"fileData": fd}))
        }
        ImageSource::FileId { .. } => {
            warnings.push(format!("image referenced by file_id cannot be sent to {} and was dropped", d.name));
            None
        }
    }
}

fn function_response_object(text: &str, is_error: bool) -> Value {
    if is_error {
        return json!({"error": text});
    }
    if text.trim_start().starts_with('{') {
        if let Ok(v @ Value::Object(_)) = serde_json::from_str::<Value>(text) {
            return v;
        }
    }
    json!({"output": text})
}

struct EmittedCall {
    id: String,
    name: String,
    seq: usize,
}

/// 编码并返回工具名映射。
pub fn encode_request_with(req: &CanonicalRequest, d: &Dialect) -> ProtocolResult<(Encoded, ToolNameMap)> {
    let mut warnings: Vec<String> = Vec::new();
    let mut names = ToolNameMap::default();

    // 先登记工具定义里的名字，让冲突消解优先照顾定义。
    let mut declarations: Vec<Value> = Vec::new();
    for t in &req.tools {
        let wire = names.wire(&t.name);
        let san = sanitize_for_gemini(&t.input_schema);
        for w in &san.warnings {
            warnings.push(format!("tool `{}` schema: {w}", t.name));
        }
        let mut decl = Map::new();
        decl.insert("name".into(), json!(wire));
        decl.insert("description".into(), json!(t.description.clone().unwrap_or_default()));
        if let Some(s) = san.schema {
            decl.insert("parameters".into(), s);
        }
        declarations.push(Value::Object(decl));
    }

    // 历史：functionCall id 去重 + functionResponse 配对。
    let mut used_ids: HashSet<String> = HashSet::new();
    let mut queues: HashMap<String, VecDeque<EmittedCall>> = HashMap::new();
    let mut seq = 0usize;
    let mut warned_thinking = false;

    let mut contents: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in &req.messages {
        let (role, parts) = match m.role {
            Role::Assistant => {
                let mut parts: Vec<Value> = Vec::new();
                let mut pending_sig: Option<String> = None;
                let mut first_call_in_turn = true;
                for p in &m.content {
                    match p {
                        ContentPart::Thinking { text, signature } => {
                            match signature.as_deref().and_then(|s| signature::untag(signature::GEMINI, s)) {
                                Some(raw) => pending_sig = Some(raw.to_string()),
                                None => {
                                    if (signature.is_some() || !text.is_empty()) && !warned_thinking {
                                        warnings.push("assistant thinking content / foreign thought signatures were not sent to Gemini".into());
                                        warned_thinking = true;
                                    }
                                }
                            }
                        }
                        ContentPart::Text { text } => {
                            if text.is_empty() {
                                continue;
                            }
                            let mut part = json!({"text": text});
                            if let Some(s) = pending_sig.take() {
                                part["thoughtSignature"] = json!(s);
                            }
                            parts.push(part);
                        }
                        ContentPart::ToolUse { id, name, input } => {
                            let mut emit = id.clone();
                            let mut n = 2;
                            while !used_ids.insert(emit.clone()) {
                                emit = format!("{id}-{n}");
                                n += 1;
                            }
                            let wire = names.wire(name);
                            queues.entry(id.clone()).or_default().push_back(EmittedCall {
                                id: emit.clone(),
                                name: wire.clone(),
                                seq,
                            });
                            seq += 1;
                            let mut fc = Map::new();
                            if d.send_call_ids {
                                fc.insert("id".into(), json!(emit));
                            }
                            fc.insert("name".into(), json!(wire));
                            fc.insert("args".into(), if input.is_object() { input.clone() } else { json!({}) });
                            let mut part = json!({"functionCall": fc});
                            let sig = pending_sig.take().or_else(|| {
                                (first_call_in_turn && d.dummy_thought_signature)
                                    .then(|| DUMMY_THOUGHT_SIGNATURE.to_string())
                            });
                            if let Some(s) = sig {
                                part["thoughtSignature"] = json!(s);
                            }
                            first_call_in_turn = false;
                            parts.push(part);
                        }
                        ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {
                            warnings.push("image/tool_result part inside an assistant message was dropped".into());
                        }
                    }
                }
                if let Some(s) = pending_sig.take() {
                    parts.push(json!({"text": "", "thoughtSignature": s}));
                }
                ("model", parts)
            }
            Role::User => {
                let mut results: Vec<(usize, Value)> = Vec::new();
                let mut rest: Vec<Value> = Vec::new();
                let mut extra_images: Vec<Value> = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::Text { text } => {
                            if !text.is_empty() {
                                rest.push(json!({"text": text}));
                            }
                        }
                        ContentPart::Image { source, .. } => {
                            if let Some(v) = image_part(source, d, &mut warnings) {
                                rest.push(v);
                            }
                        }
                        ContentPart::ToolResult { tool_use_id, content, is_error } => {
                            let call = queues.get_mut(tool_use_id).and_then(VecDeque::pop_front);
                            let (emit_id, name, order) = match call {
                                Some(c) => (c.id, c.name, c.seq),
                                None => {
                                    warnings.push(format!("tool_result `{tool_use_id}` has no matching tool_use; sent with a placeholder name"));
                                    (tool_use_id.clone(), names.wire(tool_use_id), usize::MAX)
                                }
                            };
                            let text = content
                                .iter()
                                .filter_map(
                                    |c| if let ContentPart::Text { text } = c { Some(text.as_str()) } else { None },
                                )
                                .collect::<Vec<_>>()
                                .join("\n");
                            for c in content {
                                if let ContentPart::Image { source, .. } = c {
                                    if let Some(v) = image_part(source, d, &mut warnings) {
                                        extra_images.push(v);
                                    }
                                }
                            }
                            let mut fr = Map::new();
                            if d.send_call_ids {
                                fr.insert("id".into(), json!(emit_id));
                            }
                            fr.insert("name".into(), json!(name));
                            fr.insert("response".into(), function_response_object(&text, *is_error));
                            results.push((order, json!({"functionResponse": fr})));
                        }
                        ContentPart::ToolUse { .. } | ContentPart::Thinking { .. } => {
                            warnings.push("tool_use/thinking part inside a user message was dropped".into());
                        }
                    }
                }
                if !extra_images.is_empty() {
                    warnings.push("images inside tool_result were moved next to the functionResponse parts (Gemini function responses are JSON objects)".into());
                }
                results.sort_by_key(|(o, _)| *o);
                let mut parts: Vec<Value> = results.into_iter().map(|(_, v)| v).collect();
                parts.extend(extra_images);
                parts.extend(rest);
                ("user", parts)
            }
        };
        if parts.is_empty() {
            continue;
        }
        match contents.last_mut() {
            Some((r, existing)) if *r == role => existing.extend(parts),
            _ => contents.push((role, parts)),
        }
    }

    // 校验 functionCall / functionResponse 数量（Gemini 要求一一对应）。
    for (i, (role, parts)) in contents.iter().enumerate() {
        if *role != "model" {
            continue;
        }
        let calls = parts.iter().filter(|p| p.get("functionCall").is_some()).count();
        if calls == 0 {
            continue;
        }
        let responses = contents
            .get(i + 1)
            .filter(|(r, _)| *r == "user")
            .map(|(_, ps)| ps.iter().filter(|p| p.get("functionResponse").is_some()).count())
            .unwrap_or(0);
        if responses != calls {
            warnings.push(format!(
                "contents[{i}] has {calls} functionCall part(s) but the following user turn has {responses} functionResponse part(s); Gemini may reject this"
            ));
        }
    }

    let mut body = Map::new();
    body.insert(
        "contents".into(),
        Value::Array(contents.into_iter().map(|(r, parts)| json!({"role": r, "parts": parts})).collect()),
    );
    if !req.system.is_empty() {
        let parts: Vec<Value> = req.system.iter().map(|s| json!({"text": s})).collect();
        body.insert("systemInstruction".into(), json!({"parts": parts}));
    }

    let mut gc = Map::new();
    let p = &req.params;
    if let Some(v) = p.max_tokens {
        gc.insert("maxOutputTokens".into(), json!(v));
    }
    if let Some(v) = p.temperature {
        gc.insert("temperature".into(), json!(v));
    }
    if let Some(v) = p.top_p {
        gc.insert("topP".into(), json!(v));
    }
    if let Some(v) = p.top_k {
        gc.insert("topK".into(), json!(v));
    }
    if !p.stop.is_empty() {
        gc.insert("stopSequences".into(), json!(p.stop));
    }
    if let Some(v) = p.presence_penalty {
        gc.insert("presencePenalty".into(), json!(v));
    }
    if let Some(v) = p.frequency_penalty {
        gc.insert("frequencyPenalty".into(), json!(v));
    }
    if let Some(v) = p.seed {
        gc.insert("seed".into(), json!(v));
    }
    if let Some(r) = &req.reasoning {
        let mut tc = Map::new();
        if let Some(b) = r.budget_tokens.or_else(|| r.effort.as_deref().and_then(effort_to_budget)) {
            tc.insert("thinkingBudget".into(), json!(b));
        }
        if let Some(i) = r.include_thoughts {
            tc.insert("includeThoughts".into(), json!(i));
        }
        if !tc.is_empty() {
            gc.insert("thinkingConfig".into(), Value::Object(tc));
        }
    }
    if !gc.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gc));
    }

    if !declarations.is_empty() {
        body.insert("tools".into(), json!([{"functionDeclarations": declarations}]));
        if let Some(choice) = &req.tool_choice {
            let cfg = match choice {
                ToolChoice::Auto => json!({"mode": "AUTO"}),
                ToolChoice::None => json!({"mode": "NONE"}),
                ToolChoice::Any => json!({"mode": "ANY"}),
                ToolChoice::Tool { name } => json!({"mode": "ANY", "allowedFunctionNames": [names.wire(name)]}),
            };
            body.insert("toolConfig".into(), json!({"functionCallingConfig": cfg}));
        }
        if req.parallel_tool_calls == Some(false) {
            warnings.push("parallel_tool_calls=false cannot be expressed in Gemini and was ignored".into());
        }
    }
    if let Some(ss) = &req.gemini_safety_settings {
        body.insert("safetySettings".into(), ss.clone());
    }
    if req.user.is_some() {
        warnings.push("user identifier is not supported by Gemini and was dropped".into());
    }
    Ok((Encoded { body: Value::Object(body), warnings }, names))
}

/// Responses/Chat 的 effort 档位 → Gemini 2.5 `thinkingBudget`（近似值，需要精确控制请直接给 `budget_tokens`）。
fn effort_to_budget(effort: &str) -> Option<u32> {
    match effort {
        "none" => Some(0),
        "minimal" => Some(512),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" => Some(24576),
        _ => None,
    }
}

// ───────────────────────── 响应编码 / 解码 ─────────────────────────

fn finish_reason_str(r: &StopReason) -> &str {
    match r {
        StopReason::EndTurn | StopReason::StopSequence | StopReason::ToolUse => "STOP",
        StopReason::MaxTokens => "MAX_TOKENS",
        StopReason::ContentFilter => "SAFETY",
        StopReason::Other(s) => match s.as_str() {
            "MALFORMED_FUNCTION_CALL"
            | "UNEXPECTED_TOOL_CALL"
            | "TOO_MANY_TOOL_CALLS"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "LANGUAGE"
            | "OTHER" => s.as_str(),
            _ => "OTHER",
        },
    }
}

fn decode_finish_reason(r: &str, saw_tool: bool) -> StopReason {
    match r {
        "STOP" | "FINISH_REASON_UNSPECIFIED" => {
            if saw_tool {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        }
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY"
        | "RECITATION"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION" => StopReason::ContentFilter,
        other => StopReason::Other(other.to_string()),
    }
}

fn usage_metadata(u: &Usage) -> Value {
    let input = u.input_tokens.unwrap_or(0);
    let output = u.output_tokens.unwrap_or(0);
    let mut v = json!({"promptTokenCount": input, "candidatesTokenCount": output, "totalTokenCount": input + output});
    if let Some(c) = u.cache_read_tokens {
        v["cachedContentTokenCount"] = json!(c);
    }
    v
}

pub fn decode_usage(v: Option<&Value>) -> Usage {
    let Some(o) = v.and_then(Value::as_object) else {
        return Usage::default();
    };
    let get = |k: &str| g(o, k).and_then(as_u32);
    let prompt = get("promptTokenCount");
    let tool_prompt = get("toolUsePromptTokenCount");
    let (cand, thoughts) = (get("candidatesTokenCount"), get("thoughtsTokenCount"));
    Usage {
        input_tokens: match (prompt, tool_prompt) {
            (None, None) => None,
            (p, t) => Some(p.unwrap_or(0) + t.unwrap_or(0)),
        },
        output_tokens: match (cand, thoughts) {
            (None, None) => None,
            (c, t) => Some(c.unwrap_or(0) + t.unwrap_or(0)),
        },
        cache_read_tokens: get("cachedContentTokenCount"),
        cache_creation_tokens: None,
    }
}

/// canonical 助手内容 → Gemini parts（签名附着规则见模块文档）。
fn response_parts(content: &[ContentPart]) -> Vec<Value> {
    let mut parts = Vec::new();
    let mut pending: Option<String> = None;
    for p in content {
        match p {
            ContentPart::Thinking { text, signature } => {
                let sig = signature.as_deref().and_then(|s| signature::untag(signature::GEMINI, s)).map(str::to_string);
                if !text.is_empty() {
                    let mut part = json!({"text": text, "thought": true});
                    if let Some(s) = sig {
                        part["thoughtSignature"] = json!(s);
                    }
                    parts.push(part);
                } else if sig.is_some() {
                    pending = sig;
                }
            }
            ContentPart::Text { text } => {
                if text.is_empty() {
                    continue;
                }
                let mut part = json!({"text": text});
                if let Some(s) = pending.take() {
                    part["thoughtSignature"] = json!(s);
                }
                parts.push(part);
            }
            ContentPart::ToolUse { id, name, input } => {
                let mut part = json!({"functionCall": {"id": id, "name": name, "args": input}});
                if let Some(s) = pending.take() {
                    part["thoughtSignature"] = json!(s);
                }
                parts.push(part);
            }
            ContentPart::Image { source: ImageSource::Base64 { media_type, data }, .. } => {
                parts.push(json!({"inlineData": {"mimeType": media_type, "data": data}}));
            }
            ContentPart::Image { .. } | ContentPart::ToolResult { .. } => {}
        }
    }
    if let Some(s) = pending {
        parts.push(json!({"text": "", "thoughtSignature": s}));
    }
    parts
}

pub fn encode_response(resp: &CanonicalResponse) -> Value {
    let mut parts = response_parts(&resp.content);
    if parts.is_empty() {
        parts.push(json!({"text": ""}));
    }
    let mut v = json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": finish_reason_str(&resp.stop_reason),
            "index": 0,
        }],
        "usageMetadata": usage_metadata(&resp.usage),
        "modelVersion": resp.model,
    });
    if !resp.id.is_empty() {
        v["responseId"] = json!(resp.id);
    }
    v
}

fn new_call_id() -> String {
    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
}

pub fn decode_response(body: &Value) -> ProtocolResult<CanonicalResponse> {
    decode_response_mapped(body, &ToolNameMap::default())
}

/// 同 [`decode_response`]，并用 `map` 还原被改写过的工具名。
pub fn decode_response_mapped(body: &Value, map: &ToolNameMap) -> ProtocolResult<CanonicalResponse> {
    let obj = body.as_object().ok_or_else(|| invalid_resp("response body must be an object"))?;
    if obj.get("candidates").is_none() {
        if let Some(e) = obj.get("error").filter(|e| e.is_object()) {
            return Err(invalid_resp(format!("upstream returned an error: {}", error_message(e))));
        }
    }
    let usage = decode_usage(g(obj, "usageMetadata"));
    let base = |content: Vec<ContentPart>, stop_reason: StopReason| CanonicalResponse {
        id: g_str(obj, "responseId").unwrap_or_default(),
        model: g_str(obj, "modelVersion").unwrap_or_default(),
        created: None,
        content,
        stop_reason,
        stop_sequence: None,
        usage: usage.clone(),
    };

    let candidate = obj.get("candidates").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object);
    let Some(candidate) = candidate else {
        if let Some(pf) = g(obj, "promptFeedback").and_then(Value::as_object) {
            if pf.contains_key("blockReason") || pf.contains_key("block_reason") {
                return Ok(base(vec![], StopReason::ContentFilter));
            }
        }
        return Err(invalid_resp("response.candidates is empty"));
    };

    let mut content = Vec::new();
    let mut saw_tool = false;
    if let Some(parts) =
        candidate.get("content").and_then(|c| c.as_object()).and_then(|c| g(c, "parts")).and_then(Value::as_array)
    {
        for p in parts {
            let o = p.as_object().ok_or_else(|| invalid_resp("response part must be an object"))?;
            let mut warnings = Vec::new();
            let mut ctx = DecodeCtx::default();
            let before = content.len();
            decode_part(p, Role::Assistant, "part", &mut ctx, &mut content, &mut warnings)
                .map_err(|e| invalid_resp(e.to_string()))?;
            // 响应里的 functionCall：用 uuid id（跨轮不撞）并还原工具名。
            for c in content[before..].iter_mut() {
                if let ContentPart::ToolUse { id, name, .. } = c {
                    saw_tool = true;
                    if g(g(o, "functionCall").and_then(Value::as_object).unwrap_or(o), "id")
                        .and_then(Value::as_str)
                        .is_none()
                    {
                        *id = new_call_id();
                    }
                    *name = map.original(name);
                }
            }
        }
    }
    let reason = candidate.get("finishReason").and_then(Value::as_str);
    let stop = reason.map(|r| decode_finish_reason(r, saw_tool)).unwrap_or(if saw_tool {
        StopReason::ToolUse
    } else {
        StopReason::EndTurn
    });
    Ok(base(content, stop))
}

// ───────────────────────── 错误 ─────────────────────────

fn error_message(e: &Value) -> String {
    e.get("message").and_then(Value::as_str).unwrap_or("unknown upstream error").to_string()
}

fn kind_from_gemini_status(status: &str) -> Option<ApiErrorKind> {
    Some(match status {
        "INVALID_ARGUMENT" | "FAILED_PRECONDITION" | "OUT_OF_RANGE" => ApiErrorKind::InvalidRequest,
        "UNAUTHENTICATED" => ApiErrorKind::Authentication,
        "PERMISSION_DENIED" => ApiErrorKind::PermissionDenied,
        "NOT_FOUND" => ApiErrorKind::NotFound,
        "RESOURCE_EXHAUSTED" => ApiErrorKind::RateLimit,
        "UNIMPLEMENTED" | "NOT_IMPLEMENTED" => ApiErrorKind::NotImplemented,
        "UNAVAILABLE" => ApiErrorKind::BadGateway,
        "DEADLINE_EXCEEDED" => ApiErrorKind::Timeout,
        "INTERNAL" | "UNKNOWN" | "DATA_LOSS" | "ABORTED" | "CANCELLED" => ApiErrorKind::Internal,
        _ => return None,
    })
}

/// 解码上游错误体。`status` 为 HTTP 状态码；`body` 可以是 `{"error":{...}}`、Vertex 的 `[{"error":{...}}]`，
/// 或非 JSON 文本（以字符串 Value 传入）。
pub fn decode_error(status: u16, body: &Value) -> ApiError {
    let unwrapped = match body {
        Value::Array(a) => a.first().unwrap_or(&Value::Null),
        other => other,
    };
    if let Some(e) = unwrapped.get("error").filter(|e| e.is_object()) {
        let gstatus = e.get("status").and_then(Value::as_str);
        let kind = gstatus.and_then(kind_from_gemini_status).unwrap_or_else(|| super::wire::kind_from_status(status));
        let mut err = ApiError::new(kind, error_message(e));
        if let Some(s) = gstatus {
            err = err.with_code(s);
        }
        return err;
    }
    let msg = match body {
        Value::String(s) if !s.trim().is_empty() => s.clone(),
        _ => format!("upstream returned HTTP {status}"),
    };
    ApiError::new(super::wire::kind_from_status(status), msg)
}

/// Gemini 风格错误体 `{"error":{"code","message","status"}}`。
pub fn encode_error(e: &ApiError) -> Value {
    e.to_body(ProtocolKind::Gemini)
}

// ───────────────────────── 流式编码（canonical → ?alt=sse） ─────────────────────────

enum OpenBlock {
    Text,
    Thinking,
    Tool { id: String, name: String, args: String },
}

/// canonical 事件 → Gemini `streamGenerateContent?alt=sse` 帧。
///
/// Gemini 没有 start/stop 事件：文本 / 思考增量立即输出；functionCall 在参数完整（`ContentBlockStop`）时作为整块输出；
/// `MessageStop` 输出带 `finishReason` 与完整 `usageMetadata` 的最后一帧。流以连接关闭结束（无 `[DONE]`）。
pub struct GeminiStreamEncoder {
    started: bool,
    finished: bool,
    id: String,
    model: String,
    usage: Usage,
    open: BTreeMap<u32, OpenBlock>,
    pending_sig: Option<String>,
    saw_tool: bool,
}

impl Default for GeminiStreamEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiStreamEncoder {
    pub fn new() -> Self {
        Self {
            started: false,
            finished: false,
            id: String::new(),
            model: String::new(),
            usage: Usage::default(),
            open: BTreeMap::new(),
            pending_sig: None,
            saw_tool: false,
        }
    }

    fn chunk(&self, parts: Vec<Value>, finish: Option<&str>, usage: Option<Value>) -> SseFrame {
        let mut cand = json!({"content": {"role": "model", "parts": parts}, "index": 0});
        if let Some(f) = finish {
            cand["finishReason"] = json!(f);
        }
        let mut v = json!({"candidates": [cand], "modelVersion": self.model});
        if !self.id.is_empty() {
            v["responseId"] = json!(self.id);
        }
        if let Some(u) = usage {
            v["usageMetadata"] = u;
        }
        SseFrame::data(v.to_string())
    }

    fn close_tool(&mut self, id: String, name: String, args: String) -> ProtocolResult<SseFrame> {
        let input = if args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str::<Value>(&args)
                .map_err(|e| invalid_resp(format!("tool call `{name}` arguments are not valid JSON: {e}")))?
        };
        self.saw_tool = true;
        let mut part = json!({"functionCall": {"id": id, "name": name, "args": input}});
        if let Some(s) = self.pending_sig.take() {
            part["thoughtSignature"] = json!(s);
        }
        Ok(self.chunk(vec![part], None, None))
    }
}

impl StreamEncoder for GeminiStreamEncoder {
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>> {
        if self.finished {
            return Err(invalid_resp("stream event received after the stream ended"));
        }
        if let StreamEvent::Error { kind, message } = &event {
            self.finished = true;
            return Ok(vec![ApiError::new(*kind, message.clone()).to_sse_frame(ProtocolKind::Gemini)]);
        }
        if !self.started {
            let StreamEvent::MessageStart { id, model, usage } = event else {
                return Err(invalid_resp("first stream event must be message_start"));
            };
            self.started = true;
            self.id = id;
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
                        let mut part = json!({"text": text});
                        if let Some(s) = self.pending_sig.take() {
                            part["thoughtSignature"] = json!(s);
                        }
                        Ok(vec![self.chunk(vec![part], None, None)])
                    }
                    (BlockDelta::Thinking { text }, OpenBlock::Thinking) => {
                        Ok(vec![self.chunk(vec![json!({"text": text, "thought": true})], None, None)])
                    }
                    (BlockDelta::Signature { signature: sig }, OpenBlock::Thinking) => {
                        if let Some(raw) = signature::untag(signature::GEMINI, &sig) {
                            self.pending_sig = Some(raw.to_string());
                        }
                        Ok(vec![])
                    }
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
                let mut frames = Vec::new();
                for (_, b) in std::mem::take(&mut self.open) {
                    if let OpenBlock::Tool { id, name, args } = b {
                        frames.push(self.close_tool(id, name, args)?);
                    }
                }
                let mut last = json!({"text": ""});
                if let Some(s) = self.pending_sig.take() {
                    last["thoughtSignature"] = json!(s);
                }
                frames.push(self.chunk(
                    vec![last],
                    Some(finish_reason_str(&stop_reason)),
                    Some(usage_metadata(&self.usage)),
                ));
                Ok(frames)
            }
            StreamEvent::Error { .. } => unreachable!("handled above"),
        }
    }
}

// ───────────────────────── 流式解码（上游字节 → canonical 事件） ─────────────────────────

enum Source {
    Detecting(Vec<u8>),
    Sse(SseParser),
    Array(JsonArrayStream),
}

struct PendingCall2 {
    id: Option<String>,
    name: String,
    args: Value,
}

#[derive(Clone, Copy, PartialEq)]
enum OpenKind {
    Text,
    Thinking,
}

/// Gemini 上游流 → [`StreamEvent`]。同时支持 `?alt=sse` 与不带 `alt` 的 JSON 数组流，
/// 任意位置的半包都安全；`finish()` 必须在上游 EOF 后调用。
pub struct GeminiStreamDecoder {
    source: Source,
    names: ToolNameMap,
    started: bool,
    finished: bool,
    next_index: u32,
    open: Option<(u32, OpenKind)>,
    pending_call: Option<PendingCall2>,
    usage: Usage,
    finish_reason: Option<String>,
    blocked: bool,
    saw_tool: bool,
}

impl Default for GeminiStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl GeminiStreamDecoder {
    pub fn new() -> Self {
        Self::with_name_map(ToolNameMap::default())
    }

    pub fn with_name_map(names: ToolNameMap) -> Self {
        Self {
            source: Source::Detecting(Vec::new()),
            names,
            started: false,
            finished: false,
            next_index: 0,
            open: None,
            pending_call: None,
            usage: Usage::default(),
            finish_reason: None,
            blocked: false,
            saw_tool: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        let mut out = Vec::new();
        let mut chunks: Vec<Value> = Vec::new();
        let mut frames_data: Vec<String> = Vec::new();

        let feed =
            |src: &mut Source, bytes: &[u8], frames: &mut Vec<String>, chunks: &mut Vec<Value>| -> ProtocolResult<()> {
                match src {
                    Source::Sse(p) => frames.extend(p.push(bytes).into_iter().map(|f| f.data)),
                    Source::Array(a) => chunks.extend(a.push(bytes)?),
                    Source::Detecting(_) => unreachable!(),
                }
                Ok(())
            };

        if let Source::Detecting(buf) = &mut self.source {
            buf.extend_from_slice(bytes);
            let Some(first) =
                buf.iter().copied().find(|b| !b.is_ascii_whitespace() && *b != 0xEF && *b != 0xBB && *b != 0xBF)
            else {
                return Ok(out);
            };
            let all = std::mem::take(buf);
            self.source = if first == b'[' {
                Source::Array(JsonArrayStream::default())
            } else {
                Source::Sse(SseParser::default())
            };
            feed(&mut self.source, &all, &mut frames_data, &mut chunks)?;
        } else {
            feed(&mut self.source, bytes, &mut frames_data, &mut chunks)?;
        }
        self.consume(frames_data, chunks, &mut out)?;
        Ok(out)
    }

    fn consume(&mut self, frames: Vec<String>, chunks: Vec<Value>, out: &mut Vec<StreamEvent>) -> ProtocolResult<()> {
        for data in frames {
            let t = data.trim();
            if t.is_empty() || t == "[DONE]" {
                continue;
            }
            let v: Value =
                serde_json::from_str(t).map_err(|e| invalid_resp(format!("malformed Gemini stream chunk: {e}")))?;
            self.process_chunk(&v, out)?;
        }
        for v in chunks {
            self.process_chunk(&v, out)?;
        }
        Ok(())
    }

    fn ensure_started(&mut self, v: &Map<String, Value>, out: &mut Vec<StreamEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        let usage = decode_usage(g(v, "usageMetadata"));
        out.push(StreamEvent::MessageStart {
            id: g_str(v, "responseId").unwrap_or_else(|| format!("gemini-{}", uuid::Uuid::new_v4().simple())),
            model: g_str(v, "modelVersion").unwrap_or_default(),
            // 首块通常带完整的输入侧用量（含缓存命中）；输出侧用量只在结束时给出。
            usage: usage.input_tokens.map(|_| Usage {
                input_tokens: usage.input_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                ..Default::default()
            }),
        });
    }

    fn close_open(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((index, _)) = self.open.take() {
            out.push(StreamEvent::ContentBlockStop { index });
        }
    }

    fn alloc(&mut self) -> u32 {
        let i = self.next_index;
        self.next_index += 1;
        i
    }

    fn emit_signature(&mut self, sig: &str, out: &mut Vec<StreamEvent>) {
        let tagged = signature::tag(signature::GEMINI, sig);
        if let Some((index, OpenKind::Thinking)) = self.open {
            out.push(StreamEvent::ContentBlockDelta { index, delta: BlockDelta::Signature { signature: tagged } });
            self.close_open(out);
            return;
        }
        self.close_open(out);
        let index = self.alloc();
        out.push(StreamEvent::ContentBlockStart { index, block: BlockStart::Thinking });
        out.push(StreamEvent::ContentBlockDelta { index, delta: BlockDelta::Signature { signature: tagged } });
        out.push(StreamEvent::ContentBlockStop { index });
    }

    fn emit_text(&mut self, kind: OpenKind, text: &str, out: &mut Vec<StreamEvent>) {
        if text.is_empty() {
            return;
        }
        let index = match &self.open {
            Some((i, k)) if *k == kind => *i,
            _ => {
                self.close_open(out);
                let i = self.alloc();
                out.push(StreamEvent::ContentBlockStart {
                    index: i,
                    block: if kind == OpenKind::Thinking { BlockStart::Thinking } else { BlockStart::Text },
                });
                self.open = Some((i, kind));
                i
            }
        };
        let delta = match &self.open {
            Some((_, OpenKind::Thinking)) => BlockDelta::Thinking { text: text.to_string() },
            _ => BlockDelta::Text { text: text.to_string() },
        };
        out.push(StreamEvent::ContentBlockDelta { index, delta });
    }

    fn emit_call(&mut self, id: Option<String>, wire_name: &str, args: &Value, out: &mut Vec<StreamEvent>) {
        self.close_open(out);
        let index = self.alloc();
        self.saw_tool = true;
        out.push(StreamEvent::ContentBlockStart {
            index,
            block: BlockStart::ToolUse { id: id.unwrap_or_else(new_call_id), name: self.names.original(wire_name) },
        });
        out.push(StreamEvent::ToolCallDelta { index, partial_json: args.to_string() });
        out.push(StreamEvent::ContentBlockStop { index });
    }

    fn handle_function_call(&mut self, fc: &Map<String, Value>, out: &mut Vec<StreamEvent>) {
        let will_continue = g(fc, "willContinue").and_then(Value::as_bool).unwrap_or(false);
        let partial = g(fc, "partialArgs").and_then(Value::as_array);
        let name = g_str(fc, "name").unwrap_or_default();
        if self.pending_call.is_none() && !will_continue && partial.is_none() {
            let args = g(fc, "args").cloned().unwrap_or_else(|| json!({}));
            self.emit_call(g_str(fc, "id"), &name, &args, out);
            return;
        }
        let pc =
            self.pending_call.get_or_insert_with(|| PendingCall2 { id: None, name: String::new(), args: json!({}) });
        if pc.name.is_empty() {
            pc.name = name;
        }
        if pc.id.is_none() {
            pc.id = g_str(fc, "id");
        }
        if let Some(Value::Object(a)) = g(fc, "args") {
            if let Value::Object(dst) = &mut pc.args {
                dst.extend(a.clone());
            }
        }
        for pa in partial.into_iter().flatten() {
            if let Some(po) = pa.as_object() {
                apply_partial_arg(&mut pc.args, po);
            }
        }
        if !will_continue {
            let pc = self.pending_call.take().expect("pending call set above");
            self.emit_call(pc.id, &pc.name, &pc.args, out);
        }
    }

    fn process_chunk(&mut self, v: &Value, out: &mut Vec<StreamEvent>) -> ProtocolResult<()> {
        if self.finished {
            return Ok(());
        }
        let obj = v.as_object().ok_or_else(|| invalid_resp("Gemini stream chunk must be an object"))?;
        if let Some(e) = obj.get("error").filter(|e| e.is_object()) {
            let err = decode_error(e.get("code").and_then(Value::as_u64).unwrap_or(500) as u16, v);
            self.close_open(out);
            self.finished = true;
            out.push(StreamEvent::Error { kind: err.kind, message: err.message });
            return Ok(());
        }
        self.ensure_started(obj, out);
        if g(obj, "usageMetadata").is_some() {
            self.usage.merge(&decode_usage(g(obj, "usageMetadata")));
        }
        if let Some(pf) = g(obj, "promptFeedback").and_then(Value::as_object) {
            if pf.contains_key("blockReason") || pf.contains_key("block_reason") {
                self.blocked = true;
            }
        }
        let Some(cand) =
            obj.get("candidates").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object)
        else {
            return Ok(());
        };
        if let Some(parts) =
            cand.get("content").and_then(Value::as_object).and_then(|c| g(c, "parts")).and_then(Value::as_array)
        {
            for p in parts {
                let Some(po) = p.as_object() else { continue };
                let sig = g_str(po, "thoughtSignature").filter(|s| !s.is_empty());
                let is_thought = g(po, "thought").and_then(Value::as_bool).unwrap_or(false);
                if let Some(text) = po.get("text").and_then(Value::as_str) {
                    if is_thought {
                        self.emit_text(OpenKind::Thinking, text, out);
                        if let Some(s) = sig {
                            self.emit_signature(&s, out);
                        }
                    } else if let Some(s) = sig {
                        let continuing = matches!(self.open, Some((_, OpenKind::Text)));
                        if continuing {
                            self.emit_text(OpenKind::Text, text, out);
                            self.emit_signature(&s, out);
                        } else {
                            self.emit_signature(&s, out);
                            self.emit_text(OpenKind::Text, text, out);
                        }
                    } else {
                        self.emit_text(OpenKind::Text, text, out);
                    }
                } else if let Some(fc) = g(po, "functionCall").and_then(Value::as_object) {
                    if let Some(s) = sig {
                        self.emit_signature(&s, out);
                    }
                    self.handle_function_call(fc, out);
                } else if g(po, "inlineData").is_some() || g(po, "fileData").is_some() {
                    tracing::debug!("Gemini stream: media part in a streamed response was dropped (not representable as a stream event)");
                } else if g(po, "executableCode").is_some() || g(po, "codeExecutionResult").is_some() {
                    tracing::debug!("Gemini stream: code execution part was dropped");
                }
            }
        }
        if let Some(r) = cand.get("finishReason").and_then(Value::as_str) {
            self.finish_reason = Some(r.to_string());
        }
        Ok(())
    }

    /// 上游 EOF：交出残留数据并输出终止事件。
    pub fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        let mut out = Vec::new();
        let mut frames: Vec<String> = Vec::new();
        match &mut self.source {
            Source::Sse(p) => frames.extend(p.finish().into_iter().map(|f| f.data)),
            Source::Array(a) => {
                if a.is_truncated() && !self.finished {
                    return Err(invalid_resp("upstream JSON array stream was truncated"));
                }
            }
            Source::Detecting(buf) => {
                if !buf.is_empty() && !buf.iter().all(u8::is_ascii_whitespace) {
                    return Err(invalid_resp("upstream stream ended with unparseable data"));
                }
            }
        }
        self.consume(frames, Vec::new(), &mut out)?;
        if self.finished {
            return Ok(out);
        }
        if !self.started {
            return Err(invalid_resp("upstream stream ended without any data"));
        }
        if let Some(pc) = self.pending_call.take() {
            self.emit_call(pc.id, &pc.name, &pc.args, &mut out);
        }
        self.close_open(&mut out);
        self.finished = true;
        if self.finish_reason.is_none() && !self.blocked {
            out.push(StreamEvent::Error {
                kind: ApiErrorKind::BadGateway,
                message: "upstream stream ended without a finishReason".into(),
            });
            return Ok(out);
        }
        if !self.usage.is_empty() {
            out.push(StreamEvent::Usage(self.usage.clone()));
        }
        let stop_reason = match &self.finish_reason {
            Some(r) => decode_finish_reason(r, self.saw_tool),
            None => StopReason::ContentFilter,
        };
        out.push(StreamEvent::MessageStop { stop_reason, stop_sequence: None });
        Ok(out)
    }
}

/// 把 `partialArgs` 条目（`jsonPath` + 值）应用到累积中的 args 上。字符串值按顺序拼接。
fn apply_partial_arg(args: &mut Value, pa: &Map<String, Value>) {
    let Some(path) = pa.get("jsonPath").or_else(|| pa.get("json_path")).and_then(Value::as_str) else { return };
    let value = if let Some(s) = g(pa, "stringValue") {
        s.clone()
    } else if let Some(n) = g(pa, "numberValue") {
        n.clone()
    } else if let Some(b) = g(pa, "boolValue") {
        b.clone()
    } else if pa.contains_key("nullValue") || pa.contains_key("null_value") {
        Value::Null
    } else {
        return;
    };
    // 解析 `$.a.b[0].c`
    let mut tokens: Vec<Result<String, usize>> = Vec::new();
    let rest = path.strip_prefix('$').unwrap_or(path);
    let mut cur = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !cur.is_empty() {
                    tokens.push(Ok(std::mem::take(&mut cur)));
                }
            }
            '[' => {
                if !cur.is_empty() {
                    tokens.push(Ok(std::mem::take(&mut cur)));
                }
                let mut num = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    num.push(d);
                }
                if let Ok(n) = num.parse::<usize>() {
                    tokens.push(Err(n));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(Ok(cur));
    }
    if tokens.is_empty() {
        return;
    }
    let mut node = args;
    for (i, tok) in tokens.iter().enumerate() {
        let last = i + 1 == tokens.len();
        let slot: &mut Value = match tok {
            Ok(key) => {
                if !node.is_object() {
                    *node = json!({});
                }
                node.as_object_mut().expect("object ensured").entry(key.clone()).or_insert(Value::Null)
            }
            Err(idx) => {
                if !node.is_array() {
                    *node = json!([]);
                }
                let arr = node.as_array_mut().expect("array ensured");
                while arr.len() <= *idx {
                    arr.push(Value::Null);
                }
                &mut arr[*idx]
            }
        };
        if last {
            match (&value, &*slot) {
                (Value::String(add), Value::String(old)) => *slot = Value::String(format!("{old}{add}")),
                _ => *slot = value.clone(),
            }
            return;
        }
        node = slot;
    }
}

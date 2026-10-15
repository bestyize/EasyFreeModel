//! 上游 SSE 解码：字节流 → 帧（处理半包）→ 规范化 [`StreamEvent`] 状态机。
//!
//! - [`SseSplitter`] 按空行切分帧，保留每帧的原始字节（透传时逐帧原样转发）。
//! - [`OpenAiStreamDecoder`]：delta / tool_call 分片 / finish_reason / usage chunk / `[DONE]`，
//!   并把 `reasoning_content`、`reasoning` 映射为思考块。
//! - [`AnthropicStreamDecoder`]：`message_start … message_stop`。
//! - 两个解码器都要求流以「终止标记」结束；上游中途断开时 [`StreamDecoder::finish`] 返回错误，
//!   由调用方转成协议风格的错误事件（绝不静默结束）。
//! - [`NdjsonSplitter`]：Ollama 的 NDJSON 流按行切帧（一行一帧）。
//! - [`BytesStreamDecoder`]：把协议层按字节喂入的解码器（Responses / Gemini / Ollama）适配成帧级 [`StreamDecoder`]。
//!
//! 日志纪律：错误信息只描述结构问题，不回显帧内容。

use std::collections::{BTreeSet, HashMap};

use bytes::Bytes;
use serde_json::Value;

use crate::protocols::canonical::{BlockDelta, BlockStart, StopReason, StreamEvent, Usage};
use crate::protocols::gemini::GeminiStreamDecoder;
use crate::protocols::ollama::OllamaStreamDecoder;
use crate::protocols::openai_responses::ResponsesStreamDecoder;
use crate::protocols::{anthropic, openai_chat, sse::parse_sse, ApiErrorKind, ProtocolError, ProtocolResult};

/// 单个 SSE 帧的上限，超过视为上游异常，避免无限缓冲。
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// 帧的原始字节（含结尾空行），透传时原样写给客户端。
    pub raw: Bytes,
    pub event: Option<String>,
    pub data: String,
}

impl std::fmt::Debug for RawFrame {
    // 帧内容是模型输出，Debug 只显示结构
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawFrame").field("event", &self.event).field("data_bytes", &self.data.len()).finish()
    }
}

impl RawFrame {
    /// 只有注释 / 空内容（例如 `: keepalive`）。
    pub fn is_empty(&self) -> bool {
        self.event.is_none() && self.data.is_empty()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SseError {
    #[error("the upstream sent an SSE frame larger than the allowed limit")]
    FrameTooLarge,
}

fn make_frame(raw: Vec<u8>) -> RawFrame {
    let text = String::from_utf8_lossy(&raw);
    let parsed = parse_sse(&text).into_iter().next();
    let (event, data) = parsed.map(|f| (f.event, f.data)).unwrap_or((None, String::new()));
    RawFrame { raw: Bytes::from(raw), event, data }
}

/// 增量切帧器：喂入任意切分的字节块，吐出完整帧。
#[derive(Debug, Default)]
pub struct SseSplitter {
    buf: Vec<u8>,
    /// 下一次扫描 `\n` 的起点。
    scan: usize,
    /// 当前帧是否已经出现过非空行。
    has_content: bool,
    /// 当前行起点。
    line_start: usize,
}

impl SseSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<RawFrame>, SseError> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            let Some(rel) = self.buf[self.scan..].iter().position(|b| *b == b'\n') else {
                self.scan = self.buf.len();
                break;
            };
            let nl = self.scan + rel;
            let mut line_end = nl;
            if line_end > self.line_start && self.buf[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            if line_end == self.line_start {
                let raw: Vec<u8> = self.buf.drain(..=nl).collect();
                if self.has_content {
                    frames.push(make_frame(raw));
                }
                self.scan = 0;
                self.line_start = 0;
                self.has_content = false;
            } else {
                self.has_content = true;
                self.scan = nl + 1;
                self.line_start = nl + 1;
            }
        }
        if self.buf.len() > MAX_FRAME_BYTES {
            return Err(SseError::FrameTooLarge);
        }
        Ok(frames)
    }

    /// 流结束时的残留：有内容就当作最后一帧（宽松处理缺少结尾空行的上游）。
    pub fn finish(&mut self) -> Option<RawFrame> {
        if !self.has_content && self.buf.iter().all(|b| b.is_ascii_whitespace()) {
            self.buf.clear();
            return None;
        }
        let raw = std::mem::take(&mut self.buf);
        self.scan = 0;
        self.line_start = 0;
        self.has_content = false;
        Some(make_frame(raw))
    }
}

/// NDJSON 切行器：一行一帧（`data` 是该行文本，`raw` 是该行加换行）。空行被忽略。
#[derive(Debug, Default)]
pub struct NdjsonSplitter {
    buf: Vec<u8>,
}

fn ndjson_frame(mut line: Vec<u8>) -> Option<RawFrame> {
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    let text = String::from_utf8_lossy(&line).into_owned();
    if text.trim().is_empty() {
        return None;
    }
    let mut raw = line;
    raw.push(b'\n');
    Some(RawFrame { raw: Bytes::from(raw), event: None, data: text })
}

impl NdjsonSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<RawFrame>, SseError> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop(); // '\n'
            frames.extend(ndjson_frame(line));
        }
        if self.buf.len() > MAX_FRAME_BYTES {
            return Err(SseError::FrameTooLarge);
        }
        Ok(frames)
    }

    /// 流结束时的残留半行（宽松处理没有结尾换行的上游）。
    pub fn finish(&mut self) -> Option<RawFrame> {
        ndjson_frame(std::mem::take(&mut self.buf))
    }
}

/// 上游流的切帧器：SSE 或 NDJSON。
#[derive(Debug)]
pub enum FrameSplitter {
    Sse(SseSplitter),
    Ndjson(NdjsonSplitter),
}

impl FrameSplitter {
    pub fn new(framing: crate::upstream::StreamFraming) -> Self {
        match framing {
            crate::upstream::StreamFraming::Sse => FrameSplitter::Sse(SseSplitter::new()),
            crate::upstream::StreamFraming::Ndjson => FrameSplitter::Ndjson(NdjsonSplitter::new()),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<RawFrame>, SseError> {
        match self {
            FrameSplitter::Sse(s) => s.push(chunk),
            FrameSplitter::Ndjson(s) => s.push(chunk),
        }
    }

    pub fn finish(&mut self) -> Option<RawFrame> {
        match self {
            FrameSplitter::Sse(s) => s.finish(),
            FrameSplitter::Ndjson(s) => s.finish(),
        }
    }
}

// ───────────────────────── 解码器 ─────────────────────────

pub trait StreamDecoder: Send {
    fn push(&mut self, frame: &RawFrame) -> ProtocolResult<Vec<StreamEvent>>;
    /// 上游字节流 EOF 时调用。已经正常结束返回空；否则返回 `InvalidResponse`（截断）。
    fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>>;
    /// 已经看到终止标记（或足以认定结束的信号）。
    fn is_complete(&self) -> bool;
    /// 已经看到明确的终止标记（`[DONE]` / `message_stop` / 错误帧），之后不会再有有意义的帧。
    fn is_terminated(&self) -> bool;
    /// 最近一次 `push` 的帧是否是只携带 usage 的 chunk（OpenAI `choices: []`）。
    fn last_was_usage_only(&self) -> bool {
        false
    }
}

fn bad(msg: &str) -> ProtocolError {
    ProtocolError::InvalidResponse(msg.to_string())
}

fn error_kind_from(v: &Value) -> ApiErrorKind {
    let hay = ["type", "code", "status"]
        .iter()
        .filter_map(|k| v.get(*k))
        .map(|x| x.to_string().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if hay.contains("rate") || hay.contains("429") || hay.contains("quota") {
        ApiErrorKind::RateLimit
    } else if hay.contains("auth") || hay.contains("api_key") || hay.contains("401") {
        ApiErrorKind::Authentication
    } else if hay.contains("permission") || hay.contains("403") {
        ApiErrorKind::PermissionDenied
    } else if hay.contains("invalid_request") || hay.contains("400") {
        ApiErrorKind::InvalidRequest
    } else if hay.contains("timeout") {
        ApiErrorKind::Timeout
    } else {
        ApiErrorKind::BadGateway
    }
}

fn error_event(err: &Value) -> StreamEvent {
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| err.as_str())
        .unwrap_or("the upstream reported an error")
        .chars()
        .take(2000)
        .collect::<String>();
    StreamEvent::Error { kind: error_kind_from(err), message }
}

/// OpenAI Chat Completions chunk 流。
#[derive(Debug, Default)]
pub struct OpenAiStreamDecoder {
    started: bool,
    done: bool,
    text_block: Option<u32>,
    thinking_block: Option<u32>,
    /// OpenAI tool_calls[].index → 规范化块下标
    tools: HashMap<u32, u32>,
    open_tools: BTreeSet<u32>,
    next_index: u32,
    finish: Option<StopReason>,
    usage_only: bool,
}

impl OpenAiStreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc(&mut self) -> u32 {
        let i = self.next_index;
        self.next_index += 1;
        i
    }

    fn close_text_like(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some(i) = self.text_block.take() {
            out.push(StreamEvent::ContentBlockStop { index: i });
        }
        if let Some(i) = self.thinking_block.take() {
            out.push(StreamEvent::ContentBlockStop { index: i });
        }
    }

    fn close_all(&mut self, out: &mut Vec<StreamEvent>) {
        self.close_text_like(out);
        for i in std::mem::take(&mut self.open_tools) {
            out.push(StreamEvent::ContentBlockStop { index: i });
        }
    }

    fn stop(&mut self, out: &mut Vec<StreamEvent>) {
        self.close_all(out);
        self.done = true;
        out.push(StreamEvent::MessageStop { stop_reason: self.finish.take().unwrap_or(StopReason::EndTurn), stop_sequence: None });
    }

    fn handle_delta(&mut self, delta: &Value, out: &mut Vec<StreamEvent>) {
        let content = delta.get("content").and_then(Value::as_str).filter(|s| !s.is_empty());
        // Some OpenAI-compatible gateways (including CodeBuddy) interleave
        // reasoning_content with the final answer, sometimes in the *same*
        // SSE delta and sometimes in later deltas. Once answer text begins,
        // don't reopen a thinking block for every token: that closes and
        // recreates text blocks, which can render as one word per line in
        // Anthropic/Responses clients. Keep genuine newlines untouched.
        if self.text_block.is_none() && content.is_none() {
            for key in ["reasoning_content", "reasoning"] {
                if let Some(t) = delta.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    let idx = match self.thinking_block {
                        Some(i) => i,
                        None => {
                            let i = self.alloc();
                            self.thinking_block = Some(i);
                            out.push(StreamEvent::ContentBlockStart { index: i, block: BlockStart::Thinking });
                            i
                        }
                    };
                    out.push(StreamEvent::ContentBlockDelta { index: idx, delta: BlockDelta::Thinking { text: t.to_string() } });
                    break;
                }
            }
        }
        if let Some(t) = content {
            if let Some(i) = self.thinking_block.take() {
                out.push(StreamEvent::ContentBlockStop { index: i });
            }
            let idx = match self.text_block {
                Some(i) => i,
                None => {
                    let i = self.alloc();
                    self.text_block = Some(i);
                    out.push(StreamEvent::ContentBlockStart { index: i, block: BlockStart::Text });
                    i
                }
            };
            out.push(StreamEvent::ContentBlockDelta { index: idx, delta: BlockDelta::Text { text: t.to_string() } });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (pos, call) in calls.iter().enumerate() {
                let oi = call.get("index").and_then(Value::as_u64).map(|n| n as u32).unwrap_or(pos as u32);
                let func = call.get("function");
                let block = match self.tools.get(&oi) {
                    Some(b) => *b,
                    None => {
                        self.close_text_like(out);
                        let b = self.alloc();
                        self.tools.insert(oi, b);
                        self.open_tools.insert(b);
                        let id = call.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
                            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
                        let name = func.and_then(|f| f.get("name")).and_then(Value::as_str).unwrap_or("").to_string();
                        out.push(StreamEvent::ContentBlockStart { index: b, block: BlockStart::ToolUse { id, name } });
                        b
                    }
                };
                let args = match func.and_then(|f| f.get("arguments")) {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                if !args.is_empty() {
                    out.push(StreamEvent::ToolCallDelta { index: block, partial_json: args });
                }
            }
        }
    }
}

impl StreamDecoder for OpenAiStreamDecoder {
    fn push(&mut self, frame: &RawFrame) -> ProtocolResult<Vec<StreamEvent>> {
        self.usage_only = false;
        if self.done {
            return Ok(vec![]);
        }
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        if data == "[DONE]" {
            if !self.started {
                return Err(bad("the upstream stream ended without producing any data"));
            }
            self.stop(&mut out);
            return Ok(out);
        }
        let v: Value = serde_json::from_str(data).map_err(|_| bad("the upstream sent an SSE frame that is not valid JSON"))?;
        let choice = v.get("choices").and_then(Value::as_array).and_then(|a| a.first());
        if choice.is_none() {
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                self.done = true;
                return Ok(vec![error_event(err)]);
            }
        }
        if !self.started {
            self.started = true;
            out.push(StreamEvent::MessageStart {
                id: v.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                model: v.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
                usage: None,
            });
        }
        if let Some(choice) = choice {
            if let Some(delta) = choice.get("delta") {
                self.handle_delta(delta, &mut out);
            }
            if let Some(fr) = choice.get("finish_reason").filter(|f| !f.is_null()) {
                self.close_all(&mut out);
                self.finish = Some(openai_chat::decode_finish_reason(Some(fr)));
            }
        } else if v.get("usage").is_some_and(|u| !u.is_null()) {
            self.usage_only = true;
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let usage = openai_chat::decode_usage(Some(u));
            if !usage.is_empty() {
                out.push(StreamEvent::Usage(usage));
            }
        }
        Ok(out)
    }

    fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        if self.done {
            return Ok(vec![]);
        }
        if self.started && self.finish.is_some() {
            let mut out = Vec::new();
            self.stop(&mut out);
            return Ok(out);
        }
        Err(bad("the upstream stream ended before it completed"))
    }

    fn is_complete(&self) -> bool {
        self.done || self.finish.is_some()
    }

    fn is_terminated(&self) -> bool {
        self.done
    }

    fn last_was_usage_only(&self) -> bool {
        self.usage_only
    }
}

/// Anthropic Messages 事件流。
#[derive(Debug, Default)]
pub struct AnthropicStreamDecoder {
    started: bool,
    done: bool,
    open: BTreeSet<u32>,
    stop: Option<(StopReason, Option<String>)>,
}

impl AnthropicStreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamDecoder for AnthropicStreamDecoder {
    fn push(&mut self, frame: &RawFrame) -> ProtocolResult<Vec<StreamEvent>> {
        if self.done {
            return Ok(vec![]);
        }
        let data = frame.data.trim();
        if data.is_empty() {
            return Ok(vec![]);
        }
        let v: Value = serde_json::from_str(data).map_err(|_| bad("the upstream sent an SSE frame that is not valid JSON"))?;
        let ty = v.get("type").and_then(Value::as_str).or(frame.event.as_deref()).unwrap_or("");
        let index = v.get("index").and_then(Value::as_u64).map(|n| n as u32);
        let mut out = Vec::new();
        match ty {
            "message_start" => {
                if self.started {
                    return Err(bad("duplicate message_start from the upstream"));
                }
                self.started = true;
                let m = v.get("message");
                let usage = anthropic::decode_usage(m.and_then(|m| m.get("usage")));
                out.push(StreamEvent::MessageStart {
                    id: m.and_then(|m| m.get("id")).and_then(Value::as_str).unwrap_or("").to_string(),
                    model: m.and_then(|m| m.get("model")).and_then(Value::as_str).unwrap_or("").to_string(),
                    usage: (!usage.is_empty()).then_some(usage),
                });
            }
            "content_block_start" => {
                let index = index.ok_or_else(|| bad("content_block_start without an index"))?;
                let cb = v.get("content_block").ok_or_else(|| bad("content_block_start without a content_block"))?;
                let block = match cb.get("type").and_then(Value::as_str) {
                    Some("text") => BlockStart::Text,
                    Some("thinking") => BlockStart::Thinking,
                    Some("tool_use") => BlockStart::ToolUse {
                        id: cb.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: cb.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    },
                    _ => return Err(ProtocolError::Unsupported("the upstream sent a content block type that cannot be converted".into())),
                };
                self.open.insert(index);
                out.push(StreamEvent::ContentBlockStart { index, block });
                if let Some(t) = cb.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    out.push(StreamEvent::ContentBlockDelta { index, delta: BlockDelta::Text { text: t.to_string() } });
                }
            }
            "content_block_delta" => {
                let index = index.ok_or_else(|| bad("content_block_delta without an index"))?;
                let d = v.get("delta").ok_or_else(|| bad("content_block_delta without a delta"))?;
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => out.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: BlockDelta::Text { text: d.get("text").and_then(Value::as_str).unwrap_or("").to_string() },
                    }),
                    Some("thinking_delta") => out.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: BlockDelta::Thinking { text: d.get("thinking").and_then(Value::as_str).unwrap_or("").to_string() },
                    }),
                    Some("signature_delta") => out.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: BlockDelta::Signature { signature: d.get("signature").and_then(Value::as_str).unwrap_or("").to_string() },
                    }),
                    Some("input_json_delta") => out.push(StreamEvent::ToolCallDelta {
                        index,
                        partial_json: d.get("partial_json").and_then(Value::as_str).unwrap_or("").to_string(),
                    }),
                    _ => {} // citations_delta 等：规范化模型无法表达，忽略
                }
            }
            "content_block_stop" => {
                let index = index.ok_or_else(|| bad("content_block_stop without an index"))?;
                if self.open.remove(&index) {
                    out.push(StreamEvent::ContentBlockStop { index });
                }
            }
            "message_delta" => {
                if let Some(d) = v.get("delta") {
                    let reason = anthropic::decode_stop_reason(d.get("stop_reason"));
                    let seq = d.get("stop_sequence").and_then(Value::as_str).map(str::to_string);
                    self.stop = Some((reason, seq));
                }
                let usage = anthropic::decode_usage(v.get("usage"));
                if !usage.is_empty() {
                    out.push(StreamEvent::Usage(usage));
                }
            }
            "message_stop" => {
                for i in std::mem::take(&mut self.open) {
                    out.push(StreamEvent::ContentBlockStop { index: i });
                }
                self.done = true;
                let (stop_reason, stop_sequence) = self.stop.take().unwrap_or((StopReason::EndTurn, None));
                out.push(StreamEvent::MessageStop { stop_reason, stop_sequence });
            }
            "error" => {
                self.done = true;
                out.push(error_event(v.get("error").unwrap_or(&v)));
            }
            _ => {} // ping 等
        }
        Ok(out)
    }

    fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        if self.done {
            Ok(vec![])
        } else {
            Err(bad("the upstream stream ended before it completed"))
        }
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn is_terminated(&self) -> bool {
        self.done
    }
}

/// 协议层按字节喂入的流解码器（Responses / Gemini / Ollama 的统一视图）。
pub trait ByteStreamDecode: Send {
    fn push_bytes(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>>;
    fn finish_bytes(&mut self) -> ProtocolResult<Vec<StreamEvent>>;
}

impl ByteStreamDecode for ResponsesStreamDecoder {
    fn push_bytes(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        self.push(bytes)
    }
    fn finish_bytes(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        self.finish()
    }
}

impl ByteStreamDecode for GeminiStreamDecoder {
    fn push_bytes(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        self.push(bytes)
    }
    fn finish_bytes(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        self.finish()
    }
}

impl ByteStreamDecode for OllamaStreamDecoder {
    fn push_bytes(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<StreamEvent>> {
        self.push(bytes)
    }
    fn finish_bytes(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        self.finish()
    }
}

/// 把 [`ByteStreamDecode`] 适配成帧级 [`StreamDecoder`]：每个原始帧的字节原样喂给内部解码器。
///
/// 内部解码器在流被截断时不返回 `Err`，而是在 `finish()` 里产生 `StreamEvent::Error`；
/// 调用方（`gateway::stream`）必须检查 `finish()` 返回的事件里是否有 `Error`。
pub struct BytesStreamDecoder<D: ByteStreamDecode> {
    inner: D,
    terminated: bool,
}

impl<D: ByteStreamDecode> BytesStreamDecoder<D> {
    pub fn new(inner: D) -> Self {
        Self { inner, terminated: false }
    }

    fn observe(&mut self, events: &[StreamEvent]) {
        if events.iter().any(|e| matches!(e, StreamEvent::MessageStop { .. } | StreamEvent::Error { .. })) {
            self.terminated = true;
        }
    }
}

impl<D: ByteStreamDecode> StreamDecoder for BytesStreamDecoder<D> {
    fn push(&mut self, frame: &RawFrame) -> ProtocolResult<Vec<StreamEvent>> {
        let events = self.inner.push_bytes(&frame.raw)?;
        self.observe(&events);
        Ok(events)
    }

    fn finish(&mut self) -> ProtocolResult<Vec<StreamEvent>> {
        let events = self.inner.finish_bytes()?;
        self.observe(&events);
        Ok(events)
    }

    fn is_complete(&self) -> bool {
        self.terminated
    }

    fn is_terminated(&self) -> bool {
        self.terminated
    }
}

/// 累计 usage 与输出体量的小工具，用于用量记录（透传与转换两种模式共用）。
#[derive(Debug, Default, Clone)]
pub struct StreamTally {
    pub usage: Usage,
    /// 已输出的文本类字节数（文本 / 思考 / 工具入参），用于缺少 usage 时的粗略估算。
    pub output_bytes: usize,
}

impl StreamTally {
    pub fn observe(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::MessageStart { usage: Some(u), .. } => self.usage.merge(u),
            StreamEvent::Usage(u) => self.usage.merge(u),
            StreamEvent::ContentBlockDelta { delta, .. } => {
                self.output_bytes += match delta {
                    BlockDelta::Text { text } | BlockDelta::Thinking { text } => text.len(),
                    BlockDelta::Signature { .. } => 0,
                };
            }
            StreamEvent::ToolCallDelta { partial_json, .. } => self.output_bytes += partial_json.len(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frames(text: &str) -> Vec<RawFrame> {
        let mut s = SseSplitter::new();
        let mut out = s.push(text.as_bytes()).unwrap();
        out.extend(s.finish());
        out
    }

    fn oa(chunks: &[Value], done: bool) -> String {
        let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        if done {
            s.push_str("data: [DONE]\n\n");
        }
        s
    }

    fn run(dec: &mut dyn StreamDecoder, text: &str) -> ProtocolResult<Vec<StreamEvent>> {
        let mut events = Vec::new();
        for f in frames(text) {
            events.extend(dec.push(&f)?);
        }
        events.extend(dec.finish()?);
        Ok(events)
    }

    #[test]
    fn splitter_handles_partial_chunks_crlf_comments_and_multibyte() {
        let text = ": keepalive\n\nevent: a\ndata: {\"t\":\"你好\"}\n\r\ndata: x\r\ndata: y\r\n\r\n";
        let bytes = text.as_bytes();
        for split in 1..bytes.len() {
            let mut s = SseSplitter::new();
            let mut got = s.push(&bytes[..split]).unwrap();
            got.extend(s.push(&bytes[split..]).unwrap());
            assert!(s.finish().is_none(), "split at {split}");
            assert_eq!(got.len(), 3, "split at {split}");
            assert!(got[0].is_empty());
            assert_eq!(got[1].event.as_deref(), Some("a"));
            assert_eq!(got[1].data, "{\"t\":\"你好\"}");
            assert_eq!(got[2].data, "x\ny");
            let joined: Vec<u8> = got.iter().flat_map(|f| f.raw.to_vec()).collect();
            assert_eq!(joined, bytes, "raw bytes must round-trip (split at {split})");
        }
    }

    #[test]
    fn splitter_unterminated_tail_and_size_limit() {
        let mut s = SseSplitter::new();
        assert!(s.push(b"data: [DONE]").unwrap().is_empty());
        assert_eq!(s.finish().unwrap().data, "[DONE]");
        let mut s = SseSplitter::new();
        assert!(s.push(b"\n\n\n").unwrap().is_empty());
        assert!(s.finish().is_none());
        let mut s = SseSplitter::new();
        let big = vec![b'a'; MAX_FRAME_BYTES + 1];
        assert_eq!(s.push(&big).unwrap_err(), SseError::FrameTooLarge);
    }

    #[test]
    fn openai_text_reasoning_tools_and_trailing_usage() {
        let chunks = [
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"reasoning_content":"think "}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"reasoning":"more"}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"Hel"}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"content":"lo"}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Paris\"}"}}]}}]}),
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            json!({"id":"c1","model":"m","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":4}}}),
        ];
        let mut dec = OpenAiStreamDecoder::new();
        let events = run(&mut dec, &oa(&chunks, true)).unwrap();
        assert!(dec.is_complete());
        let kinds: Vec<String> = events
            .iter()
            .map(|e| match e {
                StreamEvent::MessageStart { .. } => "start".into(),
                StreamEvent::ContentBlockStart { index, block } => format!("bs{index}:{block:?}"),
                StreamEvent::ContentBlockDelta { index, delta } => format!("d{index}:{delta:?}"),
                StreamEvent::ToolCallDelta { index, partial_json } => format!("t{index}:{partial_json}"),
                StreamEvent::ContentBlockStop { index } => format!("stop{index}"),
                StreamEvent::Usage(u) => format!("usage{:?}/{:?}/{:?}", u.input_tokens, u.output_tokens, u.cache_read_tokens),
                StreamEvent::MessageStop { stop_reason, .. } => format!("end:{stop_reason:?}"),
                StreamEvent::Error { .. } => "error".into(),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "start",
                "bs0:Thinking",
                "d0:Thinking { text: \"think \" }",
                "d0:Thinking { text: \"more\" }",
                "stop0",
                "bs1:Text",
                "d1:Text { text: \"Hel\" }",
                "d1:Text { text: \"lo\" }",
                "stop1",
                "bs2:ToolUse { id: \"call_a\", name: \"get_weather\" }",
                "t2:{\"city\":",
                "t2:\"Paris\"}",
                "stop2",
                "usageSome(11)/Some(7)/Some(4)",
                "end:ToolUse",
            ]
        );
    }

    #[test]
    fn codebuddy_interleaved_reasoning_keeps_text_contiguous() {
        // Every SSE chunk is a token fragment, not a paragraph. Empty
        // tool_calls arrays and stray reasoning chunks must not split text.
        let chunks = [
            json!({"id":"c","model":"m","choices":[{"delta":{"reasoning_content":"planning"}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{"content":"你好","tool_calls":[]}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{"content":"，","reasoning_content":"stale","tool_calls":[]}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{"reasoning_content":"late"}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{"content":"世界","tool_calls":[]}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{"content":"\n下一行"}}]}),
            json!({"id":"c","model":"m","choices":[{"delta":{},"finish_reason":"stop"}]}),
        ];
        let mut decoder = OpenAiStreamDecoder::new();
        let events = run(&mut decoder, &oa(&chunks, true)).unwrap();
        let text_blocks = events.iter().filter(|event| matches!(
            event, StreamEvent::ContentBlockStart { block: BlockStart::Text, .. }
        )).count();
        assert_eq!(text_blocks, 1, "response text must be one content block");
        let text: String = events.iter().filter_map(|event| match event {
            StreamEvent::ContentBlockDelta { delta: BlockDelta::Text { text }, .. } => Some(text.as_str()),
            _ => None,
        }).collect();
        assert_eq!(text, "你好，世界\n下一行");
        assert_eq!(events.iter().filter(|event| matches!(
            event, StreamEvent::ContentBlockStart { block: BlockStart::Thinking, .. }
        )).count(), 1);
        assert!(decoder.is_terminated());
    }

    #[test]
    fn openai_usage_only_chunk_is_flagged() {
        let mut dec = OpenAiStreamDecoder::new();
        let fs = frames(&oa(
            &[
                json!({"id":"c","model":"m","choices":[{"delta":{"content":"a"}}]}),
                json!({"id":"c","model":"m","choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
            ],
            false,
        ));
        dec.push(&fs[0]).unwrap();
        assert!(!dec.last_was_usage_only());
        dec.push(&fs[1]).unwrap();
        assert!(dec.last_was_usage_only());
    }

    #[test]
    fn openai_truncation_and_missing_done() {
        // 没有 finish_reason 也没有 [DONE]：截断
        let mut dec = OpenAiStreamDecoder::new();
        let err = run(&mut dec, &oa(&[json!({"id":"c","model":"m","choices":[{"delta":{"content":"a"}}]})], false)).unwrap_err();
        assert!(matches!(err, ProtocolError::InvalidResponse(_)));
        assert!(!dec.is_complete());
        // 有 finish_reason 但没有 [DONE]：视为完成
        let mut dec = OpenAiStreamDecoder::new();
        let ev = run(
            &mut dec,
            &oa(
                &[
                    json!({"id":"c","model":"m","choices":[{"delta":{"content":"a"}}]}),
                    json!({"id":"c","model":"m","choices":[{"delta":{},"finish_reason":"stop"}]}),
                ],
                false,
            ),
        )
        .unwrap();
        assert!(matches!(ev.last(), Some(StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, .. })));
        // 空流
        let mut dec = OpenAiStreamDecoder::new();
        assert!(run(&mut dec, "").is_err());
        // [DONE] 之前没有任何数据
        let mut dec = OpenAiStreamDecoder::new();
        assert!(run(&mut dec, "data: [DONE]\n\n").is_err());
        // 非法 JSON
        let mut dec = OpenAiStreamDecoder::new();
        assert!(run(&mut dec, "data: {oops\n\n").is_err());
    }

    #[test]
    fn openai_error_frame_becomes_error_event() {
        let mut dec = OpenAiStreamDecoder::new();
        let ev = run(&mut dec, &oa(&[json!({"error":{"message":"slow down","type":"rate_limit_error"}})], false)).unwrap();
        assert_eq!(ev, vec![StreamEvent::Error { kind: ApiErrorKind::RateLimit, message: "slow down".into() }]);
        assert!(dec.is_complete());
    }

    #[test]
    fn openai_tool_call_without_index_or_id_still_works() {
        let mut dec = OpenAiStreamDecoder::new();
        let ev = run(
            &mut dec,
            &oa(
                &[
                    json!({"id":"c","model":"m","choices":[{"delta":{"tool_calls":[{"function":{"name":"f","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),
                ],
                true,
            ),
        )
        .unwrap();
        assert!(ev.iter().any(|e| matches!(e, StreamEvent::ContentBlockStart { block: BlockStart::ToolUse { id, name }, .. } if id.starts_with("call_") && name == "f")));
    }

    fn an(events: &[(&str, Value)]) -> String {
        events.iter().map(|(e, d)| format!("event: {e}\ndata: {d}\n\n")).collect()
    }

    #[test]
    fn anthropic_full_stream() {
        let text = an(&[
            ("message_start", json!({"type":"message_start","message":{"id":"msg_1","model":"claude-x","role":"assistant","content":[],"usage":{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":1}}})),
            ("ping", json!({"type":"ping"})),
            ("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}})),
            ("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}})),
            ("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}})),
            ("content_block_stop", json!({"type":"content_block_stop","index":0})),
            ("content_block_start", json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}})),
            ("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ok"}})),
            ("content_block_stop", json!({"type":"content_block_stop","index":1})),
            ("content_block_start", json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"f","input":{}}})),
            ("content_block_delta", json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"a\":1}"}})),
            ("content_block_stop", json!({"type":"content_block_stop","index":2})),
            ("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}})),
            ("message_stop", json!({"type":"message_stop"})),
        ]);
        let mut dec = AnthropicStreamDecoder::new();
        let ev = run(&mut dec, &text).unwrap();
        assert!(dec.is_complete());
        assert!(matches!(&ev[0], StreamEvent::MessageStart { id, usage: Some(u), .. } if id == "msg_1" && u.input_tokens == Some(15) && u.cache_read_tokens == Some(5)));
        let mut tally = StreamTally::default();
        ev.iter().for_each(|e| tally.observe(e));
        assert_eq!(tally.usage.input_tokens, Some(15));
        assert_eq!(tally.usage.output_tokens, Some(42));
        assert_eq!(tally.output_bytes, "hm".len() + "ok".len() + "{\"a\":1}".len());
        assert!(matches!(ev.last(), Some(StreamEvent::MessageStop { stop_reason: StopReason::ToolUse, .. })));
        assert!(ev.contains(&StreamEvent::ToolCallDelta { index: 2, partial_json: "{\"a\":1}".into() }));
    }

    #[test]
    fn anthropic_truncation_error_and_unsupported_block() {
        let mut dec = AnthropicStreamDecoder::new();
        let text = an(&[("message_start", json!({"type":"message_start","message":{"id":"m","model":"x"}}))]);
        assert!(run(&mut dec, &text).is_err());

        let mut dec = AnthropicStreamDecoder::new();
        let ev = run(&mut dec, &an(&[("error", json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}))])).unwrap();
        assert!(matches!(&ev[0], StreamEvent::Error { message, .. } if message == "busy"));

        let mut dec = AnthropicStreamDecoder::new();
        let text = an(&[
            ("message_start", json!({"type":"message_start","message":{"id":"m","model":"x"}})),
            ("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"s","name":"web_search"}})),
        ]);
        assert!(matches!(run(&mut dec, &text), Err(ProtocolError::Unsupported(_))));
    }
}

//! 流式转发：首帧探测（决定能否回退）+ 逐块转发的拉取式响应流。
//!
//! - [`prime`]：在向客户端写出任何字节之前读到上游的第一个数据帧。读不到（断开 / 超时 / 立即 EOF）或首帧是
//!   带错误的 200，都按失败处理，由路由层回退到下一个账号。
//! - [`Pump`]：响应体流。它是**拉取式**的——只有 hyper 准备好写下一块时才会去读上游，
//!   因此客户端慢时上游读取随之变慢，内存里最多缓冲一个上游块解出的帧（背压，无无限缓冲）。
//!   客户端断开时 hyper 丢弃响应体，`Pump` 随之被 drop：上游连接关闭，用量记为 `cancelled`。
//! - 两种模式：透传（逐帧原样转发，同时旁路解码仅用于记账 / 完整性判断）与转换（上游帧 → 规范化事件 →
//!   客户端协议的流编码器）。
//! - 上游流的分帧：SSE（Chat / Anthropic / Responses / Gemini / Vertex）或 NDJSON（Ollama），见 [`FrameSplitter`]。
//! - 客户端流的线上格式：SSE、NDJSON（Ollama）或 JSON 数组（Gemini 不带 `alt=sse`），见 [`ClientWire`]。
//! - 上游中途出错 / 断流 / 空闲超时：向客户端发协议风格的错误帧并把请求记为失败，绝不静默结束。

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::OwnedSemaphorePermit;

use super::convert::{scrub, stream_error};
use crate::models::UsageStatus;
use crate::protocols::canonical::StreamEvent;
use crate::protocols::gemini::{GeminiStreamDecoder, ToolNameMap};
use crate::protocols::ollama::{OllamaStreamDecoder, OllamaStreamEncoder};
use crate::protocols::openai_responses::ResponsesStreamDecoder;
use crate::protocols::{ApiErrorKind, ProtocolKind, ProtocolResult, SseFrame, StreamEncoder};
use crate::routing::Runtime;
use crate::upstream::sse_decode::{
    AnthropicStreamDecoder, BytesStreamDecoder, FrameSplitter, OpenAiStreamDecoder, RawFrame, StreamDecoder, StreamTally,
};
use crate::upstream::{AdapterKind, StreamFraming, UpstreamError, UpstreamReply};
use crate::usage::recorder::UsageGuard;

pub(crate) type UpstreamBody = BoxStream<'static, Result<Bytes, UpstreamError>>;

/// `names` 是编码请求时得到的 Gemini / Vertex 工具名映射（其它上游忽略）。
pub(crate) fn new_decoder(kind: AdapterKind, names: ToolNameMap) -> Box<dyn StreamDecoder> {
    match kind {
        AdapterKind::OpenAiCompatible => Box::new(OpenAiStreamDecoder::new()),
        AdapterKind::AnthropicCompatible => Box::new(AnthropicStreamDecoder::new()),
        AdapterKind::OpenAiResponses => Box::new(BytesStreamDecoder::new(ResponsesStreamDecoder::new())),
        AdapterKind::Gemini | AdapterKind::CodeAssist | AdapterKind::Vertex => Box::new(BytesStreamDecoder::new(GeminiStreamDecoder::with_name_map(names))),
        AdapterKind::Ollama => Box::new(BytesStreamDecoder::new(OllamaStreamDecoder::new())),
    }
}

// ───────────────────────── 客户端流的线上格式 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireFormat {
    /// `text/event-stream`
    Sse,
    /// `application/x-ndjson`（Ollama）
    Ndjson,
    /// 一个 JSON 数组，元素陆续写出（Gemini `streamGenerateContent` 不带 `alt=sse`）。
    JsonArray,
}

impl WireFormat {
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            WireFormat::Sse => "text/event-stream",
            WireFormat::Ndjson => "application/x-ndjson",
            WireFormat::JsonArray => "application/json",
        }
    }

    /// 客户端协议与请求参数决定的格式。
    pub(crate) fn for_client(protocol: ProtocolKind, gemini_sse: bool) -> Self {
        match protocol {
            ProtocolKind::Ollama => WireFormat::Ndjson,
            ProtocolKind::Gemini if !gemini_sse => WireFormat::JsonArray,
            _ => WireFormat::Sse,
        }
    }
}

/// 把协议层产出的 [`SseFrame`]（`data` 是一段 JSON）渲染成客户端协议的线上字节。
pub(crate) struct ClientWire {
    format: WireFormat,
    array_open: bool,
    array_closed: bool,
}

impl ClientWire {
    pub(crate) fn new(format: WireFormat) -> Self {
        Self { format, array_open: false, array_closed: false }
    }

    pub(crate) fn format(&self) -> WireFormat {
        self.format
    }

    pub(crate) fn render(&mut self, frame: &SseFrame) -> Vec<Bytes> {
        match self.format {
            WireFormat::Sse => vec![Bytes::from(frame.to_wire())],
            WireFormat::Ndjson => vec![Bytes::from(format!("{}\n", frame.data))],
            WireFormat::JsonArray => {
                if self.array_closed {
                    return vec![];
                }
                let prefix = if self.array_open { ",\n" } else { "[" };
                self.array_open = true;
                vec![Bytes::from(format!("{prefix}{}", frame.data))]
            }
        }
    }

    /// 流结束：JSON 数组需要补上 `]`。其它格式没有结束标记。
    pub(crate) fn close(&mut self) -> Vec<Bytes> {
        if self.format != WireFormat::JsonArray || self.array_closed {
            return vec![];
        }
        self.array_closed = true;
        vec![Bytes::from(if self.array_open { "\n]" } else { "[]" })]
    }
}

/// Ollama 的流编码器吐出的是 NDJSON 行；包成 `SseFrame`（只用 `data`）以便与其它编码器共用一条管线。
struct OllamaFrameEncoder(OllamaStreamEncoder);

impl StreamEncoder for OllamaFrameEncoder {
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>> {
        Ok(self.0.push(event)?.into_iter().map(|l| SseFrame::data(l.trim_end_matches('\n'))).collect())
    }
}

/// 转换模式下，面向客户端协议的流编码器。
pub(crate) fn client_encoder(protocol: ProtocolKind, client_wants_usage: bool) -> Box<dyn StreamEncoder + Send> {
    use crate::protocols::anthropic::AnthropicStreamEncoder;
    use crate::protocols::gemini::GeminiStreamEncoder;
    use crate::protocols::openai_chat::OpenAiStreamEncoder;
    use crate::protocols::openai_responses::ResponsesStreamEncoder;
    match protocol {
        ProtocolKind::AnthropicMessages => Box::new(AnthropicStreamEncoder::new()),
        ProtocolKind::OpenAiResponses => Box::new(ResponsesStreamEncoder::new()),
        ProtocolKind::Gemini | ProtocolKind::Vertex => Box::new(GeminiStreamEncoder::new()),
        ProtocolKind::Ollama => Box::new(OllamaFrameEncoder(OllamaStreamEncoder::new())),
        ProtocolKind::OpenAiChat => Box::new(OpenAiStreamEncoder::new(client_wants_usage)),
    }
}

/// 已读到首个数据帧的上游流。
pub(crate) struct Primed {
    pub body: UpstreamBody,
    pub splitter: FrameSplitter,
    /// 探测期间已读出的帧（含首帧之前的注释帧），按顺序，尚未向客户端写出。
    pub frames: VecDeque<RawFrame>,
}

pub(crate) enum PrimeOutcome {
    Ready(Primed),
    /// 200 里的首帧是错误：映射成合成的非 2xx 回复，走与 HTTP 错误相同的分类。
    InBandError(UpstreamReply),
    Failed(UpstreamError),
}

/// 错误对象 → 合成 HTTP 状态。
fn status_from_error_object(err: &Value) -> u16 {
    // Gemini：`error.code` 就是 HTTP 状态
    if let Some(c) = err.get("code").and_then(Value::as_u64).filter(|c| (400..600).contains(c)) {
        return c as u16;
    }
    let hay = ["type", "code", "status"]
        .iter()
        .filter_map(|k| err.get(*k))
        .map(|x| x.to_string().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    if hay.contains("rate") || hay.contains("429") || hay.contains("quota") || hay.contains("exhausted") {
        429
    } else if hay.contains("auth") || hay.contains("api_key") || hay.contains("401") {
        401
    } else if hay.contains("permission") || hay.contains("403") {
        403
    } else if hay.contains("invalid_request") || hay.contains("invalid_argument") || hay.contains("400") {
        400
    } else if hay.contains("overload") {
        529
    } else {
        502
    }
}

/// Ollama 的错误是一段纯文本。
fn status_from_error_text(text: &str) -> u16 {
    let t = text.to_ascii_lowercase();
    if t.contains("rate limit") || t.contains("too many requests") || t.contains("429") {
        429
    } else if t.contains("unauthorized") || t.contains("api key") || t.contains("401") {
        401
    } else if t.contains("forbidden") || t.contains("403") {
        403
    } else if t.contains("not found") {
        404
    } else {
        502
    }
}

/// 上游 200 + 首帧错误 → 合成的 `(HTTP 状态, 错误体 JSON 文本)`。
fn inband_error(kind: AdapterKind, frame: &RawFrame) -> Option<(u16, String)> {
    let v: Value = serde_json::from_str(frame.data.trim()).ok()?;
    match kind {
        AdapterKind::OpenAiCompatible => {
            if !(v.get("error").is_some_and(|e| !e.is_null()) && v.get("choices").is_none()) {
                return None;
            }
            Some((status_from_error_object(v.get("error").unwrap_or(&v)), frame.data.clone()))
        }
        AdapterKind::AnthropicCompatible => {
            if !(frame.event.as_deref() == Some("error") || v.get("type").and_then(Value::as_str) == Some("error")) {
                return None;
            }
            Some((status_from_error_object(v.get("error").unwrap_or(&v)), frame.data.clone()))
        }
        AdapterKind::OpenAiResponses => {
            let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
            if frame.event.as_deref() == Some("error") || ty == "error" {
                return Some((status_from_error_object(&v), frame.data.clone()));
            }
            if ty == "response.failed" {
                let err = v.get("response").and_then(|r| r.get("error")).cloned().unwrap_or(Value::Null);
                let body = if err.is_object() { err.to_string() } else { frame.data.clone() };
                return Some((status_from_error_object(&err), body));
            }
            None
        }
        AdapterKind::Gemini | AdapterKind::CodeAssist | AdapterKind::Vertex => {
            let e = v.get("error").filter(|e| e.is_object())?;
            Some((status_from_error_object(e), frame.data.clone()))
        }
        AdapterKind::Ollama => {
            let e = v.get("error").filter(|e| !e.is_null())?;
            let status = match e {
                Value::String(t) => status_from_error_text(t),
                other => status_from_error_object(other),
            };
            Some((status, frame.data.clone()))
        }
    }
}

pub(crate) async fn prime(mut body: UpstreamBody, kind: AdapterKind, idle: Duration) -> PrimeOutcome {
    let mut splitter = FrameSplitter::new(kind.framing());
    let mut frames: VecDeque<RawFrame> = VecDeque::new();
    loop {
        match tokio::time::timeout(idle, body.next()).await {
            Err(_) => return PrimeOutcome::Failed(UpstreamError::Timeout),
            Ok(Some(Err(e))) => return PrimeOutcome::Failed(e),
            Ok(Some(Ok(chunk))) => match splitter.push(&chunk) {
                Err(e) => return PrimeOutcome::Failed(UpstreamError::Malformed(e.to_string())),
                Ok(fs) => frames.extend(fs),
            },
            Ok(None) => {
                frames.extend(splitter.finish());
                if frames.iter().all(RawFrame::is_empty) {
                    return PrimeOutcome::Failed(UpstreamError::Malformed("the upstream closed the stream before sending any data".into()));
                }
                return PrimeOutcome::Ready(Primed { body, splitter, frames });
            }
        }
        if let Some(first) = frames.iter().find(|f| !f.is_empty()) {
            if let Some((status, error_body)) = inband_error(kind, first) {
                return PrimeOutcome::InBandError(UpstreamReply {
                    status,
                    retry_after: None,
                    content_type: Some("application/json".into()),
                    body: Bytes::from(error_body),
                });
            }
            return PrimeOutcome::Ready(Primed { body, splitter, frames });
        }
    }
}

pub(crate) struct PumpConfig {
    pub runtime: Arc<Runtime>,
    pub client_protocol: ProtocolKind,
    pub upstream_kind: AdapterKind,
    /// `Some` = 转换模式；`None` = 透传。
    pub encoder: Option<Box<dyn StreamEncoder + Send>>,
    /// 客户端流的线上格式。
    pub wire: WireFormat,
    /// Gemini / Vertex 上游的工具名映射，贯穿整个流。
    pub tool_names: ToolNameMap,
    /// 透传时：上游多发的（我们自己要求的）usage-only chunk 不转给没有要求它的客户端。
    pub drop_injected_usage_chunk: bool,
    pub connection_id: String,
    pub secrets: Vec<String>,
    pub idle_timeout: Duration,
    /// 随响应流一起存活，流结束 / 被丢弃时释放并发名额。
    pub _permit: OwnedSemaphorePermit,
}

/// Mirrors 9Router's OpenAI SSE passthrough normalization. Some compatible
/// providers (CodeBuddy CN included) send `tool_calls: []` with every token.
/// AI SDKs may interpret the presence of the field as the beginning/end of a
/// tool or reasoning segment, splitting contiguous text into separate blocks.
/// Do not alter actual text deltas, nonempty tool calls, or other protocols.
fn normalize_openai_passthrough(frame: &RawFrame) -> Option<Bytes> {
    let mut data: Value = serde_json::from_str(&frame.data).ok()?;
    let choices = data.get_mut("choices")?.as_array_mut()?;
    let mut changed = false;
    for choice in choices {
        let Some(delta) = choice.get_mut("delta").and_then(Value::as_object_mut) else { continue };
        if matches!(delta.get("tool_calls"), Some(Value::Array(calls)) if calls.is_empty()) {
            delta.remove("tool_calls");
            changed = true;
        }
    }
    if !changed { return None; }
    Some(Bytes::from(SseFrame { event: frame.event.clone(), data: data.to_string() }.to_wire()))
}

pub(crate) struct Pump {
    body: UpstreamBody,
    splitter: FrameSplitter,
    pending_frames: VecDeque<RawFrame>,
    out: VecDeque<Bytes>,
    decoder: Box<dyn StreamDecoder>,
    wire: ClientWire,
    /// 透传模式下旁路解码一旦出错就停用（不影响转发）。
    observer_ok: bool,
    cfg: PumpConfig,
    tally: StreamTally,
    finished: bool,
    upstream_eof: bool,
    guard: Option<UsageGuard>,
}

impl Drop for Pump {
    /// 响应流在结束前被丢弃 = 客户端断开：记为取消，并带上已累计的用量 / 输出量（用于估算）。
    fn drop(&mut self) {
        if let Some(g) = self.guard.take() {
            g.finish(UsageStatus::Cancelled, &self.tally.usage, self.tally.output_bytes);
        }
    }
}

impl Pump {
    pub fn new(primed: Primed, mut cfg: PumpConfig, guard: UsageGuard) -> Self {
        let names = std::mem::take(&mut cfg.tool_names);
        Self {
            body: primed.body,
            splitter: primed.splitter,
            pending_frames: primed.frames,
            out: VecDeque::new(),
            decoder: new_decoder(cfg.upstream_kind, names),
            wire: ClientWire::new(cfg.wire),
            observer_ok: true,
            tally: StreamTally::default(),
            finished: false,
            upstream_eof: false,
            guard: Some(guard),
            cfg,
        }
    }

    pub fn into_stream(self) -> impl futures_util::Stream<Item = Result<Bytes, Infallible>> + Send + 'static {
        futures_util::stream::unfold(self, |mut st| async move { st.next_chunk().await.map(|b| (Ok(b), st)) })
    }

    async fn next_chunk(&mut self) -> Option<Bytes> {
        loop {
            if let Some(b) = self.out.pop_front() {
                return Some(b);
            }
            if self.finished {
                return None;
            }
            if let Some(frame) = self.pending_frames.pop_front() {
                self.handle_frame(frame);
                continue;
            }
            if self.upstream_eof {
                self.handle_eof();
                continue;
            }
            match tokio::time::timeout(self.cfg.idle_timeout, self.body.next()).await {
                Err(_) => self.fail(ApiErrorKind::Timeout, "The upstream provider stopped sending data."),
                Ok(Some(Err(_))) => {
                    self.fail(ApiErrorKind::BadGateway, "The connection to the upstream provider was interrupted.")
                }
                Ok(Some(Ok(chunk))) => match self.splitter.push(&chunk) {
                    Ok(frames) => self.pending_frames.extend(frames),
                    Err(_) => self.fail(ApiErrorKind::BadGateway, "The upstream provider sent an oversized stream frame."),
                },
                Ok(None) => self.upstream_eof = true,
            }
        }
    }

    fn write_frames(&mut self, frames: Vec<SseFrame>) {
        for f in frames {
            self.out.extend(self.wire.render(&f));
        }
    }

    /// 向客户端发协议风格的错误帧并结束；请求记为失败，账号软降级。
    fn fail(&mut self, kind: ApiErrorKind, message: &str) {
        if self.finished {
            return;
        }
        let message = scrub(message, &self.cfg.secrets);
        let frame = stream_error(kind, &message).to_sse_frame(self.cfg.client_protocol);
        self.write_frames(vec![frame]);
        self.cfg.runtime.health().record_transient(&self.cfg.connection_id);
        tracing::warn!(connection = %self.cfg.connection_id, kind = ?kind, "upstream stream failed after it had started");
        self.complete(UsageStatus::Error);
    }

    fn complete(&mut self, status: UsageStatus) {
        self.finished = true;
        let tail = self.wire.close();
        self.out.extend(tail);
        if let Some(g) = self.guard.take() {
            g.finish(status, &self.tally.usage, self.tally.output_bytes);
        }
    }

    fn handle_frame(&mut self, frame: RawFrame) {
        if self.cfg.encoder.is_none() {
            self.handle_passthrough(frame);
        } else {
            self.handle_converted(frame);
        }
    }

    /// 透传一帧。上游分帧方式与客户端线上格式一致时原样写出原始字节（可选地先擦除密钥）；
    /// 不一致（Gemini 上游恒为 SSE，客户端可能要 JSON 数组）时只重新包装，内容不变。
    fn emit_raw(&mut self, frame: &RawFrame, scrub_secrets: bool) {
        let same_shape = matches!(
            (self.cfg.upstream_kind.framing(), self.wire.format()),
            (StreamFraming::Sse, WireFormat::Sse) | (StreamFraming::Ndjson, WireFormat::Ndjson)
        );
        if same_shape {
            if !scrub_secrets
                && self.cfg.upstream_kind == AdapterKind::OpenAiCompatible
                && self.cfg.client_protocol == ProtocolKind::OpenAiChat
            {
                if let Some(normalized) = normalize_openai_passthrough(frame) {
                    self.out.push_back(normalized);
                    return;
                }
            }
            if scrub_secrets {
                let raw = scrub(&String::from_utf8_lossy(&frame.raw), &self.cfg.secrets);
                self.out.push_back(Bytes::from(raw));
            } else {
                self.out.push_back(frame.raw.clone());
            }
        } else {
            let data = if scrub_secrets { scrub(&frame.data, &self.cfg.secrets) } else { frame.data.clone() };
            self.write_frames(vec![SseFrame { event: frame.event.clone(), data }]);
        }
    }

    fn handle_passthrough(&mut self, frame: RawFrame) {
        let mut terminated_error: Option<(ApiErrorKind, String)> = None;
        if self.observer_ok {
            match self.decoder.push(&frame) {
                Ok(events) => {
                    for ev in &events {
                        self.tally.observe(ev);
                        if let StreamEvent::Error { kind, message } = ev {
                            terminated_error = Some((*kind, message.clone()));
                        }
                    }
                }
                Err(_) => self.observer_ok = false,
            }
        }
        let skip = self.cfg.drop_injected_usage_chunk && self.observer_ok && self.decoder.last_was_usage_only();
        if let Some((kind, _)) = terminated_error {
            // 上游自己发了错误帧：转给客户端（擦除我们发出的密钥），记为失败并结束
            self.emit_raw(&frame, true);
            tracing::warn!(connection = %self.cfg.connection_id, kind = ?kind, "upstream reported an in-stream error");
            self.cfg.runtime.health().record_transient(&self.cfg.connection_id);
            self.complete(UsageStatus::Error);
            return;
        }
        if !skip {
            self.emit_raw(&frame, false);
        }
        if self.observer_ok && self.decoder.is_terminated() {
            self.complete(UsageStatus::Success);
        }
    }

    fn handle_converted(&mut self, frame: RawFrame) {
        match self.decoder.push(&frame) {
            Err(_) => self.fail(ApiErrorKind::BadGateway, "The upstream stream could not be converted to the requested protocol."),
            Ok(events) => self.apply_events(events),
        }
    }

    fn apply_events(&mut self, events: Vec<StreamEvent>) {
        for ev in events {
            self.tally.observe(&ev);
            if let StreamEvent::Error { kind, message } = &ev {
                let (kind, message) = (*kind, message.clone());
                self.fail(kind, &message);
                return;
            }
            let stop = matches!(ev, StreamEvent::MessageStop { .. });
            let Some(enc) = self.cfg.encoder.as_mut() else { return };
            match enc.push(ev) {
                Ok(frames) => self.write_frames(frames),
                Err(_) => {
                    self.fail(ApiErrorKind::BadGateway, "The upstream stream could not be converted to the requested protocol.");
                    return;
                }
            }
            if stop {
                self.complete(UsageStatus::Success);
                return;
            }
        }
    }

    fn handle_eof(&mut self) {
        if let Some(last) = self.splitter.finish() {
            self.pending_frames.push_back(last);
            self.upstream_eof = true;
            // 先处理残留帧；再次进入 handle_eof 时 splitter 已空
            return;
        }
        if self.cfg.encoder.is_none() {
            if self.observer_ok {
                match self.decoder.finish() {
                    Ok(events) => {
                        events.iter().for_each(|e| self.tally.observe(e));
                        // Responses / Gemini / Ollama 的解码器在流被截断时不返回 Err，而是产生 `Error` 事件
                        if let Some(StreamEvent::Error { kind, message }) =
                            events.iter().find(|e| matches!(e, StreamEvent::Error { .. }))
                        {
                            let (kind, message) = (*kind, message.clone());
                            self.fail(kind, &message);
                        } else {
                            self.complete(UsageStatus::Success);
                        }
                    }
                    Err(_) => self.fail(ApiErrorKind::BadGateway, "The upstream provider closed the stream before it completed."),
                }
            } else {
                // 旁路解码已停用，无法判断完整性：按已转发的内容处理
                self.complete(UsageStatus::Success);
            }
            return;
        }
        match self.decoder.finish() {
            Ok(events) => {
                self.apply_events(events);
                if !self.finished {
                    self.complete(UsageStatus::Success);
                }
            }
            Err(_) => self.fail(ApiErrorKind::BadGateway, "The upstream provider closed the stream before it completed."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_array_wire_is_a_valid_array_however_it_ends() {
        let mut w = ClientWire::new(WireFormat::JsonArray);
        let mut out = String::new();
        for f in [SseFrame::data(json!({"a": 1}).to_string()), SseFrame::data(json!({"b": 2}).to_string())] {
            out.extend(w.render(&f).iter().map(|b| String::from_utf8_lossy(b).into_owned()));
        }
        out.extend(w.close().iter().map(|b| String::from_utf8_lossy(b).into_owned()));
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!([{"a": 1}, {"b": 2}]), "{out}");
        // 关闭后不再写入；重复关闭无副作用
        assert!(w.render(&SseFrame::data("{}")).is_empty() && w.close().is_empty());
        // 没有任何元素时仍是合法数组
        assert_eq!(String::from_utf8_lossy(&ClientWire::new(WireFormat::JsonArray).close()[0]), "[]");
    }

    #[test]
    fn sse_and_ndjson_wires() {
        let f = SseFrame::event("e", "{\"x\":1}");
        assert_eq!(&ClientWire::new(WireFormat::Sse).render(&f)[0][..], b"event: e\ndata: {\"x\":1}\n\n");
        assert_eq!(&ClientWire::new(WireFormat::Ndjson).render(&f)[0][..], b"{\"x\":1}\n");
        assert!(ClientWire::new(WireFormat::Ndjson).close().is_empty());
        assert_eq!(WireFormat::for_client(ProtocolKind::Gemini, true), WireFormat::Sse);
        assert_eq!(WireFormat::for_client(ProtocolKind::Gemini, false), WireFormat::JsonArray);
        assert_eq!(WireFormat::for_client(ProtocolKind::Ollama, false), WireFormat::Ndjson);
        assert_eq!(WireFormat::for_client(ProtocolKind::OpenAiResponses, false), WireFormat::Sse);
    }

    #[test]
    fn openai_passthrough_removes_empty_tool_calls_without_adding_line_breaks() {
        use crate::protocols::sse::parse_sse;
        let text = "你好，世界".to_owned() + "\n" + "真实换行";
        let delta = json!({"id":"a","choices":[{"index":0,"delta":{
            "content": text, "reasoning_content":"thought", "tool_calls":[]
        }}]});
        let frame = RawFrame { raw: Bytes::new(), event: None, data: delta.to_string() };
        let corrected = normalize_openai_passthrough(&frame).expect("empty tool calls must be stripped");
        let encoded = String::from_utf8(corrected.to_vec()).unwrap();
        let parsed = parse_sse(&encoded);
        assert_eq!(parsed.len(), 1);
        let output: Value = serde_json::from_str(&parsed[0].data).unwrap();
        assert!(output["choices"][0]["delta"].get("tool_calls").is_none());
        assert_eq!(output["choices"][0]["delta"]["content"], text);
        assert_eq!(output["choices"][0]["delta"]["reasoning_content"], "thought");
        assert_eq!(encoded.matches("data: ").count(), 1, "one incoming SSE event must stay one event");
    }

    #[test]
    fn openai_passthrough_preserves_real_tool_calls_and_regular_frames() {
        let actual_tool = RawFrame { raw: Bytes::new(), event: None,
            data: json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"read"}}]}}]}).to_string() };
        assert!(normalize_openai_passthrough(&actual_tool).is_none());
        let text = RawFrame { raw: Bytes::new(), event: None,
            data: json!({"choices":[{"delta":{"content":"Hello"}}]}).to_string() };
        assert!(normalize_openai_passthrough(&text).is_none());
        let done = RawFrame { raw: Bytes::from_static(b"data: [DONE]\n\n"), event: None,
            data: "[DONE]".into() };
        assert!(normalize_openai_passthrough(&done).is_none());
    }

    #[test]
    fn inband_errors_per_upstream_kind() {
        let frame = |event: Option<&str>, data: Value| RawFrame { raw: Bytes::new(), event: event.map(str::to_string), data: data.to_string() };
        // Gemini：error.code 就是状态码
        let g = frame(None, json!({"error": {"code": 429, "message": "quota", "status": "RESOURCE_EXHAUSTED"}}));
        assert_eq!(inband_error(AdapterKind::Gemini, &g).unwrap().0, 429);
        assert_eq!(inband_error(AdapterKind::Vertex, &frame(None, json!({"error": {"status": "UNAUTHENTICATED"}}))).unwrap().0, 401);
        assert!(inband_error(AdapterKind::Gemini, &frame(None, json!({"candidates": []}))).is_none());
        // Ollama：纯文本错误
        assert_eq!(inband_error(AdapterKind::Ollama, &frame(None, json!({"error": "model 'x' not found"}))).unwrap().0, 404);
        assert_eq!(inband_error(AdapterKind::Ollama, &frame(None, json!({"error": "server busy"}))).unwrap().0, 502);
        assert!(inband_error(AdapterKind::Ollama, &frame(None, json!({"message": {"content": "generate"}, "done": false}))).is_none());
        // Responses：error 事件与 response.failed
        let r = frame(Some("error"), json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow"}));
        assert_eq!(inband_error(AdapterKind::OpenAiResponses, &r).unwrap().0, 429);
        let failed = frame(Some("response.failed"), json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "x"}}}));
        let (status, body) = inband_error(AdapterKind::OpenAiResponses, &failed).unwrap();
        assert_eq!(status, 502);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["message"], "x");
        let ok = frame(Some("response.created"), json!({"type": "response.created", "response": {}}));
        assert!(inband_error(AdapterKind::OpenAiResponses, &ok).is_none());
    }
}

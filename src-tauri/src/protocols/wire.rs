//! 上游流式字节的增量解析（SSE / NDJSON / JSON 数组）与错误状态码归类。
//!
//! 供 Responses / Gemini / Ollama 的「上游流 → StreamEvent」解码器共用。所有解析器都能处理
//! 半包：任意位置切分的字节块（包括切在多字节 UTF-8 字符中间）都不会丢数据或产生乱码。

use serde_json::Value;

use super::common::invalid_resp;
use super::{ApiErrorKind, ProtocolResult, SseFrame};

/// HTTP 状态码 → 错误类别（用于解码上游错误体）。
pub(crate) fn kind_from_status(status: u16) -> ApiErrorKind {
    match status {
        400 | 422 => ApiErrorKind::InvalidRequest,
        401 => ApiErrorKind::Authentication,
        403 => ApiErrorKind::PermissionDenied,
        404 => ApiErrorKind::NotFound,
        405 => ApiErrorKind::MethodNotAllowed,
        408 | 504 => ApiErrorKind::Timeout,
        413 => ApiErrorKind::PayloadTooLarge,
        429 => ApiErrorKind::RateLimit,
        501 => ApiErrorKind::NotImplemented,
        502 | 503 | 529 => ApiErrorKind::BadGateway,
        s if (400..500).contains(&s) => ApiErrorKind::InvalidRequest,
        _ => ApiErrorKind::Internal,
    }
}

/// 数值字段：接受 JSON 数字或数字字符串（protobuf JSON 会把 int64 序列化成字符串）。
pub(crate) fn as_u32(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

/// 按字节切行（`\n`，容忍 `\r\n`）。未结束的半行保留到下次 `push`。
#[derive(Debug, Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                let mut line = std::mem::take(&mut self.buf);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                out.push(String::from_utf8_lossy(&line).into_owned());
            } else {
                self.buf.push(b);
            }
        }
        out
    }

    /// 流结束时取出残留的半行。
    pub fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let mut line = std::mem::take(&mut self.buf);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }
}

/// 增量 SSE 解析器：`push` 字节块，得到已完整的帧。
#[derive(Debug, Default)]
pub struct SseParser {
    lines: LineSplitter,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    fn feed_line(&mut self, line: &str, out: &mut Vec<SseFrame>) {
        if line.is_empty() {
            if !self.data.is_empty() || self.event.is_some() {
                out.push(SseFrame { event: self.event.take(), data: self.data.join("\n") });
                self.data.clear();
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for line in self.lines.push(bytes) {
            self.feed_line(&line, &mut out);
        }
        out
    }

    /// 流结束：把残留的半行与未以空行结束的帧也交出来（上游有时不写最后的空行）。
    pub fn finish(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if let Some(line) = self.lines.finish() {
            self.feed_line(&line, &mut out);
        }
        self.feed_line("", &mut out);
        out
    }
}

/// 增量 NDJSON 解析器（Ollama）。空行跳过；坏行返回 `InvalidResponse`。
#[derive(Debug, Default)]
pub struct NdjsonParser {
    lines: LineSplitter,
}

impl NdjsonParser {
    fn parse(line: &str) -> ProtocolResult<Option<Value>> {
        let t = line.trim();
        if t.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(t)
            .map(Some)
            .map_err(|e| invalid_resp(format!("malformed NDJSON line in upstream stream: {e}")))
    }

    pub fn push(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<Value>> {
        let mut out = Vec::new();
        for line in self.lines.push(bytes) {
            if let Some(v) = Self::parse(&line)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> ProtocolResult<Vec<Value>> {
        match self.lines.finish() {
            Some(line) => Ok(Self::parse(&line)?.into_iter().collect()),
            None => Ok(vec![]),
        }
    }
}

/// 增量解析「顶层 JSON 数组」流（`[{...},\n{...}]`）——Gemini / Vertex 不带 `alt=sse` 时的流式格式。
/// 只在数组元素粒度上切分，不关心元素内部结构。
#[derive(Debug, Default)]
pub struct JsonArrayStream {
    started: bool,
    done: bool,
    depth: usize,
    in_string: bool,
    escape: bool,
    cur: Vec<u8>,
}

impl JsonArrayStream {
    pub fn push(&mut self, bytes: &[u8]) -> ProtocolResult<Vec<Value>> {
        let mut out = Vec::new();
        for &b in bytes {
            if self.done {
                if !b.is_ascii_whitespace() {
                    return Err(invalid_resp("unexpected data after the end of the upstream JSON array"));
                }
                continue;
            }
            if !self.started {
                if b.is_ascii_whitespace() || b == 0xEF || b == 0xBB || b == 0xBF {
                    continue; // 空白与可能的 UTF-8 BOM
                }
                if b != b'[' {
                    return Err(invalid_resp("upstream stream is not a JSON array"));
                }
                self.started = true;
                continue;
            }
            if self.depth == 0 {
                match b {
                    b'{' | b'[' => {
                        self.depth = 1;
                        self.cur.clear();
                        self.cur.push(b);
                    }
                    b']' => self.done = true,
                    b',' => {}
                    _ if b.is_ascii_whitespace() => {}
                    _ => return Err(invalid_resp("unexpected byte between elements of the upstream JSON array")),
                }
                continue;
            }
            self.cur.push(b);
            if self.in_string {
                if self.escape {
                    self.escape = false;
                } else if b == b'\\' {
                    self.escape = true;
                } else if b == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match b {
                b'"' => self.in_string = true,
                b'{' | b'[' => self.depth += 1,
                b'}' | b']' => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        let v = serde_json::from_slice(&self.cur)
                            .map_err(|e| invalid_resp(format!("malformed element in upstream JSON array: {e}")))?;
                        self.cur.clear();
                        out.push(v);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// 流是否在数组中途被截断。
    pub fn is_truncated(&self) -> bool {
        self.started && !self.done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_and_utf8() {
        let wire = "event: a\ndata: {\"t\":\"你好\"}\n\ndata: second\r\n\r\n: comment\ndata: tail";
        let bytes = wire.as_bytes();
        // 逐字节喂入，覆盖切在多字节字符内部的情况
        let mut p = SseParser::default();
        let mut frames = Vec::new();
        for b in bytes {
            frames.extend(p.push(std::slice::from_ref(b)));
        }
        frames.extend(p.finish());
        assert_eq!(
            frames,
            vec![SseFrame::event("a", "{\"t\":\"你好\"}"), SseFrame::data("second"), SseFrame::data("tail"),]
        );
    }

    #[test]
    fn ndjson_parser_buffers_partial_lines() {
        let mut p = NdjsonParser::default();
        assert!(p.push(b"{\"a\":1}\n{\"b\"").unwrap().len() == 1);
        let rest = p.push(b":2}\n\n").unwrap();
        assert_eq!(rest, vec![serde_json::json!({"b": 2})]);
        assert!(p.push(b"{oops\n").is_err());
        assert_eq!(NdjsonParser::default().finish().unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn json_array_stream_splits_elements_with_tricky_strings() {
        let wire = "[{\"a\":\"}]\\\"{\"},\n{\"b\":[1,{\"c\":2}]}\n]";
        let mut p = JsonArrayStream::default();
        let mut out = Vec::new();
        for chunk in wire.as_bytes().chunks(3) {
            out.extend(p.push(chunk).unwrap());
        }
        assert_eq!(out, vec![serde_json::json!({"a": "}]\"{"}), serde_json::json!({"b": [1, {"c": 2}]})]);
        assert!(!p.is_truncated());
        let mut q = JsonArrayStream::default();
        q.push(b"[{\"a\":1},{\"b\"").unwrap();
        assert!(q.is_truncated());
        assert!(JsonArrayStream::default().push(b"{\"a\":1}").is_err());
    }

    #[test]
    fn status_mapping() {
        assert_eq!(kind_from_status(429), ApiErrorKind::RateLimit);
        assert_eq!(kind_from_status(503), ApiErrorKind::BadGateway);
        assert_eq!(kind_from_status(418), ApiErrorKind::InvalidRequest);
        assert_eq!(kind_from_status(500), ApiErrorKind::Internal);
    }

    #[test]
    fn numeric_strings_are_accepted() {
        assert_eq!(as_u32(&serde_json::json!("12")), Some(12));
        assert_eq!(as_u32(&serde_json::json!(7)), Some(7));
        assert_eq!(as_u32(&serde_json::json!(-1)), None);
    }
}

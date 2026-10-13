//! SSE 帧。编码器产出 `SseFrame`，网关负责把它写到 HTTP 响应体。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// `event:` 行；OpenAI 风格为 None，Anthropic 风格为事件名。
    pub event: Option<String>,
    /// `data:` 内容（单行 JSON 或 `[DONE]`）。
    pub data: String,
}

impl SseFrame {
    pub fn data(data: impl Into<String>) -> Self {
        Self { event: None, data: data.into() }
    }

    pub fn event(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self { event: Some(event.into()), data: data.into() }
    }

    /// 序列化为线上格式（以空行结尾）。`data` 中的换行会拆成多条 `data:` 行。
    pub fn to_wire(&self) -> String {
        let mut out = String::new();
        if let Some(e) = &self.event {
            out.push_str("event: ");
            out.push_str(e);
            out.push('\n');
        }
        for line in self.data.split('\n') {
            out.push_str("data: ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
        out
    }
}

/// 解析 SSE 文本为帧（测试与后续上游适配器共用）。忽略注释行与 `id:`/`retry:`。
pub fn parse_sse(text: &str) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    let mut event: Option<String> = None;
    let mut data: Vec<&str> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.is_empty() {
            if !data.is_empty() || event.is_some() {
                frames.push(SseFrame { event: event.take(), data: data.join("\n") });
                data.clear();
            }
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if !data.is_empty() || event.is_some() {
        frames.push(SseFrame { event, data: data.join("\n") });
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip() {
        let frames = vec![SseFrame::event("message_start", "{\"a\":1}"), SseFrame::data("[DONE]")];
        let wire: String = frames.iter().map(SseFrame::to_wire).collect();
        assert_eq!(wire, "event: message_start\ndata: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(parse_sse(&wire), frames);
    }

    #[test]
    fn multiline_data_and_comments() {
        let frames = parse_sse(": keepalive\r\ndata: a\r\ndata: b\r\n\r\n");
        assert_eq!(frames, vec![SseFrame::data("a\nb")]);
    }
}

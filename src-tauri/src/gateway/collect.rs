//! 内部流式调用：模型探测读取首个内容，非流式客户端等待完整流并聚合成 JSON。
//! 解码器负责验证协议终止标记；中途断流、错误帧和超限都不能作为成功返回。

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};

use super::stream::{new_decoder, Primed};
use crate::protocols::canonical::{BlockDelta, BlockStart, CanonicalResponse, ContentPart, StopReason, StreamEvent, Usage};
use crate::protocols::gemini::ToolNameMap;
use crate::upstream::{AdapterKind, UpstreamError, MAX_RESPONSE_BYTES};

fn malformed() -> UpstreamError {
    UpstreamError::Malformed("the upstream stream ended before a complete response was received".into())
}

async fn decode_until(
    primed: Primed,
    kind: AdapterKind,
    names: ToolNameMap,
    idle: Duration,
    mut observe: impl FnMut(StreamEvent) -> Result<bool, UpstreamError>,
) -> Result<(), UpstreamError> {
    let Primed { mut body, mut splitter, mut frames } = primed;
    let mut decoder = new_decoder(kind, names.clone());
    let mut received = 0usize;
    loop {
        if let Some(mut frame) = frames.pop_front() {
            crate::upstream::opencode_compat::restore_frame_names(&mut frame, &names);
            let events = decoder.push(&frame).map_err(|_| malformed())?;
            for event in events {
                if observe(event)? { return Ok(()); }
            }
            if decoder.is_terminated() { return Ok(()); }
            continue;
        }
        match tokio::time::timeout(idle, body.next()).await {
            Err(_) => return Err(UpstreamError::Timeout),
            Ok(Some(Err(e))) => return Err(e),
            Ok(Some(Ok(chunk))) => {
                received = received.saturating_add(chunk.len());
                if received > MAX_RESPONSE_BYTES {
                    return Err(UpstreamError::Malformed("the upstream stream is too large".into()));
                }
                frames.extend(splitter.push(&chunk).map_err(|_| malformed())?);
            }
            Ok(None) => {
                if let Some(last) = splitter.finish() {
                    frames.push_back(last);
                    continue;
                }
                for event in decoder.finish().map_err(|_| malformed())? {
                    if observe(event)? { return Ok(()); }
                }
                return Ok(());
            }
        }
    }
}

pub(crate) async fn probe_first_content(primed: Primed, kind: AdapterKind, idle: Duration) -> Result<bool, UpstreamError> {
    let mut found = false;
    decode_until(primed, kind, ToolNameMap::default(), idle, |event| {
        match event {
            StreamEvent::ContentBlockDelta { delta: BlockDelta::Text { ref text } | BlockDelta::Thinking { ref text }, .. }
                if !text.is_empty() => found = true,
            StreamEvent::ToolCallDelta { ref partial_json, .. } if !partial_json.is_empty() => found = true,
            StreamEvent::ContentBlockStart { block: BlockStart::ToolUse { .. }, .. } => found = true,
            StreamEvent::Error { .. } => return Err(malformed()),
            _ => {}
        }
        Ok(found)
    }).await?;
    Ok(found)
}

enum Block {
    Text(String),
    Thinking(String, Option<String>),
    Tool(String, String, String),
}

#[derive(Default)]
struct Collector {
    id: Option<String>,
    model: Option<String>,
    blocks: BTreeMap<u32, Block>,
    usage: Usage,
    stop: Option<(StopReason, Option<String>)>,
}

impl Collector {
    fn push(&mut self, event: StreamEvent) -> Result<bool, UpstreamError> {
        match event {
            StreamEvent::MessageStart { id, model, usage } => {
                self.id = Some(id);
                self.model = Some(model);
                if let Some(u) = usage { self.usage.merge(&u); }
            }
            StreamEvent::ContentBlockStart { index, block } => {
                let state = match block {
                    BlockStart::Text => Block::Text(String::new()),
                    BlockStart::Thinking => Block::Thinking(String::new(), None),
                    BlockStart::ToolUse { id, name } => Block::Tool(id, name, String::new()),
                };
                self.blocks.insert(index, state);
            }
            StreamEvent::ContentBlockDelta { index, delta } => match (self.blocks.get_mut(&index), delta) {
                (Some(Block::Text(s)), BlockDelta::Text { text }) => s.push_str(&text),
                (Some(Block::Thinking(s, _)), BlockDelta::Thinking { text }) => s.push_str(&text),
                (Some(Block::Thinking(_, sig)), BlockDelta::Signature { signature }) => *sig = Some(signature),
                _ => return Err(malformed()),
            },
            StreamEvent::ToolCallDelta { index, partial_json } => match self.blocks.get_mut(&index) {
                Some(Block::Tool(_, _, args)) => args.push_str(&partial_json),
                _ => return Err(malformed()),
            },
            StreamEvent::Usage(u) => self.usage.merge(&u),
            StreamEvent::MessageStop { stop_reason, stop_sequence } => self.stop = Some((stop_reason, stop_sequence)),
            StreamEvent::Error { .. } => return Err(malformed()),
            StreamEvent::ContentBlockStop { .. } => {}
        }
        Ok(false)
    }

    fn finish(self) -> Result<CanonicalResponse, UpstreamError> {
        let (stop_reason, stop_sequence) = self.stop.ok_or_else(malformed)?;
        let content = self.blocks.into_values().map(|block| match block {
            Block::Text(text) => Ok(ContentPart::Text { text }),
            Block::Thinking(text, signature) => Ok(ContentPart::Thinking { text, signature }),
            Block::Tool(id, name, args) => {
                let input: Value = if args.trim().is_empty() { json!({}) } else { serde_json::from_str(&args).map_err(|_| malformed())? };
                Ok(ContentPart::ToolUse { id, name, input })
            }
        }).collect::<Result<Vec<_>, UpstreamError>>()?;
        Ok(CanonicalResponse {
            id: self.id.ok_or_else(malformed)?, model: self.model.ok_or_else(malformed)?, created: None,
            content, stop_reason, stop_sequence, usage: self.usage,
        })
    }
}

pub(crate) async fn collect_complete(
    primed: Primed,
    kind: AdapterKind,
    names: ToolNameMap,
    idle: Duration,
) -> Result<CanonicalResponse, UpstreamError> {
    let mut collected = Collector::default();
    decode_until(primed, kind, names, idle, |event| collected.push(event)).await?;
    collected.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures_util::stream;
    use crate::gateway::stream::{prime, PrimeOutcome};

    async fn prepared(s: &str) -> Primed {
        let body = stream::iter(vec![Ok(Bytes::from(s.to_string()))]).boxed();
        match prime(body, AdapterKind::OpenAiCompatible, Duration::from_secs(1)).await {
            PrimeOutcome::Ready(p) => p,
            _ => panic!("expected a valid first frame"),
        }
    }

    #[tokio::test]
    async fn collects_complete_stream_and_rejects_truncation() {
        let first = "data: {\"id\":\"chat-1\",\"model\":\"model-1\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n";
        let end = "data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let collected = collect_complete(prepared(&format!("{first}{end}")).await,
            AdapterKind::OpenAiCompatible, ToolNameMap::default(), Duration::from_secs(1)).await.unwrap();
        assert_eq!(collected.content, vec![ContentPart::Text { text: "Hello world".into() }]);

        let broken = collect_complete(prepared(first).await, AdapterKind::OpenAiCompatible,
            ToolNameMap::default(), Duration::from_secs(1)).await;
        assert!(broken.is_err(), "an interrupted stream must not produce a partial JSON response");
    }

    #[tokio::test]
    async fn collected_tool_calls_restore_provider_aliases_before_encoding() {
        let first = "data: {\"id\":\"chat-1\",\"model\":\"model-1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"shell_client\",\"arguments\":\"{\\\"cmd\\\":\\\"pwd\\\"}\"}}]},\"finish_reason\":null}]}\n\n";
        let end = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
        let mut names = ToolNameMap::default();
        names.insert_alias("shell", "shell_client");
        let response = collect_complete(prepared(&format!("{first}{end}")).await,
            AdapterKind::OpenAiCompatible, names, Duration::from_secs(1)).await.unwrap();
        assert!(response.content.iter().any(|part| matches!(part,
            ContentPart::ToolUse { name, input, .. } if name == "shell" && input["cmd"] == "pwd")));
    }

    #[tokio::test]
    async fn probe_accepts_first_real_increment_even_without_done() {
        let first = "data: {\"id\":\"chat-1\",\"model\":\"model-1\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n";
        assert!(probe_first_content(prepared(first).await, AdapterKind::OpenAiCompatible, Duration::from_secs(1)).await.unwrap());
    }
}

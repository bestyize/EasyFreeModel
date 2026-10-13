//! 仅测试用：Responses / Gemini / Ollama 测试共用的小工具。

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::canonical::*;
use super::sse::SseFrame;

pub fn fx(s: &str) -> Value {
    serde_json::from_str(s).expect("fixture must be valid JSON")
}

/// warnings 描述的是解码过程而非语义，比较语义时清空。
pub fn semantic(mut r: CanonicalRequest) -> CanonicalRequest {
    r.warnings.clear();
    r
}

pub fn has_warning(warnings: &[String], needle: &str) -> bool {
    warnings.iter().any(|w| w.contains(needle))
}

/// 把事件序列折叠成一个 `CanonicalResponse`，同时断言事件序列合法：
/// 以 `MessageStart` 开头、块先开后用再关、`Usage` 在 `MessageStop` 之前、结束时没有未关闭的块。
pub fn collect(events: &[StreamEvent]) -> CanonicalResponse {
    enum B {
        Text(String),
        Thinking(String, Option<String>),
        Tool(String, String, String),
    }
    let mut it = events.iter();
    let Some(StreamEvent::MessageStart { id, model, usage }) = it.next() else {
        panic!("first event must be MessageStart: {events:?}");
    };
    let mut usage_acc = usage.clone().unwrap_or_default();
    let mut blocks: BTreeMap<u32, B> = BTreeMap::new();
    let mut open: Vec<u32> = Vec::new();
    let mut order: Vec<u32> = Vec::new();
    let mut stop: Option<(StopReason, Option<String>)> = None;
    for ev in it {
        assert!(stop.is_none(), "event after MessageStop: {ev:?}");
        match ev {
            StreamEvent::MessageStart { .. } => panic!("duplicate MessageStart"),
            StreamEvent::ContentBlockStart { index, block } => {
                assert!(!blocks.contains_key(index), "block index {index} reused");
                blocks.insert(
                    *index,
                    match block {
                        BlockStart::Text => B::Text(String::new()),
                        BlockStart::Thinking => B::Thinking(String::new(), None),
                        BlockStart::ToolUse { id, name } => B::Tool(id.clone(), name.clone(), String::new()),
                    },
                );
                open.push(*index);
                order.push(*index);
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                assert!(open.contains(index), "delta for closed block {index}");
                match (blocks.get_mut(index).unwrap(), delta) {
                    (B::Text(t), BlockDelta::Text { text }) => t.push_str(text),
                    (B::Thinking(t, _), BlockDelta::Thinking { text }) => t.push_str(text),
                    (B::Thinking(_, s), BlockDelta::Signature { signature }) => *s = Some(signature.clone()),
                    (b, d) => panic!(
                        "delta {d:?} does not match block {}",
                        match b {
                            B::Text(_) => "text",
                            B::Thinking(..) => "thinking",
                            B::Tool(..) => "tool",
                        }
                    ),
                }
            }
            StreamEvent::ToolCallDelta { index, partial_json } => {
                assert!(open.contains(index), "tool delta for closed block {index}");
                match blocks.get_mut(index).unwrap() {
                    B::Tool(_, _, args) => args.push_str(partial_json),
                    _ => panic!("tool delta for a non-tool block"),
                }
            }
            StreamEvent::ContentBlockStop { index } => {
                let pos = open.iter().position(|i| i == index).expect("stop for a block that is not open");
                open.remove(pos);
            }
            StreamEvent::Usage(u) => usage_acc.merge(u),
            StreamEvent::MessageStop { stop_reason, stop_sequence } => {
                assert!(open.is_empty(), "blocks still open at MessageStop: {open:?}");
                stop = Some((stop_reason.clone(), stop_sequence.clone()));
            }
            StreamEvent::Error { .. } => panic!("collect() does not accept Error events"),
        }
    }
    let (stop_reason, stop_sequence) = stop.expect("stream has no MessageStop");
    let content = order
        .into_iter()
        .map(|i| match blocks.remove(&i).unwrap() {
            B::Text(text) => ContentPart::Text { text },
            B::Thinking(text, signature) => ContentPart::Thinking { text, signature },
            B::Tool(id, name, args) => ContentPart::ToolUse {
                id,
                name,
                input: if args.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&args).expect("tool args must be valid JSON")
                },
            },
        })
        .collect();
    CanonicalResponse {
        id: id.clone(),
        model: model.clone(),
        created: None,
        content,
        stop_reason,
        stop_sequence,
        usage: usage_acc,
    }
}

/// 把自动生成的 call id（`call_` + 24 位十六进制）替换成固定占位，便于比较。
pub fn normalize_generated_ids(mut r: CanonicalResponse) -> CanonicalResponse {
    for p in &mut r.content {
        if let ContentPart::ToolUse { id, .. } = p {
            if id.starts_with("call_") && id.len() == 29 {
                *id = "<generated>".into();
            }
        }
    }
    r
}

pub fn json_of(f: &SseFrame) -> Value {
    serde_json::from_str(&f.data).expect("frame data must be JSON")
}

//! CodeBuddy CN / Intl chat-request compatibility (9Router provider-specific behavior).
//! Only the two built-in CodeBuddy providers use this transformation; generic
//! OpenAI-compatible providers retain the caller's request unchanged.

use serde_json::{json, Value};

pub(crate) fn is_codebuddy(provider: &str) -> bool {
    matches!(provider, "codebuddy-cn" | "codebuddy-intl")
}

const NEUTRAL_AGENT_PROMPT: &str =
    "You are a helpful AI assistant that helps with software engineering tasks.";

fn system_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn is_agent_identity(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "you are claude code", "anthropic's official cli",
        "you are cursor", "you are windsurf", "you are cline",
        "you are aider", "you are continue", "you are copilot",
        "you are cody", "you are an ai coding agent",
        "you are a coding agent", "cc_entrypoint",
        "orchestration capabilities", "ohmyopencode",
        "<agent-identity>", "<role>", "<behavior_instructions>",
    ];
    MARKERS.iter().any(|marker| lower.contains(marker))
}

fn prepare_cn(obj: &mut serde_json::Map<String, Value>) {
    // 9Router treats long / identity-bearing agent system prompts specially.
    // Preserve ordinary system prompts; preserve the shape of typed blocks.
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if message.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            let Some(content) = message.get("content") else { continue };
            let text = system_text(content);
            if text.len() <= 2000 && !is_agent_identity(&text) {
                continue;
            }
            let replacement = match content {
                Value::String(_) => Value::String(NEUTRAL_AGENT_PROMPT.into()),
                Value::Array(_) => json!([{"type": "text", "text": NEUTRAL_AGENT_PROMPT}]),
                _ => continue,
            };
            if let Some(content) = message.get_mut("content") {
                *content = replacement;
            }
        }
    }
}

fn prepare_intl(obj: &mut serde_json::Map<String, Value>) {
    // 9Router Intl gateway expects a leading system instruction and user text
    // expressed as typed blocks. It doesn't accept the ordinary OpenAI shape.
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let previous = std::mem::take(messages);
    messages.push(json!({"role": "system", "content": "You are CodeBuddy Code."}));
    for mut message in previous {
        match message.get("role").and_then(Value::as_str) {
            Some("system" | "developer") => continue,
            Some("user") => {
                if let Some(Value::String(text)) = message.get_mut("content") {
                    let text = std::mem::take(text);
                    message["content"] = json!([{"type": "text", "text": text}]);
                }
            }
            _ => {}
        }
        messages.push(message);
    }
}

/// Some clients replay a streamed assistant turn as many adjacent text parts.
/// CodeBuddy treats each part as a separate paragraph, so rebuild the original
/// text before sending the conversation history. Newlines already present in
/// a part remain intact; no separator is invented between stream fragments.
fn coalesce_assistant_text(obj: &mut serde_json::Map<String, Value>) {
    let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) else { return };
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") { continue }
        let Some(parts) = message.get("content").and_then(Value::as_array) else { continue };
        if parts.is_empty() || !parts.iter().all(|part| {
            part.get("type").and_then(Value::as_str) == Some("text")
                && part.get("text").and_then(Value::as_str).is_some()
        }) { continue }
        let text: String = parts.iter().filter_map(|part| part.get("text").and_then(Value::as_str)).collect();
        message["content"] = Value::String(text);
    }
}

pub(crate) fn prepare_body(body: &mut Value, provider: &str) {
    if !is_codebuddy(provider) {
        return;
    }
    let Some(obj) = body.as_object_mut() else { return };
    // Both gateways require SSE even when FreeApiBox will collect it as JSON.
    obj.insert("stream".into(), Value::Bool(true));
    obj.remove("stream_options");

    match obj.get("reasoning_effort").and_then(Value::as_str) {
        Some("none" | "off") => {
            obj.remove("reasoning_effort");
            obj.remove("reasoning_summary");
        }
        Some(_) => {
            obj.insert("reasoning_summary".into(), Value::String("auto".into()));
        }
        None => {
            // Don't force reasoning on plain requests (9Router #2071).
            obj.remove("reasoning_summary");
        }
    }
    match provider {
        "codebuddy-cn" => prepare_cn(obj),
        "codebuddy-intl" => prepare_intl(obj),
        _ => {}
    }
    coalesce_assistant_text(obj);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_keeps_short_user_system_and_normalizes_agent_identity() {
        let mut b = json!({"stream": false, "messages": [
            {"role":"system","content":"Keep outputs concise"},
            {"role":"system","content":[{"type":"text","text":"You are Claude Code, Anthropic's official CLI"}]},
            {"role":"user","content":"hello"}
        ],"reasoning_effort":"high","stream_options":{"include_usage":true}});
        prepare_body(&mut b, "codebuddy-cn");
        assert_eq!(b["stream"], true);
        assert!(b.get("stream_options").is_none());
        assert_eq!(b["reasoning_summary"], "auto");
        assert_eq!(b["messages"][0]["content"], "Keep outputs concise");
        assert_eq!(b["messages"][1]["content"][0]["text"], NEUTRAL_AGENT_PROMPT);
        assert_eq!(b["messages"][2]["content"], "hello");
    }

    #[test]
    fn intl_uses_typed_user_blocks_and_leading_system() {
        let mut b = json!({"stream":false,"messages":[
            {"role":"system","content":"old"},
            {"role":"developer","content":"old"},
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"user","content":[{"type":"text","text":"again"}]}
        ]});
        prepare_body(&mut b, "codebuddy-intl");
        assert_eq!(b["messages"].as_array().unwrap().len(), 4);
        assert_eq!(b["messages"][0]["content"], "You are CodeBuddy Code.");
        assert_eq!(b["messages"][1]["content"][0]["text"], "hi");
        assert_eq!(b["messages"][2]["content"], "hello");
        assert_eq!(b["messages"][3]["content"][0]["text"], "again");
    }

    #[test]
    fn reasoning_none_does_not_force_thinking_and_others_unchanged() {
        let source = json!({"messages":[{"role":"user","content":"hello"}],
            "stream":false,"reasoning_effort":"none","reasoning_summary":"auto"});
        let mut cn = source.clone();
        prepare_body(&mut cn, "codebuddy-cn");
        assert!(cn.get("reasoning_effort").is_none());
        assert!(cn.get("reasoning_summary").is_none());
        let mut openai = source.clone();
        prepare_body(&mut openai, "openai");
        assert_eq!(openai, source);
    }

    #[test]
    fn long_system_prompt_is_normalized_only_for_cn() {
        let mut cn = json!({"messages":[{"role":"system","content":"x".repeat(2001)}]});
        prepare_body(&mut cn, "codebuddy-cn");
        assert_eq!(cn["messages"][0]["content"], NEUTRAL_AGENT_PROMPT);
    }

    #[test]
    fn replayed_assistant_tokens_do_not_become_paragraphs() {
        for provider in ["codebuddy-cn", "codebuddy-intl"] {
            let mut body = json!({"messages": [
                {"role": "user", "content": "read this"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Hi"},
                    {"type": "text", "text": "way"},
                    {"type": "text", "text": "/aggreg"},
                    {"type": "text", "text": "ate.rs\nnext line"}
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "look"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}}]}
            ]});
            prepare_body(&mut body, provider);
            let assistant = body["messages"].as_array().unwrap().iter()
                .filter(|message| message["role"] == "assistant").collect::<Vec<_>>();
            assert_eq!(assistant[0]["content"], "Hiway/aggregate.rs\nnext line");
            assert!(assistant[1]["content"].is_array(), "mixed image content must be preserved");
        }
    }
}

//! 9Router-compatible estimate for `POST /v1/messages/count_tokens`.
//! This is a character heuristic, not a provider tokenizer.

use serde_json::Value;

fn text_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn value_chars(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::String(s) => text_len(s),
        Value::Number(_) | Value::Bool(_) => text_len(&value.to_string()),
        Value::Array(items) => items.iter().map(value_chars).sum(),
        Value::Object(fields) => fields.iter().map(|(key, value)| text_len(key) + value_chars(value)).sum(),
    }
}

fn block_chars(block: &Value) -> usize {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block.get("text").map(value_chars).unwrap_or(0),
        Some("tool_use") => block.get("name").map(value_chars).unwrap_or(0)
            + block.get("input").map(value_chars).unwrap_or(0),
        Some("tool_result") => block.get("content").map(value_chars).unwrap_or(0),
        Some("thinking") => block.get("thinking").map(value_chars).unwrap_or(0),
        _ => value_chars(block),
    }
}

pub fn estimate(body: &Value) -> usize {
    let mut chars = body.get("system").map(value_chars).unwrap_or(0)
        + body.get("tools").map(value_chars).unwrap_or(0);
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(content) = message.get("content") {
                chars += match content {
                    Value::Array(parts) => parts.iter().map(block_chars).sum(),
                    other => value_chars(other),
                };
            }
        }
    }
    chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_nested_tools_and_utf16_like_the_reference() {
        let body = json!({
            "system": "hi😀", "tools": [{"name":"run", "input_schema":{"type":"object"}}],
            "messages": [
                {"role":"user", "content":[{"type":"text", "text":"abcd"}]},
                {"role":"assistant", "content":[{"type":"tool_use", "name":"run", "input":{"x":1}}]},
            ],
        });
        // system: 4 UTF-16 units; tools: 29; text: 4; tool_use: 5 = 42.
        assert_eq!(estimate(&body), 11);
    }
}

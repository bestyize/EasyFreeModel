//! OpenCode Free inference v2 wire compatibility.
//!
//! The free inference and paid Zen transports are intentionally separate.
//! Scoped to the built-in Free endpoint, never user-defined or paid endpoints.
//! Wire details derived from OpenCode v2 and 9Router's OpenCode executor.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde_json::{json, Value};

use super::{AdapterKind, UpstreamTarget};

const V2_USER_AGENT: &str = "opencode/latest/2.0.26/cli";
const MAX_SESSIONS: usize = 1000;
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

fn is_free(target: &UpstreamTarget) -> bool {
    target.provider_id == "opencode"
        && target.endpoint.base_url.host_str() == Some("opencode.ai")
        && target.endpoint.base_url.path().starts_with("/inference/v1/")
}

fn session_for(connection_id: &str) -> String {
    static SESSIONS: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    let sessions = SESSIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut entries = sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    entries.retain(|_, (_, used)| now.duration_since(*used) < SESSION_TTL);
    if let Some((session, last_used)) = entries.get_mut(connection_id) {
        *last_used = now;
        return session.clone();
    }
    if entries.len() >= MAX_SESSIONS {
        if let Some(oldest) = entries.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) {
            entries.remove(&oldest);
        }
    }
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    // OpenCode v2 session ids are 12 descending-time hex digits and 14 base62 digits.
    let prefix = (!(ms.wrapping_mul(4096).wrapping_add(1))) & 0xffff_ffff_ffff;
    const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let entropy = uuid::Uuid::new_v4();
    let random: String = entropy.as_bytes()[..14].iter()
        .map(|byte| BASE62[*byte as usize % BASE62.len()] as char)
        .collect();
    let session = format!("ses_{prefix:012x}{random}");
    entries.insert(connection_id.to_owned(), (session.clone(), now));
    session
}

pub(super) fn add_headers(target: &UpstreamTarget, headers: &mut HeaderMap) {
    if !is_free(target) { return; }
    let session = session_for(&target.connection_id);
    let value = HeaderValue::from_str(&session).expect("generated session must be valid");
    headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer public"));
    headers.insert(USER_AGENT, HeaderValue::from_static(V2_USER_AGENT));
    headers.insert("x-opencode-client", HeaderValue::from_static("cli"));
    headers.insert("x-opencode-project", HeaderValue::from_static("global"));
    for name in ["x-opencode-session", "x-opencode-session-id", "x-session-affinity", "x-session-id"] {
        headers.insert(name, value.clone());
    }
}

fn fingerprint_tools(flat: bool) -> [Value; 2] {
    let read = json!({
        "description": "Read the contents of a file or directory. Supports text files, images, and PDFs. Images and PDFs are presented directly to the model. Each text line is prefixed by its 1-based line number as <line>: <content>. The prefix is for reference and is not part of the file content. Directory entries are returned one per line. Use offset and limit to read large files or directories in sections. Prefer one larger read over many small slices, and use grep to find specific content in large files.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File or directory to read"},
                "offset": {"type": "integer", "minimum": 0, "description": "The line or directory entry to start reading from (1-based)"},
                "limit": {"type": "integer", "minimum": 0, "description": "The maximum number of lines or directory entries to read (defaults to and capped at 2000)"}
            },
            "required": ["path"],
            "additionalProperties": false
        }
    });
    let shell = json!({
        "description": "Execute a shell command and return its output. Commands run on Linux using bash. Quote file paths containing spaces or special characters. Prefer dedicated tools over shell commands when possible. When output is large, the full result is saved to a file and a truncated preview is returned. Rely on automatic truncation unless filtering the output is more useful. Commands accept an optional timeout, background commands have no timeout by default. Background commands return immediately, and you will be notified when they complete.",
        "parameters": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command string to execute"},
                "workdir": {"type": "string", "description": "Working directory to execute the command in. Defaults to the current working directory. When possible, avoid changing directories in the command and set the working directory here instead."},
                "timeout": {"type": "integer", "minimum": 0, "description": "Timeout in milliseconds. Set to 0 to disable the timeout. Defaults to 120000 for foreground commands. Background commands have no timeout by default."},
                "background": {"type": "boolean", "description": "Run the command in the background and return immediately (useful for dev servers and long-running builds). You do not need to use '&' at the end of the command when using this parameter. You will be notified when they complete. DO NOT poll for completion."}
            },
            "required": ["command"],
            "additionalProperties": false
        }
    });
    let wrap = |name: &str, def: Value| {
        let mut def = def;
        if flat {
            let obj = def.as_object_mut().expect("definition object");
            obj.insert("type".into(), json!("function"));
            obj.insert("name".into(), json!(name));
            obj.insert("strict".into(), json!(false));
            def
        } else {
            json!({"type": "function", "function": {"name": name, "description": def["description"], "parameters": def["parameters"]}})
        }
    };
    [wrap("read", read), wrap("shell", shell)]
}

fn tool_name(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str)
        .or_else(|| tool.get("function")?.get("name")?.as_str())
}

fn inject_fingerprint_tools(body: &mut Value, flat: bool) {
    let Some(obj) = body.as_object_mut() else { return };
    let current = obj.entry("tools").or_insert_with(|| json!([]));
    if !current.is_array() { *current = json!([]); }
    let list = current.as_array_mut().expect("array normalised");
    let mut appended = Vec::new();
    for definition in fingerprint_tools(flat) {
        let Some(name) = tool_name(&definition) else { continue };
        // Preserve a user's real tool even when its name collides; never
        // silently replace a caller tool with a dummy. Matching native
        // declarations are accepted without a duplicate.
        if list.iter().any(|tool| tool_name(tool) == Some(name)) { continue; }
        appended.push(name.to_owned());
        list.push(definition);
    }
    if !appended.is_empty() {
        // The added tools are protocol declarations, not executable tools in
        // FreeApiBox. Explicitly discourage calling them.
        let note = format!(
            "Do not call the `{}` tools; they are placeholders that do nothing. Use the other tools you have been given instead.",
            appended.join("` or `")
        );
        if flat {
            let text = obj.get("instructions").and_then(Value::as_str).unwrap_or("");
            let combined = if text.is_empty() { note } else { format!("{text}\n\n{note}") };
            obj.insert("instructions".into(), json!(combined));
        } else {
            let messages = obj.entry("messages").or_insert_with(|| json!([]));
            if let Some(list) = messages.as_array_mut() {
                if let Some(first) = list.first_mut() {
                    if first.get("role").and_then(Value::as_str) == Some("system") {
                        if let Some(text) = first.get("content").and_then(Value::as_str) {
                            let combined = format!("{text}\n\n{note}");
                            first["content"] = json!(combined);
                            return;
                        }
                    }
                }
                list.insert(0, json!({"role": "system", "content": note}));
            }
        }
    }
}

/// Responses requires flat function tools rather than Chat's nested function
/// wrapper. This is a wire-format conversion, not a mutation of caller tools.
fn normalize_responses_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else { return };
    tools.retain_mut(|tool| {
        let Some(obj) = tool.as_object() else { return false };
        let function = obj.get("function").and_then(Value::as_object).unwrap_or(obj);
        let Some(name) = function.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else { return false };
        let mut parameters = function.get("parameters").cloned()
            .filter(Value::is_object).unwrap_or_else(|| json!({"type":"object","properties":{}}));
        if parameters.get("type").and_then(Value::as_str) == Some("object")
            && parameters.get("properties").is_none()
        { parameters["properties"] = json!({}); }
        let mut flattened = json!({
            "type": "function",
            "name": name,
            "parameters": parameters,
        });
        if let Some(desc) = function.get("description").and_then(Value::as_str) {
            flattened["description"] = json!(desc);
        }
        *tool = flattened;
        true
    });
}

fn normalize_responses_input(body: &mut Value) {
    let Some(input) = body.get_mut("input") else { return };
    if let Some(text) = input.as_str() {
        let text = if text.trim().is_empty() { "..." } else { text };
        *input = json!([{"type":"message", "role":"user", "content":[{"type":"input_text","text":text}]}]);
    }
    let Some(items) = input.as_array_mut() else { return };
    if items.is_empty() {
        items.push(json!({"type":"message", "role":"user", "content":[{"type":"input_text","text":"..."}]}));
    }
    items.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let valid = item.get("name").and_then(Value::as_str)
                    .is_some_and(|name| !name.trim().is_empty());
                if !valid { continue; }
                if let Some(id) = item.get("call_id").and_then(Value::as_str).map(str::to_owned) {
                    if id.len() > 64 { item["call_id"] = json!(&id[..64]); }
                }
                let args = item.get("arguments").cloned().unwrap_or(Value::Null);
                let arguments = match args {
                    Value::String(ref raw) if serde_json::from_str::<Value>(raw).is_ok() => raw.clone(),
                    Value::String(_) | Value::Null => "{}".to_string(),
                    other => other.to_string(),
                };
                item["arguments"] = json!(arguments);
            }
            Some("function_call_output") => {
                if let Some(id) = item.get("call_id").and_then(Value::as_str).map(str::to_owned) {
                    if id.len() > 64 { item["call_id"] = json!(&id[..64]); }
                }
                let output = item.get("output").cloned().unwrap_or(Value::Null);
                let text = match output {
                    Value::String(s) => s,
                    Value::Null => String::new(),
                    Value::Array(parts) => parts.iter().map(|part| part.get("text")
                        .and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| part.to_string()))
                        .collect::<Vec<_>>().join(""),
                    other => other.to_string(),
                };
                item["output"] = json!(text);
            }
            _ => {}
        }
    }
}

pub(super) fn prepare_body(body: &mut Value, target: &UpstreamTarget) {
    if !is_free(target) { return; }
    let Some(obj) = body.as_object_mut() else { return };
    obj.insert("stream".into(), json!(true));
    obj.remove("stream_options");
    match target.endpoint.kind {
        AdapterKind::OpenAiResponses => {
            obj.insert("store".into(), json!(false));
            if !obj.contains_key("max_output_tokens") {
                if let Some(cap) = obj.remove("max_completion_tokens").or_else(|| obj.remove("max_tokens")) {
                    obj.insert("max_output_tokens".into(), cap);
                }
            }
            obj.remove("max_tokens");
            obj.remove("max_completion_tokens");
            if let Some(effort) = obj.remove("reasoning_effort").and_then(|v| v.as_str().map(str::to_owned)) {
                if !obj.get("reasoning").is_some_and(Value::is_object) { obj.insert("reasoning".into(), json!({})); }
                obj["reasoning"]["effort"] = json!(effort);
            }
            if let Some(reasoning) = obj.get_mut("reasoning").and_then(Value::as_object_mut) {
                reasoning.entry("summary").or_insert_with(|| json!("auto"));
            }
            obj.entry("tool_choice").or_insert_with(|| json!("auto"));
            // Strip encrypted reasoning items from prior pooled-account turns
            // and normalize the Responses function-call wire shape.
            normalize_responses_input(body);
            normalize_responses_tools(body);
            inject_fingerprint_tools(body, true);
        }
        AdapterKind::OpenAiCompatible => inject_fingerprint_tools(body, false),
        AdapterKind::AnthropicCompatible => { /* Anthropic native tools retain their original schema. */ }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::{AuthStyle, ResolvedEndpoint};
    use std::time::Duration;
    use url::Url;

    fn target(provider: &str) -> UpstreamTarget {
        UpstreamTarget {
            provider_id: provider.to_string(), connection_id: "opencode-test-connection".into(),
            endpoint: ResolvedEndpoint::new(AdapterKind::OpenAiCompatible,
                Url::parse("https://opencode.ai/inference/v1/chat/completions").unwrap(), AuthStyle::None),
            api_key: None, extra_headers: vec![], timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn headers_share_a_stable_session_and_are_free_only() {
        let t = target("opencode");
        let mut first = HeaderMap::new();
        add_headers(&t, &mut first);
        assert_eq!(first.get(AUTHORIZATION).unwrap(), "Bearer public");
        assert_eq!(first.get(USER_AGENT).unwrap(), V2_USER_AGENT);
        let id = first.get("x-opencode-session").unwrap().clone();
        assert_eq!(id.to_str().unwrap().len(), 30);
        let mut second = HeaderMap::new();
        add_headers(&t, &mut second);
        assert_eq!(second.get("x-opencode-session"), Some(&id));
        assert_eq!(second.get("x-opencode-session-id"), Some(&id));
        let mut paid = target("opencode-zen");
        let mut absent = HeaderMap::new();
        add_headers(&paid, &mut absent);
        assert!(absent.is_empty());
        paid.provider_id = "opencode".into();
        paid.endpoint.base_url = Url::parse("https://opencode.ai/zen/v1/chat/completions").unwrap();
        add_headers(&paid, &mut absent);
        assert!(absent.is_empty());
    }

    #[test]
    fn chat_tools_and_existing_content_are_preserved() {
        let t = target("opencode");
        let mut body = json!({"model":"deepseek-v4.1-flash-free", "stream":false,
            "messages":[{"role":"user","content":"hi"}], "tools":[{"type":"function","function":{"name":"my_tool","parameters":{"type":"object"}}}]});
        prepare_body(&mut body, &t);
        assert_eq!(body["stream"], true);
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["tools"].as_array().unwrap().len(), 3);
        assert_eq!(body["tools"][0]["function"]["name"], "my_tool");
        assert_eq!(body["tools"][1]["function"]["name"], "read");
        assert_eq!(body["tools"][2]["function"]["name"], "shell");
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("placeholders"));
    }

    #[test]
    fn responses_normalizes_text_input_and_function_history() {
        let mut t = target("opencode");
        t.endpoint.kind = AdapterKind::OpenAiResponses;
        let mut body = json!({"input":[
            {"type":"function_call","name":"lookup","call_id":"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZlong",
             "arguments":{"q":"hi"}},
            {"type":"function_call_output","call_id":"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZlong",
             "output":[{"text":"ok"}]}]});
        prepare_body(&mut body, &t);
        assert_eq!(body["input"][0]["arguments"], r#"{"q":"hi"}"#);
        assert_eq!(body["input"][0]["call_id"], body["input"][1]["call_id"]);
        assert_eq!(body["input"][0]["call_id"].as_str().unwrap().len(), 64);
        assert_eq!(body["input"][1]["output"], "ok");
        let mut text = json!({"input":"hello"});
        prepare_body(&mut text, &t);
        assert_eq!(text["input"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn responses_flattens_chat_style_tool_definitions() {
        let mut t = target("opencode");
        t.endpoint.kind = AdapterKind::OpenAiResponses;
        let mut body = json!({"model":"muse-spark-1.3-contributor-free", "input":"ping",
            "tools":[{"type":"function","function":{"name":"my_tool","description":"hello","parameters":{"type":"object"}}}]});
        prepare_body(&mut body, &t);
        assert_eq!(body["tools"][0]["name"], "my_tool");
        assert_eq!(body["tools"][0]["parameters"]["properties"], json!({}));
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tools"][1]["name"], "read");
    }

    #[test]
    fn responses_use_native_tool_format_and_drop_old_reasoning() {
        let mut t = target("opencode");
        t.endpoint.kind = AdapterKind::OpenAiResponses;
        let mut body = json!({"model":"muse-spark-1.3-contributor-free",
            "input":[{"role":"user","content":[{"type":"input_text","text":"hi"}]},
                     {"type":"reasoning","encrypted_content":"previous-account"}],
            "reasoning_effort":"high","max_tokens":40,"stream":false});
        prepare_body(&mut body, &t);
        assert_eq!(body["store"], false);
        assert_eq!(body["max_output_tokens"], 40);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["tools"][1]["name"], "shell");
        assert_eq!(body["tool_choice"], "auto");
    }
}

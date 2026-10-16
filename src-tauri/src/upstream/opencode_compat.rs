//! OpenCode Free inference v2 wire compatibility.
//!
//! The free inference and paid Zen transports are intentionally separate.
//! Scoped to the built-in Free endpoint, never user-defined or paid endpoints.
//! Wire details derived from OpenCode v2 and 9Router's OpenCode executor.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde_json::{json, Value};
use crate::protocols::gemini::ToolNameMap;

use super::{AdapterKind, ResolvedEndpoint, UpstreamTarget};

const V2_USER_AGENT: &str = "opencode/latest/2.0.26/cli";
const MAX_SESSIONS: usize = 1000;
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

fn is_free(target: &UpstreamTarget) -> bool {
    is_free_endpoint(&target.provider_id, &target.endpoint)
}

fn is_free_endpoint(provider: &str, endpoint: &ResolvedEndpoint) -> bool {
    free_user_agent(provider, &endpoint.base_url).is_some()
}

/// Used by the final HTTP send path as well as header construction.
pub(super) fn free_user_agent(provider: &str, url: &url::Url) -> Option<&'static str> {
    (provider == "opencode" && url.host_str() == Some("opencode.ai")
        && url.path().starts_with("/inference/v1/")).then_some(V2_USER_AGENT)
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
    headers.remove("x-opencode-request");
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

fn inject_fingerprint_tools(body: &mut Value, kind: AdapterKind) -> ToolNameMap {
    let mut names = ToolNameMap::default();
    let flat = kind == AdapterKind::OpenAiResponses;
    let anthropic = kind == AdapterKind::AnthropicCompatible;
    let Some(obj) = body.as_object_mut() else { return names };
    let definitions = fingerprint_tools(flat).map(|mut tool| {
        if anthropic {
            let def = &tool["function"];
            tool = json!({"name":def["name"], "description":def["description"], "input_schema":def["parameters"]});
        }
        tool
    });
    let mut list = obj.remove("tools").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut taken: HashSet<String> = list.iter().filter_map(tool_name).map(str::to_string).collect();
    let mut aliases = HashMap::new();
    let mut seen = HashSet::new();
    list.retain_mut(|tool| {
        let Some(name) = tool_name(tool).map(str::to_string) else { return true };
        let Some(expected) = definitions.iter().find(|def| tool_name(def) == Some(name.as_str())) else { return true };
        if !seen.insert(name.clone()) { return false; }
        let actual = tool.get("function").unwrap_or(tool);
        let expected = expected.get("function").unwrap_or(expected);
        let actual_schema = actual.get("parameters").or_else(|| actual.get("input_schema"));
        let expected_schema = expected.get("parameters").or_else(|| expected.get("input_schema"));
        if actual.get("description") == expected.get("description") && actual_schema == expected_schema { return true; }
        let mut alias = format!("{name}_client");
        let mut n = 2;
        while taken.contains(&alias) { alias = format!("{name}_client{n}"); n += 1; }
        taken.insert(alias.clone());
        names.insert_alias(&name, &alias);
        aliases.insert(name, alias.clone());
        if tool.get("function").is_some() { tool["function"]["name"] = json!(alias); }
        else { tool["name"] = json!(alias); }
        true
    });
    let mut appended = Vec::new();
    for definition in definitions {
        let Some(name) = tool_name(&definition) else { continue };
        if list.iter().any(|tool| tool_name(tool) == Some(name)) { continue; }
        appended.push(name.to_owned());
        list.push(definition);
    }
    obj.insert("tools".into(), json!(list));
    if let Some(choice) = obj.get_mut("tool_choice") {
        if let Some(name) = choice.pointer_mut("/function/name") { alias_name(name, &aliases); }
        else if let Some(name) = choice.get_mut("name") { alias_name(name, &aliases); }
    }
    if let Some(input) = obj.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                if let Some(name) = item.get_mut("name") { alias_name(name, &aliases); }
            }
        }
    }
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for call in calls {
                    if let Some(name) = call.pointer_mut("/function/name") { alias_name(name, &aliases); }
                }
            }
            if let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) {
                for part in parts {
                    if part.get("type").and_then(Value::as_str) == Some("tool_use") {
                        if let Some(name) = part.get_mut("name") { alias_name(name, &aliases); }
                    }
                }
            }
        }
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
        } else if anthropic {
            match obj.get_mut("system") {
                Some(Value::Array(parts)) => parts.push(json!({"type":"text", "text":note})),
                _ => {
                    let text = obj.get("system").and_then(Value::as_str).unwrap_or("");
                    obj.insert("system".into(), json!(if text.is_empty() { note } else { format!("{text}\n\n{note}") }));
                }
            }
        } else {
            let messages = obj.entry("messages").or_insert_with(|| json!([]));
            if let Some(list) = messages.as_array_mut() {
                if let Some(first) = list.first_mut() {
                    if first.get("role").and_then(Value::as_str) == Some("system") {
                        if let Some(text) = first.get("content").and_then(Value::as_str) {
                            let combined = format!("{text}\n\n{note}");
                            first["content"] = json!(combined);
                            return names;
                        }
                    }
                }
                list.insert(0, json!({"role": "system", "content": note}));
            }
        }
    }
    names
}

fn alias_name(value: &mut Value, aliases: &HashMap<String, String>) {
    if let Some(alias) = value.as_str().and_then(|name| aliases.get(name)).cloned() { *value = json!(alias); }
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
    items.retain(|item| {
        let kind = item.get("type").and_then(Value::as_str);
        kind != Some("reasoning") && (kind != Some("function_call") || item.get("name")
            .and_then(Value::as_str).is_some_and(|name| !name.trim().is_empty()))
    });
    if items.is_empty() {
        items.push(json!({"type":"message", "role":"user", "content":[{"type":"input_text","text":"..."}]}));
    }
    for item in items {
        if let Some(obj) = item.as_object_mut() {
            obj.remove("encrypted_content");
            obj.remove("reasoning_encrypted_content");
        }
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let valid = item.get("name").and_then(Value::as_str)
                    .is_some_and(|name| !name.trim().is_empty());
                if !valid { continue; }
                if let Some(id) = item.get("call_id").and_then(Value::as_str).map(str::to_owned) {
                    if id.len() > 64 { item["call_id"] = json!(id.chars().take(64).collect::<String>()); }
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
                    if id.len() > 64 { item["call_id"] = json!(id.chars().take(64).collect::<String>()); }
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
    let _ = prepare_for_endpoint(body, &target.provider_id, &target.endpoint);
}

/// Run before dispatch, keeping the alias map with this request's response pipeline.
/// The send-layer fallback is idempotent for already prepared requests and probes.
pub(crate) fn prepare_for_endpoint(body: &mut Value, provider: &str, endpoint: &ResolvedEndpoint) -> ToolNameMap {
    if !is_free_endpoint(provider, endpoint) { return ToolNameMap::default(); }
    let Some(obj) = body.as_object_mut() else { return ToolNameMap::default() };
    obj.insert("stream".into(), json!(true));
    obj.remove("stream_options");
    match endpoint.kind {
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
            if body.get("input").is_none() { body["input"] = json!([]); }
            normalize_responses_input(body);
            normalize_responses_tools(body);
            inject_fingerprint_tools(body, endpoint.kind)
        }
        AdapterKind::OpenAiCompatible | AdapterKind::AnthropicCompatible => inject_fingerprint_tools(body, endpoint.kind),
        _ => ToolNameMap::default(),
    }
}

/// Restore only protocol tool-name fields; text, arguments and unrelated metadata
/// are never searched/replaced. Covers JSON responses and the corresponding SSE events.
pub(crate) fn restore_tool_names(value: &mut Value, names: &ToolNameMap) -> bool {
    if names.is_identity() { return false; }
    fn restore(name: Option<&mut Value>, names: &ToolNameMap) -> bool {
        let Some(name) = name else { return false };
        let Some(wire) = name.as_str() else { return false };
        let original = names.original(wire);
        if original == wire { return false; }
        *name = json!(original);
        true
    }
    fn item(value: &mut Value, names: &ToolNameMap) -> bool {
        if matches!(value.get("type").and_then(Value::as_str), Some("function_call" | "tool_use")) {
            restore(value.get_mut("name"), names)
        } else { false }
    }
    let mut changed = false;
    if let Some(items) = value.as_array_mut() {
        for value in items { changed |= restore_tool_names(value, names); }
        return changed;
    }
    for holder in ["content_block", "item"] {
        if let Some(value) = value.get_mut(holder) { changed |= item(value, names); }
    }
    for holder in ["content", "output"] {
        if let Some(items) = value.get_mut(holder).and_then(Value::as_array_mut) {
            for value in items { changed |= item(value, names); }
        }
    }
    // Responses completion events wrap the final response object.
    if let Some(response) = value.get_mut("response") { changed |= restore_tool_names(response, names); }
    if let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) {
        for choice in choices {
            for holder in ["delta", "message"] {
                if let Some(calls) = choice.get_mut(holder).and_then(|v| v.get_mut("tool_calls")).and_then(Value::as_array_mut) {
                    for call in calls { changed |= restore(call.pointer_mut("/function/name"), names); }
                }
            }
        }
    }
    changed
}

pub(crate) fn restore_frame_names(frame: &mut super::sse_decode::RawFrame, names: &ToolNameMap) {
    if names.is_identity() { return; }
    let Ok(mut value) = serde_json::from_str::<Value>(&frame.data) else { return };
    if restore_tool_names(&mut value, names) {
        frame.data = value.to_string();
        frame.raw = crate::protocols::SseFrame { event: frame.event.clone(), data: frame.data.clone() }.to_wire().into();
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
    fn aliases_declarations_choice_and_history_without_losing_parameters() {
        let t = target("opencode");
        let mut body = json!({"messages":[{"role":"assistant","tool_calls":[
            {"id":"c1","type":"function","function":{"name":"shell","arguments":"{}"}}
        ]}], "tools":[
            {"type":"function","function":{"name":"shell","description":"Caller shell","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}}},
            {"type":"function","function":{"name":"shell_client","parameters":{"type":"object"}}}
        ], "tool_choice":{"type":"function","function":{"name":"shell"}}});
        let names = prepare_for_endpoint(&mut body, "opencode", &t.endpoint);
        assert_eq!(body["tools"][0]["function"]["name"], "shell_client2");
        assert!(body["tools"][0]["function"]["parameters"]["properties"].get("cmd").is_some());
        assert_eq!(body["tool_choice"]["function"]["name"], "shell_client2");
        assert_eq!(body["messages"][1]["tool_calls"][0]["function"]["name"], "shell_client2");
        assert_eq!(names.original("shell_client2"), "shell");
        assert_eq!(names.original("shell_client"), "shell_client");
        let before = body.clone();
        // The send-layer fallback must not remap an already prepared request.
        assert!(prepare_for_endpoint(&mut body, "opencode", &t.endpoint).is_identity());
        assert_eq!(body, before);
    }

    #[test]
    fn aliases_responses_tools_and_restores_every_response_envelope() {
        let mut t = target("opencode");
        t.endpoint.kind = AdapterKind::OpenAiResponses;
        let mut body = json!({"input":[{"type":"function_call","name":"read","call_id":"c","arguments":"{}"}],
            "tools":[{"type":"function","name":"read","parameters":{"type":"object","properties":{"file":{"type":"string"}}}}],
            "tool_choice":{"type":"function","name":"read"}});
        let names = prepare_for_endpoint(&mut body, "opencode", &t.endpoint);
        assert_eq!(body["input"][0]["name"], "read_client");
        assert_eq!(body["tool_choice"]["name"], "read_client");
        assert!(body["tools"][0]["parameters"]["properties"].get("file").is_some());
        for mut response in [
            json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"read_client","arguments":"read_client"}}]}}]}),
            json!({"choices":[{"message":{"tool_calls":[{"function":{"name":"read_client"}}]}}]}),
            json!({"type":"content_block_start","content_block":{"type":"tool_use","name":"read_client"}}),
            json!({"content":[{"type":"tool_use","name":"read_client"}]}),
            json!({"type":"response.output_item.added","item":{"type":"function_call","name":"read_client"}}),
            json!({"type":"response.output_item.done","item":{"type":"function_call","name":"read_client"}}),
            json!({"output":[{"type":"function_call","name":"read_client"}]}),
            json!({"type":"response.completed","response":{"output":[{"type":"function_call","name":"read_client"}]}}),
        ] {
            assert!(restore_tool_names(&mut response, &names));
            assert!(!response.to_string().contains("\"name\":\"read_client\""));
            assert!(!restore_tool_names(&mut response, &names), "restoration is idempotent");
        }
        let mut payload = json!({"text":"read_client", "arguments":"read_client"});
        assert!(!restore_tool_names(&mut payload, &names));
        assert_eq!(payload["text"], "read_client");
    }

    #[test]
    fn anthropic_tools_use_native_schema_and_alias_history() {
        let mut t = target("opencode");
        t.endpoint.kind = AdapterKind::AnthropicCompatible;
        let mut body = json!({"system":[{"type":"text","text":"Keep me"}],
            "messages":[{"role":"assistant","content":[{"type":"tool_use","id":"c","name":"shell","input":{"cmd":"pwd"}}]}],
            "tools":[{"name":"shell","input_schema":{"type":"object","properties":{"cmd":{"type":"string"}}}}],
            "tool_choice":{"type":"tool","name":"shell"}});
        let names = prepare_for_endpoint(&mut body, "opencode", &t.endpoint);
        assert_eq!(names.original("shell_client"), "shell");
        assert_eq!(body["messages"][0]["content"][0]["name"], "shell_client");
        assert_eq!(body["tool_choice"]["name"], "shell_client");
        assert_eq!(body["system"][0]["text"], "Keep me");
        assert!(body["system"][1]["text"].as_str().unwrap().contains("placeholders"));
        for tool in body["tools"].as_array().unwrap() {
            assert!(tool.get("input_schema").is_some());
            assert!(tool.get("function").is_none());
        }
    }

    #[test]
    fn exact_definitions_and_case_variants_are_kept_and_maps_are_request_local() {
        let t = target("opencode");
        let mut exact = json!({"tools":fingerprint_tools(false),"messages":[]});
        assert!(prepare_for_endpoint(&mut exact, "opencode", &t.endpoint).is_identity());
        assert_eq!(exact["tools"].as_array().unwrap().len(), 2);
        let mut body = json!({"tools":[
            {"type":"function","function":{"name":"Read","parameters":{"type":"object"}}},
            {"type":"function","function":{"name":"read","parameters":{"type":"object"}}},
            {"type":"function","function":{"name":"read","parameters":{"type":"object"}}}
        ],"messages":[]});
        let first = prepare_for_endpoint(&mut body, "opencode", &t.endpoint);
        assert_eq!(first.original("read_client"), "read");
        assert_eq!(body["tools"][0]["function"]["name"], "Read");
        assert_eq!(body["tools"].as_array().unwrap().len(), 4);
        let mut another = json!({"messages":[],"tools":[]});
        assert!(prepare_for_endpoint(&mut another, "opencode", &t.endpoint).is_identity());
        let before = body.clone();
        assert!(prepare_for_endpoint(&mut body, "opencode-zen", &t.endpoint).is_identity());
        assert_eq!(body, before);
    }

    #[test]
    fn sse_restoration_preserves_events_arguments_and_text() {
        let mut names = ToolNameMap::default();
        names.insert_alias("shell", "shell_client");
        let data = json!({"choices":[{"delta":{"content":"hello\nworld", "tool_calls":[{"function":{"name":"shell_client","arguments":"{\"cmd\":\"shell_client\"}"}}]}}]});
        let mut frame = super::super::sse_decode::RawFrame {
            raw: bytes::Bytes::new(), event: Some("custom-event".into()), data:data.to_string(),
        };
        restore_frame_names(&mut frame, &names);
        let parsed = crate::protocols::sse::parse_sse(std::str::from_utf8(&frame.raw).unwrap());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].event.as_deref(), Some("custom-event"));
        let out: Value = serde_json::from_str(&parsed[0].data).unwrap();
        assert_eq!(out["choices"][0]["delta"]["content"], "hello\nworld");
        assert_eq!(out["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(out["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], data["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]);
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

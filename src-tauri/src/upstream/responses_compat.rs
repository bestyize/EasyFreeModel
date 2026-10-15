//! OpenAI Responses 上游适配器：`POST <base>/responses`，流式为带 `event:` 名的 SSE。
//!
//! 鉴权默认 `Authorization: Bearer`。base 已以 `/responses` 结尾（完整端点）时原样使用，否则追加 `responses`。
//! 模型列表沿用 OpenAI 风格的 `GET <base>/models`。

use serde_json::Value;
use url::Url;

use super::endpoint::join_path;
use super::{Adapter, AdapterKind, Capabilities, UpstreamRequest, UpstreamTarget};

pub struct ResponsesAdapter;

fn has_web_search(tools: &Value) -> bool {
    tools.as_array().is_some_and(|items| items.iter()
        .any(|tool| tool.get("type").and_then(Value::as_str) == Some("web_search")))
}

pub(crate) fn codex_uses_lite(body: &Value, model: &str) -> bool {
    codex_uses_lite_fields(body.get("tools"), body.get("input"), model)
}

fn codex_uses_lite_fields(tools: Option<&Value>, input: Option<&Value>, model: &str) -> bool {
    matches!(model, "gpt-6.1-sol" | "gpt-6-sol" | "gpt-6-luna")
        && !tools.is_some_and(has_web_search)
        && !input.and_then(Value::as_array).is_some_and(|items| items.iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools")
                && item.get("tools").is_some_and(has_web_search)))
}

impl ResponsesAdapter {
    pub fn responses_url(base: &Url, path: Option<&str>) -> Url {
        if let Some(p) = path {
            return join_path(base, p);
        }
        if base.path().trim_end_matches('/').ends_with("/responses") {
            return base.clone();
        }
        join_path(base, "responses")
    }
}

impl Adapter for ResponsesAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::OpenAiResponses
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        Self::responses_url(&target.endpoint.base_url, target.endpoint.path.as_deref())
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        let base = &target.endpoint.base_url;
        let trimmed = base.path().trim_end_matches('/');
        if let Some(root) = trimmed.strip_suffix("/responses") {
            let mut u = base.clone();
            u.set_path(&format!("{root}/models"));
            return u;
        }
        join_path(base, "models")
    }

    fn prepare_body(&self, body: &mut Value, _req: &UpstreamRequest, target: &UpstreamTarget) {
        if target.provider_id != "codex" { return; }
        let Some(obj) = body.as_object_mut() else { return; };
        // ChatGPT Codex 的 Responses 入口要求明确的 instructions / store，并只接受其子集。
        obj.retain(|k, _| matches!(k.as_str(),
            "model" | "input" | "instructions" | "tools" | "tool_choice" |
            "parallel_tool_calls" | "reasoning" | "include" | "stream" | "store" | "text"
        ));
        if let Some(Value::String(text)) = obj.get("input") {
            let text = text.clone();
            obj.insert("input".into(), serde_json::json!([{"type":"message", "role":"user", "content":[{"type":"input_text", "text":text}]}]));
        }
        if let Some(items) = obj.get_mut("input").and_then(Value::as_array_mut) {
            items.retain(|item| item.get("type").and_then(Value::as_str) != Some("item_reference"));
            for item in items {
                if let Some(message) = item.as_object_mut() {
                    if message.get("role").and_then(Value::as_str) == Some("system") {
                        message.insert("role".into(), Value::String("developer".into()));
                    }
                    if message.get("id").and_then(Value::as_str)
                        .is_some_and(|id| ["rs_", "fc_", "resp_", "msg_"].iter().any(|prefix| id.starts_with(prefix))) {
                        message.remove("id");
                    }
                }
            }
        }
        if !obj.get("instructions").is_some_and(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())) {
            obj.insert("instructions".into(), Value::String("You are a helpful assistant.".into()));
        }
        obj.insert("store".into(), Value::Bool(false));
        let model = obj.get("model").and_then(Value::as_str).unwrap_or_default().to_owned();
        let lite = codex_uses_lite_fields(obj.get("tools"), obj.get("input"), &model);
        if !lite {
            let mut prefix_tools = Vec::new();
            if let Some(items) = obj.get_mut("input").and_then(Value::as_array_mut) {
                items.retain(|item| {
                    if item.get("type").and_then(Value::as_str) != Some("additional_tools") { return true }
                    if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                        prefix_tools.extend(tools.iter().cloned());
                    }
                    false
                });
            }
            if !prefix_tools.is_empty() {
                let tools = obj.entry("tools").or_insert_with(|| Value::Array(vec![]));
                if !tools.is_array() { *tools = Value::Array(vec![]); }
                if let Some(list) = tools.as_array_mut() {
                    for tool in prefix_tools {
                        if !list.contains(&tool) { list.push(tool); }
                    }
                }
            }
        }
        if lite {
            let has_prefix = obj.get("input").and_then(Value::as_array)
                .is_some_and(|items| items.iter().any(|item| item.get("type").and_then(Value::as_str) == Some("additional_tools")));
            let tools = obj.remove("tools").filter(Value::is_array).unwrap_or_else(|| Value::Array(vec![]));
            let instructions = obj.insert("instructions".into(), Value::String(String::new()))
                .and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            if let Some(items) = obj.get_mut("input").and_then(Value::as_array_mut) {
                if !has_prefix {
                    items.insert(0, serde_json::json!({"type":"message", "role":"developer", "content":[{"type":"input_text", "text":instructions}]}));
                    items.insert(0, serde_json::json!({"type":"additional_tools", "role":"developer", "tools":tools}));
                }
            }
            obj.insert("tools".into(), Value::Null);
            obj.entry("tool_choice").or_insert_with(|| Value::String("auto".into()));
            obj.insert("parallel_tool_calls".into(), Value::Bool(false));
            let reasoning = obj.entry("reasoning").or_insert_with(|| serde_json::json!({"effort":"medium"}));
            if !reasoning.is_object() { *reasoning = serde_json::json!({"effort":"medium"}); }
            if let Some(r) = reasoning.as_object_mut() {
                r.entry("effort").or_insert_with(|| Value::String("medium".into()));
                r.remove("summary");
                r.insert("context".into(), Value::String("all_turns".into()));
            }
        } else {
            obj.remove("parallel_tool_calls");
            if !obj.contains_key("reasoning") {
                obj.insert("reasoning".into(), serde_json::json!({"effort":"low", "summary":"auto"}));
            }
        }
        if obj.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str) != Some("none") {
            obj.insert("include".into(), serde_json::json!(["reasoning.encrypted_content"]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(ResponsesAdapter::responses_url(&u("https://a.example/v1"), None).as_str(), "https://a.example/v1/responses");
        assert_eq!(ResponsesAdapter::responses_url(&u("https://a.example/v1/"), None).as_str(), "https://a.example/v1/responses");
        assert_eq!(ResponsesAdapter::responses_url(&u("https://a.example/v1/responses"), None).as_str(), "https://a.example/v1/responses");
        assert_eq!(ResponsesAdapter::responses_url(&u("https://a.example"), Some("/openai/v1/responses")).as_str(), "https://a.example/openai/v1/responses");
    }

    #[test]
    fn codex_lite_request_uses_prefix_and_required_stream_fields() {
        let target = UpstreamTarget {
            provider_id: "codex".into(), connection_id: "account-1".into(),
            endpoint: super::super::ResolvedEndpoint::new(
                AdapterKind::OpenAiResponses,
                Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap(),
                super::super::AuthStyle::Bearer,
            ),
            api_key: None, extra_headers: vec![], timeout: std::time::Duration::from_secs(10),
        };
        let req = UpstreamRequest {
            model: "gpt-6.1-sol".into(), stream: true, passthrough: false,
            body: serde_json::json!({"model":"gpt-6.1-sol", "input":[{"role":"user", "content":[{"type":"input_text", "text":"ping"}]}], "tools":[], "stream":true, "max_output_tokens":16}),
            extra_headers: vec![],
        };
        let mut body = req.body.clone();
        ResponsesAdapter.prepare_body(&mut body, &req, &target);
        assert_eq!(body["input"][0]["type"], "additional_tools");
        assert_eq!(body["input"][1]["role"], "developer");
        assert_eq!(body["instructions"], "");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(body.get("max_output_tokens").is_none());
    }

    #[test]
    fn codex_preserves_reasoning_and_moves_web_search_out_of_lite_prefix() {
        let target = UpstreamTarget { provider_id: "codex".into(), connection_id: "c".into(),
            endpoint: super::super::ResolvedEndpoint::new(AdapterKind::OpenAiResponses,
                Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap(), super::super::AuthStyle::Bearer),
            api_key: None, extra_headers: vec![], timeout: std::time::Duration::from_secs(10) };
        let req = UpstreamRequest { model: "gpt-6.1-sol".into(), stream: true, passthrough: true,
            body: serde_json::json!({"model":"gpt-6.1-sol", "input":[
                {"type":"additional_tools", "role":"developer", "tools":[{"type":"web_search"}]},
                {"type":"message", "role":"user", "content":[{"type":"input_text", "text":"find"}]}
            ], "reasoning":{"effort":"high"}, "stream":true}), extra_headers: vec![] };
        assert!(!codex_uses_lite(&req.body, &req.model));
        let mut body = req.body.clone();
        ResponsesAdapter.prepare_body(&mut body, &req, &target);
        assert_eq!(body["tools"][0]["type"], "web_search");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body.get("parallel_tool_calls").is_none());

        let mut lite_req = req.clone();
        lite_req.body = serde_json::json!({"model":"gpt-6.1-sol", "input":[{"role":"user", "content":"hello"}], "reasoning":{"effort":"high"}, "stream":true});
        let mut lite_body = lite_req.body.clone();
        ResponsesAdapter.prepare_body(&mut lite_body, &lite_req, &target);
        assert_eq!(lite_body["reasoning"]["effort"], "high");
        assert_eq!(lite_body["reasoning"]["context"], "all_turns");
    }
}

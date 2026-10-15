//! OpenAI Responses 上游适配器：`POST <base>/responses`，流式为带 `event:` 名的 SSE。
//!
//! 鉴权默认 `Authorization: Bearer`。base 已以 `/responses` 结尾（完整端点）时原样使用，否则追加 `responses`。
//! 模型列表沿用 OpenAI 风格的 `GET <base>/models`。

use serde_json::Value;
use url::Url;

use super::endpoint::join_path;
use super::{Adapter, AdapterKind, Capabilities, UpstreamRequest, UpstreamTarget};

pub struct ResponsesAdapter;

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
        if !obj.get("instructions").is_some_and(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())) {
            obj.insert("instructions".into(), Value::String("You are a helpful assistant.".into()));
        }
        obj.insert("store".into(), Value::Bool(false));
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
}

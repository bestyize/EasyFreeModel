//! OpenAI 兼容适配器：`POST <base>/chat/completions`。
//!
//! - 鉴权默认 `Authorization: Bearer`，可由配置改成其它请求头（例如 Azure 风格 `api-key`）。
//! - 流式请求默认附带 `stream_options.include_usage = true`，以便拿到上游真实 token 用量
//!   （`streamUsage: false` 可关闭，用于不接受该字段的上游）。
//! - 推理内容字段（`reasoning_content` / `reasoning`）由解码器处理，见 [`super::sse_decode`]。

use serde_json::{json, Value};
use url::Url;

use super::endpoint::join_path;
use super::{Adapter, AdapterKind, Capabilities, UpstreamRequest, UpstreamTarget};

pub struct OpenAiCompatAdapter;

impl OpenAiCompatAdapter {
    pub fn chat_url(base: &Url, path: Option<&str>) -> Url {
        if let Some(p) = path {
            return join_path(base, p);
        }
        if base.path().trim_end_matches('/').ends_with("/chat/completions") {
            return base.clone();
        }
        join_path(base, "chat/completions")
    }
}

impl Adapter for OpenAiCompatAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::OpenAiCompatible
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        Self::chat_url(&target.endpoint.base_url, target.endpoint.path.as_deref())
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        let base = &target.endpoint.base_url;
        let trimmed = base.path().trim_end_matches('/');
        if let Some(root) = trimmed.strip_suffix("/chat/completions") {
            let mut u = base.clone();
            u.set_path(&format!("{root}/models"));
            return u;
        }
        join_path(base, "models")
    }

    fn prepare_body(&self, body: &mut Value, req: &UpstreamRequest, target: &UpstreamTarget) {
        let Some(obj) = body.as_object_mut() else { return };
        if !req.stream {
            return;
        }
        if target.endpoint.stream_usage {
            let so = obj.entry("stream_options").or_insert_with(|| json!({}));
            if let Some(so) = so.as_object_mut() {
                so.insert("include_usage".into(), Value::Bool(true));
            }
        } else if !req.passthrough || target.endpoint.force_stream {
            obj.remove("stream_options");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(OpenAiCompatAdapter::chat_url(&u("https://a.example/v1"), None).as_str(), "https://a.example/v1/chat/completions");
        assert_eq!(OpenAiCompatAdapter::chat_url(&u("https://a.example/v1/"), None).as_str(), "https://a.example/v1/chat/completions");
        assert_eq!(
            OpenAiCompatAdapter::chat_url(&u("https://a.example/openai/v1/chat/completions"), None).as_str(),
            "https://a.example/openai/v1/chat/completions"
        );
        assert_eq!(
            OpenAiCompatAdapter::chat_url(&u("https://a.example/api"), Some("/paas/v4/chat/completions")).as_str(),
            "https://a.example/api/paas/v4/chat/completions"
        );
    }
}

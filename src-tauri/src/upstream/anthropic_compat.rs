//! Anthropic 兼容适配器：`POST <base>/v1/messages`。
//!
//! - 必带 `anthropic-version`（透传时优先使用客户端给出的值，否则用配置 / 默认值）。
//! - 鉴权默认 `x-api-key`，也可配置为 Bearer 等。
//! - 透传时转发客户端的 `anthropic-beta`（这是客户端自己声明的功能开关，不是伪装）。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::endpoint::join_path;
use super::{Adapter, AdapterKind, Capabilities, UpstreamRequest, UpstreamTarget};

pub struct AnthropicCompatAdapter;

impl AnthropicCompatAdapter {
    pub fn messages_url(base: &Url, path: Option<&str>) -> Url {
        if let Some(p) = path {
            return join_path(base, p);
        }
        let trimmed = base.path().trim_end_matches('/');
        if trimmed.ends_with("/messages") {
            base.clone()
        } else if trimmed.ends_with("/v1") {
            join_path(base, "messages")
        } else {
            join_path(base, "v1/messages")
        }
    }
}

impl Adapter for AnthropicCompatAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::AnthropicCompatible
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        Self::messages_url(&target.endpoint.base_url, target.endpoint.path.as_deref())
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        let base = &target.endpoint.base_url;
        let trimmed = base.path().trim_end_matches('/');
        if let Some(root) = trimmed.strip_suffix("/messages") {
            let mut u = base.clone();
            u.set_path(&format!("{root}/models"));
            u
        } else if trimmed.ends_with("/v1") {
            join_path(base, "models")
        } else {
            join_path(base, "v1/models")
        }
    }

    fn protocol_headers(&self, target: &UpstreamTarget, _req: &UpstreamRequest) -> HeaderMap {
        let mut h = HeaderMap::new();
        let version = HeaderValue::from_str(&target.endpoint.anthropic_version)
            .unwrap_or_else(|_| HeaderValue::from_static(super::DEFAULT_ANTHROPIC_VERSION));
        h.insert(HeaderName::from_static("anthropic-version"), version);
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert_eq!(AnthropicCompatAdapter::messages_url(&u("https://api.anthropic.com"), None).as_str(), "https://api.anthropic.com/v1/messages");
        assert_eq!(AnthropicCompatAdapter::messages_url(&u("https://h.example/anthropic/v1"), None).as_str(), "https://h.example/anthropic/v1/messages");
        assert_eq!(AnthropicCompatAdapter::messages_url(&u("https://h.example/v1/messages"), None).as_str(), "https://h.example/v1/messages");
        assert_eq!(AnthropicCompatAdapter::messages_url(&u("https://h.example"), Some("/x/messages")).as_str(), "https://h.example/x/messages");
    }
}

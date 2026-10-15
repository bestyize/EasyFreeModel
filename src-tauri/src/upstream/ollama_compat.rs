//! Ollama 适配器：`POST <base>/api/chat`，响应为 NDJSON（每行一个 JSON，不是 SSE）。
//!
//! - 云端 Ollama（`https://ollama.com`）用 `Authorization: Bearer`；本机 Ollama（`ollama-local`）不需要鉴权，
//!   由 `routing::endpoint` 把它的鉴权方式设为 [`AuthStyle::None`](super::AuthStyle)，此时不发送任何凭据。
//! - 内置提供商的 baseUrl 已是完整端点（`https://ollama.com/api/chat`），自定义提供商填 API 根（`http://host:11434`）。

use serde_json::Value;
use url::Url;

use super::endpoint::join_path;
use super::{client, Adapter, AdapterKind, Capabilities, UpstreamClient, UpstreamError, UpstreamTarget};

pub struct OllamaAdapter;

impl OllamaAdapter {
    pub fn chat_url(base: &Url, path: Option<&str>) -> Url {
        if let Some(p) = path {
            return join_path(base, p);
        }
        let t = base.path().trim_end_matches('/');
        if t.ends_with("/api/chat") {
            base.clone()
        } else if t.ends_with("/api") {
            join_path(base, "chat")
        } else {
            join_path(base, "api/chat")
        }
    }

    pub fn tags_url(base: &Url) -> Url {
        let t = base.path().trim_end_matches('/');
        if let Some(root) = t.strip_suffix("/api/chat") {
            let mut u = base.clone();
            u.set_path(&format!("{root}/api/tags"));
            u
        } else if t.ends_with("/api") {
            join_path(base, "tags")
        } else {
            join_path(base, "api/tags")
        }
    }
}

pub(super) fn parse_tags(body: &[u8]) -> Result<Vec<String>, UpstreamError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| UpstreamError::Malformed("the upstream model list is not valid JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| m.get("model").or_else(|| m.get("name")).and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default())
}

#[async_trait::async_trait]
impl Adapter for OllamaAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Ollama
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }

    fn accept(&self, stream: bool) -> &'static str {
        if stream {
            "application/x-ndjson"
        } else {
            "application/json"
        }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        Self::chat_url(&target.endpoint.base_url, target.endpoint.path.as_deref())
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        Self::tags_url(&target.endpoint.base_url)
    }

    async fn list_models(&self, http: &UpstreamClient, target: &UpstreamTarget) -> Result<Vec<String>, UpstreamError> {
        let mut headers = super::build_headers(self, target, None, false)?;
        headers.remove(reqwest::header::CONTENT_TYPE);
        let resp = http.get(&target.provider_id, self.models_url(target), headers, target.timeout).await?;
        let status = resp.status();
        let body = client::read_body(resp, super::MAX_ERROR_BODY_BYTES * 4, true).await?;
        if !status.is_success() {
            return Err(UpstreamError::Network(format!("the upstream answered the model list request with HTTP {}", status.as_u16())));
        }
        parse_tags(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn url_rules() {
        for (base, chat, tags) in [
            ("https://ollama.com/api/chat", "https://ollama.com/api/chat", "https://ollama.com/api/tags"),
            ("http://localhost:11434", "http://localhost:11434/api/chat", "http://localhost:11434/api/tags"),
            ("http://localhost:11434/", "http://localhost:11434/api/chat", "http://localhost:11434/api/tags"),
            ("https://gw.example.com/ollama/api", "https://gw.example.com/ollama/api/chat", "https://gw.example.com/ollama/api/tags"),
            ("https://gw.example.com/ollama", "https://gw.example.com/ollama/api/chat", "https://gw.example.com/ollama/api/tags"),
        ] {
            assert_eq!(OllamaAdapter::chat_url(&u(base), None).as_str(), chat, "{base}");
            assert_eq!(OllamaAdapter::tags_url(&u(base)).as_str(), tags, "{base}");
        }
    }

    #[test]
    fn tags_parsing() {
        let body = br#"{"models":[{"name":"llama3:8b","model":"llama3:8b"},{"name":"qwen3"}]}"#;
        assert_eq!(parse_tags(body).unwrap(), vec!["llama3:8b", "qwen3"]);
    }
}

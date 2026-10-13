//! Gemini（Google AI Studio）适配器：`POST <root>/models/<model>:generateContent`，流式为
//! `:streamGenerateContent?alt=sse`。
//!
//! - **鉴权只走请求头 `x-goog-api-key`**（由 [`ResolvedEndpoint::auth`](super::ResolvedEndpoint) 写入）。
//!   Gemini 也接受 `?key=` 查询参数，但那会让密钥出现在 URL、日志和错误信息里，这里不使用。
//! - 模型与「是否流式」在 URL 里，不在 body 里：因此覆盖 [`Adapter::request_url`]，用 `UpstreamRequest::model`。
//! - API 根的规整：内置提供商的 baseUrl 是 `https://generativelanguage.googleapis.com/v1beta/models`，
//!   自定义提供商通常只填主机或 `.../v1beta`；这里统一成 `<host>[/prefix]/v1beta`（已经是 `/v1`、`/v1alpha` 的保持不变）。
//! - 工具名映射（[`gemini::ToolNameMap`]）由网关在编码请求时取得，并贯穿到响应解码，不经过本适配器。

use serde_json::Value;
use url::Url;

use super::endpoint::join_path;
use super::{client, Adapter, AdapterKind, Capabilities, UpstreamClient, UpstreamError, UpstreamRequest, UpstreamTarget};
use crate::protocols::gemini;

/// Gemini / Vertex express 的 API Key 请求头。
pub const API_KEY_HEADER: &str = "x-goog-api-key";

pub struct GeminiAdapter;

/// `<host>[/prefix]/v1beta`（或 `/v1`、`/v1alpha`）。接受 base 里已带 `/models` 的写法。
pub fn api_root(base: &Url) -> Url {
    let trimmed = base.path().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/models").unwrap_or(trimmed);
    let mut u = base.clone();
    u.set_query(None);
    u.set_fragment(None);
    if ["/v1beta", "/v1", "/v1alpha"].iter().any(|v| trimmed.ends_with(v)) {
        u.set_path(trimmed);
    } else {
        u.set_path(&format!("{trimmed}/v1beta"));
    }
    u
}

/// 把 `models/x:generateContent?alt=sse` 这样的相对路径接到 `root` 后面，query 单独设置（不被转义）。
pub(super) fn with_path_and_query(root: &Url, path_and_query: &str) -> Url {
    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_and_query, None),
    };
    let mut u = join_path(root, path);
    u.set_query(query);
    u
}

pub fn method_url(base: &Url, model: &str, stream: bool) -> Url {
    with_path_and_query(&api_root(base), &gemini::request_path(model, stream))
}

/// 解析 `{"models":[{"name":"models/x"}]}`。
pub(super) fn parse_model_list(body: &[u8]) -> Result<Vec<String>, UpstreamError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| UpstreamError::Malformed("the upstream model list is not valid JSON".into()))?;
    Ok(v.get("models")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str))
                .map(|n| n.strip_prefix("models/").unwrap_or(n).to_string())
                .collect()
        })
        .unwrap_or_default())
}

#[async_trait::async_trait]
impl Adapter for GeminiAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Gemini
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        join_path(&api_root(&target.endpoint.base_url), "models")
    }

    fn request_url(&self, target: &UpstreamTarget, req: &UpstreamRequest) -> Url {
        method_url(&target.endpoint.base_url, &req.model, req.stream)
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        let mut u = self.endpoint_url(target);
        u.set_query(Some("pageSize=1000"));
        u
    }

    async fn list_models(&self, http: &UpstreamClient, target: &UpstreamTarget) -> Result<Vec<String>, UpstreamError> {
        let mut headers = super::build_headers(self, target, None, false)?;
        headers.remove(reqwest::header::CONTENT_TYPE);
        let resp = http.get(self.models_url(target), headers, target.timeout).await?;
        let status = resp.status();
        let body = client::read_body(resp, super::MAX_ERROR_BODY_BYTES * 4, true).await?;
        if !status.is_success() {
            return Err(UpstreamError::Network(format!("the upstream answered the model list request with HTTP {}", status.as_u16())));
        }
        parse_model_list(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn api_root_normalises_the_common_base_url_shapes() {
        for (base, root) in [
            ("https://generativelanguage.googleapis.com", "https://generativelanguage.googleapis.com/v1beta"),
            ("https://generativelanguage.googleapis.com/v1beta/models", "https://generativelanguage.googleapis.com/v1beta"),
            ("https://generativelanguage.googleapis.com/v1beta/models/", "https://generativelanguage.googleapis.com/v1beta"),
            ("https://generativelanguage.googleapis.com/v1beta", "https://generativelanguage.googleapis.com/v1beta"),
            ("https://generativelanguage.googleapis.com/v1", "https://generativelanguage.googleapis.com/v1"),
            ("https://gw.example.com/gemini", "https://gw.example.com/gemini/v1beta"),
            ("http://127.0.0.1:9000/v1alpha/", "http://127.0.0.1:9000/v1alpha"),
        ] {
            assert_eq!(api_root(&u(base)).as_str(), root, "{base}");
        }
    }

    #[test]
    fn method_urls_put_model_and_stream_flag_in_the_path_and_never_a_key() {
        let base = u("https://generativelanguage.googleapis.com/v1beta/models");
        assert_eq!(
            method_url(&base, "gemini-2.5-flash", false).as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        let s = method_url(&base, "gemini-2.5-flash", true);
        assert_eq!(s.as_str(), "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse");
        assert_eq!(s.query(), Some("alt=sse"));
        assert!(!s.as_str().contains("key="));
        // 已经是完整资源名（调优模型等）
        assert_eq!(method_url(&base, "tunedModels/my-tune", false).path(), "/v1beta/tunedModels/my-tune:generateContent");
    }

    #[test]
    fn model_list_parsing_strips_the_models_prefix() {
        let body = br#"{"models":[{"name":"models/gemini-2.5-pro"},{"name":"models/gemini-2.5-flash"},{"displayName":"x"}]}"#;
        assert_eq!(parse_model_list(body).unwrap(), vec!["gemini-2.5-pro", "gemini-2.5-flash"]);
        assert!(parse_model_list(b"<html>").is_err());
    }
}

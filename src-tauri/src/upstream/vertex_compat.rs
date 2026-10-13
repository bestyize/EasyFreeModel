//! Vertex AI 适配器，**仅 express 模式**（API Key）：`POST <host>/v1/publishers/google/models/<model>:generateContent`，
//! 流式为 `:streamGenerateContent?alt=sse`，密钥走请求头 `x-goog-api-key`。
//!
//! # 不支持什么（如实报告「尚未接入」，不伪造令牌流程）
//! - 标准模式（`v1/projects/<p>/locations/<l>/publishers/...`）需要 OAuth 访问令牌 / 服务账号，**尚未接入**。
//! - 凭据看起来是服务账号 JSON、OAuth 访问令牌（`ya29.`）或 JWT 时，请求在发出前就被拒绝（[`UpstreamError::not_ready`]），
//!   不会把这类凭据发往任何地方。
//! - express 模式没有模型列表接口，[`Adapter::list_models`] 同样报告「尚未接入」；模型由用户在提供商里手动维护。
//!
//! 请求 / 响应 body 与 Gemini 相同，方言差异（不带 functionCall id、`fileData.mimeType` 必填、占位签名）
//! 由协议层 [`crate::protocols::vertex`] 处理。

use url::Url;

use super::gemini_compat::with_path_and_query;
use super::{Adapter, AdapterKind, AuthStyle, Capabilities, UpstreamClient, UpstreamError, UpstreamRequest, UpstreamTarget};
use crate::protocols::vertex;

pub struct VertexAdapter;

const NOT_SUPPORTED: &str = "Vertex AI is only supported in express mode (an API key); Google service accounts and OAuth access tokens are not wired up yet";

/// express 模式的 API Key 不会是 JSON、也不会是 OAuth 访问令牌 / JWT。
pub fn check_express_credential(key: &str) -> Result<(), UpstreamError> {
    let k = key.trim();
    let looks_like_service_account = k.starts_with('{');
    let looks_like_oauth_token = k.starts_with("ya29.") || k.to_ascii_lowercase().starts_with("bearer ");
    let looks_like_jwt = k.starts_with("eyJ") && k.matches('.').count() == 2;
    if looks_like_service_account || looks_like_oauth_token || looks_like_jwt {
        return Err(UpstreamError::not_ready(NOT_SUPPORTED));
    }
    Ok(())
}

/// 配置层面的 express 判定（供 `routing::endpoint` 使用）：标准模式的项目 / 区域路径、Bearer 鉴权都意味着 OAuth。
pub fn check_express_config(base: &Url, auth: &AuthStyle) -> Result<(), String> {
    let p = base.path();
    if p.contains("/projects/") || p.contains("/locations/") {
        return Err("Vertex AI 标准模式（项目 / 区域路径，需要服务账号或 OAuth 访问令牌）尚未接入；目前只支持 express 模式（API Key）".into());
    }
    if matches!(auth, AuthStyle::Bearer) {
        return Err("Vertex AI 的 OAuth 访问令牌 / 服务账号鉴权尚未接入；目前只支持 express 模式（API Key，x-goog-api-key）".into());
    }
    Ok(())
}

/// `<host>[/prefix]/v1/publishers/google/models/<model>:<method>`；base 里多写的 `/v1` 会被去掉。
pub fn method_url(base: &Url, model: &str, stream: bool) -> Url {
    let trimmed = base.path().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let mut root = base.clone();
    root.set_query(None);
    root.set_fragment(None);
    root.set_path(trimmed);
    with_path_and_query(&root, &vertex::express_request_path(model, stream))
}

#[async_trait::async_trait]
impl Adapter for VertexAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Vertex
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: false }
    }

    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        method_url(&target.endpoint.base_url, "", false)
    }

    fn request_url(&self, target: &UpstreamTarget, req: &UpstreamRequest) -> Url {
        method_url(&target.endpoint.base_url, &req.model, req.stream)
    }

    fn models_url(&self, target: &UpstreamTarget) -> Url {
        self.endpoint_url(target)
    }

    fn validate_target(&self, target: &UpstreamTarget) -> Result<(), UpstreamError> {
        check_express_config(&target.endpoint.base_url, &target.endpoint.auth).map_err(|_| UpstreamError::not_ready(NOT_SUPPORTED))?;
        if let Some(k) = &target.api_key {
            check_express_credential(k.expose())?;
        }
        Ok(())
    }

    async fn list_models(&self, _client: &UpstreamClient, _target: &UpstreamTarget) -> Result<Vec<String>, UpstreamError> {
        Err(UpstreamError::not_ready("Vertex AI express mode has no model listing endpoint; add the models manually"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn express_urls() {
        assert_eq!(
            method_url(&u("https://aiplatform.googleapis.com"), "gemini-2.5-flash", false).as_str(),
            "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:generateContent"
        );
        let s = method_url(&u("https://aiplatform.googleapis.com/v1/"), "models/gemini-2.5-flash", true);
        assert_eq!(s.as_str(), "https://aiplatform.googleapis.com/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse");
        assert_eq!(
            method_url(&u("https://gw.example.com/vertex"), "m", false).as_str(),
            "https://gw.example.com/vertex/v1/publishers/google/models/m:generateContent"
        );
    }

    #[test]
    fn service_account_and_oauth_credentials_are_not_ready() {
        for bad in [
            r#"{"type":"service_account","private_key":"-----BEGIN PRIVATE KEY-----"}"#,
            "  {\"client_email\":\"x@y.iam.gserviceaccount.com\"}",
            "ya29.a0AfH6SMB-token",
            "Bearer ya29.abc",
            "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln",
        ] {
            let e = check_express_credential(bad).unwrap_err();
            assert!(e.to_string().contains(super::super::NOT_READY_MARKER), "{e}");
            assert!(!e.to_string().contains("ya29") && !e.to_string().contains("private_key"), "the credential must not be echoed: {e}");
        }
        for ok in ["AQ.Ab8RN6IexampleExpressKey", "AIzaSyExampleKey-123"] {
            check_express_credential(ok).unwrap();
        }
    }

    #[test]
    fn standard_mode_configuration_is_not_ready() {
        let hdr = AuthStyle::Header("x-goog-api-key".into());
        assert!(check_express_config(&u("https://aiplatform.googleapis.com"), &hdr).is_ok());
        assert!(check_express_config(&u("https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1"), &hdr)
            .unwrap_err()
            .contains("标准模式"));
        assert!(check_express_config(&u("https://aiplatform.googleapis.com"), &AuthStyle::Bearer).unwrap_err().contains("OAuth"));
    }
}

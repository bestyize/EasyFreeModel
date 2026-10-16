//! 提供商行 → 上游端点。
//!
//! 单一事实来源（由提供商管理侧维护）：
//! - 内置提供商：`resources/provider_status.json`（经 `providers::status` 读取）。`baseUrl` 已是完整的聊天端点
//!   （如 `https://api.openai.com/v1/chat/completions`、`https://api.anthropic.com/v1/messages`）。
//! - 自定义提供商：`providers.adapter` 与 `providers.base_url`（用户填写的 API 根，适配器按协议追加路径）。
//!
//! 转发管线支持的适配器（`AdapterKind`）：`openai-compatible`、`anthropic-compatible`、`openai-responses`、`gemini`、
//! `vertex`（仅 express / API Key 模式）、`ollama`。
//!
//! - 适配器按 id 字符串解析（`AdapterKind::from_id`），因此 `providers::status::Adapter` 将来新增 `openai-responses`
//!   变体后无需再改这里。目前 `provider_status.json` 里没有供应商使用 `openai-responses`；自定义提供商行里写了该适配器 id 的
//!   也能解析。
//! - **Vertex**：只接 express 模式。需要服务账号 / OAuth 访问令牌的配置（标准模式的项目 / 区域路径、Bearer 鉴权）如实
//!   返回 `NotReady`；凭据本身是服务账号 JSON / OAuth 令牌的情形在请求发出前由适配器拒绝（见 `upstream::vertex_compat`）。
//! - **Ollama 本机**（`ollama-local`）无需鉴权：鉴权方式为 `AuthStyle::None`，不读取也不发送任何凭据。
//!   其余 Ollama（云端、自定义）使用 `Authorization: Bearer`。

use super::repo::ProviderCfg;
use crate::providers::status::{self, AuthScheme, AuthSpec};
use crate::protocols::ProtocolKind;
use crate::upstream::endpoint::validate_base_url;
use crate::upstream::vertex_compat::check_express_config;
use crate::upstream::{AdapterKind, AuthStyle, NotReady, ResolvedEndpoint};

fn kind_for(adapter_id: &str) -> Result<AdapterKind, NotReady> {
    AdapterKind::from_id(adapter_id).ok_or_else(|| NotReady(format!("适配器 {adapter_id} 尚未在转发管线中实现")))
}

/// 本机 Ollama（内置 `ollama-local`）不需要鉴权。
const LOCAL_NO_AUTH_PROVIDER: &str = "ollama-local";

/// 这些内置端点的聊天接口只接受 `stream: true`。不影响自定义兼容端点。
const STREAM_ONLY_PROVIDERS: &[&str] = &[
    "codebuddy-cn", "codebuddy-intl", "codex", "grok-cli", "zed", "api-airforce",
    "opencode", "opencode-go", "opencode-zen",
];

fn auth_for(spec: &AuthSpec) -> AuthStyle {
    match spec {
        AuthSpec::Scheme(AuthScheme::Bearer) => AuthStyle::Bearer,
        AuthSpec::Scheme(AuthScheme::XApiKey) => AuthStyle::XApiKey,
        AuthSpec::Header { header } => AuthStyle::Header(header.clone()),
    }
}

pub fn endpoint_for(p: &ProviderCfg) -> Result<ResolvedEndpoint, NotReady> {
    if !p.adapter_ready {
        let reason = p.pending_reason.clone().filter(|r| !r.trim().is_empty());
        return Err(NotReady(reason.unwrap_or_else(|| "该提供商的适配器尚未接入".into())));
    }
    let (adapter_id, raw_url, auth): (String, String, Option<AuthStyle>) = if p.kind == "builtin" {
        let st = status::get(&p.id).ok_or_else(|| NotReady("该提供商尚未登记适配器状态".into()))?;
        let adapter = st.adapter.ok_or_else(|| NotReady(st.reason.clone().unwrap_or_else(|| "适配器尚未接入".into())))?;
        let url = st.base_url.clone().ok_or_else(|| NotReady("目录中缺少上游地址".into()))?;
        let auth = st.auth.clone().unwrap_or_else(|| status::default_auth(adapter));
        (adapter.as_str().to_string(), url, Some(auth_for(&auth)))
    } else {
        let url = p.base_url.clone().ok_or_else(|| NotReady("自定义提供商尚未配置 Base URL".into()))?;
        (p.adapter.clone(), url, None)
    };
    let kind = if p.kind == "builtin" {
        kind_for(&adapter_id)?
    } else {
        AdapterKind::from_id(&adapter_id).ok_or_else(|| NotReady("自定义提供商的协议无效".into()))?
    };
    let base_url = validate_base_url(&raw_url).map_err(NotReady)?;
    let mut auth = auth.unwrap_or_else(|| kind.default_auth());
    if p.kind == "builtin" && (p.id == LOCAL_NO_AUTH_PROVIDER
        || crate::providers::catalog::find(&p.id).is_some_and(|entry| entry.no_auth)) {
        auth = AuthStyle::None;
    }
    if kind == AdapterKind::Vertex {
        check_express_config(&base_url, &auth).map_err(NotReady)?;
    }
    let mut endpoint = ResolvedEndpoint::new(kind, base_url, auth);
    // Kimi Coding's Anthropic /messages endpoint requires the beta flag.
    // Do not attach it to unrelated custom /messages endpoints.
    if p.kind == "builtin" && p.id == "kimi" && kind == AdapterKind::AnthropicCompatible {
        endpoint.base_url.query_pairs_mut().append_pair("beta", "true");
    }
    endpoint.force_stream = p.kind == "builtin" && STREAM_ONLY_PROVIDERS.contains(&p.id.as_str());
    if endpoint.force_stream {
        // 这些端点未必接受 OpenAI 的 `stream_options` 扩展。
        endpoint.stream_usage = false;
    }
    Ok(endpoint)
}

/// A built-in provider may expose a native endpoint for a particular client protocol.
pub fn endpoint_for_client(p: &ProviderCfg, client: ProtocolKind) -> Result<Option<ResolvedEndpoint>, NotReady> {
    if p.kind != "builtin" {
        return Ok(None);
    }
    let Some(transport) = status::get(&p.id).and_then(|entry| entry.transports.get(client.id())) else {
        return Ok(None);
    };
    let kind = kind_for(transport.adapter.as_str())?;
    let base_url = validate_base_url(&transport.base_url).map_err(NotReady)?;
    Ok(Some(ResolvedEndpoint::new(kind, base_url, auth_for(&transport.auth))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_selects_native_openai_transport_without_changing_anthropic_default() {
        let glm = row("glm", "builtin", "anthropic-compatible", true, None);
        let default = endpoint_for(&glm).unwrap();
        assert_eq!(default.kind, AdapterKind::AnthropicCompatible);
        let alternate = endpoint_for_client(&glm, ProtocolKind::OpenAiChat).unwrap().unwrap();
        assert_eq!(alternate.kind, AdapterKind::OpenAiCompatible);
        assert_eq!(alternate.base_url.as_str(), "https://api.z.ai/api/coding/paas/v4/chat/completions");
        assert!(endpoint_for_client(&glm, ProtocolKind::OpenAiResponses).unwrap().is_none());
        let custom = row("glm", "custom", "anthropic-compatible", true, Some("https://example.com"));
        assert!(endpoint_for_client(&custom, ProtocolKind::OpenAiChat).unwrap().is_none());
    }

    #[test]
    fn deepseek_selects_native_anthropic_transport() {
        let deepseek = row("deepseek", "builtin", "openai-compatible", true, None);
        assert_eq!(endpoint_for(&deepseek).unwrap().kind, AdapterKind::OpenAiCompatible);
        let alternate = endpoint_for_client(&deepseek, ProtocolKind::AnthropicMessages).unwrap().unwrap();
        assert_eq!(alternate.kind, AdapterKind::AnthropicCompatible);
        assert_eq!(alternate.base_url.as_str(), "https://api.deepseek.com/anthropic/v1/messages");
    }

    #[test]
    fn github_selects_native_transports() {
        let gh = row("github", "builtin", "openai-compatible", true, None);
        assert_eq!(endpoint_for(&gh).unwrap().kind, AdapterKind::OpenAiCompatible);
        let anthropic = endpoint_for_client(&gh, ProtocolKind::AnthropicMessages).unwrap().unwrap();
        assert_eq!(anthropic.kind, AdapterKind::AnthropicCompatible);
        assert_eq!(anthropic.base_url.as_str(), "https://api.githubcopilot.com/v1/messages");
        let responses = endpoint_for_client(&gh, ProtocolKind::OpenAiResponses).unwrap().unwrap();
        assert_eq!(responses.kind, AdapterKind::OpenAiResponses);
        assert_eq!(responses.base_url.as_str(), "https://api.githubcopilot.com/responses");
    }

    #[test]
    fn kimi_selects_native_transports() {
        let kimi = row("kimi", "builtin", "anthropic-compatible", true, None);
        assert_eq!(endpoint_for(&kimi).unwrap().kind, AdapterKind::AnthropicCompatible);
        let chat = endpoint_for_client(&kimi, ProtocolKind::OpenAiChat).unwrap().unwrap();
        assert_eq!(chat.kind, AdapterKind::OpenAiCompatible);
        assert_eq!(chat.base_url.as_str(), "https://api.kimi.com/coding/v1/chat/completions");
        let responses = endpoint_for_client(&kimi, ProtocolKind::OpenAiResponses).unwrap().unwrap();
        assert_eq!(responses.kind, AdapterKind::OpenAiResponses);
        assert_eq!(responses.base_url.as_str(), "https://api.kimi.com/coding/v1/responses");
    }

    fn row(id: &str, kind: &str, adapter: &str, ready: bool, base_url: Option<&str>) -> ProviderCfg {
        ProviderCfg {
            id: id.into(),
            display_name: id.into(),
            kind: kind.into(),
            adapter: adapter.into(),
            adapter_ready: ready,
            pending_reason: Some("待接入".into()),
            base_url: base_url.map(str::to_string),
            enabled: true,
            round_robin: true,
            max_concurrency: 4,
            max_retries: 2,
        }
    }

    #[test]
    fn builtin_uses_status_file_full_endpoint() {
        let e = endpoint_for(&row("openai", "builtin", "none", true, None)).unwrap();
        assert_eq!(e.kind, AdapterKind::OpenAiCompatible);
        assert_eq!(e.base_url.as_str(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(e.auth, AuthStyle::Bearer);
        let e = endpoint_for(&row("anthropic", "builtin", "none", true, None)).unwrap();
        assert_eq!(e.kind, AdapterKind::AnthropicCompatible);
        assert_eq!(e.auth, AuthStyle::XApiKey);
        assert_eq!(e.base_url.as_str(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn stream_only_is_scoped_to_builtin_providers() {
        let builtin = endpoint_for(&row("codebuddy-cn", "builtin", "none", true, None)).unwrap();
        assert!(builtin.force_stream);
        assert!(!builtin.stream_usage);
        let custom = endpoint_for(&row("codebuddy-cn", "custom", "openai-compatible", true, Some("https://example.com/v1"))).unwrap();
        assert!(!custom.force_stream);
        let google = endpoint_for(&row("gemini-cli", "builtin", "none", true, None)).unwrap();
        assert_eq!(google.kind, AdapterKind::CodeAssist);
        assert_eq!(google.auth, AuthStyle::Bearer);
        let free = endpoint_for(&row("opencode", "builtin", "none", true, None)).unwrap();
        assert_eq!(free.auth, AuthStyle::None);
        assert!(free.force_stream);
        let paid = endpoint_for(&row("opencode-zen", "builtin", "none", true, None)).unwrap();
        assert_eq!(paid.auth, AuthStyle::Bearer);
    }

    #[test]
    fn kimi_coding_anthropic_endpoint_has_required_beta_flag() {
        let e = endpoint_for(&row("kimi", "builtin", "none", true, None)).unwrap();
        assert_eq!(e.kind, AdapterKind::AnthropicCompatible);
        assert_eq!(e.base_url.query(), Some("beta=true"));
        let custom = endpoint_for(&row("custom-kimi", "custom", "anthropic-compatible",
            true, Some("https://example.com/v1/messages"))).unwrap();
        assert!(custom.base_url.query().is_none());
    }

    #[test]
    fn pending_adapters_are_reported_honestly() {
        let p = endpoint_for(&row("claude", "builtin", "none", false, None)).unwrap_err();
        assert_eq!(p.0, "待接入");
        // 状态文件里 adapter 为 null 的内置提供商：原样给出 reason
        let p = endpoint_for(&row("grok-web", "builtin", "none", true, None)).unwrap_err();
        assert!(p.0.contains("网页会话类暂缓"), "{p}");
        // 未知适配器 id
        assert!(endpoint_for(&row("c", "custom", "bogus", true, Some("https://x.example"))).is_err());
    }

    #[test]
    fn gemini_vertex_and_ollama_are_now_wired_with_header_auth_where_required() {
        let g = endpoint_for(&row("gemini", "builtin", "none", true, None)).unwrap();
        assert_eq!(g.kind, AdapterKind::Gemini);
        assert_eq!(g.auth, AuthStyle::Header("x-goog-api-key".into()), "the key goes in a header, never in the query string");
        assert_eq!(g.base_url.as_str(), "https://generativelanguage.googleapis.com/v1beta/models");

        let v = endpoint_for(&row("vertex", "builtin", "none", true, None)).unwrap();
        assert_eq!(v.kind, AdapterKind::Vertex);
        assert_eq!(v.auth, AuthStyle::Header("x-goog-api-key".into()));
        assert_eq!(v.base_url.as_str(), "https://aiplatform.googleapis.com/");

        let cloud = endpoint_for(&row("ollama", "builtin", "none", true, None)).unwrap();
        assert_eq!((cloud.kind, cloud.auth), (AdapterKind::Ollama, AuthStyle::Bearer));
        assert_eq!(cloud.base_url.as_str(), "https://ollama.com/api/chat");

        let local = endpoint_for(&row("ollama-local", "builtin", "none", true, None)).unwrap();
        assert_eq!(local.kind, AdapterKind::Ollama);
        assert_eq!(local.auth, AuthStyle::None, "a local Ollama needs no credential");
        assert_eq!(local.base_url.as_str(), "http://localhost:11434/api/chat");
    }

    #[test]
    fn custom_providers_resolve_every_adapter_id_with_its_default_auth() {
        let u = Some("https://gw.example.com");
        for (adapter, kind, auth) in [
            ("gemini", AdapterKind::Gemini, AuthStyle::Header("x-goog-api-key".into())),
            ("ollama", AdapterKind::Ollama, AuthStyle::Bearer),
            ("openai-responses", AdapterKind::OpenAiResponses, AuthStyle::Bearer),
            ("anthropic-compatible", AdapterKind::AnthropicCompatible, AuthStyle::XApiKey),
        ] {
            let e = endpoint_for(&row("custom-x", "custom", adapter, true, u)).unwrap();
            assert_eq!((e.kind, e.auth), (kind, auth), "{adapter}");
        }
        // 自定义的本机 Ollama 仍按 Bearer（只有内置 ollama-local 免鉴权）
        let e = endpoint_for(&row("custom-y", "custom", "ollama", true, Some("http://127.0.0.1:11434"))).unwrap();
        assert_eq!(e.auth, AuthStyle::Bearer);
    }

    #[test]
    fn vertex_only_supports_express_mode() {
        // 标准模式（项目 / 区域路径 + OAuth）如实报告尚未接入
        let std_mode = endpoint_for(&row(
            "vx",
            "custom",
            "vertex",
            true,
            Some("https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/us-central1"),
        ))
        .unwrap_err();
        assert!(std_mode.0.contains("尚未接入"), "{std_mode}");
        // express：主机根即可
        let ok = endpoint_for(&row("vx", "custom", "vertex", true, Some("https://aiplatform.googleapis.com"))).unwrap();
        assert_eq!((ok.kind, ok.auth), (AdapterKind::Vertex, AuthStyle::Header("x-goog-api-key".into())));
    }

    #[test]
    fn custom_provider_uses_its_base_url_and_validates_it() {
        let e = endpoint_for(&row("custom-1", "custom", "openai-compatible", true, Some("http://127.0.0.1:9999/v1"))).unwrap();
        assert_eq!(e.kind, AdapterKind::OpenAiCompatible);
        assert_eq!(e.base_url.as_str(), "http://127.0.0.1:9999/v1");
        let a = endpoint_for(&row("custom-2", "custom", "anthropic-compatible", true, Some("https://gw.example.com"))).unwrap();
        assert_eq!(a.auth, AuthStyle::XApiKey);
        assert!(endpoint_for(&row("custom-3", "custom", "openai-compatible", true, Some("http://example.com/v1"))).is_err());
        assert!(endpoint_for(&row("custom-4", "custom", "openai-compatible", true, None)).is_err());
        assert!(endpoint_for(&row("custom-5", "custom", "bogus", true, Some("https://x.example"))).is_err());
    }
}

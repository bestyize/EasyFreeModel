//! 适配器就绪状态的单一来源：`resources/provider_status.json`（内嵌进二进制）。
//!
//! 文件形如 `{"<provider_id>": {"adapter": ..., "baseUrl": ..., "auth": ..., "reason": ...}}`：
//! - `adapter` 非空：该内置提供商可被上游转发管线使用，`baseUrl`/`auth` 必填；
//! - `adapter` 为 null：待接入，`reason` 必填，原样展示给用户（`ProviderSummary.pendingReason`）。
//!
//! `ProviderSummary.adapterReady` 完全由这个文件决定，不允许在别处硬编码。
//! 内置提供商的 `baseUrl` 是上游**完整的聊天端点**（含路径，取自 9Router registry 的 transport.baseUrl，
//! 例如 `https://api.openai.com/v1/chat/completions`），消费方不需要再拼路径。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use url::{Host, Url};

use crate::error::{AppError, AppResult};

pub const STATUS_JSON: &str = include_str!("../../resources/provider_status.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Adapter {
    #[serde(rename = "openai-compatible")]
    OpenaiCompatible,
    #[serde(rename = "anthropic-compatible")]
    AnthropicCompatible,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "vertex")]
    Vertex,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "openai-responses")]
    OpenaiResponses,
    #[serde(rename = "code-assist")]
    CodeAssist,
}

impl Adapter {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenaiCompatible => "openai-compatible",
            Self::AnthropicCompatible => "anthropic-compatible",
            Self::Gemini => "gemini",
            Self::Vertex => "vertex",
            Self::Ollama => "ollama",
            Self::OpenaiResponses => "openai-responses",
            Self::CodeAssist => "code-assist",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::OpenaiCompatible,
            Self::AnthropicCompatible,
            Self::Gemini,
            Self::Vertex,
            Self::Ollama,
            Self::OpenaiResponses,
            Self::CodeAssist,
        ]
        .into_iter()
        .find(|a| a.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`
    #[serde(rename = "bearer")]
    Bearer,
    /// `x-api-key: <key>`
    #[serde(rename = "x-api-key")]
    XApiKey,
}

/// 上游鉴权方式：预设方案，或"把原始 key 放进指定请求头"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuthSpec {
    Scheme(AuthScheme),
    Header { header: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusEntry {
    pub adapter: Option<Adapter>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
    #[serde(default)]
    pub transports: HashMap<String, TransportEntry>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportEntry {
    pub adapter: Adapter,
    pub base_url: String,
    pub auth: AuthSpec,
}

impl StatusEntry {
    pub fn is_ready(&self) -> bool {
        self.adapter.is_some()
    }
}

/// 自定义提供商按协议对应的适配器。
pub fn adapter_for_custom_protocol(protocol: crate::models::Protocol) -> Option<Adapter> {
    use crate::models::Protocol::*;
    match protocol {
        OpenaiChat => Some(Adapter::OpenaiCompatible),
        AnthropicMessages => Some(Adapter::AnthropicCompatible),
        Gemini => Some(Adapter::Gemini),
        OpenaiResponses => Some(Adapter::OpenaiResponses),
    }
}

pub fn default_auth(adapter: Adapter) -> AuthSpec {
    match adapter {
        Adapter::OpenaiCompatible | Adapter::Ollama | Adapter::OpenaiResponses | Adapter::CodeAssist => AuthSpec::Scheme(AuthScheme::Bearer),
        Adapter::AnthropicCompatible => AuthSpec::Scheme(AuthScheme::XApiKey),
        Adapter::Gemini | Adapter::Vertex => AuthSpec::Header { header: "x-goog-api-key".into() },
    }
}

/// https，或回环地址（localhost / 127.0.0.0/8 / ::1）上的 http。返回去掉尾部 `/` 的规范形式。
pub fn validate_base_url(raw: &str) -> AppResult<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(AppError::msg("Base URL 不能为空且不超过 2048 字符"));
    }
    let url = Url::parse(raw).map_err(|_| AppError::msg("Base URL 格式无效，应形如 https://api.example.com/v1"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::msg("Base URL 不能包含用户名或密码"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::msg("Base URL 不能包含查询参数或片段"));
    }
    let loopback = match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(AppError::msg("Base URL 缺少主机名")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => return Err(AppError::msg("Base URL 必须使用 https（仅回环地址 localhost / 127.0.0.1 / ::1 允许 http）")),
        _ => return Err(AppError::msg("Base URL 只支持 https（回环地址可用 http）")),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn parse_table() -> Result<HashMap<String, StatusEntry>, String> {
    let table: HashMap<String, StatusEntry> =
        serde_json::from_str(STATUS_JSON).map_err(|e| format!("provider_status.json 解析失败：{e}"))?;
    for (id, e) in &table {
        for (source, transport) in &e.transports {
            if crate::protocols::ProtocolKind::from_id(source).is_none() {
                return Err(format!("{id}: unknown transport source protocol {source}"));
            }
            validate_base_url(&transport.base_url)
                .map_err(|err| format!("{id}: transport {source} baseUrl 无效：{err}"))?;
        }
        match e.adapter {
            Some(_) => {
                let url = e.base_url.as_deref().ok_or_else(|| format!("{id}: adapter 非空时 baseUrl 必填"))?;
                validate_base_url(url).map_err(|err| format!("{id}: baseUrl 无效：{err}"))?;
                if e.auth.is_none() {
                    return Err(format!("{id}: adapter 非空时 auth 必填"));
                }
            }
            None => {
                if e.reason.as_deref().map(str::trim).unwrap_or("").is_empty() {
                    return Err(format!("{id}: adapter 为 null 时 reason 必填"));
                }
            }
        }
    }
    Ok(table)
}

fn table() -> &'static HashMap<String, StatusEntry> {
    static TABLE: OnceLock<HashMap<String, StatusEntry>> = OnceLock::new();
    // 内嵌资源在编译期就固定，unit test 会校验它；这里失败只可能是打包了损坏的文件
    TABLE.get_or_init(|| parse_table().expect("内嵌的 provider_status.json 无效"))
}

pub fn get(id: &str) -> Option<&'static StatusEntry> {
    table().get(id)
}

pub fn all() -> &'static HashMap<String, StatusEntry> {
    table()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_status_file_is_valid() {
        parse_table().unwrap();
    }

    #[test]
    fn code_assist_providers_parse_at_startup() {
        let table = parse_table().unwrap();
        for id in ["antigravity", "gemini-cli"] {
            let status = table.get(id).unwrap();
            assert_eq!(status.adapter, Some(Adapter::CodeAssist));
            assert_eq!(status.auth, Some(AuthSpec::Scheme(AuthScheme::Bearer)));
        }
    }

    #[test]
    fn base_url_validation() {
        assert_eq!(validate_base_url(" https://api.example.com/v1/ ").unwrap(), "https://api.example.com/v1");
        assert_eq!(validate_base_url("https://api.example.com").unwrap(), "https://api.example.com");
        assert_eq!(validate_base_url("http://localhost:11434").unwrap(), "http://localhost:11434");
        assert_eq!(validate_base_url("http://127.0.0.1:8080/v1").unwrap(), "http://127.0.0.1:8080/v1");
        assert_eq!(validate_base_url("http://127.5.5.5/v1").unwrap(), "http://127.5.5.5/v1");
        assert_eq!(validate_base_url("http://[::1]:9000").unwrap(), "http://[::1]:9000");
        for bad in [
            "",
            "   ",
            "api.example.com",
            "http://api.example.com/v1",
            "http://192.168.1.2/v1",
            "ftp://example.com",
            "https://",
            "https://user:pw@example.com",
            "https://example.com/v1?key=abc",
            "https://example.com/#x",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            assert!(validate_base_url(bad).is_err(), "应拒绝：{bad:?}");
        }
    }

    #[test]
    fn auth_spec_shapes_parse() {
        let a: AuthSpec = serde_json::from_str("\"bearer\"").unwrap();
        assert_eq!(a, AuthSpec::Scheme(AuthScheme::Bearer));
        let a: AuthSpec = serde_json::from_str("\"x-api-key\"").unwrap();
        assert_eq!(a, AuthSpec::Scheme(AuthScheme::XApiKey));
        let a: AuthSpec = serde_json::from_str("{\"header\":\"x-goog-api-key\"}").unwrap();
        assert_eq!(a, AuthSpec::Header { header: "x-goog-api-key".into() });
        assert!(serde_json::from_str::<AuthSpec>("\"basic\"").is_err());
    }
}

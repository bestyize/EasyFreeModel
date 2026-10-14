//! 数据驱动的授权配置（`resources/oauth_providers.json`）与客户端凭据抽象。
//!
//! 配置文件里**不允许**出现 client secret：`deny_unknown_fields` 会让任何
//! `clientSecret` 字段直接解析失败。需要 secret 的供应商写 `requiresClientSecret: true`，
//! 由用户自行填写，运行时通过 `ClientCredentialSource` 从凭据库读取。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::error::OAuthError;
use super::http::{ensure_secure_endpoint, Encoding};
use super::secret::Secret;

const EMBEDDED_CATALOG: &str = include_str!("../../resources/oauth_providers.json");
const BASE_URL_PLACEHOLDER: &str = "{base_url}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FlowKind {
    AuthorizationCode,
    Device,
    /// 非标准「state 轮询」：POST state 端点拿到 `{ state, authUrl }`，浏览器打开 authUrl，
    /// 再用 state 轮询 token 端点直到返回令牌（CodeBuddy CN / Intl 使用）。
    StatePoll,
    /// 依赖专有协议、指纹或私有登录：本框架不实现，配置里说明需要什么。
    Custom,
}

impl FlowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FlowKind::AuthorizationCode => "authorization-code",
            FlowKind::Device => "device",
            FlowKind::StatePoll => "state-poll",
            FlowKind::Custom => "custom",
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_owned()
}

fn default_path() -> String {
    "/callback".to_owned()
}

fn default_true() -> bool {
    true
}

fn default_lead() -> i64 {
    300
}

/// 回环回调地址。监听永远只绑定 127.0.0.1；`host` 只决定写进 redirect_uri 的主机名
/// （个别上游只登记了 `localhost`）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedirectConfig {
    #[serde(default = "default_host")]
    pub host: String,
    /// None = 随机端口；Some = 固定端口（上游要求精确匹配 redirect_uri 时使用）。
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default = "default_path")]
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomFlowInfo {
    /// 为什么没有硬做（面向用户的一句话）。
    pub reason: String,
    /// 要接入需要具备什么。
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthProviderConfig {
    pub id: String,
    pub flow: FlowKind,

    #[serde(default)]
    pub authorize_url: Option<String>,
    #[serde(default)]
    pub device_code_url: Option<String>,
    #[serde(default)]
    pub token_url: Option<String>,
    #[serde(default)]
    pub refresh_url: Option<String>,
    /// 设备码响应没有给出 verification_uri 时使用的回退地址。
    #[serde(default)]
    pub verification_uri: Option<String>,
    /// state-poll：POST 该地址（带 `?platform=<state_poll_platform>`）换取 `{ state, authUrl }`。
    #[serde(default)]
    pub state_url: Option<String>,
    /// state-poll：`platform` 查询参数（CodeBuddy CN 用 "CLI"，Intl 用 "ide"）。
    #[serde(default)]
    pub state_poll_platform: Option<String>,
    /// state-poll：轮询间隔（毫秒，上游给的 pollInterval）。
    #[serde(default)]
    pub state_poll_interval_ms: Option<u64>,
    /// 上游要求的附加请求头（例如 CodeBuddy 的 X-Product / X-Domain）。
    /// 仅用于个别供应商的私有协议；值会原样发送，不参与任何凭据逻辑。
    #[serde(default)]
    pub request_headers: BTreeMap<String, String>,
    /// 该供应商要求的 User-Agent。缺省时用本项目固定的 `RouterDesktop/<版本>`；
    /// 个别上游（如 CodeBuddy）按客户端身份校验，需要填写它们要求的 UA。
    #[serde(default)]
    pub user_agent: Option<String>,
    /// URL 中 `{base_url}` 占位符的默认值（例如 GitLab 自托管实例）。
    #[serde(default)]
    pub base_url_default: Option<String>,

    /// 公开的 PKCE / 设备码 clientId；None 表示必须由用户提供。
    #[serde(default)]
    pub client_id: Option<String>,
    /// true：该流程不使用 clientId（例如 CodeBuddy 的 state 轮询登录由上游自行识别），
    /// 因此缺少 clientId 不算缺凭据。
    #[serde(default)]
    pub no_client_id: bool,
    #[serde(default)]
    pub client_secret: Option<Secret>,
    #[serde(default)]
    pub requires_client_secret: bool,

    #[serde(default)]
    pub scopes: Vec<String>,
    /// authorization-code：附带 PKCE S256；device：设备码请求带 code_challenge，轮询带 code_verifier。
    #[serde(default = "default_true")]
    pub pkce: bool,
    #[serde(default)]
    pub redirect: Option<RedirectConfig>,

    /// 授权码换令牌请求的编码。
    #[serde(default)]
    pub token_encoding: Encoding,
    /// 授权码换令牌时是否附带 state（个别上游要求）。
    #[serde(default)]
    pub token_request_includes_state: bool,
    #[serde(default)]
    pub refresh_encoding: Encoding,
    /// 刷新请求附带的 scope（个别上游要求）。
    #[serde(default)]
    pub refresh_scope: Option<String>,
    #[serde(default = "default_lead")]
    pub refresh_lead_seconds: i64,
    /// false 表示上游不支持 refresh_token 授权（过期即需重新授权）。
    #[serde(default = "default_true")]
    pub supports_refresh: bool,
    /// Some(name)：刷新时把 refresh token 放进该请求头而不是请求体
    /// （CodeBuddy 用 `X-Refresh-Token`）。
    #[serde(default)]
    pub refresh_token_header: Option<String>,
    /// 刷新请求体编码（默认跟 `refreshEncoding`）。
    #[serde(default)]
    pub refresh_request_encoding: Option<Encoding>,

    #[serde(default)]
    pub extra_authorize_params: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_device_params: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_token_params: BTreeMap<String, String>,

    #[serde(default)]
    pub risk_notice: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub custom: Option<CustomFlowInfo>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CatalogFile {
    schema_version: u32,
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    providers: Vec<OAuthProviderConfig>,
}

/// 用户提供的客户端凭据（来自系统凭据库）。`Debug` 不输出 secret。
#[derive(Debug, Clone, Default)]
pub struct ClientCredentials {
    pub client_id: Option<String>,
    pub client_secret: Option<Secret>,
    /// 覆盖配置里 `{base_url}` 的值（自托管实例）。
    pub base_url: Option<String>,
}

/// 运行时从凭据库读取用户填写的客户端凭据。
#[async_trait]
pub trait ClientCredentialSource: Send + Sync {
    async fn credentials_for(&self, provider_id: &str) -> Result<ClientCredentials, OAuthError>;
}

/// 客户端凭据的写入侧：界面「配置客户端凭据」表单的后端。实现者不得在日志、错误或返回值里带入凭据内容。
#[async_trait]
pub trait ClientCredentialAdmin: ClientCredentialSource {
    /// 整体替换该供应商的客户端凭据。
    async fn save_credentials(&self, provider_id: &str, creds: &ClientCredentials) -> Result<(), OAuthError>;
    /// 删除（幂等）。
    async fn clear_credentials(&self, provider_id: &str) -> Result<(), OAuthError>;
    /// 是否已有用户配置的客户端凭据（只回答有无，不返回内容）。
    async fn has_credentials(&self, provider_id: &str) -> Result<bool, OAuthError>;
}

/// 不提供任何用户凭据：只能使用配置里公开的 clientId。
pub struct NoClientCredentials;

#[async_trait]
impl ClientCredentialSource for NoClientCredentials {
    async fn credentials_for(&self, _provider_id: &str) -> Result<ClientCredentials, OAuthError> {
        Ok(ClientCredentials::default())
    }
}

/// 内存实现，用于测试。
#[derive(Default)]
pub struct StaticClientCredentials {
    inner: Mutex<HashMap<String, ClientCredentials>>,
}

impl StaticClientCredentials {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, provider_id: &str, creds: ClientCredentials) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(provider_id.to_owned(), creds);
    }
}

#[async_trait]
impl ClientCredentialSource for StaticClientCredentials {
    async fn credentials_for(&self, provider_id: &str) -> Result<ClientCredentials, OAuthError> {
        Ok(self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider_id)
            .cloned()
            .unwrap_or_default())
    }
}

#[async_trait]
impl ClientCredentialAdmin for StaticClientCredentials {
    async fn save_credentials(&self, provider_id: &str, creds: &ClientCredentials) -> Result<(), OAuthError> {
        self.set(provider_id, creds.clone());
        Ok(())
    }

    async fn clear_credentials(&self, provider_id: &str) -> Result<(), OAuthError> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(provider_id);
        Ok(())
    }

    async fn has_credentials(&self, provider_id: &str) -> Result<bool, OAuthError> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).contains_key(provider_id))
    }
}

/// 解析后的客户端身份。
#[derive(Debug, Clone)]
pub struct ResolvedClient {
    pub client_id: String,
    pub client_secret: Option<Secret>,
}

impl OAuthProviderConfig {
    /// 合并配置里的公开 clientId 与用户凭据；缺少必需项时返回 `MissingClientCredential`。
    pub fn resolve_client(&self, creds: &ClientCredentials) -> Result<ResolvedClient, OAuthError> {
        let client_id = creds
            .client_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.client_id.clone())
            .or_else(|| self.no_client_id.then(String::new))
            .ok_or_else(|| OAuthError::MissingClientCredential { provider: self.id.clone(), field: "client_id" })?;
        let client_secret = creds
            .client_secret
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| self.client_secret.clone());
        if self.requires_client_secret && client_secret.is_none() {
            return Err(OAuthError::MissingClientCredential { provider: self.id.clone(), field: "client_secret" });
        }
        Ok(ResolvedClient { client_id, client_secret })
    }

    fn expand(&self, template: &str, creds: &ClientCredentials) -> Result<String, OAuthError> {
        if !template.contains(BASE_URL_PLACEHOLDER) {
            return Ok(template.to_owned());
        }
        let base = creds
            .base_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self.base_url_default.as_deref())
            .ok_or_else(|| OAuthError::MissingClientCredential { provider: self.id.clone(), field: "base_url" })?;
        let expanded = template.replace(BASE_URL_PLACEHOLDER, base.trim().trim_end_matches('/'));
        ensure_secure_endpoint(&expanded)?;
        Ok(expanded)
    }

    fn required(&self, value: &Option<String>, name: &str, creds: &ClientCredentials) -> Result<String, OAuthError> {
        let template = value
            .as_deref()
            .ok_or_else(|| OAuthError::Config(format!("{}: {name} is not configured", self.id)))?;
        self.expand(template, creds)
    }

    pub fn authorize_endpoint(&self, creds: &ClientCredentials) -> Result<String, OAuthError> {
        self.required(&self.authorize_url, "authorizeUrl", creds)
    }

    pub fn device_endpoint(&self, creds: &ClientCredentials) -> Result<String, OAuthError> {
        self.required(&self.device_code_url, "deviceCodeUrl", creds)
    }

    /// state-poll：换取 state 与 authUrl 的端点。
    pub fn state_endpoint(&self, creds: &ClientCredentials) -> Result<String, OAuthError> {
        self.required(&self.state_url, "stateUrl", creds)
    }

    pub fn token_endpoint(&self, creds: &ClientCredentials) -> Result<String, OAuthError> {
        self.required(&self.token_url, "tokenUrl", creds)
    }

    /// 刷新端点：优先 `refreshUrl`，否则回退 `tokenUrl`；不支持刷新时为 None。
    pub fn refresh_endpoint(&self, creds: &ClientCredentials) -> Result<Option<String>, OAuthError> {
        if !self.supports_refresh {
            return Ok(None);
        }
        match self.refresh_url.as_ref().or(self.token_url.as_ref()) {
            Some(template) => self.expand(template, creds).map(Some),
            None => Ok(None),
        }
    }

    pub fn scope_string(&self) -> String {
        self.scopes.join(" ")
    }

    fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        if id.trim().is_empty() {
            return Err("provider with empty id".into());
        }
        let url_fields = [
            ("authorizeUrl", &self.authorize_url),
            ("deviceCodeUrl", &self.device_code_url),
            ("stateUrl", &self.state_url),
            ("tokenUrl", &self.token_url),
            ("refreshUrl", &self.refresh_url),
            ("verificationUri", &self.verification_uri),
        ];
        for (name, value) in url_fields {
            let Some(raw) = value else { continue };
            let probe = if raw.contains(BASE_URL_PLACEHOLDER) {
                let base = self
                    .base_url_default
                    .as_deref()
                    .ok_or_else(|| format!("{id}: {name} uses {{base_url}} but baseUrlDefault is missing"))?;
                raw.replace(BASE_URL_PLACEHOLDER, base.trim_end_matches('/'))
            } else {
                raw.clone()
            };
            ensure_secure_endpoint(&probe).map_err(|e| format!("{id}: {name}: {e}"))?;
        }
        if self.refresh_lead_seconds < 0 {
            return Err(format!("{id}: refreshLeadSeconds must be >= 0"));
        }
        match self.flow {
            FlowKind::AuthorizationCode => {
                if self.authorize_url.is_none() || self.token_url.is_none() {
                    return Err(format!("{id}: authorization-code flow needs authorizeUrl and tokenUrl"));
                }
                let redirect = self
                    .redirect
                    .as_ref()
                    .ok_or_else(|| format!("{id}: authorization-code flow needs a redirect block"))?;
                if !matches!(redirect.host.as_str(), "127.0.0.1" | "localhost") {
                    return Err(format!("{id}: redirect.host must be 127.0.0.1 or localhost"));
                }
                if !redirect.path.starts_with('/') {
                    return Err(format!("{id}: redirect.path must start with '/'"));
                }
            }
            FlowKind::Device => {
                if self.device_code_url.is_none() || self.token_url.is_none() {
                    return Err(format!("{id}: device flow needs deviceCodeUrl and tokenUrl"));
                }
            }
            FlowKind::StatePoll => {
                if self.state_url.is_none() || self.token_url.is_none() {
                    return Err(format!("{id}: state-poll flow needs stateUrl and tokenUrl"));
                }
            }
            FlowKind::Custom => {
                if self.custom.is_none() {
                    return Err(format!("{id}: custom flow must document what it needs in `custom`"));
                }
            }
        }
        Ok(())
    }
}

/// 已加载并校验的授权配置集合。
#[derive(Debug, Clone)]
pub struct OAuthCatalog {
    providers: Vec<OAuthProviderConfig>,
}

impl OAuthCatalog {
    /// 内嵌的 `oauth_providers.json`。解析失败说明发布物损坏，调用方应当报错而不是忽略。
    pub fn builtin() -> Result<Self, OAuthError> {
        Self::from_json(EMBEDDED_CATALOG)
    }

    /// 进程内只解析一次的内嵌目录（路由热路径与连接测试使用）。内嵌文件由单元测试保证有效。
    pub fn builtin_shared() -> Result<&'static OAuthCatalog, OAuthError> {
        static SHARED: OnceLock<Result<OAuthCatalog, OAuthError>> = OnceLock::new();
        SHARED.get_or_init(OAuthCatalog::builtin).as_ref().map_err(Clone::clone)
    }

    pub fn from_json(json: &str) -> Result<Self, OAuthError> {
        let file: CatalogFile =
            serde_json::from_str(json).map_err(|e| OAuthError::Config(format!("oauth_providers.json: {e}")))?;
        if file.schema_version != 1 {
            return Err(OAuthError::Config(format!("unsupported schemaVersion {}", file.schema_version)));
        }
        let mut seen = HashSet::new();
        for provider in &file.providers {
            if !seen.insert(provider.id.clone()) {
                return Err(OAuthError::Config(format!("duplicate provider id {}", provider.id)));
            }
            provider.validate().map_err(OAuthError::Config)?;
        }
        Ok(OAuthCatalog { providers: file.providers })
    }

    pub fn get(&self, id: &str) -> Option<&OAuthProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn all(&self) -> &[OAuthProviderConfig] {
        &self.providers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const PROVIDER_CATALOG: &str = include_str!("../../resources/provider_catalog.json");

    fn oauth_ids_in_catalog() -> Vec<(String, Option<String>, Option<String>)> {
        let catalog: Vec<Value> = serde_json::from_str(PROVIDER_CATALOG).expect("provider_catalog.json parses");
        catalog
            .iter()
            .filter(|p| p["hasOAuth"] == Value::Bool(true))
            .map(|p| {
                (
                    p["id"].as_str().unwrap().to_owned(),
                    p["oauthFlow"].as_str().map(str::to_owned),
                    p["riskNotice"].as_str().map(str::to_owned),
                )
            })
            .collect()
    }

    #[test]
    fn embedded_catalog_parses_and_validates() {
        let catalog = OAuthCatalog::builtin().expect("embedded oauth_providers.json must be valid");
        assert!(!catalog.all().is_empty());
    }

    #[test]
    fn covers_exactly_the_25_oauth_providers_of_the_catalog() {
        let expected = oauth_ids_in_catalog();
        assert_eq!(expected.len(), 25, "catalog is expected to flag 25 OAuth providers");
        let catalog = OAuthCatalog::builtin().unwrap();
        let mut have: Vec<&str> = catalog.all().iter().map(|p| p.id.as_str()).collect();
        let mut want: Vec<&str> = expected.iter().map(|(id, _, _)| id.as_str()).collect();
        have.sort_unstable();
        want.sort_unstable();
        assert_eq!(have, want);
    }

    #[test]
    fn standard_flows_agree_with_catalog_and_risk_notices_are_kept() {
        let catalog = OAuthCatalog::builtin().unwrap();
        for (id, flow, risk) in oauth_ids_in_catalog() {
            let cfg = catalog.get(&id).unwrap();
            if cfg.flow != FlowKind::Custom {
                let catalog_flow = flow.as_deref().unwrap_or("other");
                assert!(
                    catalog_flow == cfg.flow.as_str() || catalog_flow == "other",
                    "{id}: catalog says {catalog_flow}, config says {}",
                    cfg.flow.as_str()
                );
            }
            if risk.is_some() {
                assert!(cfg.risk_notice.is_some(), "{id} has a risk notice in the catalog but not in oauth_providers.json");
            }
        }
    }

    #[test]
    fn custom_providers_are_exactly_the_proprietary_ones() {
        let catalog = OAuthCatalog::builtin().unwrap();
        let mut custom: Vec<&str> = catalog
            .all()
            .iter()
            .filter(|p| p.flow == FlowKind::Custom)
            .map(|p| p.id.as_str())
            .collect();
        custom.sort_unstable();
        assert_eq!(
            custom,
            [
                "cline", "clinepass", "cursor", "glm", "iflow", "kilocode",
                "kimchi", "kiro", "qoder", "qoder-cn", "xiaomi-mimo", "zed"
            ]
        );
        for p in catalog.all().iter().filter(|p| p.flow == FlowKind::Custom) {
            let info = p.custom.as_ref().unwrap();
            assert!(!info.reason.is_empty() && !info.requires.is_empty(), "{} must say what it needs", p.id);
        }
        // state-poll：非标准 state 轮询（CodeBuddy CN / Intl）
        let mut state_poll: Vec<&str> = catalog
            .all()
            .iter()
            .filter(|p| p.flow == FlowKind::StatePoll)
            .map(|p| p.id.as_str())
            .collect();
        state_poll.sort_unstable();
        assert_eq!(state_poll, ["codebuddy-cn", "codebuddy-intl"]);
        let standard: Vec<&str> = catalog
            .all()
            .iter()
            .filter(|p| p.flow != FlowKind::Custom)
            .map(|p| p.id.as_str())
            .collect();
        // 标准流程（authorization-code + device + state-poll）；13 = 11 + 2 家 state-poll
        assert_eq!(standard.len(), 13);
    }

    #[test]
    fn builtin_clients_resolve_for_antigravity_and_gemini_cli() {
        let catalog = OAuthCatalog::builtin().unwrap();
        let none = ClientCredentials::default();
        for id in ["antigravity", "gemini-cli"] {
            let cfg = catalog.get(id).unwrap();
            let resolved = cfg.resolve_client(&none).unwrap();
            assert!(!resolved.client_id.is_empty(), "{id}");
            assert!(resolved.client_secret.is_some(), "{id}");
        }
    }

    #[test]
    fn every_standard_provider_uses_secure_endpoints_and_valid_redirects() {
        let catalog = OAuthCatalog::builtin().unwrap();
        for p in catalog.all().iter().filter(|p| p.flow != FlowKind::Custom) {
            for url in [&p.authorize_url, &p.device_code_url, &p.token_url, &p.refresh_url].into_iter().flatten() {
                assert!(url.starts_with("https://") || url.starts_with("{base_url}"), "{}: {url}", p.id);
            }
            if p.flow == FlowKind::AuthorizationCode {
                assert!(p.pkce || p.id == "iflow", "{} should use PKCE", p.id);
                assert!(p.redirect.is_some());
            }
        }
        assert_eq!(catalog.get("codex").unwrap().redirect.as_ref().unwrap().port, Some(1455));
    }

    #[test]
    fn rejects_bad_configs() {
        let wrap = |provider: &str| format!(r#"{{"schemaVersion":1,"providers":[{provider}]}}"#);
        // http 端点
        assert!(OAuthCatalog::from_json(&wrap(
            r#"{"id":"a","flow":"device","deviceCodeUrl":"http://evil.example/d","tokenUrl":"https://x/t"}"#
        ))
        .is_err());
        // 缺少必需字段
        assert!(OAuthCatalog::from_json(&wrap(r#"{"id":"a","flow":"authorization-code"}"#)).is_err());
        assert!(OAuthCatalog::from_json(&wrap(r#"{"id":"a","flow":"custom"}"#)).is_err());
        // 重复 id
        let dup = r#"{"id":"a","flow":"custom","custom":{"reason":"r","requires":[]}}"#;
        assert!(OAuthCatalog::from_json(&wrap(&format!("{dup},{dup}"))).is_err());
        // 未知字段、坏 JSON、坏版本
        assert!(OAuthCatalog::from_json(&wrap(r#"{"id":"a","flow":"device","bogus":1}"#)).is_err());
        assert!(OAuthCatalog::from_json("{not json").is_err());
        assert!(OAuthCatalog::from_json(r#"{"schemaVersion":2,"providers":[]}"#).is_err());
        // redirect.host 只能是回环
        assert!(OAuthCatalog::from_json(&wrap(
            r#"{"id":"a","flow":"authorization-code","authorizeUrl":"https://x/a","tokenUrl":"https://x/t","redirect":{"host":"example.com"}}"#
        ))
        .is_err());
    }

    #[test]
    fn client_resolution_requires_user_supplied_credentials_for_gitlab() {
        let catalog = OAuthCatalog::builtin().unwrap();
        let gitlab = catalog.get("gitlab").unwrap();
        let none = ClientCredentials::default();
        assert!(matches!(
            gitlab.resolve_client(&none),
            Err(OAuthError::MissingClientCredential { field: "client_id", .. })
        ));
        let full = ClientCredentials {
            client_id: Some("my-id".into()),
            client_secret: Some(Secret::new("my-secret")),
            base_url: None,
        };
        let resolved = gitlab.resolve_client(&full).unwrap();
        assert_eq!(resolved.client_id, "my-id");
        assert!(!format!("{resolved:?}").contains("my-secret"));
        assert!(!format!("{full:?}").contains("my-secret"));

        // 内置客户端凭据的供应商直接解析就绪，无需手动输入
        assert!(catalog.get("claude").unwrap().resolve_client(&none).is_ok());
        assert!(catalog.get("antigravity").unwrap().resolve_client(&none).is_ok());
        assert!(catalog.get("gemini-cli").unwrap().resolve_client(&none).is_ok());
    }

    #[test]
    fn base_url_placeholder_expands_and_is_validated() {
        let catalog = OAuthCatalog::builtin().unwrap();
        let gitlab = catalog.get("gitlab").unwrap();
        let none = ClientCredentials::default();
        assert_eq!(gitlab.token_endpoint(&none).unwrap(), "https://gitlab.com/oauth/token");
        let own = ClientCredentials { base_url: Some("https://git.example.org/".into()), ..Default::default() };
        assert_eq!(gitlab.authorize_endpoint(&own).unwrap(), "https://git.example.org/oauth/authorize");
        let insecure = ClientCredentials { base_url: Some("http://git.example.org".into()), ..Default::default() };
        assert!(gitlab.token_endpoint(&insecure).is_err());
    }

    #[test]
    fn refresh_endpoint_rules() {
        let catalog = OAuthCatalog::builtin().unwrap();
        let none = ClientCredentials::default();
        assert!(catalog.get("muse").unwrap().refresh_endpoint(&none).unwrap().is_none());
        assert!(catalog.get("claude").unwrap().refresh_endpoint(&none).unwrap().is_some());
    }
}

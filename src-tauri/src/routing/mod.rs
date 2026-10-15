//! 路由：模型解析、账号调度、健康 / 冷却、重试回退（PRODUCT_DESIGN §5.1）。
//!
//! - [`Runtime`]：转发管线的共享状态（数据库、凭据库、出站客户端、健康表、调度游标、并发信号量、端点注册表）。
//! - [`resolve`]：模型名解析（别名 / `供应商/模型` / 裸上游 ID；二义时 400 并列出候选）。
//! - [`scheduler`]：按 priority / weight / roundRobin 排出尝试顺序。
//! - [`health`]：内存健康状态与冷却。
//! - [`dispatch`]：并发限制 + 逐账号尝试 + 失败分类 + 回退（只在尚未向客户端写出任何字节前回退）。
//! - [`repo`]：本模块需要的读库查询。

pub mod dispatch;
pub mod endpoint;
pub mod health;
pub mod probe;
pub mod repo;
pub mod resolve;
pub mod scheduler;

#[cfg(test)]
mod oauth_tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::Semaphore;

use crate::error::{AppError, AppResult};
use crate::gateway::VerifiedKey;
use crate::oauth::{exchange, OAuthError, OAuthService};
use crate::protocols::{ApiError, ApiErrorKind};
use crate::security::vault::{self, CredentialVault};
use crate::store::settings as settings_repo;
use crate::upstream::{ResolvedEndpoint, Secret, UpstreamClient, UpstreamError, UpstreamTarget};

use self::health::Health;
use self::repo::{ConnCfg, ModelCfg, ProviderCfg};
use self::resolve::{ModelEntry, ResolveError};
use self::scheduler::Scheduler;

/// 转发管线的共享运行时。构造后以 `Arc` 交给网关（见 `GatewayManager::set_runtime`）。
pub struct Runtime {
    pub(crate) db: SqlitePool,
    pub(crate) vault: Arc<dyn CredentialVault>,
    pub(crate) client: UpstreamClient,
    pub(crate) health: Health,
    pub(crate) scheduler: Scheduler,
    /// OAuth 令牌服务：kind=oauth 的连接据此取得（必要时先刷新）访问令牌。未注入时 OAuth 连接不可用。
    oauth: Option<Arc<OAuthService>>,
    semaphores: Mutex<HashMap<String, (usize, Arc<Semaphore>)>>,
    /// 拒绝过 `stream_options` 的提供商（运行期学到的；进程重启后重新探测）。
    stream_usage_off: Mutex<HashSet<String>>,
}

/// 一次模型解析的结果。
#[derive(Debug, Clone)]
pub struct Resolved {
    pub provider: ProviderCfg,
    pub model: ModelCfg,
    pub endpoint: ResolvedEndpoint,
}

/// `/v1/models` 的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicModel {
    pub id: String,
    pub provider_id: String,
    pub provider_name: String,
}

struct Catalog {
    entries: Vec<ModelEntry>,
    providers: HashMap<String, ProviderCfg>,
    endpoints: HashMap<String, ResolvedEndpoint>,
}

#[derive(Debug)]
pub enum TargetError {
    /// 连接没有可用凭据（未设置，或凭据库里没有）。
    MissingCredential,
    /// 凭据库访问失败（消息不含密钥内容）。
    Vault(String),
    /// 该提供商授权后的令牌兑换步骤（github -> Copilot 令牌 / muse -> Model API Key）尚未接入：
    /// 不得把账户令牌当模型 Key 发给上游。
    ExchangePending(&'static str),
    /// OAuth 令牌已失效（refresh token 被撤销 / 过期，或没有令牌）：需要用户重新授权。
    ReauthRequired,
    /// 令牌刷新遇到瞬时故障（网络 / 5xx / 429），凭据保留。
    TokenTransient,
    /// 其它 OAuth 失败；只带稳定的错误码，不带任何服务端文本。
    OAuth(&'static str),
    /// 自定义请求头的秘密值丢失。
    MissingHeader(String),
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::MissingCredential => f.write_str("the connection has no stored credential"),
            TargetError::Vault(m) => write!(f, "the credential vault is unavailable: {m}"),
            TargetError::ExchangePending(note) => f.write_str(note),
            TargetError::ReauthRequired => f.write_str("the OAuth connection needs to be re-authorized"),
            TargetError::TokenTransient => f.write_str("the OAuth token could not be refreshed right now"),
            TargetError::OAuth(code) => write!(f, "the OAuth token is unavailable ({code})"),
            TargetError::MissingHeader(h) => write!(f, "the secret value of custom header \"{h}\" is missing from the credential vault"),
        }
    }
}

impl Runtime {
    pub fn new(db: SqlitePool, vault: Arc<dyn CredentialVault>) -> AppResult<Self> {
        let client = UpstreamClient::new().map_err(|e| AppError::msg(format!("无法创建出站 HTTP 客户端：{e}")))?;
        Ok(Self::with_client(db, vault, client))
    }

    pub fn with_client(db: SqlitePool, vault: Arc<dyn CredentialVault>, client: UpstreamClient) -> Self {
        Self {
            db,
            vault,
            client,
            health: Health::new(),
            scheduler: Scheduler::new(),
            oauth: None,
            semaphores: Mutex::new(HashMap::new()),
            stream_usage_off: Mutex::new(HashSet::new()),
        }
    }

    /// 注入 OAuth 令牌服务（AppState 构造时调用）。
    pub fn with_oauth(mut self, oauth: Arc<OAuthService>) -> Self {
        self.oauth = Some(oauth);
        self
    }

    pub(crate) fn stream_usage_disabled(&self, provider_id: &str) -> bool {
        self.stream_usage_off.lock().unwrap_or_else(|e| e.into_inner()).contains(provider_id)
    }

    pub(crate) fn disable_stream_usage(&self, provider_id: &str) {
        self.stream_usage_off.lock().unwrap_or_else(|e| e.into_inner()).insert(provider_id.to_string());
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    pub fn db(&self) -> &SqlitePool {
        &self.db
    }

    /// 上游请求超时（来自设置页的 `timeoutSeconds`）。
    pub async fn request_timeout(&self) -> Duration {
        match settings_repo::load(&self.db).await {
            Ok(s) => Duration::from_secs(u64::from(s.timeout_seconds.max(1))),
            Err(_) => Duration::from_secs(120),
        }
    }

    async fn load_catalog(&self) -> AppResult<Catalog> {
        let providers = repo::list_enabled_providers(&self.db).await?;
        let models = repo::list_enabled_models(&self.db).await?;
        let mut provider_map = HashMap::new();
        let mut endpoints = HashMap::new();
        let mut readiness: HashMap<String, Result<(), String>> = HashMap::new();
        for p in providers {
            let ready = match endpoint::endpoint_for(&p) {
                Ok(e) => {
                    endpoints.insert(p.id.clone(), e);
                    Ok(())
                }
                Err(e) => Err(e.0),
            };
            readiness.insert(p.id.clone(), ready);
            provider_map.insert(p.id.clone(), p);
        }
        let entries = models
            .into_iter()
            .map(|m| {
                let ready = readiness.get(&m.provider_id).cloned().unwrap_or_else(|| Err("provider is disabled".into()));
                ModelEntry { provider_alias: resolve::catalog_alias(&m.provider_id), model: m, ready }
            })
            .collect();
        Ok(Catalog { entries, providers: provider_map, endpoints })
    }

    /// 解析客户端请求的模型名，并执行密钥的 `allowed_models`。
    pub async fn resolve(&self, requested: &str, key: &VerifiedKey) -> Result<Resolved, ApiError> {
        let catalog = self.load_catalog().await.map_err(|_| {
            ApiError::new(ApiErrorKind::Internal, "Failed to read the model catalog.").with_code("catalog_unavailable")
        })?;
        match resolve::resolve(requested, &catalog.entries, &key.allowed_models) {
            Ok(i) => {
                let entry = &catalog.entries[i];
                let provider = catalog.providers.get(&entry.model.provider_id).cloned();
                let endpoint = catalog.endpoints.get(&entry.model.provider_id).cloned();
                match (provider, endpoint) {
                    (Some(provider), Some(endpoint)) => Ok(Resolved { provider, model: entry.model.clone(), endpoint }),
                    _ => Err(ApiError::new(ApiErrorKind::Internal, "The model catalog is inconsistent.").with_code("catalog_inconsistent")),
                }
            }
            Err(ResolveError::NotFound) => Err(ApiError::new(
                ApiErrorKind::NotFound,
                format!("The model `{}` does not exist or is not enabled.", truncate(requested, 120)),
            )
            .with_code("model_not_found")),
            Err(ResolveError::Ambiguous { candidates }) => Err(ApiError::new(
                ApiErrorKind::InvalidRequest,
                format!(
                    "The model name `{}` is ambiguous: it matches {} models ({}). Use one of these exact names.",
                    truncate(requested, 120),
                    candidates.len(),
                    candidates.join(", ")
                ),
            )
            .with_code("ambiguous_model")),
            Err(ResolveError::NotAllowed) => Err(ApiError::new(
                ApiErrorKind::PermissionDenied,
                format!("This API key is not allowed to use the model `{}`.", truncate(requested, 120)),
            )
            .with_code("model_not_allowed")),
            Err(ResolveError::NotReady { provider_id, reason }) => Err(ApiError::new(
                ApiErrorKind::NotImplemented,
                format!("The provider `{provider_id}` has no usable upstream adapter yet: {reason}"),
            )
            .with_code("upstream_adapter_not_implemented")),
        }
    }

    /// `/v1/models`：启用且适配器就绪的提供商的启用模型，经密钥白名单过滤。
    /// 别名在已启用模型间重复（理论上保存时已阻止）时，退回 `提供商/上游模型` 这个一定可寻址的名字。
    pub async fn list_public_models(&self, key: &VerifiedKey) -> AppResult<Vec<PublicModel>> {
        let catalog = self.load_catalog().await?;
        let visible: Vec<&ModelEntry> =
            catalog.entries.iter().filter(|e| e.ready.is_ok() && resolve::is_allowed(&key.allowed_models, e)).collect();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for e in &visible {
            *counts.entry(e.public_name()).or_default() += 1;
        }
        Ok(visible
            .into_iter()
            .map(|e| {
                let name = e.public_name();
                let id = if counts.get(&name).copied().unwrap_or(0) > 1 { e.qualified_name() } else { name };
                let provider_name = catalog.providers.get(&e.model.provider_id).map(|p| p.display_name.clone()).unwrap_or_default();
                PublicModel { id, provider_id: e.model.provider_id.clone(), provider_name }
            })
            .collect())
    }

    /// 提供商的并发信号量。上限变化时换成新的（旧信号量上的在途请求不受影响）。
    pub(crate) fn semaphore(&self, provider_id: &str, limit: usize) -> Arc<Semaphore> {
        let limit = limit.clamp(1, 4096);
        let mut g = self.semaphores.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(provider_id) {
            Some((l, s)) if *l == limit => s.clone(),
            _ => {
                let s = Arc::new(Semaphore::new(limit));
                g.insert(provider_id.to_string(), (limit, s.clone()));
                s
            }
        }
    }

    /// OAuth 连接的访问令牌：未到刷新窗口直接返回，否则先刷新（同账户并发去重）；`force` 无视有效期强制刷新
    /// （上游返回 401 之后）。调用方须先排除需要令牌兑换步骤的提供商（见 `build_target_with`）。
    async fn oauth_access_token(&self, provider_id: &str, conn: &ConnCfg, force: bool) -> Result<Secret, TargetError> {
        let service = self.oauth.as_ref().ok_or(TargetError::OAuth("unavailable"))?;
        let cref = conn.credential_ref.as_deref().ok_or(TargetError::ReauthRequired)?;
        let result = if force {
            service.force_refresh(provider_id, cref).await
        } else {
            service.get_valid_token(provider_id, cref).await
        };
        match result {
            Ok(tokens) => Ok(Secret::new(tokens.access_token.expose())),
            Err(e) => Err(classify_oauth_error(&e)),
        }
    }

    /// 读取凭据库，组装一次出站调用所需的全部信息。
    pub(crate) async fn build_target(
        &self,
        provider: &ProviderCfg,
        endpoint: &ResolvedEndpoint,
        conn: &ConnCfg,
        headers: &[repo::HeaderCfg],
        timeout: Duration,
    ) -> Result<UpstreamTarget, TargetError> {
        self.build_target_with(provider, endpoint, conn, headers, timeout, false).await
    }

    /// 同 [`build_target`]；`force_refresh` 对 OAuth 连接无视有效期强制刷新令牌。
    pub(crate) async fn build_target_with(
        &self,
        provider: &ProviderCfg,
        endpoint: &ResolvedEndpoint,
        conn: &ConnCfg,
        headers: &[repo::HeaderCfg],
        timeout: Duration,
        force_refresh: bool,
    ) -> Result<UpstreamTarget, TargetError> {
        let is_oauth = conn.kind == "oauth";
        let api_key = if is_oauth {
            // 兑换步骤未接入的提供商即使不需要密钥头也要如实拒绝
            if let Some(note) = exchange::token_exchange_note(&provider.id) {
                return Err(TargetError::ExchangePending(note));
            }
            if endpoint.auth.needs_key() {
                Some(self.oauth_access_token(&provider.id, conn, force_refresh).await?)
            } else {
                None
            }
        } else if endpoint.auth.needs_key() {
            let cref = conn.credential_ref.as_deref().ok_or(TargetError::MissingCredential)?;
            match vault::get(&self.vault, cref).await {
                Ok(Some(k)) if !k.trim().is_empty() => Some(Secret::new(k.trim())),
                Ok(_) => return Err(TargetError::MissingCredential),
                Err(e) => return Err(TargetError::Vault(e.to_string())),
            }
        } else {
            None
        };
        let mut extra = Vec::new();
        for h in headers {
            let value = if h.secret {
                let cref = h.credential_ref.as_deref().ok_or_else(|| TargetError::MissingHeader(h.header_key.clone()))?;
                match vault::get(&self.vault, cref).await {
                    Ok(Some(v)) => v,
                    Ok(None) => return Err(TargetError::MissingHeader(h.header_key.clone())),
                    Err(e) => return Err(TargetError::Vault(e.to_string())),
                }
            } else {
                h.value.clone().unwrap_or_default()
            };
            extra.push((h.header_key.clone(), Secret::new(value)));
        }
        Ok(UpstreamTarget {
            provider_id: provider.id.clone(),
            connection_id: conn.id.clone(),
            endpoint: endpoint.clone(),
            api_key,
            extra_headers: extra,
            timeout,
        })
    }
}

/// 令牌服务的失败 -> 路由层处理方式：需重新授权 / 瞬时故障（保留凭据，按瞬时错误回退）/ 其它。
fn classify_oauth_error(e: &OAuthError) -> TargetError {
    if e.requires_reauth() {
        TargetError::ReauthRequired
    } else if e.is_retryable() {
        TargetError::TokenTransient
    } else {
        TargetError::OAuth(e.code())
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// 上游错误 → 面向客户端的说明（固定文案，不含 URL 或任何来自上游的内容）。
pub(crate) fn describe_upstream_error(e: &UpstreamError) -> (ApiErrorKind, &'static str) {
    match e {
        UpstreamError::Timeout => (ApiErrorKind::Timeout, "The upstream provider did not respond in time."),
        UpstreamError::Connect => (ApiErrorKind::BadGateway, "The gateway could not connect to the upstream provider."),
        UpstreamError::Network(_) => (ApiErrorKind::BadGateway, "The connection to the upstream provider failed."),
        UpstreamError::Malformed(_) => (ApiErrorKind::BadGateway, "The upstream provider returned a malformed response."),
        UpstreamError::Config(_) => (ApiErrorKind::Internal, "The upstream connection is misconfigured."),
    }
}

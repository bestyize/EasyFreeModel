//! `OAuthService`：对界面/数据层暴露的统一入口。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use reqwest::Client;
use tokio::sync::watch;

use super::config::{ClientCredentialSource, FlowKind, OAuthCatalog, OAuthProviderConfig};
use super::device::{self, DeviceSession};
use super::error::OAuthError;
use super::flow::{self, AuthSession};
use super::http;
use super::refresh;
use super::state_poll::{self, StateSession};
use super::token::{TokenSet, TokenStore};

/// 超时与轮询节奏。默认值面向生产；测试可缩短。
#[derive(Debug, Clone)]
pub struct OAuthTuning {
    /// 等待浏览器回调的最长时间。
    pub callback_timeout: Duration,
    pub http_timeout: Duration,
    /// Some = 忽略服务端 interval，固定使用该间隔（测试用）。
    pub device_interval_override: Option<Duration>,
    /// 收到 slow_down 后增加的间隔（RFC 8628 为 5 秒）。
    pub slow_down_step: Duration,
}

impl Default for OAuthTuning {
    fn default() -> Self {
        OAuthTuning {
            callback_timeout: Duration::from_secs(300),
            http_timeout: Duration::from_secs(30),
            device_interval_override: None,
            slow_down_step: Duration::from_secs(5),
        }
    }
}

/// 取消等待中的授权（回环回调或设备码轮询）。
#[derive(Clone)]
pub struct CancelHandle {
    tx: Arc<watch::Sender<bool>>,
}

impl CancelHandle {
    pub fn new() -> Self {
        CancelHandle { tx: Arc::new(watch::channel(false).0) }
    }

    pub fn cancel(&self) {
        self.tx.send_replace(true);
    }

    pub(super) async fn cancelled(&self) {
        let mut rx = self.tx.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

impl Default for CancelHandle {
    fn default() -> Self {
        Self::new()
    }
}

type RefreshResult = Result<TokenSet, OAuthError>;
type Inflight = Arc<Mutex<HashMap<String, watch::Receiver<Option<RefreshResult>>>>>;

/// 同一 key 正在刷新时，后来者等待领头者的结果，只发一次上游请求。
struct Leader {
    inflight: Inflight,
    key: String,
    tx: Option<watch::Sender<Option<RefreshResult>>>,
}

impl Leader {
    fn finish(mut self, result: RefreshResult) {
        // 先摘掉登记，再广播结果；令牌已在广播前写入存储。
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
        if let Some(tx) = self.tx.take() {
            tx.send_replace(Some(result));
        }
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        // 领头者被中途取消（future 被丢弃）：撤销登记，让等待者重新竞选。
        if self.tx.is_some() {
            self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
        }
    }
}

enum Role {
    Leader(Leader),
    Follower(watch::Receiver<Option<RefreshResult>>),
}

pub struct OAuthService {
    http: Client,
    catalog: Arc<OAuthCatalog>,
    credentials: Arc<dyn ClientCredentialSource>,
    store: Arc<dyn TokenStore>,
    tuning: OAuthTuning,
    inflight: Inflight,
}

impl OAuthService {
    pub fn new(
        catalog: OAuthCatalog,
        credentials: Arc<dyn ClientCredentialSource>,
        store: Arc<dyn TokenStore>,
        tuning: OAuthTuning,
    ) -> Result<Self, OAuthError> {
        Ok(OAuthService {
            http: http::build_client(tuning.http_timeout)?,
            catalog: Arc::new(catalog),
            credentials,
            store,
            tuning,
            inflight: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 使用内嵌的 `oauth_providers.json` 与默认节奏。
    pub fn with_builtin_catalog(
        credentials: Arc<dyn ClientCredentialSource>,
        store: Arc<dyn TokenStore>,
    ) -> Result<Self, OAuthError> {
        Self::new(OAuthCatalog::builtin()?, credentials, store, OAuthTuning::default())
    }

    pub fn catalog(&self) -> &OAuthCatalog {
        &self.catalog
    }

    /// 令牌存储（授权成功后写入新连接的令牌条目、断开时删除）。
    pub fn token_store(&self) -> &Arc<dyn TokenStore> {
        &self.store
    }

    /// Exchange an account login token for the credential accepted by its inference API.
    pub async fn prepare_inference_tokens(&self, provider_id: &str, tokens: TokenSet) -> Result<TokenSet, OAuthError> {
        if !matches!(provider_id, "nous" | "github" | "muse") {
            return Ok(tokens);
        }
        let cfg = self.catalog.get(provider_id).ok_or_else(|| OAuthError::UnknownProvider(provider_id.into()))?;
        let creds = self.credentials.credentials_for(provider_id).await?;
        match provider_id {
            "nous" => super::nous::mint_agent_key(&self.http, cfg, &creds, tokens).await,
            "github" => super::exchange::github_copilot_token(&self.http, cfg, &creds, tokens).await,
            "muse" => super::exchange::muse_model_key(&self.http, cfg, &creds, tokens).await,
            _ => unreachable!(),
        }
    }

    fn inference_exchange_needed(provider_id: &str, tokens: &TokenSet, lead_seconds: i64) -> bool {
        match provider_id {
            "nous" => tokens.extra_value("nous_agent_key").is_none(),
            "github" => super::exchange::copilot_token_needs_exchange(tokens, Utc::now(), lead_seconds),
            "muse" => tokens.extra_value("muse_oauth_token").is_none(),
            _ => false,
        }
    }

    fn provider(&self, provider_id: &str, expected: FlowKind) -> Result<&OAuthProviderConfig, OAuthError> {
        let cfg = self
            .catalog
            .get(provider_id)
            .ok_or_else(|| OAuthError::UnknownProvider(provider_id.to_owned()))?;
        if cfg.flow == FlowKind::Custom {
            let reason = cfg.custom.as_ref().map(|c| c.reason.clone()).unwrap_or_default();
            return Err(OAuthError::CustomFlowRequired { provider: cfg.id.clone(), reason });
        }
        if cfg.flow != expected {
            return Err(OAuthError::UnsupportedFlow { provider: cfg.id.clone(), expected: expected.as_str() });
        }
        Ok(cfg)
    }

    /// 启动 authorization-code 流程：绑定 127.0.0.1 回调端口并生成授权地址。
    pub async fn start_authorization(&self, provider_id: &str) -> Result<AuthSession, OAuthError> {
        let cfg = self.provider(provider_id, FlowKind::AuthorizationCode)?.clone();
        let creds = self.credentials.credentials_for(provider_id).await?;
        flow::start(self.http.clone(), cfg, creds, &self.tuning).await
    }

    /// 启动设备码流程：请求设备码，返回 user_code / verification_uri。
    pub async fn start_device(&self, provider_id: &str) -> Result<DeviceSession, OAuthError> {
        let cfg = self.provider(provider_id, FlowKind::Device)?.clone();
        let creds = self.credentials.credentials_for(provider_id).await?;
        device::start(self.http.clone(), cfg, creds, &self.tuning, CancelHandle::new()).await
    }

    /// 启动 state-poll 流程（CodeBuddy）：POST state 端点拿到 authUrl，后台轮询令牌。
    pub async fn start_state_poll(&self, provider_id: &str) -> Result<StateSession, OAuthError> {
        let cfg = self.provider(provider_id, FlowKind::StatePoll)?.clone();
        let creds = self.credentials.credentials_for(provider_id).await?;
        state_poll::start(self.http.clone(), cfg, creds, &self.tuning, CancelHandle::new()).await
    }

    /// 取得可用的令牌：未到刷新窗口直接返回；否则刷新（并发去重）并写回存储。
    pub async fn get_valid_token(&self, provider_id: &str, key: &str) -> Result<TokenSet, OAuthError> {
        self.ensure(provider_id, key, false).await
    }

    /// 无视有效期强制刷新（例如上游返回 401 之后）。同样并发去重。
    pub async fn force_refresh(&self, provider_id: &str, key: &str) -> Result<TokenSet, OAuthError> {
        self.ensure(provider_id, key, true).await
    }

    fn join_or_lead(&self, key: &str) -> Role {
        let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rx) = map.get(key) {
            return Role::Follower(rx.clone());
        }
        let (tx, rx) = watch::channel(None);
        map.insert(key.to_owned(), rx);
        Role::Leader(Leader { inflight: self.inflight.clone(), key: key.to_owned(), tx: Some(tx) })
    }

    async fn ensure(&self, provider_id: &str, key: &str, force: bool) -> Result<TokenSet, OAuthError> {
        let cfg = self
            .catalog
            .get(provider_id)
            .ok_or_else(|| OAuthError::UnknownProvider(provider_id.to_owned()))?;
        if cfg.flow == FlowKind::Custom {
            let reason = cfg.custom.as_ref().map(|c| c.reason.clone()).unwrap_or_default();
            return Err(OAuthError::CustomFlowRequired { provider: cfg.id.clone(), reason });
        }
        loop {
            if !force {
                let tokens = self
                    .store
                    .load(key)
                    .await?
                    .ok_or_else(|| OAuthError::NoTokens(provider_id.to_owned()))?;
                if !tokens.needs_refresh(cfg.refresh_lead_seconds, Utc::now())
                    && !Self::inference_exchange_needed(&cfg.id, &tokens, cfg.refresh_lead_seconds) {
                    return Ok(tokens);
                }
            }
            match self.join_or_lead(key) {
                Role::Follower(mut rx) => {
                    let outcome = rx.wait_for(|v| v.is_some()).await;
                    match outcome {
                        Ok(value) => {
                            return value.clone().expect("wait_for guarantees Some");
                        }
                        // 领头者被取消：重新竞选。
                        Err(_) => continue,
                    }
                }
                Role::Leader(leader) => {
                    let result = self.refresh_now(cfg, key, force).await;
                    leader.finish(result.clone());
                    return result;
                }
            }
        }
    }

    async fn refresh_now(&self, cfg: &OAuthProviderConfig, key: &str, force: bool) -> Result<TokenSet, OAuthError> {
        // 成为领头者后再读一次存储：可能刚有别的刷新完成。
        let current = self
            .store
            .load(key)
            .await?
            .ok_or_else(|| OAuthError::NoTokens(cfg.id.clone()))?;
        if !force && !current.needs_refresh(cfg.refresh_lead_seconds, Utc::now())
            && !Self::inference_exchange_needed(&cfg.id, &current, cfg.refresh_lead_seconds) {
            return Ok(current);
        }
        let refresh_portal = cfg.id != "muse"
            && (current.needs_refresh(cfg.refresh_lead_seconds, Utc::now())
                || (force && cfg.id != "github"));
        let fresh = if refresh_portal {
            let creds = self.credentials.credentials_for(&cfg.id).await?;
            refresh::refresh_tokens(&self.http, cfg, &creds, &current).await?
        } else {
            current
        };
        if matches!(cfg.id.as_str(), "nous" | "github" | "muse") {
            // Persist a rotated account refresh token before the independent exchange.
            if refresh_portal { self.store.save(key, &fresh).await?; }
            let prepared = self.prepare_inference_tokens(&cfg.id, fresh).await?;
            self.store.save(key, &prepared).await?;
            return Ok(prepared);
        }
        self.store.save(key, &fresh).await?;
        Ok(fresh)
    }
}

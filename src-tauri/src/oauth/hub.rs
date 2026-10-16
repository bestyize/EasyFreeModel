//! OAuth 授权的应用层（命令层 `commands/oauth.rs` 只调用这里）。
//!
//! - 供应商状态列表：flow / 状态（ready、need_client_credentials、custom）/ 风险提示 / 转发限制说明。
//! - 客户端凭据表单的后端：写入 / 清除用户自备的 clientId、clientSecret、baseUrl（返回值不含任何已存内容）。
//! - 授权会话：`start` / `reauthorize` 启动框架流程并登记会话（sessionId），后台任务等待授权完成；
//!   `poll` 读取状态；`cancel` 立即关闭回调监听；会话有数量上限、生命周期上限与完成后保留期，退出时全部关闭。
//! - 授权成功：令牌写入凭据库（key = `Connection.credential_ref`），创建 / 恢复 `kind=oauth` 的连接。
//!
//! 红线：令牌、授权码、code_verifier、refresh token、client secret 只存在于 `Secret` / `TokenSet` 中，
//! 不进日志、错误值或任何返回给界面的结构。界面只拿到授权 URL、设备码页面的 user_code 与 verification_uri、
//! 状态枚举与脱敏错误码。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;
use sqlx::SqlitePool;
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;

use super::config::{ClientCredentialAdmin, ClientCredentials, FlowKind, OAuthProviderConfig};
use super::error::OAuthError;
use super::http::ensure_secure_endpoint;
use super::secret::Secret;
use super::service::{CancelHandle, OAuthService};
use super::token::TokenSet;
use crate::error::AppError;
use crate::models::CredentialKind;
use crate::store::new_id;
use crate::store::providers as repo;

/// 同时进行（未完成）的授权会话上限。
pub const MAX_PENDING_SESSIONS: usize = 8;
/// 会话结束后保留多久供界面读取最终状态；之后再轮询得到 `expired`。
pub const FINISHED_RETENTION: Duration = Duration::from_secs(600);
/// 在授权自身的有效期之外再给后台任务的宽限（用于硬性超时兜底）。
pub const SESSION_GRACE: Duration = Duration::from_secs(15);
const MAX_LABEL_CHARS: usize = 128;
const CANCEL_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

// ───────────────────────── 错误 ─────────────────────────

/// 命令层错误：只有稳定的、脱敏的错误码。序列化为 `{ code, message }`，前端据 code 映射中文文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubError {
    code: &'static str,
}

impl HubError {
    pub fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OAuth request failed ({})", self.code)
    }
}

impl std::error::Error for HubError {}

impl From<OAuthError> for HubError {
    fn from(e: OAuthError) -> Self {
        HubError::new(e.code())
    }
}

impl From<AppError> for HubError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::NotFound(_) => HubError::new("not_found"),
            other => {
                // 数据库 / IO 错误文本不含令牌；只记录，不返回给界面
                tracing::warn!("OAuth 数据层错误：{other}");
                HubError::new("internal")
            }
        }
    }
}

impl Serialize for HubError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("HubError", 2)?;
        st.serialize_field("code", self.code)?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

// ───────────────────────── 返回给界面的结构 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Ready,
    NeedClientCredentials,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SecretRequirement {
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub provider_id: String,
    pub flow: FlowKind,
    pub status: ProviderStatus,
    /// custom：为什么不做（取自 oauth_providers.json）。
    pub reason: Option<String>,
    pub requires: Vec<String>,
    /// 授权后在转发环节的限制（如实展示）。
    pub limitation: Option<String>,
    pub token_exchange_pending: bool,
    pub risk_notice: Option<String>,
    pub client_id_required: bool,
    pub client_secret: SecretRequirement,
    pub supports_base_url: bool,
    /// 凭据库里是否已有用户配置的客户端凭据（只是布尔值）。
    pub client_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub session_id: String,
    pub flow: FlowKind,
    pub authorize_url: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PollResult {
    Pending,
    Succeeded { connection_id: String },
    Failed { code: String },
    Cancelled,
    Expired,
}

// ───────────────────────── 会话 ─────────────────────────

#[derive(Debug, Clone)]
enum Target {
    /// 授权成功后新建连接；label 为用户填写的账户标识（可为空）。
    New { label: String },
    /// 对已有连接重新授权。
    Reauthorize { connection_id: String },
}

enum State {
    Pending,
    Done(PollResult),
}

struct Session {
    provider_id: String,
    target: Target,
    state: State,
    /// 本次授权专属的回调地址与 state；仅用于校验手动粘贴的回调 URL。
    callback: Option<(String, String)>,
    /// 框架流程的取消句柄；流程启动完成前为 None。
    cancel: Option<CancelHandle>,
    task: Option<JoinHandle<()>>,
    finished_at: Option<Instant>,
}

impl Session {
    fn is_pending(&self) -> bool {
        matches!(self.state, State::Pending)
    }

    /// 结束会话并取出需要收尾的句柄。
    fn end(&mut self, result: PollResult) -> (Option<CancelHandle>, Option<JoinHandle<()>>) {
        self.state = State::Done(result);
        self.finished_at = Some(Instant::now());
        (self.cancel.clone(), self.task.take())
    }
}

#[derive(Debug, Clone)]
pub struct HubLimits {
    pub max_pending: usize,
    pub finished_retention: Duration,
    pub grace: Duration,
}

impl Default for HubLimits {
    fn default() -> Self {
        Self { max_pending: MAX_PENDING_SESSIONS, finished_retention: FINISHED_RETENTION, grace: SESSION_GRACE }
    }
}

type FlowFuture = Pin<Box<dyn Future<Output = Result<TokenSet, OAuthError>> + Send>>;

struct Inner {
    service: Arc<OAuthService>,
    clients: Arc<dyn ClientCredentialAdmin>,
    db: SqlitePool,
    limits: HubLimits,
    sessions: Mutex<HashMap<String, Session>>,
    closed: AtomicBool,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 清理过了保留期的已结束会话。
    fn gc(&self) {
        let now = Instant::now();
        let retention = self.limits.finished_retention;
        self.lock().retain(|_, s| s.finished_at.is_none_or(|t| now.duration_since(t) < retention));
    }

    /// 后台任务收尾：只有仍处于 Pending 的会话才会被写入结果（已被取消的保持 Cancelled）。
    fn finish(&self, session_id: &str, result: PollResult) {
        let mut sessions = self.lock();
        if let Some(s) = sessions.get_mut(session_id) {
            if s.is_pending() {
                s.end(result);
            }
        }
    }

    fn pending_target(&self, session_id: &str) -> Option<(String, Target)> {
        self.lock().get(session_id).filter(|s| s.is_pending()).map(|s| (s.provider_id.clone(), s.target.clone()))
    }

    /// 授权成功：写入令牌、创建 / 恢复连接。失败时不留下半成品（令牌条目会被回收）。
    async fn complete(&self, session_id: &str, tokens: TokenSet) -> PollResult {
        // 授权完成的同时被取消 / 会话已被清理：丢弃令牌，不创建连接
        let Some((provider_id, target)) = self.pending_target(session_id) else {
            return PollResult::Cancelled;
        };
        let tokens = match self.service.prepare_inference_tokens(&provider_id, tokens).await {
            Ok(tokens) => tokens,
            Err(e) => return PollResult::Failed { code: e.code().to_owned() },
        };
        if self.pending_target(session_id).is_none() {
            return PollResult::Cancelled;
        }
        let store = self.service.token_store();
        match target {
            Target::New { label } => {
                let display_name = match repo::get_provider(&self.db, &provider_id).await {
                    Ok(Some(row)) => row.display_name,
                    _ => provider_id.clone(),
                };
                let label = resolve_label(&label, &tokens, &display_name);
                let id = new_id();
                let credential_ref = format!("connection/{id}/{}", new_id());
                if let Err(e) = store.save(&credential_ref, &tokens).await {
                    return PollResult::Failed { code: e.code().to_owned() };
                }
                match repo::insert_oauth_connection(&self.db, &id, &provider_id, &label, &credential_ref).await {
                    Ok(_) => PollResult::Succeeded { connection_id: id },
                    Err(e) => {
                        tracing::warn!("OAuth 授权成功但创建连接失败：{e}");
                        let _ = store.delete(&credential_ref).await;
                        PollResult::Failed { code: HubError::from(e).code().to_owned() }
                    }
                }
            }
            Target::Reauthorize { connection_id } => {
                let conn = match repo::get_connection(&self.db, &connection_id).await {
                    Ok(Some(c))
                        if c.provider_id == provider_id && CredentialKind::parse(&c.kind) == Some(CredentialKind::Oauth) =>
                    {
                        c
                    }
                    Ok(_) => return PollResult::Failed { code: "not_found".to_owned() },
                    Err(e) => return PollResult::Failed { code: HubError::from(e).code().to_owned() },
                };
                let credential_ref = conn.credential_ref.clone().unwrap_or_else(|| format!("connection/{}/{}", conn.id, new_id()));
                if let Err(e) = store.save(&credential_ref, &tokens).await {
                    return PollResult::Failed { code: e.code().to_owned() };
                }
                match repo::mark_connection_reauthorized(&self.db, &conn.id, &credential_ref).await {
                    Ok(()) => PollResult::Succeeded { connection_id: conn.id },
                    Err(e) => PollResult::Failed { code: HubError::from(e).code().to_owned() },
                }
            }
        }
    }

    /// 取消全部未完成会话并中止后台任务（同步；监听随任务被丢弃而释放）。
    fn abort_all(&self) {
        let drained: Vec<Session> = self.lock().drain().map(|(_, s)| s).collect();
        for mut s in drained {
            if let Some(c) = &s.cancel {
                c.cancel();
            }
            if let Some(t) = s.task.take() {
                t.abort();
            }
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.abort_all();
    }
}

fn outcome_of_error(e: &OAuthError) -> PollResult {
    match e {
        OAuthError::Cancelled => PollResult::Cancelled,
        OAuthError::Timeout | OAuthError::DeviceCodeExpired => PollResult::Expired,
        other => PollResult::Failed { code: other.code().to_owned() },
    }
}

/// 等待取消生效（回调监听关闭、任务结束）；超时则强制中止，保证端口被释放。
async fn settle(cancel: Option<CancelHandle>, task: Option<JoinHandle<()>>) {
    if let Some(c) = cancel {
        c.cancel();
    }
    if let Some(t) = task {
        let abort: AbortHandle = t.abort_handle();
        if tokio::time::timeout(CANCEL_JOIN_TIMEOUT, t).await.is_err() {
            abort.abort();
        }
    }
}

// ───────────────────────── 账户标识 ─────────────────────────

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// 用户填写的标识优先；其次令牌里 id_token 的 email 声明（只用作显示名，不参与任何信任判断）；再退回「<提供商> 账户」。
fn resolve_label(user_label: &str, tokens: &TokenSet, display_name: &str) -> String {
    let user = user_label.trim();
    if !user.is_empty() {
        return user.to_owned();
    }
    if let Some(email) = email_from_id_token(tokens) {
        return email;
    }
    format!("{display_name} 账户")
}

fn email_from_id_token(tokens: &TokenSet) -> Option<String> {
    let jwt = tokens.extra_value("id_token")?;
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let email = claims.get("email").and_then(Value::as_str)?.trim();
    (!email.is_empty() && email.chars().count() <= MAX_LABEL_CHARS && !has_control(email)).then(|| email.to_owned())
}

fn check_label(raw: &str) -> Result<String, HubError> {
    let label = raw.trim();
    if label.chars().count() > MAX_LABEL_CHARS || has_control(label) {
        return Err(HubError::new("label_invalid"));
    }
    Ok(label.to_owned())
}

// ───────────────────────── Hub ─────────────────────────

pub struct OAuthHub {
    inner: Arc<Inner>,
}

impl OAuthHub {
    pub fn new(service: Arc<OAuthService>, clients: Arc<dyn ClientCredentialAdmin>, db: SqlitePool) -> Self {
        Self::with_limits(service, clients, db, HubLimits::default())
    }

    pub fn with_limits(
        service: Arc<OAuthService>,
        clients: Arc<dyn ClientCredentialAdmin>,
        db: SqlitePool,
        limits: HubLimits,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                service,
                clients,
                db,
                limits,
                sessions: Mutex::new(HashMap::new()),
                closed: AtomicBool::new(false),
            }),
        }
    }

    fn standard_config(&self, provider_id: &str) -> Result<&OAuthProviderConfig, HubError> {
        let cfg = self
            .inner
            .service
            .catalog()
            .get(provider_id)
            .ok_or_else(|| OAuthError::UnknownProvider(provider_id.to_owned()))?;
        if cfg.flow == FlowKind::Custom {
            let reason = cfg.custom.as_ref().map(|c| c.reason.clone()).unwrap_or_default();
            return Err(OAuthError::CustomFlowRequired { provider: cfg.id.clone(), reason }.into());
        }
        Ok(cfg)
    }

    // ---- 供应商状态 ----

    pub async fn list_providers(&self) -> Result<Vec<ProviderInfo>, HubError> {
        let mut out = Vec::with_capacity(self.inner.service.catalog().all().len());
        for cfg in self.inner.service.catalog().all() {
            out.push(self.provider_info(cfg).await);
        }
        Ok(out)
    }

    async fn provider_info(&self, cfg: &OAuthProviderConfig) -> ProviderInfo {
        if cfg.flow == FlowKind::Custom {
            let custom = cfg.custom.as_ref();
            return ProviderInfo {
                provider_id: cfg.id.clone(),
                flow: cfg.flow,
                status: ProviderStatus::Custom,
                reason: custom.map(|c| c.reason.clone()),
                requires: custom.map(|c| c.requires.clone()).unwrap_or_default(),
                limitation: None,
                token_exchange_pending: false,
                risk_notice: cfg.risk_notice.clone(),
                client_id_required: false,
                client_secret: SecretRequirement::None,
                supports_base_url: false,
                client_configured: false,
            };
        }

        // 凭据库暂时读不出来时按「没有用户凭据」处理：列表仍可展示，开始授权时会给出真实错误
        let clients = &self.inner.clients;
        let configured = clients.has_credentials(&cfg.id).await.unwrap_or(false);
        let creds = clients.credentials_for(&cfg.id).await.unwrap_or_default();
        let ready = cfg.resolve_client(&creds).is_ok();

        let adapter_ready = match repo::get_provider(&self.inner.db, &cfg.id).await {
            Ok(Some(row)) => row.adapter_ready,
            _ => false,
        };
        let limitation = (!adapter_ready)
            .then_some("该提供商的转发适配器尚未接入，授权成功的连接暂不会参与路由".to_owned());

        ProviderInfo {
            provider_id: cfg.id.clone(),
            flow: cfg.flow,
            status: if ready { ProviderStatus::Ready } else { ProviderStatus::NeedClientCredentials },
            reason: None,
            requires: Vec::new(),
            limitation,
            token_exchange_pending: false,
            risk_notice: cfg.risk_notice.clone(),
            client_id_required: cfg.client_id.is_none() && !cfg.no_client_id,
            client_secret: if cfg.requires_client_secret && cfg.client_secret.is_none() {
                SecretRequirement::Required
            } else if cfg.client_id.is_none() && !cfg.no_client_id {
                SecretRequirement::Optional
            } else {
                SecretRequirement::None
            },
            supports_base_url: cfg.base_url_default.is_some(),
            client_configured: configured,
        }
    }

    // ---- 客户端凭据 ----

    pub async fn set_client_credentials(
        &self,
        provider_id: &str,
        client_id: &str,
        client_secret: Option<Secret>,
        base_url: Option<&str>,
    ) -> Result<ProviderInfo, HubError> {
        let cfg = self.standard_config(provider_id)?;
        let invalid = || HubError::new("invalid_input");
        let accepts_input = cfg.client_id.is_none() || cfg.requires_client_secret || cfg.base_url_default.is_some();
        if !accepts_input {
            return Err(invalid());
        }

        let client_id = client_id.trim();
        if client_id.chars().count() > 512 || has_control(client_id) {
            return Err(invalid());
        }
        let secret = client_secret.filter(|s| !s.is_empty());
        if let Some(s) = &secret {
            if s.expose().chars().count() > 1024 || has_control(s.expose()) {
                return Err(invalid());
            }
        }
        let base_url = base_url.map(str::trim).filter(|s| !s.is_empty());
        if let Some(url) = base_url {
            if cfg.base_url_default.is_none() || url.chars().count() > 512 || ensure_secure_endpoint(url).is_err() {
                return Err(invalid());
            }
        }
        if (cfg.client_id.is_none() && client_id.is_empty()) || (cfg.requires_client_secret && secret.is_none()) {
            return Err(OAuthError::MissingClientCredential {
                provider: cfg.id.clone(),
                field: if client_id.is_empty() && cfg.client_id.is_none() { "client_id" } else { "client_secret" },
            }
            .into());
        }

        let creds = ClientCredentials {
            client_id: (!client_id.is_empty()).then(|| client_id.to_owned()),
            client_secret: secret,
            base_url: base_url.map(str::to_owned),
        };
        self.inner.clients.save_credentials(provider_id, &creds).await?;
        Ok(self.provider_info(cfg).await)
    }

    pub async fn clear_client_credentials(&self, provider_id: &str) -> Result<ProviderInfo, HubError> {
        let cfg = self.standard_config(provider_id)?;
        self.inner.clients.clear_credentials(provider_id).await?;
        Ok(self.provider_info(cfg).await)
    }

    // ---- 授权会话 ----

    /// 开始授权并（成功后）新建连接。`account_label` 可为空：由令牌里的邮箱或「<提供商> 账户」命名。
    pub async fn start(&self, provider_id: &str, account_label: &str) -> Result<StartResult, HubError> {
        let label = check_label(account_label)?;
        self.begin(provider_id, Target::New { label }).await
    }

    /// 对已有 OAuth 连接重新授权；成功后凭据原位更新、连接恢复为 active。
    pub async fn reauthorize(&self, connection_id: &str) -> Result<StartResult, HubError> {
        let conn = repo::get_connection(&self.inner.db, connection_id).await?.ok_or(HubError::new("not_found"))?;
        if CredentialKind::parse(&conn.kind) != Some(CredentialKind::Oauth) {
            return Err(HubError::new("not_oauth"));
        }
        self.begin(&conn.provider_id, Target::Reauthorize { connection_id: conn.id }).await
    }

    async fn begin(&self, provider_id: &str, target: Target) -> Result<StartResult, HubError> {
        let inner = &self.inner;
        if inner.closed.load(Ordering::SeqCst) {
            return Err(HubError::new("cancelled"));
        }
        let cfg = self.standard_config(provider_id)?;
        inner.gc();

        // 占位登记：同一供应商只保留一个进行中的会话（固定端口的回调监听不能并存，
        // 用户重新打开面板即视为放弃上一个）；并检查总数上限。
        let session_id = new_id();
        let (replaced, over_cap) = {
            let mut sessions = inner.lock();
            let mut replaced = Vec::new();
            for s in sessions.values_mut().filter(|s| s.is_pending() && s.provider_id == provider_id) {
                replaced.push(s.end(PollResult::Cancelled));
            }
            let over_cap = sessions.values().filter(|s| s.is_pending()).count() >= inner.limits.max_pending;
            if !over_cap {
                sessions.insert(
                    session_id.clone(),
                    Session {
                        provider_id: provider_id.to_owned(),
                        target: target.clone(),
                        state: State::Pending,
                        callback: None,
                        cancel: None,
                        task: None,
                        finished_at: None,
                    },
                );
            }
            (replaced, over_cap)
        };
        for (c, t) in replaced {
            settle(c, t).await;
        }
        if over_cap {
            return Err(HubError::new("too_many_sessions"));
        }

        // 启动框架流程（绑定监听 / 请求设备码）。失败则撤销占位。
        let started: Result<(StartResult, CancelHandle, FlowFuture, DateTime<Utc>), OAuthError> = match cfg.flow {
            FlowKind::AuthorizationCode => inner.service.start_authorization(provider_id).await.map(|auth| {
                let cancel = auth.cancel_handle();
                let expires_at = auth.expires_at;
                let result = StartResult {
                    session_id: session_id.clone(),
                    flow: FlowKind::AuthorizationCode,
                    authorize_url: Some(auth.authorize_url.clone()),
                    user_code: None,
                    verification_uri: None,
                    expires_at,
                };
                (result, cancel, Box::pin(auth.wait()) as FlowFuture, expires_at)
            }),
            FlowKind::Device => inner.service.start_device(provider_id).await.map(|dev| {
                let cancel = dev.cancel_handle();
                let expires_at = dev.info.expires_at;
                let result = StartResult {
                    session_id: session_id.clone(),
                    flow: FlowKind::Device,
                    authorize_url: None,
                    user_code: Some(dev.info.user_code.clone()),
                    verification_uri: Some(dev.info.verification_uri.clone()),
                    expires_at,
                };
                (result, cancel, Box::pin(dev.wait()) as FlowFuture, expires_at)
            }),
            FlowKind::StatePoll => inner.service.start_state_poll(provider_id).await.map(|st| {
                let cancel = st.cancel_handle();
                let expires_at = st.info.expires_at;
                let result = StartResult {
                    session_id: session_id.clone(),
                    flow: FlowKind::StatePoll,
                    // 面板会把这个地址在系统浏览器里打开（与 authorization-code 相同）
                    authorize_url: Some(st.info.authorize_url.clone()),
                    user_code: None,
                    verification_uri: None,
                    expires_at,
                };
                (result, cancel, Box::pin(st.wait()) as FlowFuture, expires_at)
            }),
            FlowKind::Custom => Err(OAuthError::UnknownProvider(provider_id.to_owned())),
        };
        let (result, cancel, flow, expires_at) = match started {
            Ok(v) => v,
            Err(e) => {
                inner.lock().remove(&session_id);
                return Err(e.into());
            }
        };
        let callback = if result.flow == FlowKind::AuthorizationCode {
            result.authorize_url.as_deref().and_then(|raw| {
                let url = reqwest::Url::parse(raw).ok()?;
                let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
                Some((params.get("redirect_uri")?.clone(), params.get("state")?.clone()))
            })
        } else {
            None
        };

        // 后台任务：等待授权结果；硬性超时兜底。
        let lifetime = (expires_at - Utc::now()).to_std().unwrap_or_default() + inner.limits.grace;
        let task_inner = inner.clone();
        let sid = session_id.clone();
        let task = tokio::spawn(async move {
            let outcome = match tokio::time::timeout(lifetime, flow).await {
                Err(_) => PollResult::Expired,
                Ok(Err(e)) => outcome_of_error(&e),
                Ok(Ok(tokens)) => task_inner.complete(&sid, tokens).await,
            };
            task_inner.finish(&sid, outcome);
        });

        // 登记取消句柄与任务；若启动期间会话已被取消 / 应用正在退出，立即收尾。
        let abandoned = {
            let mut sessions = inner.lock();
            match sessions.get_mut(&session_id) {
                Some(s) if s.is_pending() && !inner.closed.load(Ordering::SeqCst) => {
                    s.cancel = Some(cancel);
                    s.callback = callback;
                    s.task = Some(task);
                    None
                }
                _ => Some((cancel, task)),
            }
        };
        if let Some((cancel, task)) = abandoned {
            settle(Some(cancel), Some(task)).await;
            return Err(HubError::new("cancelled"));
        }
        Ok(result)
    }

    /// 读取会话状态。未知或已过保留期的会话视为 `expired`。
    pub fn poll(&self, session_id: &str) -> PollResult {
        self.inner.gc();
        match self.inner.lock().get(session_id) {
            None => PollResult::Expired,
            Some(s) => match &s.state {
                State::Pending => PollResult::Pending,
                State::Done(r) => r.clone(),
            },
        }
    }

    /// 浏览器未能访问本机监听端口时，接收用户粘贴的完整回调 URL。
    /// 只向当前会话预先绑定的回环地址发送请求，仍由同一个监听器校验 state 并兑换令牌。
    pub async fn submit_callback(&self, session_id: &str, callback_url: &str) -> Result<(), HubError> {
        self.inner.gc();
        let (expected, state) = {
            let sessions = self.inner.lock();
            let session = sessions.get(session_id).ok_or(HubError::new("expired"))?;
            if !session.is_pending() { return Err(HubError::new("expired")); }
            session.callback.clone().ok_or(HubError::new("unsupported_flow"))?
        };
        if callback_url.len() > 8192 {
            return Err(HubError::new("invalid_callback"));
        }
        let url = reqwest::Url::parse(callback_url.trim()).map_err(|_| HubError::new("invalid_callback"))?;
        let target = reqwest::Url::parse(&expected).map_err(|_| HubError::new("internal"))?;
        if url.scheme() != "http" || url.host_str() != target.host_str() || url.port() != target.port()
            || url.path() != target.path() || url.username() != "" || url.password().is_some() || url.fragment().is_some()
            || url.query_pairs().filter(|(k, _)| k == "state").count() != 1
            || url.query_pairs().find(|(k, _)| k == "state").is_none_or(|(_, v)| v.as_ref() != state.as_str())
            || url.query_pairs().filter(|(k, _)| k == "code").count() != 1
            || url.query_pairs().find(|(k, _)| k == "code").is_none_or(|(_, v)| v.is_empty())
        {
            return Err(HubError::new("invalid_callback"));
        }
        // 重组目标地址，避免请求任意地址；不记录 URL 或响应内容（包含授权码）。
        let mut local = target;
        local.set_query(url.query());
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3)).build().map_err(|_| HubError::new("internal"))?;
        let response = client.get(local).send().await.map_err(|_| HubError::new("listen_failed"))?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::CONFLICT { Ok(()) }
        else { Err(HubError::new("invalid_callback")) }
    }

    /// 取消会话：状态立即变为 `cancelled`，返回时回调监听已关闭、端口已释放。
    pub async fn cancel(&self, session_id: &str) {
        let handles = {
            let mut sessions = self.inner.lock();
            sessions.get_mut(session_id).filter(|s| s.is_pending()).map(|s| s.end(PollResult::Cancelled))
        };
        if let Some((cancel, task)) = handles {
            settle(cancel, task).await;
        }
    }

    /// 断开：删除连接（及其配额快照）并清除令牌条目。
    pub async fn disconnect(&self, connection_id: &str) -> Result<(), HubError> {
        let conn = repo::get_connection(&self.inner.db, connection_id).await?.ok_or(HubError::new("not_found"))?;
        if CredentialKind::parse(&conn.kind) != Some(CredentialKind::Oauth) {
            return Err(HubError::new("not_oauth"));
        }
        // 正在为它重新授权的会话一并取消
        let doomed: Vec<(Option<CancelHandle>, Option<JoinHandle<()>>)> = {
            let mut sessions = self.inner.lock();
            sessions
                .values_mut()
                .filter(|s| s.is_pending() && matches!(&s.target, Target::Reauthorize { connection_id: id } if *id == conn.id))
                .map(|s| s.end(PollResult::Cancelled))
                .collect()
        };
        for (c, t) in doomed {
            settle(c, t).await;
        }
        // 数据库先提交，再清令牌条目；清理失败只记录（孤儿条目无引用、不会被使用）
        if let Some(cref) = repo::delete_connection(&self.inner.db, &conn.id).await? {
            if let Err(e) = self.inner.service.token_store().delete(&cref).await {
                tracing::warn!("清理 OAuth 令牌条目失败：{e}");
            }
        }
        Ok(())
    }

    /// 关闭全部监听并中止后台任务（应用退出时调用；同步，可在退出钩子里使用）。
    pub fn shutdown_now(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.abort_all();
    }

    /// 同 [`shutdown_now`]，并等待被中止的任务真正结束（端口已释放）。
    pub async fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let drained: Vec<Session> = self.inner.lock().drain().map(|(_, s)| s).collect();
        for mut s in drained {
            if let Some(c) = &s.cancel {
                c.cancel();
            }
            if let Some(t) = s.task.take() {
                t.abort();
                let _ = t.await;
            }
        }
    }

    /// 进行中的会话数（测试与诊断用）。
    pub fn pending_sessions(&self) -> usize {
        self.inner.lock().values().filter(|s| s.is_pending()).count()
    }

    /// 仍保留的会话总数（含已结束但未过保留期的）。
    pub fn retained_sessions(&self) -> usize {
        self.inner.gc();
        self.inner.lock().len()
    }
}

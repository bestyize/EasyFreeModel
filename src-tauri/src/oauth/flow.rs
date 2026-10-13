//! Authorization-code 流程 + 回环回调。
//!
//! `OAuthService::start_authorization` 绑定回环端口并返回 `AuthSession`；界面打开
//! `authorize_url`，随后调用 `AuthSession::wait` 等待回调、校验 state、换取令牌。

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Client, Url};
use serde::Serialize;

use super::config::{ClientCredentials, OAuthProviderConfig, ResolvedClient};
use super::error::OAuthError;
use super::http::{self, Encoding};
use super::loopback::{CallbackOutcome, LoopbackServer};
use super::pkce;
use super::secret::Secret;
use super::service::{CancelHandle, OAuthTuning};
use super::token::{tokens_from_reply, TokenSet};

/// 可安全交给界面的会话信息。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionInfo {
    pub provider_id: String,
    pub authorize_url: String,
    pub state: String,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
}

pub struct AuthSession {
    pub provider_id: String,
    /// 交给系统浏览器打开的授权地址。
    pub authorize_url: String,
    pub state: String,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
    cfg: OAuthProviderConfig,
    http: Client,
    client: ResolvedClient,
    token_url: String,
    verifier: Option<Secret>,
    server: LoopbackServer,
    timeout: Duration,
    cancel: CancelHandle,
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // authorize_url / state / verifier 不输出
        f.debug_struct("AuthSession")
            .field("provider_id", &self.provider_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

pub(super) async fn start(
    http: Client,
    cfg: OAuthProviderConfig,
    creds: ClientCredentials,
    tuning: &OAuthTuning,
) -> Result<AuthSession, OAuthError> {
    // 先校验凭据与配置，再占用端口。
    let client = cfg.resolve_client(&creds)?;
    let authorize_endpoint = cfg.authorize_endpoint(&creds)?;
    let token_url = cfg.token_endpoint(&creds)?;
    let redirect = cfg
        .redirect
        .clone()
        .ok_or_else(|| OAuthError::Config(format!("{}: missing redirect block", cfg.id)))?;
    let mut url = http::ensure_secure_endpoint(&authorize_endpoint)?;

    let state = pkce::random_state();
    let pkce = cfg.pkce.then(pkce::generate);
    let server = LoopbackServer::bind(redirect.port.unwrap_or(0), &redirect.path, &state).await?;
    let redirect_uri = format!("http://{}:{}{}", redirect.host, server.port, redirect.path);

    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &client.client_id);
        query.append_pair("redirect_uri", &redirect_uri);
        let scope = cfg.scope_string();
        if !scope.is_empty() {
            query.append_pair("scope", &scope);
        }
        query.append_pair("state", &state);
        if let Some(pkce) = &pkce {
            query.append_pair("code_challenge", &pkce.challenge);
            query.append_pair("code_challenge_method", pkce::CHALLENGE_METHOD);
        }
        for (key, value) in &cfg.extra_authorize_params {
            query.append_pair(key, value);
        }
    }

    let timeout = tuning.callback_timeout;
    Ok(AuthSession {
        provider_id: cfg.id.clone(),
        authorize_url: Url::to_string(&url),
        state,
        redirect_uri,
        expires_at: Utc::now() + chrono::Duration::from_std(timeout).unwrap_or_else(|_| chrono::Duration::minutes(5)),
        cfg,
        http,
        client,
        token_url,
        verifier: pkce.map(|p| p.verifier),
        server,
        timeout,
        cancel: CancelHandle::new(),
    })
}

impl AuthSession {
    pub fn info(&self) -> AuthSessionInfo {
        AuthSessionInfo {
            provider_id: self.provider_id.clone(),
            authorize_url: self.authorize_url.clone(),
            state: self.state.clone(),
            redirect_uri: self.redirect_uri.clone(),
            expires_at: self.expires_at,
        }
    }

    /// 本地回调监听的端口（127.0.0.1）。
    pub fn port(&self) -> u16 {
        self.server.port
    }

    /// 取消句柄：可在另一个任务里调用 `cancel()` 终止等待并关闭监听。
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    /// 等待浏览器回调 -> 校验 state -> 换取令牌。无论结果如何，返回前都会关闭监听。
    pub async fn wait(mut self) -> Result<TokenSet, OAuthError> {
        let cancel = self.cancel.clone();
        let received = tokio::select! {
            received = &mut self.server.rx => received.map_err(|_| OAuthError::Cancelled),
            _ = tokio::time::sleep(self.timeout) => Err(OAuthError::Timeout),
            _ = cancel.cancelled() => Err(OAuthError::Cancelled),
        };
        self.server.shutdown().await;

        match received? {
            CallbackOutcome::StateMismatch => Err(OAuthError::StateMismatch),
            CallbackOutcome::MissingCode => Err(OAuthError::MissingCode),
            CallbackOutcome::ProviderError { code, description } => {
                if code == "access_denied" {
                    Err(OAuthError::AccessDenied)
                } else {
                    Err(OAuthError::CallbackError { code, description })
                }
            }
            CallbackOutcome::Code(code) => self.exchange(&code).await,
        }
    }

    async fn exchange(&self, code: &Secret) -> Result<TokenSet, OAuthError> {
        let mut fields: Vec<(String, String)> = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.expose().to_owned()),
            ("redirect_uri".into(), self.redirect_uri.clone()),
            ("client_id".into(), self.client.client_id.clone()),
        ];
        if let Some(verifier) = &self.verifier {
            fields.push(("code_verifier".into(), verifier.expose().to_owned()));
        }
        if let Some(secret) = &self.client.client_secret {
            fields.push(("client_secret".into(), secret.expose().to_owned()));
        }
        if self.cfg.token_request_includes_state {
            fields.push(("state".into(), self.state.clone()));
        }
        for (key, value) in &self.cfg.extra_token_params {
            fields.push((key.clone(), value.clone()));
        }
        let encoding: Encoding = self.cfg.token_encoding;
        let reply = http::post(&self.http, &self.token_url, encoding, &fields).await?;

        let mut secrets: Vec<&str> = vec![code.expose(), &self.state];
        if let Some(v) = &self.verifier {
            secrets.push(v.expose());
        }
        if let Some(s) = &self.client.client_secret {
            secrets.push(s.expose());
        }
        tokens_from_reply(&reply, &self.cfg.scopes, &secrets)
    }
}

//! RFC 8628 设备码流程。
//!
//! `OAuthService::start_device` 请求设备码并返回 `DeviceSession`（含 user_code 与
//! verification_uri 供界面展示）；`DeviceSession::wait` 按 interval 轮询，处理
//! authorization_pending / slow_down / expired_token / access_denied。
//! 兼容个别上游的变体：`status` 信封、缺少 device_code 时以 user_code 轮询、
//! `expired_in` 毫秒时间戳、毫秒级 interval。

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use super::config::{ClientCredentials, OAuthProviderConfig, ResolvedClient};
use super::error::{sanitize_code, OAuthError};
use super::http::{self, Encoding};
use super::pkce;
use super::secret::Secret;
use super::service::{CancelHandle, OAuthTuning};
use super::token::{tokens_from_reply, TokenSet};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_INTERVAL_SECS: u64 = 5;
const DEFAULT_EXPIRES_SECS: i64 = 600;
const MAX_CONSECUTIVE_TRANSIENT: u32 = 3;

/// 展示给用户的设备码信息。user_code 本就是给人读的短码，不属于机密；device_code 不在其中。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCodeInfo {
    pub provider_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub interval_secs: u64,
}

pub struct DeviceSession {
    pub info: DeviceCodeInfo,
    cfg: OAuthProviderConfig,
    http: Client,
    client: ResolvedClient,
    token_url: String,
    device_code: Secret,
    poll_by_user_code: bool,
    user_code: String,
    verifier: Option<Secret>,
    tuning: OAuthTuning,
    cancel: CancelHandle,
}

impl fmt::Debug for DeviceSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceSession")
            .field("provider_id", &self.info.provider_id)
            .field("expires_at", &self.info.expires_at)
            .finish_non_exhaustive()
    }
}

fn text<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn number(body: &Value, key: &str) -> Option<f64> {
    match body.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n| n.is_finite() && *n > 0.0)
}

/// `expires_in`（秒）或 `expired_in`（秒，或毫秒级 epoch 时间戳）-> 剩余秒数。
fn lifetime_secs(body: &Value) -> i64 {
    if let Some(secs) = number(body, "expires_in") {
        return secs as i64;
    }
    match number(body, "expired_in") {
        Some(v) if v >= 1e12 => (((v - Utc::now().timestamp_millis() as f64) / 1000.0).ceil() as i64).max(1),
        Some(v) => v as i64,
        None => DEFAULT_EXPIRES_SECS,
    }
}

fn interval_secs(body: &Value, poll_by_user_code: bool) -> u64 {
    match number(body, "interval") {
        None => DEFAULT_INTERVAL_SECS,
        Some(v) if poll_by_user_code && v >= 60.0 => (v / 1000.0).round().max(1.0) as u64,
        Some(v) => v as u64,
    }
}

fn check_verification_uri(raw: &str) -> Result<(), OAuthError> {
    http::ensure_secure_endpoint(raw)
        .map(|_| ())
        .map_err(|_| OAuthError::InvalidResponse("verification URI is not a secure URL".into()))
}

pub(super) async fn start(
    http: Client,
    cfg: OAuthProviderConfig,
    creds: ClientCredentials,
    tuning: &OAuthTuning,
    cancel: CancelHandle,
) -> Result<DeviceSession, OAuthError> {
    let client = cfg.resolve_client(&creds)?;
    let device_url = cfg.device_endpoint(&creds)?;
    let token_url = cfg.token_endpoint(&creds)?;
    let pkce = cfg.pkce.then(pkce::generate);

    let mut fields: Vec<(String, String)> = vec![("client_id".into(), client.client_id.clone())];
    let scope = cfg.scope_string();
    if !scope.is_empty() {
        fields.push(("scope".into(), scope));
    }
    if let Some(secret) = &client.client_secret {
        fields.push(("client_secret".into(), secret.expose().to_owned()));
    }
    if let Some(pkce) = &pkce {
        fields.push(("code_challenge".into(), pkce.challenge.clone()));
        fields.push(("code_challenge_method".into(), pkce::CHALLENGE_METHOD.into()));
    }
    for (key, value) in &cfg.extra_device_params {
        fields.push((key.clone(), value.clone()));
    }

    let reply = http::post(&http, &device_url, Encoding::Form, &fields).await?;
    let secrets: Vec<&str> = client.client_secret.iter().map(Secret::expose).collect();
    match reply.status {
        200..=299 => {}
        408 | 425 | 429 | 500..=599 => {
            return Err(OAuthError::Transient(format!("device authorization returned HTTP {}", reply.status)))
        }
        status => {
            let (code, description) = http::error_fields(reply.body.as_ref(), &secrets);
            return Err(OAuthError::TokenEndpoint { status, code, description });
        }
    }
    let body = reply
        .body
        .ok_or_else(|| OAuthError::InvalidResponse("device authorization response is not JSON".into()))?;

    let user_code = text(&body, "user_code")
        .ok_or_else(|| OAuthError::InvalidResponse("device authorization response has no user_code".into()))?
        .to_owned();
    let device_code = text(&body, "device_code");
    let poll_by_user_code = device_code.is_none();
    let verification_uri = text(&body, "verification_uri")
        .or_else(|| text(&body, "verification_url"))
        .map(str::to_owned)
        .or_else(|| cfg.verification_uri.clone());
    let complete = text(&body, "verification_uri_complete").map(str::to_owned);
    let shown = verification_uri
        .clone()
        .or_else(|| complete.clone())
        .ok_or_else(|| OAuthError::InvalidResponse("device authorization response has no verification URI".into()))?;
    check_verification_uri(&shown)?;
    if let Some(complete) = &complete {
        check_verification_uri(complete)?;
    }

    let lifetime = lifetime_secs(&body);
    let interval = interval_secs(&body, poll_by_user_code);
    let info = DeviceCodeInfo {
        provider_id: cfg.id.clone(),
        user_code: user_code.clone(),
        verification_uri: shown,
        verification_uri_complete: complete,
        expires_at: Utc::now() + chrono::Duration::seconds(lifetime),
        interval_secs: interval,
    };
    Ok(DeviceSession {
        info,
        cfg,
        http,
        client,
        token_url,
        device_code: Secret::new(device_code.unwrap_or(&user_code)),
        poll_by_user_code,
        user_code,
        verifier: pkce.map(|p| p.verifier),
        tuning: tuning.clone(),
        cancel,
    })
}

enum Poll {
    Pending,
    SlowDown,
    Done(TokenSet),
}

impl DeviceSession {
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    fn poll_fields(&self) -> Vec<(String, String)> {
        let mut fields: Vec<(String, String)> = vec![
            ("grant_type".into(), DEVICE_GRANT.into()),
            ("client_id".into(), self.client.client_id.clone()),
        ];
        if self.poll_by_user_code {
            fields.push(("user_code".into(), self.user_code.clone()));
        } else {
            fields.push(("device_code".into(), self.device_code.expose().to_owned()));
        }
        if let Some(verifier) = &self.verifier {
            fields.push(("code_verifier".into(), verifier.expose().to_owned()));
        }
        if let Some(secret) = &self.client.client_secret {
            fields.push(("client_secret".into(), secret.expose().to_owned()));
        }
        for (key, value) in &self.cfg.extra_token_params {
            fields.push((key.clone(), value.clone()));
        }
        fields
    }

    async fn poll_once(&self) -> Result<Poll, OAuthError> {
        let reply = http::post(&self.http, &self.token_url, Encoding::Form, &self.poll_fields()).await?;
        let mut secrets: Vec<&str> = vec![self.device_code.expose()];
        if let Some(v) = &self.verifier {
            secrets.push(v.expose());
        }
        if let Some(s) = &self.client.client_secret {
            secrets.push(s.expose());
        }

        let has_token = reply
            .body
            .as_ref()
            .and_then(|b| b.get("access_token"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if has_token {
            return tokens_from_reply(&reply, &self.cfg.scopes, &secrets).map(Poll::Done);
        }

        // 个别上游用 `status` 信封表达状态；标准上游用 `error` 码。
        let status_word = reply.body.as_ref().and_then(|b| b.get("status")).and_then(Value::as_str);
        let (error_code, _) = http::error_fields(reply.body.as_ref(), &secrets);
        let code = match status_word {
            Some("pending") => "authorization_pending".to_owned(),
            Some("denied") => "access_denied".to_owned(),
            Some("expired") => "expired_token".to_owned(),
            Some(other) if error_code == "unknown" => sanitize_code(other),
            _ => error_code,
        };
        match code.as_str() {
            "authorization_pending" => Ok(Poll::Pending),
            "slow_down" => Ok(Poll::SlowDown),
            "expired_token" | "expired" => Err(OAuthError::DeviceCodeExpired),
            "access_denied" | "denied" => Err(OAuthError::AccessDenied),
            _ => tokens_from_reply(&reply, &self.cfg.scopes, &secrets).map(Poll::Done),
        }
    }

    /// 按 interval 轮询直到成功、被拒绝、过期、超时或被取消。
    pub async fn wait(self) -> Result<TokenSet, OAuthError> {
        let mut interval = self
            .tuning
            .device_interval_override
            .unwrap_or(Duration::from_secs(self.info.interval_secs));
        let remaining = (self.info.expires_at - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        let deadline = Instant::now() + remaining;
        let cancel = self.cancel.clone();
        let mut transient_in_a_row = 0u32;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = cancel.cancelled() => return Err(OAuthError::Cancelled),
            }
            if Instant::now() >= deadline {
                return Err(OAuthError::DeviceCodeExpired);
            }
            let polled = tokio::select! {
                polled = self.poll_once() => polled,
                _ = cancel.cancelled() => return Err(OAuthError::Cancelled),
            };
            match polled {
                Ok(Poll::Done(tokens)) => return Ok(tokens),
                Ok(Poll::Pending) => transient_in_a_row = 0,
                Ok(Poll::SlowDown) => {
                    transient_in_a_row = 0;
                    interval += self.tuning.slow_down_step;
                }
                Err(err) if err.is_retryable() => {
                    transient_in_a_row += 1;
                    if transient_in_a_row >= MAX_CONSECUTIVE_TRANSIENT {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lifetime_and_interval_normalisation() {
        assert_eq!(lifetime_secs(&json!({"expires_in": 900})), 900);
        assert_eq!(lifetime_secs(&json!({"expired_in": 300})), 300);
        let future_ms = Utc::now().timestamp_millis() + 120_000;
        let secs = lifetime_secs(&json!({ "expired_in": future_ms }));
        assert!((118..=121).contains(&secs), "got {secs}");
        assert_eq!(lifetime_secs(&json!({})), DEFAULT_EXPIRES_SECS);

        assert_eq!(interval_secs(&json!({"interval": 7}), false), 7);
        assert_eq!(interval_secs(&json!({}), false), DEFAULT_INTERVAL_SECS);
        assert_eq!(interval_secs(&json!({"interval": 3000}), true), 3);
        assert_eq!(interval_secs(&json!({"interval": 5}), true), 5);
    }
}

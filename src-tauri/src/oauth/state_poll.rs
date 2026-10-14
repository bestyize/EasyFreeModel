//! 非标准「state 轮询」授权（CodeBuddy CN / Intl）。
//!
//! 流程与 RFC 8628 / authorization-code 都不同：
//! 1. `POST <stateUrl>?platform=<平台>` → `{ code: 0, data: { state, authUrl } }`；
//! 2. 浏览器打开 `authUrl` 让用户登录；
//! 3. `GET <tokenUrl>?state=<state>` 轮询：上游用 `code` 表达状态
//!    （0 = 成功、11217 = 登录未完成），成功时 `data.accessToken` 即令牌。
//!
//! state 属于机密：只存在于 `Secret` 中，不进日志 / 错误 / Debug。

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use tokio::time::Instant;

use super::config::{ClientCredentials, OAuthProviderConfig};
use super::error::OAuthError;
use super::http::{self, ExtraHeaders};
use super::secret::Secret;
use super::service::{CancelHandle, OAuthTuning};
use super::token::TokenSet;

const DEFAULT_INTERVAL_MS: u64 = 5000;
const DEFAULT_EXPIRES_SECS: i64 = 600;
const MAX_CONSECUTIVE_TRANSIENT: u32 = 3;

/// 上游成功码。
const CODE_OK: i64 = 0;
/// 上游「登录尚未完成，继续轮询」码。
const CODE_PENDING: i64 = 11217;

/// 交给界面的会话信息：只需要 authUrl 与有效期；state 不进这里。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePollInfo {
    pub provider_id: String,
    pub authorize_url: String,
    pub expires_at: DateTime<Utc>,
}

pub struct StateSession {
    pub info: StatePollInfo,
    cfg: OAuthProviderConfig,
    http: Client,
    token_url: String,
    state: Secret,
    headers: ExtraHeaders,
    interval: Duration,
    cancel: CancelHandle,
}

impl fmt::Debug for StateSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateSession")
            .field("provider_id", &self.info.provider_id)
            .field("expires_at", &self.info.expires_at)
            .finish_non_exhaustive()
    }
}

fn text<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn code_of(body: &Value) -> Option<i64> {
    match body.get("code")? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// 成功后把上游的 token 信封（camelCase）转成 `TokenSet`。
fn token_from_envelope(body: &Value, scopes: &[String]) -> Result<TokenSet, OAuthError> {
    let data = body.get("data").ok_or_else(|| OAuthError::InvalidResponse("token response has no data".into()))?;
    let access = text(data, "accessToken")
        .or_else(|| text(data, "access_token"))
        .ok_or_else(|| OAuthError::InvalidResponse("token response has no accessToken".into()))?;
    let refresh = text(data, "refreshToken").or_else(|| text(data, "refresh_token")).map(Secret::new);
    let expires_at = match data.get("expiresIn").or_else(|| data.get("expires_in")).and_then(Value::as_i64) {
        Some(secs) if secs > 0 => Some(Utc::now() + chrono::Duration::seconds(secs)),
        _ => None,
    };
    Ok(TokenSet {
        access_token: Secret::new(access),
        refresh_token: refresh,
        expires_at,
        scopes: scopes.to_vec(),
        extra: Default::default(),
    })
}

/// 从 state-poll 上游的令牌信封响应里取出令牌；供刷新路径复用。
pub(super) fn token_from_envelope_reply(reply: &http::Reply, scopes: &[String]) -> Result<TokenSet, OAuthError> {
    if !(200..300).contains(&reply.status) {
        return Err(OAuthError::TokenEndpoint {
            status: reply.status,
            code: "token_endpoint".to_owned(),
            description: None,
        });
    }
    let body = reply
        .body
        .as_ref()
        .ok_or_else(|| OAuthError::InvalidResponse("token response is not JSON".into()))?;
    if code_of(body) != Some(CODE_OK) {
        return Err(OAuthError::InvalidResponse("token response has no code 0".into()));
    }
    token_from_envelope(body, scopes)
}

pub(super) async fn start(
    http: Client,
    cfg: OAuthProviderConfig,
    creds: ClientCredentials,
    tuning: &OAuthTuning,
    cancel: CancelHandle,
) -> Result<StateSession, OAuthError> {
    let _client = cfg.resolve_client(&creds)?;
    let state_url = cfg.state_endpoint(&creds)?;
    let token_url = cfg.token_endpoint(&creds)?;

    let headers = ExtraHeaders::from_config(&cfg.request_headers, cfg.user_agent.as_deref());
    let platform = cfg.state_poll_platform.clone().unwrap_or_else(|| "CLI".to_owned());
    // state 端点的 platform 是查询参数，不是 body 字段。
    let url = match state_url.split_once('?') {
        Some((base, _)) => format!("{base}?platform={platform}"),
        None => format!("{state_url}?platform={platform}"),
    };

    let reply = http::post_with(&http, &url, http::Encoding::Json, &[], &headers).await?;
    if !(200..300).contains(&reply.status) {
        return Err(OAuthError::TokenEndpoint {
            status: reply.status,
            code: "state_request_failed".to_owned(),
            description: None,
        });
    }
    let body = reply
        .body
        .ok_or_else(|| OAuthError::InvalidResponse("state response is not JSON".into()))?;
    if code_of(&body) != Some(CODE_OK) {
        return Err(OAuthError::InvalidResponse("state response has no code 0".into()));
    }
    let data = body.get("data").ok_or_else(|| OAuthError::InvalidResponse("state response has no data".into()))?;
    let state = text(data, "state").ok_or_else(|| OAuthError::InvalidResponse("state response has no state".into()))?;
    let auth_url = text(data, "authUrl")
        .or_else(|| text(data, "auth_url"))
        .ok_or_else(|| OAuthError::InvalidResponse("state response has no authUrl".into()))?;
    // 用户会在系统浏览器里打开这个地址，同样要求 https（回环除外）。
    http::ensure_secure_endpoint(auth_url)?;

    let interval = tuning
        .device_interval_override
        .unwrap_or_else(|| {
            cfg.state_poll_interval_ms
                .map(Duration::from_millis)
                .filter(|d| !d.is_zero())
                .unwrap_or(Duration::from_millis(DEFAULT_INTERVAL_MS))
        });
    let expires_at = Utc::now() + chrono::Duration::seconds(DEFAULT_EXPIRES_SECS);

    tracing::info!(provider = %cfg.id, interval_ms = interval.as_millis() as u64, "state-poll authorization started");
    Ok(StateSession {
        info: StatePollInfo {
            provider_id: cfg.id.clone(),
            authorize_url: auth_url.to_owned(),
            expires_at,
        },
        cfg,
        http,
        token_url,
        state: Secret::new(state),
        headers,
        interval,
        cancel,
    })
}

enum Poll {
    Pending,
    Done(TokenSet),
}

impl StateSession {
    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    fn poll_url(&self) -> String {
        let sep = if self.token_url.contains('?') { '&' } else { '?' };
        // state 需要 URL 编码；用 form 编码器保证 %XX 正确。
        format!(
            "{}{sep}state={}",
            self.token_url,
            http::form_encode(&[("state".to_owned(), self.state.expose().to_owned())])
                .trim_start_matches("state=")
                .to_owned()
        )
    }

    async fn poll_once(&self) -> Result<Poll, OAuthError> {
        let reply = http::get_with(&self.http, &self.poll_url(), &self.headers).await?;
        let secrets: Vec<&str> = vec![self.state.expose()];
        if !(200..300).contains(&reply.status) {
            return Err(OAuthError::Transient(format!("token endpoint returned HTTP {}", reply.status)));
        }
        let Some(body) = reply.body.as_ref() else {
            return Err(OAuthError::InvalidResponse("token poll response is not JSON".into()));
        };
        match code_of(body) {
            Some(CODE_OK) => token_from_envelope(body, &self.cfg.scopes).map(Poll::Done),
            Some(CODE_PENDING) => Ok(Poll::Pending),
            Some(_) => {
                // 其它非 0 码：可能是仍在登录，也可能是明确拒绝；取上游 msg 脱敏后分类。
                let (code, _) = http::error_fields(reply.body.as_ref(), &secrets);
                if code == "unknown" {
                    Ok(Poll::Pending)
                } else {
                    Err(OAuthError::AccessDenied)
                }
            }
            None => Err(OAuthError::InvalidResponse("token poll response has no code".into())),
        }
    }

    /// 按 interval 轮询直到登录完成、过期、超时或被取消。
    pub async fn wait(self) -> Result<TokenSet, OAuthError> {
        let interval = self.interval;
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
    fn envelope_success_maps_to_token_set() {
        let t = token_from_envelope(
            &json!({"code": 0, "data": {"accessToken": "AT", "refreshToken": "RT", "expiresIn": 3600}}),
            &["s".to_owned()],
        )
        .unwrap();
        assert_eq!(t.access_token.expose(), "AT");
        assert_eq!(t.refresh_token.unwrap().expose(), "RT");
        assert!(t.expires_at.is_some());
    }

    #[test]
    fn envelope_snake_case_and_missing_token() {
        let t = token_from_envelope(&json!({"code": 0, "data": {"access_token": "AT2"}}), &[]).unwrap();
        assert_eq!(t.access_token.expose(), "AT2");
        assert!(t.refresh_token.is_none());
        assert!(token_from_envelope(&json!({"code": 0, "data": {}}), &[]).is_err());
        assert!(token_from_envelope(&json!({"code": 0}), &[]).is_err());
    }

    #[test]
    fn code_parsing_accepts_numbers_and_numeric_strings() {
        assert_eq!(code_of(&json!({"code": 0})), Some(0));
        assert_eq!(code_of(&json!({"code": "11217"})), Some(11217));
        assert_eq!(code_of(&json!({"code": "x"})), None);
        assert_eq!(code_of(&json!({})), None);
    }
}

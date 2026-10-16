//! 令牌刷新：form / json 两种编码，并把失败分成「需重新授权」与「可重试」。

use chrono::Utc;
use reqwest::Client;

use super::config::{ClientCredentials, FlowKind, OAuthProviderConfig};
use super::error::OAuthError;
use super::http;
use super::state_poll;
use super::token::{tokens_from_reply, TokenSet};

/// 这些 error 码表示 refresh token 已不可用，重试没有意义。
const REAUTH_CODES: [&str; 9] = [
    "invalid_grant",
    "invalid_token",
    "invalid_client",
    "unauthorized_client",
    "access_denied",
    "bad_refresh_token",
    "refresh_token_expired",
    "invalid_refresh_token",
    "expired_token",
];

fn reauth(cfg: &OAuthProviderConfig, reason: impl Into<String>) -> OAuthError {
    OAuthError::ReauthRequired { provider: cfg.id.clone(), reason: reason.into() }
}

/// 用 refresh token 换新令牌。上游未轮换 refresh token 时沿用旧的。
pub async fn refresh_tokens(
    http: &Client,
    cfg: &OAuthProviderConfig,
    creds: &ClientCredentials,
    current: &TokenSet,
) -> Result<TokenSet, OAuthError> {
    let Some(refresh_token) = current.refresh_token.as_ref().filter(|t| !t.is_empty()) else {
        return Err(reauth(cfg, "no refresh token stored"));
    };
    let Some(url) = cfg.refresh_endpoint(creds)? else {
        return Err(reauth(cfg, "provider does not support token refresh"));
    };
    let client = cfg.resolve_client(creds)?;

    let mut headers = http::ExtraHeaders::from_config(&cfg.request_headers, cfg.user_agent.as_deref());
    let encoding = cfg.refresh_request_encoding.unwrap_or(cfg.refresh_encoding);

    let mut fields: Vec<(String, String)> = vec![];
    if let Some(header) = cfg.refresh_token_header.as_deref() {
        headers.0.push((header.to_owned(), refresh_token.expose().to_owned()));
    }
    // CodeBuddy only needs the header; Nous requires the token in both places.
    if cfg.refresh_token_header.is_none() || cfg.refresh_token_in_body_with_header {
        fields.push(("grant_type".into(), "refresh_token".into()));
        fields.push(("refresh_token".into(), refresh_token.expose().to_owned()));
        fields.push(("client_id".into(), client.client_id.clone()));
        if let Some(secret) = &client.client_secret {
            fields.push(("client_secret".into(), secret.expose().to_owned()));
        }
        if let Some(scope) = &cfg.refresh_scope {
            fields.push(("scope".into(), scope.clone()));
        }
    }

    let reply = http::post_with(http, &url, encoding, &fields, &headers).await?;

    let mut secrets: Vec<&str> = vec![refresh_token.expose()];
    if let Some(s) = &client.client_secret {
        secrets.push(s.expose());
    }
    let mut fresh = match tokens_from_reply(&reply, &current.scopes, &secrets) {
        Ok(tokens) => tokens,
        // state-poll 类上游把令牌包在 `{ code, data: { accessToken } }` 里，
        // `tokens_from_reply` 认不出，这里补一次。
        Err(_) if cfg.flow == FlowKind::StatePoll => {
            state_poll::token_from_envelope_reply(&reply, &current.scopes).map_err(|_| reauth(cfg, "invalid refresh response"))?
        }
        Err(OAuthError::TokenEndpoint { status, code, .. })
            if status == 401 || REAUTH_CODES.contains(&code.as_str()) =>
        {
            return Err(reauth(cfg, code));
        }
        Err(other) => return Err(other),
    };

    if fresh.refresh_token.is_none() {
        fresh.refresh_token = current.refresh_token.clone();
    }
    // 沿用旧的附加字段（例如 id_token），新响应里的同名字段优先。
    for (key, value) in &current.extra {
        fresh.extra.entry(key.clone()).or_insert_with(|| value.clone());
    }
    if cfg.id == "nous" {
        fresh.extra.remove("nous_agent_key");
    }
    if fresh.scopes.is_empty() {
        fresh.scopes = current.scopes.clone();
    }
    debug_assert!(fresh.expires_at.is_none_or(|at| at > Utc::now() - chrono::Duration::days(1)));
    Ok(fresh)
}

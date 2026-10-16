//! Nous Portal OAuth tokens require a separate short-lived inference Agent Key.

use chrono::{DateTime, Duration, Utc};
use reqwest::Client;
use serde_json::{json, Value};

use super::config::{ClientCredentials, OAuthProviderConfig};
use super::error::OAuthError;
use super::http::{self, ExtraHeaders};
use super::secret::Secret;
use super::token::TokenSet;

pub async fn mint_agent_key(
    client: &Client,
    cfg: &OAuthProviderConfig,
    creds: &ClientCredentials,
    mut tokens: TokenSet,
) -> Result<TokenSet, OAuthError> {
    let url = cfg.agent_key_endpoint(creds)?;
    let mut headers = ExtraHeaders::default();
    headers.0.push(("Authorization".into(), format!("Bearer {}", tokens.access_token.expose())));
    let reply = http::post_json_with(client, &cfg.id, &url, &json!({"min_ttl_seconds": 7200}), &headers).await?;
    match reply.status {
        200..=299 => {}
        401 | 403 => return Err(OAuthError::ReauthRequired {
            provider: cfg.id.clone(), reason: "Portal rejected the inference credential exchange".into(),
        }),
        408 | 425 | 429 | 500..=599 => return Err(OAuthError::Transient(format!(
            "Nous Agent Key exchange returned HTTP {}", reply.status,
        ))),
        status => {
            let (code, description) = http::error_fields(reply.body.as_ref(), &[tokens.access_token.expose()]);
            return Err(OAuthError::TokenEndpoint { status, code, description });
        }
    }
    let body = reply.body.ok_or_else(|| OAuthError::InvalidResponse("Nous Agent Key response is not JSON".into()))?;
    let key = body.get("api_key").and_then(Value::as_str).filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::InvalidResponse("Nous Agent Key response has no api_key".into()))?;
    tokens.extra.insert("nous_agent_key".into(), Secret::new(key));

    // Never keep an Agent Key beyond its own lifetime, even if the Portal token lasts longer.
    let ttl = body.get("expires_in").and_then(Value::as_i64).filter(|s| *s > 0).unwrap_or(7200).min(7200);
    let key_expiry = body.get("expires_at").and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc::now() + Duration::seconds(ttl));
    tokens.expires_at = Some(tokens.expires_at.map_or(key_expiry, |portal| portal.min(key_expiry)));
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::{HeaderMap, StatusCode}, routing::post, Json, Router};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn portal_token_is_exchanged_for_inference_key_without_exposing_it() {
        async fn exchange(headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer PORTAL-SECRET")
                || body["min_ttl_seconds"] != 7200 {
                return (StatusCode::UNAUTHORIZED, Json(json!({"error":"unauthorized"})));
            }
            (StatusCode::OK, Json(json!({"api_key":"AGENT-SECRET", "expires_in":1800})))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/agent", listener.local_addr().unwrap());
        let app = Router::new().route("/agent", post(exchange));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let cfg: OAuthProviderConfig = serde_json::from_value(json!({
            "id":"nous", "flow":"device", "clientId":"hermes-cli",
            "deviceCodeUrl":"https://portal.nousresearch.com/api/oauth/device/code",
            "tokenUrl":"https://portal.nousresearch.com/api/oauth/token", "agentKeyUrl":url,
        })).unwrap();
        let tokens = TokenSet { access_token: Secret::new("PORTAL-SECRET"), refresh_token: None,
            expires_at: Some(Utc::now() + Duration::hours(3)), scopes: vec![], extra: BTreeMap::new() };
        let client = http::build_client(std::time::Duration::from_secs(3)).unwrap();
        let result = mint_agent_key(&client, &cfg, &ClientCredentials::default(), tokens).await.unwrap();
        assert_eq!(result.extra_value("nous_agent_key"), Some("AGENT-SECRET"));
        assert!(result.expires_at.unwrap() < Utc::now() + Duration::minutes(31));
        let debug = format!("{result:?}");
        assert!(!debug.contains("PORTAL-SECRET") && !debug.contains("AGENT-SECRET"));
        server.abort();
    }
}

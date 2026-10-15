//! 令牌模型、令牌响应解析与 `TokenStore` 抽象。

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::OAuthError;
use super::http::{error_fields, Reply};
use super::secret::Secret;

/// 一个账户的令牌集合。`Debug` 脱敏：不会输出任何令牌或 extra 的值。
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    /// None 表示上游没有给出有效期（例如 Muse 铸造的长期密钥）。
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    /// 上游返回的其它标量字段（id_token、token_type 等）。一律视为敏感。
    pub extra: BTreeMap<String, Secret>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("extra_keys", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl TokenSet {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }

    /// 距离到期不足 `lead_seconds`（含已过期）时需要刷新。没有到期时间则不主动刷新。
    pub fn needs_refresh(&self, lead_seconds: i64, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at - Duration::seconds(lead_seconds) <= now)
    }

    pub fn extra_value(&self, key: &str) -> Option<&str> {
        self.extra.get(key).map(Secret::expose)
    }

    /// Codex account binding lives in the signed JWT claims. Only use a short,
    /// printable claim as an HTTP header; never expose the JWT itself.
    pub fn chatgpt_account_id(&self) -> Option<String> {
        [Some(self.access_token.expose()), self.extra_value("id_token")]
            .into_iter().flatten().find_map(|jwt| {
                let payload = jwt.split('.').nth(1)?;
                let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
                let claims: Value = serde_json::from_slice(&bytes).ok()?;
                let id = claims.get("https://api.openai.com/auth")
                    .and_then(|v| v.get("chatgpt_account_id"))
                    .or_else(|| claims.get("chatgpt_account_id"))?
                    .as_str()?;
                (!id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic()))
                    .then(|| id.to_owned())
            })
    }

    /// 序列化为可写入系统凭据库的 JSON。返回 `Secret`，避免被误打印。
    pub fn to_storage_json(&self) -> Result<Secret, OAuthError> {
        let dto = StoredTokenSet {
            access_token: self.access_token.expose().to_owned(),
            refresh_token: self.refresh_token.as_ref().map(|s| s.expose().to_owned()),
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
            extra: self.extra.iter().map(|(k, v)| (k.clone(), v.expose().to_owned())).collect(),
        };
        serde_json::to_string(&dto)
            .map(Secret::new)
            .map_err(|_| OAuthError::Storage("failed to serialize token set".into()))
    }

    pub fn from_storage_json(json: &str) -> Result<Self, OAuthError> {
        let dto: StoredTokenSet = serde_json::from_str(json)
            .map_err(|_| OAuthError::Storage("stored token set is malformed".into()))?;
        Ok(TokenSet {
            access_token: Secret::new(dto.access_token),
            refresh_token: dto.refresh_token.map(Secret::new),
            expires_at: dto.expires_at,
            scopes: dto.scopes,
            extra: dto.extra.into_iter().map(|(k, v)| (k, Secret::new(v))).collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredTokenSet {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    extra: BTreeMap<String, String>,
}

#[cfg(test)]
mod account_tests {
    use super::*;

    #[test]
    fn account_claim_is_extracted_without_exposing_token() {
        let claims = serde_json::json!({"https://api.openai.com/auth":{"chatgpt_account_id":"acc_123"}});
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let tokens = TokenSet { access_token: Secret::new(format!("header.{payload}.signature")),
            refresh_token: None, expires_at: None, scopes: vec![], extra: BTreeMap::new() };
        assert_eq!(tokens.chatgpt_account_id().as_deref(), Some("acc_123"));
        assert!(!format!("{tokens:?}").contains("acc_123"));
    }
}

/// 令牌存储抽象。数据层用系统凭据库（Keychain / Credential Manager）实现；
/// `key` 是连接的 `credential_ref`。实现者不得在日志或错误中带入令牌内容。
#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn load(&self, key: &str) -> Result<Option<TokenSet>, OAuthError>;
    async fn save(&self, key: &str, tokens: &TokenSet) -> Result<(), OAuthError>;
    async fn delete(&self, key: &str) -> Result<(), OAuthError>;
}

/// 内存实现，用于测试与临时场景。进程退出即丢失。
#[derive(Default)]
pub struct MemoryTokenStore {
    inner: Mutex<HashMap<String, TokenSet>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TokenStore for MemoryTokenStore {
    async fn load(&self, key: &str) -> Result<Option<TokenSet>, OAuthError> {
        Ok(self.inner.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned())
    }

    async fn save(&self, key: &str, tokens: &TokenSet) -> Result<(), OAuthError> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_owned(), tokens.clone());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), OAuthError> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }
}

const KNOWN_KEYS: [&str; 4] = ["access_token", "refresh_token", "expires_in", "scope"];

fn as_seconds(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// 解析成功的令牌响应体。`requested_scopes` 在响应没有 `scope` 字段时作为回退。
pub fn parse_token_response(
    body: &Value,
    requested_scopes: &[String],
    now: DateTime<Utc>,
) -> Result<TokenSet, OAuthError> {
    let access = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::InvalidResponse("token response has no access_token".into()))?;
    let refresh = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(Secret::new);
    let expires_at = body
        .get("expires_in")
        .and_then(as_seconds)
        .filter(|secs| *secs > 0)
        .map(|secs| now + Duration::seconds(secs));
    let scopes = match body.get("scope") {
        Some(Value::String(s)) => s.split_whitespace().map(str::to_owned).collect(),
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => requested_scopes.to_vec(),
    };
    let mut extra = BTreeMap::new();
    if let Some(obj) = body.as_object() {
        for (key, value) in obj {
            if KNOWN_KEYS.contains(&key.as_str()) {
                continue;
            }
            match value {
                Value::String(s) => {
                    extra.insert(key.clone(), Secret::new(s.as_str()));
                }
                Value::Number(n) => {
                    extra.insert(key.clone(), Secret::new(n.to_string()));
                }
                Value::Bool(b) => {
                    extra.insert(key.clone(), Secret::new(b.to_string()));
                }
                _ => {}
            }
        }
    }
    Ok(TokenSet { access_token: Secret::new(access), refresh_token: refresh, expires_at, scopes, extra })
}

/// 把令牌端点的响应转成 `TokenSet` 或分类后的错误。
/// `secrets` 是本次请求涉及的机密（授权码、verifier、refresh token 等），会从说明文字里抹掉。
pub fn tokens_from_reply(
    reply: &Reply,
    requested_scopes: &[String],
    secrets: &[&str],
) -> Result<TokenSet, OAuthError> {
    let has_access = reply
        .body
        .as_ref()
        .and_then(|b| b.get("access_token"))
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if (200..300).contains(&reply.status) && has_access {
        let body = reply.body.as_ref().expect("checked above");
        return parse_token_response(body, requested_scopes, Utc::now());
    }
    match reply.status {
        408 | 425 | 429 | 500..=599 => Err(OAuthError::Transient(format!("authorization server returned HTTP {}", reply.status))),
        200..=299 => {
            // 2xx 但没有令牌：有些服务端用 200 + error 字段表达失败。
            let (code, description) = error_fields(reply.body.as_ref(), secrets);
            if code == "unknown" {
                Err(OAuthError::InvalidResponse("token response has no access_token".into()))
            } else {
                Err(OAuthError::TokenEndpoint { status: reply.status, code, description })
            }
        }
        status => {
            let (code, description) = error_fields(reply.body.as_ref(), secrets);
            Err(OAuthError::TokenEndpoint { status, code, description })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> TokenSet {
        parse_token_response(
            &json!({
                "access_token": "AT-very-secret",
                "refresh_token": "RT-very-secret",
                "expires_in": 3600,
                "scope": "a b",
                "id_token": "IDT-very-secret",
                "token_type": "Bearer",
                "nested": {"ignored": true}
            }),
            &[],
            Utc::now(),
        )
        .unwrap()
    }

    #[test]
    fn debug_is_redacted() {
        let dbg = format!("{:?}", sample());
        for leak in ["AT-very-secret", "RT-very-secret", "IDT-very-secret"] {
            assert!(!dbg.contains(leak), "Debug leaked {leak}");
        }
        assert!(dbg.contains("id_token"), "extra keys may be listed");
    }

    #[test]
    fn parses_fields() {
        let t = sample();
        assert_eq!(t.access_token.expose(), "AT-very-secret");
        assert_eq!(t.refresh_token.as_ref().unwrap().expose(), "RT-very-secret");
        assert_eq!(t.scopes, vec!["a", "b"]);
        assert_eq!(t.extra_value("token_type"), Some("Bearer"));
        assert!(t.extra_value("nested").is_none());
        assert!(t.expires_at.unwrap() > Utc::now());
    }

    #[test]
    fn expires_in_as_string_and_missing() {
        let t = parse_token_response(&json!({"access_token": "x", "expires_in": "120"}), &["r".into()], Utc::now()).unwrap();
        assert!(t.expires_at.is_some());
        assert_eq!(t.scopes, vec!["r"]);
        let t = parse_token_response(&json!({"access_token": "x"}), &[], Utc::now()).unwrap();
        assert!(t.expires_at.is_none());
        assert!(!t.needs_refresh(300, Utc::now()));
    }

    #[test]
    fn missing_access_token_is_invalid() {
        assert!(matches!(
            parse_token_response(&json!({"refresh_token": "x"}), &[], Utc::now()),
            Err(OAuthError::InvalidResponse(_))
        ));
    }

    #[test]
    fn needs_refresh_uses_lead_time() {
        let now = Utc::now();
        let mut t = sample();
        t.expires_at = Some(now + Duration::seconds(100));
        assert!(t.needs_refresh(300, now));
        assert!(!t.needs_refresh(50, now));
        t.expires_at = Some(now - Duration::seconds(1));
        assert!(t.is_expired(now));
    }

    #[test]
    fn storage_roundtrip_and_secret_wrapper() {
        let t = sample();
        let json = t.to_storage_json().unwrap();
        assert!(!format!("{json:?}").contains("AT-very-secret"));
        let back = TokenSet::from_storage_json(json.expose()).unwrap();
        assert_eq!(back.access_token.expose(), "AT-very-secret");
        assert_eq!(back.extra_value("id_token"), Some("IDT-very-secret"));
        assert!(TokenSet::from_storage_json("{").is_err());
    }

    #[test]
    fn reply_classification() {
        let ok = Reply { status: 200, body: Some(json!({"access_token": "t"})) };
        assert!(tokens_from_reply(&ok, &[], &[]).is_ok());
        let e500 = Reply { status: 503, body: None };
        assert!(tokens_from_reply(&e500, &[], &[]).unwrap_err().is_retryable());
        let e429 = Reply { status: 429, body: None };
        assert!(tokens_from_reply(&e429, &[], &[]).unwrap_err().is_retryable());
        let e400 = Reply { status: 400, body: Some(json!({"error": "invalid_grant"})) };
        assert!(matches!(
            tokens_from_reply(&e400, &[], &[]),
            Err(OAuthError::TokenEndpoint { status: 400, .. })
        ));
        let gh = Reply { status: 200, body: Some(json!({"error": "bad_verification_code"})) };
        assert!(matches!(tokens_from_reply(&gh, &[], &[]), Err(OAuthError::TokenEndpoint { .. })));
        let empty = Reply { status: 200, body: Some(json!({})) };
        assert!(matches!(tokens_from_reply(&empty, &[], &[]), Err(OAuthError::InvalidResponse(_))));
    }

    #[tokio::test]
    async fn memory_store_works() {
        let store = MemoryTokenStore::new();
        assert!(store.load("k").await.unwrap().is_none());
        store.save("k", &sample()).await.unwrap();
        assert!(store.load("k").await.unwrap().is_some());
        store.delete("k").await.unwrap();
        assert!(store.load("k").await.unwrap().is_none());
    }
}

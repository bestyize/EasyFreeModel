//! OAuth 框架的凭据库实现：令牌（`TokenStore`）与用户自备的客户端凭据（`ClientCredentialSource`）。
//!
//! - 令牌条目的 key 就是 `Connection.credential_ref`；值是 `TokenSet::to_storage_json()` 的 JSON。
//! - 客户端凭据条目的 key 是 `oauth-client/<provider_id>`；值是 `{client_id, client_secret, base_url}`。
//! - 所有错误统一映射为 `OAuthError::Storage`，文案固定，不带 key、不带内容，也不带凭据库的原始错误文本
//!   （平台错误的文本不受我们控制）。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::oauth::{
    ClientCredentialAdmin, ClientCredentialSource, ClientCredentials, OAuthError, Secret, TokenSet, TokenStore,
};
use crate::security::vault::{self, CredentialVault};

fn storage(msg: &'static str) -> OAuthError {
    OAuthError::Storage(msg.to_owned())
}

/// 令牌 -> 系统凭据库。
pub struct VaultTokenStore {
    vault: Arc<dyn CredentialVault>,
}

impl VaultTokenStore {
    pub fn new(vault: Arc<dyn CredentialVault>) -> Self {
        Self { vault }
    }
}

#[async_trait]
impl TokenStore for VaultTokenStore {
    async fn load(&self, key: &str) -> Result<Option<TokenSet>, OAuthError> {
        let raw = vault::get(&self.vault, key).await.map_err(|_| storage("credential vault read failed"))?;
        match raw {
            None => Ok(None),
            Some(json) => {
                let json = Zeroizing::new(json);
                TokenSet::from_storage_json(&json).map(Some)
            }
        }
    }

    async fn save(&self, key: &str, tokens: &TokenSet) -> Result<(), OAuthError> {
        let json = tokens.to_storage_json()?;
        vault::set(&self.vault, key, json.expose())
            .await
            .map_err(|_| storage("credential vault write failed"))
    }

    async fn delete(&self, key: &str) -> Result<(), OAuthError> {
        vault::delete(&self.vault, key).await.map_err(|_| storage("credential vault delete failed"))
    }
}

/// 客户端凭据条目的存储形态。
#[derive(Serialize, Deserialize, Default)]
struct StoredClient {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
}

/// 用户自备的客户端凭据 -> 系统凭据库。没有条目时返回 `ClientCredentials::default()`。
pub struct VaultClientCredentials {
    vault: Arc<dyn CredentialVault>,
}

pub fn client_key(provider_id: &str) -> String {
    format!("oauth-client/{provider_id}")
}

impl VaultClientCredentials {
    pub fn new(vault: Arc<dyn CredentialVault>) -> Self {
        Self { vault }
    }

    async fn read(&self, provider_id: &str) -> Result<Option<StoredClient>, OAuthError> {
        let raw = vault::get(&self.vault, &client_key(provider_id))
            .await
            .map_err(|_| storage("credential vault read failed"))?;
        match raw {
            None => Ok(None),
            Some(json) => {
                let json = Zeroizing::new(json);
                serde_json::from_str::<StoredClient>(&json)
                    .map(Some)
                    .map_err(|_| storage("stored client credentials are malformed"))
            }
        }
    }
}

#[async_trait]
impl ClientCredentialSource for VaultClientCredentials {
    async fn credentials_for(&self, provider_id: &str) -> Result<ClientCredentials, OAuthError> {
        Ok(match self.read(provider_id).await? {
            None => ClientCredentials::default(),
            Some(stored) => ClientCredentials {
                client_id: stored.client_id.filter(|s| !s.trim().is_empty()),
                client_secret: stored.client_secret.filter(|s| !s.is_empty()).map(Secret::new),
                base_url: stored.base_url.filter(|s| !s.trim().is_empty()),
            },
        })
    }
}

#[async_trait]
impl ClientCredentialAdmin for VaultClientCredentials {
    async fn save_credentials(&self, provider_id: &str, creds: &ClientCredentials) -> Result<(), OAuthError> {
        let stored = StoredClient {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.as_ref().map(|s| s.expose().to_owned()),
            base_url: creds.base_url.clone(),
        };
        let json = Zeroizing::new(serde_json::to_string(&stored).map_err(|_| storage("failed to serialize client credentials"))?);
        vault::set(&self.vault, &client_key(provider_id), &json)
            .await
            .map_err(|_| storage("credential vault write failed"))
    }

    async fn clear_credentials(&self, provider_id: &str) -> Result<(), OAuthError> {
        vault::delete(&self.vault, &client_key(provider_id))
            .await
            .map_err(|_| storage("credential vault delete failed"))
    }

    async fn has_credentials(&self, provider_id: &str) -> Result<bool, OAuthError> {
        Ok(self.read(provider_id).await?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{AppError, AppResult};
    use crate::oauth::token::parse_token_response;
    use crate::security::vault::MemoryVault;
    use chrono::Utc;
    use serde_json::json;

    fn sample() -> TokenSet {
        parse_token_response(
            &json!({"access_token": "AT-vault-secret", "refresh_token": "RT-vault-secret", "expires_in": 3600,
                    "scope": "a b", "id_token": "IDT-vault-secret"}),
            &[],
            Utc::now(),
        )
        .unwrap()
    }

    /// 凭据库的所有操作都失败，且平台错误文本里“恰好”带着机密：映射后的错误必须不含它。
    struct LeakyVault;

    impl CredentialVault for LeakyVault {
        fn set(&self, key: &str, secret: &str) -> AppResult<()> {
            Err(AppError::msg(format!("platform failure for {key} with {secret}")))
        }
        fn get(&self, key: &str) -> AppResult<Option<String>> {
            Err(AppError::msg(format!("platform failure for {key}: AT-vault-secret")))
        }
        fn delete(&self, key: &str) -> AppResult<()> {
            Err(AppError::msg(format!("platform failure for {key}: AT-vault-secret")))
        }
    }

    #[tokio::test]
    async fn token_store_round_trips_through_the_vault() {
        let vault: Arc<dyn CredentialVault> = Arc::new(MemoryVault::default());
        let store = VaultTokenStore::new(vault.clone());
        assert!(store.load("connection/c1/r1").await.unwrap().is_none());

        store.save("connection/c1/r1", &sample()).await.unwrap();
        let back = store.load("connection/c1/r1").await.unwrap().expect("token set is stored");
        assert_eq!(back.access_token.expose(), "AT-vault-secret");
        assert_eq!(back.refresh_token.as_ref().unwrap().expose(), "RT-vault-secret");
        assert_eq!(back.extra_value("id_token"), Some("IDT-vault-secret"));
        assert_eq!(back.scopes, vec!["a", "b"]);
        assert!(back.expires_at.is_some());

        // 凭据库里存的是 JSON（由框架序列化），不是裸 access token
        let raw = vault.get("connection/c1/r1").unwrap().unwrap();
        assert!(raw.contains("access_token"));

        store.delete("connection/c1/r1").await.unwrap();
        assert!(store.load("connection/c1/r1").await.unwrap().is_none());
        store.delete("connection/c1/r1").await.unwrap(); // 幂等
    }

    #[tokio::test]
    async fn token_store_errors_never_carry_content_or_keys() {
        let leaky: Arc<dyn CredentialVault> = Arc::new(LeakyVault);
        let store = VaultTokenStore::new(leaky);
        let errors = [
            store.load("connection/secret-ref/xyz").await.unwrap_err(),
            store.save("connection/secret-ref/xyz", &sample()).await.unwrap_err(),
            store.delete("connection/secret-ref/xyz").await.unwrap_err(),
        ];
        for e in errors {
            assert!(matches!(e, OAuthError::Storage(_)), "{e:?}");
            for text in [e.to_string(), format!("{e:?}")] {
                for leak in ["AT-vault-secret", "RT-vault-secret", "secret-ref", "platform failure"] {
                    assert!(!text.contains(leak), "error leaked {leak}: {text}");
                }
            }
            assert_eq!(e.code(), "storage");
        }
    }

    #[tokio::test]
    async fn malformed_entries_are_a_content_free_storage_error() {
        let vault: Arc<dyn CredentialVault> = Arc::new(MemoryVault::default());
        vault.set("k", "{\"access_token\": 12345 SECRET-GARBAGE").unwrap();
        let err = VaultTokenStore::new(vault).load("k").await.unwrap_err();
        assert!(matches!(err, OAuthError::Storage(_)));
        assert!(!err.to_string().contains("SECRET-GARBAGE"));
    }

    #[tokio::test]
    async fn client_credentials_round_trip_and_default_when_absent() {
        let vault: Arc<dyn CredentialVault> = Arc::new(MemoryVault::default());
        let creds = VaultClientCredentials::new(vault.clone());

        let none = creds.credentials_for("antigravity").await.unwrap();
        assert!(none.client_id.is_none() && none.client_secret.is_none() && none.base_url.is_none());
        assert!(!creds.has_credentials("antigravity").await.unwrap());

        creds
            .save_credentials(
                "antigravity",
                &ClientCredentials {
                    client_id: Some("my-client-id".into()),
                    client_secret: Some(Secret::new("my-client-secret")),
                    base_url: Some("https://git.example.org".into()),
                },
            )
            .await
            .unwrap();
        assert!(creds.has_credentials("antigravity").await.unwrap());
        assert!(!creds.has_credentials("gemini-cli").await.unwrap(), "按供应商隔离");

        let back = creds.credentials_for("antigravity").await.unwrap();
        assert_eq!(back.client_id.as_deref(), Some("my-client-id"));
        assert_eq!(back.client_secret.as_ref().unwrap().expose(), "my-client-secret");
        assert_eq!(back.base_url.as_deref(), Some("https://git.example.org"));
        assert!(!format!("{back:?}").contains("my-client-secret"));

        // 条目 key 固定，值是 JSON
        let raw = vault.get("oauth-client/antigravity").unwrap().unwrap();
        assert!(raw.contains("client_id"));

        creds.clear_credentials("antigravity").await.unwrap();
        assert!(!creds.has_credentials("antigravity").await.unwrap());
        creds.clear_credentials("antigravity").await.unwrap(); // 幂等
    }

    #[tokio::test]
    async fn client_credential_errors_never_carry_content() {
        let creds = VaultClientCredentials::new(Arc::new(LeakyVault));
        let secret_creds = ClientCredentials {
            client_id: Some("cid".into()),
            client_secret: Some(Secret::new("AT-vault-secret")),
            base_url: None,
        };
        let errors = [
            creds.credentials_for("antigravity").await.unwrap_err(),
            creds.save_credentials("antigravity", &secret_creds).await.unwrap_err(),
            creds.clear_credentials("antigravity").await.unwrap_err(),
            creds.has_credentials("antigravity").await.unwrap_err(),
        ];
        for e in errors {
            assert!(matches!(e, OAuthError::Storage(_)));
            let text = format!("{e} {e:?}");
            assert!(!text.contains("AT-vault-secret") && !text.contains("platform failure") && !text.contains("oauth-client"), "{text}");
        }
    }
}

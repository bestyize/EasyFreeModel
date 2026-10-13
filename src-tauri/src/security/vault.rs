//! 第三方凭据（上游 API Key / OAuth 令牌 / 秘密请求头）的系统凭据库封装。
//!
//! 选型：`keyring` 4.2（默认 `v1` feature）。4.x 把平台存储拆成独立 crate，`v1` feature 会按目标平台
//! 自动选择：macOS Keychain（apple-native-keyring-store）、Windows Credential Manager
//! （windows-native-keyring-store）、Linux Secret Service（zbus）。无需自己注册 default store。
//!
//! SQLite 只保存 credential_ref；错误信息只包含 ref，不得包含密钥内容。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::{AppError, AppResult};

pub trait CredentialVault: Send + Sync + 'static {
    fn set(&self, key: &str, secret: &str) -> AppResult<()>;
    fn get(&self, key: &str) -> AppResult<Option<String>>;
    /// 幂等：条目不存在也返回 Ok。
    fn delete(&self, key: &str) -> AppResult<()>;
}

/// 生产实现：操作系统凭据库。
pub struct KeyringVault {
    service: String,
}

impl KeyringVault {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    fn entry(&self, key: &str) -> AppResult<keyring::Entry> {
        keyring::Entry::new(&self.service, key).map_err(map_err)
    }

    /// 凭据库是否可用（例如 Linux 无 Secret Service 时为 false）。
    pub fn is_available() -> bool {
        keyring::Entry::store_status().is_ok()
    }
}

fn map_err(e: keyring::Error) -> AppError {
    // keyring 的错误文本只描述平台故障，不包含密钥内容
    AppError::msg(format!("系统凭据库错误：{e}"))
}

impl CredentialVault for KeyringVault {
    fn set(&self, key: &str, secret: &str) -> AppResult<()> {
        self.entry(key)?.set_password(secret).map_err(map_err)
    }

    fn get(&self, key: &str) -> AppResult<Option<String>> {
        match self.entry(key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn delete(&self, key: &str) -> AppResult<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

/// 内存实现，供测试使用（也可作为凭据库不可用时的显式降级——目前不做静默降级）。
#[derive(Default)]
pub struct MemoryVault {
    items: Mutex<HashMap<String, String>>,
}

impl CredentialVault for MemoryVault {
    fn set(&self, key: &str, secret: &str) -> AppResult<()> {
        self.items.lock().unwrap().insert(key.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> AppResult<Option<String>> {
        Ok(self.items.lock().unwrap().get(key).cloned())
    }

    fn delete(&self, key: &str) -> AppResult<()> {
        self.items.lock().unwrap().remove(key);
        Ok(())
    }
}

// 系统凭据库调用是阻塞的（macOS 可能弹出授权框），放到 blocking 线程池里执行。

async fn blocking<T: Send + 'static>(
    vault: &Arc<dyn CredentialVault>,
    f: impl FnOnce(&dyn CredentialVault) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let vault = Arc::clone(vault);
    tokio::task::spawn_blocking(move || f(vault.as_ref()))
        .await
        .map_err(|e| AppError::msg(format!("凭据库任务失败：{e}")))?
}

pub async fn set(vault: &Arc<dyn CredentialVault>, key: &str, secret: &str) -> AppResult<()> {
    let (k, s) = (key.to_string(), secret.to_string());
    blocking(vault, move |v| v.set(&k, &s)).await
}

pub async fn get(vault: &Arc<dyn CredentialVault>, key: &str) -> AppResult<Option<String>> {
    let k = key.to_string();
    blocking(vault, move |v| v.get(&k)).await
}

pub async fn delete(vault: &Arc<dyn CredentialVault>, key: &str) -> AppResult<()> {
    let k = key.to_string();
    blocking(vault, move |v| v.delete(&k)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_vault_set_get_delete() {
        let v = MemoryVault::default();
        assert_eq!(v.get("a").unwrap(), None);
        v.set("a", "s1").unwrap();
        assert_eq!(v.get("a").unwrap().as_deref(), Some("s1"));
        v.set("a", "s2").unwrap();
        assert_eq!(v.get("a").unwrap().as_deref(), Some("s2"));
        v.delete("a").unwrap();
        assert_eq!(v.get("a").unwrap(), None);
        v.delete("a").unwrap(); // 幂等
    }

    #[tokio::test]
    async fn async_wrappers_work() {
        let v: Arc<dyn CredentialVault> = Arc::new(MemoryVault::default());
        set(&v, "k", "secret").await.unwrap();
        assert_eq!(get(&v, "k").await.unwrap().as_deref(), Some("secret"));
        delete(&v, "k").await.unwrap();
        assert_eq!(get(&v, "k").await.unwrap(), None);
    }
}

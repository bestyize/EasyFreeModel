//! 第三方凭据（上游 API Key / OAuth 令牌 / 秘密请求头）的系统凭据库封装。
//!
//! 选型：`keyring` 4.2（默认 `v1` feature）。4.x 把平台存储拆成独立 crate，`v1` feature 会按目标平台
//! 自动选择：macOS Keychain（apple-native-keyring-store）、Windows Credential Manager
//! （windows-native-keyring-store）、Linux Secret Service（zbus）。无需自己注册 default store。
//!
//! SQLite 只保存 credential_ref；错误信息只包含 ref，不得包含密钥内容。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::error::{AppError, AppResult};

pub trait CredentialVault: Send + Sync + 'static {
    fn set(&self, key: &str, secret: &str) -> AppResult<()>;
    fn get(&self, key: &str) -> AppResult<Option<String>>;
    /// 幂等：条目不存在也返回 Ok。
    fn delete(&self, key: &str) -> AppResult<()>;
}

/// 本地安全存储实现（对齐 9Router 机制，彻底杜绝系统钥匙串弹密码框）。
/// 数据保存在应用独立目录下（权限 0600，仅本用户可读写），内存缓存毫秒级读取。
pub struct LocalVault {
    path: PathBuf,
    items: RwLock<HashMap<String, String>>,
    fallback_keyring: Option<KeyringVault>,
}

impl LocalVault {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let items = Self::load_file(&path).unwrap_or_default();
        Self {
            path,
            items: RwLock::new(items),
            fallback_keyring: Some(KeyringVault::new("app.routerdesktop.client")),
        }
    }

    pub fn without_fallback(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let items = Self::load_file(&path).unwrap_or_default();
        Self {
            path,
            items: RwLock::new(items),
            fallback_keyring: None,
        }
    }

    fn load_file(path: &Path) -> Option<HashMap<String, String>> {
        if !path.exists() {
            return None;
        }
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn persist(&self) -> AppResult<()> {
        let guard = self.items.read().map_err(|_| AppError::msg("锁冲突"))?;
        let json = serde_json::to_string_pretty(&*guard)
            .map_err(|e| AppError::msg(format!("序列化凭据库失败：{e}")))?;

        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let tmp_path = self.path.with_extension("tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(|e| AppError::msg(format!("写入凭据文件失败：{e}")))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
            }

            file.write_all(json.as_bytes())
                .map_err(|e| AppError::msg(format!("写入凭据内容失败：{e}")))?;
            file.flush()
                .map_err(|e| AppError::msg(format!("刷新凭据内容失败：{e}")))?;
        }

        fs::rename(&tmp_path, &self.path)
            .map_err(|e| AppError::msg(format!("重命名凭据文件失败：{e}")))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }
}

impl CredentialVault for LocalVault {
    fn set(&self, key: &str, secret: &str) -> AppResult<()> {
        {
            let mut guard = self.items.write().map_err(|_| AppError::msg("锁冲突"))?;
            guard.insert(key.to_string(), secret.to_string());
        }
        self.persist()
    }

    fn get(&self, key: &str) -> AppResult<Option<String>> {
        // 1. 优先读取本地高速内存缓存，0 毫秒响应，永不弹窗
        {
            let guard = self.items.read().map_err(|_| AppError::msg("锁冲突"))?;
            if let Some(v) = guard.get(key) {
                return Ok(Some(v.clone()));
            }
        }

        // 2. 本地尚无时，尝试从历史钥匙串迁移一次已有凭据（迁移后存入本地，后续不再触发）
        if let Some(ref kr) = self.fallback_keyring {
            if let Ok(Some(legacy_secret)) = kr.get(key) {
                let _ = self.set(key, &legacy_secret);
                return Ok(Some(legacy_secret));
            }
        }

        Ok(None)
    }

    fn delete(&self, key: &str) -> AppResult<()> {
        let removed = {
            let mut guard = self.items.write().map_err(|_| AppError::msg("锁冲突"))?;
            guard.remove(key).is_some()
        };
        if removed {
            let _ = self.persist();
        }
        if let Some(ref kr) = self.fallback_keyring {
            let _ = kr.delete(key);
        }
        Ok(())
    }
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

    #[test]
    fn local_vault_set_get_delete_persists() {
        let path = std::env::temp_dir().join(format!("test_vault_{}.json", uuid::Uuid::new_v4()));
        {
            let v = LocalVault::without_fallback(&path);
            assert_eq!(v.get("token1").unwrap(), None);
            v.set("token1", "secret123").unwrap();
            assert_eq!(v.get("token1").unwrap().as_deref(), Some("secret123"));
        }
        // 重新从文件加载验证持久化
        {
            let v = LocalVault::without_fallback(&path);
            assert_eq!(v.get("token1").unwrap().as_deref(), Some("secret123"));
            v.delete("token1").unwrap();
            assert_eq!(v.get("token1").unwrap(), None);
        }
        let _ = std::fs::remove_file(path);
    }
}

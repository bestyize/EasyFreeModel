//! 安全：本地 API 密钥（只存哈希）、系统凭据库封装、日志脱敏。

pub mod api_keys;
pub mod oauth_store;
pub mod vault;

pub use api_keys::DbKeyVerifier;
pub use oauth_store::{VaultClientCredentials, VaultTokenStore};
pub use vault::{CredentialVault, KeyringVault, LocalVault, MemoryVault};

/// 日志/错误信息用的脱敏：只保留首尾少量字符。永远不要把完整密钥交给 tracing。
pub fn redact(secret: &str) -> String {
    let n = secret.chars().count();
    if n <= 8 {
        "***".to_string()
    } else {
        let head: String = secret.chars().take(3).collect();
        let tail: String = secret.chars().skip(n - 2).collect();
        format!("{head}…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redact_never_returns_full_secret() {
        assert_eq!(redact("short"), "***");
        let s = "sk-rd-0123456789abcdef";
        let r = redact(s);
        assert!(!r.contains("0123456789"));
        assert!(r.starts_with("sk-"));
    }
}

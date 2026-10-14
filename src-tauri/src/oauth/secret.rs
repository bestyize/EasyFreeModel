//! 不打印内容的敏感字符串包装类型。
//!
//! 令牌、授权码、refresh token、code_verifier、client secret 一律使用 `Secret`：
//! `Debug` 只输出占位符，释放时清零内存。读取明文必须显式调用 `expose()`。

use std::fmt;

use zeroize::Zeroize;

#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// 取出明文。只应在构造出站请求或写入系统凭据库时调用。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Secret(value)
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Secret(s))
    }
}

impl serde::Serialize for Secret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Secret(value.to_owned())
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_content() {
        let s = Secret::new("super-secret-value");
        assert!(!format!("{s:?}").contains("super-secret-value"));
        assert!(format!("{s:?}").contains("redacted"));
        assert_eq!(s.expose(), "super-secret-value");
    }
}

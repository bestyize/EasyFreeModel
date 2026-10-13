//! 上游鉴权方式：决定 API Key 以哪个请求头发送。

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};

use super::UpstreamError;

/// 上游鉴权方式：API Key 以哪个请求头发送。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`
    Bearer,
    /// `x-api-key: <key>`
    XApiKey,
    /// 自定义请求头名，值为原始密钥。
    Header(String),
    /// 不发送任何凭据（例如本机 Ollama）。
    None,
}

impl AuthStyle {
    pub fn needs_key(&self) -> bool {
        !matches!(self, AuthStyle::None)
    }

    /// 把密钥写入请求头。值被标记为 sensitive，hyper / reqwest 的 Debug 输出不会泄露它。
    pub fn apply(&self, headers: &mut HeaderMap, key: &str) -> Result<(), UpstreamError> {
        let bad = || UpstreamError::Config("the API key contains characters that are not allowed in an HTTP header".into());
        match self {
            AuthStyle::None => {}
            AuthStyle::Bearer => {
                let mut v = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| bad())?;
                v.set_sensitive(true);
                headers.insert(AUTHORIZATION, v);
            }
            AuthStyle::XApiKey => {
                let mut v = HeaderValue::from_str(key).map_err(|_| bad())?;
                v.set_sensitive(true);
                headers.insert(HeaderName::from_static("x-api-key"), v);
            }
            AuthStyle::Header(name) => {
                let n = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| UpstreamError::Config("invalid auth header name".into()))?;
                let mut v = HeaderValue::from_str(key).map_err(|_| bad())?;
                v.set_sensitive(true);
                headers.insert(n, v);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_marks_values_sensitive() {
        let mut h = HeaderMap::new();
        AuthStyle::Bearer.apply(&mut h, "k-123").unwrap();
        assert_eq!(h.get(AUTHORIZATION).unwrap().to_str().unwrap(), "Bearer k-123");
        assert!(h.get(AUTHORIZATION).unwrap().is_sensitive());
        assert!(!format!("{h:?}").contains("k-123"));

        let mut h = HeaderMap::new();
        AuthStyle::XApiKey.apply(&mut h, "k-123").unwrap();
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "k-123");

        let mut h = HeaderMap::new();
        AuthStyle::Header("api-key".into()).apply(&mut h, "k-123").unwrap();
        assert_eq!(h.get("api-key").unwrap().to_str().unwrap(), "k-123");

        let mut h = HeaderMap::new();
        AuthStyle::None.apply(&mut h, "ignored").unwrap();
        assert!(h.is_empty());
        assert!(AuthStyle::Bearer.apply(&mut h, "bad\nkey").is_err());
    }
}

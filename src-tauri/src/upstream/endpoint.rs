//! 上游端点：适配器类型、请求 URL、鉴权方式（已校验）。
//!
//! 数据来源见 `routing::endpoint`（内置提供商取自 `resources/provider_status.json`，自定义提供商取自
//! `providers.base_url`）。这里只负责校验与 URL 拼接。
//!
//! 安全：base URL 必须是 https；只有明确的回环地址（localhost / 127.0.0.0/8 / ::1）允许 http。

use std::net::IpAddr;

use url::Url;

use super::auth::AuthStyle;
use super::{AdapterKind, DEFAULT_ANTHROPIC_VERSION};

/// 已校验、可直接使用的端点。`base_url` 可以是 API 根（自定义提供商，适配器会追加路径），
/// 也可以已经是完整端点（内置提供商，如 `https://api.openai.com/v1/chat/completions`）。
#[derive(Debug, Clone)]
pub struct ResolvedEndpoint {
    pub kind: AdapterKind,
    pub base_url: Url,
    pub auth: AuthStyle,
    /// 覆盖默认请求路径（相对 base URL）。
    pub path: Option<String>,
    /// 流式请求时是否向 OpenAI 兼容上游发送 `stream_options.include_usage`。
    pub stream_usage: bool,
    pub anthropic_version: String,
}

impl ResolvedEndpoint {
    pub fn new(kind: AdapterKind, base_url: Url, auth: AuthStyle) -> Self {
        Self { kind, base_url, auth, path: None, stream_usage: true, anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string() }
    }
}

/// 适配器未就绪的原因（面向用户，不含密钥）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotReady(pub String);

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") || h.to_ascii_lowercase().ends_with(".localhost") {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// 校验并规整 base URL：https（回环地址可用 http）、无用户名密码、无 query / fragment。
pub fn validate_base_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "上游 base URL 不是合法的 URL".to_string())?;
    let Some(host) = url.host_str() else {
        return Err("上游 base URL 缺少主机名".into());
    };
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        "http" => return Err("上游 base URL 必须使用 https（只有 localhost / 回环地址允许 http）".into()),
        other => return Err(format!("上游 base URL 的协议不受支持：{other}")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("上游 base URL 不能包含用户名或密码".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("上游 base URL 不能包含 query 或 fragment".into());
    }
    Ok(url)
}

/// 日志 / 错误信息用的 URL 形式：只保留 `scheme://host[:port]/path`，去掉用户名密码、查询串与片段
/// （Gemini 之类的上游可能把密钥放在查询串里，我们自己不用，但也不允许它们进入日志）。
pub fn display_url(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.to_string()
}

/// 在 base URL 路径后追加 `suffix`（避免出现 `//`）。
pub fn join_path(base: &Url, suffix: &str) -> Url {
    let mut url = base.clone();
    let path = format!("{}/{}", base.path().trim_end_matches('/'), suffix.trim_start_matches('/'));
    url.set_path(&path);
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_required_except_loopback() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:11434/v1").is_ok());
        assert!(validate_base_url("http://localhost:8080").is_ok());
        assert!(validate_base_url("http://[::1]:8080").is_ok());
        assert!(validate_base_url("http://example.com/v1").is_err());
        assert!(validate_base_url("http://127.0.0.1.evil.com/v1").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
        assert!(validate_base_url("https://user:pw@example.com").is_err());
        assert!(validate_base_url("https://example.com/v1?x=1").is_err());
        assert!(validate_base_url("not a url").is_err());
    }

    #[test]
    fn display_url_drops_query_credentials_and_fragment() {
        let u = Url::parse("https://user:pw@h.example:8443/v1beta/models/m:streamGenerateContent?alt=sse&key=SECRET#frag").unwrap();
        let shown = display_url(&u);
        assert_eq!(shown, "https://h.example:8443/v1beta/models/m:streamGenerateContent");
        assert!(!shown.contains("SECRET") && !shown.contains("pw"));
    }

    #[test]
    fn join_handles_slashes() {
        let b = Url::parse("https://h.example/api/v1/").unwrap();
        assert_eq!(join_path(&b, "/chat/completions").as_str(), "https://h.example/api/v1/chat/completions");
        let b = Url::parse("https://h.example").unwrap();
        assert_eq!(join_path(&b, "v1/messages").as_str(), "https://h.example/v1/messages");
    }
}

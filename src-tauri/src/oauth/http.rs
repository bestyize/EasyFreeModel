//! 出站 HTTP：固定 User-Agent、禁止跟随重定向、仅允许 https（回环地址除外）。

use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{sanitize_code, sanitize_text, OAuthError};

const MAX_BODY_BYTES: usize = 1024 * 1024;

/// 所有 OAuth 出站请求的默认 User-Agent，固定为 `RouterDesktop/<版本>`。
/// 个别按客户端身份校验的上游可在配置里用 `userAgent` 显式覆盖（见 `request_headers`）。
pub fn user_agent() -> String {
    format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION"))
}

/// 一次出站请求要附带的额外请求头（来自配置的 `requestHeaders`；
/// 其中 `userAgent` 单独覆盖 UA）。值不参与任何凭据逻辑。
#[derive(Debug, Clone, Default)]
pub struct ExtraHeaders(pub Vec<(String, String)>);

impl ExtraHeaders {
    /// 把配置里的 requestHeaders / userAgent 转成可以直接加到请求上的头。
    pub fn from_config(headers: &std::collections::BTreeMap<String, String>, user_agent: Option<&str>) -> Self {
        let mut out: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(ua) = user_agent {
            out.push(("User-Agent".to_owned(), ua.to_owned()));
        }
        ExtraHeaders(out)
    }

    fn apply(&self, mut rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (k, v) in &self.0 {
            rb = rb.header(k.as_str(), v.as_str());
        }
        rb
    }

    /// 供错误信息脱敏使用：这些值不是凭据，无需抹除。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    #[default]
    Form,
    Json,
}

pub fn build_client(timeout: Duration) -> Result<Client, OAuthError> {
    Client::builder()
        .user_agent(user_agent())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        // 令牌端点不应重定向；跟随重定向可能把凭据带到意外的主机。
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| OAuthError::Config("failed to build HTTP client".into()))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// 只接受 https；http 仅限回环地址（本机假服务与测试）。
pub fn ensure_secure_endpoint(raw: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(raw).map_err(|_| OAuthError::Config(format!("invalid URL: {}", sanitize_text(raw, &[]))))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if url.host_str().is_some_and(is_loopback_host) => Ok(url),
        _ => Err(OAuthError::Config(format!(
            "endpoint must use https: {}",
            sanitize_text(raw, &[])
        ))),
    }
}

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    /// 响应体按 JSON 解析的结果；非 JSON 时为 None。永不直接进入日志或错误信息。
    pub body: Option<Value>,
}

fn map_reqwest(err: reqwest::Error) -> OAuthError {
    // 不使用 err 的 Display：其中会带 URL，且内容不可控。
    let kind = if err.is_timeout() {
        "request timed out"
    } else if err.is_connect() {
        "connection failed"
    } else {
        "network error"
    };
    OAuthError::Transient(kind.to_owned())
}

pub fn form_encode(fields: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish()
}

pub async fn post(
    client: &Client,
    url: &str,
    encoding: Encoding,
    fields: &[(String, String)],
) -> Result<Reply, OAuthError> {
    post_with(client, url, encoding, fields, &ExtraHeaders::default()).await
}

/// 同 [`post`]，但附带额外请求头（个别供应商的私有协议需要）。
pub async fn post_with(
    client: &Client,
    url: &str,
    encoding: Encoding,
    fields: &[(String, String)],
    headers: &ExtraHeaders,
) -> Result<Reply, OAuthError> {
    let url = ensure_secure_endpoint(url)?;
    let request = headers.apply(client.post(url).header(ACCEPT, "application/json"));
    let request = match encoding {
        Encoding::Form => request
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_encode(fields)),
        Encoding::Json => {
            let map: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            request.json(&map)
        }
    };
    finish(request).await
}

/// JSON POST with typed values, for provider-specific token exchange endpoints.
pub async fn post_json_with(
    client: &Client,
    url: &str,
    body: &Value,
    headers: &ExtraHeaders,
) -> Result<Reply, OAuthError> {
    let url = ensure_secure_endpoint(url)?;
    let request = headers.apply(client.post(url).header(ACCEPT, "application/json")).json(body);
    finish(request).await
}

/// GET 请求（CodeBuddy 的令牌轮询以 GET + 查询串进行）。
pub async fn get_with(client: &Client, url: &str, headers: &ExtraHeaders) -> Result<Reply, OAuthError> {
    let url = ensure_secure_endpoint(url)?;
    finish(headers.apply(client.get(url).header(ACCEPT, "application/json"))).await
}

async fn finish(request: reqwest::RequestBuilder) -> Result<Reply, OAuthError> {
    let response = request.send().await.map_err(map_reqwest)?;
    let status = response.status().as_u16();
    if response.content_length().is_some_and(|n| n as usize > MAX_BODY_BYTES) {
        return Err(OAuthError::InvalidResponse("response too large".into()));
    }
    let bytes = response.bytes().await.map_err(map_reqwest)?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err(OAuthError::InvalidResponse("response too large".into()));
    }
    Ok(Reply { status, body: serde_json::from_slice(&bytes).ok() })
}

/// 从错误响应里取出 (error 码, 说明)。两者都已脱敏，可进入错误值。
pub fn error_fields(body: Option<&Value>, secrets: &[&str]) -> (String, Option<String>) {
    let Some(body) = body else {
        return ("unknown".to_owned(), None);
    };
    let (code, description) = match body.get("error") {
        Some(Value::String(code)) => (
            code.as_str(),
            body.get("error_description").and_then(Value::as_str),
        ),
        // 形如 {"error": {"type": "invalid_grant", "message": "..."}}
        Some(Value::Object(obj)) => (
            ["type", "code", "error"]
                .iter()
                .find_map(|k| obj.get(*k).and_then(Value::as_str))
                .unwrap_or("unknown"),
            obj.get("message").and_then(Value::as_str),
        ),
        _ => ("unknown", body.get("message").and_then(Value::as_str)),
    };
    (
        sanitize_code(code),
        description.map(|d| sanitize_text(d, secrets)).filter(|d| !d.is_empty()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_agent_is_fixed_format() {
        let ua = user_agent();
        assert!(ua.starts_with("RouterDesktop/"));
        assert!(!ua.to_lowercase().contains("claude"));
    }

    #[test]
    fn endpoint_must_be_https_or_loopback() {
        assert!(ensure_secure_endpoint("https://auth.example.com/token").is_ok());
        assert!(ensure_secure_endpoint("http://127.0.0.1:9/token").is_ok());
        assert!(ensure_secure_endpoint("http://localhost:9/token").is_ok());
        assert!(ensure_secure_endpoint("http://example.com/token").is_err());
        assert!(ensure_secure_endpoint("ftp://example.com/").is_err());
        assert!(ensure_secure_endpoint("not a url").is_err());
    }

    #[test]
    fn error_fields_handle_both_shapes_and_scrub() {
        let flat = json!({"error": "invalid_grant", "error_description": "bad token SECRET99 here"});
        let (code, desc) = error_fields(Some(&flat), &["SECRET99"]);
        assert_eq!(code, "invalid_grant");
        assert_eq!(desc.as_deref(), Some("bad token [redacted] here"));

        let nested = json!({"error": {"type": "invalid_request_error", "message": "oops"}});
        let (code, desc) = error_fields(Some(&nested), &[]);
        assert_eq!(code, "invalid_request_error");
        assert_eq!(desc.as_deref(), Some("oops"));

        assert_eq!(error_fields(None, &[]).0, "unknown");
    }

    #[test]
    fn form_encoding_escapes() {
        let s = form_encode(&[("a".into(), "b c&d".into())]);
        assert_eq!(s, "a=b+c%26d");
    }
}

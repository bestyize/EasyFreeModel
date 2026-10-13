//! OAuth 错误类型。
//!
//! 约束：任何变体的 `Display`/`Debug` 都不得包含令牌、授权码、code_verifier、
//! client secret 或服务端返回的原始响应体。服务端给出的 `error` 码会被限制为
//! 安全字符集，`description` 只保存在字段中（已脱敏、截断），不进入 `Display`。

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OAuthError {
    #[error("unknown OAuth provider: {0}")]
    UnknownProvider(String),

    /// 该供应商是 `flow: "custom"`，本框架不实现它的登录流程。
    #[error("provider {provider} uses a custom sign-in flow that this framework does not implement")]
    CustomFlowRequired { provider: String, reason: String },

    /// 对供应商调用了与其流程类型不符的入口（例如对 device 供应商调用 start_authorization）。
    #[error("provider {provider} does not use the {expected} flow")]
    UnsupportedFlow { provider: String, expected: &'static str },

    /// 缺少用户需要自行提供的客户端凭据。界面据此显示「需要配置客户端凭据」。
    #[error("client credential required: provider {provider} is missing {field}")]
    MissingClientCredential { provider: String, field: &'static str },

    #[error("invalid OAuth configuration: {0}")]
    Config(String),

    #[error("authorization callback state mismatch; authorization rejected")]
    StateMismatch,

    #[error("authorization was denied")]
    AccessDenied,

    #[error("authorization callback reported an error: {code}")]
    CallbackError { code: String, description: Option<String> },

    #[error("authorization callback did not contain an authorization code")]
    MissingCode,

    #[error("timed out waiting for authorization")]
    Timeout,

    #[error("authorization was cancelled")]
    Cancelled,

    #[error("device code expired before authorization completed")]
    DeviceCodeExpired,

    #[error("cannot listen on 127.0.0.1:{port}: {message}")]
    ListenFailed { port: u16, message: String },

    /// 令牌端点明确拒绝（不可重试，也不是「需重新授权」类）。
    #[error("token endpoint rejected the request (HTTP {status}): {code}")]
    TokenEndpoint {
        status: u16,
        code: String,
        description: Option<String>,
    },

    /// refresh token 失效、被撤销，或根本没有可用的 refresh token：用户需重新授权。
    #[error("re-authorization required for {provider}: {reason}")]
    ReauthRequired { provider: String, reason: String },

    /// 网络错误、超时、429、5xx：可以稍后重试，不要清除凭据。
    #[error("temporary failure, retry later: {0}")]
    Transient(String),

    #[error("invalid response from authorization server: {0}")]
    InvalidResponse(String),

    #[error("no stored tokens for {0}")]
    NoTokens(String),

    #[error("token storage error: {0}")]
    Storage(String),
}

impl OAuthError {
    /// 稳定的机器可读错误码，供界面映射文案。
    pub fn code(&self) -> &'static str {
        match self {
            OAuthError::UnknownProvider(_) => "unknown_provider",
            OAuthError::CustomFlowRequired { .. } => "custom_flow_required",
            OAuthError::UnsupportedFlow { .. } => "unsupported_flow",
            OAuthError::MissingClientCredential { .. } => "missing_client_credential",
            OAuthError::Config(_) => "config",
            OAuthError::StateMismatch => "state_mismatch",
            OAuthError::AccessDenied => "access_denied",
            OAuthError::CallbackError { .. } => "callback_error",
            OAuthError::MissingCode => "missing_code",
            OAuthError::Timeout => "timeout",
            OAuthError::Cancelled => "cancelled",
            OAuthError::DeviceCodeExpired => "device_code_expired",
            OAuthError::ListenFailed { .. } => "listen_failed",
            OAuthError::TokenEndpoint { .. } => "token_endpoint",
            OAuthError::ReauthRequired { .. } => "reauth_required",
            OAuthError::Transient(_) => "transient",
            OAuthError::InvalidResponse(_) => "invalid_response",
            OAuthError::NoTokens(_) => "no_tokens",
            OAuthError::Storage(_) => "storage",
        }
    }

    /// 是否值得稍后重试（网络 / 5xx / 429）。
    pub fn is_retryable(&self) -> bool {
        matches!(self, OAuthError::Transient(_))
    }

    /// 是否需要用户重新走授权流程。
    pub fn requires_reauth(&self) -> bool {
        matches!(self, OAuthError::ReauthRequired { .. } | OAuthError::NoTokens(_))
    }
}

const MAX_TEXT: usize = 200;
const MAX_CODE: usize = 64;

/// 把服务端返回的 `error` 码限制为安全字符集。
pub(crate) fn sanitize_code(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
        .take(MAX_CODE)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// 清洗服务端给出的说明文字：去控制字符、抹掉已知机密、截断。
pub(crate) fn sanitize_text(raw: &str, secrets: &[&str]) -> String {
    let mut text: String = raw.chars().filter(|c| !c.is_control()).collect();
    for secret in secrets {
        if secret.len() >= 4 {
            text = text.replace(secret, "[redacted]");
        }
    }
    text.chars().take(MAX_TEXT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizers_limit_and_scrub() {
        assert_eq!(sanitize_code("invalid_grant\n<script>"), "invalid_grantscript");
        assert_eq!(sanitize_code("!!!"), "unknown");
        let out = sanitize_text("token abc123XYZ was bad\r\n", &["abc123XYZ"]);
        assert_eq!(out, "token [redacted] was bad");
        assert_eq!(sanitize_text(&"a".repeat(500), &[]).chars().count(), MAX_TEXT);
    }

    #[test]
    fn classification_helpers() {
        assert!(OAuthError::Transient("x".into()).is_retryable());
        assert!(!OAuthError::Timeout.is_retryable());
        assert!(OAuthError::ReauthRequired { provider: "p".into(), reason: "r".into() }.requires_reauth());
        assert_eq!(OAuthError::StateMismatch.code(), "state_mismatch");
    }
}

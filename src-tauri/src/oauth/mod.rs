//! OAuth 授权框架。
//!
//! - `config`：数据驱动的供应商授权配置（`resources/oauth_providers.json`）与客户端凭据抽象
//! - `pkce`：PKCE S256 与 state
//! - `loopback` / `flow`：authorization-code + 127.0.0.1 回环回调
//! - `device`：RFC 8628 设备码
//! - `token` / `refresh` / `service`：令牌模型、`TokenStore`、刷新与并发去重
//! - `hub`：应用层——授权会话、连接创建 / 重新授权 / 断开、客户端凭据表单后端、供应商状态列表
//! - `exchange`：github / muse 授权后令牌兑换步骤的接入状态（未接入，如实报告）
//!
//! 约束：令牌、授权码、refresh token、code_verifier 只存在于 `Secret` 中，
//! 不进日志、错误信息或 Debug；出站 User-Agent 固定为 `RouterDesktop/<版本>`；
//! 授权必须校验 state；监听只绑定 127.0.0.1。

pub mod config;
pub mod device;
pub mod error;
pub mod exchange;
pub mod flow;
pub mod http;
pub mod hub;
pub mod loopback;
pub mod pkce;
pub mod refresh;
pub mod secret;
pub mod service;
pub mod state_poll;
pub mod token;

#[cfg(test)]
mod hub_tests;
#[cfg(test)]
pub(crate) mod testkit;

pub use config::{
    ClientCredentialAdmin, ClientCredentialSource, ClientCredentials, FlowKind, NoClientCredentials, OAuthCatalog, OAuthProviderConfig,
    StaticClientCredentials,
};
pub use device::{DeviceCodeInfo, DeviceSession};
pub use error::OAuthError;
pub use flow::{AuthSession, AuthSessionInfo};
pub use hub::{HubError, HubLimits, OAuthHub, PollResult, ProviderInfo, StartResult};
pub use secret::Secret;
pub use service::{CancelHandle, OAuthService, OAuthTuning};
pub use token::{MemoryTokenStore, TokenSet, TokenStore};

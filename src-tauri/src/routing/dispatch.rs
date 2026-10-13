//! 并发限制、逐账号尝试、失败分类与回退。
//!
//! 回退只发生在「尚未向客户端写出任何字节」之前：调用方传入的 `op` 负责完成一次完整的尝试
//! （对流式请求包括读到上游的第一个数据帧），一旦 `op` 返回 `Attempt::Ok`，本模块不再重试。
//!
//! 失败分类：
//! - 401 / 402 / 403：账号级。该账号硬冷却，库里状态标为 `error`，换下一个账号。
//! - 429：该账号按 `Retry-After` 冷却，换下一个账号。
//! - 408 / 5xx / 网络错误 / 超时 / 上游响应畸形：瞬时故障。账号软降级，换下一个账号（账号用尽后可回头重试）。
//! - 其它 4xx（400、404、413、422…）：请求本身的问题，换账号没有意义——原样返回给客户端，不影响账号健康。
//! - 3xx：base URL 配错了（出站不跟随重定向）——如实返回，不重试。
//!
//! OAuth 连接（kind=oauth）：构造上游鉴权前先取令牌（`OAuthService::get_valid_token`，按 refreshLeadSeconds 提前刷新、
//! 同账户去重）。上游返回 401 时对该账号强制刷新一次并重试同一请求一次（不占用尝试次数）；令牌需要重新授权
//! （`requires_reauth`）→ 连接标记为 `needsReauth` 并换下一个账号；令牌刷新的瞬时故障（`is_retryable`）→ 保留凭据，
//! 按瞬时错误回退。github / muse 授权后的令牌兑换步骤尚未接入：如实报告，绝不把账户令牌当模型 Key 发出。
//!
//! 每个请求最多尝试 `maxRetries + 1` 次。

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::OwnedSemaphorePermit;

use super::health::Availability;
use super::repo::{self, ConnCfg, ProviderCfg};
use super::{Runtime, TargetError};
use crate::upstream::{ResolvedEndpoint, UpstreamError, UpstreamReply, UpstreamTarget};

/// 秘密值列表（用于擦除上游错误信息里回显的密钥）。Debug 脱敏。
#[derive(Clone, Default)]
pub struct SecretList(pub Vec<String>);

impl std::fmt::Debug for SecretList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretList(<{} redacted>)", self.0.len())
    }
}

/// 一次尝试的结果。
pub enum Attempt<T> {
    Ok(T),
    /// 上游给出了非 2xx（或在 2xx 的首个流帧里给出了错误）。
    Rejected(UpstreamReply),
    /// 传输层失败，或流在第一个数据帧之前就断了。
    Failed(UpstreamError),
}

pub struct Dispatched<T> {
    pub value: T,
    pub connection: ConnCfg,
    /// 持有至响应结束（流式时随响应体一起释放），保证 maxConcurrency 真正限制在途请求。
    pub permit: OwnedSemaphorePermit,
    /// 该账号的秘密值，用于擦除后续流内错误信息。
    pub secrets: SecretList,
    pub attempts: u32,
}

#[derive(Debug)]
pub enum DispatchError {
    /// 等待并发名额超时。
    Busy,
    /// 没有任何可尝试的账号。
    NoConnection(NoConnection),
    /// 最后一次尝试得到了上游的非 2xx 响应；应如实返回给客户端。
    Upstream { reply: UpstreamReply, secrets: SecretList },
    /// 最后一次尝试是传输层失败。
    Transport(UpstreamError),
    /// 本地问题（凭据库不可用等），文案不含密钥。
    Local(String),
    /// 该提供商授权后的令牌兑换步骤尚未接入（github / muse）。文案固定。
    NotImplemented(String),
    /// OAuth 令牌暂时无法刷新（瞬时故障），稍后重试。文案固定。
    TokenUnavailable(String),
}

#[derive(Debug, Default, Clone)]
pub struct NoConnection {
    pub total: usize,
    pub inactive: usize,
    pub cooling: usize,
    pub shortest_cooldown: Option<Duration>,
    pub missing_credential: usize,
    /// 因 OAuth 令牌失效（needsReauth）而无法使用的连接数（含本次请求中刚被标记的）。
    pub needs_reauth: usize,
}

#[derive(Debug)]
pub struct DispatchFailure {
    pub error: DispatchError,
    /// 最后一次尝试所用的连接（用于用量记录）。
    pub connection_id: Option<String>,
    pub attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// 账号级鉴权 / 额度问题。
    Auth,
    RateLimited,
    Transient,
    /// 请求本身的问题，原样返回。
    Fatal,
}

fn classify(status: u16) -> Class {
    match status {
        401..=403 => Class::Auth,
        429 => Class::RateLimited,
        408 | 500..=599 => Class::Transient,
        _ => Class::Fatal,
    }
}

const RECYCLE_BACKOFF: Duration = Duration::from_millis(150);

/// 令牌刷新遇到瞬时故障时给客户端的固定说明。
pub const TOKEN_UNAVAILABLE: &str = "The OAuth token for this connection could not be refreshed right now; retry shortly.";

impl Runtime {
    /// 令牌已失效：把连接标记为 `needsReauth`（失败只记录，不影响本次请求换账号）。
    async fn mark_needs_reauth(&self, conn: &ConnCfg) {
        tracing::warn!(connection = %conn.id, provider = %conn.provider_id, "OAuth token is no longer valid; marking the connection as needing re-authorization");
        if let Err(e) = repo::set_connection_needs_reauth(&self.db, &conn.id).await {
            tracing::warn!(connection = %conn.id, "failed to mark connection as needing re-authorization: {e}");
        }
    }

    /// 执行一次带回退的调度。`op` 在每个候选账号上被调用一次（必要时多次）。
    pub async fn dispatch<T, F, Fut>(
        self: &Arc<Self>,
        provider: &ProviderCfg,
        endpoint: &ResolvedEndpoint,
        mut op: F,
    ) -> Result<Dispatched<T>, Box<DispatchFailure>>
    where
        F: FnMut(UpstreamTarget) -> Fut,
        Fut: Future<Output = Attempt<T>>,
    {
        let fail = |error: DispatchError, connection_id: Option<String>, attempts: u32| Box::new(DispatchFailure { error, connection_id, attempts });

        let conns = repo::list_connections(&self.db, &provider.id)
            .await
            .map_err(|_| fail(DispatchError::Local("failed to read the provider's connections".into()), None, 0))?;
        let headers = repo::list_headers(&self.db, &provider.id)
            .await
            .map_err(|_| fail(DispatchError::Local("failed to read the provider's custom headers".into()), None, 0))?;
        let total = conns.len();
        let plan = self.scheduler.plan(provider, conns, &self.health);
        let mut no_conn = NoConnection {
            total,
            inactive: plan.inactive,
            cooling: plan.cooling,
            shortest_cooldown: plan.shortest_cooldown,
            missing_credential: 0,
            needs_reauth: plan.needs_reauth,
        };
        if plan.order.is_empty() {
            return Err(fail(DispatchError::NoConnection(no_conn), None, 0));
        }

        let timeout = self.request_timeout().await;
        let sem = self.semaphore(&provider.id, provider.max_concurrency.max(1) as usize);
        let permit = match tokio::time::timeout(timeout, sem.acquire_owned()).await {
            Ok(Ok(p)) => p,
            _ => return Err(fail(DispatchError::Busy, None, 0)),
        };

        let max_attempts = provider.max_retries.max(0) as u32 + 1;
        let mut queue: VecDeque<ConnCfg> = plan.order.into();
        let mut soft_failed: Vec<ConnCfg> = Vec::new();
        let mut attempts = 0u32;
        let mut last: Option<Box<DispatchFailure>> = None;

        'accounts: while attempts < max_attempts {
            let conn = match queue.pop_front() {
                Some(c) => c,
                None if !soft_failed.is_empty() => {
                    queue.extend(soft_failed.drain(..));
                    tokio::time::sleep(RECYCLE_BACKOFF * attempts.max(1)).await;
                    continue;
                }
                None => break,
            };
            // 并发请求可能刚把它冷却了
            if matches!(self.health.availability(&conn.id), Availability::Cooling(_)) {
                continue;
            }
            let mut target = match self.build_target(provider, endpoint, &conn, &headers, timeout).await {
                Ok(t) => t,
                Err(TargetError::MissingCredential) => {
                    no_conn.missing_credential += 1;
                    continue;
                }
                Err(TargetError::ReauthRequired) => {
                    self.mark_needs_reauth(&conn).await;
                    no_conn.needs_reauth += 1;
                    continue;
                }
                Err(TargetError::ExchangePending(note)) => {
                    last = Some(fail(DispatchError::NotImplemented(note.to_string()), Some(conn.id.clone()), attempts));
                    continue;
                }
                Err(TargetError::TokenTransient) => {
                    // 瞬时故障：保留凭据，按瞬时错误回退（计一次尝试，避免无限回收）
                    attempts += 1;
                    self.health.record_transient(&conn.id);
                    soft_failed.push(conn.clone());
                    last = Some(fail(DispatchError::TokenUnavailable(TOKEN_UNAVAILABLE.into()), Some(conn.id.clone()), attempts));
                    continue;
                }
                Err(e) => {
                    last = Some(fail(DispatchError::Local(e.to_string()), Some(conn.id.clone()), attempts));
                    continue;
                }
            };
            let mut secrets = SecretList(target.secrets());
            attempts += 1;
            tracing::debug!(provider = %provider.id, connection = %conn.id, attempt = attempts, "dispatching upstream attempt");

            let mut refreshed = false;
            let outcome = 'call: loop {
                let out = op(target.clone()).await;
                // OAuth 账号收到 401：强制刷新一次令牌，用新令牌重试同一请求一次
                if conn.kind == "oauth" && !refreshed && matches!(&out, Attempt::Rejected(r) if r.status == 401) {
                    refreshed = true;
                    match self.build_target_with(provider, endpoint, &conn, &headers, timeout, true).await {
                        Ok(fresh) => {
                            tracing::info!(provider = %provider.id, connection = %conn.id, "401 from upstream; retrying once with a refreshed OAuth token");
                            secrets = SecretList(fresh.secrets());
                            target = fresh;
                            continue 'call;
                        }
                        Err(TargetError::ReauthRequired) => {
                            self.mark_needs_reauth(&conn).await;
                            no_conn.needs_reauth += 1;
                            continue 'accounts;
                        }
                        Err(TargetError::TokenTransient) => {
                            self.health.record_transient(&conn.id);
                            soft_failed.push(conn.clone());
                            last = Some(fail(DispatchError::TokenUnavailable(TOKEN_UNAVAILABLE.into()), Some(conn.id.clone()), attempts));
                            continue 'accounts;
                        }
                        // 刷新本身出了别的问题：沿用上游原始的 401 结论
                        Err(_) => {}
                    }
                }
                break out;
            };

            match outcome {
                Attempt::Ok(value) => {
                    self.health.record_success(&conn.id);
                    if conn.status == "error" {
                        let _ = repo::set_connection_error(&self.db, &conn.id, false).await;
                    }
                    return Ok(Dispatched { value, connection: conn, permit, secrets, attempts });
                }
                Attempt::Rejected(reply) => {
                    let class = classify(reply.status);
                    tracing::warn!(provider = %provider.id, connection = %conn.id, status = reply.status, class = ?class, "upstream rejected the request");
                    match class {
                        Class::Auth => {
                            self.health.record_auth_failure(&conn.id);
                            let _ = repo::set_connection_error(&self.db, &conn.id, true).await;
                        }
                        Class::RateLimited => {
                            let cd = self.health.record_rate_limited(&conn.id, reply.retry_after);
                            tracing::info!(connection = %conn.id, cooldown_secs = cd.as_secs(), "connection cooled down after 429");
                        }
                        Class::Transient => {
                            self.health.record_transient(&conn.id);
                            soft_failed.push(conn.clone());
                        }
                        Class::Fatal => {
                            return Err(fail(DispatchError::Upstream { reply, secrets }, Some(conn.id), attempts));
                        }
                    }
                    last = Some(fail(DispatchError::Upstream { reply, secrets }, Some(conn.id.clone()), attempts));
                }
                Attempt::Failed(err) => {
                    tracing::warn!(provider = %provider.id, connection = %conn.id, error = %err, "upstream attempt failed");
                    if matches!(err, UpstreamError::Config(_)) {
                        return Err(fail(DispatchError::Local(err.to_string()), Some(conn.id), attempts));
                    }
                    self.health.record_transient(&conn.id);
                    soft_failed.push(conn.clone());
                    last = Some(fail(DispatchError::Transport(err), Some(conn.id.clone()), attempts));
                }
            }
        }

        Err(last.unwrap_or_else(|| fail(DispatchError::NoConnection(no_conn), None, attempts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification() {
        for s in [401, 402, 403] {
            assert_eq!(classify(s), Class::Auth);
        }
        assert_eq!(classify(429), Class::RateLimited);
        for s in [408, 500, 502, 503, 529] {
            assert_eq!(classify(s), Class::Transient, "{s}");
        }
        for s in [300, 302, 400, 404, 413, 422] {
            assert_eq!(classify(s), Class::Fatal, "{s}");
        }
    }
}

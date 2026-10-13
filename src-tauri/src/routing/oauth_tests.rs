//! OAuth 连接在转发管线里的行为：取令牌（含提前刷新）、401 强制刷新并重试一次、需重新授权 → 标记并换账号、
//! 瞬时故障 → 保留凭据并回退、github / muse 的令牌兑换步骤未接入。
//!
//! 直接驱动 `Runtime::dispatch`（不经 HTTP 网关）：`op` 记录每次尝试用的访问令牌；令牌刷新走真实 HTTP 的假授权服务器。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};

use super::dispatch::{Attempt, DispatchError};
use super::repo::{self, ProviderCfg};
use super::{endpoint, Runtime};
use crate::oauth::exchange::TOKEN_EXCHANGE_PENDING;
use crate::oauth::testkit::*;
use crate::oauth::{Secret, TokenSet, TokenStore};
use crate::security::{CredentialVault, VaultTokenStore};
use crate::upstream::{ResolvedEndpoint, UpstreamClient, UpstreamReply};

struct Rt {
    env: Env,
    rt: Arc<Runtime>,
    provider: ProviderCfg,
    endpoint: ResolvedEndpoint,
    tokens: VaultTokenStore,
}

/// 把测试提供商 `id` 配成「适配器就绪的 OpenAI 兼容自定义提供商」（上游地址不会被真正访问：op 是假的）。
async fn setup(id: &str, with_oauth: bool) -> Rt {
    let env = env().await;
    sqlx::query(
        "UPDATE providers SET kind = 'custom', grp = 'custom', adapter = 'openai-compatible', adapter_ready = 1, \
                base_url = 'http://127.0.0.1:9/v1', round_robin = 0, max_retries = 1 WHERE id = ?1",
    )
    .bind(id)
    .execute(&env.db)
    .await
    .unwrap();
    let provider = repo::get_provider(&env.db, id).await.unwrap().unwrap();
    let endpoint = endpoint::endpoint_for(&provider).unwrap();
    let vault: Arc<dyn CredentialVault> = env.vault.clone();
    let mut rt = Runtime::with_client(env.db.clone(), vault.clone(), UpstreamClient::new().unwrap());
    if with_oauth {
        rt = rt.with_oauth(env.service.clone());
    }
    Rt { tokens: VaultTokenStore::new(vault), rt: Arc::new(rt), provider, endpoint, env }
}

impl Rt {
    /// 写入一个令牌条目并创建指向它的 OAuth 连接。`expires_in_secs`：距到期的秒数。
    async fn add_oauth_connection(&self, id: &str, priority: i64, access: &str, refresh: &str, expires_in_secs: i64) {
        let cref = format!("connection/{id}/ref");
        let tokens = TokenSet {
            access_token: Secret::new(access),
            refresh_token: Some(Secret::new(refresh)),
            expires_at: Some(Utc::now() + ChronoDuration::seconds(expires_in_secs)),
            scopes: vec![],
            extra: Default::default(),
        };
        self.tokens.save(&cref, &tokens).await.unwrap();
        sqlx::query(
            "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at) \
             VALUES (?1, ?2, ?1, 'oauth', 'active', ?3, 1, 1, ?4, 't', 't')",
        )
        .bind(id)
        .bind(&self.provider.id)
        .bind(priority)
        .bind(&cref)
        .execute(&self.env.db)
        .await
        .unwrap();
    }

    async fn status_of(&self, id: &str) -> String {
        repo::list_connections(&self.env.db, &self.provider.id)
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.id == id)
            .unwrap()
            .status
    }

    async fn stored_access(&self, id: &str) -> String {
        self.tokens.load(&format!("connection/{id}/ref")).await.unwrap().unwrap().access_token.expose().to_owned()
    }

    async fn stored_refresh(&self, id: &str) -> String {
        self.tokens.load(&format!("connection/{id}/ref")).await.unwrap().unwrap().refresh_token.unwrap().expose().to_owned()
    }

    /// 以每次尝试用的 (连接, 访问令牌) 记录；`respond` 决定每次尝试的结果。
    async fn run(
        &self,
        calls: &Arc<Mutex<Vec<(String, String)>>>,
        respond: impl Fn(usize, &str) -> Option<u16> + Send + Sync + 'static,
    ) -> Result<(String, String), Box<super::dispatch::DispatchFailure>> {
        let respond = Arc::new(respond);
        let calls = calls.clone();
        self.rt
            .dispatch(&self.provider, &self.endpoint, move |target| {
                let (calls, respond) = (calls.clone(), respond.clone());
                async move {
                    let token = target.api_key.as_ref().map(|k| k.expose().to_owned()).unwrap_or_default();
                    let n = {
                        let mut g = calls.lock().unwrap();
                        g.push((target.connection_id.clone(), token.clone()));
                        g.len()
                    };
                    match respond(n, &token) {
                        None => Attempt::Ok((target.connection_id.clone(), token)),
                        Some(status) => Attempt::Rejected(UpstreamReply {
                            status,
                            retry_after: None,
                            content_type: None,
                            body: Bytes::from_static(b"{}"),
                        }),
                    }
                }
            })
            .await
            .map(|d| d.value)
    }
}

fn new_calls() -> Arc<Mutex<Vec<(String, String)>>> {
    Arc::new(Mutex::new(Vec::new()))
}

#[tokio::test]
async fn a_valid_token_is_used_as_is_without_refreshing() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-A", "REFRESH-A", 3600).await;
    let calls = new_calls();
    let (conn, token) = t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!((conn.as_str(), token.as_str()), ("a", "ACCESS-A"));
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 0);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_token_inside_the_refresh_window_is_refreshed_and_written_back_before_use() {
    let t = setup("t-code", true).await;
    // refreshLeadSeconds = 300：60 秒后到期 -> 先刷新
    t.add_oauth_connection("a", 1, "ACCESS-OLD", "REFRESH-A", 60).await;
    let calls = new_calls();
    let (_, token) = t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!(token, "ACCESS-REFRESH-1", "the refreshed token is what goes upstream");
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 1);
    // 新令牌与轮换后的 refresh token 已写回凭据库
    assert_eq!(t.stored_access("a").await, "ACCESS-REFRESH-1");
    assert_eq!(t.stored_refresh("a").await, "REFRESH-ROT-1");
    // 下一个请求用已刷新的令牌，不再刷新
    t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_upstream_401_forces_one_refresh_and_retries_the_same_request_once() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-A", "REFRESH-A", 3600).await;
    let calls = new_calls();
    // 第一次（旧令牌）401，第二次（刷新后的令牌）成功
    let (conn, token) = t.run(&calls, |n, _| (n == 1).then_some(401)).await.unwrap();
    assert_eq!((conn.as_str(), token.as_str()), ("a", "ACCESS-REFRESH-1"));
    let seen: Vec<String> = calls.lock().unwrap().iter().map(|(_, tok)| tok.clone()).collect();
    assert_eq!(seen, vec!["ACCESS-A", "ACCESS-REFRESH-1"]);
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 1, "exactly one forced refresh");
    assert_eq!(t.status_of("a").await, "active", "a recovered 401 does not mark the account as failing");
}

#[tokio::test]
async fn a_second_401_is_final_and_does_not_loop() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-A", "REFRESH-A", 3600).await;
    let calls = new_calls();
    let failure = t.run(&calls, |_, _| Some(401)).await.unwrap_err();
    assert!(matches!(failure.error, DispatchError::Upstream { ref reply, .. } if reply.status == 401), "{:?}", failure.error);
    assert_eq!(calls.lock().unwrap().len(), 2, "original attempt + one retry, never more");
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 1);
    // 刷新后的令牌仍被拒绝：按既有规则标记账号异常（不是需要重新授权）
    assert_eq!(t.status_of("a").await, "error");
}

#[tokio::test]
async fn api_key_connections_never_trigger_a_refresh_on_401() {
    let t = setup("t-code", true).await;
    sqlx::query(
        "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at) \
         VALUES ('k', 't-code', 'k', 'apiKey', 'active', 1, 1, 1, 'connection/k/ref', 't', 't')",
    )
    .execute(&t.env.db)
    .await
    .unwrap();
    t.env.vault.set("connection/k/ref", "sk-plain-key").unwrap();
    let calls = new_calls();
    let failure = t.run(&calls, |_, _| Some(401)).await.unwrap_err();
    assert!(matches!(failure.error, DispatchError::Upstream { .. }));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap()[0].1, "sk-plain-key");
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reauth_required_marks_the_connection_and_the_request_moves_to_the_next_account() {
    let t = setup("t-code", true).await;
    // a 的令牌已过期且 refresh token 被撤销；b 正常
    t.add_oauth_connection("a", 1, "ACCESS-DEAD", "REFRESH-REVOKED", -10).await;
    t.add_oauth_connection("b", 2, "ACCESS-B", "REFRESH-B", 3600).await;
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::InvalidGrant;

    let calls = new_calls();
    let (conn, token) = t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!((conn.as_str(), token.as_str()), ("b", "ACCESS-B"));
    assert_eq!(calls.lock().unwrap().len(), 1, "the dead account was never sent upstream");
    assert_eq!(t.status_of("a").await, "needsReauth");
    assert_eq!(t.status_of("b").await, "active");
    assert_eq!(t.stored_refresh("a").await, "REFRESH-REVOKED", "the stored entry is left for the re-authorization flow");

    // 之后的请求直接跳过它，不再打令牌端点
    let before = t.env.fake.refresh_count.load(Ordering::SeqCst);
    t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn when_every_account_needs_reauth_the_failure_says_so() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-DEAD", "REFRESH-REVOKED", -10).await;
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::InvalidGrant;
    let calls = new_calls();
    let failure = t.run(&calls, |_, _| None).await.unwrap_err();
    match failure.error {
        DispatchError::NoConnection(n) => assert_eq!(n.needs_reauth, 1),
        other => panic!("unexpected {other:?}"),
    }
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(t.status_of("a").await, "needsReauth");
    // 此时调度器一开始就把它排除了：同样报告需要重新授权
    let failure = t.run(&calls, |_, _| None).await.unwrap_err();
    assert!(matches!(failure.error, DispatchError::NoConnection(ref n) if n.needs_reauth == 1 && n.total == 1));
}

#[tokio::test]
async fn a_401_whose_refresh_is_rejected_marks_needs_reauth_and_moves_on() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-A", "REFRESH-REVOKED", 3600).await;
    t.add_oauth_connection("b", 2, "ACCESS-B", "REFRESH-B", 3600).await;
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::InvalidGrant;
    let calls = new_calls();
    let (conn, token) = t.run(&calls, |_, tok| (tok == "ACCESS-A").then_some(401)).await.unwrap();
    assert_eq!((conn.as_str(), token.as_str()), ("b", "ACCESS-B"));
    assert_eq!(t.status_of("a").await, "needsReauth");
    assert_eq!(t.status_of("b").await, "active");
}

#[tokio::test]
async fn a_transient_refresh_failure_keeps_the_credentials_and_falls_back() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-OLD", "REFRESH-A", -10).await;
    t.add_oauth_connection("b", 2, "ACCESS-B", "REFRESH-B", 3600).await;
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::ServerError;

    let calls = new_calls();
    let (conn, _) = t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!(conn, "b", "falls back to the next account like any transient error");
    assert_eq!(t.status_of("a").await, "active", "a transient failure never marks the account");
    assert_eq!(t.stored_refresh("a").await, "REFRESH-A", "credentials are kept");
    assert_eq!(t.stored_access("a").await, "ACCESS-OLD");
}

#[tokio::test]
async fn a_lone_account_with_a_transient_refresh_failure_ends_with_a_503_style_error_and_terminates() {
    let t = setup("t-code", true).await;
    t.add_oauth_connection("a", 1, "ACCESS-OLD", "REFRESH-A", -10).await;
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::ServerError;
    let calls = new_calls();
    let failure = t.run(&calls, |_, _| None).await.unwrap_err();
    assert!(matches!(failure.error, DispatchError::TokenUnavailable(_)), "{:?}", failure.error);
    assert!(calls.lock().unwrap().is_empty(), "no request is ever sent with an expired token");
    assert_eq!(t.status_of("a").await, "active");
    assert_eq!(t.stored_refresh("a").await, "REFRESH-A");
    // 上游恢复后同一账号立即可用（凭据没有被清掉）
    *t.env.fake.refresh_mode.lock().unwrap() = RefreshMode::Rotate;
    // 软降级的账号排在最后但仍会被尝试
    let (conn, token) = t.run(&calls, |_, _| None).await.unwrap();
    assert_eq!(conn, "a");
    assert!(token.starts_with("ACCESS-REFRESH-"), "{token}");
}

#[tokio::test]
async fn github_and_muse_report_the_missing_exchange_step_and_never_send_the_account_token() {
    for id in ["github", "muse"] {
        let t = setup(id, true).await;
        t.add_oauth_connection("a", 1, "ACCOUNT-TOKEN-MUST-NOT-LEAK", "REFRESH-A", 3600).await;
        let calls = new_calls();
        let failure = t.run(&calls, |_, _| None).await.unwrap_err();
        match failure.error {
            DispatchError::NotImplemented(msg) => {
                assert!(msg.starts_with(TOKEN_EXCHANGE_PENDING), "{id}: {msg}");
                assert!(!msg.contains("ACCOUNT-TOKEN"), "{id}");
            }
            other => panic!("{id}: expected NotImplemented, got {other:?}"),
        }
        assert!(calls.lock().unwrap().is_empty(), "{id}: the account token is never used as a model key");
        assert_eq!(t.env.fake.refresh_count.load(Ordering::SeqCst), 0, "{id}: not even a refresh is attempted");
        assert_eq!(t.status_of("a").await, "active", "{id}: the connection itself is healthy");
    }
}

#[tokio::test]
async fn without_an_oauth_service_oauth_connections_are_reported_not_misused() {
    let t = setup("t-code", false).await;
    t.add_oauth_connection("a", 1, "ACCESS-A", "REFRESH-A", 3600).await;
    let calls = new_calls();
    let failure = t.run(&calls, |_, _| None).await.unwrap_err();
    assert!(matches!(failure.error, DispatchError::Local(_)), "{:?}", failure.error);
    assert!(calls.lock().unwrap().is_empty(), "the stored JSON must never be sent as a Bearer value");
    // 同一个 MemoryVault 里存的是 JSON：确认它不是裸令牌
    let raw = t.env.vault.get("connection/a/ref").unwrap().unwrap();
    assert!(raw.starts_with('{'));
}

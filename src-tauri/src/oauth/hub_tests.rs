//! OAuthHub 的端到端测试：真实 HTTP 假授权服务器 + 内存凭据库 + 内存 SQLite。

use std::time::Duration;

use sqlx::SqlitePool;

use super::hub::{HubError, HubLimits, PollResult};
use super::testkit::*;
use super::{FlowKind, OAuthTuning, Secret};
use crate::models::AccountStatus;
use crate::security::CredentialVault;
use crate::store::providers as repo;

const LEAKS: [&str; 9] = [
    "ACCESS-AUTH",
    "REFRESH-AUTH",
    "ACCESS-DEV",
    "REFRESH-DEV",
    "AUTHCODE-",
    "DEVICE-SECRET",
    "CLIENT-SECRET-XYZ",
    "code_verifier",
    "id_token",
];

fn assert_no_leak(text: &str) {
    for needle in LEAKS {
        assert!(!text.contains(needle), "leaked {needle:?}: {text}");
    }
}

async fn connection_count(db: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM connections").fetch_one(db).await.unwrap()
}

#[tokio::test]
async fn code_flow_start_then_poll_creates_an_oauth_connection() {
    let env = env().await;
    let started = env.hub.start("t-code", "  work account  ").await.unwrap();
    assert_eq!(started.flow, FlowKind::AuthorizationCode);
    let url = started.authorize_url.clone().expect("code flow returns the authorize URL");
    assert!(started.user_code.is_none() && started.verification_uri.is_none());
    assert!(url.starts_with(&env.base));
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Pending);
    assert_eq!(env.hub.pending_sessions(), 1);

    // 用户在浏览器里完成登录：授权服务器重定向到 127.0.0.1 回环回调
    assert_eq!(browse(&url).await, 200);
    let done = wait_done(&env.hub, &started.session_id).await;
    let PollResult::Succeeded { connection_id } = done else { panic!("expected success, got {done:?}") };

    let conn = repo::get_connection(&env.db, &connection_id).await.unwrap().expect("connection created");
    assert_eq!((conn.kind.as_str(), conn.status.as_str(), conn.provider_id.as_str()), ("oauth", "active", "t-code"));
    assert_eq!(conn.account_label, "work account");
    assert!(conn.enabled);
    let cref = conn.credential_ref.clone().expect("credential_ref points at the token entry");

    // 令牌在凭据库里（JSON，key = credential_ref），数据库行里没有任何令牌
    let stored = env.vault.get(&cref).unwrap().expect("token entry exists");
    assert!(stored.contains("ACCESS-AUTH-0") && stored.contains("REFRESH-AUTH-0"));
    let row_debug = format!("{conn:?}");
    assert!(!row_debug.contains("ACCESS-AUTH") && !row_debug.contains("REFRESH-AUTH"));
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT account_label || id || provider_id || kind || status || IFNULL(credential_ref, '') FROM connections",
    )
    .fetch_all(&env.db)
    .await
    .unwrap();
    assert!(columns.iter().all(|c| !c.contains("ACCESS-AUTH")));

    // 令牌服务可以直接取到它（转发管线走的就是这条路）
    let tokens = env.service.get_valid_token("t-code", &cref).await.unwrap();
    assert_eq!(tokens.access_token.expose(), "ACCESS-AUTH-0");

    // 完成后的状态保留一段时间，重复轮询结果一致；回调监听已关闭
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Succeeded { connection_id });
    assert!(port_is_closed(callback_port(&url)).await);
    assert_eq!(env.hub.pending_sessions(), 0);
}

#[tokio::test]
async fn device_flow_shows_the_user_code_and_succeeds_after_approval() {
    let env = env().await;
    *env.fake.id_token.lock().unwrap() = Some(fake_jwt_with_email("person@example.com"));

    let started = env.hub.start("t-device", "").await.unwrap();
    assert_eq!(started.flow, FlowKind::Device);
    assert_eq!(started.user_code.as_deref(), Some("WDJB-MJHT"));
    assert_eq!(started.verification_uri.as_deref(), Some("https://verify.example/device"));
    assert!(started.authorize_url.is_none());
    // 设备码（device_code）是机密，不在返回值里
    assert_no_leak(&serde_json::to_string(&started).unwrap());

    assert_eq!(env.hub.poll(&started.session_id), PollResult::Pending);
    env.fake.device_approved.store(true, std::sync::atomic::Ordering::SeqCst);
    let PollResult::Succeeded { connection_id } = wait_done(&env.hub, &started.session_id).await else {
        panic!("device flow should succeed")
    };
    let conn = repo::get_connection(&env.db, &connection_id).await.unwrap().unwrap();
    // 用户没有填账户标识：用令牌里 id_token 的邮箱
    assert_eq!(conn.account_label, "person@example.com");
    assert_eq!(conn.kind, "oauth");
}

#[tokio::test]
async fn default_label_falls_back_to_the_provider_name() {
    let env = env().await;
    env.fake.device_approved.store(true, std::sync::atomic::Ordering::SeqCst);
    let started = env.hub.start("t-device", "").await.unwrap();
    let PollResult::Succeeded { connection_id } = wait_done(&env.hub, &started.session_id).await else { panic!() };
    let conn = repo::get_connection(&env.db, &connection_id).await.unwrap().unwrap();
    assert_eq!(conn.account_label, "Provider t-device 账户");
}

#[tokio::test]
async fn invalid_labels_are_rejected_before_anything_starts() {
    let env = env().await;
    for bad in ["x".repeat(129), "a\nb".to_owned(), "tab\there".to_owned()] {
        assert_eq!(env.hub.start("t-code", &bad).await.unwrap_err().code(), "label_invalid");
    }
    assert_eq!(env.hub.pending_sessions(), 0);
}

#[tokio::test]
async fn cancel_releases_the_callback_port_immediately() {
    let env = env().await;
    let started = env.hub.start("t-code", "x").await.unwrap();
    let port = callback_port(started.authorize_url.as_deref().unwrap());
    assert!(port_is_open(port).await, "listener is up while the session is pending");

    env.hub.cancel(&started.session_id).await;

    // 不等任何超时：cancel 返回时端口已释放，状态已是 cancelled
    assert!(port_is_closed(port).await, "port must be released as soon as cancel returns");
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Cancelled);
    assert_eq!(env.hub.pending_sessions(), 0);
    assert_eq!(connection_count(&env.db).await, 0);

    // 取消后的授权码即使到达也不会创建连接（监听已关闭）
    cancel_is_idempotent(&env, &started.session_id).await;
}

async fn cancel_is_idempotent(env: &Env, session_id: &str) {
    env.hub.cancel(session_id).await;
    env.hub.cancel("no-such-session").await;
    assert_eq!(env.hub.poll(session_id), PollResult::Cancelled);
}

#[tokio::test]
async fn cancel_stops_a_device_poll() {
    let env = env().await;
    let started = env.hub.start("t-device", "x").await.unwrap();
    env.hub.cancel(&started.session_id).await;
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Cancelled);
    // 之后即使用户批准了，也不会产生连接
    env.fake.device_approved.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(connection_count(&env.db).await, 0);
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Cancelled);
}

#[tokio::test]
async fn sessions_time_out_and_are_cleaned_up() {
    let tuning = OAuthTuning { callback_timeout: Duration::from_millis(300), ..fast_tuning() };
    let limits = HubLimits { finished_retention: Duration::from_millis(150), ..HubLimits::default() };
    let env = env_with(tuning, limits).await;

    let started = env.hub.start("t-code", "x").await.unwrap();
    let port = callback_port(started.authorize_url.as_deref().unwrap());
    assert_eq!(wait_done(&env.hub, &started.session_id).await, PollResult::Expired);
    assert!(port_is_closed(port).await, "timed-out session releases its port");
    assert_eq!(connection_count(&env.db).await, 0);

    // 过了保留期，会话被清理：再轮询得到 expired，且不再占用内存
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(env.hub.poll(&started.session_id), PollResult::Expired);
    assert_eq!(env.hub.retained_sessions(), 0);
}

#[tokio::test]
async fn unknown_sessions_poll_as_expired() {
    let env = env().await;
    assert_eq!(env.hub.poll("never-existed"), PollResult::Expired);
}

#[tokio::test]
async fn failed_authorization_reports_only_a_redacted_code() {
    let env = env().await;
    // 授权码被换成无效的：令牌端点拒绝 -> failed{code}
    let started = env.hub.start("t-code", "x").await.unwrap();
    let url = started.authorize_url.clone().unwrap();
    let port = callback_port(&url);
    // 直接把一个伪造的回调打到监听上（state 不匹配）：授权必须被拒绝
    let bad = format!("http://127.0.0.1:{port}/cb?code=AUTHCODE-forged&state=wrong");
    let client = reqwest::Client::new();
    assert_eq!(client.get(&bad).send().await.unwrap().status().as_u16(), 400);
    let done = wait_done(&env.hub, &started.session_id).await;
    assert_eq!(done, PollResult::Failed { code: "state_mismatch".into() });
    assert_no_leak(&serde_json::to_string(&done).unwrap());
    assert_eq!(connection_count(&env.db).await, 0);
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn pending_sessions_are_capped_and_a_provider_keeps_only_one() {
    let limits = HubLimits { max_pending: 2, ..HubLimits::default() };
    let env = env_with(fast_tuning(), limits).await;

    let a = env.hub.start("t-code", "a").await.unwrap();
    let b = env.hub.start("t-code2", "b").await.unwrap();
    assert_eq!(env.hub.pending_sessions(), 2);
    assert_eq!(env.hub.start("t-device", "c").await.unwrap_err().code(), "too_many_sessions");

    // 再次对同一供应商开始授权 = 放弃上一个（它占着的回调端口立即释放）
    let port_a = callback_port(a.authorize_url.as_deref().unwrap());
    let a2 = env.hub.start("t-code", "a2").await.unwrap();
    assert_eq!(env.hub.poll(&a.session_id), PollResult::Cancelled);
    assert!(port_is_closed(port_a).await);
    assert_eq!(env.hub.pending_sessions(), 2);
    assert_eq!(env.hub.poll(&b.session_id), PollResult::Pending);
    assert_eq!(env.hub.poll(&a2.session_id), PollResult::Pending);

    env.hub.shutdown().await;
}

#[tokio::test]
async fn custom_unknown_and_unconfigured_providers_are_rejected_without_binding_a_port() {
    let env = env().await;
    assert_eq!(env.hub.start("t-custom", "x").await.unwrap_err().code(), "custom_flow_required");
    assert_eq!(env.hub.start("nope", "x").await.unwrap_err().code(), "unknown_provider");
    // 需要用户自备凭据而没配置：对应界面文案「需要配置客户端凭据」
    assert_eq!(env.hub.start("t-secret", "x").await.unwrap_err().code(), "missing_client_credential");
    assert_eq!(env.hub.pending_sessions(), 0);
    assert_eq!(env.hub.retained_sessions(), 0, "failed starts leave no session behind");
}

#[tokio::test]
async fn provider_listing_reflects_flow_status_and_client_credentials() {
    let env = env().await;
    let list = env.hub.list_providers().await.unwrap();
    assert_eq!(list.len(), 9);
    let info = |id: &str| serde_json::to_value(list.iter().find(|p| p.provider_id == id).unwrap()).unwrap();

    let code = info("t-code");
    assert_eq!((code["flow"].as_str(), code["status"].as_str()), (Some("authorization-code"), Some("ready")));
    assert_eq!(code["riskNotice"], "risk text");
    assert_eq!(code["clientConfigured"], false);
    assert_eq!(info("t-device")["flow"], "device");

    let custom = info("t-custom");
    assert_eq!((custom["flow"].as_str(), custom["status"].as_str()), (Some("custom"), Some("custom")));
    assert_eq!(custom["reason"], "proprietary protocol");
    assert_eq!(custom["requires"], serde_json::json!(["official API"]));

    let secret = info("t-secret");
    assert_eq!(secret["status"], "need_client_credentials");
    assert_eq!(secret["clientIdRequired"], true);
    assert_eq!(secret["clientSecret"], "required");
    let hosted = info("t-hosted");
    assert_eq!((hosted["supportsBaseUrl"].as_bool(), hosted["clientSecret"].as_str()), (Some(true), Some("optional")));
    assert_eq!(code["clientSecret"], "none");

    // github / muse：可以授权，但令牌兑换步骤没接入，状态说明里如实写明
    for id in ["github", "muse"] {
        let v = info(id);
        assert_eq!(v["status"], "ready", "{id}");
        assert_eq!(v["tokenExchangePending"], true, "{id}");
        assert!(v["limitation"].as_str().unwrap().contains("令牌兑换步骤尚未接入"), "{id}");
    }
    assert_eq!(info("t-device")["tokenExchangePending"], false);
}

#[tokio::test]
async fn client_credentials_are_stored_in_the_vault_and_never_returned() {
    let env = env().await;
    *env.fake.expect_secret.lock().unwrap() = Some("CLIENT-SECRET-XYZ".into());

    // 缺必需项：提示需要配置客户端凭据；不会写入任何东西
    let err = env.hub.set_client_credentials("t-secret", "my-client", None, None).await.unwrap_err();
    assert_eq!(err.code(), "missing_client_credential");
    let err = env.hub.set_client_credentials("t-secret", "", Some(Secret::new("CLIENT-SECRET-XYZ")), None).await.unwrap_err();
    assert_eq!(err.code(), "missing_client_credential");
    assert!(env.vault.get("oauth-client/t-secret").unwrap().is_none());

    let info = env
        .hub
        .set_client_credentials("t-secret", "  my-client  ", Some(Secret::new("CLIENT-SECRET-XYZ")), None)
        .await
        .unwrap();
    let json = serde_json::to_string(&info).unwrap();
    assert_no_leak(&json);
    assert!(!json.contains("my-client"), "client id content is not echoed either: {json}");
    assert_eq!(info.provider_id, "t-secret");
    assert!(info.client_configured);
    assert_eq!(serde_json::to_value(&info).unwrap()["status"], "ready");
    assert!(env.vault.get("oauth-client/t-secret").unwrap().unwrap().contains("my-client"));

    // 用它走完一次授权（令牌端点校验 client_secret）
    let started = env.hub.start("t-secret", "s").await.unwrap();
    browse(started.authorize_url.as_deref().unwrap()).await;
    assert!(matches!(wait_done(&env.hub, &started.session_id).await, PollResult::Succeeded { .. }));

    // 清除后回到「需要配置」
    let cleared = env.hub.clear_client_credentials("t-secret").await.unwrap();
    assert!(!cleared.client_configured);
    assert_eq!(serde_json::to_value(&cleared).unwrap()["status"], "need_client_credentials");
    assert!(env.vault.get("oauth-client/t-secret").unwrap().is_none());
    assert_eq!(env.hub.start("t-secret", "s").await.unwrap_err().code(), "missing_client_credential");
}

#[tokio::test]
async fn client_credential_input_is_validated() {
    let env = env().await;
    let s = || Some(Secret::new("CLIENT-SECRET-XYZ"));
    // 不需要用户凭据的供应商（公开 clientId）：拒绝写入
    assert_eq!(env.hub.set_client_credentials("t-code", "x", None, None).await.unwrap_err().code(), "invalid_input");
    assert_eq!(env.hub.set_client_credentials("t-custom", "x", None, None).await.unwrap_err().code(), "custom_flow_required");
    assert_eq!(env.hub.set_client_credentials("nope", "x", None, None).await.unwrap_err().code(), "unknown_provider");
    // 控制字符 / 过长
    assert_eq!(env.hub.set_client_credentials("t-secret", "a\nb", s(), None).await.unwrap_err().code(), "invalid_input");
    assert_eq!(
        env.hub.set_client_credentials("t-secret", &"x".repeat(513), s(), None).await.unwrap_err().code(),
        "invalid_input"
    );
    // 只有支持 baseUrl 的供应商接受它，且必须是 https（或回环）
    assert_eq!(
        env.hub.set_client_credentials("t-secret", "id", s(), Some("https://git.example.org")).await.unwrap_err().code(),
        "invalid_input"
    );
    assert_eq!(
        env.hub.set_client_credentials("t-hosted", "id", None, Some("http://git.example.org")).await.unwrap_err().code(),
        "invalid_input"
    );
    let ok = env.hub.set_client_credentials("t-hosted", "id", None, Some("https://git.example.org")).await.unwrap();
    assert!(ok.client_configured);
    assert!(env.vault.get("oauth-client/t-hosted").unwrap().unwrap().contains("git.example.org"));
    assert_eq!(env.hub.pending_sessions(), 0);
}

async fn authorize_new_connection(env: &Env, provider: &str, label: &str) -> String {
    let started = env.hub.start(provider, label).await.unwrap();
    browse(started.authorize_url.as_deref().unwrap()).await;
    match wait_done(&env.hub, &started.session_id).await {
        PollResult::Succeeded { connection_id } => connection_id,
        other => panic!("authorization failed: {other:?}"),
    }
}

#[tokio::test]
async fn reauthorize_replaces_the_tokens_in_place_and_restores_the_connection() {
    let env = env().await;
    let id = authorize_new_connection(&env, "t-code", "acct").await;
    let before = repo::get_connection(&env.db, &id).await.unwrap().unwrap();
    let cref = before.credential_ref.clone().unwrap();
    let old_json = env.vault.get(&cref).unwrap().unwrap();

    // 令牌失效后连接被标记为 needsReauth
    sqlx::query("UPDATE connections SET status = 'needsReauth' WHERE id = ?1").bind(&id).execute(&env.db).await.unwrap();

    let started = env.hub.reauthorize(&id).await.unwrap();
    assert_eq!(started.flow, FlowKind::AuthorizationCode);
    browse(started.authorize_url.as_deref().unwrap()).await;
    assert_eq!(wait_done(&env.hub, &started.session_id).await, PollResult::Succeeded { connection_id: id.clone() });

    let after = repo::get_connection(&env.db, &id).await.unwrap().unwrap();
    assert_eq!(AccountStatus::parse(&after.status), Some(AccountStatus::Active));
    assert_eq!(after.credential_ref.as_deref(), Some(cref.as_str()), "credential entry is updated in place");
    assert_eq!(after.account_label, "acct");
    assert_eq!(connection_count(&env.db).await, 1, "no second connection is created");
    let new_json = env.vault.get(&cref).unwrap().unwrap();
    assert_ne!(new_json, old_json);
    assert!(new_json.contains("ACCESS-AUTH-1"));

    // 用户手动暂停的连接重新授权后仍是 paused
    sqlx::query("UPDATE connections SET status = 'paused' WHERE id = ?1").bind(&id).execute(&env.db).await.unwrap();
    let again = env.hub.reauthorize(&id).await.unwrap();
    browse(again.authorize_url.as_deref().unwrap()).await;
    wait_done(&env.hub, &again.session_id).await;
    let paused = repo::get_connection(&env.db, &id).await.unwrap().unwrap();
    assert_eq!(paused.status, "paused");
}

#[tokio::test]
async fn reauthorize_validates_the_connection() {
    let env = env().await;
    assert_eq!(env.hub.reauthorize("missing").await.unwrap_err().code(), "not_found");
    sqlx::query(
        "INSERT INTO connections (id, provider_id, account_label, kind, created_at, updated_at) \
         VALUES ('k1', 't-code', 'key', 'apiKey', 't', 't')",
    )
    .execute(&env.db)
    .await
    .unwrap();
    assert_eq!(env.hub.reauthorize("k1").await.unwrap_err().code(), "not_oauth");
    assert_eq!(env.hub.disconnect("k1").await.unwrap_err().code(), "not_oauth");
    assert_eq!(env.hub.disconnect("missing").await.unwrap_err().code(), "not_found");
    assert_eq!(connection_count(&env.db).await, 1, "an API-key connection is untouched by OAuth disconnect");
}

#[tokio::test]
async fn disconnect_removes_the_connection_and_its_token_entry() {
    let env = env().await;
    let id = authorize_new_connection(&env, "t-code", "acct").await;
    let cref = repo::get_connection(&env.db, &id).await.unwrap().unwrap().credential_ref.unwrap();
    assert!(env.vault.get(&cref).unwrap().is_some());
    sqlx::query(
        "INSERT INTO quota_snapshots (connection_id, kind, quota_name, checked_at) VALUES (?1, 'period', '5h', 't')",
    )
    .bind(&id)
    .execute(&env.db)
    .await
    .unwrap();

    // 断开时正在进行的重新授权会话一并取消
    let reauth = env.hub.reauthorize(&id).await.unwrap();
    let port = callback_port(reauth.authorize_url.as_deref().unwrap());

    env.hub.disconnect(&id).await.unwrap();

    assert!(repo::get_connection(&env.db, &id).await.unwrap().is_none());
    assert!(env.vault.get(&cref).unwrap().is_none(), "token entry is deleted");
    let snaps: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_snapshots").fetch_one(&env.db).await.unwrap();
    assert_eq!(snaps, 0, "quota snapshots cascade with the connection");
    assert_eq!(env.hub.poll(&reauth.session_id), PollResult::Cancelled);
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn shutdown_closes_every_listener() {
    let env = env().await;
    let a = env.hub.start("t-code", "a").await.unwrap();
    let b = env.hub.start("t-code2", "b").await.unwrap();
    let ports = [callback_port(a.authorize_url.as_deref().unwrap()), callback_port(b.authorize_url.as_deref().unwrap())];
    assert!(port_is_open(ports[0]).await && port_is_open(ports[1]).await);

    env.hub.shutdown().await;

    for port in ports {
        assert!(port_is_closed(port).await, "port {port} must be closed after shutdown");
    }
    assert_eq!(env.hub.pending_sessions(), 0);
    assert_eq!(env.hub.start("t-code", "c").await.unwrap_err().code(), "cancelled", "no new sessions after shutdown");
}

#[tokio::test]
async fn hub_errors_serialize_as_code_and_message_only() {
    let json = serde_json::to_value(HubError::new("missing_client_credential")).unwrap();
    assert_eq!(json["code"], "missing_client_credential");
    assert!(json["message"].as_str().unwrap().contains("missing_client_credential"));
    assert_eq!(json.as_object().unwrap().len(), 2);
}

// ───────────────────────── 迁移 ─────────────────────────

/// 0001 + 0002 的旧库升级到 0003：连接、凭据引用、配额快照一个不丢，新状态可用，外键仍然级联。
#[tokio::test]
async fn migration_0003_keeps_existing_connections_and_allows_the_new_status() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let opts = SqliteConnectOptions::new().in_memory(true).foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(opts)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0001_init.sql")).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0002_provider_catalog.sql")).execute(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, created_at, updated_at) VALUES ('claude', 'builtin', 'Claude', 'oauth', 't', 't')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (id, kind, status, cref) in [
        ("c-key", "apiKey", "active", "connection/c-key/r1"),
        ("c-oauth", "oauth", "error", "connection/c-oauth/r2"),
        ("c-paused", "oauth", "paused", "connection/c-paused/r3"),
    ] {
        sqlx::query(
            "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at) \
             VALUES (?1, 'claude', ?1, ?2, ?3, 7, 3, 1, ?4, 'created', 'updated')",
        )
        .bind(id)
        .bind(kind)
        .bind(status)
        .bind(cref)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO quota_snapshots (connection_id, kind, quota_name, checked_at) VALUES ('c-oauth', 'period', '5h', 't')")
        .execute(&pool)
        .await
        .unwrap();
    // 旧约束确实不接受新状态
    assert!(sqlx::query("UPDATE connections SET status = 'needsReauth' WHERE id = 'c-oauth'").execute(&pool).await.is_err());

    sqlx::raw_sql(include_str!("../../migrations/0003_oauth_connection_status.sql")).execute(&pool).await.unwrap();

    #[derive(sqlx::FromRow, Debug, PartialEq)]
    struct Row {
        id: String,
        provider_id: String,
        kind: String,
        status: String,
        priority: i64,
        weight: i64,
        enabled: bool,
        credential_ref: Option<String>,
        created_at: String,
        updated_at: String,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, provider_id, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at \
         FROM connections ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "no connection is lost");
    assert_eq!(
        rows[0],
        Row {
            id: "c-key".into(),
            provider_id: "claude".into(),
            kind: "apiKey".into(),
            status: "active".into(),
            priority: 7,
            weight: 3,
            enabled: true,
            credential_ref: Some("connection/c-key/r1".into()),
            created_at: "created".into(),
            updated_at: "updated".into(),
        }
    );
    assert_eq!(rows[1].status, "error");
    assert_eq!(rows[2].status, "paused");
    let snaps: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_snapshots").fetch_one(&pool).await.unwrap();
    assert_eq!(snaps, 1, "quota snapshots survive the rebuild");

    // 新状态可写入，其它非法值仍被拒绝；索引与外键都还在
    sqlx::query("UPDATE connections SET status = 'needsReauth' WHERE id = 'c-oauth'").execute(&pool).await.unwrap();
    assert!(sqlx::query("UPDATE connections SET status = 'bogus' WHERE id = 'c-oauth'").execute(&pool).await.is_err());
    let index: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_connections_provider'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(index, 1);
    sqlx::query("DELETE FROM connections WHERE id = 'c-oauth'").execute(&pool).await.unwrap();
    let snaps: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_snapshots").fetch_one(&pool).await.unwrap();
    assert_eq!(snaps, 0, "ON DELETE CASCADE still works for quota snapshots");
    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(&pool).await.unwrap();
    assert_eq!(fk, 1, "foreign keys are switched back on");
    let orphan = sqlx::query("INSERT INTO connections (id, provider_id, account_label, kind, created_at, updated_at) VALUES ('x', 'no-such-provider', 'x', 'oauth', 't', 't')")
        .execute(&pool)
        .await;
    assert!(orphan.is_err(), "the provider foreign key is enforced on the rebuilt table");
}

//! OAuth 框架集成测试：用 axum 在随机端口起假授权 / 令牌 / 设备码服务器，走真实 HTTP。
//!
//! 直接使用库 crate 的公开 API（`router_desktop_lib::oauth`）。
#![allow(dead_code, unused_imports, clippy::too_many_arguments)]

use router_desktop_lib::oauth;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use oauth::pkce::challenge_for;
use oauth::{
    ClientCredentials, MemoryTokenStore, NoClientCredentials, OAuthCatalog, OAuthError, OAuthService, OAuthTuning,
    Secret, StaticClientCredentials, TokenSet, TokenStore,
};

// ---------------------------------------------------------------------------
// 假服务器
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum RefreshMode {
    #[default]
    Rotate,
    NoRotate,
    InvalidGrant,
    ServerError,
    EchoToken,
}

#[derive(Clone, Debug)]
struct Recorded {
    fields: HashMap<String, String>,
    user_agent: String,
    content_type: String,
}

#[derive(Default)]
struct Fake {
    challenges: Mutex<HashMap<String, Option<String>>>,
    authorize_state_override: Mutex<Option<String>>,
    authorize_error: Mutex<Option<String>>,
    expect_secret: Mutex<Option<String>>,
    echo_exchange_errors: Mutex<bool>,
    token_requests: Mutex<Vec<Recorded>>,
    code_counter: AtomicUsize,
    refresh_count: AtomicUsize,
    refresh_mode: Mutex<RefreshMode>,
    refresh_delay_ms: AtomicU64,
    current_refresh: Mutex<String>,
    device_requests: Mutex<Vec<Recorded>>,
    device_response: Mutex<Option<(u16, Value)>>,
    device_polls: Mutex<VecDeque<(u16, Value)>>,
    device_poll_times: Mutex<Vec<Instant>>,
    device_poll_records: Mutex<Vec<Recorded>>,
}

fn record(headers: &HeaderMap, body: &Bytes) -> Recorded {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let fields: HashMap<String, String> = if content_type.contains("json") {
        serde_json::from_slice::<HashMap<String, Value>>(body)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.as_str().unwrap_or("").to_owned()))
            .collect()
    } else {
        url::form_urlencoded::parse(body).into_owned().collect()
    };
    Recorded {
        fields,
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned(),
        content_type,
    }
}

async fn authorize(State(f): State<Arc<Fake>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let redirect_uri = q.get("redirect_uri").cloned().unwrap_or_default();
    let state = f
        .authorize_state_override
        .lock()
        .unwrap()
        .clone()
        .or_else(|| q.get("state").cloned())
        .unwrap_or_default();
    let mut target = url::Url::parse(&redirect_uri).expect("redirect_uri is a URL");
    if let Some(error) = f.authorize_error.lock().unwrap().clone() {
        target.query_pairs_mut().append_pair("error", &error).append_pair("state", &state);
    } else {
        let code = format!("AUTHCODE-{}", f.code_counter.fetch_add(1, Ordering::SeqCst));
        f.challenges.lock().unwrap().insert(code.clone(), q.get("code_challenge").cloned());
        target.query_pairs_mut().append_pair("code", &code).append_pair("state", &state);
    }
    Redirect::to(target.as_str()).into_response()
}

fn json_reply(status: u16, body: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

async fn token(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let rec = record(&headers, &body);
    f.token_requests.lock().unwrap().push(rec.clone());
    let fields = &rec.fields;
    match fields.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let code = fields.get("code").cloned().unwrap_or_default();
            if *f.echo_exchange_errors.lock().unwrap() {
                return json_reply(
                    400,
                    json!({"error": "invalid_grant", "error_description": format!(
                        "bad code {code} verifier {}", fields.get("code_verifier").cloned().unwrap_or_default())}),
                );
            }
            let Some(challenge) = f.challenges.lock().unwrap().remove(&code) else {
                return json_reply(400, json!({"error": "invalid_grant"}));
            };
            if let Some(challenge) = challenge {
                let verifier = fields.get("code_verifier").cloned().unwrap_or_default();
                if challenge_for(&verifier) != challenge {
                    return json_reply(400, json!({"error": "invalid_grant", "error_description": "PKCE verification failed"}));
                }
            }
            if let Some(expected) = f.expect_secret.lock().unwrap().clone() {
                if fields.get("client_secret") != Some(&expected) {
                    return json_reply(401, json!({"error": "invalid_client"}));
                }
            }
            *f.current_refresh.lock().unwrap() = "REFRESH-1".to_owned();
            json_reply(
                200,
                json!({"access_token": "ACCESS-1", "refresh_token": "REFRESH-1", "expires_in": 3600,
                       "scope": "a b", "token_type": "Bearer"}),
            )
        }
        Some("refresh_token") => {
            let n = f.refresh_count.fetch_add(1, Ordering::SeqCst) + 1;
            let delay = f.refresh_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let presented = fields.get("refresh_token").cloned().unwrap_or_default();
            if let Some(expected) = f.expect_secret.lock().unwrap().clone() {
                if fields.get("client_secret") != Some(&expected) {
                    return json_reply(401, json!({"error": "invalid_client"}));
                }
            }
            match *f.refresh_mode.lock().unwrap() {
                RefreshMode::InvalidGrant => json_reply(400, json!({"error": "invalid_grant"})),
                RefreshMode::ServerError => json_reply(503, json!({"error": "temporarily_unavailable"})),
                RefreshMode::EchoToken => json_reply(
                    400,
                    json!({"error": "invalid_request", "error_description": format!("cannot use refresh_token {presented}")}),
                ),
                mode => {
                    if presented != *f.current_refresh.lock().unwrap() {
                        return json_reply(400, json!({"error": "invalid_grant"}));
                    }
                    let new_refresh = format!("REFRESH-{}", n + 1);
                    *f.current_refresh.lock().unwrap() = new_refresh.clone();
                    let mut body = json!({"access_token": format!("ACCESS-{}", n + 1), "expires_in": 3600});
                    if mode == RefreshMode::Rotate {
                        body["refresh_token"] = json!(new_refresh);
                    } else {
                        *f.current_refresh.lock().unwrap() = presented;
                    }
                    json_reply(200, body)
                }
            }
        }
        _ => json_reply(400, json!({"error": "unsupported_grant_type"})),
    }
}

async fn device_code(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    f.device_requests.lock().unwrap().push(record(&headers, &body));
    let scripted = f.device_response.lock().unwrap().clone();
    let (status, body) = scripted.unwrap_or((
        200,
        json!({"device_code": "DEVICE-SECRET-1", "user_code": "WDJB-MJHT",
               "verification_uri": "https://verify.example/device", "expires_in": 600, "interval": 1}),
    ));
    json_reply(status, body)
}

async fn device_token(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    f.device_poll_times.lock().unwrap().push(Instant::now());
    f.device_poll_records.lock().unwrap().push(record(&headers, &body));
    let next = f.device_polls.lock().unwrap().pop_front();
    let (status, body) = next.unwrap_or((400, json!({"error": "authorization_pending"})));
    json_reply(status, body)
}

struct Harness {
    base: String,
    fake: Arc<Fake>,
    store: Arc<MemoryTokenStore>,
    creds: Arc<StaticClientCredentials>,
    service: Arc<OAuthService>,
}

fn catalog_json(base: &str) -> String {
    let code_common = json!({
        "flow": "authorization-code",
        "authorizeUrl": format!("{base}/authorize"),
        "tokenUrl": format!("{base}/token"),
        "refreshUrl": format!("{base}/token"),
        "clientId": "cid",
        "scopes": ["a", "b"],
        "pkce": true,
        "redirect": {"host": "127.0.0.1", "path": "/cb"},
        "refreshLeadSeconds": 300
    });
    let mut json_enc = code_common.clone();
    json_enc["id"] = json!("t-json");
    json_enc["pkce"] = json!(false);
    json_enc["tokenEncoding"] = json!("json");
    json_enc["refreshEncoding"] = json!("json");
    json_enc["tokenRequestIncludesState"] = json!(true);
    json_enc["refreshScope"] = json!("a b");
    let mut code = code_common.clone();
    code["id"] = json!("t-code");
    let mut secret = code_common.clone();
    secret["id"] = json!("t-secret");
    secret["clientId"] = Value::Null;
    secret["requiresClientSecret"] = json!(true);
    let device = json!({
        "id": "t-device", "flow": "device",
        "deviceCodeUrl": format!("{base}/device/code"),
        "tokenUrl": format!("{base}/device/token"),
        "clientId": "dcid", "scopes": ["x"], "pkce": false
    });
    let mut device_pkce = device.clone();
    device_pkce["id"] = json!("t-device-pkce");
    device_pkce["pkce"] = json!(true);
    device_pkce["extraDeviceParams"] = json!({"audience": "agent-backend"});
    let mut norefresh = code_common.clone();
    norefresh["id"] = json!("t-norefresh");
    norefresh["supportsRefresh"] = json!(false);
    let custom = json!({"id": "t-custom", "flow": "custom", "custom": {"reason": "proprietary", "requires": ["x"]}});
    json!({"schemaVersion": 1, "providers": [code, json_enc, secret, device, device_pkce, norefresh, custom]}).to_string()
}

fn fast_tuning() -> OAuthTuning {
    OAuthTuning {
        callback_timeout: Duration::from_secs(10),
        http_timeout: Duration::from_secs(10),
        device_interval_override: Some(Duration::from_millis(20)),
        slow_down_step: Duration::from_millis(80),
    }
}

async fn harness_with(tuning: OAuthTuning) -> Harness {
    let fake = Arc::new(Fake::default());
    *fake.current_refresh.lock().unwrap() = "REFRESH-0".to_owned();
    let app = Router::new()
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/device/code", post(device_code))
        .route("/device/token", post(device_token))
        .with_state(fake.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let store = Arc::new(MemoryTokenStore::new());
    let creds = Arc::new(StaticClientCredentials::new());
    let catalog = OAuthCatalog::from_json(&catalog_json(&base)).expect("test catalog is valid");
    let service = Arc::new(OAuthService::new(catalog, creds.clone(), store.clone(), tuning).unwrap());
    Harness { base, fake, store, creds, service }
}

async fn harness() -> Harness {
    harness_with(fast_tuning()).await
}

fn browser() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

async fn port_is_closed(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).await.is_err()
}

fn token_set(access: &str, refresh: Option<&str>, expires_in_secs: i64) -> TokenSet {
    TokenSet {
        access_token: Secret::new(access),
        refresh_token: refresh.map(Secret::new),
        expires_at: Some(Utc::now() + chrono::Duration::seconds(expires_in_secs)),
        scopes: vec!["a".into(), "b".into()],
        extra: Default::default(),
    }
}

fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url).unwrap().query_pairs().into_owned().collect()
}

fn assert_no_leak(haystack: &str, needles: &[&str]) {
    for needle in needles {
        assert!(!haystack.contains(needle), "output leaked {needle:?}: {haystack}");
    }
}

// ---------------------------------------------------------------------------
// authorization-code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_authorization_code_end_to_end_with_pkce() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let port = session.port();
    let q = query_of(&session.authorize_url);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["scope"], "a b");
    assert_eq!(q["state"], session.state);
    assert!(session.state.len() >= 43, "state must be high entropy");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["redirect_uri"], format!("http://127.0.0.1:{port}/cb"));
    assert_eq!(q["redirect_uri"], session.redirect_uri);

    let url = session.authorize_url.clone();
    let browse = tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    let tokens = session.wait().await.expect("flow completes");
    assert_eq!(browse.await.unwrap().status(), 200);

    assert_eq!(tokens.access_token.expose(), "ACCESS-1");
    assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "REFRESH-1");
    assert_eq!(tokens.scopes, vec!["a", "b"]);
    assert!(tokens.expires_at.unwrap() > Utc::now());
    assert_eq!(tokens.extra_value("token_type"), Some("Bearer"));

    let reqs = h.fake.token_requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].user_agent, format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));
    assert!(reqs[0].content_type.starts_with("application/x-www-form-urlencoded"));
    assert!(reqs[0].fields.contains_key("code_verifier"));
    assert_eq!(reqs[0].fields["redirect_uri"], format!("http://127.0.0.1:{port}/cb"));
    assert!(!reqs[0].fields.contains_key("client_secret"));

    assert!(port_is_closed(port).await, "listener must be closed right after authorization");
}

#[tokio::test]
async fn oauth_json_token_request_carries_state_when_configured() {
    let h = harness().await;
    let session = h.service.start_authorization("t-json").await.unwrap();
    let state = session.state.clone();
    assert!(!query_of(&session.authorize_url).contains_key("code_challenge"));
    let url = session.authorize_url.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    session.wait().await.unwrap();
    let req = h.fake.token_requests.lock().unwrap()[0].clone();
    assert!(req.content_type.starts_with("application/json"));
    assert_eq!(req.fields["state"], state);
    assert!(!req.fields.contains_key("code_verifier"));
}

#[tokio::test]
async fn oauth_state_mismatch_is_rejected_without_exchanging_the_code() {
    let h = harness().await;
    *h.fake.authorize_state_override.lock().unwrap() = Some("attacker-chosen-state".into());
    let session = h.service.start_authorization("t-code").await.unwrap();
    let port = session.port();
    let url = session.authorize_url.clone();
    let browse = tokio::spawn(async move { browser().get(url).send().await.unwrap().status() });
    let err = session.wait().await.unwrap_err();
    assert_eq!(err, OAuthError::StateMismatch);
    assert_eq!(browse.await.unwrap(), 400, "browser is told the callback was rejected");
    assert!(h.fake.token_requests.lock().unwrap().is_empty(), "code must not be exchanged");
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn oauth_callback_without_state_is_rejected() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let redirect = session.redirect_uri.clone();
    tokio::spawn(async move { browser().get(format!("{redirect}?code=stolen")).send().await.unwrap() });
    assert_eq!(session.wait().await.unwrap_err(), OAuthError::StateMismatch);
    assert!(h.fake.token_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oauth_callback_times_out_and_releases_the_port() {
    let tuning = OAuthTuning { callback_timeout: Duration::from_millis(300), ..fast_tuning() };
    let h = harness_with(tuning).await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let port = session.port();
    let started = Instant::now();
    assert_eq!(session.wait().await.unwrap_err(), OAuthError::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn oauth_user_denial_and_provider_errors_are_distinct() {
    let h = harness().await;

    *h.fake.authorize_error.lock().unwrap() = Some("access_denied".into());
    let session = h.service.start_authorization("t-code").await.unwrap();
    let url = session.authorize_url.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    assert_eq!(session.wait().await.unwrap_err(), OAuthError::AccessDenied);

    *h.fake.authorize_error.lock().unwrap() = Some("server_error".into());
    let session = h.service.start_authorization("t-code").await.unwrap();
    let url = session.authorize_url.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    match session.wait().await.unwrap_err() {
        OAuthError::CallbackError { code, .. } => assert_eq!(code, "server_error"),
        other => panic!("unexpected {other:?}"),
    }
    assert!(h.fake.token_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oauth_cancel_stops_waiting_and_releases_the_port() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let port = session.port();
    let cancel = session.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    assert_eq!(session.wait().await.unwrap_err(), OAuthError::Cancelled);
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn oauth_dropping_a_session_releases_the_port() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let port = session.port();
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(port_is_closed(port).await);
}

#[tokio::test]
async fn oauth_callback_with_foreign_host_header_is_ignored() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let state = session.state.clone();
    let redirect = session.redirect_uri.clone();
    let cancel = session.cancel_handle();
    let waiter = tokio::spawn(session.wait());

    let rebinding = browser()
        .get(format!("{redirect}?code=x&state={state}"))
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebinding.status(), 400);
    // 其它路径不受影响，也不会结束授权
    let other = browser().get(redirect.replace("/cb", "/other")).send().await.unwrap();
    assert_eq!(other.status(), 404);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished(), "foreign-host request must not complete the flow");
    cancel.cancel();
    assert_eq!(waiter.await.unwrap().unwrap_err(), OAuthError::Cancelled);
}

#[tokio::test]
async fn oauth_missing_client_credential_is_reported_clearly() {
    let h = harness().await;

    // 没有任何用户凭据
    let err = h.service.start_authorization("t-secret").await.unwrap_err();
    assert_eq!(
        err,
        OAuthError::MissingClientCredential { provider: "t-secret".into(), field: "client_id" }
    );
    assert_eq!(err.code(), "missing_client_credential");

    // 只有 clientId，没有 secret
    h.creds.set(
        "t-secret",
        ClientCredentials { client_id: Some("user-cid".into()), ..Default::default() },
    );
    let err = h.service.start_authorization("t-secret").await.unwrap_err();
    assert_eq!(
        err,
        OAuthError::MissingClientCredential { provider: "t-secret".into(), field: "client_secret" }
    );

    // 完整提供后，secret 只出现在出站请求里
    *h.fake.expect_secret.lock().unwrap() = Some("USER-SECRET-VALUE".into());
    h.creds.set(
        "t-secret",
        ClientCredentials {
            client_id: Some("user-cid".into()),
            client_secret: Some(Secret::new("USER-SECRET-VALUE")),
            base_url: None,
        },
    );
    let session = h.service.start_authorization("t-secret").await.unwrap();
    assert_eq!(query_of(&session.authorize_url)["client_id"], "user-cid");
    assert_no_leak(&format!("{session:?}"), &["USER-SECRET-VALUE"]);
    assert_no_leak(&session.authorize_url, &["USER-SECRET-VALUE"]);
    let url = session.authorize_url.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    session.wait().await.unwrap();
    assert_eq!(h.fake.token_requests.lock().unwrap()[0].fields["client_secret"], "USER-SECRET-VALUE");
}

#[tokio::test]
async fn oauth_builtin_catalog_entry_points_reject_unusable_providers() {
    let store = Arc::new(MemoryTokenStore::new());
    let service = OAuthService::with_builtin_catalog(Arc::new(NoClientCredentials), store).unwrap();

    // 需要用户自备 clientId / clientSecret
    assert!(matches!(
        service.start_authorization("gemini-cli").await.unwrap_err(),
        OAuthError::MissingClientCredential { .. }
    ));
    assert!(matches!(
        service.start_authorization("antigravity").await.unwrap_err(),
        OAuthError::MissingClientCredential { .. }
    ));
    // custom 不硬做
    for id in ["cursor", "qoder", "kiro", "zed", "cline"] {
        assert!(matches!(
            service.start_authorization(id).await.unwrap_err(),
            OAuthError::CustomFlowRequired { .. }
        ));
        assert!(matches!(service.start_device(id).await.unwrap_err(), OAuthError::CustomFlowRequired { .. }));
        assert!(matches!(
            service.get_valid_token(id, "k").await.unwrap_err(),
            OAuthError::CustomFlowRequired { .. }
        ));
    }
    // 流程类型不符 / 未知
    assert!(matches!(service.start_device("claude").await.unwrap_err(), OAuthError::UnsupportedFlow { .. }));
    assert!(matches!(
        service.start_authorization("github").await.unwrap_err(),
        OAuthError::UnsupportedFlow { .. }
    ));
    assert!(matches!(service.start_authorization("nope").await.unwrap_err(), OAuthError::UnknownProvider(_)));
}

// ---------------------------------------------------------------------------
// device code
// ---------------------------------------------------------------------------

fn pending() -> (u16, Value) {
    (400, json!({"error": "authorization_pending"}))
}

fn device_token_ok() -> (u16, Value) {
    (200, json!({"access_token": "DEVICE-ACCESS", "refresh_token": "DEVICE-REFRESH", "expires_in": 3600}))
}

#[tokio::test]
async fn oauth_device_flow_handles_pending_slow_down_and_success() {
    let h = harness().await;
    h.fake
        .device_polls
        .lock()
        .unwrap()
        .extend([pending(), (400, json!({"error": "slow_down"})), pending(), device_token_ok()]);

    let session = h.service.start_device("t-device").await.unwrap();
    assert_eq!(session.info.user_code, "WDJB-MJHT");
    assert_eq!(session.info.verification_uri, "https://verify.example/device");
    assert!(session.info.verification_uri_complete.is_none());
    assert_no_leak(&format!("{session:?}{:?}", session.info), &["DEVICE-SECRET-1"]);

    let tokens = session.wait().await.unwrap();
    assert_eq!(tokens.access_token.expose(), "DEVICE-ACCESS");
    assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "DEVICE-REFRESH");

    let polls = h.fake.device_poll_records.lock().unwrap().clone();
    assert_eq!(polls.len(), 4);
    assert_eq!(polls[0].fields["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
    assert_eq!(polls[0].fields["device_code"], "DEVICE-SECRET-1");
    assert_eq!(polls[0].fields["client_id"], "dcid");
    assert_eq!(polls[0].user_agent, format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));
    let times = h.fake.device_poll_times.lock().unwrap().clone();
    assert!(times[1] - times[0] < Duration::from_millis(75), "before slow_down: base interval");
    assert!(
        times[2] - times[1] >= Duration::from_millis(90),
        "after slow_down the interval grows ({:?})",
        times[2] - times[1]
    );
    let device_req = h.fake.device_requests.lock().unwrap()[0].clone();
    assert_eq!(device_req.fields["client_id"], "dcid");
    assert_eq!(device_req.fields["scope"], "x");
    assert!(!device_req.fields.contains_key("code_challenge"));
}

#[tokio::test]
async fn oauth_device_flow_accepts_http_200_error_bodies() {
    let h = harness().await;
    h.fake
        .device_polls
        .lock()
        .unwrap()
        .extend([(200, json!({"error": "authorization_pending"})), device_token_ok()]);
    let tokens = h.service.start_device("t-device").await.unwrap().wait().await.unwrap();
    assert_eq!(tokens.access_token.expose(), "DEVICE-ACCESS");
}

#[tokio::test]
async fn oauth_device_flow_expired_and_denied() {
    let h = harness().await;
    h.fake.device_polls.lock().unwrap().extend([pending(), (400, json!({"error": "expired_token"}))]);
    let err = h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err();
    assert_eq!(err, OAuthError::DeviceCodeExpired);

    h.fake.device_polls.lock().unwrap().extend([(400, json!({"error": "access_denied"}))]);
    let err = h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err();
    assert_eq!(err, OAuthError::AccessDenied);

    h.fake.device_polls.lock().unwrap().extend([(400, json!({"error": "invalid_client"}))]);
    let err = h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err();
    assert!(matches!(err, OAuthError::TokenEndpoint { status: 400, ref code, .. } if code == "invalid_client"));
}

#[tokio::test]
async fn oauth_device_flow_gives_up_when_the_local_deadline_passes() {
    let h = harness().await;
    *h.fake.device_response.lock().unwrap() = Some((
        200,
        json!({"device_code": "D", "user_code": "U", "verification_uri": "https://v.example/", "expires_in": 1}),
    ));
    let started = Instant::now();
    let err = h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err();
    assert_eq!(err, OAuthError::DeviceCodeExpired);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn oauth_device_flow_tolerates_brief_server_errors_but_not_persistent_ones() {
    let h = harness().await;
    h.fake.device_polls.lock().unwrap().extend([(503, json!({})), device_token_ok()]);
    assert!(h.service.start_device("t-device").await.unwrap().wait().await.is_ok());

    h.fake
        .device_polls
        .lock()
        .unwrap()
        .extend([(503, json!({})), (502, json!({})), (500, json!({}))]);
    let err = h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
}

#[tokio::test]
async fn oauth_device_flow_can_be_cancelled() {
    let h = harness().await;
    let session = h.service.start_device("t-device").await.unwrap();
    let cancel = session.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        cancel.cancel();
    });
    assert_eq!(session.wait().await.unwrap_err(), OAuthError::Cancelled);
}

#[tokio::test]
async fn oauth_device_flow_supports_pkce_status_envelope_and_user_code_polling() {
    let h = harness().await;
    // 没有 device_code，毫秒级 interval 与 expired_in 时间戳（MiniMax 风格）
    *h.fake.device_response.lock().unwrap() = Some((
        200,
        json!({"user_code": "ABCD-1234", "verification_uri": "https://v.example/d",
               "verification_uri_complete": "https://v.example/d?c=ABCD-1234",
               "expired_in": Utc::now().timestamp_millis() + 120_000, "interval": 2000}),
    ));
    h.fake
        .device_polls
        .lock()
        .unwrap()
        .extend([(200, json!({"status": "pending"})), (200, json!({"status": "slow_down"})), device_token_ok()]);

    let session = h.service.start_device("t-device-pkce").await.unwrap();
    assert_eq!(session.info.interval_secs, 2);
    assert_eq!(
        session.info.verification_uri_complete.as_deref(),
        Some("https://v.example/d?c=ABCD-1234")
    );
    session.wait().await.unwrap();

    let device_req = h.fake.device_requests.lock().unwrap()[0].clone();
    assert_eq!(device_req.fields["code_challenge_method"], "S256");
    assert_eq!(device_req.fields["audience"], "agent-backend");
    let poll = h.fake.device_poll_records.lock().unwrap()[0].clone();
    assert_eq!(poll.fields["user_code"], "ABCD-1234");
    assert!(!poll.fields.contains_key("device_code"));
    assert_eq!(
        challenge_for(&poll.fields["code_verifier"]),
        device_req.fields["code_challenge"],
        "the verifier used for polling must match the challenge sent with the device request"
    );

    // status 信封里的终态
    for (status, expected) in [("denied", OAuthError::AccessDenied), ("expired", OAuthError::DeviceCodeExpired)] {
        h.fake.device_polls.lock().unwrap().extend([(200, json!({ "status": status }))]);
        assert_eq!(h.service.start_device("t-device").await.unwrap().wait().await.unwrap_err(), expected);
    }
}

#[tokio::test]
async fn oauth_device_start_rejects_bad_server_answers() {
    let h = harness().await;
    *h.fake.device_response.lock().unwrap() = Some((
        200,
        json!({"device_code": "D", "user_code": "U", "verification_uri": "http://evil.example/steal"}),
    ));
    assert!(matches!(
        h.service.start_device("t-device").await.unwrap_err(),
        OAuthError::InvalidResponse(_)
    ));

    *h.fake.device_response.lock().unwrap() = Some((200, json!({"device_code": "D"})));
    assert!(matches!(
        h.service.start_device("t-device").await.unwrap_err(),
        OAuthError::InvalidResponse(_)
    ));

    *h.fake.device_response.lock().unwrap() = Some((400, json!({"error": "invalid_client"})));
    assert!(matches!(
        h.service.start_device("t-device").await.unwrap_err(),
        OAuthError::TokenEndpoint { status: 400, .. }
    ));

    *h.fake.device_response.lock().unwrap() = Some((503, json!({})));
    assert!(h.service.start_device("t-device").await.unwrap_err().is_retryable());
}

// ---------------------------------------------------------------------------
// refresh
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_refresh_success_rotates_and_persists() {
    let h = harness().await;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 60)).await.unwrap();

    let fresh = h.service.get_valid_token("t-code", "acct").await.unwrap();
    assert_eq!(fresh.access_token.expose(), "ACCESS-2");
    assert_eq!(fresh.refresh_token.as_ref().unwrap().expose(), "REFRESH-2");
    let stored = h.store.load("acct").await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), "ACCESS-2");
    assert_eq!(stored.refresh_token.as_ref().unwrap().expose(), "REFRESH-2");

    let req = h.fake.token_requests.lock().unwrap()[0].clone();
    assert_eq!(req.fields["grant_type"], "refresh_token");
    assert_eq!(req.fields["refresh_token"], "REFRESH-0");
    assert_eq!(req.fields["client_id"], "cid");
    assert!(req.content_type.starts_with("application/x-www-form-urlencoded"));
    assert_eq!(req.user_agent, format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));

    // 刚刷新过：不会再发请求
    h.service.get_valid_token("t-code", "acct").await.unwrap();
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oauth_refresh_is_not_triggered_outside_the_lead_window() {
    let h = harness().await;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 3600)).await.unwrap();
    let t = h.service.get_valid_token("t-code", "acct").await.unwrap();
    assert_eq!(t.access_token.expose(), "ACCESS-0");
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 0);

    // 无到期时间的令牌（例如长期密钥）也不刷新
    let mut never = token_set("LONGLIVED", None, 0);
    never.expires_at = None;
    h.store.save("never", &never).await.unwrap();
    assert_eq!(h.service.get_valid_token("t-code", "never").await.unwrap().access_token.expose(), "LONGLIVED");

    // force_refresh 无视有效期
    let forced = h.service.force_refresh("t-code", "acct").await.unwrap();
    assert_eq!(forced.access_token.expose(), "ACCESS-2");
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 1);

    assert_eq!(h.service.get_valid_token("t-code", "missing").await.unwrap_err(), OAuthError::NoTokens("t-code".into()));
}

#[tokio::test]
async fn oauth_refresh_supports_json_encoding_and_scope() {
    let h = harness().await;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();
    h.service.get_valid_token("t-json", "acct").await.unwrap();
    let req = h.fake.token_requests.lock().unwrap()[0].clone();
    assert!(req.content_type.starts_with("application/json"));
    assert_eq!(req.fields["grant_type"], "refresh_token");
    assert_eq!(req.fields["scope"], "a b");
}

#[tokio::test]
async fn oauth_refresh_keeps_the_old_refresh_token_when_not_rotated() {
    let h = harness().await;
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::NoRotate;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();
    let fresh = h.service.get_valid_token("t-code", "acct").await.unwrap();
    assert_eq!(fresh.access_token.expose(), "ACCESS-2");
    assert_eq!(fresh.refresh_token.as_ref().unwrap().expose(), "REFRESH-0");
}

#[tokio::test]
async fn oauth_refresh_invalid_grant_requires_reauthorization() {
    let h = harness().await;
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::InvalidGrant;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();
    let err = h.service.get_valid_token("t-code", "acct").await.unwrap_err();
    assert!(matches!(err, OAuthError::ReauthRequired { .. }), "{err:?}");
    assert!(err.requires_reauth());
    assert!(!err.is_retryable());
    // 存储里的旧令牌原样保留，由数据层决定如何标记连接
    assert_eq!(h.store.load("acct").await.unwrap().unwrap().access_token.expose(), "ACCESS-0");
}

#[tokio::test]
async fn oauth_refresh_server_errors_are_retryable_not_reauth() {
    let h = harness().await;
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::ServerError;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();
    let err = h.service.get_valid_token("t-code", "acct").await.unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
    assert!(!err.requires_reauth());
    assert_eq!(h.store.load("acct").await.unwrap().unwrap().access_token.expose(), "ACCESS-0");

    // 服务恢复后下一次调用成功（失败结果没有被缓存）
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::Rotate;
    assert_eq!(h.service.get_valid_token("t-code", "acct").await.unwrap().access_token.expose(), "ACCESS-3");

    // 网络不可达同样可重试
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let catalog = OAuthCatalog::from_json(&catalog_json(&dead_base)).unwrap();
    let svc = OAuthService::new(catalog, Arc::new(NoClientCredentials), h.store.clone(), fast_tuning()).unwrap();
    h.store.save("acct2", &token_set("A", Some("R"), 10)).await.unwrap();
    assert!(svc.get_valid_token("t-code", "acct2").await.unwrap_err().is_retryable());
}

#[tokio::test]
async fn oauth_refresh_without_refresh_token_or_support_needs_reauth_without_http() {
    let h = harness().await;
    h.store.save("no-rt", &token_set("A", None, 10)).await.unwrap();
    assert!(matches!(
        h.service.get_valid_token("t-code", "no-rt").await.unwrap_err(),
        OAuthError::ReauthRequired { .. }
    ));
    h.store.save("unsupported", &token_set("A", Some("R"), 10)).await.unwrap();
    assert!(matches!(
        h.service.get_valid_token("t-norefresh", "unsupported").await.unwrap_err(),
        OAuthError::ReauthRequired { .. }
    ));
    assert!(h.fake.token_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn oauth_refresh_sends_the_user_supplied_client_secret() {
    let h = harness().await;
    *h.fake.expect_secret.lock().unwrap() = Some("USER-SECRET-VALUE".into());
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();

    // 没有凭据 -> MissingClientCredential，且不发请求
    let err = h.service.get_valid_token("t-secret", "acct").await.unwrap_err();
    assert!(matches!(err, OAuthError::MissingClientCredential { .. }), "{err:?}");
    assert!(h.fake.token_requests.lock().unwrap().is_empty());

    h.creds.set(
        "t-secret",
        ClientCredentials {
            client_id: Some("user-cid".into()),
            client_secret: Some(Secret::new("USER-SECRET-VALUE")),
            base_url: None,
        },
    );
    h.service.get_valid_token("t-secret", "acct").await.unwrap();
    let req = h.fake.token_requests.lock().unwrap()[0].clone();
    assert_eq!(req.fields["client_secret"], "USER-SECRET-VALUE");
    assert_eq!(req.fields["client_id"], "user-cid");
}

#[tokio::test]
async fn oauth_concurrent_refreshes_of_one_account_hit_upstream_once() {
    let h = harness().await;
    h.fake.refresh_delay_ms.store(200, Ordering::SeqCst);
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();

    let mut tasks = Vec::new();
    for _ in 0..24 {
        let service = h.service.clone();
        tasks.push(tokio::spawn(async move { service.get_valid_token("t-code", "acct").await }));
    }
    for task in tasks {
        let tokens = task.await.unwrap().expect("every caller gets the refreshed token");
        assert_eq!(tokens.access_token.expose(), "ACCESS-2");
        assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "REFRESH-2");
    }
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 1, "single-flight: exactly one upstream refresh");
    assert_eq!(h.store.load("acct").await.unwrap().unwrap().access_token.expose(), "ACCESS-2");
}

#[tokio::test]
async fn oauth_concurrent_failed_refreshes_also_hit_upstream_once() {
    let h = harness().await;
    h.fake.refresh_delay_ms.store(200, Ordering::SeqCst);
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::InvalidGrant;
    h.store.save("acct", &token_set("ACCESS-0", Some("REFRESH-0"), 10)).await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let service = h.service.clone();
        tasks.push(tokio::spawn(async move { service.get_valid_token("t-code", "acct").await }));
    }
    for task in tasks {
        assert!(matches!(task.await.unwrap().unwrap_err(), OAuthError::ReauthRequired { .. }));
    }
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oauth_different_accounts_refresh_independently() {
    let h = harness().await;
    h.fake.refresh_delay_ms.store(100, Ordering::SeqCst);
    // 两个账户共用同一假上游的 REFRESH-0 会互相轮换冲突，这里用 NoRotate 让两者都成功
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::NoRotate;
    h.store.save("a1", &token_set("A", Some("REFRESH-0"), 10)).await.unwrap();
    h.store.save("a2", &token_set("B", Some("REFRESH-0"), 10)).await.unwrap();
    let (s1, s2) = (h.service.clone(), h.service.clone());
    let (r1, r2) = tokio::join!(s1.get_valid_token("t-code", "a1"), s2.get_valid_token("t-code", "a2"));
    assert!(r1.is_ok() && r2.is_ok());
    assert_eq!(h.fake.refresh_count.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// 机密不外泄
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_error_strings_never_contain_tokens_codes_or_verifiers() {
    let h = harness().await;

    // 令牌端点把授权码与 verifier 回显进 error_description
    *h.fake.echo_exchange_errors.lock().unwrap() = true;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let session_debug = format!("{session:?}");
    let url = session.authorize_url.clone();
    let state = session.state.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    let err = session.wait().await.unwrap_err();
    let req = h.fake.token_requests.lock().unwrap()[0].clone();
    let code = req.fields["code"].clone();
    let verifier = req.fields["code_verifier"].clone();
    let rendered = format!("{err} | {err:?}");
    assert_no_leak(&rendered, &[&code, &verifier]);
    assert_no_leak(&session_debug, &[&state, &code, &verifier]);
    match &err {
        OAuthError::TokenEndpoint { code: c, description, .. } => {
            assert_eq!(c, "invalid_grant");
            assert!(description.as_deref().unwrap_or("").contains("[redacted]"), "{description:?}");
        }
        other => panic!("unexpected {other:?}"),
    }

    // 刷新端点回显 refresh token
    *h.fake.refresh_mode.lock().unwrap() = RefreshMode::EchoToken;
    h.store.save("acct", &token_set("ACCESS-SECRET-0", Some("REFRESH-SECRET-0"), 10)).await.unwrap();
    let err = h.service.get_valid_token("t-code", "acct").await.unwrap_err();
    assert_no_leak(&format!("{err} | {err:?}"), &["REFRESH-SECRET-0", "ACCESS-SECRET-0"]);
    assert!(matches!(err, OAuthError::TokenEndpoint { .. }));
}

#[tokio::test]
async fn oauth_token_set_debug_is_redacted_after_a_real_flow() {
    let h = harness().await;
    let session = h.service.start_authorization("t-code").await.unwrap();
    let url = session.authorize_url.clone();
    tokio::spawn(async move { browser().get(url).send().await.unwrap() });
    let tokens = session.wait().await.unwrap();
    assert_no_leak(&format!("{tokens:?}"), &["ACCESS-1", "REFRESH-1"]);
    let stored = tokens.to_storage_json().unwrap();
    assert_no_leak(&format!("{stored:?}"), &["ACCESS-1", "REFRESH-1"]);
    // 存储 JSON 可往返
    let back = TokenSet::from_storage_json(stored.expose()).unwrap();
    assert_eq!(back.access_token.expose(), "ACCESS-1");
}

#[tokio::test]
async fn oauth_refuses_insecure_remote_endpoints() {
    let insecure = r#"{"schemaVersion":1,"providers":[{"id":"x","flow":"device","deviceCodeUrl":"http://remote.example/d","tokenUrl":"https://remote.example/t"}]}"#;
    assert!(OAuthCatalog::from_json(insecure).is_err());
}

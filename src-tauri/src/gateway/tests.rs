//! 网关测试：在随机端口上启动真实服务，用 reqwest 发真实 HTTP 请求。
//! KeyVerifier 使用内存实现。

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

use super::server::{build_router_with, AppState};
use super::*;

const KEY_ALL: &str = "rk-live-VALID-SECRET-0001";
const KEY_OPENAI_ONLY: &str = "rk-live-OPENAI-ONLY-0002";
const KEY_LIMITED: &str = "rk-live-LIMITED-0003";
const KEY_NO_PROTOCOLS: &str = "rk-live-NO-PROTOCOLS-0004";

const NOT_IMPLEMENTED_PHRASE: &str = "该协议的上游适配器尚未接入";

struct MemVerifier {
    keys: HashMap<String, VerifiedKey>,
}

impl KeyVerifier for MemVerifier {
    fn verify(&self, bearer: String) -> VerifyFuture {
        let found = self.keys.get(&bearer).cloned();
        Box::pin(async move { found })
    }
}

fn vkey(id: &str, protocols: &[&str], rpm: Option<u32>) -> VerifiedKey {
    VerifiedKey {
        id: id.into(),
        allowed_protocols: protocols.iter().map(|s| s.to_string()).collect(),
        allowed_models: vec![],
        rpm_limit: rpm,
        tpm_limit: None,
    }
}

fn verifier() -> Arc<dyn KeyVerifier> {
    let all = ["openai-chat", "openai-responses", "anthropic-messages", "gemini"];
    let keys = HashMap::from([
        (KEY_ALL.to_string(), vkey("k-all", &all, None)),
        (KEY_OPENAI_ONLY.to_string(), vkey("k-openai", &["openai-chat"], None)),
        (KEY_LIMITED.to_string(), vkey("k-limited", &all, Some(3))),
        (KEY_NO_PROTOCOLS.to_string(), vkey("k-none", &[], None)),
    ]);
    Arc::new(MemVerifier { keys })
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start_default() -> (GatewayManager, String) {
    start_with(GatewayConfig::default()).await
}

async fn start_with(config: GatewayConfig) -> (GatewayManager, String) {
    capture_logs();
    let mgr = GatewayManager::new();
    let st = mgr.start_with_config("127.0.0.1", 0, verifier(), config).await.expect("gateway should start");
    assert!(st.running);
    assert_ne!(st.port, 0, "status must report the real port");
    (mgr, format!("http://127.0.0.1:{}", st.port))
}

async fn json_body(resp: reqwest::Response) -> Value {
    resp.json().await.expect("error responses must be JSON")
}

fn bearer(key: &str) -> (String, String) {
    ("authorization".into(), format!("Bearer {key}"))
}

async fn post(base: &str, path: &str, headers: &[(String, String)], body: &str) -> reqwest::Response {
    let mut rb = client().post(format!("{base}{path}")).body(body.to_string());
    rb = rb.header("content-type", "application/json");
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    rb.send().await.unwrap()
}

fn assert_openai_error(body: &Value, ty: &str) {
    assert_eq!(body["error"]["type"], ty, "{body}");
    assert!(body["error"]["message"].as_str().is_some_and(|m| !m.is_empty()), "{body}");
    assert!(body.get("type").is_none(), "OpenAI-style bodies have no top-level `type`: {body}");
}

fn assert_anthropic_error(body: &Value, ty: &str) {
    assert_eq!(body["type"], "error", "{body}");
    assert_eq!(body["error"]["type"], ty, "{body}");
    assert!(body["error"]["message"].as_str().is_some_and(|m| !m.is_empty()), "{body}");
}

// ───────────── 基础路由 ─────────────

#[tokio::test]
async fn health_needs_no_auth_and_reports_version_only() {
    let (mgr, base) = start_default().await;
    let resp = client().get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().contains_key("x-request-id"));
    assert_eq!(json_body(resp).await, json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}));

    // 带了错误凭据也不影响 /health
    let resp = client().get(format!("{base}/health")).header("authorization", "Bearer nonsense").send().await.unwrap();
    assert_eq!(resp.status(), 200);
    mgr.stop().await;
}

#[tokio::test]
async fn models_requires_auth_and_returns_empty_list() {
    let (mgr, base) = start_default().await;
    let resp = client().get(format!("{base}/v1/models")).send().await.unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client().get(format!("{base}/v1/models")).header("authorization", format!("Bearer {KEY_ALL}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(json_body(resp).await, json!({"object": "list", "data": []}));

    // Anthropic 客户端用 x-api-key 也可以
    let resp = client().get(format!("{base}/v1/models")).header("x-api-key", KEY_OPENAI_ONLY).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    mgr.stop().await;
}

#[tokio::test]
async fn unknown_route_and_wrong_method_return_json_errors() {
    let (mgr, base) = start_default().await;
    let resp = client().get(format!("{base}/nope")).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert_openai_error(&json_body(resp).await, "invalid_request_error");

    let resp = client()
        .get(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {KEY_ALL}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405);
    assert!(resp.headers()["content-type"].to_str().unwrap().contains("json"));
    assert_openai_error(&json_body(resp).await, "invalid_request_error");

    let resp = client().post(format!("{base}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 405);
    mgr.stop().await;
}

// ───────────── 认证 ─────────────

#[tokio::test]
async fn missing_key_is_401_in_each_protocols_style() {
    let (mgr, base) = start_default().await;
    for path in ["/v1/chat/completions", "/v1/responses"] {
        let resp = post(&base, path, &[], "{}").await;
        assert_eq!(resp.status(), 401, "{path}");
        assert_eq!(resp.headers()["www-authenticate"], "Bearer");
        assert_openai_error(&json_body(resp).await, "authentication_error");
    }
    let resp = post(&base, "/v1/messages", &[], "{}").await;
    assert_eq!(resp.status(), 401);
    assert_anthropic_error(&json_body(resp).await, "authentication_error");
    mgr.stop().await;
}

#[tokio::test]
async fn wrong_key_is_401_via_bearer_and_x_api_key() {
    let (mgr, base) = start_default().await;
    let wrong = "rk-live-WRONG-KEY-9999";

    let resp = post(&base, "/v1/chat/completions", &[bearer(wrong)], "{}").await;
    assert_eq!(resp.status(), 401);
    let text = resp.text().await.unwrap();
    assert!(!text.contains(wrong), "error body must not echo the submitted key");
    assert_openai_error(&serde_json::from_str(&text).unwrap(), "authentication_error");

    let resp = post(&base, "/v1/messages", &[("x-api-key".into(), wrong.into())], "{}").await;
    assert_eq!(resp.status(), 401);
    let text = resp.text().await.unwrap();
    assert!(!text.contains(wrong));
    assert_anthropic_error(&serde_json::from_str(&text).unwrap(), "authentication_error");

    // /v1/models 没有协议归属：用 x-api-key / anthropic-version 判定为 Anthropic 风格
    let resp = client()
        .get(format!("{base}/v1/models"))
        .header("x-api-key", wrong)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    assert_anthropic_error(&json_body(resp).await, "authentication_error");
    mgr.stop().await;
}

#[tokio::test]
async fn valid_key_reaches_protocol_routes_which_return_501_not_fake_success() {
    let (mgr, base) = start_default().await;

    for (path, name) in [("/v1/chat/completions", "OpenAI Chat Completions"), ("/v1/responses", "OpenAI Responses")] {
        let resp = post(&base, path, &[bearer(KEY_ALL)], r#"{"model":"m","messages":[]}"#).await;
        assert_eq!(resp.status(), 501, "{path}");
        let body = json_body(resp).await;
        assert_openai_error(&body, "not_implemented_error");
        assert_eq!(body["error"]["code"], "upstream_adapter_not_implemented");
        let msg = body["error"]["message"].as_str().unwrap();
        assert!(msg.contains(NOT_IMPLEMENTED_PHRASE) && msg.contains(name), "{msg}");
        assert!(body.get("choices").is_none() && body.get("output").is_none());
    }

    // Anthropic：x-api-key 与 Bearer 都可以
    for header in [("x-api-key".to_string(), KEY_ALL.to_string()), bearer(KEY_ALL)] {
        let resp = post(&base, "/v1/messages", &[header, ("anthropic-version".into(), "2023-06-01".into())], r#"{"model":"m"}"#).await;
        assert_eq!(resp.status(), 501);
        let body = json_body(resp).await;
        assert_anthropic_error(&body, "api_error");
        assert!(body["error"]["message"].as_str().unwrap().contains(NOT_IMPLEMENTED_PHRASE));
        assert!(body.get("content").is_none());
    }

    // 流式请求同样 501，且不是 SSE
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_ALL)], r#"{"model":"m","stream":true}"#).await;
    assert_eq!(resp.status(), 501);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    mgr.stop().await;
}

// ───────────── 协议授权 ─────────────

#[tokio::test]
async fn protocol_not_allowed_is_403_in_matching_style() {
    let (mgr, base) = start_default().await;

    // 只允许 openai-chat 的密钥
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_OPENAI_ONLY)], "{}").await;
    assert_eq!(resp.status(), 501, "allowed protocol passes authorization");

    let resp = post(&base, "/v1/messages", &[bearer(KEY_OPENAI_ONLY)], "{}").await;
    assert_eq!(resp.status(), 403);
    let body = json_body(resp).await;
    assert_anthropic_error(&body, "permission_error");
    assert!(body["error"]["message"].as_str().unwrap().contains("Anthropic Messages"));

    let resp = post(&base, "/v1/responses", &[bearer(KEY_OPENAI_ONLY)], "{}").await;
    assert_eq!(resp.status(), 403);
    assert_openai_error(&json_body(resp).await, "permission_error");

    // 空的 allowed_protocols 表示不允许任何协议（安全默认值）
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_NO_PROTOCOLS)], "{}").await;
    assert_eq!(resp.status(), 403);
    mgr.stop().await;
}

// ───────────── 限流 ─────────────

#[tokio::test]
async fn rpm_limit_returns_429_with_retry_after_per_key() {
    let (mgr, base) = start_default().await;
    for i in 0..3 {
        let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_LIMITED)], "{}").await;
        assert_eq!(resp.status(), 501, "request {i} should pass the limiter");
    }
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_LIMITED)], "{}").await;
    assert_eq!(resp.status(), 429);
    let retry: u64 = resp.headers()["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&retry), "Retry-After = {retry}");
    let body = json_body(resp).await;
    assert_openai_error(&body, "rate_limit_error");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");

    // 同一把密钥走 Anthropic 协议同样被限流（按 key id 计数），且为 Anthropic 风格
    let resp = post(&base, "/v1/messages", &[bearer(KEY_LIMITED)], "{}").await;
    assert_eq!(resp.status(), 429);
    assert!(resp.headers().contains_key("retry-after"));
    assert_anthropic_error(&json_body(resp).await, "rate_limit_error");

    // 其它密钥不受影响
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_ALL)], "{}").await;
    assert_eq!(resp.status(), 501);
    mgr.stop().await;
}

// ───────────── 请求体大小 / 超时 / 取消 ─────────────

#[tokio::test]
async fn oversized_body_is_413_after_authentication() {
    let (mgr, base) = start_with(GatewayConfig { max_body_bytes: 1024, ..Default::default() }).await;
    let big = "x".repeat(4096);

    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_ALL)], &big).await;
    assert_eq!(resp.status(), 413);
    assert_openai_error(&json_body(resp).await, "invalid_request_error");

    let resp = post(&base, "/v1/messages", &[bearer(KEY_ALL)], &big).await;
    assert_eq!(resp.status(), 413);
    assert_anthropic_error(&json_body(resp).await, "request_too_large");

    // 未通过认证的超大请求先得到 401，而不是泄露限制细节
    let resp = post(&base, "/v1/chat/completions", &[], &big).await;
    assert_eq!(resp.status(), 401);

    // 限额以内正常放行
    let resp = post(&base, "/v1/chat/completions", &[bearer(KEY_ALL)], &"x".repeat(512)).await;
    assert_eq!(resp.status(), 501);
    mgr.stop().await;
}

#[test]
fn default_limits_match_the_spec() {
    let c = GatewayConfig::default();
    assert_eq!(c.max_body_bytes, 32 * 1024 * 1024);
    assert!(c.request_timeout >= Duration::from_secs(60));
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 起一个带额外慢路由 `/v1/slow` 的网关（与正式路由共用同一套中间件）。返回 (base, 处理器是否被 drop 的标志)。
async fn serve_with_slow_route(config: GatewayConfig) -> (String, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    capture_logs();
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = dropped.clone();
    let extra: Router<AppState> = Router::new().route(
        "/v1/slow",
        get(move || {
            let guard = DropFlag(flag.clone());
            async move {
                let _guard = guard;
                tokio::time::sleep(Duration::from_secs(30)).await;
                "too late"
            }
        }),
    );
    let app = build_router_with(AppState::new(verifier(), config), extra);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let join = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, dropped, join)
}

#[tokio::test]
async fn request_timeout_returns_504_and_cancels_the_handler() {
    let (base, dropped, join) =
        serve_with_slow_route(GatewayConfig { request_timeout: Duration::from_millis(150), ..Default::default() }).await;
    let resp = client().get(format!("{base}/v1/slow")).header("authorization", format!("Bearer {KEY_ALL}")).send().await.unwrap();
    assert_eq!(resp.status(), 504);
    assert_openai_error(&json_body(resp).await, "server_error");
    assert!(dropped.load(Ordering::SeqCst), "timed-out handler must be dropped");
    join.abort();
}

#[tokio::test]
async fn client_disconnect_cancels_the_handler() {
    let (base, dropped, join) = serve_with_slow_route(GatewayConfig::default()).await;
    let short = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(200)).build().unwrap();
    let err = short
        .get(format!("{base}/v1/slow"))
        .header("authorization", format!("Bearer {KEY_ALL}"))
        .send()
        .await
        .unwrap_err();
    assert!(err.is_timeout());

    // 客户端已断开：处理器应被 drop（不会继续跑满 30 秒）。
    let mut cancelled = false;
    for _ in 0..40 {
        if dropped.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(cancelled, "handler future was not dropped after the client disconnected");
    join.abort();
}

// ───────────── 生命周期 ─────────────

#[tokio::test]
async fn port_in_use_returns_clear_error_without_panicking() {
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocker.local_addr().unwrap().port();

    let mgr = GatewayManager::new();
    let err = mgr.start("127.0.0.1", port, verifier()).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&port.to_string()), "error should name the port: {msg}");
    assert!(msg.contains("占用"), "{msg}");

    let st = mgr.status().await;
    assert!(!st.running);
    assert_eq!(st.port, port);
    assert!(st.error.as_deref().is_some_and(|e| e.contains(&port.to_string())));

    // 占用解除后可以启动，并清除旧错误
    drop(blocker);
    let st = mgr.start("127.0.0.1", port, verifier()).await.unwrap();
    assert!(st.running && st.port == port && st.error.is_none());
    assert!(mgr.status().await.error.is_none());
    mgr.stop().await;
}

#[tokio::test]
async fn invalid_listen_address_is_an_error_not_a_panic() {
    let mgr = GatewayManager::new();
    assert!(mgr.start("", 0, verifier()).await.is_err());
    assert!(mgr.start("203.0.113.77", 0, verifier()).await.is_err(), "non-local address cannot be bound");
    assert!(!mgr.status().await.running);
}

#[tokio::test]
async fn start_stop_start_cycle_reflects_real_state() {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let mgr = GatewayManager::new();

    let before = mgr.status().await;
    assert!(!before.running);

    let st = mgr.start("127.0.0.1", port, verifier()).await.unwrap();
    assert!(st.running);
    assert_eq!((st.address.as_str(), st.port), ("127.0.0.1", port));
    assert_eq!(client().get(format!("{base}/health")).send().await.unwrap().status(), 200);
    assert!(mgr.status().await.running);

    // 重复启动同一地址 / 端口：幂等
    let again = mgr.start("127.0.0.1", port, verifier()).await.unwrap();
    assert!(again.running && again.port == port);
    // 不同端口：明确报错，原服务不受影响
    let other = free_port();
    let err = mgr.start("127.0.0.1", other, verifier()).await.unwrap_err();
    assert!(err.to_string().contains("已在"), "{err}");
    assert_eq!(client().get(format!("{base}/health")).send().await.unwrap().status(), 200);

    let st = mgr.stop().await;
    assert!(!st.running && st.error.is_none());
    assert!(!mgr.status().await.running);
    assert!(client().get(format!("{base}/health")).send().await.is_err(), "port must be closed after stop");
    // 重复 stop 是空操作
    assert!(!mgr.stop().await.running);

    let st = mgr.start("127.0.0.1", port, verifier()).await.unwrap();
    assert!(st.running);
    assert_eq!(client().get(format!("{base}/health")).send().await.unwrap().status(), 200);
    mgr.stop().await;
}

#[tokio::test]
async fn stop_returns_promptly_with_idle_keepalive_connection() {
    let (mgr, base) = start_with(GatewayConfig { shutdown_grace: Duration::from_millis(300), ..Default::default() }).await;
    let c = client();
    assert_eq!(c.get(format!("{base}/health")).send().await.unwrap().status(), 200);
    let started = std::time::Instant::now();
    mgr.stop().await;
    assert!(started.elapsed() < Duration::from_secs(3));
}

// ───────────── 日志不泄露密钥 / 正文 ─────────────

#[derive(Clone)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 进程级日志捕获（TRACE 级别）。测试并行运行时，线程级 subscriber 会因 tracing 的 callsite 兴趣缓存而
/// 漏事件，所以安装一次全局 subscriber，所有网关测试产生的日志都会进入同一个缓冲区。
pub(super) fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = buf.clone();
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || LogBuf(writer.clone()))
            .try_init();
        buf
    })
    .clone()
}

#[tokio::test]
async fn logs_and_responses_never_contain_keys_or_request_bodies() {
    let buf = capture_logs();
    let (mgr, base) = start_default().await;
    let wrong = "rk-live-WRONG-SECRET-4242";
    let prompt = "PROMPT-SENTINEL-do-not-log-me";
    let body = format!(r#"{{"model":"m","messages":[{{"role":"user","content":"{prompt}"}}]}}"#);

    let mut responses = Vec::new();
    for (path, headers) in [
        ("/v1/chat/completions", vec![bearer(KEY_ALL)]),
        ("/v1/messages", vec![("x-api-key".to_string(), KEY_ALL.to_string())]),
        ("/v1/chat/completions", vec![bearer(wrong)]),
        ("/v1/messages", vec![("x-api-key".to_string(), wrong.to_string())]),
        ("/v1/messages", vec![bearer(KEY_OPENAI_ONLY)]),
        ("/v1/chat/completions", vec![bearer(KEY_NO_PROTOCOLS)]),
    ] {
        let resp = post(&base, path, &headers, &body).await;
        responses.push(resp.text().await.unwrap());
    }
    mgr.stop().await;

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    // 先证明捕获确实生效，再断言不泄露（缓冲区包含并行运行的所有网关测试的日志，检查更严格）。
    assert!(logs.contains("gateway request"), "access log should have been captured");
    assert!(logs.contains("k-all"), "log should identify the key by id");
    for secret in [KEY_ALL, KEY_OPENAI_ONLY, KEY_LIMITED, KEY_NO_PROTOCOLS, wrong, prompt, "Bearer ", "bearer "] {
        assert!(!logs.contains(secret), "logs leaked `{secret}`");
    }
    for (i, text) in responses.iter().enumerate() {
        for secret in [KEY_ALL, KEY_OPENAI_ONLY, KEY_LIMITED, KEY_NO_PROTOCOLS, wrong, prompt] {
            assert!(!text.contains(secret), "response {i} leaked `{secret}`: {text}");
        }
    }
}

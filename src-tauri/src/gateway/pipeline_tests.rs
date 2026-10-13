//! 转发管线端到端测试：真实网关（随机端口）→ 真实出站 HTTP → axum 假上游（随机端口）。
//!
//! 每个测试自己搭建内存数据库、凭据库（MemoryVault）、假上游与网关，互不共享状态。

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Router;
use bytes::Bytes;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use super::tests::capture_logs;
use super::*;
use crate::protocols::sse::parse_sse;
use crate::routing::Runtime;
use crate::security::MemoryVault;
use crate::store;
use crate::upstream::{ClientOptions, UpstreamClient};

const KEY_ALL: &str = "rk-pipe-ALL-KEY-0001";
const UP_KEY_A: &str = "sk-upstream-AAAA-secret-1111";
const UP_KEY_B: &str = "sk-upstream-BBBB-secret-2222";

// ───────────── 假上游 ─────────────

#[derive(Clone, Debug)]
struct Recorded {
    path: String,
    /// 查询串（不含 `?`）。Gemini 的流式请求是 `alt=sse`；任何请求都不应带 `key=`。
    query: Option<String>,
    headers: HashMap<String, String>,
    body: Bytes,
}

impl Recorded {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("upstream received non-JSON body")
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

enum End {
    Finish,
    /// 发完 chunks 后让连接异常中断
    Abort,
    /// 发完 chunks 后永远不再发送
    Hang,
}

enum Reply {
    Json { status: u16, body: Value, headers: Vec<(&'static str, String)>, delay_ms: u64 },
    Raw { status: u16, body: String },
    Sse { chunks: Vec<(String, u64)>, end: End },
    /// 与 `Sse` 相同，但可指定 Content-Type（Ollama 的 NDJSON、Gemini 的 JSON 数组流）。
    Chunks { chunks: Vec<(String, u64)>, end: End, content_type: &'static str },
    Hang,
    Redirect,
}

impl Reply {
    fn json(status: u16, body: Value) -> Reply {
        Reply::Json { status, body, headers: vec![], delay_ms: 0 }
    }
    fn json_with(status: u16, body: Value, headers: Vec<(&'static str, String)>) -> Reply {
        Reply::Json { status, body, headers, delay_ms: 0 }
    }
    fn sse(chunks: Vec<String>) -> Reply {
        Reply::Sse { chunks: chunks.into_iter().map(|c| (c, 0)).collect(), end: End::Finish }
    }
    /// NDJSON 流（Ollama）。
    fn ndjson(chunks: Vec<String>) -> Reply {
        Reply::Chunks { chunks: chunks.into_iter().map(|c| (c, 0)).collect(), end: End::Finish, content_type: "application/x-ndjson" }
    }
}

type Script = Arc<dyn Fn(&Recorded, usize) -> Reply + Send + Sync>;

#[derive(Clone)]
struct Shared {
    script: Script,
    calls: Arc<AtomicUsize>,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    in_flight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    cancelled: Arc<AtomicUsize>,
}

struct Guard {
    shared: Shared,
    completed: bool,
}

impl Guard {
    fn new(shared: &Shared) -> Self {
        let n = shared.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        shared.peak.fetch_max(n, Ordering::SeqCst);
        Guard { shared: shared.clone(), completed: false }
    }
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.shared.in_flight.fetch_sub(1, Ordering::SeqCst);
        if !self.completed {
            self.shared.cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

async fn fake_handler(State(st): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 64 << 20).await.unwrap();
    let headers = parts.headers.iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string())).collect();
    let rec = Recorded { path: parts.uri.path().to_string(), query: parts.uri.query().map(str::to_string), headers, body: bytes };
    let n = st.calls.fetch_add(1, Ordering::SeqCst);
    st.recorded.lock().unwrap().push(rec.clone());
    let reply = (st.script)(&rec, n);
    let guard = Guard::new(&st);
    match reply {
        Reply::Json { status, body, headers, delay_ms } => {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            let mut b = Response::builder().status(status).header("content-type", "application/json");
            for (k, v) in headers {
                b = b.header(k, v);
            }
            let resp = b.body(Body::from(body.to_string())).unwrap();
            guard.complete();
            resp
        }
        Reply::Raw { status, body } => {
            let resp = Response::builder().status(status).header("content-type", "text/html").body(Body::from(body)).unwrap();
            guard.complete();
            resp
        }
        Reply::Redirect => {
            let resp = Response::builder().status(302).header("location", "https://evil.example/steal").body(Body::empty()).unwrap();
            guard.complete();
            resp
        }
        Reply::Hang => {
            let _g = guard;
            std::future::pending::<()>().await;
            unreachable!()
        }
        Reply::Sse { chunks, end } => chunk_response(chunks, end, guard, "text/event-stream"),
        Reply::Chunks { chunks, end, content_type } => chunk_response(chunks, end, guard, content_type),
    }
}

fn chunk_response(chunks: Vec<(String, u64)>, end: End, guard: Guard, content_type: &'static str) -> Response {
    let stream = futures_util::stream::unfold((chunks.into_iter(), end, Some(guard)), |(mut it, end, mut guard)| async move {
        match it.next() {
            Some((data, delay)) => {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Some((Ok::<Bytes, io::Error>(Bytes::from(data)), (it, end, guard)))
            }
            None => match end {
                End::Finish => {
                    if let Some(g) = guard.take() {
                        g.complete();
                    }
                    None
                }
                End::Abort => {
                    // 给客户端留出读完已发数据的时间：RST 会让对端丢弃尚未读取的数据
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Some((Err(io::Error::new(io::ErrorKind::ConnectionReset, "boom")), (it, End::Finish, guard)))
                }
                End::Hang => {
                    std::future::pending::<()>().await;
                    None
                }
            },
        }
    });
    Response::builder().status(200).header("content-type", content_type).body(Body::from_stream(stream)).unwrap()
}

struct FakeUpstream {
    addr: SocketAddr,
    shared: Shared,
    join: tokio::task::JoinHandle<()>,
}

impl FakeUpstream {
    async fn start(script: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static) -> FakeUpstream {
        let shared = Shared {
            script: Arc::new(script),
            calls: Arc::new(AtomicUsize::new(0)),
            recorded: Arc::new(Mutex::new(Vec::new())),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new().fallback(fake_handler).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let join = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FakeUpstream { addr, shared, join }
    }

    /// 自定义提供商的 Base URL（适配器会追加 `/chat/completions` 或 `/messages`）。
    fn base(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
    /// 不带路径的根地址（Gemini 适配器会追加 `/v1beta`，Ollama 追加 `/api/chat`）。
    fn root(&self) -> String {
        format!("http://{}", self.addr)
    }
    fn calls(&self) -> usize {
        self.shared.calls.load(Ordering::SeqCst)
    }
    fn requests(&self) -> Vec<Recorded> {
        self.shared.recorded.lock().unwrap().clone()
    }
    fn last(&self) -> Recorded {
        self.requests().last().cloned().expect("upstream received no request")
    }
    fn peak(&self) -> usize {
        self.shared.peak.load(Ordering::SeqCst)
    }
    async fn wait_cancelled(&self) -> bool {
        for _ in 0..80 {
            if self.shared.cancelled.load(Ordering::SeqCst) > 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }
}

impl Drop for FakeUpstream {
    fn drop(&mut self) {
        self.join.abort();
    }
}

// ───────────── 数据库 / 网关搭建 ─────────────

struct ProviderSpec {
    id: &'static str,
    adapter: &'static str,
    base_url: String,
    round_robin: bool,
    max_concurrency: i64,
    max_retries: i64,
}

impl ProviderSpec {
    fn new(id: &'static str, adapter: &'static str, base_url: String) -> Self {
        Self { id, adapter, base_url, round_robin: false, max_concurrency: 8, max_retries: 2 }
    }
}

async fn add_provider(db: &SqlitePool, p: &ProviderSpec) {
    sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, protocols_json, enabled, sort_order, \
            round_robin, max_concurrency, max_retries, created_at, updated_at, base_url) \
         VALUES (?1, 'custom', ?2, 'custom', ?3, 1, '[]', 1, 0, ?4, ?5, ?6, 't', 't', ?7)",
    )
    .bind(p.id)
    .bind(format!("Provider {}", p.id))
    .bind(p.adapter)
    .bind(p.round_robin)
    .bind(p.max_concurrency)
    .bind(p.max_retries)
    .bind(&p.base_url)
    .execute(db)
    .await
    .unwrap();
}

async fn add_conn(db: &SqlitePool, vault: &MemoryVault, provider: &str, id: &str, priority: i64, key: Option<&str>) {
    use crate::security::vault::CredentialVault;
    let cref = key.map(|k| {
        let r = format!("cred/{id}");
        vault.set(&r, k).unwrap();
        r
    });
    sqlx::query(
        "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref, created_at, updated_at) \
         VALUES (?1, ?2, ?1, 'apiKey', 'active', ?3, 1, 1, ?4, 't', 't')",
    )
    .bind(id)
    .bind(provider)
    .bind(priority)
    .bind(cref)
    .execute(db)
    .await
    .unwrap();
}

async fn add_model(db: &SqlitePool, provider: &str, upstream: &str, alias: &str) {
    sqlx::query(
        "INSERT INTO models (id, provider_id, upstream_model_id, public_alias, enabled, position) VALUES (?1, ?2, ?3, ?4, 1, 0)",
    )
    .bind(format!("{provider}/{upstream}"))
    .bind(provider)
    .bind(upstream)
    .bind(alias)
    .execute(db)
    .await
    .unwrap();
}

async fn add_header(db: &SqlitePool, vault: &MemoryVault, provider: &str, name: &str, value: &str, secret: bool) {
    use crate::security::vault::CredentialVault;
    let cref = secret.then(|| {
        let r = format!("hdr/{provider}/{name}");
        vault.set(&r, value).unwrap();
        r
    });
    sqlx::query("INSERT INTO provider_headers (provider_id, header_key, value, secret, credential_ref, position) VALUES (?1, ?2, ?3, ?4, ?5, 0)")
        .bind(provider)
        .bind(name)
        .bind((!secret).then_some(value))
        .bind(secret)
        .bind(cref)
        .execute(db)
        .await
        .unwrap();
}

struct MemVerifier(HashMap<String, VerifiedKey>);

impl KeyVerifier for MemVerifier {
    fn verify(&self, bearer: String) -> VerifyFuture {
        let found = self.0.get(&bearer).cloned();
        Box::pin(async move { found })
    }
}

fn vkey(id: &str, allowed_models: &[&str]) -> VerifiedKey {
    VerifiedKey {
        id: id.into(),
        allowed_protocols: ["openai-chat", "openai-responses", "anthropic-messages", "gemini", "ollama"].map(String::from).to_vec(),
        allowed_models: allowed_models.iter().map(|s| s.to_string()).collect(),
        rpm_limit: None,
        tpm_limit: None,
    }
}

struct Harness {
    mgr: GatewayManager,
    base: String,
    db: SqlitePool,
    vault: Arc<MemoryVault>,
    runtime: Arc<Runtime>,
}

impl Harness {
    async fn new() -> Harness {
        Self::with_keys(vec![(KEY_ALL, vkey("k-all", &[]))]).await
    }

    async fn with_keys(keys: Vec<(&str, VerifiedKey)>) -> Harness {
        capture_logs();
        let db = store::open_memory().await.unwrap();
        let vault = Arc::new(MemoryVault::default());
        let client = UpstreamClient::with_options(ClientOptions { no_proxy: true, ..Default::default() }).unwrap();
        let runtime = Arc::new(Runtime::with_client(db.clone(), vault.clone(), client));
        let verifier = Arc::new(MemVerifier(keys.into_iter().map(|(k, v)| (k.to_string(), v)).collect()));
        let mgr = GatewayManager::with_runtime(runtime.clone());
        let st = mgr.start("127.0.0.1", 0, verifier).await.expect("gateway should start");
        Harness { mgr, base: format!("http://127.0.0.1:{}", st.port), db, vault, runtime }
    }

    /// 一个提供商 + 一个账号 + 一个模型的最常见搭配。
    async fn simple(&self, ups: &FakeUpstream, id: &'static str, adapter: &'static str, upstream_model: &str, alias: &str) {
        // Gemini / Vertex / Ollama 的适配器自己追加 `/v1beta` · `/v1` · `/api/chat`，base 只需要主机
        let base = if matches!(adapter, "gemini" | "vertex" | "ollama") { ups.root() } else { ups.base() };
        add_provider(&self.db, &ProviderSpec::new(id, adapter, base)).await;
        add_conn(&self.db, &self.vault, id, &format!("{id}-c1"), 0, Some(UP_KEY_A)).await;
        add_model(&self.db, id, upstream_model, alias).await;
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(15)).build().unwrap()
    }

    async fn chat(&self, body: Value) -> reqwest::Response {
        self.client()
            .post(format!("{}/v1/chat/completions", self.base))
            .bearer_auth(KEY_ALL)
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    async fn messages(&self, body: Value) -> reqwest::Response {
        self.messages_with(body, &[]).await
    }

    async fn messages_with(&self, body: Value, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut rb = self.client().post(format!("{}/v1/messages", self.base)).header("x-api-key", KEY_ALL).json(&body);
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.send().await.unwrap()
    }

    /// `POST /v1/responses`（OpenAI Responses）。
    async fn responses(&self, body: Value) -> reqwest::Response {
        self.client().post(format!("{}/v1/responses", self.base)).bearer_auth(KEY_ALL).json(&body).send().await.unwrap()
    }

    /// `POST /api/chat`（Ollama，Bearer 认证）。
    async fn ollama(&self, body: Value) -> reqwest::Response {
        self.client().post(format!("{}/api/chat", self.base)).bearer_auth(KEY_ALL).json(&body).send().await.unwrap()
    }

    /// Gemini 请求（`x-goog-api-key` 认证）。`target` 是 `models/` 之后的部分，如 `p1/gem:generateContent?alt=sse`。
    fn gemini_req(&self, target: &str, body: &Value) -> reqwest::RequestBuilder {
        self.client().post(format!("{}/v1beta/models/{target}", self.base)).header("x-goog-api-key", KEY_ALL).json(body)
    }

    /// 非流式 `generateContent`。
    async fn gemini(&self, model: &str, body: Value) -> reqwest::Response {
        self.gemini_req(&format!("{model}:generateContent"), &body).send().await.unwrap()
    }

    /// `streamGenerateContent?alt=sse`。
    async fn gemini_sse(&self, model: &str, body: Value) -> reqwest::Response {
        self.gemini_req(&format!("{model}:streamGenerateContent?alt=sse"), &body).send().await.unwrap()
    }

    /// `streamGenerateContent`（不带 `alt=sse`：JSON 数组流）。
    async fn gemini_array(&self, model: &str, body: Value) -> reqwest::Response {
        self.gemini_req(&format!("{model}:streamGenerateContent"), &body).send().await.unwrap()
    }

    async fn stop(self) {
        self.mgr.stop().await;
    }

    /// 等用量记录写入（写库是异步的）并返回。
    async fn usage(&self, expected: usize) -> Vec<UsageRow> {
        let mut rows = Vec::new();
        for _ in 0..100 {
            rows = sqlx::query_as::<_, UsageRow>(
                "SELECT api_key_id, provider_id, connection_id, model_id, protocol, status, input_tokens, output_tokens, \
                        cached_tokens, token_source, estimated_cost FROM usage_requests ORDER BY created_at, id",
            )
            .fetch_all(&self.db)
            .await
            .unwrap();
            if rows.len() >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(rows.len(), expected, "usage rows: {rows:?}");
        rows
    }

    async fn conn_status(&self, id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM connections WHERE id = ?1").bind(id).fetch_one(&self.db).await.unwrap()
    }
}

#[derive(Debug, sqlx::FromRow)]
struct UsageRow {
    api_key_id: Option<String>,
    provider_id: String,
    connection_id: String,
    model_id: String,
    protocol: String,
    status: String,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    token_source: String,
    estimated_cost: f64,
}

fn expected_ua() -> String {
    format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION"))
}

// ───────────── 响应夹具 ─────────────

fn oa_completion(text: &str) -> Value {
    json!({
        "id": "chatcmpl-up-1", "object": "chat.completion", "created": 1_700_000_000, "model": "gpt-up",
        "system_fingerprint": "fp_vendor",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text, "reasoning_content": "step by step"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 21, "completion_tokens": 8, "total_tokens": 29, "prompt_tokens_details": {"cached_tokens": 5}}
    })
}

fn an_message(text: &str) -> Value {
    json!({
        "id": "msg_up_1", "type": "message", "role": "assistant", "model": "claude-up",
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 12, "output_tokens": 6, "cache_read_input_tokens": 4, "cache_creation_input_tokens": 0}
    })
}

fn sse_data(v: &Value) -> String {
    format!("data: {v}\n\n")
}

fn oa_chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"id": "chatcmpl-up-2", "object": "chat.completion.chunk", "created": 1, "model": "gpt-up",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn oa_usage_chunk(prompt: u32, completion: u32) -> Value {
    json!({"id": "chatcmpl-up-2", "object": "chat.completion.chunk", "created": 1, "model": "gpt-up", "choices": [],
           "usage": {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion}})
}

fn an_event(name: &str, v: Value) -> String {
    format!("event: {name}\ndata: {v}\n\n")
}

fn an_text_stream() -> Vec<String> {
    vec![
        an_event("message_start", json!({"type": "message_start", "message": {"id": "msg_s1", "type": "message", "role": "assistant", "model": "claude-up", "content": [], "stop_reason": null, "usage": {"input_tokens": 25, "output_tokens": 1, "cache_read_input_tokens": 10}}})),
        an_event("ping", json!({"type": "ping"})),
        an_event("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello"}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": " world"}})),
        an_event("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        an_event("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null}, "usage": {"output_tokens": 15}})),
        an_event("message_stop", json!({"type": "message_stop"})),
    ]
}

fn user_json_messages() -> Value {
    json!([
        {"role": "system", "content": "You are a geography tutor."},
        {"role": "user", "content": "What is the capital of France? Answer in a sentence."}
    ])
}

// ───────────── 透传 ─────────────

#[tokio::test]
async fn openai_to_openai_passthrough_keeps_unknown_fields_and_records_upstream_usage() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("Paris is the capital of France."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h
        .chat(json!({
            "model": "p1/gpt-up", "messages": user_json_messages(), "temperature": 0.2,
            "logit_bias": {"50256": -100}, "vendor_extension": {"deep": [1, 2, 3]}
        }))
        .await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("x-router-warnings").is_none(), "passthrough has nothing to warn about");
    assert_eq!(resp.json::<Value>().await.unwrap(), oa_completion("Paris is the capital of France."), "body is the upstream body, untouched");

    let up = ups.last();
    assert_eq!(up.path, "/v1/chat/completions");
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()), "outbound User-Agent is fixed");
    let body = up.json();
    assert_eq!(body["model"], "gpt-up", "only the model is rewritten to the upstream id");
    assert_eq!(body["logit_bias"], json!({"50256": -100}));
    assert_eq!(body["vendor_extension"], json!({"deep": [1, 2, 3]}));
    assert_eq!(body["messages"], user_json_messages());
    assert!(body.get("stream_options").is_none(), "no stream_options on a non-stream request");

    let rows = h.usage(1).await;
    let r = &rows[0];
    assert_eq!(r.api_key_id.as_deref(), Some("k-all"));
    assert_eq!((r.provider_id.as_str(), r.connection_id.as_str(), r.model_id.as_str()), ("p1", "p1-c1", "p1/gpt-up"));
    assert_eq!((r.protocol.as_str(), r.status.as_str()), ("openai-chat", "success"));
    assert_eq!((r.input_tokens, r.output_tokens, r.cached_tokens), (21, 8, 5));
    assert_eq!(r.token_source, "upstream");
    assert_eq!(r.estimated_cost, 0.0);
    h.stop().await;
}

#[tokio::test]
async fn openai_stream_passthrough_forwards_chunks_and_injects_usage_only_for_accounting() {
    let chunks = vec![
        sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)),
        sse_data(&oa_chunk(json!({"content": "Par"}), None)),
        sse_data(&oa_chunk(json!({"content": "is."}), None)),
        sse_data(&oa_chunk(json!({}), Some("stop"))),
        sse_data(&oa_usage_chunk(30, 4)),
        "data: [DONE]\n\n".to_string(),
    ];
    let expected_without_usage: String =
        chunks.iter().enumerate().filter(|(i, _)| *i != 4).map(|(_, c)| c.clone()).collect();
    let all: String = chunks.concat();
    let script_chunks = chunks.clone();
    let ups = FakeUpstream::start(move |_, _| {
        // 半包：把每个帧拆成两段发送
        let mut parts = Vec::new();
        for c in &script_chunks {
            let mid = c.len() / 2;
            parts.push((c[..mid].to_string(), 0));
            parts.push((c[mid..].to_string(), 0));
        }
        Reply::Sse { chunks: parts, end: End::Finish }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    // 客户端没有要求 usage：上游多发的 usage chunk 不转给它
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages(), "stream": true})).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    assert_eq!(resp.text().await.unwrap(), expected_without_usage);
    assert_eq!(ups.last().json()["stream_options"], json!({"include_usage": true}), "usage is requested from upstream for accounting");

    // 客户端要求 usage：原样转发
    let resp = h
        .chat(json!({"model": "p1/gpt-up", "messages": user_json_messages(), "stream": true, "stream_options": {"include_usage": true}}))
        .await;
    assert_eq!(resp.text().await.unwrap(), all);

    let rows = h.usage(2).await;
    for r in &rows {
        assert_eq!((r.status.as_str(), r.input_tokens, r.output_tokens, r.token_source.as_str()), ("success", 30, 4, "upstream"));
    }
    h.stop().await;
}

#[tokio::test]
async fn anthropic_to_anthropic_passthrough_nonstream_and_stream() {
    let ups = FakeUpstream::start(|rec, _| {
        if rec.json()["stream"] == true {
            Reply::sse(an_text_stream())
        } else {
            Reply::json(200, an_message("Bonjour"))
        }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let req = json!({
        "model": "an1/claude-up", "max_tokens": 128, "system": "Be brief.",
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Say hello in French"}]}],
        "metadata": {"user_id": "u-1"}, "vendor_flag": true
    });
    let resp = h.messages_with(req.clone(), &[("anthropic-version", "2023-06-01"), ("anthropic-beta", "tools-2024-04-04")]).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap(), an_message("Bonjour"));
    let up = ups.last();
    assert_eq!(up.path, "/v1/messages");
    assert_eq!(up.header("x-api-key"), Some(UP_KEY_A));
    assert!(up.header("authorization").is_none());
    assert_eq!(up.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(up.header("anthropic-beta"), Some("tools-2024-04-04"));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    let ub = up.json();
    assert_eq!(ub["model"], "claude-up");
    assert_eq!(ub["vendor_flag"], true);
    assert_eq!(ub["system"], "Be brief.");

    // 没带版本头时使用默认版本
    let mut streaming = req.clone();
    streaming["stream"] = json!(true);
    let resp = h.messages(streaming).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), an_text_stream().concat(), "stream bytes are forwarded verbatim");
    assert_eq!(ups.last().header("anthropic-version"), Some("2023-06-01"));
    assert!(ups.last().header("anthropic-beta").is_none());

    let rows = h.usage(2).await;
    // Anthropic input_tokens 不含缓存：12 + 4 + 0 = 16（规范化口径：含缓存的总输入）
    assert_eq!((rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens), (16, 6, 4));
    assert_eq!((rows[1].input_tokens, rows[1].output_tokens, rows[1].cached_tokens), (35, 15, 10));
    assert!(rows.iter().all(|r| r.token_source == "upstream" && r.protocol == "anthropic-messages"));
    h.stop().await;
}

// ───────────── 跨协议：Anthropic 请求 → OpenAI 上游 ─────────────

#[tokio::test]
async fn anthropic_request_to_openai_upstream_text_system_and_warnings() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("Paris."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h
        .messages(json!({
            "model": "p1/gpt-up", "max_tokens": 200, "system": "You are a geography tutor.",
            "messages": [{"role": "user", "content": "What is the capital of France?"}],
            "top_k": 5, "mystery_field": 1
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let warnings = resp.headers()["x-router-warnings"].to_str().unwrap().to_string();
    assert!(warnings.contains("top_k") && warnings.contains("mystery_field"), "{warnings}");
    assert!(!warnings.contains("geography") && !warnings.contains("France"), "warnings carry field names only: {warnings}");
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["role"], "assistant");
    assert_eq!(v["content"][0], json!({"type": "thinking", "thinking": "step by step"}));
    assert_eq!(v["content"][1], json!({"type": "text", "text": "Paris."}));
    assert_eq!(v["stop_reason"], "end_turn");
    assert_eq!(v["usage"]["output_tokens"], 8);

    let up = ups.last().json();
    assert_eq!(up["model"], "gpt-up");
    assert_eq!(up["messages"][0], json!({"role": "system", "content": "You are a geography tutor."}));
    assert_eq!(up["messages"][1]["role"], "user");
    assert_eq!(up["max_tokens"], 200);
    assert!(up.get("mystery_field").is_none(), "unknown fields are not blindly copied across protocols");
    assert!(up.get("top_k").is_none());

    let rows = h.usage(1).await;
    assert_eq!((rows[0].protocol.as_str(), rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens), ("anthropic-messages", 21, 8, 5));
    h.stop().await;
}

#[tokio::test]
async fn anthropic_request_to_openai_upstream_multi_turn_tool_chain_and_image() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(
            200,
            json!({"id": "c9", "model": "gpt-up", "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null,
                "tool_calls": [{"id": "call_2", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Lyon\"}"}}]}}],
                "usage": {"prompt_tokens": 90, "completion_tokens": 12}}),
        )
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let resp = h
        .messages(json!({
            "model": "p1/gpt-up", "max_tokens": 300,
            "tools": [{"name": "get_weather", "description": "Weather lookup", "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}],
            "tool_choice": {"type": "auto"},
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "What does this chart show, and what is the weather in Paris?"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": png}}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me check."},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "18C and sunny"},
                    {"type": "text", "text": "Now do Lyon too."}]}
            ]
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["stop_reason"], "tool_use");
    assert_eq!(v["content"][0], json!({"type": "tool_use", "id": "call_2", "name": "get_weather", "input": {"city": "Lyon"}}));

    let up = ups.last().json();
    let msgs = up["messages"].as_array().unwrap();
    // user(image+text) / assistant(tool_calls) / tool / user
    assert_eq!(msgs[0]["role"], "user");
    let parts = msgs[0]["content"].as_array().unwrap();
    assert!(parts.iter().any(|p| p["type"] == "image_url" && p["image_url"]["url"] == format!("data:image/png;base64,{png}")), "{parts:?}");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(serde_json::from_str::<Value>(msgs[1]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Paris"}));
    assert_eq!(msgs[2], json!({"role": "tool", "tool_call_id": "toolu_1", "content": "18C and sunny"}));
    assert_eq!(msgs[3], json!({"role": "user", "content": "Now do Lyon too."}));
    assert_eq!(up["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(up["tool_choice"], "auto");
    h.stop().await;
}

#[tokio::test]
async fn anthropic_request_to_openai_upstream_stream_event_order_and_final_usage() {
    let chunks = vec![
        sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)),
        sse_data(&oa_chunk(json!({"reasoning_content": "Hmm. "}), None)),
        sse_data(&oa_chunk(json!({"content": "It is "}), None)),
        sse_data(&oa_chunk(json!({"content": "Paris."}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "get_weather", "arguments": ""}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"city\":"}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"Paris\"}"}}]}), None)),
        sse_data(&oa_chunk(json!({}), Some("tool_calls"))),
        sse_data(&oa_usage_chunk(40, 17)),
        "data: [DONE]\n\n".to_string(),
    ];
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h
        .messages(json!({"model": "p1/gpt-up", "max_tokens": 100, "stream": true, "messages": [{"role": "user", "content": "capital of France?"}]}))
        .await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let frames = parse_sse(&text);
    let events: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(
        events,
        vec![
            "message_start",
            "content_block_start", "content_block_delta", "content_block_stop", // thinking
            "content_block_start", "content_block_delta", "content_block_delta", "content_block_stop", // text
            "content_block_start", "content_block_delta", "content_block_delta", "content_block_stop", // tool_use
            "message_delta", "message_stop",
        ],
        "{text}"
    );
    let datas: Vec<Value> = frames.iter().map(|f| serde_json::from_str(&f.data).unwrap()).collect();
    assert_eq!(datas[1]["content_block"]["type"], "thinking");
    assert_eq!(datas[2]["delta"], json!({"type": "thinking_delta", "thinking": "Hmm. "}));
    assert_eq!(datas[5]["delta"]["text"], "It is ");
    assert_eq!(datas[8]["content_block"], json!({"type": "tool_use", "id": "call_a", "name": "get_weather", "input": {}}));
    assert_eq!(datas[10]["delta"]["partial_json"], "\"Paris\"}");
    let md = &datas[12];
    assert_eq!(md["delta"]["stop_reason"], "tool_use");
    assert_eq!(md["usage"]["output_tokens"], 17, "final usage arrives in message_delta");

    let upstream_req = ups.last().json();
    assert_eq!(upstream_req["stream"], true);
    assert_eq!(upstream_req["stream_options"], json!({"include_usage": true}));

    let rows = h.usage(1).await;
    assert_eq!((rows[0].status.as_str(), rows[0].input_tokens, rows[0].output_tokens, rows[0].token_source.as_str()), ("success", 40, 17, "upstream"));
    h.stop().await;
}

// ───────────── 跨协议：OpenAI 请求 → Anthropic 上游 ─────────────

#[tokio::test]
async fn openai_request_to_anthropic_upstream_text_system_tools_and_image() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(
            200,
            json!({"id": "msg_9", "type": "message", "role": "assistant", "model": "claude-up",
                   "content": [{"type": "thinking", "thinking": "let me see", "signature": "sig"}, {"type": "text", "text": "Looks like a pixel."},
                               {"type": "tool_use", "id": "toolu_9", "name": "get_weather", "input": {"city": "Nice"}}],
                   "stop_reason": "tool_use", "usage": {"input_tokens": 50, "output_tokens": 20, "cache_read_input_tokens": 10}}),
        )
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    let resp = h
        .chat(json!({
            "model": "an1/claude-up",
            "messages": [
                {"role": "system", "content": "You are terse."},
                {"role": "user", "content": [
                    {"type": "text", "text": "Describe the image and check the weather in Paris."},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{png}")}}]},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "18C and sunny"},
                {"role": "user", "content": "And Nice?"}
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}],
            "frequency_penalty": 0.3
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let warnings = resp.headers()["x-router-warnings"].to_str().unwrap().to_string();
    assert!(warnings.contains("frequency_penalty") && warnings.contains("max_tokens"), "{warnings}");
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(v["choices"][0]["message"]["content"], "Looks like a pixel.");
    assert_eq!(v["choices"][0]["message"]["reasoning_content"], "let me see");
    assert_eq!(v["choices"][0]["message"]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(v["usage"]["prompt_tokens"], 60, "50 + 10 cached");
    assert_eq!(v["usage"]["completion_tokens"], 20);

    let up = ups.last();
    assert_eq!(up.path, "/v1/messages");
    assert_eq!(up.header("x-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("anthropic-version"), Some("2023-06-01"));
    let b = up.json();
    assert_eq!(b["model"], "claude-up");
    assert_eq!(b["system"], "You are terse.");
    assert_eq!(b["max_tokens"], 4096, "Anthropic requires max_tokens; the default is applied and reported");
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m[0]["role"], "user");
    assert!(m[0]["content"].as_array().unwrap().iter().any(|c| c["type"] == "image" && c["source"] == json!({"type": "base64", "media_type": "image/png", "data": png})));
    assert_eq!(m[1]["role"], "assistant");
    assert_eq!(m[1]["content"][0]["type"], "tool_use");
    assert_eq!(m[2]["role"], "user");
    assert_eq!(m[2]["content"][0]["type"], "tool_result", "tool_result comes first in the user turn");
    assert_eq!(m[2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(m[2]["content"][1], json!({"type": "text", "text": "And Nice?"}));
    assert_eq!(b["tools"][0]["name"], "get_weather");
    assert!(b.get("frequency_penalty").is_none());
    h.stop().await;
}

#[tokio::test]
async fn openai_request_to_anthropic_upstream_stream_with_final_usage_chunk() {
    let ups = FakeUpstream::start(|_, _| Reply::sse(an_text_stream())).await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let resp = h
        .chat(json!({"model": "an1/claude-up", "stream": true, "stream_options": {"include_usage": true}, "max_tokens": 50,
                     "messages": [{"role": "user", "content": "say hello world"}]}))
        .await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let frames = parse_sse(&text);
    assert!(frames.iter().all(|f| f.event.is_none()), "OpenAI-style frames have no event names");
    assert_eq!(frames.last().unwrap().data, "[DONE]");
    let chunks: Vec<Value> = frames[..frames.len() - 1].iter().map(|f| serde_json::from_str(&f.data).unwrap()).collect();
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    let content: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["content"].as_str()).collect();
    assert_eq!(content, "Hello world");
    let finish = chunks.iter().find(|c| c["choices"][0]["finish_reason"].is_string()).unwrap();
    assert_eq!(finish["choices"][0]["finish_reason"], "stop");
    let usage = chunks.last().unwrap();
    assert_eq!(usage["choices"], json!([]));
    assert_eq!(usage["usage"]["prompt_tokens"], 35, "25 + 10 cached");
    assert_eq!(usage["usage"]["completion_tokens"], 15);

    let rows = h.usage(1).await;
    assert_eq!((rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens, rows[0].token_source.as_str()), (35, 15, 10, "upstream"));

    // 不要求 usage 时不输出 usage chunk
    let resp = h.chat(json!({"model": "an1/claude-up", "stream": true, "max_tokens": 50, "messages": [{"role": "user", "content": "again"}]})).await;
    let frames = parse_sse(&resp.text().await.unwrap());
    assert!(frames.iter().all(|f| !f.data.contains("\"usage\"")));
    h.stop().await;
}

// ───────────── 错误、冷却与回退 ─────────────

#[tokio::test]
async fn upstream_client_errors_are_returned_faithfully_in_each_protocol_style() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(400, json!({"error": {"message": "max_tokens is too large", "type": "invalid_request_error", "param": "max_tokens", "code": "context_length_exceeded"}}))
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    // 同协议：上游错误体（含 param / code）原样返回
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.headers()["x-router-source"], "upstream");
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["param"], "max_tokens");
    assert_eq!(v["error"]["code"], "context_length_exceeded");
    assert_eq!(ups.calls(), 1, "a request-scoped 4xx is not retried on another account");

    // 跨协议：状态码保持，错误体换成 Anthropic 风格
    let resp = h.messages(json!({"model": "p1/gpt-up", "max_tokens": 10, "messages": [{"role": "user", "content": "x"}]})).await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert_eq!(v["error"]["message"], "max_tokens is too large");

    let rows = h.usage(2).await;
    assert!(rows.iter().all(|r| r.status == "error" && r.input_tokens == 0 && r.output_tokens == 0));
    h.stop().await;
}

#[tokio::test]
async fn upstream_5xx_is_reported_with_its_status_and_html_bodies_are_not_echoed() {
    let ups = FakeUpstream::start(|_, _| Reply::Raw { status: 502, body: "<html><body>Bad gateway from the CDN</body></html>".into() }).await;
    let h = Harness::new().await;
    {
        let mut p = ProviderSpec::new("p1", "openai-compatible", ups.base());
        p.max_retries = 1;
        add_provider(&h.db, &p).await;
        add_conn(&h.db, &h.vault, "p1", "c1", 0, Some(UP_KEY_A)).await;
        add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
    }
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 502);
    let text = resp.text().await.unwrap();
    assert!(!text.contains("<html>"), "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["error"]["message"], "The upstream provider returned HTTP 502.");
    assert_eq!(ups.calls(), 2, "maxRetries=1 means two attempts in total, even with a single account");
    h.stop().await;
}

#[tokio::test]
async fn upstream_error_bodies_never_echo_our_credentials() {
    let leak = format!("Incorrect API key provided: {UP_KEY_A}. See https://example/keys");
    let ups = FakeUpstream::start(move |_, _| Reply::json(422, json!({"error": {"message": leak.clone(), "type": "invalid_request_error"}}))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    for resp in [
        h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await,
        h.messages(json!({"model": "p1/gpt-up", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}]})).await,
    ] {
        assert_eq!(resp.status(), 422);
        let text = resp.text().await.unwrap();
        assert!(!text.contains(UP_KEY_A), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }
    h.stop().await;
}

#[tokio::test]
async fn rate_limited_account_cools_down_and_the_next_account_serves_the_request() {
    // 单个假上游按 Authorization 区分账号：a 第一次 429（Retry-After: 1），之后成功；b 一直成功
    let up = FakeUpstream::start(|rec, n| {
        let key = rec.header("authorization").unwrap_or("");
        if key.ends_with(UP_KEY_A) {
            if n == 0 {
                Reply::json_with(429, json!({"error": {"message": "slow down", "type": "rate_limit_error"}}), vec![("retry-after", "1".into())])
            } else {
                Reply::json(200, oa_completion("from a"))
            }
        } else {
            Reply::json(200, oa_completion("from b"))
        }
    })
    .await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", up.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 1, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;

    let body = json!({"model": "p1/gpt-up", "messages": user_json_messages()});
    let resp = h.chat(body.clone()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "from b");
    let keys: Vec<String> = up.requests().iter().map(|r| r.header("authorization").unwrap().to_string()).collect();
    assert_eq!(keys, vec![format!("Bearer {UP_KEY_A}"), format!("Bearer {UP_KEY_B}")], "a was tried first, 429, then b");
    assert!(matches!(h.runtime.health().availability("a"), crate::routing::health::Availability::Cooling(_)));

    // 冷却期内 a 被完全跳过
    let resp = h.chat(body.clone()).await;
    assert_eq!(resp.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "from b");
    assert_eq!(up.calls(), 3);
    assert_eq!(up.last().header("authorization"), Some(format!("Bearer {UP_KEY_B}").as_str()));

    // 冷却结束后 a 恢复
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let resp = h.chat(body).await;
    assert_eq!(resp.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "from a");
    assert_eq!(h.conn_status("a").await, "active", "a 429 does not mark the account as broken");

    let rows = h.usage(3).await;
    assert_eq!(rows.iter().map(|r| r.connection_id.as_str()).collect::<Vec<_>>(), vec!["b", "b", "a"], "usage is attributed to the account that answered");
    h.stop().await;
}

#[tokio::test]
async fn all_accounts_failing_returns_the_last_upstream_error_then_503_while_cooling() {
    let up = FakeUpstream::start(|_, _| {
        Reply::json_with(429, json!({"error": {"message": "quota exhausted", "type": "rate_limit_error"}}), vec![("retry-after", "30".into())])
    })
    .await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", up.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 1, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;

    // 两个账号都 429：如实把最后一个上游错误（含 Retry-After）交给客户端
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 429);
    assert_eq!(resp.headers()["retry-after"], "30");
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["message"], "quota exhausted");
    assert_eq!(up.calls(), 2);

    // 之后全部账号都在冷却：不再打上游，直接 503（协议风格）并带 Retry-After
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 503);
    assert!(resp.headers().contains_key("retry-after"));
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "no_available_connection");
    assert!(v["error"]["message"].as_str().unwrap().contains("cooling down"));

    let resp = h.messages(json!({"model": "p1/gpt-up", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}]})).await;
    assert_eq!(resp.status(), 503);
    let v: Value = resp.json().await.unwrap();
    assert_eq!((v["type"].as_str(), v["error"]["type"].as_str()), (Some("error"), Some("api_error")));
    assert_eq!(up.calls(), 2, "cooling accounts are not contacted");

    let rows = h.usage(3).await;
    assert!(rows.iter().all(|r| r.status == "error"));
    h.stop().await;
}

#[tokio::test]
async fn auth_failure_marks_the_account_as_error_and_falls_back() {
    let up = FakeUpstream::start(|rec, _| {
        if rec.header("authorization").unwrap_or("").ends_with(UP_KEY_A) {
            Reply::json(401, json!({"error": {"message": "invalid key", "type": "authentication_error"}}))
        } else {
            Reply::json(200, oa_completion("from b"))
        }
    })
    .await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", up.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 1, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;

    let body = json!({"model": "p1/gpt-up", "messages": user_json_messages()});
    assert_eq!(h.chat(body.clone()).await.status(), 200);
    assert_eq!(h.conn_status("a").await, "error");
    assert_eq!(h.conn_status("b").await, "active");
    let before = up.calls();
    assert_eq!(h.chat(body).await.status(), 200);
    assert_eq!(up.calls(), before + 1, "the broken account is skipped while cooling");
    h.stop().await;
}

#[tokio::test]
async fn retries_follow_max_retries_across_accounts() {
    let make = |max_retries: i64| async move {
        let up = FakeUpstream::start(|rec, _| {
            if rec.header("authorization").unwrap_or("").ends_with("CCCC") {
                Reply::json(200, oa_completion("from c"))
            } else {
                Reply::json(500, json!({"error": {"message": "boom", "type": "server_error"}}))
            }
        })
        .await;
        let h = Harness::new().await;
        let mut p = ProviderSpec::new("p1", "openai-compatible", up.base());
        p.max_retries = max_retries;
        add_provider(&h.db, &p).await;
        add_conn(&h.db, &h.vault, "p1", "a", 0, Some("sk-aaaaaa")).await;
        add_conn(&h.db, &h.vault, "p1", "b", 1, Some("sk-bbbbbb")).await;
        add_conn(&h.db, &h.vault, "p1", "c", 2, Some("sk-CCCC")).await;
        add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
        let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
        let status = resp.status().as_u16();
        let calls = up.calls();
        h.stop().await;
        (status, calls)
    };
    assert_eq!(make(1).await, (500, 2), "two attempts, never reaches c");
    assert_eq!(make(2).await, (200, 3), "three attempts reach c");
    assert_eq!(make(0).await, (500, 1), "maxRetries=0: a single attempt");
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let ups = FakeUpstream::start(|_, _| Reply::Redirect).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 502);
    let text = resp.text().await.unwrap();
    assert!(text.contains("base URL") && !text.contains("evil.example"), "{text}");
    assert_eq!(ups.calls(), 1);
    h.stop().await;
}

#[tokio::test]
async fn missing_credential_is_a_clear_503_and_never_calls_upstream() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("x"))).await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", ups.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, None).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 503);
    assert!(resp.json::<Value>().await.unwrap()["error"]["message"].as_str().unwrap().contains("stored credential"));
    assert_eq!(ups.calls(), 0);
    h.stop().await;
}

#[tokio::test]
async fn custom_headers_are_sent_secret_values_come_from_the_vault_and_user_agent_is_fixed() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("ok"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    add_header(&h.db, &h.vault, "p1", "X-Org", "org-123", false).await;
    add_header(&h.db, &h.vault, "p1", "X-Secret-Token", "super-secret-header-value", true).await;
    add_header(&h.db, &h.vault, "p1", "User-Agent", "Mozilla/5.0 Impostor", false).await;
    let resp = h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 200);
    let up = ups.last();
    assert_eq!(up.header("x-org"), Some("org-123"));
    assert_eq!(up.header("x-secret-token"), Some("super-secret-header-value"));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()), "custom headers cannot override the fixed User-Agent");
    h.stop().await;
}

// ───────────── 流式失败 ─────────────

#[tokio::test]
async fn stream_cut_off_mid_way_sends_a_protocol_style_error_event_and_records_failure() {
    let chunks = vec![
        (sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)), 0),
        (sse_data(&oa_chunk(json!({"content": "partial answer"}), None)), 0),
    ];
    let ups = FakeUpstream::start(move |_, _| Reply::Sse { chunks: chunks.clone(), end: End::Abort }).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    // OpenAI 风格客户端（透传）
    let resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    let frames = parse_sse(&text);
    assert!(text.contains("partial answer"), "{text}");
    let last: Value = serde_json::from_str(&frames.last().unwrap().data).unwrap();
    assert_eq!(last["error"]["type"], "server_error", "{text}");
    assert!(!text.contains("[DONE]"), "a failed stream must not look like a clean finish");

    // Anthropic 风格客户端（转换）：error 事件
    let resp = h.messages(json!({"model": "p1/gpt-up", "max_tokens": 50, "stream": true, "messages": [{"role": "user", "content": "x"}]})).await;
    let text = resp.text().await.unwrap();
    let frames = parse_sse(&text);
    let last = frames.last().unwrap();
    assert_eq!(last.event.as_deref(), Some("error"), "{text}");
    assert_eq!(serde_json::from_str::<Value>(&last.data).unwrap()["type"], "error");
    assert!(frames.iter().all(|f| f.event.as_deref() != Some("message_stop")));

    assert_eq!(ups.calls(), 2, "a stream that has started is never retried");
    let rows = h.usage(2).await;
    assert!(rows.iter().all(|r| r.status == "error" && r.connection_id == "p1-c1"), "{rows:?}");
    h.stop().await;
}

#[tokio::test]
async fn stream_that_dies_before_the_first_frame_falls_back_to_the_next_account() {
    let up = FakeUpstream::start(|rec, _| {
        if rec.header("authorization").unwrap_or("").ends_with(UP_KEY_A) {
            Reply::Sse { chunks: vec![], end: End::Abort }
        } else {
            Reply::sse(vec![
                sse_data(&oa_chunk(json!({"role": "assistant", "content": "from b"}), None)),
                sse_data(&oa_chunk(json!({}), Some("stop"))),
                "data: [DONE]\n\n".to_string(),
            ])
        }
    })
    .await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", up.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 1, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
    let resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("from b") && text.ends_with("data: [DONE]\n\n"), "{text}");
    assert_eq!(up.calls(), 2);
    let rows = h.usage(1).await;
    assert_eq!((rows[0].status.as_str(), rows[0].connection_id.as_str()), ("success", "b"));
    h.stop().await;
}

#[tokio::test]
async fn a_200_response_whose_first_frame_is_a_rate_limit_error_is_treated_as_429() {
    let up = FakeUpstream::start(|rec, _| {
        if rec.header("authorization").unwrap_or("").ends_with(UP_KEY_A) {
            Reply::sse(vec![sse_data(&json!({"error": {"message": "too many requests", "type": "rate_limit_error"}}))])
        } else {
            Reply::sse(vec![sse_data(&oa_chunk(json!({"content": "ok"}), Some("stop"))), "data: [DONE]\n\n".into()])
        }
    })
    .await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("p1", "openai-compatible", up.base())).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 1, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
    let resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
    assert!(resp.text().await.unwrap().contains("\"ok\""));
    assert!(matches!(h.runtime.health().availability("a"), crate::routing::health::Availability::Cooling(_)));
    h.stop().await;
}

#[tokio::test]
async fn upstream_rejecting_stream_options_is_retried_without_it_and_remembered() {
    let up = FakeUpstream::start(|rec, _| {
        if rec.json().get("stream_options").is_some() {
            Reply::json(400, json!({"error": {"message": "Unknown parameter: 'stream_options'.", "type": "invalid_request_error"}}))
        } else {
            Reply::sse(vec![sse_data(&oa_chunk(json!({"content": "ok"}), Some("stop"))), "data: [DONE]\n\n".into()])
        }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&up, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    for _ in 0..2 {
        let resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
        assert_eq!(resp.status(), 200);
        assert!(resp.text().await.unwrap().contains("\"ok\""));
    }
    assert_eq!(up.calls(), 3, "first request: rejected + retried; second request: no stream_options at all");
    // 没有 usage → 用量估算并如实标注
    let rows = h.usage(2).await;
    assert!(rows.iter().all(|r| r.status == "success" && r.token_source == "estimated"));
    h.stop().await;
}

// ───────────── 取消传播 ─────────────

#[tokio::test]
async fn client_disconnect_cancels_the_upstream_request_and_records_cancelled() {
    let ups = FakeUpstream::start(|_, _| Reply::Hang).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let short = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(300)).build().unwrap();
    let err = short
        .post(format!("{}/v1/chat/completions", h.base))
        .bearer_auth(KEY_ALL)
        .json(&json!({"model": "p1/gpt-up", "messages": user_json_messages()}))
        .send()
        .await
        .unwrap_err();
    assert!(err.is_timeout());
    assert!(ups.wait_cancelled().await, "the upstream request must be dropped after the client went away");
    let rows = h.usage(1).await;
    assert_eq!(rows[0].status, "cancelled");
    h.stop().await;
}

#[tokio::test]
async fn client_disconnect_during_a_stream_cancels_the_upstream_stream() {
    let ups = FakeUpstream::start(|_, _| Reply::Sse {
        chunks: vec![
            (sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)), 0),
            (sse_data(&oa_chunk(json!({"content": "first words"}), None)), 0),
        ],
        end: End::Hang,
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let mut resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 200);
    let mut seen = String::new();
    while !seen.contains("first words") {
        let chunk = resp.chunk().await.unwrap().expect("stream ended early");
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    drop(resp);
    assert!(ups.wait_cancelled().await, "dropping the client response must close the upstream stream");
    let rows = h.usage(1).await;
    assert_eq!(rows[0].status, "cancelled");
    assert!(rows[0].output_tokens > 0, "partial output is estimated");
    assert_eq!(rows[0].token_source, "estimated");
    h.stop().await;
}

// ───────────── 模型解析 / 白名单 / 并发 ─────────────

#[tokio::test]
async fn ambiguous_model_names_return_400_with_candidates_and_never_pick_silently() {
    let a = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("from a"))).await;
    let b = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("from b"))).await;
    let h = Harness::new().await;
    h.simple(&a, "pa", "openai-compatible", "gpt-4o", "pa/gpt-4o").await;
    h.simple(&b, "pb", "openai-compatible", "gpt-4o", "pb/gpt-4o").await;

    let resp = h.chat(json!({"model": "gpt-4o", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "ambiguous_model");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.contains("pa/gpt-4o") && msg.contains("pb/gpt-4o"), "{msg}");
    assert_eq!((a.calls(), b.calls()), (0, 0));

    // Anthropic 风格
    let resp = h.messages(json!({"model": "gpt-4o", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}]})).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["type"], "invalid_request_error");

    // 显式别名、provider/上游ID 都能确定地命中
    let resp = h.chat(json!({"model": "pb/gpt-4o", "messages": user_json_messages()})).await;
    assert_eq!(resp.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "from b");
    let resp = h.chat(json!({"model": "pa/gpt-4o", "messages": user_json_messages()})).await;
    assert_eq!(resp.json::<Value>().await.unwrap()["choices"][0]["message"]["content"], "from a");

    // 数据库里被写进了重复别名（绕过保存时的校验）：同样是 400，而不是后者覆盖前者
    sqlx::query("UPDATE models SET public_alias = 'shared' WHERE provider_id IN ('pa', 'pb')").execute(&h.db).await.unwrap();
    let resp = h.chat(json!({"model": "shared", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 400);
    let msg = resp.json::<Value>().await.unwrap()["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains("pa/gpt-4o") && msg.contains("pb/gpt-4o"), "{msg}");
    // /v1/models 对重复别名退回完整限定名，保证列出的每个名字都能寻址
    let list: Value = h.client().get(format!("{}/v1/models", h.base)).bearer_auth(KEY_ALL).send().await.unwrap().json().await.unwrap();
    let ids: Vec<&str> = list["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["pa/gpt-4o", "pb/gpt-4o"]);
    h.stop().await;
}

#[tokio::test]
async fn unknown_model_is_404_and_requests_without_model_are_400() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("x"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    let resp = h.chat(json!({"model": "nope", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["code"], "model_not_found");
    assert_eq!(h.chat(json!({"messages": user_json_messages()})).await.status(), 400);
    let resp = h.client().post(format!("{}/v1/chat/completions", h.base)).bearer_auth(KEY_ALL).body("{not json").send().await.unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(ups.calls(), 0);
    h.stop().await;
}

#[tokio::test]
async fn allowed_models_are_enforced_by_alias_and_filter_the_model_list() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("ok"))).await;
    let h = Harness::with_keys(vec![
        (KEY_ALL, vkey("k-all", &[])),
        ("rk-restricted", vkey("k-r", &["p1/gpt-up"])),
        ("rk-wild", vkey("k-w", &["p1/*"])),
    ])
    .await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    add_model(&h.db, "p1", "gpt-secret", "p1/gpt-secret").await;

    let call = |key: &'static str, model: &'static str| {
        let c = h.client();
        let url = format!("{}/v1/chat/completions", h.base);
        async move { c.post(url).bearer_auth(key).json(&json!({"model": model, "messages": [{"role": "user", "content": "q"}]})).send().await.unwrap() }
    };
    assert_eq!(call("rk-restricted", "p1/gpt-up").await.status(), 200);
    let resp = call("rk-restricted", "p1/gpt-secret").await;
    assert_eq!(resp.status(), 403);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["code"], "model_not_allowed");
    assert_eq!(ups.calls(), 1, "blocked requests never reach the upstream");
    assert_eq!(call("rk-restricted", "gpt-secret").await.status(), 403, "the bare upstream id of a blocked model is blocked too");
    assert_eq!(call("rk-wild", "p1/gpt-secret").await.status(), 200);

    let ids = |key: &'static str| {
        let c = h.client();
        let url = format!("{}/v1/models", h.base);
        async move {
            let v: Value = c.get(url).bearer_auth(key).send().await.unwrap().json().await.unwrap();
            v["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(ids("rk-restricted").await, vec!["p1/gpt-up"]);
    assert_eq!(ids(KEY_ALL).await.len(), 2);
    h.stop().await;
}

#[tokio::test]
async fn max_concurrency_limits_in_flight_upstream_requests() {
    let ups = FakeUpstream::start(|_, _| Reply::Json { status: 200, body: oa_completion("ok"), headers: vec![], delay_ms: 250 }).await;
    let h = Harness::new().await;
    let mut p = ProviderSpec::new("p1", "openai-compatible", ups.base());
    p.max_concurrency = 2;
    add_provider(&h.db, &p).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, "p1", "b", 0, Some(UP_KEY_B)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;

    let started = std::time::Instant::now();
    let futs = (0..6).map(|_| h.chat(json!({"model": "p1/gpt-up", "messages": user_json_messages()})));
    let responses = futures_util::future::join_all(futs).await;
    assert!(responses.iter().all(|r| r.status() == 200));
    assert_eq!(ups.calls(), 6);
    assert!(ups.peak() <= 2, "peak in-flight upstream requests was {}", ups.peak());
    assert!(ups.peak() >= 2, "the limit should actually be reached");
    assert!(started.elapsed() >= Duration::from_millis(700), "6 requests at 250ms with 2 slots need at least 3 waves");
    h.stop().await;
}

#[tokio::test]
async fn concurrency_slot_is_held_for_the_whole_stream() {
    let ups = FakeUpstream::start(|_, _| Reply::Sse {
        chunks: vec![
            (sse_data(&oa_chunk(json!({"content": "a"}), None)), 0),
            (sse_data(&oa_chunk(json!({"content": "b"}), None)), 400),
            (sse_data(&oa_chunk(json!({}), Some("stop"))), 0),
            ("data: [DONE]\n\n".to_string(), 0),
        ],
        end: End::Finish,
    })
    .await;
    let h = Harness::new().await;
    let mut p = ProviderSpec::new("p1", "openai-compatible", ups.base());
    p.max_concurrency = 1;
    add_provider(&h.db, &p).await;
    add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
    add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;

    let body = json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()});
    let (r1, r2) = tokio::join!(
        async { h.chat(body.clone()).await.text().await.unwrap() },
        async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            h.chat(body.clone()).await.text().await.unwrap()
        }
    );
    assert!(r1.contains("[DONE]") && r2.contains("[DONE]"));
    assert_eq!(ups.peak(), 1, "the second stream must wait for the first to finish");
    h.stop().await;
}

// ───────────── /v1/models、端点未就绪、501 ─────────────

#[tokio::test]
async fn models_lists_only_enabled_models_of_ready_providers() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("x"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    add_model(&h.db, "p1", "gpt-second", "p1/second").await;
    sqlx::query("UPDATE models SET enabled = 0 WHERE id = 'p1/gpt-second'").execute(&h.db).await.unwrap();
    add_model(&h.db, "p1", "gpt-third", "p1/third").await;

    // 停用的提供商
    add_provider(&h.db, &ProviderSpec::new("off", "openai-compatible", ups.base())).await;
    add_model(&h.db, "off", "m", "off/m").await;
    sqlx::query("UPDATE providers SET enabled = 0 WHERE id = 'off'").execute(&h.db).await.unwrap();
    // 不安全的 base URL：适配器未就绪
    add_provider(&h.db, &ProviderSpec::new("insecure", "openai-compatible", "http://example.com/v1".into())).await;
    add_model(&h.db, "insecure", "m", "insecure/m").await;
    // 内置：claude 待接入（不就绪），gemini / ollama-local 的适配器已接入
    for (id, ready) in [("claude", 0), ("gemini", 1), ("ollama-local", 1)] {
        sqlx::query(
            "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, created_at, updated_at) \
             VALUES (?1, 'builtin', ?1, 'oauth', 'none', ?2, 't', 't')",
        )
        .bind(id)
        .bind(ready)
        .execute(&h.db)
        .await
        .unwrap();
        add_model(&h.db, id, "m", &format!("{id}/m")).await;
    }
    // 另一个就绪的内置提供商（地址来自 provider_status.json，只用于判定就绪，不会真的被调用）
    sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, created_at, updated_at) \
         VALUES ('openai', 'builtin', 'OpenAI', 'apikey', 'none', 1, 't', 't')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    add_model(&h.db, "openai", "gpt-4o", "openai/gpt-4o").await;

    let resp = h.client().get(format!("{}/v1/models", h.base)).bearer_auth(KEY_ALL).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["object"], "list");
    let mut ids: Vec<(String, String)> =
        v["data"].as_array().unwrap().iter().map(|m| (m["id"].as_str().unwrap().into(), m["owned_by"].as_str().unwrap().into())).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            // gemini / ollama-local 的适配器现已接入转发管线
            ("gemini/m".to_string(), "gemini".to_string()),
            ("ollama-local/m".to_string(), "ollama-local".to_string()),
            ("openai/gpt-4o".to_string(), "openai".to_string()),
            ("p1/gpt-up".to_string(), "p1".to_string()),
            ("p1/third".to_string(), "p1".to_string()),
        ]
    );
    assert!(v["data"][0]["object"] == "model");

    // 请求一个适配器未就绪的模型：如实 501，而不是 404 或假装成功
    let resp = h.chat(json!({"model": "claude/m", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 501);
    let msg = resp.json::<Value>().await.unwrap()["error"]["message"].as_str().unwrap().to_string();
    assert!(msg.contains("claude"), "{msg}");
    let resp = h.chat(json!({"model": "insecure/m", "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 501);
    assert!(resp.json::<Value>().await.unwrap()["error"]["message"].as_str().unwrap().contains("https"));
    h.stop().await;
}

#[tokio::test]
async fn responses_endpoint_is_wired_and_reports_unknown_models_in_openai_style() {
    let h = Harness::new().await;
    let resp = h.responses(json!({"model": "nope/x", "input": "hi"})).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "no longer a blanket 501");
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "model_not_found");
    h.stop().await;
}

// ───────────── 日志卫生 ─────────────

#[tokio::test]
async fn logs_never_contain_credentials_headers_or_bodies() {
    let buf = capture_logs();
    let prompt = "PIPELINE-PROMPT-SENTINEL-7731";
    let reply_text = "PIPELINE-REPLY-SENTINEL-9942";
    let image = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo0NDQ0NDQ0NDQ0NDQ0NDQ0NDQ=";
    let ups = FakeUpstream::start(move |rec, n| {
        let _ = rec;
        match n {
            0 => Reply::json(200, oa_completion(reply_text)),
            1 => Reply::json(500, json!({"error": {"message": format!("boom echoing {UP_KEY_A}"), "type": "server_error"}})),
            2 => Reply::Sse { chunks: vec![(sse_data(&oa_chunk(json!({"content": reply_text}), None)), 0)], end: End::Abort },
            _ => Reply::json(401, json!({"error": {"message": "no", "type": "authentication_error"}})),
        }
    })
    .await;
    let h = Harness::new().await;
    {
        let mut p = ProviderSpec::new("p1", "openai-compatible", ups.base());
        p.max_retries = 0;
        add_provider(&h.db, &p).await;
        add_conn(&h.db, &h.vault, "p1", "a", 0, Some(UP_KEY_A)).await;
        add_model(&h.db, "p1", "gpt-up", "p1/gpt-up").await;
        add_header(&h.db, &h.vault, "p1", "X-Secret-Token", "hdr-secret-LOGCHECK-5521", true).await;
    }
    let body = json!({"model": "p1/gpt-up", "messages": [{"role": "user", "content": [
        {"type": "text", "text": prompt},
        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{image}")}}]}]});
    let mut stream_body = body.clone();
    stream_body["stream"] = json!(true);
    let mut bodies = Vec::new();
    bodies.push(h.chat(body.clone()).await.text().await.unwrap());
    bodies.push(h.chat(body.clone()).await.text().await.unwrap());
    bodies.push(h.chat(stream_body).await.text().await.unwrap());
    bodies.push(h.chat(body).await.text().await.unwrap());
    h.stop().await;

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("upstream request completed") || logs.contains("dispatching upstream attempt"), "pipeline logs should have been captured");
    for needle in [
        UP_KEY_A, UP_KEY_B, KEY_ALL, "hdr-secret-LOGCHECK-5521", prompt, reply_text, image, "Bearer ", "bearer ", "authorization", "Authorization",
        "x-api-key",
    ] {
        assert!(!logs.contains(needle), "logs leaked `{needle}`");
    }
    for (i, text) in bodies.iter().enumerate() {
        for needle in [UP_KEY_A, KEY_ALL, "hdr-secret-LOGCHECK-5521"] {
            assert!(!text.contains(needle), "response {i} leaked `{needle}`: {text}");
        }
    }
}

#[tokio::test]
async fn openai_request_to_anthropic_upstream_stream_with_thinking_and_tool_use() {
    let stream = vec![
        an_event("message_start", json!({"type": "message_start", "message": {"id": "msg_t", "model": "claude-up", "usage": {"input_tokens": 70, "output_tokens": 1}}})),
        an_event("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "need weather"}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig=="}})),
        an_event("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        an_event("content_block_start", json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_77", "name": "get_weather", "input": {}}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}})),
        an_event("content_block_delta", json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"Oslo\"}"}})),
        an_event("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
        an_event("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 33}})),
        an_event("message_stop", json!({"type": "message_stop"})),
    ];
    let ups = FakeUpstream::start(move |_, _| Reply::sse(stream.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;
    let resp = h
        .chat(json!({"model": "an1/claude-up", "stream": true, "stream_options": {"include_usage": true}, "max_tokens": 64,
                     "messages": [{"role": "user", "content": "weather in Oslo?"}],
                     "tools": [{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]}))
        .await;
    let frames = parse_sse(&resp.text().await.unwrap());
    let chunks: Vec<Value> = frames.iter().filter(|f| f.data != "[DONE]").map(|f| serde_json::from_str(&f.data).unwrap()).collect();
    let reasoning: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str()).collect();
    assert_eq!(reasoning, "need weather");
    let first_call = chunks.iter().find_map(|c| c["choices"][0]["delta"]["tool_calls"][0].as_object().filter(|o| o.contains_key("id"))).unwrap();
    assert_eq!((first_call["id"].as_str(), first_call["function"]["name"].as_str()), (Some("toolu_77"), Some("get_weather")));
    let args: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()).collect();
    assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), json!({"city": "Oslo"}));
    assert!(chunks.iter().any(|c| c["choices"][0]["finish_reason"] == "tool_calls"));
    assert_eq!(chunks.last().unwrap()["usage"]["prompt_tokens"], 70);
    assert_eq!(chunks.last().unwrap()["usage"]["completion_tokens"], 33);
    assert_eq!(frames.last().unwrap().data, "[DONE]");
    h.stop().await;
}

#[tokio::test]
async fn upstream_stalling_mid_stream_hits_the_idle_timeout_and_reports_an_error() {
    let ups = FakeUpstream::start(|_, _| Reply::Sse {
        chunks: vec![(sse_data(&oa_chunk(json!({"role": "assistant", "content": "start"}), None)), 0)],
        end: End::Hang,
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    sqlx::query("INSERT INTO app_settings (key, value) VALUES ('timeoutSeconds', '1')").execute(&h.db).await.unwrap();

    let started = std::time::Instant::now();
    let resp = h.chat(json!({"model": "p1/gpt-up", "stream": true, "messages": user_json_messages()})).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    let frames = parse_sse(&text);
    let last: Value = serde_json::from_str(&frames.last().unwrap().data).unwrap();
    assert_eq!(last["error"]["type"], "server_error", "{text}");
    let rows = h.usage(1).await;
    assert_eq!(rows[0].status, "error");
    h.stop().await;
}

// ───────────── 新协议（Gemini / Vertex / Responses / Ollama）的端到端测试 ─────────────

mod fixtures;
mod gemini;
mod misc;
mod ollama;
mod responses;
mod vertex;

//! Ollama 入口（`/api/chat`、`/api/tags`）与 Ollama 上游的端到端测试。

use futures_util::StreamExt;

use super::fixtures::*;
use super::*;
use crate::upstream::{AdapterKind, AuthStyle, ResolvedEndpoint, Secret, StreamOpen, UpstreamRequest, UpstreamTarget};

const SYSTEM_PROMPT: &str = "You are a geography tutor.";
const QUESTION: &str = "What is the capital of France? Answer in a sentence.";

// ───────────── 夹具 ─────────────

/// 单轮请求：system + 带图片（裸 base64）的 user。`stream` 为 None 时不写该字段（Ollama 缺省即流式）。
fn ol_user_request(model: &str, stream: Option<bool>) -> Value {
    let mut v = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": QUESTION, "images": [PNG_B64]}
        ],
        "options": {"temperature": 0.2, "num_predict": 128}
    });
    if let Some(s) = stream {
        v["stream"] = json!(s);
    }
    v
}

/// 多轮工具调用链：system / 带图 user / assistant(tool_calls，无 id) / tool(tool_name) / user，外加工具定义与 options。
/// `num_ctx` 是协议层不映射的选项，跨协议时只会进 warnings（字段名）。
fn ol_chain_request(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "messages": [
            {"role": "system", "content": "You are a weather assistant."},
            {"role": "user", "content": "What does this chart show, and what is the weather in Paris?", "images": [PNG_B64]},
            {"role": "assistant", "content": "Let me check.",
             "tool_calls": [{"function": {"name": "get_weather", "arguments": {"city": "Paris"}}}]},
            {"role": "tool", "tool_name": "get_weather", "content": "18C and sunny"},
            {"role": "user", "content": "Now do Lyon too."}
        ],
        "tools": [chat_weather_tool()],
        "options": {"temperature": 0.2, "num_predict": 256, "num_ctx": 4096}
    })
}

/// 把每一行从中间拆成两个 chunk（半包）。
fn halves(lines: &[String]) -> Vec<(String, u64)> {
    let mut parts = Vec::new();
    for l in lines {
        let mid = l.len() / 2;
        parts.push((l[..mid].to_string(), 0));
        parts.push((l[mid..].to_string(), 0));
    }
    parts
}

fn ndjson_lines(values: &[Value]) -> Vec<String> {
    values.iter().map(ol_line).collect()
}

fn error_line(msg: &str) -> String {
    ol_line(&json!({"error": msg}))
}

fn vkey_with_protocols(id: &str, protocols: &[&str]) -> VerifiedKey {
    VerifiedKey { allowed_protocols: protocols.iter().map(|s| s.to_string()).collect(), ..vkey(id, &[]) }
}

async fn post_chat_as(h: &Harness, key: &str, body: &Value) -> reqwest::Response {
    h.client().post(format!("{}/api/chat", h.base)).bearer_auth(key).json(body).send().await.unwrap()
}

/// 响应头 `x-router-warnings` 的值（没有则空串）。
fn warnings_of(resp: &reqwest::Response) -> String {
    resp.headers().get("x-router-warnings").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default()
}

/// 所有 Ollama 风格的错误体都是 `{"error": "<字符串>"}`，且没有别的键。
fn assert_ollama_error(v: &Value, context: &str) {
    assert!(v["error"].is_string() && v.as_object().is_some_and(|o| o.len() == 1), "{context}: not an Ollama-style error: {v}");
}

fn v_keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort();
    k
}

// ───────────── 1. Ollama → Ollama 透传 ─────────────

#[tokio::test]
async fn ollama_passthrough_nonstream_rewrites_only_the_model_and_records_upstream_usage() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, ol_response("Paris is the capital of France."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;

    // 多轮工具链 + 图片 + 工具定义 + 协议层不认识的字段：透传时一个都不能少
    let mut req = ol_chain_request("o1/llama-up", false);
    req["keep_alive"] = json!("5m");
    req["format"] = json!("json");
    req["vendor_extension"] = json!({"deep": [1, 2, 3]});
    let resp = h.ollama(req.clone()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    assert!(resp.headers().get("x-router-warnings").is_none(), "passthrough has nothing to warn about");
    assert_eq!(resp.json::<Value>().await.unwrap(), ol_response("Paris is the capital of France."), "upstream body untouched");

    let up = ups.last();
    assert_eq!(up.path, "/api/chat");
    assert_eq!(up.query, None);
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()), "cloud/custom Ollama uses a Bearer token");
    assert_eq!(up.header("x-api-key"), None);
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    assert_eq!(up.header("accept"), Some("application/json"));
    assert!(!up.path.contains(UP_KEY_A) && up.query.is_none(), "the key never travels in the URL");
    let mut expected = req;
    expected["model"] = json!("llama-up");
    assert_eq!(up.json(), expected, "only the model is rewritten; everything else (images, tool_name, unknown fields) is kept");

    let rows = h.usage(1).await;
    let r = &rows[0];
    assert_eq!(r.api_key_id.as_deref(), Some("k-all"));
    assert_eq!((r.provider_id.as_str(), r.connection_id.as_str(), r.model_id.as_str()), ("o1", "o1-c1", "o1/llama-up"));
    assert_eq!((r.protocol.as_str(), r.status.as_str()), ("ollama", "success"));
    // prompt_eval_count → input、eval_count → output；Ollama 没有缓存拆分
    assert_eq!((r.input_tokens, r.output_tokens, r.cached_tokens), (21, 8, 0));
    assert_eq!(r.token_source, "upstream");
    h.stop().await;
}

#[tokio::test]
async fn ollama_passthrough_stream_is_byte_identical_defaults_to_streaming_and_records_final_usage() {
    let text_lines = ndjson_lines(&ol_text_stream());
    let tool_lines = ndjson_lines(&ol_tool_stream("get_weather", json!({"city": "Oslo"})));
    let (t, w) = (text_lines.clone(), tool_lines.clone());
    let ups = FakeUpstream::start(move |rec, n| {
        let body = rec.json();
        if body["stream"] == false {
            return Reply::json(200, ol_response("not streamed"));
        }
        // 半包：每一行拆成两段发送
        let lines = if n == 0 { &t } else { &w };
        Reply::Chunks { chunks: halves(lines), end: End::Finish, content_type: "application/x-ndjson" }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;

    // 请求里没有 `stream`：Ollama 缺省即流式；网关向上游显式写出 stream:true
    let req = ol_user_request("o1/llama-up", None);
    assert!(req.get("stream").is_none());
    let resp = h.ollama(req).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/x-ndjson"));
    assert_eq!(resp.text().await.unwrap(), text_lines.concat(), "client bytes == upstream bytes");
    let up = ups.last();
    assert_eq!(up.json()["stream"], json!(true), "the default is made explicit upstream");
    assert_eq!(up.json()["model"], "llama-up");
    assert_eq!(up.header("accept"), Some("application/x-ndjson"));
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    assert_eq!(up.path, "/api/chat");
    let rows = h.usage(1).await;
    assert_eq!((rows[0].status.as_str(), rows[0].input_tokens, rows[0].output_tokens, rows[0].token_source.as_str()), ("success", 42, 11, "upstream"));

    // 显式 stream:true + 工具调用整块到达 + 未知字段：同样逐字节转发
    let mut req = ol_chain_request("o1/llama-up", true);
    req["vendor_extension"] = json!(7);
    let resp = h.ollama(req).await;
    assert_eq!(resp.text().await.unwrap(), tool_lines.concat());
    assert_eq!(ups.last().json()["vendor_extension"], 7);
    let rows = h.usage(2).await;
    assert_eq!((rows[1].status.as_str(), rows[1].input_tokens, rows[1].output_tokens), ("success", 50, 14));

    // 显式 stream:false 仍是单个 JSON，上游也收到 false
    let resp = h.ollama(ol_user_request("o1/llama-up", Some(false))).await;
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    assert_eq!(resp.json::<Value>().await.unwrap(), ol_response("not streamed"));
    assert_eq!(ups.last().json()["stream"], json!(false));
    assert_eq!(ups.last().header("accept"), Some("application/json"));
    assert_eq!(ups.calls(), 3);
    h.stop().await;
}

// ───────────── 4. 适配器层：本地无鉴权 / URL 规则 ─────────────

fn adapter_target(ups: &FakeUpstream, auth: AuthStyle, key: Option<&str>) -> UpstreamTarget {
    UpstreamTarget {
        provider_id: "ollama-local".into(),
        connection_id: "local-c1".into(),
        endpoint: ResolvedEndpoint::new(AdapterKind::Ollama, url::Url::parse(&ups.root()).unwrap(), auth),
        api_key: key.map(Secret::new),
        extra_headers: vec![],
        timeout: Duration::from_secs(10),
    }
}

fn adapter_request(stream: bool) -> UpstreamRequest {
    UpstreamRequest {
        body: json!({"model": "llama3:8b", "messages": [{"role": "user", "content": QUESTION}], "stream": stream}),
        model: "llama3:8b".into(),
        stream,
        passthrough: true,
        extra_headers: vec![],
    }
}

#[tokio::test]
async fn local_ollama_without_auth_sends_no_credentials_at_all() {
    let stream_lines = ndjson_lines(&ol_text_stream());
    let sl = stream_lines.clone();
    let ups = FakeUpstream::start(move |rec, _| {
        if rec.json()["stream"] == true {
            Reply::ndjson(sl.clone())
        } else {
            Reply::json(200, ol_response("local ok"))
        }
    })
    .await;
    let client = UpstreamClient::with_options(ClientOptions { no_proxy: true, ..Default::default() }).unwrap();
    let adapter = AdapterKind::Ollama.adapter();
    // 本机 Ollama：AuthStyle::None，没有凭据；即使（误）带着一个密钥也不能发出去
    for key in [None, Some("sk-should-never-be-sent-1234")] {
        let target = adapter_target(&ups, AuthStyle::None, key);

        let reply = adapter.invoke(&client, &target, adapter_request(false)).await.unwrap();
        assert!(reply.is_success());
        assert_eq!(serde_json::from_slice::<Value>(&reply.body).unwrap(), ol_response("local ok"));
        let rec = ups.last();
        assert_eq!(rec.path, "/api/chat");
        assert_eq!(rec.header("accept"), Some("application/json"));
        assert_eq!(rec.header("user-agent"), Some(expected_ua().as_str()));
        assert_eq!(rec.header("content-type"), Some("application/json"));
        for h in ["authorization", "x-api-key", "x-goog-api-key", "api-key", "cookie"] {
            assert_eq!(rec.header(h), None, "{h} must not be sent to a no-auth local Ollama");
        }
        assert_eq!(rec.json()["model"], "llama3:8b");

        let StreamOpen::Stream(us) = adapter.stream(&client, &target, adapter_request(true)).await.unwrap() else {
            panic!("expected a stream");
        };
        let bytes: Vec<u8> = us.body.map(|c| c.unwrap().to_vec()).collect::<Vec<_>>().await.concat();
        assert_eq!(String::from_utf8(bytes).unwrap(), stream_lines.concat());
        let rec = ups.last();
        assert_eq!(rec.path, "/api/chat");
        assert_eq!(rec.header("accept"), Some("application/x-ndjson"), "Ollama streams NDJSON, not SSE");
        assert_eq!(rec.header("user-agent"), Some(expected_ua().as_str()));
        for h in ["authorization", "x-api-key", "x-goog-api-key"] {
            assert_eq!(rec.header(h), None, "{h} (stream)");
        }
    }
    // 对照：Bearer 目标会带上凭据
    let target = adapter_target(&ups, AuthStyle::Bearer, Some("sk-cloud-key-5678"));
    adapter.invoke(&client, &target, adapter_request(false)).await.unwrap();
    assert_eq!(ups.last().header("authorization"), Some("Bearer sk-cloud-key-5678"));
    // 没有凭据的 Bearer 目标是配置错误，根本不发请求
    let calls = ups.calls();
    let target = adapter_target(&ups, AuthStyle::Bearer, None);
    assert!(adapter.invoke(&client, &target, adapter_request(false)).await.is_err());
    assert_eq!(ups.calls(), calls);
}

#[tokio::test]
async fn ollama_base_url_may_be_the_root_the_api_prefix_the_full_endpoint_or_a_proxy_path() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, ol_response("ok"))).await;
    let h = Harness::new().await;
    for (id, base) in [
        ("u1", ups.root()),
        ("u2", format!("{}/api", ups.root())),
        ("u3", format!("{}/api/chat", ups.root())),
        ("u4", format!("{}/proxy/ollama/", ups.root())),
        ("u5", format!("{}/proxy/api", ups.root())),
    ] {
        add_provider(&h.db, &ProviderSpec::new(id, "ollama", base)).await;
        add_conn(&h.db, &h.vault, id, &format!("{id}-c1"), 0, Some(UP_KEY_A)).await;
        add_model(&h.db, id, "llama-up", &format!("{id}/llama-up")).await;
    }
    for id in ["u1", "u2", "u3", "u4", "u5"] {
        let resp = h.ollama(ol_user_request(&format!("{id}/llama-up"), Some(false))).await;
        assert_eq!(resp.status(), 200, "{id}");
    }
    let paths: Vec<String> = ups.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, vec!["/api/chat", "/api/chat", "/api/chat", "/proxy/ollama/api/chat", "/proxy/api/chat"]);
    assert!(ups.requests().iter().all(|r| r.query.is_none() && r.header("authorization") == Some(format!("Bearer {UP_KEY_A}").as_str())));
    h.stop().await;
}

// ───────────── 5. /api/tags、/v1/models、认证、授权、请求校验 ─────────────

#[tokio::test]
async fn api_tags_lists_enabled_models_of_all_ready_providers_in_ollama_style_and_honours_allowed_models() {
    let h = Harness::with_keys(vec![
        (KEY_ALL, vkey("k-all", &[])),
        ("rk-one-model", vkey("k-one", &["o1/llama-up"])),
        ("rk-gemini-wild", vkey("k-gem", &["g1/*"])),
    ])
    .await;
    let ups = FakeUpstream::start(|_, _| Reply::json(200, ol_response("x"))).await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;
    add_model(&h.db, "o1", "llama-off", "o1/llama-off").await;
    sqlx::query("UPDATE models SET enabled = 0 WHERE id = 'o1/llama-off'").execute(&h.db).await.unwrap();
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;
    h.simple(&ups, "oa", "openai-compatible", "gpt-up", "oa/gpt-up").await;
    h.simple(&ups, "an", "anthropic-compatible", "claude-up", "an/claude-up").await;
    h.simple(&ups, "rs", "openai-responses", "gpt-up", "rs/gpt-up").await;
    // 别名里带冒号的 Ollama tag
    add_model(&h.db, "o1", "llama3:8b", "o1/llama3:8b").await;
    // 停用的提供商 / 不安全的 base URL（适配器未就绪）：不列出
    add_provider(&h.db, &ProviderSpec::new("off", "ollama", ups.root())).await;
    add_model(&h.db, "off", "m", "off/m").await;
    sqlx::query("UPDATE providers SET enabled = 0 WHERE id = 'off'").execute(&h.db).await.unwrap();
    add_provider(&h.db, &ProviderSpec::new("insecure", "ollama", "http://example.com".into())).await;
    add_model(&h.db, "insecure", "m", "insecure/m").await;
    // 内置本机 Ollama（适配器已接入）
    sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, created_at, updated_at) \
         VALUES ('ollama-local', 'builtin', 'Ollama (local)', 'free', 'none', 1, 't', 't')",
    )
    .execute(&h.db)
    .await
    .unwrap();
    add_model(&h.db, "ollama-local", "qwen3", "ollama-local/qwen3").await;

    let tags = |key: &'static str| {
        let c = h.client();
        let url = format!("{}/api/tags", h.base);
        async move {
            let resp = c.get(url).bearer_auth(key).send().await.unwrap();
            assert_eq!(resp.status(), 200);
            assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
            resp.json::<Value>().await.unwrap()
        }
    };
    let names = |v: &Value| -> Vec<String> {
        let mut out: Vec<String> = v["models"]
            .as_array()
            .unwrap_or_else(|| panic!("no `models` array: {v}"))
            .iter()
            .map(|m| {
                assert_eq!(m["name"], m["model"], "name and model carry the same public id: {m}");
                m["name"].as_str().unwrap().to_string()
            })
            .collect();
        out.sort();
        out
    };
    let all = tags(KEY_ALL).await;
    assert_eq!(v_keys(&all), vec!["models"], "Ollama-style envelope");
    let expected = vec![
        "an/claude-up", "g1/gem-up", "o1/llama-up", "o1/llama3:8b", "oa/gpt-up", "ollama-local/qwen3", "rs/gpt-up",
    ];
    assert_eq!(names(&all), expected);

    // /v1/models 列出同一批模型（含 Ollama 提供商的模型）
    let list: Value = h.client().get(format!("{}/v1/models", h.base)).bearer_auth(KEY_ALL).send().await.unwrap().json().await.unwrap();
    let mut v1: Vec<String> = list["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
    v1.sort();
    assert_eq!(v1, expected);
    assert!(list["data"].as_array().unwrap().iter().any(|m| m["id"] == "o1/llama-up" && m["owned_by"] == "o1"));

    // allowed_models 过滤：精确名与前缀通配
    assert_eq!(names(&tags("rk-one-model").await), vec!["o1/llama-up"]);
    assert_eq!(names(&tags("rk-gemini-wild").await), vec!["g1/gem-up"]);
    let v1_one: Value = h.client().get(format!("{}/v1/models", h.base)).bearer_auth("rk-one-model").send().await.unwrap().json().await.unwrap();
    assert_eq!(v1_one["data"].as_array().unwrap().len(), 1);

    // 带冒号的 tag 能被 /api/chat 直接寻址
    let resp = h.ollama(ol_user_request("o1/llama3:8b", Some(false))).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(ups.last().json()["model"], "llama3:8b");
    // 受限密钥不能调用列表之外的模型：Ollama 风格的 403
    let resp = post_chat_as(&h, "rk-one-model", &ol_user_request("g1/gem-up", Some(false))).await;
    assert_eq!(resp.status(), 403);
    assert_ollama_error(&resp.json::<Value>().await.unwrap(), "model not allowed");
    h.stop().await;
}

#[tokio::test]
async fn missing_or_invalid_keys_are_401_in_ollama_style_and_never_reach_the_upstream() {
    let h = Harness::new().await;
    let ups = FakeUpstream::start(|_, _| Reply::json(200, ol_response("x"))).await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;
    let body = ol_user_request("o1/llama-up", Some(false));

    for (what, builder) in [
        ("tags without key", h.client().get(format!("{}/api/tags", h.base))),
        ("tags with wrong key", h.client().get(format!("{}/api/tags", h.base)).bearer_auth("rk-wrong")),
        ("chat without key", h.client().post(format!("{}/api/chat", h.base)).json(&body)),
        ("chat with wrong key", h.client().post(format!("{}/api/chat", h.base)).bearer_auth("rk-wrong-KEY").json(&body)),
        ("chat with wrong x-api-key", h.client().post(format!("{}/api/chat", h.base)).header("x-api-key", "rk-wrong-KEY").json(&body)),
    ] {
        let resp = builder.send().await.unwrap();
        assert_eq!(resp.status(), 401, "{what}");
        assert_eq!(resp.headers()["www-authenticate"], "Bearer", "{what}");
        let text = resp.text().await.unwrap();
        assert!(!text.contains("rk-wrong") && !text.contains(KEY_ALL), "{what}: the response must not echo keys: {text}");
        assert_ollama_error(&serde_json::from_str(&text).unwrap(), what);
    }
    assert_eq!(ups.calls(), 0);
    h.stop().await;
}

#[tokio::test]
async fn ollama_endpoint_protocol_permission_rules() {
    let h = Harness::with_keys(vec![
        (KEY_ALL, vkey("k-all", &[])),
        ("rk-explicit-ollama", vkey_with_protocols("k-ollama", &["ollama"])),
        // 「不限制协议」的密钥在前端被展开成四个已知协议：等价放行
        ("rk-four-known", vkey_with_protocols("k-four", &["openai-chat", "openai-responses", "anthropic-messages", "gemini"])),
        ("rk-partial", vkey_with_protocols("k-partial", &["openai-chat", "gemini"])),
        ("rk-none", vkey_with_protocols("k-none", &[])),
    ])
    .await;
    let ups = FakeUpstream::start(|_, _| Reply::json(200, ol_response("ok"))).await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;
    let body = ol_user_request("o1/llama-up", Some(false));

    for key in [KEY_ALL, "rk-explicit-ollama", "rk-four-known"] {
        assert_eq!(post_chat_as(&h, key, &body).await.status(), 200, "{key} may use Ollama");
    }
    assert_eq!(ups.calls(), 3);
    for key in ["rk-partial", "rk-none"] {
        let resp = post_chat_as(&h, key, &body).await;
        assert_eq!(resp.status(), 403, "{key}");
        let v: Value = resp.json().await.unwrap();
        assert_ollama_error(&v, key);
        assert!(v["error"].as_str().unwrap().contains("Ollama"), "{v}");
    }
    assert_eq!(ups.calls(), 3, "rejected requests never reach the upstream");

    // 模型列表对任何有效密钥开放，与协议勾选无关
    for key in ["rk-partial", "rk-none", "rk-explicit-ollama"] {
        let resp = h.client().get(format!("{}/api/tags", h.base)).bearer_auth(key).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{key} may list models");
    }
    // 反过来：只勾了 ollama 的密钥不能走别的协议入口（该入口自己的风格报错）
    let resp = h
        .client()
        .post(format!("{}/v1/chat/completions", h.base))
        .bearer_auth("rk-explicit-ollama")
        .json(&json!({"model": "o1/llama-up", "messages": [{"role": "user", "content": "x"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["code"], "protocol_not_allowed");
    assert_eq!(ups.calls(), 3);
    h.stop().await;
}

#[tokio::test]
async fn malformed_requests_are_400_or_404_in_ollama_style_without_reaching_the_upstream() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("x"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    h.simple(&ups, "o1", "ollama", "llama-up", "o1/llama-up").await;

    // 不是 JSON
    let resp = h.client().post(format!("{}/api/chat", h.base)).bearer_auth(KEY_ALL).body("{not json").send().await.unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_ollama_error(&v, "not json");
    assert!(v["error"].as_str().unwrap().contains("JSON"), "{v}");
    // 缺少 model / model 为空 / model 不是字符串
    for body in [json!({"messages": [{"role": "user", "content": "x"}]}), json!({"model": "  ", "messages": []}), json!({"model": 7})] {
        let resp = h.ollama(body.clone()).await;
        assert_eq!(resp.status(), 400, "{body}");
        let v: Value = resp.json().await.unwrap();
        assert_ollama_error(&v, "missing model");
        assert!(v["error"].as_str().unwrap().contains("model"), "{v}");
    }
    // 未知模型
    let resp = h.ollama(ol_user_request("nope/llama", Some(false))).await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert_ollama_error(&v, "unknown model");
    assert!(v["error"].as_str().unwrap().contains("nope/llama"), "{v}");
    // 转换路径上的请求校验：messages 缺失 / 为空 / 角色未知 / images 不是数组，都是 400（不会发给上游）
    for (what, body) in [
        ("no messages", json!({"model": "p1/gpt-up"})),
        ("empty messages", json!({"model": "p1/gpt-up", "messages": []})),
        ("unknown role", json!({"model": "p1/gpt-up", "messages": [{"role": "narrator", "content": "x"}]})),
        ("images not an array", json!({"model": "p1/gpt-up", "messages": [{"role": "user", "content": "x", "images": "abc"}]})),
        ("tools not an array", json!({"model": "p1/gpt-up", "messages": [{"role": "user", "content": "x"}], "tools": "get_weather"})),
    ] {
        let resp = h.ollama(body).await;
        assert_eq!(resp.status(), 400, "{what}");
        assert_ollama_error(&resp.json::<Value>().await.unwrap(), what);
    }
    assert_eq!(ups.calls(), 0);

    // 别名冲突：400，列出候选，从不静默选一个
    let other = FakeUpstream::start(|_, _| Reply::json(200, ol_response("from other"))).await;
    h.simple(&other, "o2", "ollama", "llama-up", "o2/llama-up").await;
    let resp = h.ollama(ol_user_request("llama-up", Some(false))).await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_ollama_error(&v, "ambiguous");
    assert!(v["error"].as_str().unwrap().contains("o1/llama-up") && v["error"].as_str().unwrap().contains("o2/llama-up"), "{v}");
    assert_eq!((ups.calls(), other.calls()), (0, 0));

    // 方法不对 / 路由不存在：同样是 Ollama 风格
    let resp = h.client().get(format!("{}/api/chat", h.base)).bearer_auth(KEY_ALL).send().await.unwrap();
    assert_eq!(resp.status(), 405);
    assert_ollama_error(&resp.json::<Value>().await.unwrap(), "GET /api/chat");
    let resp = h.client().get(format!("{}/api/show", h.base)).bearer_auth(KEY_ALL).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert_ollama_error(&resp.json::<Value>().await.unwrap(), "/api/show");
    h.stop().await;
}

#[allow(dead_code)]
fn _unused_helpers() {
    let _ = (error_line, warnings_of);
}

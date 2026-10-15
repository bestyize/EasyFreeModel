//! 模型级可用性探测（`Runtime::probe_model`）：复用真实适配器与凭据路径的最小请求。

#[allow(unused_imports)]
use super::fixtures::*;
#[allow(unused_imports)]
use super::*;

/// 2xx：探测成功，报告真实往返延迟，且只发出一次最小请求。
#[tokio::test]
async fn probe_model_reports_success_with_latency() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("pong"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-ok", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-ok", "probe-ok/gpt-up").await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert!(r.latency_ms.is_some());
    assert_eq!(r.provider_id.as_deref(), Some("probe-ok"));

    // 只发一次请求；请求体是最小探测体，且模型被改写成上游 ID
    assert_eq!(ups.calls(), 1);
    let body = ups.last().json();
    assert_eq!(body["model"], "gpt-up");
    assert!(body["max_tokens"].as_u64().is_some());
    assert_eq!(body["messages"][0]["content"], "ping");
    // 非流式：OpenAI 兼容上游不带 stream_options
    assert!(body.get("stream_options").is_none());
}

/// 401：如实报告凭据被拒，且**绝不**在错误里回显密钥。
#[tokio::test]
async fn probe_model_reports_auth_failure_without_leaking_the_key() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(401, json!({ "error": { "message": format!("bad key {UP_KEY_A}") } }))
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-401", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-401", "probe-401/gpt-up").await.unwrap();
    assert!(!r.ok);
    assert!(r.latency_ms.is_none());
    let err = r.error.unwrap();
    assert!(!err.contains(UP_KEY_A), "错误信息泄露了密钥：{err}");
    assert!(err.contains("[redacted]"), "{err}");
    assert!(err.contains("401"), "{err}");
}

/// 404：上游找不到该模型时给出明确说法（这是用户最需要区分的情形）。
#[tokio::test]
async fn probe_model_explains_a_missing_model() {
    let ups = FakeUpstream::start(|_, _| Reply::json(404, json!({ "error": { "message": "model not found" } }))).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-404", "openai-compatible", "gpt-nope", "gpt-nope").await;

    let r = h.runtime.probe_model("probe-404", "probe-404/gpt-nope").await.unwrap();
    assert!(!r.ok);
    let err = r.error.unwrap();
    assert!(err.contains("找不到该模型"), "{err}");
    assert!(err.contains("404"), "{err}");
}

/// 没有连接：如实说明，而不是报成功，也不发出任何请求。
#[tokio::test]
async fn probe_model_without_a_connection_says_so() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("should not happen"))).await;
    let h = Harness::new().await;
    add_provider(&h.db, &ProviderSpec::new("probe-empty", "openai-compatible", ups.base())).await;
    add_model(&h.db, "probe-empty", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-empty", "probe-empty/gpt-up").await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("还没有添加任何连接"));
    assert_eq!(ups.calls(), 0, "不应发出任何上游请求");
}

/// 模型不存在：如实报错，不发出请求。
#[tokio::test]
async fn probe_model_with_an_unknown_model_id_says_so() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("nope"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-nodef", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-nodef", "probe-nodef/does-not-exist").await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("模型不存在"));
    assert_eq!(ups.calls(), 0);
}

/// 提供商不存在：如实报错。
#[tokio::test]
async fn probe_model_with_an_unknown_provider_says_so() {
    let h = Harness::new().await;
    let r = h.runtime.probe_model("no-such-provider", "x/y").await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("提供商不存在"));
}

/// 停用的连接不参与探测（调度器语义）：如实报告没有可用连接。
#[tokio::test]
async fn probe_model_ignores_disabled_connections() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("nope"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-off", "openai-compatible", "gpt-up", "gpt-up").await;
    sqlx::query("UPDATE connections SET enabled = 0 WHERE provider_id = 'probe-off'")
        .execute(&h.db)
        .await
        .unwrap();

    let r = h.runtime.probe_model("probe-off", "probe-off/gpt-up").await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("停用"));
    assert_eq!(ups.calls(), 0);
}

/// Anthropic 上游：探测请求经规范化转换后是合法的 Messages 体（模型在 body，走 x-api-key）。
#[tokio::test]
async fn probe_model_encodes_for_an_anthropic_upstream() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(
            200,
            json!({
                "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-up",
                "content": [{ "type": "text", "text": "pong" }],
                "stop_reason": "end_turn", "usage": { "input_tokens": 3, "output_tokens": 1 }
            }),
        )
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-an", "anthropic-compatible", "claude-up", "claude-up").await;

    let r = h.runtime.probe_model("probe-an", "probe-an/claude-up").await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    let rec = ups.last();
    assert_eq!(rec.json()["model"], "claude-up");
    // Anthropic 需要 max_tokens；且凭据走 x-api-key 头而不是 Bearer
    assert!(rec.json()["max_tokens"].as_u64().is_some(), "缺少 max_tokens：{}", rec.json());
    assert!(rec.header("x-api-key").is_some(), "Anthropic 上游应使用 x-api-key");
}

/// HTTP 200 但响应体为空结果 —— 不能算成功（9Router 同样会检查响应体）。
#[tokio::test]
async fn probe_model_rejects_a_200_with_no_content() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, json!({ "choices": [] }))).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-empty-body", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-empty-body", "probe-empty-body/gpt-up").await.unwrap();
    assert!(!r.ok, "200 + 空结果不应算成功");
    // 空 choices 无法解码成一次有效回复，因此走“响应体不符合协议”分支
    let err = r.error.unwrap();
    assert!(err.contains("200"), "{err}");
}

/// HTTP 200 但响应体不是 JSON（被代理/网关拦截）—— 如实报告，不假装成功。
#[tokio::test]
async fn probe_model_rejects_a_200_that_is_not_json() {
    let ups = FakeUpstream::start(|_, _| Reply::raw(200, "<html>blocked by proxy</html>")).await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-html", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-html", "probe-html/gpt-up").await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("不是合法 JSON"));
}

/// 200 且带 choices，但没有任何 content 片段 —— 同样不算成功。
#[tokio::test]
async fn probe_model_rejects_a_200_with_an_empty_message() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(
            200,
            json!({
                "id": "c1", "object": "chat.completion", "created": 1, "model": "gpt-up",
                "choices": [{ "index": 0, "message": { "role": "assistant", "content": "" }, "finish_reason": "stop" }]
            }),
        )
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "probe-blank", "openai-compatible", "gpt-up", "gpt-up").await;

    let r = h.runtime.probe_model("probe-blank", "probe-blank/gpt-up").await.unwrap();
    assert!(!r.ok, "200 + 空 content 不应算成功");
}

//! Vertex AI（express 模式）上游的端到端测试。

use super::fixtures::*;
use super::*;

fn vertex_user_body() -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": "You are a geography tutor."}]},
        "contents": [{"role": "user", "parts": [{"text": "What is the capital of France?"}]}],
        "generationConfig": {"temperature": 0.2, "maxOutputTokens": 128},
    })
}

#[tokio::test]
async fn vertex_express_mode_generate_content_passthrough() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, gm_response("Paris is the capital of France."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "v1", "vertex", "gemini-2.5-flash", "v1/gemini-flash").await;

    let resp = h.gemini("v1/gemini-flash", vertex_user_body()).await;
    assert_eq!(resp.status(), 200);

    let up = ups.last();
    // Vertex express URL path: /v1/publishers/google/models/<model>:generateContent
    assert_eq!(
        up.path,
        "/v1/publishers/google/models/gemini-2.5-flash:generateContent"
    );
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("authorization"), None);
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));

    let rows = h.usage(1).await;
    assert_eq!(rows[0].provider_id, "v1");
    assert_eq!(rows[0].status, "success");
    h.stop().await;
}

#[tokio::test]
async fn vertex_express_mode_stream_generate_content() {
    let chunks: Vec<String> = gm_text_stream().iter().map(gm_sse).collect();
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "v1", "vertex", "gemini-2.5-flash", "v1/gemini-flash").await;

    let resp = h.gemini_sse("v1/gemini-flash", vertex_user_body()).await;
    assert_eq!(resp.status(), 200);

    let up = ups.last();
    assert_eq!(
        up.path,
        "/v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent"
    );
    assert_eq!(up.query.as_deref(), Some("alt=sse"));
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));

    let rows = h.usage(1).await;
    assert_eq!(rows[0].provider_id, "v1");
    assert_eq!(rows[0].status, "success");
    h.stop().await;
}

#[tokio::test]
async fn vertex_cross_protocol_from_openai_chat() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, gm_response("Paris"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "v1", "vertex", "gemini-2.5-flash", "v1/gemini-flash").await;

    let chat_body = json!({
        "model": "v1/gemini-flash",
        "messages": [{"role": "user", "content": "What is the capital of France?"}],
    });
    let resp = h.chat(chat_body).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "Paris");

    let up = ups.last();
    assert_eq!(
        up.path,
        "/v1/publishers/google/models/gemini-2.5-flash:generateContent"
    );
    h.stop().await;
}

#[tokio::test]
async fn vertex_rejects_service_account_credentials_with_not_ready() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, gm_response("ok"))).await;
    let h = Harness::new().await;
    // Add vertex provider with a fake service account json key
    let base = ups.root();
    add_provider(&h.db, &ProviderSpec::new("v-bad", "vertex", base)).await;
    let sa_key = r#"{"type":"service_account","project_id":"my-proj"}"#;
    add_conn(&h.db, &h.vault, "v-bad", "v-bad-c1", 0, Some(sa_key)).await;
    add_model(&h.db, "v-bad", "gemini-2.5-flash", "v-bad/flash").await;

    let resp = h.gemini("v-bad/flash", vertex_user_body()).await;
    // Should fail with 501 / NotReady because service account is not express mode
    assert!(resp.status().is_client_error() || resp.status().is_server_error());
    let v: Value = resp.json().await.unwrap();
    let msg = v["error"]["message"].as_str().unwrap_or("");
    assert!(msg.contains("express") || msg.contains("Vertex AI"), "{msg}");
    h.stop().await;
}

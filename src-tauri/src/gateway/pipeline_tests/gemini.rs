//! Gemini 入口（`/v1beta/models/{model}:generateContent` 等）与 Gemini 上游的端到端测试。

use super::fixtures::*;
use super::*;

fn gemini_user_body() -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": "You are a geography tutor."}]},
        "contents": [{"role": "user", "parts": [{"text": "What is the capital of France? Answer in a sentence."}]}],
        "generationConfig": {"temperature": 0.2, "maxOutputTokens": 128},
    })
}

// ───────────── 路径解析 / 认证 ─────────────

#[tokio::test]
async fn gemini_paths_parse_the_model_and_method_around_colons_and_slashes() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("ok"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    // 对外别名里本身带冒号（Ollama 风格的 tag）：方法永远是最后一个冒号之后的部分
    add_provider(&h.db, &ProviderSpec::new("p2", "openai-compatible", ups.base())).await;
    add_conn(&h.db, &h.vault, "p2", "p2-c1", 0, Some(UP_KEY_A)).await;
    add_model(&h.db, "p2", "llama3:8b", "p2/llama3:8b").await;

    for (target, upstream_model) in [
        ("p1/gpt-up:generateContent", "gpt-up"),
        ("p1%2Fgpt-up:generateContent", "gpt-up"),
        ("p1/gpt-up%3AgenerateContent", "gpt-up"),
        ("gpt-up:generateContent", "gpt-up"),
        ("p2/llama3:8b:generateContent", "llama3:8b"),
    ] {
        let resp = h.gemini_req(target, &gemini_user_body()).send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert_eq!(status, 200, "{target}: {text}");
        assert_eq!(ups.last().json()["model"], upstream_model, "{target}");
        let v: Value = serde_json::from_str(&text).unwrap();
        let parts = v["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert!(parts.iter().any(|p| p["text"] == "ok" && p.get("thought").is_none()), "{target}: {text}");
    }
    // 稳定版 /v1 前缀同样可用
    let resp = h.client().post(format!("{}/v1/models/p1/gpt-up:generateContent", h.base)).header("x-goog-api-key", KEY_ALL).json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 不支持的方法：Gemini 风格的 501，而不是 404 或假装成功
    let resp = h.gemini_req("p1/gpt-up:countTokens", &gemini_user_body()).send().await.unwrap();
    assert_eq!(resp.status(), 501);
    let v: Value = resp.json().await.unwrap();
    assert_eq!((v["error"]["code"].as_u64(), v["error"]["status"].as_str()), (Some(501), Some("UNIMPLEMENTED")), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("countTokens"));
    // 没有方法
    let resp = h.gemini_req("p1/gpt-up", &gemini_user_body()).send().await.unwrap();
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.json::<Value>().await.unwrap()["error"]["status"], "NOT_FOUND");
    // 未知模型
    let resp = h.gemini("nope/model", gemini_user_body()).await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["status"], "NOT_FOUND", "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("nope/model"));
    h.stop().await;
}

#[tokio::test]
async fn gemini_accepts_x_goog_api_key_and_bearer_and_reports_auth_errors_in_gemini_style() {
    let partial = vkey_with_protocols("k-chat-only", &["openai-chat"]);
    let h = Harness::with_keys(vec![(KEY_ALL, vkey("k-all", &[])), ("rk-chat-only-KEY-0002", partial)]).await;
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("ok"))).await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;
    let url = format!("{}/v1beta/models/p1/gpt-up:generateContent", h.base);

    // x-goog-api-key
    let r = h.client().post(&url).header("x-goog-api-key", KEY_ALL).json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 200);
    // Authorization: Bearer
    let r = h.client().post(&url).bearer_auth(KEY_ALL).json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 200);
    // 缺少密钥
    let r = h.client().post(&url).json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let v: Value = r.json().await.unwrap();
    assert_eq!((v["error"]["code"].as_u64(), v["error"]["status"].as_str()), (Some(401), Some("UNAUTHENTICATED")), "{v}");
    // 无效密钥（Gemini 风格而不是 OpenAI 风格）
    let r = h.client().post(&url).header("x-goog-api-key", "rk-wrong").json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["status"], "UNAUTHENTICATED");
    // 密钥没有 gemini 协议权限
    let r = h.client().post(&url).header("x-goog-api-key", "rk-chat-only-KEY-0002").json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["status"], "PERMISSION_DENIED", "{v}");
    assert_eq!(ups.calls(), 2, "rejected requests never reach the upstream");
    // 查询串里的 ?key= 不是认证方式
    let r = h.client().post(format!("{url}?key={KEY_ALL}")).json(&gemini_user_body()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    h.stop().await;
}

fn vkey_with_protocols(id: &str, protocols: &[&str]) -> VerifiedKey {
    VerifiedKey { allowed_protocols: protocols.iter().map(|s| s.to_string()).collect(), ..vkey(id, &[]) }
}

// ───────────── Gemini → Gemini 透传 ─────────────

#[tokio::test]
async fn gemini_to_gemini_passthrough_sends_the_key_in_a_header_and_never_in_the_url() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, gm_response("Paris is the capital of France."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;

    let mut body = gemini_user_body();
    body["safetySettings"] = json!([{"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH"}]);
    body["vendorExtension"] = json!({"deep": [1, 2, 3]});
    let resp = h.gemini("g1/gem-up", body.clone()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("x-router-warnings").is_none());
    assert_eq!(resp.json::<Value>().await.unwrap(), gm_response("Paris is the capital of France."), "upstream body untouched");

    let up = ups.last();
    assert_eq!(up.path, "/v1beta/models/gem-up:generateContent");
    assert_eq!(up.query, None, "no query string at all: the key never travels in the URL");
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("authorization"), None, "Gemini uses x-goog-api-key, not a Bearer token");
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    assert!(!up.path.contains(UP_KEY_A));
    assert_eq!(up.json(), body, "passthrough forwards the body as is (the model lives in the path)");

    let rows = h.usage(1).await;
    let r = &rows[0];
    assert_eq!((r.provider_id.as_str(), r.model_id.as_str(), r.protocol.as_str(), r.status.as_str()), ("g1", "g1/gem-up", "gemini", "success"));
    // 21 含缓存 5；8 候选 + 3 思考 = 11
    assert_eq!((r.input_tokens, r.output_tokens, r.cached_tokens, r.token_source.as_str()), (21, 11, 5, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn gemini_passthrough_rewrites_a_model_field_in_the_body_to_the_upstream_model() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, gm_response("x"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;
    let mut body = gemini_user_body();
    body["model"] = json!("models/g1/gem-up");
    assert_eq!(h.gemini("g1/gem-up", body).await.status(), 200);
    assert_eq!(ups.last().json()["model"], "models/gem-up", "a `model` in the body must agree with the URL");
    h.stop().await;
}

#[tokio::test]
async fn gemini_stream_sse_passthrough_is_byte_identical_and_records_final_usage() {
    let chunks: Vec<String> = gm_text_stream().iter().map(gm_sse).collect();
    let all = chunks.concat();
    let script_chunks = chunks.clone();
    let ups = FakeUpstream::start(move |_, _| {
        // 半包：把每帧拆成两段
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
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;

    let resp = h.gemini_sse("g1/gem-up", gemini_user_body()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    assert_eq!(resp.text().await.unwrap(), all);

    let up = ups.last();
    assert_eq!(up.path, "/v1beta/models/gem-up:streamGenerateContent");
    assert_eq!(up.query.as_deref(), Some("alt=sse"));
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("accept"), Some("text/event-stream"));
    let rows = h.usage(1).await;
    // 25 含缓存 10；6 候选 + 2 思考
    assert_eq!((rows[0].status.as_str(), rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens), ("success", 25, 8, 10));
    assert_eq!(rows[0].token_source, "upstream");
    h.stop().await;
}

#[tokio::test]
async fn gemini_stream_without_alt_sse_is_a_json_array_while_the_upstream_is_still_asked_for_sse() {
    let chunks = gm_text_stream();
    let up_chunks = chunks.clone();
    let ups = FakeUpstream::start(move |_, _| Reply::sse(up_chunks.iter().map(gm_sse).collect())).await;
    let h = Harness::new().await;
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;

    let resp = h.gemini_array("g1/gem-up", gemini_user_body()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    let text = resp.text().await.unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not a JSON array ({e}): {text}"));
    assert_eq!(parsed, Value::Array(chunks), "{text}");
    assert_eq!(ups.last().query.as_deref(), Some("alt=sse"));
    let rows = h.usage(1).await;
    assert_eq!((rows[0].status.as_str(), rows[0].input_tokens, rows[0].output_tokens), ("success", 25, 8));
    h.stop().await;
}

// ───────────── Gemini 客户端 → Chat / Anthropic 上游 ─────────────

/// 多轮工具链 + 图片 + system + 生成参数的 Gemini 请求。
fn gemini_tool_chain_body() -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": "You are a weather bot."}]},
        "contents": [
            {"role": "user", "parts": [{"text": "What is in this picture, and what's the weather in Paris?"},
                                       {"inlineData": {"mimeType": "image/png", "data": PNG_B64}}]},
            {"role": "model", "parts": [{"functionCall": {"id": "call_1", "name": "get_weather", "args": {"city": "Paris"}}}]},
            {"role": "user", "parts": [{"functionResponse": {"id": "call_1", "name": "get_weather", "response": {"output": "18C and sunny"}}}]},
            {"role": "model", "parts": [{"text": "It is 18C in Paris."}]},
            {"role": "user", "parts": [{"text": "Thanks. And in Rome?"}]}
        ],
        "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "Weather by city",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}]}],
        "generationConfig": {"temperature": 0.1, "maxOutputTokens": 256, "stopSequences": ["END"]}
    })
}

fn oa_tool_completion(id: &str, name: &str, args: &str) -> Value {
    json!({
        "id": "chatcmpl-tool", "object": "chat.completion", "created": 1, "model": "gpt-up",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": id, "type": "function", "function": {"name": name, "arguments": args}}]}, "finish_reason": "tool_calls"}],
        "usage": {"prompt_tokens": 77, "completion_tokens": 13, "total_tokens": 90}
    })
}

#[tokio::test]
async fn gemini_request_to_chat_upstream_system_images_and_multi_turn_tool_chain() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_tool_completion("call_9", "get_weather", "{\"city\":\"Rome\"}"))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h.gemini("p1/gpt-up", gemini_tool_chain_body()).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();

    let up = ups.last();
    assert_eq!(up.path, "/v1/chat/completions");
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    let b = up.json();
    assert_eq!(b["model"], "gpt-up");
    let msgs = b["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "tool", "assistant", "user"], "{b}");
    assert_eq!(msgs[0]["content"], "You are a weather bot.");
    let user0 = msgs[1]["content"].as_array().expect("text + image parts");
    assert_eq!(user0[0]["text"], "What is in this picture, and what's the weather in Paris?");
    assert_eq!(user0[1]["image_url"]["url"], format!("data:image/png;base64,{PNG_B64}"));
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(serde_json::from_str::<Value>(msgs[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Paris"}));
    assert_eq!(msgs[3]["tool_call_id"], "call_1");
    assert!(msgs[3]["content"].as_str().unwrap().contains("18C and sunny"), "{}", msgs[3]);
    assert_eq!(msgs[4]["content"], "It is 18C in Paris.");
    assert_eq!(msgs[5]["content"], "Thanks. And in Rome?");
    assert_eq!(b["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(b["tools"][0]["function"]["parameters"]["required"], json!(["city"]));
    assert_eq!(b["temperature"], 0.1);
    assert_eq!(b["stop"], json!(["END"]));
    assert!(b["max_tokens"] == 256 || b["max_completion_tokens"] == 256, "{b}");
    assert!(b.get("stream").is_none() || b["stream"] == false);

    // Gemini 风格的响应：functionCall + 用量
    let cand = &v["candidates"][0];
    assert_eq!(cand["finishReason"], "STOP");
    let fc = &cand["content"]["parts"][0]["functionCall"];
    assert_eq!((fc["name"].as_str(), fc["args"].clone()), (Some("get_weather"), json!({"city": "Rome"})), "{v}");
    assert_eq!(v["usageMetadata"]["promptTokenCount"], 77);
    assert_eq!(v["usageMetadata"]["candidatesTokenCount"], 13);

    let rows = h.usage(1).await;
    assert_eq!((rows[0].protocol.as_str(), rows[0].input_tokens, rows[0].output_tokens, rows[0].token_source.as_str()), ("gemini", 77, 13, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn gemini_request_to_anthropic_upstream_system_images_and_tool_chain() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(200, json!({
            "id": "msg_t", "type": "message", "role": "assistant", "model": "claude-up",
            "content": [{"type": "text", "text": "Checking Rome."}, {"type": "tool_use", "id": "toolu_9", "name": "get_weather", "input": {"city": "Rome"}}],
            "stop_reason": "tool_use", "stop_sequence": null,
            "usage": {"input_tokens": 50, "output_tokens": 20, "cache_read_input_tokens": 30, "cache_creation_input_tokens": 0}
        }))
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let resp = h.gemini("an1/claude-up", gemini_tool_chain_body()).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();

    let up = ups.last();
    assert_eq!(up.path, "/v1/messages");
    assert_eq!(up.header("x-api-key"), Some(UP_KEY_A));
    assert!(up.header("anthropic-version").is_some());
    let b = up.json();
    assert_eq!(b["system"], json!("You are a weather bot.").clone(), "{b}");
    let msgs = b["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "assistant", "user", "assistant", "user"], "{b}");
    let first = msgs[0]["content"].as_array().unwrap();
    assert!(first.iter().any(|p| p["type"] == "image" && p["source"]["type"] == "base64" && p["source"]["media_type"] == "image/png" && p["source"]["data"] == PNG_B64), "{b}");
    assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][0]["id"], "call_1");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(b["tools"][0]["name"], "get_weather");
    assert_eq!(b["max_tokens"], 256);
    assert_eq!(b["stop_sequences"], json!(["END"]));

    let parts = v["candidates"][0]["content"]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "Checking Rome.");
    assert_eq!(parts[1]["functionCall"]["name"], "get_weather");
    assert_eq!(parts[1]["functionCall"]["args"], json!({"city": "Rome"}));
    // Anthropic 的 input_tokens 不含缓存：规范化后 50 + 30 = 80，缓存 30 是其中的子集
    assert_eq!(v["usageMetadata"]["promptTokenCount"], 80);
    assert_eq!(v["usageMetadata"]["cachedContentTokenCount"], 30);
    assert_eq!(v["usageMetadata"]["candidatesTokenCount"], 20);
    let rows = h.usage(1).await;
    assert_eq!((rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens), (80, 20, 30));
    h.stop().await;
}

fn chat_text_and_tool_stream() -> Vec<String> {
    vec![
        sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)),
        sse_data(&oa_chunk(json!({"content": "It is "}), None)),
        sse_data(&oa_chunk(json!({"content": "Paris."}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "get_weather", "arguments": ""}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"city\":"}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"Rome\"}"}}]}), None)),
        sse_data(&oa_chunk(json!({}), Some("tool_calls"))),
        sse_data(&oa_usage_chunk(40, 17)),
        "data: [DONE]\n\n".to_string(),
    ]
}

#[tokio::test]
async fn gemini_sse_stream_from_chat_upstream_event_order_tool_call_and_final_usage() {
    let chunks = chat_text_and_tool_stream();
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h.gemini_sse("p1/gpt-up", gemini_tool_chain_body()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    let frames = sse_data_json(&text);
    // 文本增量 → 整块 functionCall → 带 finishReason 与最终 usageMetadata 的末帧
    let texts: Vec<&str> = frames.iter().filter_map(|f| f["candidates"][0]["content"]["parts"][0]["text"].as_str()).collect();
    assert_eq!(&texts[..2], &["It is ", "Paris."], "{text}");
    let call_idx = frames.iter().position(|f| f["candidates"][0]["content"]["parts"][0].get("functionCall").is_some()).expect("functionCall chunk");
    assert_eq!(frames[call_idx]["candidates"][0]["content"]["parts"][0]["functionCall"]["name"], "get_weather");
    assert_eq!(frames[call_idx]["candidates"][0]["content"]["parts"][0]["functionCall"]["args"], json!({"city": "Rome"}));
    assert!(call_idx > 1, "tool call arrives after the text chunks");
    let last = frames.last().unwrap();
    assert_eq!(last["candidates"][0]["finishReason"], "STOP", "{text}");
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 40);
    assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 17);
    assert!(frames[..frames.len() - 1].iter().all(|f| f["candidates"][0].get("finishReason").is_none()), "finishReason only on the last chunk");
    assert!(!text.contains("[DONE]"), "Gemini streams have no [DONE] sentinel");

    let up = ups.last().json();
    assert_eq!(up["stream"], true);
    let rows = h.usage(1).await;
    assert_eq!((rows[0].status.as_str(), rows[0].input_tokens, rows[0].output_tokens), ("success", 40, 17));
    h.stop().await;
}

#[tokio::test]
async fn gemini_json_array_stream_from_chat_upstream_is_a_valid_array_with_final_usage() {
    let chunks = chat_text_and_tool_stream();
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h.gemini_array("p1/gpt-up", gemini_tool_chain_body()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    let text = resp.text().await.unwrap();
    let arr: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("not a JSON array ({e}): {text}"));
    let arr = arr.as_array().unwrap();
    assert!(arr.len() >= 3, "{text}");
    assert_eq!(arr[0]["candidates"][0]["content"]["parts"][0]["text"], "It is ");
    let last = arr.last().unwrap();
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 40);
    h.stop().await;
}

#[tokio::test]
async fn gemini_sse_stream_from_anthropic_upstream_maps_cache_and_final_usage() {
    let stream = an_text_stream();
    let ups = FakeUpstream::start(move |_, _| Reply::sse(stream.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let resp = h.gemini_sse("an1/claude-up", gemini_user_body()).await;
    assert_eq!(resp.status(), 200);
    let frames = sse_data_json(&resp.text().await.unwrap());
    let texts: String = frames.iter().filter_map(|f| f["candidates"][0]["content"]["parts"][0]["text"].as_str()).collect();
    assert_eq!(texts, "Hello world");
    let last = frames.last().unwrap();
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    // message_start 的 input 25（不含缓存）+ cache_read 10 = 35；output 15
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 35);
    assert_eq!(last["usageMetadata"]["cachedContentTokenCount"], 10);
    assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 15);
    let rows = h.usage(1).await;
    assert_eq!((rows[0].input_tokens, rows[0].output_tokens, rows[0].cached_tokens), (35, 15, 10));
    h.stop().await;
}

//! OpenAI Responses 入口（`/v1/responses`）与 Responses 上游的端到端测试。
//!
//! 覆盖：Responses→Responses 透传；Responses 客户端 → Chat / Anthropic / Gemini 上游；
//! Chat / Anthropic / Gemini / Ollama 客户端 → Responses 上游；Responses 专有错误；冷却回退、并发、取消、日志卫生。

use super::fixtures::*;
use super::*;

// ───────────── 夹具与小工具 ─────────────

const SYSTEM_PROMPT: &str = "You are a meticulous weather assistant.";
const IMAGE_PROMPT: &str = "What does this chart show, and what is the weather in Paris?";

fn png_data_url() -> String {
    format!("data:image/png;base64,{PNG_B64}")
}

fn weather_params() -> Value {
    json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]})
}

/// Responses 请求：instructions + 图片 + 多轮工具链（assistant 文本 + function_call → function_call_output）+ 末尾追问。
fn rs_chain_request(model: &str, tool_name: &str) -> Value {
    json!({
        "model": model,
        "instructions": SYSTEM_PROMPT,
        "input": [
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": IMAGE_PROMPT},
                {"type": "input_image", "image_url": png_data_url()}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Let me check."}]},
            {"type": "function_call", "call_id": "call_1", "name": tool_name, "arguments": "{\"city\":\"Paris\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "18C and sunny"},
            {"type": "message", "role": "user", "content": "Now do Lyon too."}
        ],
        "tools": [{"type": "function", "name": tool_name, "description": "Weather by city", "parameters": weather_params(), "strict": false}],
        "tool_choice": "auto",
        "max_output_tokens": 300
    })
}

/// 把每个帧拆成两段发送（模拟 TCP 半包）。
fn split_halves(chunks: &[String]) -> Vec<(String, u64)> {
    let mut parts = Vec::new();
    for c in chunks {
        let mut mid = c.len() / 2;
        while !c.is_char_boundary(mid) {
            mid += 1;
        }
        parts.push((c[..mid].to_string(), 0));
        parts.push((c[mid..].to_string(), 0));
    }
    parts
}

/// Responses SSE 文本 → 事件 JSON 列表；同时断言每帧的 `event:` 名与 `data.type` 一致。
fn rs_events(text: &str) -> Vec<Value> {
    sse_events(text)
        .into_iter()
        .map(|(name, v)| {
            assert_eq!(name.as_deref(), v["type"].as_str(), "the SSE event name must equal data.type: {v}\n{text}");
            v
        })
        .collect()
}

fn ev_types(evs: &[Value]) -> Vec<&str> {
    evs.iter().map(|e| e["type"].as_str().unwrap_or("?")).collect()
}

/// `sequence_number` 从 0 开始严格递增。
fn assert_sequence_numbers(evs: &[Value]) {
    let seqs: Vec<u64> = evs.iter().map(|e| e["sequence_number"].as_u64().unwrap_or_else(|| panic!("missing sequence_number: {e}"))).collect();
    assert_eq!(seqs[0], 0, "{seqs:?}");
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "sequence_number must be strictly increasing: {seqs:?}");
}

/// 最后一个事件必须是 `response.completed`，返回其中的 response 对象。
fn completed_response(evs: &[Value]) -> &Value {
    let last = evs.last().expect("no events");
    assert_eq!(last["type"], "response.completed", "{last}");
    &last["response"]
}

/// 取出所有 `response.output_text.delta` 的文本。
fn text_deltas(evs: &[Value]) -> String {
    evs.iter().filter(|e| e["type"] == "response.output_text.delta").filter_map(|e| e["delta"].as_str()).collect()
}

fn usage_tuple(r: &UsageRow) -> (&str, &str, i64, i64, i64, &str) {
    (r.protocol.as_str(), r.status.as_str(), r.input_tokens, r.output_tokens, r.cached_tokens, r.token_source.as_str())
}

/// 单个账号、可控 maxRetries 的提供商（便于精确断言上游调用次数）。
#[allow(dead_code)]
async fn add_single(h: &Harness, id: &'static str, adapter: &'static str, base: String, max_retries: i64) {
    let mut p = ProviderSpec::new(id, adapter, base);
    p.max_retries = max_retries;
    add_provider(&h.db, &p).await;
    add_conn(&h.db, &h.vault, id, &format!("{id}-c1"), 0, Some(UP_KEY_A)).await;
}

#[allow(dead_code)]
async fn add_two_accounts(h: &Harness, id: &'static str, adapter: &'static str, base: String) {
    add_provider(&h.db, &ProviderSpec::new(id, adapter, base)).await;
    add_conn(&h.db, &h.vault, id, "a", 0, Some(UP_KEY_A)).await;
    add_conn(&h.db, &h.vault, id, "b", 1, Some(UP_KEY_B)).await;
}

#[allow(dead_code)]
fn is_key_a(rec: &Recorded) -> bool {
    rec.header("authorization").unwrap_or("").ends_with(UP_KEY_A)
}

// ───────────── 1. Responses → Responses 透传 ─────────────

#[tokio::test]
async fn responses_to_responses_passthrough_nonstream_keeps_every_field_and_records_upstream_usage() {
    let upstream_body = rs_response("Paris is the capital of France.");
    let reply = upstream_body.clone();
    let ups = FakeUpstream::start(move |_, _| Reply::json(200, reply.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "r1", "openai-responses", "gpt-up", "r1/gpt-up").await;

    // 完整多轮请求 + 一批网关不认识 / 不转换的字段：透传时必须全部原样保留
    let mut req = rs_chain_request("r1/gpt-up", "get_weather");
    req["store"] = json!(true);
    req["include"] = json!(["reasoning.encrypted_content"]);
    req["metadata"] = json!({"trace": "abc"});
    req["truncation"] = json!("auto");
    req["vendor_extension"] = json!({"deep": [1, 2, 3]});
    let resp = h.responses(req.clone()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("x-router-warnings").is_none(), "passthrough has nothing to warn about");
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    assert_eq!(resp.json::<Value>().await.unwrap(), upstream_body, "the response body is the upstream body, untouched");

    let up = ups.last();
    assert_eq!(up.path, "/v1/responses");
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()), "outbound User-Agent is fixed");
    assert_eq!(up.header("accept"), Some("application/json"));
    let mut expected = req;
    expected["model"] = json!("gpt-up");
    assert_eq!(up.json(), expected, "only `model` is rewritten to the upstream id; everything else is JSON-equal");

    // 21 含缓存 5；8 含推理 3（Responses 的口径与规范化口径一致，原样搬运）
    let rows = h.usage(1).await;
    let r = &rows[0];
    assert_eq!(r.api_key_id.as_deref(), Some("k-all"));
    assert_eq!((r.provider_id.as_str(), r.connection_id.as_str(), r.model_id.as_str()), ("r1", "r1-c1", "r1/gpt-up"));
    assert_eq!(usage_tuple(r), ("openai-responses", "success", 21, 8, 5, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_to_responses_passthrough_stream_is_byte_identical_and_usage_comes_from_completed() {
    let text_stream = rs_text_stream();
    let tool_stream = rs_tool_stream("call_s", "get_weather", ["{\"city\":", "\"Lyon\"}"]);
    let (t1, t2) = (text_stream.clone(), tool_stream.clone());
    let ups = FakeUpstream::start(move |_, n| {
        // 半包：每个事件拆成两段发送
        let chunks = split_halves(if n == 0 { &t1 } else { &t2 });
        Reply::Sse { chunks, end: End::Finish }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "r1", "openai-responses", "gpt-up", "r1/gpt-up").await;

    let mut req = rs_chain_request("r1/gpt-up", "get_weather");
    req["stream"] = json!(true);
    req["vendor_extension"] = json!(true);

    let resp = h.responses(req.clone()).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    assert_eq!(resp.text().await.unwrap(), text_stream.concat(), "the client receives exactly the upstream bytes");
    let up = ups.last();
    assert_eq!(up.path, "/v1/responses");
    assert_eq!(up.header("accept"), Some("text/event-stream"));
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    let mut expected = req.clone();
    expected["model"] = json!("gpt-up");
    assert_eq!(up.json(), expected);

    // 函数调用流同样逐字节一致
    let resp = h.responses(req).await;
    assert_eq!(resp.text().await.unwrap(), tool_stream.concat());

    let rows = h.usage(2).await;
    // 文本流：42 含缓存 10；11 含推理 4。工具流：60 / 15，没有缓存明细
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 42, 11, 10, "upstream"));
    assert_eq!(usage_tuple(&rows[1]), ("openai-responses", "success", 60, 15, 0, "upstream"));
    h.stop().await;
}

// ───────────── 2. Responses 客户端 → Chat 上游 ─────────────

#[tokio::test]
async fn responses_request_to_chat_upstream_text_instructions_reasoning_usage_and_warnings() {
    let ups = FakeUpstream::start(|_, _| Reply::json(200, oa_completion("Paris is the capital of France."))).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h
        .responses(json!({
            "model": "p1/gpt-up", "instructions": SYSTEM_PROMPT, "input": "What is the capital of France?",
            "max_output_tokens": 200, "temperature": 0.2, "truncation": "auto", "mystery_field": 1
        }))
        .await;
    assert_eq!(resp.status(), 200);
    let warnings = resp.headers()["x-router-warnings"].to_str().unwrap().to_string();
    assert!(warnings.contains("truncation") && warnings.contains("mystery_field"), "{warnings}");
    assert!(!warnings.contains("France") && !warnings.contains("meticulous"), "warnings carry field names only: {warnings}");

    let v: Value = resp.json().await.unwrap();
    assert_eq!((v["object"].as_str(), v["status"].as_str(), v["model"].as_str()), (Some("response"), Some("completed"), Some("gpt-up")));
    let out = v["output"].as_array().unwrap();
    assert_eq!(out.len(), 2, "{v}");
    assert_eq!(out[0]["type"], "reasoning");
    assert_eq!(out[0]["summary"], json!([{"type": "summary_text", "text": "step by step"}]));
    assert_eq!(out[1]["type"], "message");
    assert_eq!((out[1]["role"].as_str(), out[1]["status"].as_str()), (Some("assistant"), Some("completed")));
    assert_eq!(out[1]["content"], json!([{"type": "output_text", "text": "Paris is the capital of France.", "annotations": []}]));
    // 21 含缓存 5，8 个输出 token：Chat 的口径原样搬运到 Responses
    assert_eq!(v["usage"]["input_tokens"], 21);
    assert_eq!(v["usage"]["input_tokens_details"]["cached_tokens"], 5);
    assert_eq!(v["usage"]["output_tokens"], 8);
    assert_eq!(v["usage"]["total_tokens"], 29);

    let up = ups.last();
    assert_eq!(up.path, "/v1/chat/completions");
    assert_eq!(up.header("authorization"), Some(format!("Bearer {UP_KEY_A}").as_str()));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    let b = up.json();
    assert_eq!(b["model"], "gpt-up");
    assert_eq!(b["messages"][0], json!({"role": "system", "content": SYSTEM_PROMPT}), "`instructions` becomes the system message");
    assert_eq!(b["messages"][1]["role"], "user");
    assert_eq!(b["messages"].as_array().unwrap().len(), 2);
    assert_eq!((b["max_tokens"].as_u64(), b["temperature"].as_f64()), (Some(200), Some(0.2)));
    for k in ["instructions", "input", "truncation", "mystery_field", "max_output_tokens", "store", "stream", "stream_options"] {
        assert!(b.get(k).is_none(), "`{k}` must not leak into the Chat request: {b}");
    }

    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 21, 8, 5, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_request_to_chat_upstream_multi_turn_tool_chain_image_and_function_call_output() {
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

    let resp = h.responses(rs_chain_request("p1/gpt-up", "get_weather")).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("x-router-warnings").is_none(), "a fully supported request has no warnings");
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "completed");
    let out = v["output"].as_array().unwrap();
    assert_eq!(out.len(), 1, "{v}");
    assert_eq!(out[0]["type"], "function_call");
    assert_eq!((out[0]["call_id"].as_str(), out[0]["name"].as_str(), out[0]["status"].as_str()), (Some("call_2"), Some("get_weather"), Some("completed")));
    assert_eq!(serde_json::from_str::<Value>(out[0]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Lyon"}));
    assert_eq!((v["usage"]["input_tokens"].as_u64(), v["usage"]["output_tokens"].as_u64(), v["usage"]["total_tokens"].as_u64()), (Some(90), Some(12), Some(102)));

    let b = ups.last().json();
    let msgs = b["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "tool", "user"], "{b}");
    assert_eq!(msgs[0]["content"], SYSTEM_PROMPT);
    let parts = msgs[1]["content"].as_array().unwrap();
    assert_eq!(parts[0], json!({"type": "text", "text": IMAGE_PROMPT}));
    assert_eq!(parts[1]["type"], "image_url");
    assert_eq!(parts[1]["image_url"]["url"], png_data_url(), "the base64 image survives as a data URL");
    assert_eq!(msgs[2]["content"], "Let me check.");
    assert_eq!(msgs[2]["tool_calls"].as_array().unwrap().len(), 1);
    assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
    assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(serde_json::from_str::<Value>(msgs[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Paris"}));
    assert_eq!(msgs[3], json!({"role": "tool", "tool_call_id": "call_1", "content": "18C and sunny"}), "function_call_output pairs with its call_id");
    assert_eq!(msgs[4], json!({"role": "user", "content": "Now do Lyon too."}));
    assert_eq!(b["tools"], json!([{"type": "function", "function": {"name": "get_weather", "description": "Weather by city", "parameters": weather_params()}}]));
    assert_eq!(b["tool_choice"], "auto");
    assert_eq!(b["max_tokens"], 300);

    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 90, 12, 0, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_request_to_chat_upstream_stream_event_order_sequence_numbers_and_final_usage() {
    let chunks = vec![
        sse_data(&oa_chunk(json!({"role": "assistant", "content": ""}), None)),
        sse_data(&oa_chunk(json!({"reasoning_content": "Hmm. "}), None)),
        sse_data(&oa_chunk(json!({"content": "It is "}), None)),
        sse_data(&oa_chunk(json!({"content": "Paris."}), None)),
        sse_data(&oa_chunk(json!({}), Some("stop"))),
        sse_data(&oa_usage_chunk(40, 17)),
        "data: [DONE]\n\n".to_string(),
    ];
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let resp = h.responses(json!({"model": "p1/gpt-up", "stream": true, "instructions": SYSTEM_PROMPT, "input": "capital of France?"})).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    let evs = rs_events(&text);
    assert_eq!(
        ev_types(&evs),
        vec![
            "response.created",
            "response.in_progress",
            // 推理
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.reasoning_summary_part.done",
            "response.output_item.done",
            // 文本
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ],
        "{text}"
    );
    assert_sequence_numbers(&evs);
    assert_eq!(evs[0]["response"]["status"], "in_progress");
    assert_eq!(evs[2]["item"]["type"], "reasoning");
    assert_eq!(evs[4]["delta"], "Hmm. ");
    assert_eq!(evs[8]["item"]["type"], "message");
    assert_eq!(evs[8]["item"]["status"], "in_progress");
    assert_eq!(evs[9]["part"], json!({"type": "output_text", "text": "", "annotations": []}));
    assert_eq!(text_deltas(&evs), "It is Paris.");
    let (done_text, done_part, done_item) = (&evs[12], &evs[13], &evs[14]);
    assert_eq!(done_text["text"], "It is Paris.");
    assert_eq!(done_part["part"]["text"], "It is Paris.");
    assert_eq!(done_item["item"]["content"][0]["text"], "It is Paris.");
    // item_id / output_index 在同一个 item 的各事件里一致
    assert_eq!(evs[10]["item_id"], evs[8]["item"]["id"]);
    assert_eq!(evs[10]["output_index"], 1);
    assert_eq!(done_item["item"]["id"], evs[8]["item"]["id"]);

    let resp = completed_response(&evs);
    assert_eq!(resp["status"], "completed");
    assert_eq!(resp["id"], evs[0]["response"]["id"], "created and completed describe the same response");
    let out = resp["output"].as_array().unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["type"], "reasoning");
    assert_eq!(out[1]["content"][0]["text"], "It is Paris.");
    assert_eq!(resp["usage"]["input_tokens"], 40, "the final upstream usage, not the zero at the start");
    assert_eq!(resp["usage"]["output_tokens"], 17);
    assert_eq!(resp["usage"]["total_tokens"], 57);

    let ub = ups.last().json();
    assert_eq!(ub["stream"], true);
    assert_eq!(ub["stream_options"], json!({"include_usage": true}), "usage is requested from the Chat upstream");
    assert_eq!(ub["messages"][0], json!({"role": "system", "content": SYSTEM_PROMPT}));

    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 40, 17, 0, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_request_to_chat_upstream_stream_tool_call_emits_function_call_events() {
    let chunks = vec![
        sse_data(&oa_chunk(json!({"role": "assistant", "content": "Checking."}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "get_weather", "arguments": ""}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"city\":"}}]}), None)),
        sse_data(&oa_chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"Paris\"}"}}]}), None)),
        sse_data(&oa_chunk(json!({}), Some("tool_calls"))),
        sse_data(&oa_usage_chunk(88, 21)),
        "data: [DONE]\n\n".to_string(),
    ];
    let ups = FakeUpstream::start(move |_, _| Reply::sse(chunks.clone())).await;
    let h = Harness::new().await;
    h.simple(&ups, "p1", "openai-compatible", "gpt-up", "p1/gpt-up").await;

    let mut req = rs_chain_request("p1/gpt-up", "get_weather");
    req["stream"] = json!(true);
    let resp = h.responses(req).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let evs = rs_events(&text);
    let types = ev_types(&evs);
    assert_eq!(types.first(), Some(&"response.created"), "{text}");
    assert_eq!(types[1], "response.in_progress");
    assert_sequence_numbers(&evs);
    // 文本 item 在前，function_call item 在后，且事件不交错
    let added: Vec<&Value> = evs.iter().filter(|e| e["type"] == "response.output_item.added").collect();
    assert_eq!(added.len(), 2, "{text}");
    assert_eq!(added[0]["item"]["type"], "message");
    assert_eq!(added[1]["item"]["type"], "function_call");
    assert_eq!((added[1]["item"]["call_id"].as_str(), added[1]["item"]["name"].as_str(), added[1]["item"]["status"].as_str()), (Some("call_a"), Some("get_weather"), Some("in_progress")));
    assert_eq!(added[1]["output_index"], 1);
    let arg_deltas: String = evs.iter().filter(|e| e["type"] == "response.function_call_arguments.delta").filter_map(|e| e["delta"].as_str()).collect();
    assert_eq!(arg_deltas, "{\"city\":\"Paris\"}");
    let args_done = evs.iter().find(|e| e["type"] == "response.function_call_arguments.done").expect("arguments.done");
    assert_eq!(args_done["arguments"], "{\"city\":\"Paris\"}");
    let pos = |t: &str| types.iter().position(|x| *x == t).unwrap_or_else(|| panic!("missing {t}: {types:?}"));
    assert!(pos("response.output_text.done") < pos("response.function_call_arguments.delta"), "{types:?}");
    assert!(pos("response.function_call_arguments.delta") < pos("response.function_call_arguments.done"), "{types:?}");
    assert!(pos("response.function_call_arguments.done") < pos("response.completed"), "{types:?}");
    assert_eq!(text_deltas(&evs), "Checking.");

    let resp = completed_response(&evs);
    let out = resp["output"].as_array().unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["type"], "function_call");
    assert_eq!((out[1]["call_id"].as_str(), out[1]["status"].as_str()), (Some("call_a"), Some("completed")));
    assert_eq!(out[1]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!((resp["usage"]["input_tokens"].as_u64(), resp["usage"]["output_tokens"].as_u64()), (Some(88), Some(21)));

    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 88, 21, 0, "upstream"));
    h.stop().await;
}

// ───────────── 3. Responses 客户端 → Anthropic / Gemini 上游 ─────────────

#[tokio::test]
async fn responses_request_to_anthropic_upstream_nonstream_system_image_tool_chain_and_usage() {
    let ups = FakeUpstream::start(|_, _| {
        Reply::json(
            200,
            json!({"id": "msg_9", "type": "message", "role": "assistant", "model": "claude-up",
                   "content": [{"type": "thinking", "thinking": "let me see", "signature": "sig"}, {"type": "text", "text": "Looks like a pixel."},
                               {"type": "tool_use", "id": "toolu_9", "name": "get_weather", "input": {"city": "Lyon"}}],
                   "stop_reason": "tool_use", "usage": {"input_tokens": 50, "output_tokens": 20, "cache_read_input_tokens": 10}}),
        )
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let resp = h.responses(rs_chain_request("an1/claude-up", "get_weather")).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!((v["object"].as_str(), v["status"].as_str(), v["model"].as_str()), (Some("response"), Some("completed"), Some("claude-up")));
    let out = v["output"].as_array().unwrap();
    let types: Vec<&str> = out.iter().map(|o| o["type"].as_str().unwrap()).collect();
    assert_eq!(types, vec!["reasoning", "message", "function_call"], "{v}");
    assert_eq!(out[0]["summary"][0]["text"], "let me see");
    assert_eq!(out[1]["content"][0]["text"], "Looks like a pixel.");
    assert_eq!((out[2]["call_id"].as_str(), out[2]["name"].as_str()), (Some("toolu_9"), Some("get_weather")));
    assert_eq!(serde_json::from_str::<Value>(out[2]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Lyon"}));
    // Anthropic 的 input_tokens 不含缓存：50 + 10 = 60，规范化后 Responses 的 input_tokens 含缓存
    assert_eq!(v["usage"]["input_tokens"], 60);
    assert_eq!(v["usage"]["input_tokens_details"]["cached_tokens"], 10);
    assert_eq!(v["usage"]["output_tokens"], 20);

    let up = ups.last();
    assert_eq!(up.path, "/v1/messages");
    assert_eq!(up.header("x-api-key"), Some(UP_KEY_A));
    assert!(up.header("authorization").is_none());
    assert_eq!(up.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    let b = up.json();
    assert_eq!(b["model"], "claude-up");
    assert_eq!(b["system"], SYSTEM_PROMPT, "`instructions` becomes the Anthropic system prompt");
    assert_eq!(b["max_tokens"], 300);
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 3, "{b}");
    assert_eq!(m[0]["role"], "user");
    assert_eq!(m[0]["content"][0], json!({"type": "text", "text": IMAGE_PROMPT}));
    assert_eq!(m[0]["content"][1], json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": PNG_B64}}));
    assert_eq!(m[1]["role"], "assistant");
    assert_eq!(m[1]["content"][0], json!({"type": "text", "text": "Let me check."}));
    assert_eq!(m[1]["content"][1], json!({"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"city": "Paris"}}));
    assert_eq!(m[2]["role"], "user");
    assert_eq!(m[2]["content"][0]["type"], "tool_result", "tool_result comes first in the user turn");
    assert_eq!(m[2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(m[2]["content"][1], json!({"type": "text", "text": "Now do Lyon too."}));
    assert_eq!(b["tools"][0]["name"], "get_weather");
    assert_eq!(b["tools"][0]["input_schema"], weather_params());
    assert_eq!(b["tool_choice"]["type"], "auto");

    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 60, 20, 10, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_request_to_anthropic_upstream_stream_event_order_and_final_usage() {
    let ups = FakeUpstream::start(|_, _| Reply::sse(an_text_stream())).await;
    let h = Harness::new().await;
    h.simple(&ups, "an1", "anthropic-compatible", "claude-up", "an1/claude-up").await;

    let resp = h.responses(json!({"model": "an1/claude-up", "stream": true, "instructions": SYSTEM_PROMPT, "input": "say hello world"})).await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    let evs = rs_events(&text);
    assert_eq!(
        ev_types(&evs),
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.completed",
        ],
        "{text}"
    );
    assert_sequence_numbers(&evs);
    assert_eq!(text_deltas(&evs), "Hello world");
    let resp = completed_response(&evs);
    assert_eq!(resp["model"], "claude-up");
    assert_eq!(resp["output"][0]["content"][0]["text"], "Hello world");
    // message_start: input 25 + cache_read 10；message_delta: output 15
    assert_eq!(resp["usage"]["input_tokens"], 35);
    assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 10);
    assert_eq!(resp["usage"]["output_tokens"], 15);

    let ub = ups.last().json();
    assert_eq!(ub["stream"], true);
    assert_eq!(ub["system"], SYSTEM_PROMPT);
    let rows = h.usage(1).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 35, 15, 10, "upstream"));
    h.stop().await;
}

/// Gemini 只接受 `[A-Za-z_][A-Za-z0-9_.:-]*` 的工具名；`lookup weather` 会被改写成 `lookup_weather`。
const ORIGINAL_TOOL: &str = "lookup weather";
const WIRE_TOOL: &str = "lookup_weather";

#[tokio::test]
async fn responses_request_to_gemini_upstream_nonstream_renamed_tool_round_trips_to_the_original_name() {
    let ups = FakeUpstream::start(|_, n| {
        if n == 0 {
            Reply::json(200, gm_tool_response(WIRE_TOOL, json!({"city": "Lyon"})))
        } else {
            Reply::json(200, gm_response("It is sunny in Lyon."))
        }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;

    let resp = h.responses(rs_chain_request("g1/gem-up", ORIGINAL_TOOL)).await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    let out = v["output"].as_array().unwrap();
    assert_eq!(out.len(), 1, "{v}");
    assert_eq!(out[0]["type"], "function_call");
    assert_eq!(out[0]["name"], ORIGINAL_TOOL, "the Gemini-safe name is mapped back to what the client defined");
    assert_eq!(serde_json::from_str::<Value>(out[0]["arguments"].as_str().unwrap()).unwrap(), json!({"city": "Lyon"}));
    assert!(out[0]["call_id"].as_str().is_some_and(|c| !c.is_empty()), "Gemini has no call ids; one is generated: {v}");
    assert_eq!((v["usage"]["input_tokens"].as_u64(), v["usage"]["output_tokens"].as_u64()), (Some(40), Some(12)));

    let up = ups.last();
    assert_eq!(up.path, "/v1beta/models/gem-up:generateContent");
    assert_eq!(up.query, None, "the key never travels in the URL");
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("authorization"), None);
    assert_eq!(up.header("user-agent"), Some(expected_ua().as_str()));
    let b = up.json();
    assert_eq!(b["systemInstruction"]["parts"][0]["text"], SYSTEM_PROMPT);
    assert_eq!(b["generationConfig"]["maxOutputTokens"], 300);
    assert_eq!(b["tools"][0]["functionDeclarations"][0]["name"], WIRE_TOOL, "{b}");
    assert!(!b.to_string().contains(ORIGINAL_TOOL), "the original name must not reach Gemini anywhere: {b}");
    let contents = b["contents"].as_array().unwrap();
    let roles: Vec<&str> = contents.iter().map(|c| c["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "model", "user"], "{b}");
    assert_eq!(contents[0]["parts"][0]["text"], IMAGE_PROMPT);
    assert_eq!(contents[0]["parts"][1]["inlineData"], json!({"mimeType": "image/png", "data": PNG_B64}));
    assert_eq!(contents[1]["parts"][0]["text"], "Let me check.");
    assert_eq!(contents[1]["parts"][1]["functionCall"]["name"], WIRE_TOOL);
    assert_eq!(contents[1]["parts"][1]["functionCall"]["args"], json!({"city": "Paris"}));
    let fr = &contents[2]["parts"][0]["functionResponse"];
    assert_eq!(fr["name"], WIRE_TOOL, "the function response pairs with the call by (renamed) name");
    assert!(fr["response"].to_string().contains("18C and sunny"), "{fr}");
    assert_eq!(contents[2]["parts"][1]["text"], "Now do Lyon too.");

    // 第二次：文本回复，用量 21 含缓存 5；候选 8 + 思考 3 = 11
    let resp = h.responses(json!({"model": "g1/gem-up", "input": "again"})).await;
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["output"][0]["content"][0]["text"], "It is sunny in Lyon.");
    assert_eq!((v["usage"]["input_tokens"].as_u64(), v["usage"]["output_tokens"].as_u64()), (Some(21), Some(11)));
    assert_eq!(v["usage"]["input_tokens_details"]["cached_tokens"], 5);

    let rows = h.usage(2).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 40, 12, 0, "upstream"));
    assert_eq!(usage_tuple(&rows[1]), ("openai-responses", "success", 21, 11, 5, "upstream"));
    h.stop().await;
}

#[tokio::test]
async fn responses_request_to_gemini_upstream_stream_restores_tool_names_and_reports_final_usage() {
    let ups = FakeUpstream::start(|_, n| {
        if n == 0 {
            Reply::sse(gm_tool_stream(WIRE_TOOL, json!({"city": "Lyon"})).iter().map(gm_sse).collect())
        } else {
            Reply::sse(gm_text_stream().iter().map(gm_sse).collect())
        }
    })
    .await;
    let h = Harness::new().await;
    h.simple(&ups, "g1", "gemini", "gem-up", "g1/gem-up").await;

    let mut req = rs_chain_request("g1/gem-up", ORIGINAL_TOOL);
    req["stream"] = json!(true);
    let resp = h.responses(req).await;
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    let evs = rs_events(&text);
    assert_sequence_numbers(&evs);
    let added: Vec<&Value> = evs.iter().filter(|e| e["type"] == "response.output_item.added").collect();
    assert_eq!(added.len(), 2, "{text}");
    assert_eq!(added[0]["item"]["type"], "message");
    assert_eq!(added[1]["item"]["type"], "function_call");
    assert_eq!(added[1]["item"]["name"], ORIGINAL_TOOL, "streamed tool names are restored too");
    let arg_deltas: String = evs.iter().filter(|e| e["type"] == "response.function_call_arguments.delta").filter_map(|e| e["delta"].as_str()).collect();
    assert_eq!(serde_json::from_str::<Value>(&arg_deltas).unwrap(), json!({"city": "Lyon"}));
    assert_eq!(text_deltas(&evs), "Checking.");
    let resp = completed_response(&evs);
    assert_eq!(resp["output"][1]["name"], ORIGINAL_TOOL);
    assert_eq!((resp["usage"]["input_tokens"].as_u64(), resp["usage"]["output_tokens"].as_u64()), (Some(30), Some(9)));

    let up = ups.last();
    assert_eq!(up.path, "/v1beta/models/gem-up:streamGenerateContent");
    assert_eq!(up.query.as_deref(), Some("alt=sse"));
    assert_eq!(up.header("x-goog-api-key"), Some(UP_KEY_A));
    assert_eq!(up.header("accept"), Some("text/event-stream"));
    assert_eq!(up.json()["tools"][0]["functionDeclarations"][0]["name"], WIRE_TOOL);

    // 文本流：25 含缓存 10；候选 6 + 思考 2
    let resp = h.responses(json!({"model": "g1/gem-up", "stream": true, "input": "hi"})).await;
    let evs = rs_events(&resp.text().await.unwrap());
    assert_eq!(text_deltas(&evs), "Hello world");
    let resp = completed_response(&evs);
    assert_eq!((resp["usage"]["input_tokens"].as_u64(), resp["usage"]["output_tokens"].as_u64()), (Some(25), Some(8)));
    assert_eq!(resp["usage"]["input_tokens_details"]["cached_tokens"], 10);

    let rows = h.usage(2).await;
    assert_eq!(usage_tuple(&rows[0]), ("openai-responses", "success", 30, 9, 0, "upstream"));
    assert_eq!(usage_tuple(&rows[1]), ("openai-responses", "success", 25, 8, 10, "upstream"));
    h.stop().await;
}

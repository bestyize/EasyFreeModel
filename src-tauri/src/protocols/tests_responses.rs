//! OpenAI Responses 协议测试：item 解码编码、响应、流式事件全集（编码 / 增量解码）、错误、跨协议转换。

use serde_json::{json, Value};

use super::anthropic::{self, AnthropicStreamEncoder};
use super::canonical::*;
use super::gemini::{self, GeminiStreamEncoder};
use super::ollama::{self, OllamaStreamEncoder};
use super::openai_chat::{self, OpenAiStreamEncoder};
use super::openai_responses::{self, ResponsesStreamDecoder, ResponsesStreamEncoder};
use super::testkit::*;
use super::*;

macro_rules! fixture {
    ($name:literal) => {
        fx(include_str!(concat!("fixtures/", $name, ".json")))
    };
}

const RESPONSES_STREAM: &str = include_str!("fixtures/responses_stream.sse");

fn rs(v: &Value) -> CanonicalRequest {
    openai_responses::decode_request(v).expect("responses decode")
}
fn oa(v: &Value) -> CanonicalRequest {
    openai_chat::decode_request(v).expect("openai decode")
}
fn an(v: &Value) -> CanonicalRequest {
    anthropic::decode_request(v).expect("anthropic decode")
}

/// Responses 总是返回 usage（include_usage=true）；其余与 Chat 解码结果对齐后再比较。
fn align(r: CanonicalRequest, mut other: CanonicalRequest) -> (CanonicalRequest, CanonicalRequest) {
    other.include_usage = true;
    (semantic(r), semantic(other))
}

fn decode_events(bytes: &[u8], size: usize) -> Vec<StreamEvent> {
    let mut dec = ResponsesStreamDecoder::new();
    let mut out = Vec::new();
    for chunk in bytes.chunks(size) {
        out.extend(dec.push(chunk).expect("push"));
    }
    out.extend(dec.finish().expect("finish"));
    out
}

fn sse_of(events: &[Value]) -> String {
    events.iter().map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e)).collect()
}

fn run_encoder(enc: &mut dyn StreamEncoder, events: Vec<StreamEvent>) -> Vec<SseFrame> {
    events.into_iter().flat_map(|e| enc.push(e).expect("valid sequence")).collect()
}

fn call(id: &str, name: &str, args: Value) -> ContentPart {
    ContentPart::ToolUse { id: id.into(), name: name.into(), input: args }
}
fn result(id: &str, text: &str) -> ContentPart {
    ContentPart::ToolResult { tool_use_id: id.into(), content: vec![ContentPart::text(text)], is_error: false }
}

fn user_req(text: &str) -> CanonicalRequest {
    CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![ContentPart::text(text)])],
        ..Default::default()
    }
}

// ───────────── 纯文本 / system / 参数 ─────────────

#[test]
fn text_request_decodes_to_same_canonical_as_openai() {
    let (r, mut o) = align(rs(&fixture!("responses_text_request")), oa(&fixture!("openai_text_request")));
    o.params.stop.clear(); // Responses 没有 stop
    assert_eq!(r, o);
    // instructions 在前，input 里的 developer 消息在后
    assert_eq!(r.system, vec!["You are helpful.", "Answer briefly."]);
    assert_eq!(r.params.max_tokens, Some(256));
    assert_eq!(r.user.as_deref(), Some("u-1"));
}

#[test]
fn input_may_be_a_plain_string() {
    let req = rs(&json!({"model": "m", "input": "What is the capital of France?", "instructions": "Be terse."}));
    assert_eq!(req.system, vec!["Be terse."]);
    assert_eq!(req.messages, vec![Message::user(vec![ContentPart::text("What is the capital of France?")])]);
    assert!(req.warnings.is_empty());
}

#[test]
fn mid_conversation_developer_message_is_hoisted_with_warning() {
    let req = rs(&json!({"model": "m", "input": [
        {"role": "user", "content": "hi"},
        {"role": "developer", "content": [{"type": "input_text", "text": "late rule"}]}
    ]}));
    assert_eq!(req.system, vec!["late rule"]);
    assert_eq!(req.messages.len(), 1);
    assert!(has_warning(&req.warnings, "hoisted"));
}

#[test]
fn text_request_encode_joins_instructions() {
    let canonical = rs(&fixture!("responses_text_request"));
    let enc = openai_responses::encode_request(&canonical).unwrap();
    assert_eq!(enc.body["instructions"], "You are helpful.\n\nAnswer briefly.");
    assert_eq!(enc.body["store"], false);
    assert_eq!(
        enc.body["input"][1],
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Hi!"}]})
    );
    assert_eq!(enc.body["max_output_tokens"], 256);
    assert_eq!(enc.body["temperature"], 0.2);
    assert_eq!(enc.body["user"], "u-1");
    assert!(enc.body.get("stream").is_none());
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
}

#[test]
fn unsupported_sampling_parameters_warn_on_encode() {
    let mut req = user_req("x");
    req.params = GenerationParams {
        stop: vec!["END".into()],
        top_k: Some(3),
        seed: Some(1),
        presence_penalty: Some(0.1),
        frequency_penalty: Some(0.1),
        ..Default::default()
    };
    let enc = openai_responses::encode_request(&req).unwrap();
    for name in ["stop", "top_k", "seed", "presence_penalty", "frequency_penalty"] {
        assert!(has_warning(&enc.warnings, &format!("{name} is not supported")), "{name}: {:?}", enc.warnings);
    }
}

// ───────────── 有状态 / 内置能力：明确拒绝或告警 ─────────────

#[test]
fn stateful_features_are_rejected_explicitly() {
    let base = |extra: Value| {
        let mut b = json!({"model": "m", "input": "hi"});
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };
    for (extra, needle) in [
        (json!({"previous_response_id": "resp_1"}), "previous_response_id"),
        (json!({"conversation": "conv_1"}), "conversation"),
        (json!({"prompt": {"id": "pmpt_1"}}), "prompt"),
        (json!({"background": true}), "background"),
    ] {
        match openai_responses::decode_request(&base(extra)) {
            Err(ProtocolError::Unsupported(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("expected Unsupported for {needle}, got {other:?}"),
        }
    }
    let item_ref =
        openai_responses::decode_request(&json!({"model": "m", "input": [{"type": "item_reference", "id": "msg_1"}]}));
    assert!(matches!(item_ref, Err(ProtocolError::Unsupported(_))));
    // null / false 等同于未设置，且不产生告警
    let ok = rs(&base(json!({"previous_response_id": null, "background": false, "store": true})));
    assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);
}

#[test]
fn unknown_fields_and_hosted_items_become_warnings() {
    let secret = "TOP-SECRET-PROMPT";
    let req = rs(&json!({
        "model": "m",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": secret}]},
            {"type": "web_search_call", "id": "ws_1", "status": "completed"},
            {"type": "computer_call", "id": "cu_1", "call_id": "c", "action": {"type": "click"}}
        ],
        "text": {"format": {"type": "json_schema", "name": "x", "schema": {}}},
        "metadata": {"k": "v"}, "truncation": "auto", "include": ["reasoning.encrypted_content"],
        "service_tier": "flex", "prompt_cache_key": "abc", "max_tool_calls": 2, "top_logprobs": 3,
        "reasoning": {"effort": "low", "generate_summary": "concise", "extra": 1}
    }));
    for k in [
        "text",
        "metadata",
        "truncation",
        "include",
        "service_tier",
        "prompt_cache_key",
        "max_tool_calls",
        "top_logprobs",
    ] {
        assert!(has_warning(&req.warnings, &format!("unsupported field ignored: {k}")), "{k}: {:?}", req.warnings);
    }
    assert!(has_warning(&req.warnings, "unsupported field ignored: reasoning.extra"));
    assert!(has_warning(&req.warnings, "input[1]: item type `web_search_call`"));
    assert!(has_warning(&req.warnings, "input[2]: item type `computer_call`"));
    assert_eq!(
        req.reasoning,
        Some(ReasoningConfig { effort: Some("low".into()), budget_tokens: None, include_thoughts: Some(true) })
    );
    assert!(req.warnings.iter().all(|w| !w.contains(secret)), "{:?}", req.warnings);
}

#[test]
fn invalid_requests_are_rejected() {
    for bad in [
        json!({"model": "m"}),
        json!({"model": "", "input": "x"}),
        json!({"model": "m", "input": 5}),
        json!({"model": "m", "input": []}),
        json!({"model": "m", "input": [{"foo": 1}]}),
        json!({"model": "m", "input": [{"type": "function_call", "name": "f", "arguments": "{}"}]}),
        json!({"model": "m", "input": [{"type": "function_call", "call_id": "c", "name": "f", "arguments": "{oops"}]}),
        json!({"model": "m", "input": [{"type": "function_call_output", "output": "x"}]}),
        json!({"model": "m", "input": [{"type": "message", "role": "user", "content": [{"type": "input_image"}]}]}),
        json!({"model": "m", "input": "x", "tool_choice": "sometimes"}),
        json!({"model": "m", "input": "x", "tools": [{"type": "function"}]}),
        json!({"model": "m", "input": "x", "max_output_tokens": -1}),
        json!([1]),
    ] {
        assert!(matches!(openai_responses::decode_request(&bad), Err(ProtocolError::InvalidRequest(_))), "{bad}");
    }
    for unsupported in [
        json!({"model": "m", "input": [{"type": "message", "role": "user", "content": [{"type": "input_audio", "data": "x"}]}]}),
        json!({"model": "m", "input": [{"type": "message", "role": "tool", "content": "x"}]}),
    ] {
        assert!(
            matches!(openai_responses::decode_request(&unsupported), Err(ProtocolError::Unsupported(_))),
            "{unsupported}"
        );
    }
}

// ───────────── 工具调用与多轮工具链 ─────────────

#[test]
fn tool_chain_decodes_identically_to_openai() {
    let (r, o) = align(rs(&fixture!("responses_tool_chain_request")), oa(&fixture!("openai_tool_chain_request")));
    assert_eq!(r, o);
    // function_call 与前面的 assistant 文本合并为同一条助手消息
    assert_eq!(r.messages.len(), 5);
    assert!(
        matches!(&r.messages[3].content[..], [ContentPart::Text { .. }, ContentPart::ToolUse { id, .. }] if id == "call_3")
    );
}

#[test]
fn tool_chain_encodes_to_function_call_items() {
    let canonical = oa(&fixture!("openai_tool_chain_request"));
    let enc = openai_responses::encode_request(&canonical).unwrap();
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
    assert_eq!(
        enc.body,
        json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Weather in Paris and Tokyo, then Berlin?"}]},
                {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
                {"type": "function_call", "call_id": "call_2", "name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "18C sunny"},
                {"type": "function_call_output", "call_id": "call_2", "output": "25C rain"},
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Let me also check Berlin."}]},
                {"type": "function_call", "call_id": "call_3", "name": "get_weather", "arguments": "{\"city\":\"Berlin\"}"},
                {"type": "function_call_output", "call_id": "call_3", "output": "12C cloudy"}
            ],
            "store": false,
            "max_output_tokens": 512,
            "tools": [{
                "type": "function", "name": "get_weather", "description": "Get current weather for a city",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
                "strict": false
            }],
            "tool_choice": "auto"
        })
    );
    let (back, canonical) = align(rs(&enc.body), canonical);
    assert_eq!(back, canonical);
}

#[test]
fn responses_fixture_roundtrips_through_encode() {
    for name in ["tool_chain", "image"] {
        let body = match name {
            "tool_chain" => fixture!("responses_tool_chain_request"),
            _ => fixture!("responses_image_request"),
        };
        let first = rs(&body);
        let again = rs(&openai_responses::encode_request(&first).unwrap().body);
        assert_eq!(semantic(again), semantic(first), "{name}");
    }
}

#[test]
fn function_tools_are_always_non_strict_with_object_parameters() {
    let mut req = user_req("x");
    let t = |name: &str, schema: Value| ToolDefinition { name: name.into(), description: None, input_schema: schema };
    req.tools = vec![
        t("a", json!({"type": "object"})),
        t("b", json!(null)),
        t("c", json!({"type": "object", "properties": {"x": {"type": "string"}}})),
    ];
    req.parallel_tool_calls = Some(false);
    req.tool_choice = Some(ToolChoice::Tool { name: "c".into() });
    let enc = openai_responses::encode_request(&req).unwrap();
    let tools = enc.body["tools"].as_array().unwrap();
    assert!(tools.iter().all(|t| t["strict"] == false && t["type"] == "function"));
    assert_eq!(tools[0]["parameters"], json!({"type": "object", "properties": {}}));
    assert_eq!(tools[1]["parameters"], json!({"type": "object", "properties": {}}));
    assert!(tools[0].get("description").is_none());
    assert_eq!(enc.body["parallel_tool_calls"], false);
    assert_eq!(enc.body["tool_choice"], json!({"type": "function", "name": "c"}));
    for (choice, expected) in
        [(ToolChoice::Auto, json!("auto")), (ToolChoice::None, json!("none")), (ToolChoice::Any, json!("required"))]
    {
        req.tool_choice = Some(choice);
        assert_eq!(openai_responses::encode_request(&req).unwrap().body["tool_choice"], expected);
    }
}

#[test]
fn tool_declarations_and_choices_decode() {
    let req = rs(&json!({
        "model": "m", "input": "x",
        "tools": [
            {"type": "function", "name": "f", "description": "d", "parameters": null, "strict": true},
            {"type": "web_search_preview"},
            {"type": "file_search", "vector_store_ids": ["vs"]},
            {"type": "mcp", "server_label": "x", "server_url": "https://example.com"},
            {"type": "custom", "name": "apply_patch", "description": "Edit files", "format": {"type": "grammar", "syntax": "lark", "definition": "start: x"}}
        ],
        "tool_choice": {"type": "function", "name": "f"}
    }));
    assert_eq!(req.tools.len(), 2);
    assert_eq!(req.tools[0].input_schema, json!({"type": "object", "properties": {}}));
    assert_eq!(req.tool_choice, Some(ToolChoice::Tool { name: "f".into() }));
    assert!(has_warning(&req.warnings, "tools[0]: strict mode"));
    for t in ["web_search_preview", "file_search", "mcp"] {
        assert!(
            has_warning(&req.warnings, &format!("hosted tool `{t}` is not supported and was dropped")),
            "{t}: {:?}",
            req.warnings
        );
    }
    assert!(has_warning(&req.warnings, "custom tool `apply_patch` was exposed as a function"));
    assert_eq!(req.tools[1].description.as_deref(), Some("Edit files\n\nlark\nstart: x"));
    assert_eq!(req.tools[1].input_schema["required"], json!(["input"]));

    let choice = |v: Value| rs(&json!({"model": "m", "input": "x", "tool_choice": v}));
    assert_eq!(choice(json!("auto")).tool_choice, Some(ToolChoice::Auto));
    assert_eq!(choice(json!("none")).tool_choice, Some(ToolChoice::None));
    assert_eq!(choice(json!("required")).tool_choice, Some(ToolChoice::Any));
    let one =
        choice(json!({"type": "allowed_tools", "mode": "required", "tools": [{"type": "function", "name": "f"}]}));
    assert_eq!(one.tool_choice, Some(ToolChoice::Tool { name: "f".into() }));
    let several = choice(
        json!({"type": "allowed_tools", "mode": "required", "tools": [{"type": "function", "name": "f"}, {"type": "function", "name": "g"}]}),
    );
    assert_eq!(several.tool_choice, Some(ToolChoice::Any));
    assert!(has_warning(&several.warnings, "allowed_tools"));
    let auto = choice(json!({"type": "allowed_tools", "mode": "auto", "tools": [{"type": "function", "name": "f"}]}));
    assert_eq!(auto.tool_choice, Some(ToolChoice::Auto));
    let hosted = choice(json!({"type": "web_search_preview"}));
    assert_eq!(hosted.tool_choice, None);
    assert!(has_warning(&hosted.warnings, "hosted tool"));
}

#[test]
fn custom_tool_calls_are_translated_to_a_single_input_string() {
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+hi\n*** End Patch";
    let req = rs(&json!({
        "model": "m",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "patch it"}]},
            {"type": "custom_tool_call", "call_id": "c1", "name": "apply_patch", "input": patch},
            {"type": "custom_tool_call_output", "call_id": "c1", "output": "Done!"}
        ],
        "tools": [{"type": "custom", "name": "apply_patch"}]
    }));
    assert_eq!(req.messages[1].content, vec![call("c1", "apply_patch", json!({"input": patch}))]);
    assert_eq!(req.messages[2].content, vec![result("c1", "Done!")]);
    let enc = openai_responses::encode_request(&req).unwrap();
    assert_eq!(enc.body["input"][1]["type"], "function_call");
    assert_eq!(
        serde_json::from_str::<Value>(enc.body["input"][1]["arguments"].as_str().unwrap()).unwrap(),
        json!({"input": patch})
    );
}

#[test]
fn long_call_ids_are_shortened_consistently() {
    let long = "x".repeat(100);
    let exact = "y".repeat(64);
    let mut req = user_req("go");
    req.messages.push(Message::assistant(vec![call(&long, "f", json!({})), call(&exact, "f", json!({}))]));
    req.messages.push(Message::user(vec![result(&long, "1"), result(&exact, "2")]));
    let enc = openai_responses::encode_request(&req).unwrap();
    let items = enc.body["input"].as_array().unwrap();
    let id = |i: usize| items[i]["call_id"].as_str().unwrap().to_string();
    assert!(id(1).len() <= 64 && id(1).starts_with("call_"));
    assert_eq!(id(1), id(3), "function_call and its output must agree");
    assert_eq!(id(2), exact);
    assert_eq!(id(2), id(4));
    assert_eq!(id(1), openai_responses::encode_request(&req).unwrap().body["input"][1]["call_id"], "deterministic");
}

#[test]
fn tool_results_with_images_and_errors() {
    let mut req = user_req("shot?");
    req.messages.push(Message::assistant(vec![call("c1", "screenshot", json!({})), call("c2", "other", json!({}))]));
    req.messages.push(Message::user(vec![
        ContentPart::ToolResult {
            tool_use_id: "c1".into(),
            content: vec![
                ContentPart::text("captured"),
                ContentPart::Image {
                    source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() },
                    detail: None,
                },
            ],
            is_error: false,
        },
        ContentPart::ToolResult {
            tool_use_id: "c2".into(),
            content: vec![ContentPart::text("a"), ContentPart::text("b")],
            is_error: true,
        },
        ContentPart::text("anything else?"),
    ]));
    let enc = openai_responses::encode_request(&req).unwrap();
    let items = enc.body["input"].as_array().unwrap();
    assert_eq!(
        items[3],
        json!({"type": "function_call_output", "call_id": "c1", "output": [
            {"type": "input_text", "text": "captured"},
            {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
        ]})
    );
    assert_eq!(items[4], json!({"type": "function_call_output", "call_id": "c2", "output": "a\nb"}));
    assert_eq!(
        items[5],
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "anything else?"}]})
    );
    assert!(has_warning(&enc.warnings, "is_error"));
}

// ───────────── reasoning ─────────────

#[test]
fn reasoning_items_roundtrip_with_encrypted_content() {
    let body = fixture!("responses_reasoning_chain_request");
    let req = rs(&body);
    assert_eq!(req.messages.len(), 4);
    assert_eq!(
        req.messages[1].content,
        vec![
            ContentPart::Thinking {
                text: "Need weather first.\n\nThen plan.".into(),
                signature: Some("openai-responses:ENC-1".into())
            },
            call("call_1", "get_weather", json!({"city": "Oslo"})),
        ]
    );
    assert_eq!(
        req.reasoning,
        Some(ReasoningConfig { effort: Some("high".into()), budget_tokens: None, include_thoughts: Some(true) })
    );
    assert!(req.stream);

    let enc = openai_responses::encode_request(&req).unwrap();
    let item = &enc.body["input"][1];
    assert_eq!(item["type"], "reasoning");
    assert_eq!(item["encrypted_content"], "ENC-1");
    assert_eq!(item["summary"], json!([{"type": "summary_text", "text": "Need weather first.\n\nThen plan."}]));
    assert!(item["id"].as_str().unwrap().starts_with("rs_"));
    assert_eq!(
        item["id"],
        openai_responses::encode_request(&req).unwrap().body["input"][1]["id"],
        "stable id keeps the prefix cacheable"
    );
    assert_eq!(enc.body["input"][2]["type"], "function_call");
    assert_eq!(enc.body["reasoning"], json!({"effort": "high", "summary": "auto"}));
    assert_eq!(enc.body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(enc.body["stream"], true);
    assert_eq!(enc.body["max_output_tokens"], 2000);
    assert_eq!(semantic(rs(&enc.body)).messages, req.messages);
}

#[test]
fn thinking_without_a_responses_signature_is_not_sent() {
    let mut req = user_req("go");
    req.messages.push(Message::assistant(vec![
        ContentPart::Thinking { text: "claude thoughts".into(), signature: Some("EqQBCkYI".into()) },
        ContentPart::Thinking { text: "gemini".into(), signature: Some("gemini:ABC".into()) },
        ContentPart::Thinking { text: "plain".into(), signature: None },
        ContentPart::text("answer"),
    ]));
    let enc = openai_responses::encode_request(&req).unwrap();
    let items = enc.body["input"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1]["content"][0]["text"], "answer");
    assert!(has_warning(&enc.warnings, "without an OpenAI Responses signature"));
    assert!(enc.body.get("include").is_none() && enc.body.get("reasoning").is_none());
}

#[test]
fn reasoning_config_mapping_on_encode() {
    let with = |r: ReasoningConfig| {
        let mut req = user_req("x");
        req.reasoning = Some(r);
        openai_responses::encode_request(&req).unwrap().body
    };
    let budget = |b| ReasoningConfig { effort: None, budget_tokens: Some(b), include_thoughts: None };
    assert_eq!(with(budget(500))["reasoning"], json!({"effort": "low"}));
    assert_eq!(with(budget(8000))["reasoning"], json!({"effort": "medium"}));
    assert_eq!(with(budget(30000))["reasoning"], json!({"effort": "high"}));
    assert!(with(budget(0)).get("reasoning").is_none());
    assert_eq!(
        with(ReasoningConfig { effort: None, budget_tokens: None, include_thoughts: Some(true) })["reasoning"],
        json!({"summary": "auto"})
    );
    assert_eq!(
        with(ReasoningConfig { effort: Some("minimal".into()), budget_tokens: Some(9999), include_thoughts: None })
            ["reasoning"],
        json!({"effort": "minimal"})
    );
}

// ───────────── 图片 ─────────────

#[test]
fn image_request_matches_openai_and_roundtrips() {
    let body = fixture!("responses_image_request");
    let (r, o) = align(rs(&body), oa(&fixture!("openai_image_request")));
    assert_eq!(r, o);
    let enc = openai_responses::encode_request(&r).unwrap();
    assert_eq!(enc.body["input"], body["input"]);
    assert!(enc.warnings.is_empty());
}

#[test]
fn image_variants_detail_file_id_and_files() {
    let req = rs(&json!({"model": "m", "input": [{"type": "message", "role": "user", "content": [
        {"type": "input_image", "image_url": "https://x/y.png", "detail": "high"},
        {"type": "input_image", "file_id": "file-123"},
        {"type": "input_file", "filename": "a.pdf", "file_data": "data:application/pdf;base64,JVBERi0="},
        {"type": "input_file", "filename": "b.pdf", "file_data": "JVBERi0x"},
        {"type": "input_file", "file_url": "https://x/c.pdf"},
        {"type": "input_file", "file_id": "file-9"}
    ]}]}));
    let parts = &req.messages[0].content;
    assert_eq!(
        parts[0],
        ContentPart::Image { source: ImageSource::Url { url: "https://x/y.png".into() }, detail: Some("high".into()) }
    );
    assert_eq!(
        parts[1],
        ContentPart::Image { source: ImageSource::FileId { file_id: "file-123".into() }, detail: None }
    );
    assert_eq!(
        parts[2],
        ContentPart::Image {
            source: ImageSource::Base64 { media_type: "application/pdf".into(), data: "JVBERi0=".into() },
            detail: None
        }
    );
    assert_eq!(
        parts[3],
        ContentPart::Image {
            source: ImageSource::Base64 { media_type: "application/pdf".into(), data: "JVBERi0x".into() },
            detail: None
        }
    );
    assert!(has_warning(&req.warnings, "non-image binary input"));

    let enc = openai_responses::encode_request(&req).unwrap();
    let out = &enc.body["input"][0]["content"];
    assert_eq!(out[0], json!({"type": "input_image", "image_url": "https://x/y.png", "detail": "high"}));
    assert_eq!(out[1], json!({"type": "input_image", "file_id": "file-123"}));
    assert_eq!(
        out[2],
        json!({"type": "input_file", "filename": "attachment", "file_data": "data:application/pdf;base64,JVBERi0="})
    );
}

#[test]
fn large_base64_image_stays_out_of_text() {
    let big = "A".repeat(2 * 1024 * 1024);
    let req = rs(&json!({"model": "m", "input": [{"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "hi"}, {"type": "input_image", "image_url": format!("data:image/jpeg;base64,{big}")}]}]}));
    assert_eq!(req.text_bytes(), 2);
    assert_eq!(req.media_bytes(), big.len());
}

// ───────────── 响应 ─────────────

#[test]
fn tool_response_matches_openai() {
    let r = openai_responses::decode_response(&fixture!("responses_tool_response")).unwrap();
    let o = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    assert_eq!(r, o, "the hosted web_search_call item is not part of the canonical response");
    assert_eq!(r.stop_reason, StopReason::ToolUse);
    // 用量约定：input 含缓存，output 含推理
    assert_eq!(
        r.usage,
        Usage {
            input_tokens: Some(120),
            output_tokens: Some(20),
            cache_read_tokens: Some(100),
            cache_creation_tokens: None
        }
    );
}

#[test]
fn response_encode_has_every_field_sdk_clients_require() {
    let r = openai_responses::decode_response(&fixture!("responses_tool_response")).unwrap();
    let v = openai_responses::encode_response(&r);
    for k in [
        "id",
        "object",
        "created_at",
        "status",
        "error",
        "incomplete_details",
        "instructions",
        "metadata",
        "model",
        "output",
        "parallel_tool_calls",
        "temperature",
        "tool_choice",
        "tools",
        "top_p",
        "usage",
    ] {
        assert!(v.get(k).is_some(), "missing `{k}`");
    }
    assert_eq!(v["object"], "response");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["created_at"], 1_760_000_000);
    let out = v["output"].as_array().unwrap();
    assert_eq!(out[0]["type"], "message");
    assert_eq!(out[0]["content"][0], json!({"type": "output_text", "text": "Checking.", "annotations": []}));
    assert_eq!(out[1]["type"], "function_call");
    assert_eq!(out[1]["call_id"], "call_9");
    assert_eq!(out[1]["arguments"], "{\"city\":\"Oslo\"}");
    assert!(out[0]["id"].as_str().unwrap().starts_with("msg_") && out[1]["id"].as_str().unwrap().starts_with("fc_"));
    assert_eq!(
        v["usage"],
        json!({"input_tokens": 120, "input_tokens_details": {"cached_tokens": 100}, "output_tokens": 20,
               "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 140})
    );
    assert_eq!(openai_responses::decode_response(&v).unwrap(), r);
}

#[test]
fn response_encode_maps_stop_reasons_and_thinking() {
    let mk = |content: Vec<ContentPart>, stop: StopReason| CanonicalResponse {
        id: String::new(),
        model: "m".into(),
        created: None,
        content,
        stop_reason: stop,
        stop_sequence: None,
        usage: Usage::default(),
    };
    let max = openai_responses::encode_response(&mk(vec![ContentPart::text("cut")], StopReason::MaxTokens));
    assert_eq!(
        (max["status"].as_str(), max["incomplete_details"].clone()),
        (Some("incomplete"), json!({"reason": "max_output_tokens"}))
    );
    assert!(max["id"].as_str().unwrap().starts_with("resp_"));
    let filtered = openai_responses::encode_response(&mk(vec![], StopReason::ContentFilter));
    assert_eq!(filtered["incomplete_details"], json!({"reason": "content_filter"}));
    assert_eq!(filtered["output"], json!([]));
    let stop = openai_responses::encode_response(&mk(vec![ContentPart::text("x")], StopReason::StopSequence));
    assert_eq!((stop["status"].as_str(), stop["incomplete_details"].clone()), (Some("completed"), Value::Null));

    let v = openai_responses::encode_response(&mk(
        vec![
            ContentPart::Thinking { text: "plan".into(), signature: Some("openai-responses:E1".into()) },
            ContentPart::Thinking { text: String::new(), signature: Some("gemini:x".into()) },
            ContentPart::Thinking { text: String::new(), signature: None },
            ContentPart::text("a"),
            ContentPart::text("b"),
            call("c1", "f", json!({"k": 1})),
        ],
        StopReason::ToolUse,
    ));
    let out = v["output"].as_array().unwrap();
    assert_eq!(
        out.iter().map(|i| i["type"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["reasoning", "message", "function_call"]
    );
    assert_eq!(out[0]["encrypted_content"], "E1");
    assert_eq!(out[0]["summary"], json!([{"type": "summary_text", "text": "plan"}]));
    assert_eq!(out[1]["content"].as_array().unwrap().len(), 2, "adjacent text parts stay in one message item");
}

#[test]
fn response_decode_edge_cases() {
    let status = |s: Value, details: Value| {
        openai_responses::decode_response(&json!({"status": s, "incomplete_details": details, "output": [
            {"type": "message", "content": [{"type": "output_text", "text": "x"}]}]}))
        .unwrap()
        .stop_reason
    };
    assert_eq!(status(json!("completed"), Value::Null), StopReason::EndTurn);
    assert_eq!(status(json!("incomplete"), json!({"reason": "max_output_tokens"})), StopReason::MaxTokens);
    assert_eq!(status(json!("incomplete"), json!({"reason": "content_filter"})), StopReason::ContentFilter);
    assert_eq!(status(json!("incomplete"), json!({"reason": "other"})), StopReason::Other("other".into()));

    let r = openai_responses::decode_response(&json!({"status": "completed", "output": [
        {"type": "message", "content": [{"type": "refusal", "refusal": "I can't."}]},
        {"type": "reasoning", "content": [{"type": "reasoning_text", "text": "raw"}]},
        {"type": "reasoning", "summary": []},
        {"type": "custom_tool_call", "call_id": "c", "name": "apply_patch", "input": "patch"},
        {"type": "image_generation_call", "id": "ig", "result": "AAAA"}
    ]}))
    .unwrap();
    assert_eq!(
        r.content,
        vec![
            ContentPart::text("I can't."),
            ContentPart::Thinking { text: "raw".into(), signature: None },
            call("c", "apply_patch", json!({"input": "patch"}))
        ]
    );
    assert_eq!(r.stop_reason, StopReason::ToolUse);

    let failed = openai_responses::decode_response(
        &json!({"status": "failed", "error": {"code": "server_error", "message": "boom"}, "output": []}),
    );
    assert!(matches!(&failed, Err(ProtocolError::InvalidResponse(m)) if m.contains("boom")));
    for bad in [
        json!({}),
        json!([]),
        json!({"output": [{"type": "function_call", "call_id": "c", "name": "f", "arguments": "{oops"}]}),
        json!({"output": [{"type": "function_call", "name": "f", "arguments": "{}"}]}),
    ] {
        assert!(matches!(openai_responses::decode_response(&bad), Err(ProtocolError::InvalidResponse(_))), "{bad}");
    }
}

// ───────────── 错误体 ─────────────

#[test]
fn error_bodies() {
    let e = openai_responses::decode_error(
        429,
        &json!({"error": {"message": "slow down", "type": "insufficient_quota", "code": "insufficient_quota"}}),
    );
    assert_eq!(
        (e.kind, e.message.as_str(), e.code.as_deref()),
        (ApiErrorKind::RateLimit, "slow down", Some("insufficient_quota"))
    );
    assert_eq!(
        openai_responses::decode_error(401, &json!({"error": {"message": "bad key"}})).kind,
        ApiErrorKind::Authentication
    );
    assert_eq!(openai_responses::decode_error(502, &Value::String("<html/>".into())).message, "<html/>");
    assert_eq!(openai_responses::decode_error(500, &Value::Null).message, "upstream returned HTTP 500");
    // status 0 = 流内错误对象：按 code 归类
    assert_eq!(
        openai_responses::decode_error(0, &json!({"error": {"code": "rate_limit_exceeded", "message": "m"}})).kind,
        ApiErrorKind::RateLimit
    );
    assert_eq!(
        openai_responses::decode_error(0, &json!({"error": {"code": "invalid_prompt", "message": "m"}})).kind,
        ApiErrorKind::InvalidRequest
    );
    assert_eq!(
        openai_responses::decode_error(0, &json!({"error": {"code": "weird", "message": "m"}})).kind,
        ApiErrorKind::BadGateway
    );

    let api = ApiError::new(ApiErrorKind::Authentication, "bad key").with_code("invalid_api_key");
    assert_eq!(
        openai_responses::encode_error(&api),
        json!({"error": {"message": "bad key", "type": "authentication_error", "code": "invalid_api_key"}})
    );
    let frame = openai_responses::error_event(&ApiError::new(ApiErrorKind::RateLimit, "slow"), 7);
    assert_eq!(frame.event.as_deref(), Some("error"));
    assert_eq!(
        json_of(&frame),
        json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow", "param": null, "sequence_number": 7})
    );
}

// ───────────── 流式编码 ─────────────

fn reasoning_text_tool_events() -> Vec<StreamEvent> {
    vec![
        StreamEvent::MessageStart {
            id: "resp_x".into(),
            model: "m".into(),
            usage: Some(Usage { input_tokens: Some(120), cache_read_tokens: Some(100), ..Default::default() }),
        },
        StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "plan".into() } },
        StreamEvent::ContentBlockDelta {
            index: 0,
            delta: BlockDelta::Signature { signature: "openai-responses:ENC".into() },
        },
        StreamEvent::ContentBlockStop { index: 0 },
        StreamEvent::ContentBlockStart { index: 1, block: BlockStart::Text },
        StreamEvent::ContentBlockDelta { index: 1, delta: BlockDelta::Text { text: "Hel".into() } },
        StreamEvent::ContentBlockDelta { index: 1, delta: BlockDelta::Text { text: "lo".into() } },
        StreamEvent::ContentBlockStop { index: 1 },
        StreamEvent::ContentBlockStart {
            index: 2,
            block: BlockStart::ToolUse { id: "call_1".into(), name: "get_weather".into() },
        },
        StreamEvent::ToolCallDelta { index: 2, partial_json: "{\"ci".into() },
        StreamEvent::ToolCallDelta { index: 2, partial_json: "ty\":\"Rome\"}".into() },
        StreamEvent::ContentBlockStop { index: 2 },
        StreamEvent::Usage(Usage { output_tokens: Some(9), ..Default::default() }),
        StreamEvent::MessageStop { stop_reason: StopReason::ToolUse, stop_sequence: None },
    ]
}

#[test]
fn stream_encoder_emits_the_full_event_sequence() {
    let frames = run_encoder(&mut ResponsesStreamEncoder::new(), reasoning_text_tool_events());
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.reasoning_summary_part.added",
            "response.reasoning_summary_text.delta",
            "response.reasoning_summary_text.done",
            "response.reasoning_summary_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.content_part.added",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.output_text.done",
            "response.content_part.done",
            "response.output_item.done",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.delta",
            "response.function_call_arguments.done",
            "response.output_item.done",
            "response.completed",
        ]
    );
    let data: Vec<Value> = frames.iter().map(json_of).collect();
    // event 名与 data.type 一致，sequence_number 从 0 严格递增
    for (i, d) in data.iter().enumerate() {
        assert_eq!(d["type"].as_str(), frames[i].event.as_deref());
        assert_eq!(d["sequence_number"], i as u64);
    }
    assert_eq!(data[0]["response"]["status"], "in_progress");
    assert_eq!(data[0]["response"]["id"], "resp_x");
    assert_eq!(data[0]["response"]["output"], json!([]));

    // reasoning
    let rid = data[2]["item"]["id"].as_str().unwrap();
    assert!(rid.starts_with("rs_"));
    assert_eq!(
        (data[2]["output_index"].as_u64(), data[4]["item_id"].as_str(), data[4]["delta"].as_str()),
        (Some(0), Some(rid), Some("plan"))
    );
    assert_eq!(
        data[7]["item"],
        json!({"id": rid, "type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}], "encrypted_content": "ENC"})
    );
    // message
    let mid = data[8]["item"]["id"].as_str().unwrap();
    assert_eq!(data[8]["output_index"], 1);
    assert_eq!(data[9]["part"], json!({"type": "output_text", "text": "", "annotations": []}));
    assert_eq!((data[10]["delta"].as_str(), data[11]["delta"].as_str()), (Some("Hel"), Some("lo")));
    assert_eq!((data[10]["item_id"].as_str(), data[10]["content_index"].as_u64()), (Some(mid), Some(0)));
    assert_eq!(data[12]["text"], "Hello");
    assert_eq!(data[13]["part"]["text"], "Hello");
    assert_eq!(data[14]["item"]["content"][0]["text"], "Hello");
    assert_eq!(data[14]["item"]["status"], "completed");
    // function call：参数分片逐个下发，done 里是完整字符串
    assert_eq!(data[15]["item"]["type"], "function_call");
    assert_eq!(data[15]["item"]["call_id"], "call_1");
    assert_eq!(data[15]["output_index"], 2);
    assert_eq!((data[16]["delta"].as_str(), data[17]["delta"].as_str()), (Some("{\"ci"), Some("ty\":\"Rome\"}")));
    assert_eq!(data[18]["arguments"], "{\"city\":\"Rome\"}");
    assert_eq!(data[18]["name"], "get_weather");
    assert_eq!(data[19]["item"]["arguments"], "{\"city\":\"Rome\"}");
    assert_eq!(data[19]["item"]["status"], "completed");
    // completed：完整 output + usage
    let resp = &data[20]["response"];
    assert_eq!(resp["status"], "completed");
    let kinds: Vec<&str> = resp["output"].as_array().unwrap().iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["reasoning", "message", "function_call"]);
    assert_eq!(
        resp["usage"],
        json!({"input_tokens": 120, "input_tokens_details": {"cached_tokens": 100}, "output_tokens": 9,
               "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 129})
    );
}

#[test]
fn stream_encoder_incomplete_and_signature_only_thinking() {
    let start = StreamEvent::MessageStart { id: String::new(), model: "m".into(), usage: None };
    let frames = run_encoder(
        &mut ResponsesStreamEncoder::new(),
        vec![
            start.clone(),
            // 没有任何内容的 Thinking 块（例如只有别家签名）不产生 item
            StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: BlockDelta::Signature { signature: "gemini:zzz".into() },
            },
            StreamEvent::ContentBlockStop { index: 0 },
            // 只有 Responses 签名：产生带 encrypted_content 的空摘要 item
            StreamEvent::ContentBlockStart { index: 1, block: BlockStart::Thinking },
            StreamEvent::ContentBlockDelta {
                index: 1,
                delta: BlockDelta::Signature { signature: "openai-responses:E".into() },
            },
            StreamEvent::ContentBlockStop { index: 1 },
            // 未关闭的文本块由 MessageStop 补全
            StreamEvent::ContentBlockStart { index: 2, block: BlockStart::Text },
            StreamEvent::ContentBlockDelta { index: 2, delta: BlockDelta::Text { text: "cut".into() } },
            StreamEvent::MessageStop { stop_reason: StopReason::MaxTokens, stop_sequence: None },
        ],
    );
    let data: Vec<Value> = frames.iter().map(json_of).collect();
    let last = data.last().unwrap();
    assert_eq!(last["type"], "response.incomplete");
    assert_eq!(last["response"]["status"], "incomplete");
    assert_eq!(last["response"]["incomplete_details"], json!({"reason": "max_output_tokens"}));
    let out = last["response"]["output"].as_array().unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["type"], "reasoning");
    assert_eq!(out[0]["encrypted_content"], "E");
    assert_eq!(out[1]["content"][0]["text"], "cut");
    assert_eq!(data.iter().filter(|d| d["type"] == "response.output_item.added").count(), 2);
    assert!(last["response"]["id"].as_str().unwrap().starts_with("resp_"));
}

#[test]
fn stream_encoder_errors_before_and_after_the_response_exists() {
    let mut e = ResponsesStreamEncoder::new();
    let frames = e.push(StreamEvent::Error { kind: ApiErrorKind::RateLimit, message: "slow".into() }).unwrap();
    assert_eq!(frames[0].event.as_deref(), Some("error"));
    assert_eq!(json_of(&frames[0])["code"], "rate_limit_exceeded");
    assert!(e.push(StreamEvent::Usage(Usage::default())).is_err());

    let mut e = ResponsesStreamEncoder::new();
    let mut frames = run_encoder(
        &mut e,
        vec![
            StreamEvent::MessageStart { id: "r".into(), model: "m".into(), usage: None },
            StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text },
            StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "par".into() } },
        ],
    );
    frames.extend(
        e.push(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message: "upstream died".into() }).unwrap(),
    );
    let last = json_of(frames.last().unwrap());
    assert_eq!(last["type"], "response.failed");
    assert_eq!(last["response"]["status"], "failed");
    assert_eq!(last["response"]["error"], json!({"code": "server_error", "message": "upstream died"}));
    assert_eq!(last["sequence_number"], (frames.len() - 1) as u64);
}

#[test]
fn stream_encoder_rejects_invalid_order() {
    let start = StreamEvent::MessageStart { id: "x".into(), model: "m".into(), usage: None };
    let mut e = ResponsesStreamEncoder::new();
    assert!(e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());
    let mut e = ResponsesStreamEncoder::new();
    e.push(start.clone()).unwrap();
    assert!(e.push(start).is_err(), "duplicate start");
    assert!(e.push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "x".into() } }).is_err());
    assert!(e.push(StreamEvent::ContentBlockStop { index: 0 }).is_err());
    e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).unwrap();
    assert!(e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err(), "index reuse");
    assert!(e.push(StreamEvent::ToolCallDelta { index: 0, partial_json: "{}".into() }).is_err());
    assert!(e
        .push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "x".into() } })
        .is_err());
    e.push(StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None }).unwrap();
    assert!(e.push(StreamEvent::Usage(Usage::default())).is_err());
}

// ───────────── 流式解码 ─────────────

fn expected_fixture_stream() -> CanonicalResponse {
    CanonicalResponse {
        id: "resp_1".into(),
        model: "gpt-5".into(),
        created: None,
        content: vec![
            ContentPart::Thinking {
                text: "Check the weather.".into(),
                signature: Some("openai-responses:ENC-9".into()),
            },
            ContentPart::text("Checking 你好."),
            call("call_9", "get_weather", json!({"city": "Oslo"})),
        ],
        stop_reason: StopReason::ToolUse,
        stop_sequence: None,
        usage: Usage {
            input_tokens: Some(120),
            output_tokens: Some(20),
            cache_read_tokens: Some(100),
            cache_creation_tokens: None,
        },
    }
}

#[test]
fn stream_decoder_handles_the_fixture_at_every_split() {
    let bytes = RESPONSES_STREAM.as_bytes();
    for size in [1, 2, 3, 5, 17, 64, 1000, bytes.len()] {
        assert_eq!(collect(&decode_events(bytes, size)), expected_fixture_stream(), "chunk size {size}");
    }
    let events = decode_events(bytes, 64);
    assert!(
        matches!(events.first(), Some(StreamEvent::MessageStart { id, model, .. }) if id == "resp_1" && model == "gpt-5")
    );
    let n = events.len();
    assert!(matches!(events[n - 2], StreamEvent::Usage(_)));
    assert!(matches!(events[n - 1], StreamEvent::MessageStop { .. }));
}

#[test]
fn stream_decoder_event_name_fallback_and_unknown_events() {
    let sse = "event: response.created\ndata: {\"response\":{\"id\":\"r1\",\"model\":\"m\"}}\n\n\
               event: codex.rate_limits\ndata: {\"plan\":\"pro\"}\n\n\
               event: response.mcp_call.in_progress\ndata: {\"type\":\"response.mcp_call.in_progress\"}\n\n\
               event: response.output_text.delta\ndata: {\"output_index\":0,\"content_index\":0,\"delta\":\"ok\"}\n\n\
               data: [DONE]\n\n\
               event: response.completed\ndata: {\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n\n";
    let resp = collect(&decode_events(sse.as_bytes(), 13));
    assert_eq!(resp.content, vec![ContentPart::text("ok")]);
    assert_eq!(resp.usage.output_tokens, Some(2));
}

#[test]
fn stream_decoder_tolerates_providers_that_skip_events() {
    // 只发 delta，不发 added / content_part 事件
    let only_deltas = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "Hi "}),
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "there"}),
        json!({"type": "response.output_text.done", "output_index": 0, "content_index": 0, "text": "Hi there"}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(collect(&decode_events(only_deltas.as_bytes(), 7)).content, vec![ContentPart::text("Hi there")]);

    // 只发 done 事件：用 item 里的完整内容补全
    let only_done = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "content": [{"type": "output_text", "text": "All at once"}]}}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "function_call", "call_id": "c", "name": "f", "arguments": "{\"a\":1}"}}),
        json!({"type": "response.output_item.done", "output_index": 2, "item": {"type": "reasoning", "summary": [{"type": "summary_text", "text": "why"}], "encrypted_content": "E"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    let resp = collect(&decode_events(only_done.as_bytes(), 11));
    assert_eq!(
        resp.content,
        vec![
            ContentPart::text("All at once"),
            call("c", "f", json!({"a": 1})),
            ContentPart::Thinking { text: "why".into(), signature: Some("openai-responses:E".into()) },
        ]
    );
    assert_eq!(resp.stop_reason, StopReason::ToolUse);

    // function_call 只在 arguments.done 里给出完整参数
    let args_done = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "c", "name": "f", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.done", "output_index": 0, "arguments": "{\"x\":true}"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "function_call", "call_id": "c", "name": "f", "arguments": "{\"x\":true}"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(collect(&decode_events(args_done.as_bytes(), 5)).content, vec![call("c", "f", json!({"x": true}))]);
}

#[test]
fn stream_decoder_custom_tool_input_and_summary_parts() {
    let sse = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "custom_tool_call", "call_id": "c1", "name": "apply_patch", "input": ""}}),
        json!({"type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": "*** Begin\nsay \"hi\" "}),
        json!({"type": "response.custom_tool_call_input.delta", "output_index": 0, "delta": "back\\slash"}),
        json!({"type": "response.custom_tool_call_input.done", "output_index": 0, "input": "ignored because deltas were seen"}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "custom_tool_call", "call_id": "c1", "name": "apply_patch", "input": "x"}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"type": "reasoning", "summary": []}}),
        json!({"type": "response.reasoning_summary_part.added", "output_index": 1, "summary_index": 0, "part": {"type": "summary_text", "text": ""}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 1, "summary_index": 0, "delta": "A"}),
        json!({"type": "response.reasoning_summary_part.added", "output_index": 1, "summary_index": 1, "part": {"type": "summary_text", "text": ""}}),
        json!({"type": "response.reasoning_summary_text.delta", "output_index": 1, "summary_index": 1, "delta": "B"}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": {"type": "reasoning", "summary": [{"type": "summary_text", "text": "A"}, {"type": "summary_text", "text": "B"}]}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    let resp = collect(&decode_events(sse.as_bytes(), 19));
    assert_eq!(
        resp.content,
        vec![
            call("c1", "apply_patch", json!({"input": "*** Begin\nsay \"hi\" back\\slash"})),
            ContentPart::Thinking { text: "A\n\nB".into(), signature: None },
        ]
    );

    // 自由文本工具只在 done 里给出完整 input
    let done_only = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "custom_tool_call", "call_id": "c2", "name": "t"}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "custom_tool_call", "call_id": "c2", "name": "t", "input": "a\"b"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(
        collect(&decode_events(done_only.as_bytes(), 9)).content,
        vec![call("c2", "t", json!({"input": "a\"b"}))]
    );
}

#[test]
fn stream_decoder_refusal_incomplete_and_failures() {
    let refusal = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "content": []}}),
        json!({"type": "response.content_part.added", "output_index": 0, "content_index": 0, "part": {"type": "refusal", "refusal": ""}}),
        json!({"type": "response.refusal.delta", "output_index": 0, "content_index": 0, "delta": "I cannot "}),
        json!({"type": "response.refusal.delta", "output_index": 0, "content_index": 0, "delta": "help."}),
        json!({"type": "response.refusal.done", "output_index": 0, "content_index": 0, "refusal": "I cannot help."}),
        json!({"type": "response.content_part.done", "output_index": 0, "content_index": 0, "part": {"type": "refusal", "refusal": "I cannot help."}}),
        json!({"type": "response.output_item.done", "output_index": 0, "item": {"type": "message", "content": [{"type": "refusal", "refusal": "I cannot help."}]}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(collect(&decode_events(refusal.as_bytes(), 23)).content, vec![ContentPart::text("I cannot help.")]);

    for (reason, expected) in
        [("max_output_tokens", StopReason::MaxTokens), ("content_filter", StopReason::ContentFilter)]
    {
        let sse = sse_of(&[
            json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "partial"}),
            json!({"type": "response.incomplete", "response": {"status": "incomplete", "incomplete_details": {"reason": reason},
                   "usage": {"input_tokens": 5, "output_tokens": 6}}}),
        ]);
        let resp = collect(&decode_events(sse.as_bytes(), 31));
        assert_eq!(resp.stop_reason, expected, "{reason}");
        assert_eq!(resp.content, vec![ContentPart::text("partial")]);
        assert_eq!((resp.usage.input_tokens, resp.usage.output_tokens), (Some(5), Some(6)));
    }

    let failed = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "rate_limit_exceeded", "message": "Rate limit reached"}}}),
    ]);
    let events = decode_events(failed.as_bytes(), 17);
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::RateLimit, message }) if message == "Rate limit reached")
    );
    let error_event = sse_of(&[json!({"type": "error", "code": "server_error", "message": "boom", "param": null})]);
    let events = decode_events(error_event.as_bytes(), 17);
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::Internal, message }) if message == "boom")
    );
}

#[test]
fn stream_decoder_error_paths() {
    // 没有 response.completed 的截断流
    let truncated = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "par"}),
    ]);
    let events = decode_events(truncated.as_bytes(), 29);
    assert!(matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::BadGateway, .. })));

    let mut dec = ResponsesStreamDecoder::new();
    assert!(matches!(dec.push(b"event: x\ndata: {oops\n\n"), Err(ProtocolError::InvalidResponse(_))));
    let mut dec = ResponsesStreamDecoder::new();
    assert!(matches!(dec.finish(), Err(ProtocolError::InvalidResponse(_))));
    // 流结束后多余的事件被忽略
    let mut dec = ResponsesStreamDecoder::new();
    let sse = sse_of(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "late"}),
    ]);
    let events = dec.push(sse.as_bytes()).unwrap();
    assert!(matches!(events.last(), Some(StreamEvent::MessageStop { .. })));
    assert!(dec.finish().unwrap().is_empty());
}

#[test]
fn stream_encode_then_decode_is_lossless() {
    let original = reasoning_text_tool_events();
    let wire: String =
        run_encoder(&mut ResponsesStreamEncoder::new(), original.clone()).iter().map(SseFrame::to_wire).collect();
    let expected = collect(&original);
    for size in [1, 11, 128, wire.len()] {
        assert_eq!(collect(&decode_events(wire.as_bytes(), size)), expected, "chunk size {size}");
    }
}

// ───────────── 跨协议 ─────────────

#[test]
fn responses_client_to_openai_chat_upstream_and_back() {
    let canonical = rs(&fixture!("responses_tool_chain_request"));
    let up = openai_chat::encode_request(&canonical).unwrap();
    let msgs = up.body["messages"].as_array().unwrap();
    assert_eq!(
        msgs.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["user", "assistant", "tool", "tool", "assistant", "tool"]
    );
    assert_eq!(msgs[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(up.body["max_tokens"], 512);
    let mut expected = semantic(canonical);
    expected.include_usage = false;
    assert_eq!(semantic(oa(&up.body)), expected);

    let resp = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    let out = openai_responses::encode_response(&resp);
    assert_eq!(out["output"][0]["content"][0]["text"], "Checking.");
    assert_eq!(out["output"][1]["call_id"], "call_9");
    assert_eq!(out["usage"]["input_tokens"], 120);
    assert_eq!(out["usage"]["input_tokens_details"]["cached_tokens"], 100);
    assert_eq!(out["usage"]["output_tokens"], 20);
}

#[test]
fn responses_client_to_anthropic_upstream_and_back() {
    let canonical = rs(&fixture!("responses_tool_chain_request"));
    let up = anthropic::encode_request(&canonical).unwrap();
    assert_eq!(up.body["max_tokens"], 512);
    let msgs = up.body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[2]["content"][1]["tool_use_id"], "call_2");
    let resp = anthropic::decode_response(&fixture!("anthropic_tool_response")).unwrap();
    let out = openai_responses::encode_response(&resp);
    assert_eq!(out["status"], "completed");
    assert_eq!(out["output"][1]["name"], "get_weather");
    assert_eq!(out["usage"]["input_tokens"], 120, "Anthropic input 20 + cache 100");
}

fn strip_item_ids(mut items: Value) -> Value {
    for it in items.as_array_mut().unwrap() {
        let o = it.as_object_mut().unwrap();
        o.remove("id");
        o.remove("status");
    }
    items
}

#[test]
fn openai_and_anthropic_clients_to_responses_upstream() {
    let expected = strip_item_ids(fixture!("responses_tool_chain_request")["input"].clone());
    let from_oa = openai_responses::encode_request(&oa(&fixture!("openai_tool_chain_request"))).unwrap();
    assert_eq!(from_oa.body["input"], expected);
    let from_an = openai_responses::encode_request(&an(&fixture!("anthropic_tool_chain_request"))).unwrap();
    assert_eq!(from_an.body["input"], expected);

    let resp = openai_responses::decode_response(&fixture!("responses_tool_response")).unwrap();
    let chat = openai_chat::encode_response(&resp);
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["usage"]["prompt_tokens_details"]["cached_tokens"], 100);
    let msg = anthropic::encode_response(&resp);
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["usage"]["input_tokens"], 20);
    assert_eq!(msg["usage"]["cache_read_input_tokens"], 100);
}

#[test]
fn responses_to_gemini_and_ollama_both_ways() {
    let canonical = rs(&fixture!("responses_tool_chain_request"));
    let g = gemini::encode_request(&canonical).unwrap();
    assert_eq!(g.body["contents"], fixture!("gemini_tool_chain_request")["contents"]);
    let o = ollama::encode_request(&canonical).unwrap();
    assert_eq!(o.body["messages"][3], json!({"role": "tool", "tool_name": "get_weather", "content": "25C rain"}));

    // Gemini / Ollama 上游响应 → Responses 客户端
    let from_gemini =
        openai_responses::encode_response(&gemini::decode_response(&fixture!("gemini_tool_response")).unwrap());
    assert_eq!(from_gemini["output"][1]["call_id"], "call_9");
    assert_eq!(from_gemini["usage"]["input_tokens_details"]["cached_tokens"], 100);
    let from_ollama =
        openai_responses::encode_response(&ollama::decode_response(&fixture!("ollama_tool_response")).unwrap());
    assert_eq!(from_ollama["output"][1]["arguments"], "{\"city\":\"Oslo\"}");
    assert_eq!(from_ollama["usage"]["output_tokens"], 20);

    // Gemini / Ollama 客户端请求 → Responses 上游
    let r = openai_responses::encode_request(
        &gemini::decode_request_for("m", false, &fixture!("gemini_tool_chain_request")).unwrap(),
    )
    .unwrap();
    assert_eq!(r.body["input"], strip_item_ids(fixture!("responses_tool_chain_request")["input"].clone()));
    let r = openai_responses::encode_request(&ollama::decode_request(&fixture!("ollama_tool_chain_request")).unwrap())
        .unwrap();
    assert_eq!(r.body["input"][3]["type"], "function_call_output");
    assert_eq!(r.body["input"][3]["call_id"], "call_1");
}

#[test]
fn reasoning_requests_map_to_other_protocols() {
    let canonical = rs(&fixture!("responses_reasoning_chain_request"));
    let g = gemini::encode_request(&canonical).unwrap();
    assert_eq!(g.body["generationConfig"]["thinkingConfig"], json!({"thinkingBudget": 24576, "includeThoughts": true}));
    assert!(has_warning(&g.warnings, "thinking content"));
    let o = ollama::encode_request(&canonical).unwrap();
    assert_eq!(o.body["think"], true);
    let mut oss = canonical.clone();
    oss.model = "gpt-oss:20b".into();
    assert_eq!(ollama::encode_request(&oss).unwrap().body["think"], "high");
}

#[test]
fn responses_stream_feeds_every_other_protocol_encoder() {
    let events = decode_events(RESPONSES_STREAM.as_bytes(), 41);

    let frames = run_encoder(&mut OpenAiStreamEncoder::new(true), events.clone());
    let chunks: Vec<Value> = frames.iter().filter(|f| f.data != "[DONE]").map(json_of).collect();
    let reasoning: String =
        chunks.iter().filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str()).collect();
    let text: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["content"].as_str()).collect();
    let args: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    assert_eq!(
        (reasoning.as_str(), text.as_str(), args.as_str()),
        ("Check the weather.", "Checking 你好.", "{\"city\":\"Oslo\"}")
    );
    assert!(chunks.iter().any(|c| c["choices"][0]["finish_reason"] == "tool_calls"));
    assert_eq!(chunks.last().unwrap()["usage"]["prompt_tokens_details"]["cached_tokens"], 100);

    let frames = run_encoder(&mut AnthropicStreamEncoder::new(), events.clone());
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(&names[names.len() - 2..], &["message_delta", "message_stop"]);
    let delta = json_of(&frames[frames.len() - 2]);
    assert_eq!(delta["delta"]["stop_reason"], "tool_use");
    assert_eq!(delta["usage"]["output_tokens"], 20);

    let frames = run_encoder(&mut GeminiStreamEncoder::new(), events.clone());
    assert!(
        frames.iter().all(|f| !f.data.contains("thoughtSignature")),
        "a Responses signature must never reach a Gemini client"
    );
    let parts: Vec<Value> = frames.iter().map(|f| json_of(f)["candidates"][0]["content"]["parts"][0].clone()).collect();
    assert_eq!(parts[0], json!({"text": "Check the weather.", "thought": true}));
    assert!(parts.iter().any(|p| p["functionCall"]["args"] == json!({"city": "Oslo"})));
    let last = json_of(frames.last().unwrap());
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 120);
    assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 20);

    let mut enc = OllamaStreamEncoder::new();
    let lines: Vec<Value> = events
        .iter()
        .cloned()
        .flat_map(|e| enc.push(e).unwrap())
        .map(|l| serde_json::from_str(l.trim()).unwrap())
        .collect();
    assert_eq!(lines.last().unwrap()["eval_count"], 20);
    assert!(lines.iter().any(|l| l["message"]["thinking"] == "Check the weather."));
    assert!(lines.iter().any(|l| l["message"]["tool_calls"][0]["function"]["arguments"] == json!({"city": "Oslo"})));
}

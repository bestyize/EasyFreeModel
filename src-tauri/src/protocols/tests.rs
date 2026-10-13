//! 协议层测试：fixture 往返、跨协议转换、流式事件顺序、错误与边界。
//! fixture 位于 `src/protocols/fixtures/*.json`。

use serde_json::{json, Value};

use super::anthropic::{self, AnthropicStreamEncoder};
use super::canonical::*;
use super::openai_chat::{self, OpenAiStreamEncoder};
use super::sse::parse_sse;
use super::*;

fn fx(s: &str) -> Value {
    serde_json::from_str(s).expect("fixture must be valid JSON")
}

macro_rules! fixture {
    ($name:literal) => {
        fx(include_str!(concat!("fixtures/", $name, ".json")))
    };
}

/// warnings 描述的是解码过程而非语义，比较语义时清空。
fn semantic(mut r: CanonicalRequest) -> CanonicalRequest {
    r.warnings.clear();
    r
}

fn oa(v: &Value) -> CanonicalRequest {
    openai_chat::decode_request(v).expect("openai decode")
}
fn an(v: &Value) -> CanonicalRequest {
    anthropic::decode_request(v).expect("anthropic decode")
}

fn parts_of(req: &CanonicalRequest, i: usize) -> &Vec<ContentPart> {
    &req.messages[i].content
}

// ───────────── 纯文本 ─────────────

#[test]
fn text_request_openai_and_anthropic_decode_to_same_canonical() {
    let a = semantic(oa(&fixture!("openai_text_request")));
    let b = semantic(an(&fixture!("anthropic_text_request")));
    assert_eq!(a, b);
    assert_eq!(a.system, vec!["You are helpful.", "Answer briefly."]);
    assert_eq!(a.params.max_tokens, Some(256));
    assert_eq!(a.params.stop, vec!["END"]);
    assert_eq!(a.user.as_deref(), Some("u-1"));
}

#[test]
fn openai_developer_role_is_system() {
    let req = oa(&json!({"model": "m", "messages": [
        {"role": "developer", "content": "dev rules"},
        {"role": "user", "content": "hi"}
    ]}));
    assert_eq!(req.system, vec!["dev rules"]);
    assert!(req.warnings.is_empty());
}

#[test]
fn openai_mid_conversation_system_is_hoisted_with_warning() {
    let req = oa(&json!({"model": "m", "messages": [
        {"role": "user", "content": "hi"},
        {"role": "system", "content": "late"}
    ]}));
    assert_eq!(req.system, vec!["late"]);
    assert_eq!(req.messages.len(), 1);
    assert!(req.warnings.iter().any(|w| w.contains("hoisted")));
}

#[test]
fn anthropic_system_as_string() {
    let req = an(&json!({"model": "m", "max_tokens": 5, "system": "sys", "messages": [{"role": "user", "content": "x"}]}));
    assert_eq!(req.system, vec!["sys"]);
}

#[test]
fn text_request_roundtrips_through_both_protocols() {
    for canonical in [
        semantic(oa(&fixture!("openai_text_request"))),
        semantic(an(&fixture!("anthropic_text_request"))),
    ] {
        let via_oa = openai_chat::encode_request(&canonical).unwrap();
        assert_eq!(semantic(oa(&via_oa.body)), canonical, "openai roundtrip");
        let via_an = anthropic::encode_request(&canonical).unwrap();
        assert_eq!(semantic(an(&via_an.body)), canonical, "anthropic roundtrip");
    }
}

#[test]
fn unknown_fields_become_warnings_not_silence() {
    let req = oa(&json!({
        "model": "m", "messages": [{"role": "user", "content": "x"}],
        "response_format": {"type": "json_object"}, "logprobs": true
    }));
    assert!(req.warnings.contains(&"unsupported field ignored: response_format".to_string()));
    assert!(req.warnings.contains(&"unsupported field ignored: logprobs".to_string()));

    let req = an(&json!({
        "model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}],
        "thinking": {"type": "enabled", "budget_tokens": 1024}
    }));
    assert!(req.warnings.contains(&"unsupported field ignored: thinking".to_string()));
}

#[test]
fn warnings_never_contain_request_text() {
    let secret = "TOP-SECRET-PROMPT-TEXT";
    let req = oa(&json!({
        "model": "m", "messages": [{"role": "user", "content": secret}], "weird": secret
    }));
    assert!(req.warnings.iter().all(|w| !w.contains(secret)));
}

// ───────────── 工具调用与多轮工具链 ─────────────

#[test]
fn tool_chain_decodes_identically_in_both_protocols() {
    let a = semantic(oa(&fixture!("openai_tool_chain_request")));
    let b = semantic(an(&fixture!("anthropic_tool_chain_request")));
    assert_eq!(a, b);

    // 并行工具结果被合并到同一条 user 消息
    assert_eq!(a.messages.len(), 5);
    assert_eq!(a.messages[2].role, Role::User);
    assert_eq!(parts_of(&a, 2).len(), 2);
    assert!(matches!(&parts_of(&a, 1)[0], ContentPart::ToolUse { id, name, input }
        if id == "call_1" && name == "get_weather" && input == &json!({"city": "Paris"})));
    assert_eq!(a.tools.len(), 1);
    assert_eq!(a.tool_choice, Some(ToolChoice::Auto));
}

#[test]
fn tool_chain_roundtrips_and_converts_across_protocols() {
    let canonical = semantic(oa(&fixture!("openai_tool_chain_request")));

    let to_an = anthropic::encode_request(&canonical).unwrap();
    assert_eq!(semantic(an(&to_an.body)), canonical);
    // Anthropic 请求里不能出现 OpenAI 的字段
    let s = to_an.body.to_string();
    assert!(!s.contains("tool_calls") && !s.contains("tool_call_id") && !s.contains("\"role\":\"tool\""));

    let to_oa = openai_chat::encode_request(&canonical).unwrap();
    assert_eq!(semantic(oa(&to_oa.body)), canonical);
    let msgs = to_oa.body["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "tool", "assistant", "tool"]);
    // 工具参数在 OpenAI 侧必须是 JSON 字符串
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], json!("{\"city\":\"Paris\"}"));
    assert_eq!(msgs[1]["content"], Value::Null);
}

#[test]
fn anthropic_tool_chain_to_openai_and_back() {
    let canonical = semantic(an(&fixture!("anthropic_tool_chain_request")));
    let to_oa = openai_chat::encode_request(&canonical).unwrap();
    assert_eq!(semantic(oa(&to_oa.body)), canonical);
}

#[test]
fn tool_choice_variants_and_parallel_flag() {
    let base = |tc: Value| {
        oa(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}],
                   "tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}],
                   "tool_choice": tc, "parallel_tool_calls": false}))
    };
    assert_eq!(base(json!("none")).tool_choice, Some(ToolChoice::None));
    assert_eq!(base(json!("required")).tool_choice, Some(ToolChoice::Any));
    let named = base(json!({"type": "function", "function": {"name": "f"}}));
    assert_eq!(named.tool_choice, Some(ToolChoice::Tool { name: "f".into() }));
    assert_eq!(named.parallel_tool_calls, Some(false));

    let an_body = anthropic::encode_request(&named).unwrap().body;
    assert_eq!(an_body["tool_choice"], json!({"type": "tool", "name": "f", "disable_parallel_tool_use": true}));

    let back = an(&an_body);
    assert_eq!(back.tool_choice, Some(ToolChoice::Tool { name: "f".into() }));
    assert_eq!(back.parallel_tool_calls, Some(false));

    // Anthropic any/none → OpenAI required/none
    let any = an(&json!({"model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}],
                         "tool_choice": {"type": "any"}}));
    let oa_body = openai_chat::encode_request(&any).unwrap().body;
    assert_eq!(oa_body["tool_choice"], json!("required"));
}

#[test]
fn anthropic_encode_puts_tool_results_first_and_merges_same_role() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("question")]),
            Message::assistant(vec![ContentPart::ToolUse { id: "t1".into(), name: "f".into(), input: json!({}) }]),
            // OpenAI 风格：tool 结果与随后的 user 文本是两条相邻 user 消息
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![ContentPart::text("ok")],
                is_error: false,
            }]),
            Message::user(vec![ContentPart::text("and then?")]),
        ],
        params: GenerationParams { max_tokens: Some(10), ..Default::default() },
        ..Default::default()
    };
    let body = anthropic::encode_request(&req).unwrap().body;
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    let last = msgs[2]["content"].as_array().unwrap();
    assert_eq!(last[0]["type"], "tool_result");
    assert_eq!(last[1]["type"], "text");
}

#[test]
fn anthropic_encode_defaults_max_tokens_with_warning() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![ContentPart::text("x")])],
        ..Default::default()
    };
    let enc = anthropic::encode_request(&req).unwrap();
    assert_eq!(enc.body["max_tokens"], json!(anthropic::DEFAULT_MAX_TOKENS));
    assert!(enc.warnings.iter().any(|w| w.contains("max_tokens")));
}

#[test]
fn tool_result_error_flag_survives_anthropic_roundtrip_and_warns_on_openai() {
    let req = an(&json!({"model": "m", "max_tokens": 5, "messages": [
        {"role": "assistant", "content": [{"type": "tool_use", "id": "t", "name": "f", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "boom", "is_error": true}]}
    ]}));
    assert!(matches!(&parts_of(&req, 1)[0], ContentPart::ToolResult { is_error: true, .. }));
    assert_eq!(semantic(an(&anthropic::encode_request(&req).unwrap().body)), semantic(req.clone()));
    let enc = openai_chat::encode_request(&req).unwrap();
    assert!(enc.warnings.iter().any(|w| w.contains("is_error")));
}

// ───────────── 图片 ─────────────

#[test]
fn image_request_converts_between_protocols() {
    let a = semantic(oa(&fixture!("openai_image_request")));
    let b = semantic(an(&fixture!("anthropic_image_request")));
    assert_eq!(a, b);
    let parts = parts_of(&a, 0);
    assert!(matches!(&parts[1], ContentPart::Image { source: ImageSource::Base64 { media_type, data }, .. }
        if media_type == "image/png" && data == "iVBORw0KGgoAAAANSUhEUg=="));
    assert!(matches!(&parts[2], ContentPart::Image { source: ImageSource::Url { url }, .. } if url == "https://example.com/cat.jpg"));

    // canonical → OpenAI：base64 重新变成 data URL
    let oa_body = openai_chat::encode_request(&a).unwrap().body;
    assert_eq!(
        oa_body["messages"][0]["content"][1]["image_url"]["url"],
        json!("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==")
    );
    // canonical → Anthropic：base64 source
    let an_body = anthropic::encode_request(&a).unwrap().body;
    assert_eq!(an_body["messages"][0]["content"][1]["source"]["type"], "base64");
}

#[test]
fn large_base64_image_stays_out_of_text() {
    let big = "QUJD".repeat(600 * 1024); // ~2.4 MB base64
    let body = json!({"model": "m", "messages": [{"role": "user", "content": [
        {"type": "text", "text": "look"},
        {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{big}"), "detail": "low"}}
    ]}]});
    let req = oa(&body);
    assert_eq!(req.text_bytes(), 4);
    assert_eq!(req.media_bytes(), big.len());
    assert!(matches!(&parts_of(&req, 0)[1], ContentPart::Image { detail: Some(d), .. } if d == "low"));
    // 警告、调试输出的文本里不得带入图片数据
    assert!(req.warnings.iter().all(|w| !w.contains("QUJD")));
}

#[test]
fn anthropic_file_id_image_cannot_go_to_openai() {
    let req = an(&json!({"model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": [
        {"type": "image", "source": {"type": "file", "file_id": "file_1"}}
    ]}]}));
    let enc = openai_chat::encode_request(&req).unwrap();
    assert!(enc.warnings.iter().any(|w| w.contains("file_id")));
}

#[test]
fn image_inside_tool_result_moves_to_following_user_message_for_openai() {
    let req = an(&json!({"model": "m", "max_tokens": 5, "messages": [
        {"role": "assistant", "content": [{"type": "tool_use", "id": "t", "name": "shot", "input": {}}]},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": [
            {"type": "text", "text": "screenshot"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}
        ]}]}
    ]}));
    let enc = openai_chat::encode_request(&req).unwrap();
    let msgs = enc.body["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["role"], "tool");
    assert_eq!(msgs[1]["content"], "screenshot");
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][1]["type"], "image_url");
    assert!(!enc.warnings.is_empty());
}

// ───────────── 校验与不支持项 ─────────────

#[test]
fn anthropic_requires_max_tokens() {
    let err = anthropic::decode_request(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}]})).unwrap_err();
    assert!(matches!(&err, ProtocolError::InvalidRequest(m) if m.contains("max_tokens")));
    let err = anthropic::decode_request(&json!({"model": "m", "max_tokens": 0, "messages": [{"role": "user", "content": "x"}]})).unwrap_err();
    assert!(matches!(err, ProtocolError::InvalidRequest(_)));
}

#[test]
fn anthropic_rejects_bad_roles_and_misplaced_blocks() {
    let bad_role = json!({"model": "m", "max_tokens": 5, "messages": [{"role": "system", "content": "x"}]});
    assert!(matches!(anthropic::decode_request(&bad_role), Err(ProtocolError::InvalidRequest(_))));
    let tool_use_in_user = json!({"model": "m", "max_tokens": 5, "messages": [
        {"role": "user", "content": [{"type": "tool_use", "id": "t", "name": "f", "input": {}}]}]});
    assert!(matches!(anthropic::decode_request(&tool_use_in_user), Err(ProtocolError::InvalidRequest(_))));
    let doc = json!({"model": "m", "max_tokens": 5, "messages": [
        {"role": "user", "content": [{"type": "document", "source": {}}]}]});
    assert!(matches!(anthropic::decode_request(&doc), Err(ProtocolError::Unsupported(_))));
    let server_tool = json!({"model": "m", "max_tokens": 5, "messages": [{"role": "user", "content": "x"}],
        "tools": [{"type": "web_search_20250305", "name": "web_search"}]});
    assert!(matches!(anthropic::decode_request(&server_tool), Err(ProtocolError::Unsupported(_))));
}

#[test]
fn anthropic_cache_control_is_reported() {
    let req = an(&json!({"model": "m", "max_tokens": 5,
        "system": [{"type": "text", "text": "s", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": "x"}]}));
    assert!(req.warnings.iter().any(|w| w.contains("cache_control")));
}

#[test]
fn openai_rejects_unsupported_shapes() {
    let n2 = json!({"model": "m", "n": 2, "messages": [{"role": "user", "content": "x"}]});
    assert!(matches!(openai_chat::decode_request(&n2), Err(ProtocolError::Unsupported(_))));
    let audio = json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "input_audio", "input_audio": {}}]}]});
    assert!(matches!(openai_chat::decode_request(&audio), Err(ProtocolError::Unsupported(_))));
    let builtin_tool = json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "tools": [{"type": "web_search"}]});
    assert!(matches!(openai_chat::decode_request(&builtin_tool), Err(ProtocolError::Unsupported(_))));
    let bad_args = json!({"model": "m", "messages": [
        {"role": "assistant", "tool_calls": [{"id": "c", "type": "function", "function": {"name": "f", "arguments": "{oops"}}]}]});
    assert!(matches!(openai_chat::decode_request(&bad_args), Err(ProtocolError::InvalidRequest(_))));
    let no_msgs = json!({"model": "m", "messages": []});
    assert!(matches!(openai_chat::decode_request(&no_msgs), Err(ProtocolError::InvalidRequest(_))));
    let not_obj = json!([1, 2]);
    assert!(matches!(openai_chat::decode_request(&not_obj), Err(ProtocolError::InvalidRequest(_))));
}

#[test]
fn openai_stop_string_and_max_token_precedence() {
    let req = oa(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}],
        "stop": "END", "max_tokens": 10, "max_completion_tokens": 20, "stream": true,
        "stream_options": {"include_usage": true}}));
    assert_eq!(req.params.stop, vec!["END"]);
    assert_eq!(req.params.max_tokens, Some(20));
    assert!(req.stream && req.include_usage);
}

#[test]
fn openai_encode_request_requests_usage_for_streams() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![ContentPart::text("x")])],
        stream: true,
        ..Default::default()
    };
    let body = openai_chat::encode_request(&req).unwrap().body;
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
}

#[test]
fn empty_body_is_invalid_for_responses_and_gemini() {
    // 两者都已实现：空请求体是「请求无效」，不再是「未实现」
    assert!(matches!(openai_responses::decode_request(&json!({})), Err(ProtocolError::InvalidRequest(_))));
    assert!(matches!(gemini::decode_request(&json!({})), Err(ProtocolError::InvalidRequest(_))));
}

#[test]
fn anthropic_drops_thinking_blocks_signed_by_other_providers() {
    let foreign = signature::tag(signature::GEMINI, "abc");
    let req = CanonicalRequest {
        model: "claude".into(),
        messages: vec![
            Message { role: Role::User, content: vec![ContentPart::Text { text: "hi".into() }] },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentPart::Thinking { text: String::new(), signature: Some(foreign.clone()) },
                    ContentPart::Text { text: "hello".into() },
                ],
            },
            Message { role: Role::User, content: vec![ContentPart::Text { text: "again".into() }] },
        ],
        ..Default::default()
    };
    let enc = anthropic::encode_request(&req).unwrap();
    let dumped = enc.body.to_string();
    assert!(!dumped.contains("thinking"), "{dumped}");
    assert!(!dumped.contains(&foreign), "{dumped}");
    assert!(enc.warnings.iter().any(|w| w.contains("signature")), "{:?}", enc.warnings);

    // 一条消息只剩外来思考块时整条消息省略，不能产出空 content
    let only = CanonicalRequest {
        model: "claude".into(),
        messages: vec![
            Message { role: Role::User, content: vec![ContentPart::Text { text: "hi".into() }] },
            Message { role: Role::Assistant, content: vec![ContentPart::Thinking { text: String::new(), signature: Some(foreign) }] },
            Message { role: Role::User, content: vec![ContentPart::Text { text: "again".into() }] },
        ],
        ..Default::default()
    };
    let body = anthropic::encode_request(&only).unwrap().body;
    for m in body["messages"].as_array().unwrap() {
        assert!(!m["content"].as_array().unwrap().is_empty(), "{body}");
    }
}

#[test]
fn anthropic_keeps_its_own_signatures() {
    let req = CanonicalRequest {
        model: "claude".into(),
        messages: vec![
            Message { role: Role::User, content: vec![ContentPart::Text { text: "hi".into() }] },
            Message {
                role: Role::Assistant,
                content: vec![ContentPart::Thinking { text: "hmm".into(), signature: Some("EqQBCkYIBRgC".into()) }],
            },
        ],
        ..Default::default()
    };
    let body = anthropic::encode_request(&req).unwrap().body;
    assert!(body.to_string().contains("EqQBCkYIBRgC"));
}

// ───────────── 响应 ─────────────

#[test]
fn tool_response_converts_between_protocols() {
    let a = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    let b = anthropic::decode_response(&fixture!("anthropic_tool_response")).unwrap();
    // created 只有 OpenAI 提供
    assert_eq!(a.created, Some(1_760_000_000));
    let b = CanonicalResponse { created: a.created, ..b };
    assert_eq!(a, b);
    assert_eq!(a.stop_reason, StopReason::ToolUse);
    // OpenAI prompt_tokens(含缓存)=120 ⇔ Anthropic input(20) + cache_read(100)
    assert_eq!(a.usage.input_tokens, Some(120));
    assert_eq!(a.usage.cache_read_tokens, Some(100));

    // canonical → Anthropic：输入 token 重新扣除缓存
    let an_json = anthropic::encode_response(&a);
    assert_eq!(an_json["usage"]["input_tokens"], 20);
    assert_eq!(an_json["usage"]["cache_read_input_tokens"], 100);
    assert_eq!(an_json["stop_reason"], "tool_use");
    assert_eq!(an_json["content"][1]["input"], json!({"city": "Oslo"}));
    assert_eq!(anthropic::decode_response(&an_json).unwrap(), CanonicalResponse { created: None, ..a.clone() });

    // canonical → OpenAI
    let oa_json = openai_chat::encode_response(&a);
    assert_eq!(oa_json["object"], "chat.completion");
    assert_eq!(oa_json["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(oa_json["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"city\":\"Oslo\"}");
    assert_eq!(oa_json["usage"]["total_tokens"], 140);
    assert_eq!(oa_json["usage"]["prompt_tokens_details"]["cached_tokens"], 100);
    assert_eq!(openai_chat::decode_response(&oa_json).unwrap(), a);
}

#[test]
fn text_only_openai_response_has_content_and_no_tool_calls() {
    let resp = CanonicalResponse {
        id: "".into(),
        model: "m".into(),
        created: None,
        content: vec![ContentPart::text("hello")],
        stop_reason: StopReason::MaxTokens,
        stop_sequence: None,
        usage: Usage { input_tokens: Some(3), output_tokens: Some(2), ..Default::default() },
    };
    let v = openai_chat::encode_response(&resp);
    assert!(v["id"].as_str().unwrap().starts_with("chatcmpl-"));
    assert_eq!(v["choices"][0]["message"]["content"], "hello");
    assert!(v["choices"][0]["message"].get("tool_calls").is_none());
    assert_eq!(v["choices"][0]["finish_reason"], "length");
    let a = anthropic::encode_response(&resp);
    assert!(a["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(a["stop_reason"], "max_tokens");
    assert_eq!(a["content"][0], json!({"type": "text", "text": "hello"}));
}

#[test]
fn thinking_blocks_survive_anthropic_and_map_to_reasoning_content_on_openai() {
    let resp = anthropic::decode_response(&json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
        "content": [
            {"type": "thinking", "thinking": "hmm", "signature": "sig"},
            {"type": "text", "text": "answer"}
        ],
        "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 2}
    }))
    .unwrap();
    let again = anthropic::encode_response(&resp);
    assert_eq!(again["content"][0], json!({"type": "thinking", "thinking": "hmm", "signature": "sig"}));
    let oa_json = openai_chat::encode_response(&resp);
    assert_eq!(oa_json["choices"][0]["message"]["reasoning_content"], "hmm");
}

#[test]
fn invalid_upstream_responses_are_errors() {
    assert!(matches!(openai_chat::decode_response(&json!({"choices": []})), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(anthropic::decode_response(&json!({"type": "error"})), Err(ProtocolError::InvalidResponse(_))));
    let bad_args = json!({"choices": [{"message": {"role": "assistant", "tool_calls": [
        {"id": "c", "type": "function", "function": {"name": "f", "arguments": "{oops"}}]}}]});
    assert!(matches!(openai_chat::decode_response(&bad_args), Err(ProtocolError::InvalidResponse(_))));
}

// ───────────── 流式 ─────────────

fn text_and_tool_stream() -> Vec<StreamEvent> {
    vec![
        StreamEvent::MessageStart {
            id: "msg_1".into(),
            model: "m".into(),
            usage: Some(Usage { input_tokens: Some(120), cache_read_tokens: Some(100), ..Default::default() }),
        },
        StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "Hel".into() } },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "lo".into() } },
        StreamEvent::ContentBlockStop { index: 0 },
        StreamEvent::ContentBlockStart { index: 1, block: BlockStart::ToolUse { id: "call_1".into(), name: "get_weather".into() } },
        StreamEvent::ToolCallDelta { index: 1, partial_json: "{\"ci".into() },
        StreamEvent::ToolCallDelta { index: 1, partial_json: "ty\":\"Rome\"}".into() },
        StreamEvent::ContentBlockStop { index: 1 },
        StreamEvent::Usage(Usage { output_tokens: Some(9), ..Default::default() }),
        StreamEvent::MessageStop { stop_reason: StopReason::ToolUse, stop_sequence: None },
    ]
}

fn run_stream(enc: &mut dyn StreamEncoder, events: Vec<StreamEvent>) -> Vec<SseFrame> {
    events.into_iter().flat_map(|e| enc.push(e).expect("valid sequence")).collect()
}

fn json_of(f: &SseFrame) -> Value {
    serde_json::from_str(&f.data).unwrap()
}

#[test]
fn anthropic_stream_event_order() {
    let frames = run_stream(&mut AnthropicStreamEncoder::new(), text_and_tool_stream());
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    // data 里的 type 必须与 event 名一致
    for f in &frames {
        assert_eq!(json_of(f)["type"], f.event.as_deref().unwrap());
    }
    let start = json_of(&frames[0]);
    assert_eq!(start["message"]["usage"]["input_tokens"], 20); // 120 - 100 缓存
    assert_eq!(start["message"]["usage"]["cache_read_input_tokens"], 100);
    assert_eq!(json_of(&frames[2])["delta"], json!({"type": "text_delta", "text": "Hel"}));
    assert_eq!(json_of(&frames[5])["content_block"]["type"], "tool_use");
    assert_eq!(json_of(&frames[6])["delta"], json!({"type": "input_json_delta", "partial_json": "{\"ci"}));
    let md = json_of(&frames[9]);
    assert_eq!(md["delta"]["stop_reason"], "tool_use");
    assert_eq!(md["usage"]["output_tokens"], 9);

    // 线上格式
    let wire: String = frames.iter().map(SseFrame::to_wire).collect();
    assert!(wire.starts_with("event: message_start\ndata: {"));
    assert_eq!(parse_sse(&wire), frames);
}

#[test]
fn anthropic_stream_rejects_invalid_order() {
    let mut enc = AnthropicStreamEncoder::new();
    // 首个事件不是 message_start
    assert!(enc.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());

    let mut enc = AnthropicStreamEncoder::new();
    enc.push(StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None }).unwrap();
    // delta 先于 start
    assert!(enc.push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "a".into() } }).is_err());
    enc.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).unwrap();
    // 文本块上的 tool delta
    assert!(enc.push(StreamEvent::ToolCallDelta { index: 0, partial_json: "{}".into() }).is_err());
    // 重复 index
    assert!(enc.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());
    // 重复 message_start
    assert!(enc.push(StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None }).is_err());
    enc.push(StreamEvent::ContentBlockStop { index: 0 }).unwrap();
    // 重复 stop
    assert!(enc.push(StreamEvent::ContentBlockStop { index: 0 }).is_err());
    enc.push(StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None }).unwrap();
    // 结束后不再接受事件
    assert!(enc.push(StreamEvent::Usage(Usage::default())).is_err());
}

#[test]
fn anthropic_stream_message_stop_closes_open_blocks() {
    let mut enc = AnthropicStreamEncoder::new();
    let frames = run_stream(
        &mut enc,
        vec![
            StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None },
            StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
            StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "t".into() } },
            StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Signature { signature: "s".into() } },
            StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None },
        ],
    );
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(
        names,
        vec!["message_start", "content_block_start", "content_block_delta", "content_block_delta", "content_block_stop", "message_delta", "message_stop"]
    );
}

#[test]
fn anthropic_stream_error_frame() {
    let mut enc = AnthropicStreamEncoder::new();
    let frames = enc
        .push(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message: "upstream died".into() })
        .unwrap();
    assert_eq!(frames[0].event.as_deref(), Some("error"));
    assert_eq!(
        json_of(&frames[0]),
        json!({"type": "error", "error": {"type": "api_error", "message": "upstream died"}})
    );
    assert!(enc.push(StreamEvent::Usage(Usage::default())).is_err());
}

#[test]
fn openai_stream_chunks_and_done() {
    let frames = run_stream(&mut OpenAiStreamEncoder::new(true), text_and_tool_stream());
    assert_eq!(frames.last().unwrap().data, "[DONE]");
    assert!(frames.iter().all(|f| f.event.is_none()));

    let chunks: Vec<Value> = frames[..frames.len() - 1].iter().map(json_of).collect();
    for c in &chunks {
        assert_eq!(c["object"], "chat.completion.chunk");
        assert_eq!(c["id"], "msg_1");
        assert_eq!(c["model"], "m");
    }
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "Hel");
    assert_eq!(chunks[2]["choices"][0]["delta"]["content"], "lo");
    // 工具调用起始携带 id 与 name，增量只携带 arguments，index 从 0 起
    let tc0 = &chunks[3]["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!((tc0["index"].as_u64(), tc0["id"].as_str(), tc0["function"]["name"].as_str()), (Some(0), Some("call_1"), Some("get_weather")));
    let tc1 = &chunks[4]["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!((tc1["index"].as_u64(), tc1["function"]["arguments"].as_str()), (Some(0), Some("{\"ci")));
    // 拼接后的 arguments 是合法 JSON
    let args: String = chunks[3..=5]
        .iter()
        .map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(serde_json::from_str::<Value>(&args).unwrap(), json!({"city": "Rome"}));
    // finish chunk，然后 usage chunk（choices 为空）
    assert_eq!(chunks[6]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chunks[7]["choices"], json!([]));
    assert_eq!(chunks[7]["usage"]["prompt_tokens"], 120);
    assert_eq!(chunks[7]["usage"]["completion_tokens"], 9);
    assert_eq!(chunks.len(), 8);
}

#[test]
fn openai_stream_without_include_usage_has_no_usage_chunk() {
    let frames = run_stream(&mut OpenAiStreamEncoder::new(false), text_and_tool_stream());
    assert!(frames.iter().all(|f| !f.data.contains("\"usage\"")));
    assert_eq!(frames.last().unwrap().data, "[DONE]");
}

#[test]
fn openai_stream_two_tool_calls_get_distinct_slots() {
    let frames = run_stream(
        &mut OpenAiStreamEncoder::new(false),
        vec![
            StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None },
            StreamEvent::ContentBlockStart { index: 0, block: BlockStart::ToolUse { id: "a".into(), name: "f".into() } },
            StreamEvent::ContentBlockStart { index: 1, block: BlockStart::ToolUse { id: "b".into(), name: "g".into() } },
            StreamEvent::ToolCallDelta { index: 1, partial_json: "{}".into() },
            StreamEvent::ToolCallDelta { index: 0, partial_json: "{}".into() },
            StreamEvent::MessageStop { stop_reason: StopReason::ToolUse, stop_sequence: None },
        ],
    );
    let slot = |i: usize| json_of(&frames[i])["choices"][0]["delta"]["tool_calls"][0]["index"].as_u64().unwrap();
    assert_eq!((slot(1), slot(2), slot(3), slot(4)), (0, 1, 1, 0));
}

#[test]
fn openai_stream_rejects_invalid_order_and_emits_error_frame() {
    let mut enc = OpenAiStreamEncoder::new(false);
    assert!(enc.push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "x".into() } }).is_err());
    enc.push(StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None }).unwrap();
    assert!(enc.push(StreamEvent::ToolCallDelta { index: 5, partial_json: "{}".into() }).is_err());
    let frames = enc.push(StreamEvent::Error { kind: ApiErrorKind::RateLimit, message: "slow".into() }).unwrap();
    assert_eq!(json_of(&frames[0])["error"]["type"], "rate_limit_error");
    assert!(enc.push(StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None }).is_err());
}

#[test]
fn stream_stop_reason_mapping() {
    let reasons = [
        (StopReason::EndTurn, "stop", "end_turn"),
        (StopReason::MaxTokens, "length", "max_tokens"),
        (StopReason::StopSequence, "stop", "stop_sequence"),
        (StopReason::ToolUse, "tool_calls", "tool_use"),
        (StopReason::ContentFilter, "content_filter", "refusal"),
    ];
    for (reason, oa_expect, an_expect) in reasons {
        let events = vec![
            StreamEvent::MessageStart { id: "m".into(), model: "x".into(), usage: None },
            StreamEvent::MessageStop { stop_reason: reason, stop_sequence: None },
        ];
        let o = run_stream(&mut OpenAiStreamEncoder::new(false), events.clone());
        assert_eq!(json_of(&o[1])["choices"][0]["finish_reason"], oa_expect);
        let a = run_stream(&mut AnthropicStreamEncoder::new(), events);
        assert_eq!(json_of(&a[1])["delta"]["stop_reason"], an_expect);
    }
}

// ───────────── 协议标识 ─────────────

#[test]
fn protocol_ids_match_frontend_names() {
    let ids: Vec<&str> = ProtocolKind::ALL.iter().map(|p| p.id()).collect();
    assert_eq!(ids, vec!["openai-chat", "openai-responses", "anthropic-messages", "gemini"]);
    for p in ProtocolKind::ALL {
        assert_eq!(ProtocolKind::from_id(p.id()), Some(p));
    }
    assert_eq!(ProtocolKind::from_id("nope"), None);
}

#[test]
fn error_bodies_per_protocol() {
    let e = ApiError::new(ApiErrorKind::NotImplemented, "x").with_code("adapter_not_implemented");
    let o = e.to_body(ProtocolKind::OpenAiResponses);
    assert_eq!(o["error"]["type"], "not_implemented_error");
    assert_eq!(o["error"]["code"], "adapter_not_implemented");
    let a = e.to_body(ProtocolKind::AnthropicMessages);
    assert_eq!(a["type"], "error");
    assert!(a["error"].get("code").is_none());
}

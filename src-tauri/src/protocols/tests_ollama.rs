//! Ollama 协议测试：/api/chat 请求、NDJSON 流（编码 / 增量解码）、错误、跨协议转换。

use serde_json::{json, Value};

use super::anthropic::{self, AnthropicStreamEncoder};
use super::canonical::*;
use super::gemini::GeminiStreamEncoder;
use super::ollama::{self, OllamaStreamDecoder, OllamaStreamEncoder};
use super::openai_chat::{self, OpenAiStreamEncoder};
use super::openai_responses::{self, ResponsesStreamEncoder};
use super::testkit::*;
use super::*;

macro_rules! fixture {
    ($name:literal) => {
        fx(include_str!(concat!("fixtures/", $name, ".json")))
    };
}

const OLLAMA_STREAM: &str = include_str!("fixtures/ollama_stream.ndjson");

fn ol(v: &Value) -> CanonicalRequest {
    ollama::decode_request(v).expect("ollama decode")
}
fn oa(v: &Value) -> CanonicalRequest {
    openai_chat::decode_request(v).expect("openai decode")
}
fn an(v: &Value) -> CanonicalRequest {
    anthropic::decode_request(v).expect("anthropic decode")
}

/// Ollama 没有 user / tool_choice，总是返回 usage，模型名也不同：对齐后再比较。
fn align(o: CanonicalRequest, mut other: CanonicalRequest) -> (CanonicalRequest, CanonicalRequest) {
    let o = semantic(o);
    other = semantic(other);
    other.model = o.model.clone();
    other.user = None;
    other.tool_choice = None;
    other.include_usage = true;
    other.stream = o.stream; // Ollama 缺省为流式
    (o, other)
}

fn decode_events(bytes: &[u8], size: usize) -> Vec<StreamEvent> {
    let mut dec = OllamaStreamDecoder::new();
    let mut out = Vec::new();
    for chunk in bytes.chunks(size) {
        out.extend(dec.push(chunk).expect("push"));
    }
    out.extend(dec.finish().expect("finish"));
    out
}

fn call(id: &str, name: &str, args: Value) -> ContentPart {
    ContentPart::ToolUse { id: id.into(), name: name.into(), input: args }
}
fn result(id: &str, text: &str) -> ContentPart {
    ContentPart::ToolResult { tool_use_id: id.into(), content: vec![ContentPart::text(text)], is_error: false }
}

fn lines_of(enc: &mut OllamaStreamEncoder, events: Vec<StreamEvent>) -> Vec<Value> {
    events
        .into_iter()
        .flat_map(|e| enc.push(e).expect("valid sequence"))
        .map(|l| {
            assert!(
                l.ends_with('\n') && !l[..l.len() - 1].contains('\n'),
                "every NDJSON line must end with exactly one newline: {l:?}"
            );
            serde_json::from_str(l.trim()).unwrap()
        })
        .collect()
}

fn run_encoder(enc: &mut dyn StreamEncoder, events: Vec<StreamEvent>) -> Vec<SseFrame> {
    events.into_iter().flat_map(|e| enc.push(e).expect("valid sequence")).collect()
}

// ───────────── 纯文本 / system / options ─────────────

#[test]
fn text_request_decodes_to_same_canonical_as_openai() {
    let (o, p) = align(ol(&fixture!("ollama_text_request")), oa(&fixture!("openai_text_request")));
    assert_eq!(o, p);
    assert_eq!(o.system, vec!["You are helpful.", "Answer briefly."]);
    assert_eq!(o.params.max_tokens, Some(256));
    assert_eq!(o.params.stop, vec!["END"]);
}

#[test]
fn stream_defaults_to_true_unlike_openai() {
    let body = json!({"model": "m", "messages": [{"role": "user", "content": "hi"}]});
    assert!(ol(&body).stream, "Ollama streams unless told otherwise");
    assert!(!ol(&json!({"model": "m", "stream": false, "messages": [{"role": "user", "content": "hi"}]})).stream);
    // 编码到上游总是显式写出
    let mut req = ol(&body);
    assert_eq!(ollama::encode_request(&req).unwrap().body["stream"], true);
    req.stream = false;
    assert_eq!(ollama::encode_request(&req).unwrap().body["stream"], false);
}

#[test]
fn options_mapping_and_warnings() {
    let req = ol(&json!({
        "model": "m",
        "messages": [{"role": "user", "content": "x"}],
        "options": {
            "temperature": 0.3, "top_p": 0.8, "top_k": 20, "num_predict": 77, "stop": "STOP", "seed": 5,
            "presence_penalty": 0.1, "frequency_penalty": 0.2,
            "num_ctx": 8192, "repeat_penalty": 1.1, "mirostat": 2
        },
        "format": "json", "keep_alive": "5m", "raw": true, "logprobs": true
    }));
    assert_eq!(
        req.params,
        GenerationParams {
            max_tokens: Some(77),
            temperature: Some(0.3),
            top_p: Some(0.8),
            top_k: Some(20),
            stop: vec!["STOP".into()],
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.2),
            seed: Some(5),
        }
    );
    for k in
        ["options.num_ctx", "options.repeat_penalty", "options.mirostat", "format", "keep_alive", "raw", "logprobs"]
    {
        assert!(has_warning(&req.warnings, &format!("unsupported field ignored: {k}")), "{k}: {:?}", req.warnings);
    }
    // num_predict <= 0 表示不限制 / 填满上下文
    for n in [-1, -2, 0] {
        let r =
            ol(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "options": {"num_predict": n}}));
        assert_eq!(r.params.max_tokens, None, "num_predict {n}");
        assert!(r.warnings.is_empty());
    }
    let stops =
        ol(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "options": {"stop": ["a", "b"]}}));
    assert_eq!(stops.params.stop, vec!["a", "b"]);
}

#[test]
fn encode_maps_options_back() {
    let mut req = ol(&fixture!("ollama_text_request"));
    req.params.top_k = Some(10);
    req.params.seed = Some(9);
    req.params.presence_penalty = Some(0.5);
    req.params.frequency_penalty = Some(0.25);
    req.user = Some("u".into());
    let enc = ollama::encode_request(&req).unwrap();
    assert_eq!(
        enc.body["options"],
        json!({"temperature": 0.2, "top_p": 0.9, "top_k": 10, "num_predict": 256, "stop": ["END"], "seed": 9, "presence_penalty": 0.5, "frequency_penalty": 0.25})
    );
    assert_eq!(enc.body["messages"][0], json!({"role": "system", "content": "You are helpful."}));
    assert_eq!(enc.body["messages"][1], json!({"role": "system", "content": "Answer briefly."}));
    assert!(has_warning(&enc.warnings, "user identifier"));
}

#[test]
fn think_decode_and_encode() {
    let mk =
        |think: Value| ol(&json!({"model": "qwen3", "messages": [{"role": "user", "content": "x"}], "think": think}));
    assert_eq!(
        mk(json!(true)).reasoning,
        Some(ReasoningConfig { effort: None, budget_tokens: None, include_thoughts: Some(true) })
    );
    assert_eq!(mk(json!(false)).reasoning.unwrap().include_thoughts, Some(false));
    assert_eq!(
        mk(json!("high")).reasoning,
        Some(ReasoningConfig { effort: Some("high".into()), budget_tokens: None, include_thoughts: Some(true) })
    );
    assert!(matches!(
        ollama::decode_request(&json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "think": 3})),
        Err(ProtocolError::InvalidRequest(_))
    ));

    let enc = |model: &str, r: ReasoningConfig| {
        let req = CanonicalRequest {
            model: model.into(),
            messages: vec![Message::user(vec![ContentPart::text("x")])],
            reasoning: Some(r),
            ..Default::default()
        };
        ollama::encode_request(&req).unwrap().body.get("think").cloned()
    };
    let r = |effort: Option<&str>, budget: Option<u32>, inc: Option<bool>| ReasoningConfig {
        effort: effort.map(Into::into),
        budget_tokens: budget,
        include_thoughts: inc,
    };
    assert_eq!(enc("qwen3", r(None, None, Some(true))), Some(json!(true)));
    assert_eq!(
        enc("qwen3", r(Some("high"), None, None)),
        Some(json!(true)),
        "non gpt-oss models only understand booleans"
    );
    assert_eq!(enc("qwen3", r(Some("none"), None, None)), Some(json!(false)));
    assert_eq!(enc("qwen3", r(None, None, Some(false))), Some(json!(false)));
    assert_eq!(enc("qwen3", r(None, Some(2048), None)), Some(json!(true)));
    assert_eq!(enc("qwen3", r(None, Some(0), None)), None);
    assert_eq!(enc("gpt-oss:20b", r(Some("high"), None, None)), Some(json!("high")));
    assert_eq!(enc("gpt-oss:20b", r(Some("minimal"), None, None)), Some(json!("low")));
    assert_eq!(enc("gpt-oss:120b", r(None, None, Some(true))), Some(json!("medium")));
    assert_eq!(enc("gpt-oss:20b", r(Some("none"), None, None)), None);
}

#[test]
fn invalid_and_unsupported_requests() {
    for bad in [
        json!({}),
        json!({"model": "", "messages": [{"role": "user", "content": "x"}]}),
        json!({"model": "m"}),
        json!({"model": "m", "messages": []}),
        json!({"model": "m", "messages": [{"content": "x"}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": ["x"]}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": "x", "images": "AAAA"}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": "x", "images": [5]}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "options": {"temperature": "hot"}}),
        json!({"model": "m", "messages": [{"role": "assistant", "content": "", "tool_calls": [{"function": {"arguments": {}}}]}]}),
        json!({"model": "m", "messages": [{"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "f", "arguments": "{oops"}}]}]}),
        json!([1]),
    ] {
        assert!(matches!(ollama::decode_request(&bad), Err(ProtocolError::InvalidRequest(_))), "{bad}");
    }
    for unsupported in [
        json!({"model": "m", "messages": [{"role": "narrator", "content": "x"}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": "x"}], "tools": [{"type": "web_search"}]}),
    ] {
        assert!(matches!(ollama::decode_request(&unsupported), Err(ProtocolError::Unsupported(_))), "{unsupported}");
    }
}

#[test]
fn late_system_messages_are_hoisted_and_warnings_hide_text() {
    let secret = "TOP-SECRET-PROMPT";
    let req = ol(&json!({"model": "m", "messages": [
        {"role": "user", "content": secret},
        {"role": "system", "content": "late"},
        {"role": "assistant", "content": "ok", "images": ["AAAA"]}
    ], "weird": secret}));
    assert_eq!(req.system, vec!["late"]);
    assert!(has_warning(&req.warnings, "hoisted"));
    assert!(has_warning(&req.warnings, "images on an assistant message"));
    assert!(has_warning(&req.warnings, "unsupported field ignored: weird"));
    assert!(req.warnings.iter().all(|w| !w.contains(secret)), "{:?}", req.warnings);
}

// ───────────── 工具调用与多轮工具链 ─────────────

#[test]
fn tool_chain_decodes_identically_to_openai() {
    let (o, p) = align(ol(&fixture!("ollama_tool_chain_request")), oa(&fixture!("openai_tool_chain_request")));
    assert_eq!(o, p);
    assert_eq!(o.messages.len(), 5);
    // 两条连续的 tool 消息合并为同一条 user 消息
    assert!(
        matches!(&o.messages[2].content[..], [ContentPart::ToolResult { tool_use_id: a, .. }, ContentPart::ToolResult { tool_use_id: b, .. }] if a == "call_1" && b == "call_2")
    );
}

#[test]
fn tool_chain_encodes_back_to_the_fixture_shape() {
    let body = fixture!("ollama_tool_chain_request");
    let enc = ollama::encode_request(&ol(&body)).unwrap();
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
    assert_eq!(enc.body["messages"], body["messages"]);
    assert_eq!(enc.body["tools"], body["tools"]);
    assert_eq!(enc.body["options"], body["options"]);
    // arguments 是对象而不是 JSON 字符串
    assert!(enc.body["messages"][1]["tool_calls"][0]["function"]["arguments"].is_object());
    let (back, orig) = align(ol(&enc.body), ol(&body));
    assert_eq!(back, orig);
}

#[test]
fn tool_results_are_paired_by_name_id_or_position() {
    let req = ol(&json!({"model": "m", "messages": [
        {"role": "user", "content": "go"},
        {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": "a", "arguments": {"n": 1}}},
            {"function": {"name": "b", "arguments": "{\"s\":\"x\"}"}},
            {"function": {"name": "a", "arguments": {"n": 2}}}
        ]},
        {"role": "tool", "tool_name": "a", "content": "A1"},
        {"role": "tool", "tool_name": "b", "content": "B"},
        {"role": "tool", "tool_name": "a", "content": "A2"}
    ]}));
    let ids: Vec<&str> = req.messages[1]
        .content
        .iter()
        .map(|p| if let ContentPart::ToolUse { id, .. } = p { id.as_str() } else { panic!() })
        .collect();
    assert_eq!(ids, vec!["call_0", "call_1", "call_2"], "deterministic ids keep the history prefix stable");
    assert!(matches!(&req.messages[1].content[1], ContentPart::ToolUse { input, .. } if *input == json!({"s": "x"})));
    let paired: Vec<&str> = req.messages[2]
        .content
        .iter()
        .map(|p| if let ContentPart::ToolResult { tool_use_id, .. } = p { tool_use_id.as_str() } else { panic!() })
        .collect();
    assert_eq!(paired, vec!["call_0", "call_1", "call_2"]);
    assert!(req.warnings.is_empty(), "{:?}", req.warnings);

    // 没有 tool_name：唯一未配对的调用
    let single = ol(&json!({"model": "m", "messages": [
        {"role": "user", "content": "go"},
        {"role": "assistant", "content": "", "tool_calls": [{"id": "only", "function": {"name": "a", "arguments": {}}}]},
        {"role": "tool", "content": "R"}
    ]}));
    assert!(
        matches!(&single.messages[2].content[0], ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == "only")
    );
    // 显式 tool_call_id 优先
    let explicit = ol(&json!({"model": "m", "messages": [
        {"role": "user", "content": "go"},
        {"role": "assistant", "content": "", "tool_calls": [{"id": "x1", "function": {"name": "a", "arguments": {}}}, {"id": "x2", "function": {"name": "a", "arguments": {}}}]},
        {"role": "tool", "tool_call_id": "x2", "tool_name": "a", "content": "second"}
    ]}));
    assert!(
        matches!(&explicit.messages[2].content[0], ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == "x2")
    );
    // 配不上的 tool 消息会告警
    let orphan = ol(&json!({"model": "m", "messages": [
        {"role": "user", "content": "go"}, {"role": "tool", "tool_name": "ghost", "content": "?"}]}));
    assert!(has_warning(&orphan.warnings, "could not be matched"));
}

#[test]
fn encode_resolves_tool_names_and_resets_call_index_per_message() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![
                ContentPart::Thinking { text: "hmm".into(), signature: Some("sig".into()) },
                call("a", "f", json!({"x": 1})),
                call("b", "g", json!({})),
            ]),
            Message::user(vec![
                result("b", "rb"),
                ContentPart::ToolResult {
                    tool_use_id: "ghost".into(),
                    content: vec![ContentPart::text("?")],
                    is_error: true,
                },
                ContentPart::text("and then?"),
            ]),
            Message::assistant(vec![call("c", "f", json!({"y": 2}))]),
        ],
        ..Default::default()
    };
    let enc = ollama::encode_request(&req).unwrap();
    let m = enc.body["messages"].as_array().unwrap();
    assert_eq!(m[1]["thinking"], "hmm");
    assert_eq!(m[1]["tool_calls"][1], json!({"id": "b", "function": {"index": 1, "name": "g", "arguments": {}}}));
    assert_eq!(m[2], json!({"role": "tool", "tool_name": "g", "content": "rb"}));
    assert_eq!(m[3], json!({"role": "tool", "tool_name": "ghost", "content": "?"}));
    assert_eq!(m[4], json!({"role": "user", "content": "and then?"}));
    assert_eq!(m[5]["tool_calls"][0]["function"]["index"], 0, "index is per assistant message");
    assert!(has_warning(&enc.warnings, "has no matching tool_use"));
    assert!(has_warning(&enc.warnings, "is_error"));
}

#[test]
fn tool_choice_and_parallel_flags() {
    let mk = |choice: Option<ToolChoice>, parallel: Option<bool>| {
        let req = CanonicalRequest {
            model: "m".into(),
            messages: vec![Message::user(vec![ContentPart::text("x")])],
            tools: vec![ToolDefinition {
                name: "f".into(),
                description: None,
                input_schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            }],
            tool_choice: choice,
            parallel_tool_calls: parallel,
            ..Default::default()
        };
        ollama::encode_request(&req).unwrap()
    };
    let auto = mk(Some(ToolChoice::Auto), None);
    assert_eq!(auto.body["tools"][0]["function"]["description"], "");
    assert!(auto.warnings.is_empty());
    // none = 不给模型任何工具
    let none = mk(Some(ToolChoice::None), None);
    assert!(none.body.get("tools").is_none());
    assert!(has_warning(&mk(Some(ToolChoice::Any), None).warnings, "cannot be forced"));
    assert!(has_warning(&mk(Some(ToolChoice::Tool { name: "f".into() }), None).warnings, "cannot be forced"));
    assert!(has_warning(&mk(None, Some(false)).warnings, "parallel_tool_calls=false"));
}

// ───────────── 图片 ─────────────

#[test]
fn image_request_roundtrips_and_matches_openai() {
    let body = fixture!("ollama_image_request");
    let req = ol(&body);
    assert_eq!(
        req.messages[0].content,
        vec![
            ContentPart::text("Describe this"),
            ContentPart::Image {
                source: ImageSource::Base64 { media_type: "image/png".into(), data: "iVBORw0KGgoAAAANSUhEUg==".into() },
                detail: None
            },
        ]
    );
    assert!(req.warnings.is_empty());
    let enc = ollama::encode_request(&req).unwrap();
    assert_eq!(enc.body["messages"], body["messages"]);

    // 与 OpenAI 图片请求里的 base64 图片一致
    let oa_req = oa(&fixture!("openai_image_request"));
    assert_eq!(oa_req.messages[0].content[1], req.messages[0].content[1]);
}

#[test]
fn image_media_types_are_sniffed_and_urls_are_not_sendable() {
    let req = ol(&json!({"model": "m", "messages": [{"role": "user", "content": "x", "images": [
        "/9j/4AAQ", "R0lGODlh", "UklGRhAA", "data:image/webp;base64,UklGRg==", "AAAA"
    ]}]}));
    let types: Vec<&str> = req.messages[0].content[1..]
        .iter()
        .map(|p| {
            if let ContentPart::Image { source: ImageSource::Base64 { media_type, .. }, .. } = p {
                media_type.as_str()
            } else {
                panic!()
            }
        })
        .collect();
    assert_eq!(types, vec!["image/jpeg", "image/gif", "image/webp", "image/webp", "image/png"]);
    assert!(
        matches!(&req.messages[0].content[4], ContentPart::Image { source: ImageSource::Base64 { data, .. }, .. } if data == "UklGRg=="),
        "data URL prefix is stripped"
    );
    assert_eq!(req.warnings.len(), 1);
    assert!(has_warning(&req.warnings, "images[4]: media type could not be detected"));

    let canonical = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![
            ContentPart::text("a"),
            ContentPart::text("b"),
            ContentPart::Image { source: ImageSource::Url { url: "https://x/y.png".into() }, detail: None },
            ContentPart::Image { source: ImageSource::FileId { file_id: "f".into() }, detail: None },
            ContentPart::Image {
                source: ImageSource::Base64 { media_type: "image/jpeg".into(), data: "/9j/4A==".into() },
                detail: None,
            },
        ])],
        ..Default::default()
    };
    let enc = ollama::encode_request(&canonical).unwrap();
    assert_eq!(enc.body["messages"][0], json!({"role": "user", "content": "a\nb", "images": ["/9j/4A=="]}));
    assert!(has_warning(&enc.warnings, "image URL cannot be sent to Ollama"));
    assert!(has_warning(&enc.warnings, "file_id cannot be sent to Ollama"));
}

#[test]
fn large_base64_image_stays_out_of_text() {
    let big = "A".repeat(2 * 1024 * 1024);
    let req = ol(&json!({"model": "m", "messages": [{"role": "user", "content": "hi", "images": [big]}]}));
    assert_eq!(req.text_bytes(), 2);
    assert_eq!(req.media_bytes(), big.len());
}

// ───────────── 响应 ─────────────

#[test]
fn tool_response_matches_openai_and_roundtrips() {
    let r = ollama::decode_response(&fixture!("ollama_tool_response")).unwrap();
    let mut o = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    o.usage.cache_read_tokens = None; // Ollama 没有缓存计数
    assert!(r.id.starts_with("ollama-"));
    assert_eq!(CanonicalResponse { id: o.id.clone(), ..r.clone() }, o);
    assert_eq!(r.created, Some(1_760_000_000), "created_at is parsed as RFC 3339");
    assert_eq!(r.stop_reason, StopReason::ToolUse, "done_reason `stop` with tool calls means tool_use");
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens), (Some(120), Some(20)));

    let out = ollama::encode_response(&r);
    assert_eq!(out["done"], true);
    assert_eq!(out["done_reason"], "stop");
    assert_eq!(out["message"]["role"], "assistant");
    assert_eq!(out["message"]["content"], "Checking.");
    assert_eq!(
        out["message"]["tool_calls"][0],
        json!({"id": "call_9", "function": {"index": 0, "name": "get_weather", "arguments": {"city": "Oslo"}}})
    );
    assert_eq!((out["prompt_eval_count"].as_u64(), out["eval_count"].as_u64()), (Some(120), Some(20)));
    assert!(out["created_at"].as_str().unwrap().ends_with('Z'));
    let back = ollama::decode_response(&out).unwrap();
    assert_eq!(CanonicalResponse { id: r.id.clone(), ..back }, r);
}

#[test]
fn response_decode_edge_cases() {
    let dec = |v: Value| ollama::decode_response(&v).unwrap();
    let length = dec(
        json!({"model": "m", "message": {"role": "assistant", "content": "cut"}, "done": true, "done_reason": "length", "eval_count": 3}),
    );
    assert_eq!(length.stop_reason, StopReason::MaxTokens);
    assert_eq!(length.usage.input_tokens, None);
    assert_eq!(
        dec(json!({"message": {"role": "assistant", "content": "x"}, "done": true})).stop_reason,
        StopReason::EndTurn
    );
    assert_eq!(
        dec(json!({"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "load"})).stop_reason,
        StopReason::Other("load".into())
    );
    let thinking = dec(json!({"message": {"role": "assistant", "content": "answer", "thinking": "hmm"}, "done": true}));
    assert_eq!(
        thinking.content,
        vec![ContentPart::Thinking { text: "hmm".into(), signature: None }, ContentPart::text("answer")]
    );

    // 没有 id 的 tool call 得到彼此不同的生成 id；字符串参数也能容忍
    let calls = dec(json!({"message": {"role": "assistant", "content": "", "tool_calls": [
        {"function": {"name": "f", "arguments": {"a": 1}}}, {"function": {"name": "f", "arguments": "{\"b\":2}"}}]}, "done": true}));
    let ids: Vec<&str> = calls
        .content
        .iter()
        .map(|p| if let ContentPart::ToolUse { id, .. } = p { id.as_str() } else { panic!() })
        .collect();
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|i| i.starts_with("call_") && i.len() == 29));

    for bad in [
        json!({}),
        json!([]),
        json!({"done": true}),
        json!({"error": "model not found"}),
        json!({"message": {"tool_calls": [{"function": {"arguments": {}}}]}}),
        json!({"message": {"tool_calls": [{"function": {"name": "f", "arguments": 3}}]}}),
    ] {
        assert!(matches!(ollama::decode_response(&bad), Err(ProtocolError::InvalidResponse(_))), "{bad}");
    }
}

#[test]
fn encode_response_maps_finish_reasons_and_omits_missing_counts() {
    let mk = |stop: StopReason, usage: Usage| CanonicalResponse {
        id: String::new(),
        model: "m".into(),
        created: None,
        content: vec![
            ContentPart::Thinking { text: "t".into(), signature: None },
            ContentPart::text("a"),
            ContentPart::text("b"),
        ],
        stop_reason: stop,
        stop_sequence: None,
        usage,
    };
    let v = ollama::encode_response(&mk(StopReason::MaxTokens, Usage::default()));
    assert_eq!(v["done_reason"], "length");
    assert!(v.get("prompt_eval_count").is_none() && v.get("eval_count").is_none());
    assert_eq!(v["message"]["content"], "ab");
    assert_eq!(v["message"]["thinking"], "t");
    assert!(v["message"].get("tool_calls").is_none());
    for r in [
        StopReason::EndTurn,
        StopReason::StopSequence,
        StopReason::ToolUse,
        StopReason::ContentFilter,
        StopReason::Other("x".into()),
    ] {
        assert_eq!(ollama::encode_response(&mk(r, Usage::default()))["done_reason"], "stop");
    }
}

// ───────────── 错误体 ─────────────

#[test]
fn error_bodies() {
    let e = ollama::decode_error(404, &json!({"error": "model \"nope\" not found, try pulling it first"}));
    assert_eq!(
        (e.kind, e.message.as_str()),
        (ApiErrorKind::NotFound, "model \"nope\" not found, try pulling it first")
    );
    assert_eq!(
        ollama::decode_error(500, &json!({"error": {"message": "llama runner crashed"}})).message,
        "llama runner crashed"
    );
    assert_eq!(ollama::decode_error(503, &Value::String("server busy".into())).kind, ApiErrorKind::BadGateway);
    assert_eq!(ollama::decode_error(400, &Value::Null).message, "upstream returned HTTP 400");

    let api = ApiError::new(ApiErrorKind::RateLimit, "busy");
    assert_eq!(ollama::encode_error(&api), json!({"error": "busy"}));
    assert_eq!(ollama::error_line(&api), "{\"error\":\"busy\"}\n");
}

// ───────────── 流式编码 ─────────────

fn thinking_text_tool_events() -> Vec<StreamEvent> {
    vec![
        StreamEvent::MessageStart {
            id: "m1".into(),
            model: "qwen3".into(),
            usage: Some(Usage { input_tokens: Some(120), ..Default::default() }),
        },
        StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "plan".into() } },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Signature { signature: "ignored".into() } },
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
        StreamEvent::ContentBlockStart {
            index: 3,
            block: BlockStart::ToolUse { id: "call_2".into(), name: "get_time".into() },
        },
        StreamEvent::ContentBlockStop { index: 3 },
        StreamEvent::Usage(Usage { output_tokens: Some(9), ..Default::default() }),
        StreamEvent::MessageStop { stop_reason: StopReason::ToolUse, stop_sequence: None },
    ]
}

#[test]
fn stream_encoder_emits_ndjson_lines() {
    let lines = lines_of(&mut OllamaStreamEncoder::new(), thinking_text_tool_events());
    assert_eq!(lines.len(), 6);
    assert!(lines.iter().all(|l| l["model"] == "qwen3" && l["created_at"].is_string()));
    assert_eq!(lines[0]["message"], json!({"role": "assistant", "content": "", "thinking": "plan"}));
    assert_eq!(lines[1]["message"], json!({"role": "assistant", "content": "Hel"}));
    assert_eq!(lines[2]["message"]["content"], "lo");
    // tool call 整块输出（参数是对象），index 在整个响应内递增
    assert_eq!(
        lines[3]["message"],
        json!({"role": "assistant", "content": "", "tool_calls": [{"id": "call_1", "function": {"index": 0, "name": "get_weather", "arguments": {"city": "Rome"}}}]})
    );
    assert_eq!(
        lines[4]["message"]["tool_calls"][0]["function"],
        json!({"index": 1, "name": "get_time", "arguments": {}})
    );
    assert!(lines[..5].iter().all(|l| l["done"] == false));
    let done = &lines[5];
    assert_eq!((done["done"].as_bool(), done["done_reason"].as_str()), (Some(true), Some("stop")));
    assert_eq!((done["prompt_eval_count"].as_u64(), done["eval_count"].as_u64()), (Some(120), Some(9)));
    assert_eq!(done["message"], json!({"role": "assistant", "content": ""}));
}

#[test]
fn stream_encoder_stop_reasons_flush_and_errors() {
    let start = StreamEvent::MessageStart { id: String::new(), model: "m".into(), usage: None };
    let mut e = OllamaStreamEncoder::new();
    let lines = lines_of(
        &mut e,
        vec![
            start.clone(),
            StreamEvent::ContentBlockStart {
                index: 0,
                block: BlockStart::ToolUse { id: "c".into(), name: "f".into() },
            },
            StreamEvent::ToolCallDelta { index: 0, partial_json: "{\"a\":1}".into() },
            StreamEvent::MessageStop { stop_reason: StopReason::MaxTokens, stop_sequence: None },
        ],
    );
    assert_eq!(
        lines[0]["message"]["tool_calls"][0]["function"]["arguments"],
        json!({"a": 1}),
        "open tool call is flushed before the final line"
    );
    assert_eq!(lines[1]["done_reason"], "length");
    assert!(lines[1].get("eval_count").is_none());
    assert!(e.push(StreamEvent::Usage(Usage::default())).is_err());

    let mut e = OllamaStreamEncoder::new();
    assert_eq!(
        e.push(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message: "upstream died".into() }).unwrap(),
        vec!["{\"error\":\"upstream died\"}\n".to_string()]
    );
    assert!(e.push(start.clone()).is_err());
}

#[test]
fn stream_encoder_rejects_invalid_order() {
    let start = StreamEvent::MessageStart { id: "x".into(), model: "m".into(), usage: None };
    let mut e = OllamaStreamEncoder::new();
    assert!(e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());
    let mut e = OllamaStreamEncoder::new();
    e.push(start.clone()).unwrap();
    assert!(e.push(start).is_err(), "duplicate start");
    assert!(e.push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "x".into() } }).is_err());
    assert!(e.push(StreamEvent::ContentBlockStop { index: 3 }).is_err());
    e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).unwrap();
    assert!(e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());
    assert!(e.push(StreamEvent::ToolCallDelta { index: 0, partial_json: "{}".into() }).is_err());
    assert!(e
        .push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "x".into() } })
        .is_err());
    e.push(StreamEvent::ContentBlockStart {
        index: 1,
        block: BlockStart::ToolUse { id: "c".into(), name: "f".into() },
    })
    .unwrap();
    e.push(StreamEvent::ToolCallDelta { index: 1, partial_json: "{oops".into() }).unwrap();
    assert!(e.push(StreamEvent::ContentBlockStop { index: 1 }).is_err(), "invalid tool arguments JSON");
}

// ───────────── 流式解码 ─────────────

fn expected_fixture_stream() -> CanonicalResponse {
    CanonicalResponse {
        id: String::new(),
        model: "qwen3".into(),
        created: None,
        content: vec![
            ContentPart::Thinking { text: "Need the weather.".into(), signature: None },
            ContentPart::text("Checking 你好."),
            call("<generated>", "get_weather", json!({"city": "Oslo"})),
        ],
        stop_reason: StopReason::ToolUse,
        stop_sequence: None,
        usage: Usage {
            input_tokens: Some(120),
            output_tokens: Some(20),
            cache_read_tokens: None,
            cache_creation_tokens: None,
        },
    }
}

fn normalized(events: &[StreamEvent]) -> CanonicalResponse {
    let mut r = normalize_generated_ids(collect(events));
    assert!(r.id.starts_with("ollama-"), "{}", r.id);
    r.id.clear();
    r
}

#[test]
fn stream_decoder_handles_the_ndjson_fixture_at_every_split() {
    let bytes = OLLAMA_STREAM.as_bytes();
    for size in [1, 2, 3, 7, 40, 1000, bytes.len()] {
        assert_eq!(normalized(&decode_events(bytes, size)), expected_fixture_stream(), "chunk size {size}");
    }
    let events = decode_events(bytes, 64);
    assert!(matches!(events.first(), Some(StreamEvent::MessageStart { model, .. }) if model == "qwen3"));
    let n = events.len();
    assert!(matches!(events[n - 2], StreamEvent::Usage(_)));
    assert!(matches!(events[n - 1], StreamEvent::MessageStop { .. }));
    // 推理块、文本块各自独立，工具调用整块下发
    let starts: Vec<&BlockStart> = events
        .iter()
        .filter_map(|e| if let StreamEvent::ContentBlockStart { block, .. } = e { Some(block) } else { None })
        .collect();
    assert!(matches!(starts[..], [BlockStart::Thinking, BlockStart::Text, BlockStart::ToolUse { .. }]));
}

#[test]
fn stream_decoder_variants() {
    // CRLF、空行、最后一行没有换行
    let crlf = OLLAMA_STREAM.replace('\n', "\r\n\r\n");
    let trimmed = crlf.trim_end();
    assert_eq!(normalized(&decode_events(trimmed.as_bytes(), 13)), expected_fixture_stream());

    // 带 id 的 tool call 保留 id；done_reason=length → MaxTokens
    let sse = "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"id\":\"call_x\",\"function\":{\"name\":\"f\",\"arguments\":{}}}]},\"done\":false}\n\
               {\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"length\"}\n";
    let resp = collect(&decode_events(sse.as_bytes(), 5));
    assert_eq!(resp.content, vec![call("call_x", "f", json!({}))]);
    assert_eq!(resp.stop_reason, StopReason::MaxTokens);
    assert_eq!(resp.usage, Usage::default());

    // 思考与文本交替时每次切换都开新块
    let alt = "{\"model\":\"m\",\"message\":{\"content\":\"\",\"thinking\":\"a\"},\"done\":false}\n\
               {\"model\":\"m\",\"message\":{\"content\":\"b\"},\"done\":false}\n\
               {\"model\":\"m\",\"message\":{\"content\":\"\",\"thinking\":\"c\"},\"done\":false}\n\
               {\"model\":\"m\",\"done\":true,\"done_reason\":\"stop\"}\n";
    let resp = collect(&decode_events(alt.as_bytes(), 9));
    assert_eq!(
        resp.content,
        vec![
            ContentPart::Thinking { text: "a".into(), signature: None },
            ContentPart::text("b"),
            ContentPart::Thinking { text: "c".into(), signature: None }
        ]
    );
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
}

#[test]
fn stream_decoder_error_paths() {
    // 流内错误行
    let sse = "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"par\"},\"done\":false}\n{\"error\":\"llama runner process has terminated\"}\n";
    let events = decode_events(sse.as_bytes(), 17);
    assert!(
        matches!(events[events.len() - 2], StreamEvent::ContentBlockStop { .. }),
        "open text block is closed before the error"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message }) if message == "llama runner process has terminated")
    );

    // 没有 done 的截断流
    let truncated = "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"par\"},\"done\":false}\n{\"model\":\"m\",\"mess";
    let mut dec = OllamaStreamDecoder::new();
    let mut events = dec.push(truncated.as_bytes()).unwrap();
    assert!(dec.finish().is_err(), "a half-written final line is a malformed stream");
    let complete_lines = "{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"par\"},\"done\":false}\n";
    let mut dec = OllamaStreamDecoder::new();
    events.clear();
    events.extend(dec.push(complete_lines.as_bytes()).unwrap());
    events.extend(dec.finish().unwrap());
    assert!(matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::BadGateway, .. })));

    // 坏行 / 空流 / 非对象
    assert!(matches!(OllamaStreamDecoder::new().push(b"{oops}\n"), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(OllamaStreamDecoder::new().finish(), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(OllamaStreamDecoder::new().push(b"[1]\n"), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(
        OllamaStreamDecoder::new()
            .push(b"{\"message\":{\"tool_calls\":[{\"function\":{\"arguments\":{}}}]},\"done\":false}\n"),
        Err(ProtocolError::InvalidResponse(_))
    ));
}

#[test]
fn stream_encode_then_decode_is_lossless() {
    let original = thinking_text_tool_events();
    let wire: String = {
        let mut enc = OllamaStreamEncoder::new();
        original.iter().cloned().flat_map(|e| enc.push(e).unwrap()).collect()
    };
    let mut expected = collect(&original);
    expected.id = String::new();
    expected.content = vec![
        ContentPart::Thinking { text: "plan".into(), signature: None },
        ContentPart::text("Hello"),
        call("call_1", "get_weather", json!({"city": "Rome"})),
        call("call_2", "get_time", json!({})),
    ];
    for size in [1, 9, 200, wire.len()] {
        let mut got = collect(&decode_events(wire.as_bytes(), size));
        got.id = String::new();
        assert_eq!(got, expected, "chunk size {size}");
    }
}

// ───────────── 跨协议 ─────────────

#[test]
fn ollama_client_to_openai_and_anthropic_upstreams() {
    let canonical = ol(&fixture!("ollama_tool_chain_request"));
    let up = openai_chat::encode_request(&canonical).unwrap();
    let msgs = up.body["messages"].as_array().unwrap();
    assert_eq!(
        msgs.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["user", "assistant", "tool", "tool", "assistant", "tool"]
    );
    assert_eq!(msgs[2]["tool_call_id"], "call_1", "tool_name pairing was resolved to ids");
    assert_eq!(msgs[3]["tool_call_id"], "call_2");
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], "{\"city\":\"Paris\"}");
    assert_eq!(up.body["max_tokens"], 512);

    let up = anthropic::encode_request(&canonical).unwrap();
    assert_eq!(up.body["messages"][2]["content"][1]["tool_use_id"], "call_2");
    assert_eq!(up.body["tools"][0]["name"], "get_weather");

    // 响应：OpenAI / Anthropic 上游 → Ollama 客户端
    let from_oa = ollama::encode_response(&openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap());
    assert_eq!(from_oa["message"]["tool_calls"][0]["function"]["arguments"], json!({"city": "Oslo"}));
    assert_eq!((from_oa["prompt_eval_count"].as_u64(), from_oa["eval_count"].as_u64()), (Some(120), Some(20)));
    assert_eq!(from_oa["done_reason"], "stop");
    let from_an = ollama::encode_response(&anthropic::decode_response(&fixture!("anthropic_tool_response")).unwrap());
    assert_eq!(from_an["prompt_eval_count"], 120, "Anthropic input 20 + cache 100");
    assert_eq!(from_an["message"]["tool_calls"][0]["function"]["name"], "get_weather");
}

#[test]
fn openai_and_anthropic_clients_to_ollama_upstream() {
    let expected = fixture!("ollama_tool_chain_request");
    let strip_ids = |mut msgs: Value| {
        for m in msgs.as_array_mut().unwrap() {
            for c in m.get_mut("tool_calls").and_then(Value::as_array_mut).into_iter().flatten() {
                c.as_object_mut().unwrap().remove("id");
            }
        }
        msgs
    };
    let from_oa = ollama::encode_request(&oa(&fixture!("openai_tool_chain_request"))).unwrap();
    assert_eq!(strip_ids(from_oa.body["messages"].clone()), strip_ids(expected["messages"].clone()));
    assert_eq!(from_oa.body["messages"][1]["tool_calls"][0]["id"], "call_1");
    let from_an = ollama::encode_request(&an(&fixture!("anthropic_tool_chain_request"))).unwrap();
    assert_eq!(from_an.body["messages"], from_oa.body["messages"]);
    assert_eq!(from_oa.body["options"]["num_predict"], 512);

    // OpenAI 图片请求：base64 图片被送去 images，URL 图片告警丢弃
    let img = ollama::encode_request(&oa(&fixture!("openai_image_request"))).unwrap();
    assert_eq!(
        img.body["messages"][0],
        json!({"role": "user", "content": "Describe these", "images": ["iVBORw0KGgoAAAANSUhEUg=="]})
    );
    assert!(has_warning(&img.warnings, "image URL cannot be sent to Ollama"));

    // Ollama 上游响应 → OpenAI / Anthropic 客户端
    let resp = ollama::decode_response(&fixture!("ollama_tool_response")).unwrap();
    let chat = openai_chat::encode_response(&resp);
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"city\":\"Oslo\"}");
    assert_eq!(chat["usage"], json!({"prompt_tokens": 120, "completion_tokens": 20, "total_tokens": 140}));
    let msg = anthropic::encode_response(&resp);
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["usage"], json!({"input_tokens": 120, "output_tokens": 20}));
}

#[test]
fn ollama_client_to_responses_upstream() {
    let canonical = ol(&fixture!("ollama_text_request"));
    let r = openai_responses::encode_request(&canonical).unwrap();
    assert_eq!(r.body["instructions"], "You are helpful.\n\nAnswer briefly.");
    assert_eq!(r.body["max_output_tokens"], 256);
    assert!(has_warning(&r.warnings, "stop is not supported"));
    assert_eq!(r.body["input"].as_array().unwrap().len(), 3);
}

#[test]
fn ollama_stream_feeds_every_other_protocol_encoder() {
    let events = decode_events(OLLAMA_STREAM.as_bytes(), 29);

    let frames = run_encoder(&mut OpenAiStreamEncoder::new(true), events.clone());
    let chunks: Vec<Value> = frames.iter().filter(|f| f.data != "[DONE]").map(json_of).collect();
    let reasoning: String =
        chunks.iter().filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str()).collect();
    let text: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["content"].as_str()).collect();
    assert_eq!((reasoning.as_str(), text.as_str()), ("Need the weather.", "Checking 你好."));
    assert!(chunks.iter().any(|c| c["choices"][0]["finish_reason"] == "tool_calls"));
    assert_eq!(
        chunks.last().unwrap()["usage"],
        json!({"prompt_tokens": 120, "completion_tokens": 20, "total_tokens": 140})
    );

    let frames = run_encoder(&mut AnthropicStreamEncoder::new(), events.clone());
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(&names[names.len() - 2..], &["message_delta", "message_stop"]);
    assert_eq!(json_of(&frames[frames.len() - 2])["delta"]["stop_reason"], "tool_use");

    let frames = run_encoder(&mut ResponsesStreamEncoder::new(), events.clone());
    let last = json_of(frames.last().unwrap());
    assert_eq!(last["type"], "response.completed");
    let kinds: Vec<&str> =
        last["response"]["output"].as_array().unwrap().iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["reasoning", "message", "function_call"]);
    assert_eq!(last["response"]["usage"]["output_tokens"], 20);

    let frames = run_encoder(&mut GeminiStreamEncoder::new(), events);
    let parts: Vec<Value> = frames.iter().map(|f| json_of(f)["candidates"][0]["content"]["parts"][0].clone()).collect();
    assert_eq!(parts[0], json!({"text": "Need the ", "thought": true}));
    assert!(parts
        .iter()
        .any(|p| p["functionCall"]["name"] == "get_weather" && p["functionCall"]["args"] == json!({"city": "Oslo"})));
    let last = json_of(frames.last().unwrap());
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["usageMetadata"]["promptTokenCount"], 120);
}

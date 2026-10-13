//! Gemini / Vertex 协议测试：请求解码编码、响应、流式（SSE / JSON 数组 / 半包）、错误、schema 边界、跨协议转换。

use serde_json::{json, Value};

use super::anthropic::{self, AnthropicStreamEncoder};
use super::canonical::*;
use super::gemini::{self, Dialect, GeminiStreamDecoder, GeminiStreamEncoder};
use super::ollama::{self, OllamaStreamEncoder};
use super::openai_chat::{self, OpenAiStreamEncoder};
use super::openai_responses::{self, ResponsesStreamEncoder};
use super::sse::parse_sse;
use super::testkit::*;
use super::vertex;
use super::*;

macro_rules! fixture {
    ($name:literal) => {
        fx(include_str!(concat!("fixtures/", $name, ".json")))
    };
}

const GEMINI_STREAM: &str = include_str!("fixtures/gemini_stream.sse");

fn gm(v: &Value) -> CanonicalRequest {
    gemini::decode_request_for("gemini-2.5-pro", false, v).expect("gemini decode")
}
fn oa(v: &Value) -> CanonicalRequest {
    openai_chat::decode_request(v).expect("openai decode")
}
fn an(v: &Value) -> CanonicalRequest {
    anthropic::decode_request(v).expect("anthropic decode")
}

/// 与 Gemini 无关的字段（模型名来自 URL、user 不存在）对齐后再比较。
fn align(g: CanonicalRequest, other: CanonicalRequest) -> (CanonicalRequest, CanonicalRequest) {
    let g = semantic(g);
    let mut other = semantic(other);
    other.model = g.model.clone();
    other.user = None;
    (g, other)
}

fn user_text(text: &str) -> Value {
    json!({"role": "user", "parts": [{"text": text}]})
}

fn decode_events(bytes: &[u8], size: usize) -> Vec<StreamEvent> {
    let mut dec = GeminiStreamDecoder::new();
    let mut out = Vec::new();
    for chunk in bytes.chunks(size) {
        out.extend(dec.push(chunk).expect("push"));
    }
    out.extend(dec.finish().expect("finish"));
    out
}

fn flatten(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| if let ContentPart::Text { text } = p { Some(text.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
}

fn call(id: &str, name: &str, args: Value) -> ContentPart {
    ContentPart::ToolUse { id: id.into(), name: name.into(), input: args }
}
fn result(id: &str, text: &str) -> ContentPart {
    ContentPart::ToolResult { tool_use_id: id.into(), content: vec![ContentPart::text(text)], is_error: false }
}

// ───────────── 纯文本 / system / 生成参数 ─────────────

#[test]
fn text_request_decodes_to_same_canonical_as_openai() {
    let (g, o) = align(gm(&fixture!("gemini_text_request")), oa(&fixture!("openai_text_request")));
    assert_eq!(g, o);
    assert_eq!(g.system, vec!["You are helpful.", "Answer briefly."]);
    assert_eq!(g.params.stop, vec!["END"]);
    assert_eq!(g.params.max_tokens, Some(256));
}

#[test]
fn model_and_stream_come_from_the_url_not_the_body() {
    let body = fixture!("gemini_text_request");
    let req = gemini::decode_request_for("models/gemini-2.5-flash", true, &body).unwrap();
    assert_eq!(req.model, "gemini-2.5-flash");
    assert!(req.stream);
    let bare = gemini::decode_request(&body).unwrap();
    assert_eq!(bare.model, "");
    assert!(!bare.stream);
    assert_eq!(gemini::request_path("gemini-2.5-pro", false), "models/gemini-2.5-pro:generateContent");
    assert_eq!(gemini::request_path("gemini-2.5-pro", true), "models/gemini-2.5-pro:streamGenerateContent?alt=sse");
    assert_eq!(gemini::request_path("tunedModels/abc", false), "tunedModels/abc:generateContent");
}

#[test]
fn snake_case_keys_are_accepted() {
    let body = json!({
        "system_instruction": {"parts": [{"text": "sys"}]},
        "contents": [{"role": "user", "parts": [
            {"text": "look"},
            {"inline_data": {"mime_type": "image/png", "data": "AAAA"}}
        ]}],
        "generation_config": {"max_output_tokens": 64, "top_p": 0.5, "stop_sequences": ["x"]},
        "tool_config": {"function_calling_config": {"mode": "NONE"}},
        "tools": [{"function_declarations": [{"name": "f", "parameters": {"type": "OBJECT", "properties": {"a": {"type": "STRING"}}}}]}]
    });
    let req = gm(&body);
    assert_eq!(req.system, vec!["sys"]);
    assert_eq!(req.params.max_tokens, Some(64));
    assert_eq!(req.params.top_p, Some(0.5));
    assert_eq!(req.params.stop, vec!["x"]);
    assert_eq!(req.tool_choice, Some(ToolChoice::None));
    assert_eq!(req.tools[0].name, "f");
    assert!(
        matches!(&req.messages[0].content[1], ContentPart::Image { source: ImageSource::Base64 { media_type, .. }, .. } if media_type == "image/png")
    );
    assert!(req.warnings.is_empty(), "{:?}", req.warnings);
}

#[test]
fn generation_config_variants_and_unsupported_values() {
    let req = gm(&json!({
        "contents": [user_text("x")],
        "generationConfig": {
            "temperature": 0.7, "topK": 40, "presencePenalty": 0.5, "frequencyPenalty": 0.25, "seed": 42,
            "candidateCount": 1,
            "thinkingConfig": {"thinkingBudget": 2048, "includeThoughts": true},
            "responseMimeType": "application/json", "responseSchema": {"type": "OBJECT"}, "responseModalities": ["TEXT"]
        }
    }));
    assert_eq!(req.params.temperature, Some(0.7));
    assert_eq!(req.params.top_k, Some(40));
    assert_eq!(req.params.presence_penalty, Some(0.5));
    assert_eq!(req.params.frequency_penalty, Some(0.25));
    assert_eq!(req.params.seed, Some(42));
    assert_eq!(
        req.reasoning,
        Some(ReasoningConfig { effort: None, budget_tokens: Some(2048), include_thoughts: Some(true) })
    );
    for k in ["responseMimeType", "responseSchema", "responseModalities"] {
        assert!(has_warning(&req.warnings, &format!("generationConfig.{k}")), "{k}: {:?}", req.warnings);
    }

    let dynamic =
        gm(&json!({"contents": [user_text("x")], "generationConfig": {"thinkingConfig": {"thinkingBudget": -1}}}));
    assert_eq!(dynamic.reasoning.unwrap().budget_tokens, None);
    let level =
        gm(&json!({"contents": [user_text("x")], "generationConfig": {"thinkingConfig": {"thinkingLevel": "HIGH"}}}));
    assert_eq!(level.reasoning.unwrap().effort.as_deref(), Some("high"));

    let multi =
        gemini::decode_request(&json!({"contents": [user_text("x")], "generationConfig": {"candidateCount": 2}}));
    assert!(matches!(multi, Err(ProtocolError::Unsupported(_))));
}

#[test]
fn reasoning_and_generation_params_are_encoded() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![ContentPart::text("x")])],
        params: GenerationParams {
            max_tokens: Some(100),
            temperature: Some(0.1),
            top_p: Some(0.8),
            top_k: Some(5),
            stop: vec!["a".into(), "b".into()],
            presence_penalty: Some(0.3),
            frequency_penalty: Some(0.2),
            seed: Some(7),
        },
        reasoning: Some(ReasoningConfig {
            effort: Some("medium".into()),
            budget_tokens: None,
            include_thoughts: Some(true),
        }),
        user: Some("u".into()),
        system: vec!["s1".into(), "s2".into()],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    assert_eq!(
        enc.body["generationConfig"],
        json!({
            "maxOutputTokens": 100, "temperature": 0.1, "topP": 0.8, "topK": 5, "stopSequences": ["a", "b"],
            "presencePenalty": 0.3, "frequencyPenalty": 0.2, "seed": 7,
            "thinkingConfig": {"thinkingBudget": 8192, "includeThoughts": true}
        })
    );
    assert_eq!(enc.body["systemInstruction"], json!({"parts": [{"text": "s1"}, {"text": "s2"}]}));
    assert!(has_warning(&enc.warnings, "user identifier"));
    // 预算优先于档位
    let mut explicit = req.clone();
    explicit.reasoning =
        Some(ReasoningConfig { effort: Some("high".into()), budget_tokens: Some(777), include_thoughts: None });
    assert_eq!(
        gemini::encode_request(&explicit).unwrap().body["generationConfig"]["thinkingConfig"],
        json!({"thinkingBudget": 777})
    );
}

#[test]
fn invalid_requests_are_rejected() {
    for bad in [
        json!({}),
        json!({"contents": []}),
        json!({"contents": [{"role": "user"}]}),
        json!({"contents": [{"role": "robot", "parts": [{"text": "x"}]}]}),
        json!({"contents": [{"role": "user", "parts": [{"functionCall": {"args": {}}}]}]}),
        json!({"contents": [user_text("x")], "generationConfig": {"maxOutputTokens": -3}}),
        json!([1, 2]),
    ] {
        assert!(matches!(gemini::decode_request(&bad), Err(ProtocolError::InvalidRequest(_))), "{bad}");
    }
}

#[test]
fn unsupported_pieces_become_warnings_and_never_include_text() {
    let secret = "TOP-SECRET-PROMPT";
    let req = gm(&json!({
        "contents": [{"role": "user", "parts": [
            {"text": secret},
            {"executableCode": {"language": "PYTHON", "code": secret}},
            {"videoMetadata": {"startOffset": "1s"}, "fileData": {"fileUri": "gs://b/clip", "mimeType": "video/mp4"}}
        ]}],
        "systemInstruction": {"parts": [{"text": "sys"}, {"inlineData": {"mimeType": "image/png", "data": "AA"}}]},
        "tools": [
            {"googleSearch": {}},
            {"codeExecution": {}},
            {"functionDeclarations": [{"name": "f", "description": "d", "behavior": "NON_BLOCKING",
                "parameters": {"type": "OBJECT", "properties": {"a": {"type": "STRING"}}}}]}
        ],
        "cachedContent": "cachedContents/abc",
        "labels": {"team": "x"}
    }));
    assert_eq!(req.tools.len(), 1);
    assert!(has_warning(&req.warnings, "tools[0].googleSearch"));
    assert!(has_warning(&req.warnings, "tools[1].codeExecution"));
    assert!(has_warning(&req.warnings, "functionDeclarations[0].behavior"));
    assert!(has_warning(&req.warnings, "unsupported field ignored: cachedContent"));
    assert!(has_warning(&req.warnings, "unsupported field ignored: labels"));
    assert!(has_warning(&req.warnings, "unsupported part ignored (fields: executableCode)"));
    assert!(has_warning(&req.warnings, "unsupported part ignored (fields: videoMetadata)"));
    assert!(has_warning(&req.warnings, "systemInstruction: non-text part ignored"));
    assert!(has_warning(&req.warnings, "fileData.mimeType `video/mp4`"));
    assert!(req.warnings.iter().all(|w| !w.contains(secret)), "{:?}", req.warnings);
}

#[test]
fn safety_settings_are_reported_carried_and_only_resent_to_gemini() {
    let safety = json!([{"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "BLOCK_NONE"}]);
    let req = gm(&json!({"contents": [user_text("x")], "safetySettings": safety}));
    assert!(has_warning(&req.warnings, "safetySettings"));
    assert_eq!(req.gemini_safety_settings.as_ref(), Some(&safety));
    assert_eq!(gemini::encode_request(&req).unwrap().body["safetySettings"], safety);
    assert_eq!(vertex::encode_request(&req).unwrap().body["safetySettings"], safety);
    assert!(openai_chat::encode_request(&req).unwrap().body.get("safetySettings").is_none());
    assert!(anthropic::encode_request(&req).unwrap().body.get("safetySettings").is_none());
}

// ───────────── 工具调用与多轮工具链 ─────────────

#[test]
fn tool_chain_decodes_identically_to_openai() {
    let (g, o) = align(gm(&fixture!("gemini_tool_chain_request")), oa(&fixture!("openai_tool_chain_request")));
    assert_eq!(g, o);
    // functionResponse 位于 user 轮，canonical 里是 User 消息中的 ToolResult
    assert!(
        matches!(&g.messages[2].content[0], ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == "call_1")
    );
    assert_eq!(g.messages.len(), 5);
}

#[test]
fn tool_chain_encode_matches_the_fixture_and_roundtrips() {
    let canonical = oa(&fixture!("openai_tool_chain_request"));
    let enc = gemini::encode_request(&canonical).unwrap();
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
    assert_eq!(enc.body["contents"], fixture!("gemini_tool_chain_request")["contents"]);
    assert_eq!(enc.body["toolConfig"], json!({"functionCallingConfig": {"mode": "AUTO"}}));
    assert_eq!(enc.body["generationConfig"]["maxOutputTokens"], 512);
    assert_eq!(
        enc.body["tools"][0]["functionDeclarations"][0],
        json!({
            "name": "get_weather",
            "description": "Get current weather for a city",
            "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}
        })
    );
    let back = gemini::decode_request_for(&canonical.model, false, &enc.body).unwrap();
    assert_eq!(semantic(back), semantic(canonical));
}

#[test]
fn function_calls_without_ids_are_paired_by_name_in_order() {
    let req = gm(&json!({"contents": [
        user_text("go"),
        {"role": "model", "parts": [
            {"functionCall": {"name": "a", "args": {"n": 1}}},
            {"functionCall": {"name": "b", "args": {}}},
            {"functionCall": {"name": "a", "args": {"n": 2}}}
        ]},
        {"role": "user", "parts": [
            {"functionResponse": {"name": "a", "response": {"result": "A1"}}},
            {"functionResponse": {"name": "a", "response": {"result": "A2"}}},
            {"functionResponse": {"name": "b", "response": {"error": "B failed"}}}
        ]}
    ]}));
    let ids: Vec<&str> = req.messages[1]
        .content
        .iter()
        .map(|p| if let ContentPart::ToolUse { id, .. } = p { id.as_str() } else { panic!() })
        .collect();
    assert_eq!(ids, vec!["call_0", "call_1", "call_2"], "deterministic ids keep the history prefix stable");
    let results: Vec<(&str, String, bool)> = req.messages[2]
        .content
        .iter()
        .map(|p| match p {
            ContentPart::ToolResult { tool_use_id, content, is_error } => {
                (tool_use_id.as_str(), flatten(content), *is_error)
            }
            _ => panic!(),
        })
        .collect();
    assert_eq!(
        results,
        vec![("call_0", "A1".into(), false), ("call_2", "A2".into(), false), ("call_1", "B failed".into(), true)]
    );
    assert!(req.warnings.is_empty(), "{:?}", req.warnings);

    let orphan = gm(&json!({"contents": [user_text("x"),
        {"role": "user", "parts": [{"functionResponse": {"name": "ghost", "response": {"output": "?"}}}]}]}));
    assert!(has_warning(&orphan.warnings, "has no matching functionCall"));
}

#[test]
fn function_response_object_shapes() {
    let req = gm(&json!({"contents": [
        user_text("go"),
        {"role": "model", "parts": [{"functionCall": {"id": "c1", "name": "f", "args": {}}}, {"functionCall": {"id": "c2", "name": "f", "args": {}}},
                                     {"functionCall": {"id": "c3", "name": "f", "args": {}}}]},
        {"role": "user", "parts": [
            {"functionResponse": {"id": "c1", "name": "f", "response": {"output": "plain"}}},
            {"functionResponse": {"id": "c2", "name": "f", "response": {"temp": 18, "unit": "C"}}},
            {"functionResponse": {"id": "c3", "name": "f", "response": {"output": {"nested": [1, 2]}}}}
        ]}
    ]}));
    let texts: Vec<String> = req.messages[2]
        .content
        .iter()
        .map(|p| if let ContentPart::ToolResult { content, .. } = p { flatten(content) } else { panic!() })
        .collect();
    assert_eq!(texts, vec!["plain", "{\"temp\":18,\"unit\":\"C\"}", "{\"output\":{\"nested\":[1,2]}}"]);

    let mk = |text: &str, is_error: bool| {
        let r = CanonicalRequest {
            model: "m".into(),
            messages: vec![
                Message::user(vec![ContentPart::text("go")]),
                Message::assistant(vec![call("c", "f", json!({}))]),
                Message::user(vec![ContentPart::ToolResult {
                    tool_use_id: "c".into(),
                    content: vec![ContentPart::text(text)],
                    is_error,
                }]),
            ],
            ..Default::default()
        };
        gemini::encode_request(&r).unwrap().body["contents"][2]["parts"][0]["functionResponse"]["response"].clone()
    };
    assert_eq!(mk("plain", false), json!({"output": "plain"}));
    assert_eq!(mk("{\"a\":1}", false), json!({"a": 1}));
    assert_eq!(mk("[1,2]", false), json!({"output": "[1,2]"}));
    assert_eq!(mk("{broken", false), json!({"output": "{broken"}));
    assert_eq!(mk("boom", true), json!({"error": "boom"}));
}

#[test]
fn reused_tool_call_ids_are_made_unique_and_results_follow_call_order() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![call("c1", "f", json!({"a": 1}))]),
            Message::user(vec![result("c1", "r1")]),
            // OpenAI 的 id 只在单轮内唯一：后面一轮又用了 c1，并行两个调用，且结果顺序颠倒
            Message::assistant(vec![call("c1", "f", json!({"a": 2})), call("c9", "g", json!({}))]),
            Message::user(vec![result("c9", "rg"), result("c1", "r2")]),
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
    let c = &enc.body["contents"];
    assert_eq!(c[1]["parts"][0]["functionCall"]["id"], "c1");
    assert_eq!(c[3]["parts"][0]["functionCall"]["id"], "c1-2");
    assert_eq!(c[3]["parts"][1]["functionCall"]["id"], "c9");
    // 结果按调用顺序排列，并且 id / name 与各自的调用一致
    assert_eq!(c[4]["parts"][0]["functionResponse"], json!({"id": "c1-2", "name": "f", "response": {"output": "r2"}}));
    assert_eq!(c[4]["parts"][1]["functionResponse"], json!({"id": "c9", "name": "g", "response": {"output": "rg"}}));
    assert_eq!(c[2]["parts"][0]["functionResponse"]["id"], "c1");
}

#[test]
fn mismatched_call_and_response_counts_are_flagged() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![call("a", "f", json!({})), call("b", "f", json!({}))]),
            Message::user(vec![result("a", "only one")]),
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    assert!(has_warning(&enc.warnings, "2 functionCall part(s) but the following user turn has 1"));
    let orphan = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![result("ghost", "x")])],
        ..Default::default()
    };
    assert!(has_warning(&gemini::encode_request(&orphan).unwrap().warnings, "has no matching tool_use"));
}

#[test]
fn tool_names_are_sanitized_and_restored_via_the_map() {
    let t = |name: &str, schema: Value| ToolDefinition { name: name.into(), description: None, input_schema: schema };
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![call("c1", "mcp/server tool", json!({}))]),
            Message::user(vec![result("c1", "ok")]),
        ],
        tools: vec![
            t("mcp/server tool", json!({"type": "object", "properties": {"q": {"type": "string"}}})),
            t("9lives", json!({"type": "object", "properties": {}})),
            t("a b", json!({})),
            t("a_b", json!({})),
            t(&"x".repeat(80), json!({})),
        ],
        tool_choice: Some(ToolChoice::Tool { name: "a b".into() }),
        ..Default::default()
    };
    let (enc, map) = gemini::encode_request_with(&req, &Dialect::AI_STUDIO).unwrap();
    let names: Vec<&str> = enc.body["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names[0], "mcp_server_tool");
    assert_eq!(names[1], "_9lives");
    assert_eq!((names[2], names[3]), ("a_b", "a_b_2"));
    assert_eq!(names[4].len(), 64);
    assert_eq!(enc.body["contents"][1]["parts"][0]["functionCall"]["name"], "mcp_server_tool");
    assert_eq!(enc.body["contents"][2]["parts"][0]["functionResponse"]["name"], "mcp_server_tool");
    assert_eq!(
        enc.body["toolConfig"]["functionCallingConfig"],
        json!({"mode": "ANY", "allowedFunctionNames": ["a_b"]})
    );
    assert!(!map.is_identity());
    assert_eq!(map.original("mcp_server_tool"), "mcp/server tool");
    assert_eq!(map.original("a_b_2"), "a_b");
    assert_eq!(map.original("unknown"), "unknown");

    // 不带映射的入口会提醒调用方
    assert!(has_warning(&gemini::encode_request(&req).unwrap().warnings, "ToolNameMap"));

    // 响应与流都能还原原名
    let upstream = json!({"candidates": [{"content": {"role": "model", "parts": [
        {"functionCall": {"name": "mcp_server_tool", "args": {"q": "x"}}}]}, "finishReason": "STOP"}]});
    let resp = gemini::decode_response_mapped(&upstream, &map).unwrap();
    assert!(matches!(&resp.content[0], ContentPart::ToolUse { name, .. } if name == "mcp/server tool"));
    assert!(
        matches!(&gemini::decode_response(&upstream).unwrap().content[0], ContentPart::ToolUse { name, .. } if name == "mcp_server_tool")
    );
    let sse = format!("data: {upstream}\n\n");
    let mut dec = GeminiStreamDecoder::with_name_map(map);
    let mut events = dec.push(sse.as_bytes()).unwrap();
    events.extend(dec.finish().unwrap());
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ContentBlockStart { block: BlockStart::ToolUse { name, .. }, .. } if name == "mcp/server tool")));
}

#[test]
fn tool_choice_mapping_both_ways() {
    let mk = |cfg: Value| gm(&json!({"contents": [user_text("x")], "toolConfig": {"functionCallingConfig": cfg}}));
    assert_eq!(mk(json!({"mode": "AUTO"})).tool_choice, Some(ToolChoice::Auto));
    assert_eq!(mk(json!({"mode": "NONE"})).tool_choice, Some(ToolChoice::None));
    assert_eq!(mk(json!({"mode": "ANY"})).tool_choice, Some(ToolChoice::Any));
    assert_eq!(
        mk(json!({"mode": "ANY", "allowedFunctionNames": ["f"]})).tool_choice,
        Some(ToolChoice::Tool { name: "f".into() })
    );
    let several = mk(json!({"mode": "ANY", "allowedFunctionNames": ["f", "g"]}));
    assert_eq!(several.tool_choice, Some(ToolChoice::Any));
    assert!(has_warning(&several.warnings, "allowedFunctionNames"));
    assert!(has_warning(&mk(json!({"mode": "VALIDATED"})).warnings, "VALIDATED"));
    assert!(matches!(
        gemini::decode_request(
            &json!({"contents": [user_text("x")], "toolConfig": {"functionCallingConfig": {"mode": "WHATEVER"}}})
        ),
        Err(ProtocolError::InvalidRequest(_))
    ));

    let tool = ToolDefinition {
        name: "f".into(),
        description: None,
        input_schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
    };
    let enc = |choice| {
        let r = CanonicalRequest {
            model: "m".into(),
            messages: vec![Message::user(vec![ContentPart::text("x")])],
            tools: vec![tool.clone()],
            tool_choice: Some(choice),
            parallel_tool_calls: Some(false),
            ..Default::default()
        };
        gemini::encode_request(&r).unwrap()
    };
    assert_eq!(enc(ToolChoice::None).body["toolConfig"]["functionCallingConfig"]["mode"], "NONE");
    assert_eq!(enc(ToolChoice::Any).body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    assert_eq!(enc(ToolChoice::Auto).body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
    assert!(has_warning(&enc(ToolChoice::Auto).warnings, "parallel_tool_calls=false"));
}

// ───────────── 工具 schema 边界样本 ─────────────

fn check_schema(v: &Value, path: &str) {
    let o = v.as_object().unwrap_or_else(|| panic!("{path}: schema must be an object"));
    for k in [
        "additionalProperties",
        "$ref",
        "$defs",
        "definitions",
        "oneOf",
        "allOf",
        "not",
        "const",
        "propertyNames",
        "$schema",
        "default",
    ] {
        assert!(!o.contains_key(k), "{path}: forbidden keyword `{k}` survived");
    }
    if o.contains_key("anyOf") {
        assert_eq!(o.len(), 1, "{path}: anyOf must be the only field set: {o:?}");
    }
    if let Some(t) = o.get("type") {
        assert!(t.is_string(), "{path}: type must be a string, got {t}");
    }
    if let Some(e) = o.get("enum") {
        assert!(e.as_array().unwrap().iter().all(Value::is_string), "{path}: enum must contain strings only: {e}");
    }
    if let Some(Value::Object(props)) = o.get("properties") {
        for (k, c) in props {
            check_schema(c, &format!("{path}/properties/{k}"));
        }
    }
    if let Some(items) = o.get("items") {
        check_schema(items, &format!("{path}/items"));
    }
    if let Some(Value::Array(a)) = o.get("anyOf") {
        for (i, c) in a.iter().enumerate() {
            check_schema(c, &format!("{path}/anyOf/{i}"));
        }
    }
}

#[test]
fn schema_edge_samples_are_sanitized_and_reported_per_tool() {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "either": {"description": "text or number", "anyOf": [{"type": "string"}, {"type": "number"}]},
            "pick": {"oneOf": [{"type": "string"}, {"type": "integer"}]},
            "addr": {"$ref": "#/$defs/Address"},
            "maybe": {"type": ["string", "null"]},
            "many": {"type": ["string", "integer", "null"]},
            "level": {"type": "integer", "enum": [1, 2, 3]},
            "tags": {"type": "array", "items": {"type": ["string", "null"]}},
            "free": {"type": "object", "additionalProperties": {"type": "string"}},
            "fixed": {"const": "yes"},
            "tree": {"$ref": "#/$defs/Node"}
        },
        "required": ["either", "ghost"],
        "$defs": {
            "Address": {"type": "object", "properties": {"zip": {"type": "string"}}, "required": ["zip"], "additionalProperties": false},
            "Node": {"type": "object", "properties": {"next": {"$ref": "#/$defs/Node"}, "v": {"type": "string"}}}
        }
    });
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![ContentPart::text("x")])],
        tools: vec![
            ToolDefinition { name: "complex".into(), description: Some("d".into()), input_schema: schema },
            ToolDefinition {
                name: "plain".into(),
                description: None,
                input_schema: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            },
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    let decls = enc.body["tools"][0]["functionDeclarations"].as_array().unwrap();
    check_schema(&decls[0]["parameters"], "complex");
    check_schema(&decls[1]["parameters"], "plain");
    let p = &decls[0]["parameters"]["properties"];
    assert_eq!(p["maybe"], json!({"type": "string", "nullable": true}));
    assert_eq!(p["level"], json!({"type": "string", "enum": ["1", "2", "3"]}));
    assert_eq!(p["addr"]["properties"]["zip"]["type"], "string");
    assert_eq!(p["fixed"], json!({"type": "string", "enum": ["yes"]}));
    // 告警带着工具名传出，且只属于出问题的工具
    for needle in ["additionalProperties", "oneOf", "enum values were converted", "required entries", "recursive $ref"]
    {
        assert!(
            enc.warnings.iter().any(|w| w.starts_with("tool `complex` schema:") && w.contains(needle)),
            "missing `{needle}`: {:?}",
            enc.warnings
        );
    }
    assert!(enc.warnings.iter().all(|w| !w.contains("`plain`")));
}

#[test]
fn empty_and_invalid_parameter_schemas_omit_parameters() {
    let mk = |schema: Value| {
        let r = CanonicalRequest {
            model: "m".into(),
            messages: vec![Message::user(vec![ContentPart::text("x")])],
            tools: vec![ToolDefinition { name: "t".into(), description: None, input_schema: schema }],
            ..Default::default()
        };
        gemini::encode_request(&r).unwrap()
    };
    let empty = mk(json!({"type": "object", "properties": {}}));
    assert!(empty.body["tools"][0]["functionDeclarations"][0].get("parameters").is_none());
    assert_eq!(empty.body["tools"][0]["functionDeclarations"][0]["description"], "");
    let bad = mk(json!({"type": "string"}));
    assert!(has_warning(&bad.warnings, "tool `t` schema:"));
}

#[test]
fn gemini_openapi_schema_is_normalized_to_json_schema_on_decode() {
    let req = gm(&json!({"contents": [user_text("x")], "tools": [{"functionDeclarations": [{
        "name": "t",
        "parameters": {"type": "OBJECT", "properties": {
            "a": {"type": "STRING", "nullable": true},
            "b": {"type": "ARRAY", "items": {"type": "INTEGER"}, "minItems": "1", "maxItems": "5"},
            "c": {"type": "STRING", "enum": ["x", "y"]}
        }, "propertyOrdering": ["a", "b", "c"], "required": ["a"]}
    }]}]}));
    assert_eq!(
        req.tools[0].input_schema,
        json!({"type": "object", "properties": {
            "a": {"type": ["string", "null"]},
            "b": {"type": "array", "items": {"type": "integer"}, "minItems": 1, "maxItems": 5},
            "c": {"type": "string", "enum": ["x", "y"]}
        }, "required": ["a"]})
    );
    let json_schema = gm(&json!({"contents": [user_text("x")], "tools": [{"functionDeclarations": [{
        "name": "t", "parametersJsonSchema": {"type": "object", "additionalProperties": false, "properties": {"a": {"type": "string"}}}
    }]}]}));
    assert_eq!(json_schema.tools[0].input_schema["additionalProperties"], false);
}

// ───────────── 图片 ─────────────

#[test]
fn image_request_matches_openai_and_roundtrips() {
    let (g, o) = align(gm(&fixture!("gemini_image_request")), oa(&fixture!("openai_image_request")));
    assert_eq!(g, o);
    let enc = gemini::encode_request(&g).unwrap();
    assert!(enc.warnings.is_empty(), "{:?}", enc.warnings);
    assert_eq!(enc.body["contents"], fixture!("gemini_image_request")["contents"]);
    assert_eq!(semantic(gemini::decode_request_for(&g.model, false, &enc.body).unwrap()), g);
}

#[test]
fn large_base64_image_stays_out_of_text() {
    let big = "A".repeat(2 * 1024 * 1024);
    let req = gm(&json!({"contents": [{"role": "user", "parts": [
        {"text": "hi"}, {"inlineData": {"mimeType": "image/jpeg", "data": big}}]}]}));
    assert_eq!(req.text_bytes(), 2);
    assert_eq!(req.media_bytes(), big.len());
}

#[test]
fn image_sources_and_dialect_differences() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![Message::user(vec![
            ContentPart::Image {
                source: ImageSource::Url { url: "https://x/y.png?sig=1".into() },
                detail: Some("high".into()),
            },
            ContentPart::Image { source: ImageSource::Url { url: "https://x/download".into() }, detail: None },
            ContentPart::Image { source: ImageSource::FileId { file_id: "file-1".into() }, detail: None },
            ContentPart::Image {
                source: ImageSource::Base64 { media_type: "application/pdf".into(), data: "JVBER".into() },
                detail: None,
            },
        ])],
        ..Default::default()
    };
    let studio = gemini::encode_request(&req).unwrap();
    let parts = &studio.body["contents"][0]["parts"];
    assert_eq!(parts[0], json!({"fileData": {"fileUri": "https://x/y.png?sig=1", "mimeType": "image/png"}}));
    assert_eq!(parts[1], json!({"fileData": {"fileUri": "https://x/download"}}));
    assert_eq!(parts[2], json!({"inlineData": {"mimeType": "application/pdf", "data": "JVBER"}}));
    assert_eq!(parts.as_array().unwrap().len(), 3);
    assert!(has_warning(&studio.warnings, "file_id cannot be sent to gemini"));

    // Vertex：fileData.mimeType 必填
    let v = vertex::encode_request(&req).unwrap();
    assert_eq!(
        v.body["contents"][0]["parts"][1],
        json!({"fileData": {"fileUri": "https://x/download", "mimeType": "image/jpeg"}})
    );
    assert!(has_warning(&v.warnings, "fileData.mimeType is required"));
    assert!(has_warning(&v.warnings, "file_id cannot be sent to vertex"));

    // 非图片 inlineData 解码时提示它走了二进制容器
    let dec = gm(
        &json!({"contents": [{"role": "user", "parts": [{"inlineData": {"mimeType": "audio/wav", "data": "UklGRg"}}]}]}),
    );
    assert!(has_warning(&dec.warnings, "non-image inlineData (audio/wav)"));
    assert!(
        matches!(&dec.messages[0].content[0], ContentPart::Image { source: ImageSource::Base64 { media_type, .. }, .. } if media_type == "audio/wav")
    );
}

#[test]
fn images_inside_tool_results_move_next_to_function_responses() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("shot?")]),
            Message::assistant(vec![call("c1", "screenshot", json!({}))]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "c1".into(),
                content: vec![
                    ContentPart::text("done"),
                    ContentPart::Image {
                        source: ImageSource::Base64 { media_type: "image/png".into(), data: "AAAA".into() },
                        detail: None,
                    },
                ],
                is_error: false,
            }]),
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    let parts = enc.body["contents"][2]["parts"].as_array().unwrap();
    assert!(parts[0].get("functionResponse").is_some());
    assert_eq!(parts[1], json!({"inlineData": {"mimeType": "image/png", "data": "AAAA"}}));
    assert!(has_warning(&enc.warnings, "images inside tool_result"));
}

// ───────────── 思考签名 ─────────────

#[test]
fn thought_signatures_roundtrip_through_canonical() {
    let upstream = json!({"candidates": [{"content": {"role": "model", "parts": [
        {"text": "hmm", "thought": true},
        {"text": "Answer", "thoughtSignature": "S1"},
        {"functionCall": {"id": "c1", "name": "f", "args": {"a": 1}}, "thoughtSignature": "S2"}
    ]}, "finishReason": "STOP"}]});
    let resp = gemini::decode_response(&upstream).unwrap();
    assert_eq!(
        resp.content,
        vec![
            ContentPart::Thinking { text: "hmm".into(), signature: None },
            ContentPart::Thinking { text: String::new(), signature: Some("gemini:S1".into()) },
            ContentPart::text("Answer"),
            ContentPart::Thinking { text: String::new(), signature: Some("gemini:S2".into()) },
            call("c1", "f", json!({"a": 1})),
        ]
    );
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    // 编码回 Gemini：签名重新附着到对应的 part 上
    assert_eq!(
        gemini::encode_response(&resp)["candidates"][0]["content"]["parts"],
        upstream["candidates"][0]["content"]["parts"]
    );

    // 作为请求历史回传：签名附着到 functionCall / 文本，思考文本不回传
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(resp.content.clone()),
            Message::user(vec![result("c1", "ok")]),
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    let parts = &enc.body["contents"][1]["parts"];
    assert_eq!(parts[0], json!({"text": "Answer", "thoughtSignature": "S1"}));
    assert_eq!(parts[1]["thoughtSignature"], "S2");
    assert_eq!(parts.as_array().unwrap().len(), 2);
    assert!(has_warning(&enc.warnings, "thinking content / foreign thought signatures"));
}

#[test]
fn foreign_signatures_are_never_sent_to_gemini() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![
                ContentPart::Thinking { text: "claude thoughts".into(), signature: Some("EqQBCkYIARgCKkD".into()) },
                ContentPart::Thinking { text: String::new(), signature: Some("openai-responses:ENC".into()) },
                call("c1", "f", json!({})),
            ]),
            Message::user(vec![result("c1", "ok")]),
        ],
        ..Default::default()
    };
    let enc = gemini::encode_request(&req).unwrap();
    let part = &enc.body["contents"][1]["parts"][0];
    assert!(part.get("thoughtSignature").is_none(), "{part}");
    assert_eq!(enc.body["contents"][1]["parts"].as_array().unwrap().len(), 1);
    assert!(has_warning(&enc.warnings, "foreign thought signatures"));
}

#[test]
fn vertex_adds_a_placeholder_signature_only_where_missing() {
    let req = CanonicalRequest {
        model: "m".into(),
        messages: vec![
            Message::user(vec![ContentPart::text("go")]),
            Message::assistant(vec![call("a", "f", json!({})), call("b", "f", json!({}))]),
            Message::user(vec![result("a", "1"), result("b", "2")]),
            Message::assistant(vec![
                ContentPart::Thinking { text: String::new(), signature: Some("gemini:REAL".into()) },
                call("c", "f", json!({})),
            ]),
            Message::user(vec![result("c", "3")]),
        ],
        ..Default::default()
    };
    let v = vertex::encode_request(&req).unwrap();
    assert_eq!(v.body["contents"][1]["parts"][0]["thoughtSignature"], gemini::DUMMY_THOUGHT_SIGNATURE);
    assert!(v.body["contents"][1]["parts"][1].get("thoughtSignature").is_none());
    assert_eq!(v.body["contents"][3]["parts"][0]["thoughtSignature"], "REAL");
    let (without, _) = vertex::encode_request_mapped(&req, false).unwrap();
    assert!(without.body["contents"][1]["parts"][0].get("thoughtSignature").is_none());
    // AI Studio 默认不加占位签名
    assert!(gemini::encode_request(&req).unwrap().body["contents"][1]["parts"][0].get("thoughtSignature").is_none());
}

// ───────────── 响应 ─────────────

#[test]
fn tool_response_matches_openai_and_roundtrips() {
    let g = gemini::decode_response(&fixture!("gemini_tool_response")).unwrap();
    let o = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    assert_eq!(g, CanonicalResponse { created: None, ..o }, "only `created` is OpenAI-specific");
    assert_eq!(g.stop_reason, StopReason::ToolUse, "STOP + functionCall means tool_use");
    // 用量约定：promptTokenCount 含缓存；输出 = candidates + thoughts
    assert_eq!(
        g.usage,
        Usage {
            input_tokens: Some(120),
            output_tokens: Some(20),
            cache_read_tokens: Some(100),
            cache_creation_tokens: None
        }
    );

    let out = gemini::encode_response(&g);
    assert_eq!(out["candidates"][0]["finishReason"], "STOP");
    assert_eq!(out["candidates"][0]["content"]["role"], "model");
    assert_eq!(
        out["usageMetadata"],
        json!({"promptTokenCount": 120, "candidatesTokenCount": 20, "totalTokenCount": 140, "cachedContentTokenCount": 100})
    );
    assert_eq!(out["responseId"], "chatcmpl-abc");
    assert_eq!(gemini::decode_response(&out).unwrap(), g);
}

#[test]
fn usage_metadata_conventions() {
    let u = gemini::decode_usage(Some(&json!({
        "promptTokenCount": 50, "toolUsePromptTokenCount": 7, "cachedContentTokenCount": 20,
        "candidatesTokenCount": 11, "thoughtsTokenCount": 30, "totalTokenCount": 98
    })));
    assert_eq!(
        u,
        Usage {
            input_tokens: Some(57),
            output_tokens: Some(41),
            cache_read_tokens: Some(20),
            cache_creation_tokens: None
        }
    );
    assert_eq!(gemini::decode_usage(None), Usage::default());
    assert_eq!(gemini::decode_usage(Some(&json!({}))), Usage::default());
    let only_prompt = gemini::decode_usage(Some(&json!({"promptTokenCount": 3})));
    assert_eq!((only_prompt.input_tokens, only_prompt.output_tokens), (Some(3), None));
}

#[test]
fn finish_reason_mapping() {
    let decode = |reason: &str, with_call: bool| {
        let mut parts = vec![json!({"text": "x"})];
        if with_call {
            parts.push(json!({"functionCall": {"name": "f", "args": {}}}));
        }
        gemini::decode_response(&json!({"candidates": [{"content": {"parts": parts}, "finishReason": reason}]}))
            .unwrap()
            .stop_reason
    };
    assert_eq!(decode("STOP", false), StopReason::EndTurn);
    assert_eq!(decode("STOP", true), StopReason::ToolUse);
    assert_eq!(decode("MAX_TOKENS", false), StopReason::MaxTokens);
    for r in ["SAFETY", "RECITATION", "BLOCKLIST", "PROHIBITED_CONTENT", "SPII", "IMAGE_SAFETY"] {
        assert_eq!(decode(r, false), StopReason::ContentFilter, "{r}");
    }
    assert_eq!(decode("MALFORMED_FUNCTION_CALL", false), StopReason::Other("MALFORMED_FUNCTION_CALL".into()));

    let encode = |reason: StopReason| {
        let resp = CanonicalResponse {
            id: String::new(),
            model: "m".into(),
            created: None,
            content: vec![ContentPart::text("x")],
            stop_reason: reason,
            stop_sequence: None,
            usage: Usage::default(),
        };
        gemini::encode_response(&resp)["candidates"][0]["finishReason"].as_str().unwrap().to_string()
    };
    assert_eq!(encode(StopReason::EndTurn), "STOP");
    assert_eq!(encode(StopReason::StopSequence), "STOP");
    assert_eq!(encode(StopReason::ToolUse), "STOP");
    assert_eq!(encode(StopReason::MaxTokens), "MAX_TOKENS");
    assert_eq!(encode(StopReason::ContentFilter), "SAFETY");
    assert_eq!(encode(StopReason::Other("MALFORMED_FUNCTION_CALL".into())), "MALFORMED_FUNCTION_CALL");
    assert_eq!(encode(StopReason::Other("weird".into())), "OTHER");
}

#[test]
fn blocked_prompts_and_invalid_responses() {
    let blocked = gemini::decode_response(
        &json!({"promptFeedback": {"blockReason": "SAFETY"}, "usageMetadata": {"promptTokenCount": 9}}),
    )
    .unwrap();
    assert_eq!(blocked.stop_reason, StopReason::ContentFilter);
    assert!(blocked.content.is_empty());
    assert_eq!(blocked.usage.input_tokens, Some(9));
    // 空内容也要编码成合法的 Gemini 响应
    assert_eq!(gemini::encode_response(&blocked)["candidates"][0]["content"]["parts"], json!([{"text": ""}]));

    assert!(matches!(gemini::decode_response(&json!({})), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(gemini::decode_response(&json!({"candidates": []})), Err(ProtocolError::InvalidResponse(_))));
    assert!(matches!(gemini::decode_response(&json!([1])), Err(ProtocolError::InvalidResponse(_))));
    let err = gemini::decode_response(&json!({"error": {"code": 400, "message": "bad key"}})).unwrap_err();
    assert!(err.to_string().contains("bad key"), "{err}");
}

#[test]
fn response_without_call_ids_gets_unique_generated_ids() {
    let body = json!({"candidates": [{"content": {"parts": [
        {"functionCall": {"name": "f", "args": {}}}, {"functionCall": {"name": "f", "args": {}}}]}, "finishReason": "STOP"}]});
    let ids: Vec<String> = gemini::decode_response(&body)
        .unwrap()
        .content
        .into_iter()
        .filter_map(|p| if let ContentPart::ToolUse { id, .. } = p { Some(id) } else { None })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(ids.iter().all(|i| i.starts_with("call_") && i.len() == 29), "{ids:?}");
}

// ───────────── 错误体 ─────────────

#[test]
fn error_bodies() {
    let e = gemini::decode_error(
        429,
        &json!({"error": {"code": 429, "message": "quota exceeded", "status": "RESOURCE_EXHAUSTED", "details": [{"x": 1}]}}),
    );
    assert_eq!(
        (e.kind, e.message.as_str(), e.code.as_deref()),
        (ApiErrorKind::RateLimit, "quota exceeded", Some("RESOURCE_EXHAUSTED"))
    );
    // Vertex 把错误包成数组
    let wrapped = vertex::decode_error(
        403,
        &json!([{"error": {"code": 403, "message": "denied", "status": "PERMISSION_DENIED"}}]),
    );
    assert_eq!((wrapped.kind, wrapped.message.as_str()), (ApiErrorKind::PermissionDenied, "denied"));
    // 400 但 status 是 FAILED_PRECONDITION（地区不支持）仍是请求错误
    assert_eq!(
        gemini::decode_error(
            400,
            &json!({"error": {"message": "User location is not supported", "status": "FAILED_PRECONDITION"}})
        )
        .kind,
        ApiErrorKind::InvalidRequest
    );
    // 没有 status 时回退到 HTTP 状态码
    assert_eq!(gemini::decode_error(503, &json!({"error": {"message": "overloaded"}})).kind, ApiErrorKind::BadGateway);
    // 非 JSON 文本
    let text = gemini::decode_error(502, &Value::String("<html>bad gateway</html>".into()));
    assert_eq!((text.kind, text.message.as_str()), (ApiErrorKind::BadGateway, "<html>bad gateway</html>"));
    assert_eq!(gemini::decode_error(500, &Value::Null).message, "upstream returned HTTP 500");

    // 编码
    let api = ApiError::new(ApiErrorKind::Authentication, "bad key");
    assert_eq!(
        gemini::encode_error(&api),
        json!({"error": {"code": 401, "message": "bad key", "status": "UNAUTHENTICATED"}})
    );
    assert_eq!(vertex::encode_error(&api), gemini::encode_error(&api));
    // decode → encode 保持状态语义
    assert_eq!(gemini::encode_error(&e)["error"]["status"], "RESOURCE_EXHAUSTED");
}

// ───────────── 流式编码 ─────────────

fn thinking_text_tool_events() -> Vec<StreamEvent> {
    vec![
        StreamEvent::MessageStart {
            id: "msg_1".into(),
            model: "m".into(),
            usage: Some(Usage { input_tokens: Some(120), cache_read_tokens: Some(100), ..Default::default() }),
        },
        StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Thinking { text: "think".into() } },
        StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Signature { signature: "gemini:SIG".into() } },
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

fn run_encoder(enc: &mut dyn StreamEncoder, events: Vec<StreamEvent>) -> Vec<SseFrame> {
    events.into_iter().flat_map(|e| enc.push(e).expect("valid sequence")).collect()
}

#[test]
fn stream_encoder_emits_gemini_chunks() {
    let frames = run_encoder(&mut GeminiStreamEncoder::new(), thinking_text_tool_events());
    assert!(frames.iter().all(|f| f.event.is_none()), "Gemini SSE has no event names");
    assert!(frames.iter().all(|f| f.data != "[DONE]"), "Gemini streams end by closing the connection");
    let parts: Vec<Value> = frames.iter().map(|f| json_of(f)["candidates"][0]["content"]["parts"][0].clone()).collect();
    assert_eq!(parts[0], json!({"text": "think", "thought": true}));
    assert_eq!(parts[1], json!({"text": "Hel", "thoughtSignature": "SIG"}), "signature attaches to the next part");
    assert_eq!(parts[2], json!({"text": "lo"}));
    // 工具参数分片被缓冲，stop 时整块输出
    assert_eq!(parts[3], json!({"functionCall": {"id": "call_1", "name": "get_weather", "args": {"city": "Rome"}}}));
    assert_eq!(frames.len(), 5);
    let last = json_of(&frames[4]);
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(last["candidates"][0]["content"]["parts"], json!([{"text": ""}]));
    assert_eq!(
        last["usageMetadata"],
        json!({"promptTokenCount": 120, "candidatesTokenCount": 9, "totalTokenCount": 129, "cachedContentTokenCount": 100})
    );
    assert_eq!(last["modelVersion"], "m");
    assert_eq!(last["responseId"], "msg_1");
}

#[test]
fn stream_encoder_rejects_invalid_order_and_reports_errors() {
    let mut e = GeminiStreamEncoder::new();
    assert!(e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).is_err());
    let start = StreamEvent::MessageStart { id: "x".into(), model: "m".into(), usage: None };
    let mut e = GeminiStreamEncoder::new();
    e.push(start.clone()).unwrap();
    assert!(e.push(start.clone()).is_err(), "duplicate start");
    assert!(e.push(StreamEvent::ContentBlockDelta { index: 0, delta: BlockDelta::Text { text: "x".into() } }).is_err());
    assert!(e.push(StreamEvent::ContentBlockStop { index: 7 }).is_err());
    e.push(StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Text }).unwrap();
    assert!(
        e.push(StreamEvent::ToolCallDelta { index: 0, partial_json: "{}".into() }).is_err(),
        "tool delta on a text block"
    );
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

    // MessageStop 会冲刷尚未关闭的工具块，然后拒绝后续事件
    let mut e = GeminiStreamEncoder::new();
    let frames = run_encoder(
        &mut e,
        vec![
            start,
            StreamEvent::ContentBlockStart {
                index: 0,
                block: BlockStart::ToolUse { id: "c".into(), name: "f".into() },
            },
            StreamEvent::ToolCallDelta { index: 0, partial_json: "{\"a\":1}".into() },
            StreamEvent::MessageStop { stop_reason: StopReason::MaxTokens, stop_sequence: None },
        ],
    );
    assert_eq!(json_of(&frames[0])["candidates"][0]["content"]["parts"][0]["functionCall"]["args"], json!({"a": 1}));
    assert_eq!(json_of(&frames[1])["candidates"][0]["finishReason"], "MAX_TOKENS");
    assert!(e.push(StreamEvent::Usage(Usage::default())).is_err());

    // 错误帧
    let mut e = GeminiStreamEncoder::new();
    let frames = e.push(StreamEvent::Error { kind: ApiErrorKind::RateLimit, message: "slow down".into() }).unwrap();
    assert_eq!(json_of(&frames[0])["error"]["status"], "RESOURCE_EXHAUSTED");
    assert!(frames[0].event.is_none());
}

#[test]
fn stream_encoder_flushes_a_dangling_signature_on_the_final_chunk() {
    let frames = run_encoder(
        &mut GeminiStreamEncoder::new(),
        vec![
            StreamEvent::MessageStart { id: "x".into(), model: "m".into(), usage: None },
            StreamEvent::ContentBlockStart { index: 0, block: BlockStart::Thinking },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: BlockDelta::Signature { signature: "gemini:ONLY".into() },
            },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: BlockDelta::Signature { signature: "EqQBforeign".into() },
            },
            StreamEvent::ContentBlockStop { index: 0 },
            StreamEvent::MessageStop { stop_reason: StopReason::EndTurn, stop_sequence: None },
        ],
    );
    assert_eq!(frames.len(), 1);
    assert_eq!(
        json_of(&frames[0])["candidates"][0]["content"]["parts"],
        json!([{"text": "", "thoughtSignature": "ONLY"}])
    );
}

// ───────────── 流式解码 ─────────────

fn expected_fixture_stream() -> CanonicalResponse {
    CanonicalResponse {
        id: "resp-1".into(),
        model: "gemini-2.5-pro".into(),
        created: None,
        content: vec![
            ContentPart::Thinking { text: "Let me think about the weather. Two cities.".into(), signature: None },
            ContentPart::text("Checking 你好 now."),
            ContentPart::Thinking { text: String::new(), signature: Some("gemini:c2lnLTE=".into()) },
            call("<generated>", "get_weather", json!({"city": "Oslo"})),
            call("call_b", "get_weather", json!({"city": "Rome"})),
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
fn stream_decoder_handles_the_sse_fixture_at_every_split() {
    let bytes = GEMINI_STREAM.as_bytes();
    for size in [1, 2, 3, 5, 17, 64, 1000, bytes.len()] {
        let events = decode_events(bytes, size);
        assert_eq!(normalize_generated_ids(collect(&events)), expected_fixture_stream(), "chunk size {size}");
    }
    // 事件顺序：MessageStart 先，Usage 在 MessageStop 之前
    let events = decode_events(bytes, 64);
    assert!(matches!(events.first(), Some(StreamEvent::MessageStart { id, .. }) if id == "resp-1"));
    let n = events.len();
    assert!(matches!(events[n - 2], StreamEvent::Usage(_)));
    assert!(matches!(events[n - 1], StreamEvent::MessageStop { .. }));
}

#[test]
fn stream_decoder_handles_the_json_array_stream_form() {
    let datas: Vec<String> = parse_sse(GEMINI_STREAM).into_iter().map(|f| f.data).collect();
    let array = format!("[{}]", datas.join(",\n"));
    for size in [1, 4, 33, array.len()] {
        let events = decode_events(array.as_bytes(), size);
        assert_eq!(normalize_generated_ids(collect(&events)), expected_fixture_stream(), "chunk size {size}");
    }
    // 截断的数组是错误
    let truncated = &array.as_bytes()[..array.len() - 40];
    let mut dec = GeminiStreamDecoder::new();
    dec.push(truncated).unwrap();
    assert!(dec.finish().is_err());
}

#[test]
fn stream_decoder_reassembles_partial_function_call_args() {
    let chunks = [
        json!({"functionCall": {"name": "get_weather", "willContinue": true}}),
        json!({"functionCall": {"partialArgs": [{"jsonPath": "$.city", "stringValue": "Os", "willContinue": true}], "willContinue": true}}),
        json!({"functionCall": {"partialArgs": [
            {"jsonPath": "$.city", "stringValue": "lo"},
            {"jsonPath": "$.days", "numberValue": 3},
            {"jsonPath": "$.opts.verbose", "boolValue": true},
            {"jsonPath": "$.tags[1]", "stringValue": "x"}
        ], "willContinue": true}}),
        json!({"functionCall": {"willContinue": false}}),
    ];
    let mut sse = String::new();
    for (i, part) in chunks.iter().enumerate() {
        let mut cand = json!({"content": {"role": "model", "parts": [part]}, "index": 0});
        if i == chunks.len() - 1 {
            cand["finishReason"] = json!("STOP");
        }
        sse.push_str(&format!("data: {}\n\n", json!({"candidates": [cand], "modelVersion": "g", "responseId": "r"})));
    }
    let resp = collect(&decode_events(sse.as_bytes(), 9));
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.content.len(), 1);
    assert!(matches!(&resp.content[0], ContentPart::ToolUse { name, input, .. }
        if name == "get_weather" && *input == json!({"city": "Oslo", "days": 3, "opts": {"verbose": true}, "tags": [null, "x"]})));
}

#[test]
fn stream_decoder_error_paths() {
    // 中途上游报错：先关闭文本块，再给出 Error
    let sse = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"par\"}]}}]}\n\n\
               data: {\"error\":{\"code\":503,\"message\":\"overloaded\",\"status\":\"UNAVAILABLE\"}}\n\n";
    let mut dec = GeminiStreamDecoder::new();
    let mut events = dec.push(sse.as_bytes()).unwrap();
    events.extend(dec.finish().unwrap());
    assert!(matches!(events[events.len() - 2], StreamEvent::ContentBlockStop { .. }));
    assert!(
        matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::BadGateway, message }) if message == "overloaded")
    );

    // 没有 finishReason 的截断流
    let sse = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"par\"}]}}]}\n\n";
    let mut dec = GeminiStreamDecoder::new();
    let mut events = dec.push(sse.as_bytes()).unwrap();
    events.extend(dec.finish().unwrap());
    assert!(matches!(events.last(), Some(StreamEvent::Error { kind: ApiErrorKind::BadGateway, .. })));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ContentBlockStop { .. })));

    // 提示词被拦截：没有 candidates，也没有 finishReason
    let sse = "data: {\"promptFeedback\":{\"blockReason\":\"SAFETY\"},\"usageMetadata\":{\"promptTokenCount\":9}}\n\n";
    let resp = collect(&decode_events(sse.as_bytes(), 11));
    assert_eq!(resp.stop_reason, StopReason::ContentFilter);
    assert!(resp.content.is_empty());
    assert_eq!(resp.usage.input_tokens, Some(9));

    // 坏 JSON / 空流
    let mut dec = GeminiStreamDecoder::new();
    assert!(matches!(dec.push(b"data: {not json}\n\n"), Err(ProtocolError::InvalidResponse(_))));
    let mut dec = GeminiStreamDecoder::new();
    assert!(matches!(dec.finish(), Err(ProtocolError::InvalidResponse(_))));
    let mut dec = GeminiStreamDecoder::new();
    dec.push(b"  \n").unwrap();
    assert!(dec.finish().is_err());

    // 注释 / keepalive 与 [DONE] 被忽略
    let sse = format!(": keepalive\n\n{GEMINI_STREAM}data: [DONE]\n\n");
    assert_eq!(normalize_generated_ids(collect(&decode_events(sse.as_bytes(), 50))), expected_fixture_stream());
}

#[test]
fn stream_encode_then_decode_is_lossless() {
    let original = thinking_text_tool_events();
    let wire: String =
        run_encoder(&mut GeminiStreamEncoder::new(), original.clone()).iter().map(SseFrame::to_wire).collect();
    let expected = collect(&original);
    for size in [1, 7, wire.len()] {
        assert_eq!(collect(&decode_events(wire.as_bytes(), size)), expected, "chunk size {size}");
    }
}

// ───────────── 跨协议：Gemini 作为入口 ─────────────

#[test]
fn gemini_client_to_openai_upstream_and_back() {
    let canonical = gm(&fixture!("gemini_tool_chain_request"));
    let up = openai_chat::encode_request(&canonical).unwrap();
    assert_eq!(semantic(oa(&up.body)), semantic(canonical.clone()));
    let msgs = up.body["messages"].as_array().unwrap();
    assert_eq!(
        msgs.iter().map(|m| m["role"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["user", "assistant", "tool", "tool", "assistant", "tool"]
    );
    assert_eq!(msgs[1]["tool_calls"][1]["function"]["arguments"], "{\"city\":\"Tokyo\"}");
    assert_eq!(msgs[3]["content"], "25C rain");
    assert_eq!(up.body["tool_choice"], "auto");

    let resp = openai_chat::decode_response(&fixture!("openai_tool_response")).unwrap();
    let out = gemini::encode_response(&resp);
    let parts = &out["candidates"][0]["content"]["parts"];
    assert_eq!(parts[0], json!({"text": "Checking."}));
    assert_eq!(parts[1], json!({"functionCall": {"id": "call_9", "name": "get_weather", "args": {"city": "Oslo"}}}));
    assert_eq!(out["candidates"][0]["finishReason"], "STOP");
    assert_eq!(out["usageMetadata"]["promptTokenCount"], 120);
    assert_eq!(out["usageMetadata"]["cachedContentTokenCount"], 100);
    assert_eq!(out["usageMetadata"]["candidatesTokenCount"], 20);
}

#[test]
fn gemini_client_to_anthropic_upstream_and_back() {
    let canonical = gm(&fixture!("gemini_tool_chain_request"));
    let up = anthropic::encode_request(&canonical).unwrap();
    assert_eq!(up.body["max_tokens"], 512);
    let msgs = up.body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 5);
    assert_eq!(
        msgs[2]["content"][0],
        json!({"type": "tool_result", "tool_use_id": "call_1", "content": [{"type": "text", "text": "18C sunny"}]})
    );
    assert_eq!(up.body["tools"][0]["input_schema"]["properties"]["city"]["type"], "string");

    let resp = anthropic::decode_response(&fixture!("anthropic_tool_response")).unwrap();
    let out = gemini::encode_response(&resp);
    assert_eq!(out["candidates"][0]["content"]["parts"][1]["functionCall"]["name"], "get_weather");
    assert_eq!(out["usageMetadata"]["promptTokenCount"], 120, "Anthropic input 20 + cache 100");
    assert_eq!(out["usageMetadata"]["cachedContentTokenCount"], 100);
}

#[test]
fn openai_and_anthropic_clients_to_gemini_upstream() {
    let gemini_fixture = fixture!("gemini_tool_chain_request");
    let from_oa = gemini::encode_request(&oa(&fixture!("openai_tool_chain_request"))).unwrap();
    assert_eq!(from_oa.body["contents"], gemini_fixture["contents"]);
    let from_an = gemini::encode_request(&an(&fixture!("anthropic_tool_chain_request"))).unwrap();
    assert_eq!(from_an.body["contents"], gemini_fixture["contents"]);

    let upstream = fixture!("gemini_tool_response");
    let resp = gemini::decode_response(&upstream).unwrap();
    let chat = openai_chat::encode_response(&resp);
    assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"], "{\"city\":\"Oslo\"}");
    assert_eq!(
        chat["usage"],
        json!({"prompt_tokens": 120, "completion_tokens": 20, "total_tokens": 140, "prompt_tokens_details": {"cached_tokens": 100}})
    );
    let msg = anthropic::encode_response(&resp);
    assert_eq!(msg["stop_reason"], "tool_use");
    assert_eq!(msg["usage"]["input_tokens"], 20);
    assert_eq!(msg["usage"]["cache_read_input_tokens"], 100);
    assert_eq!(msg["usage"]["output_tokens"], 20);
    assert_eq!(msg["content"][1]["input"], json!({"city": "Oslo"}));
}

#[test]
fn gemini_stream_feeds_every_other_protocol_encoder() {
    let events = decode_events(GEMINI_STREAM.as_bytes(), 23);

    // OpenAI Chat
    let frames = run_encoder(&mut OpenAiStreamEncoder::new(true), events.clone());
    let chunks: Vec<Value> = frames.iter().filter(|f| f.data != "[DONE]").map(json_of).collect();
    assert_eq!(frames.last().unwrap().data, "[DONE]");
    let reasoning: String =
        chunks.iter().filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str()).collect();
    assert_eq!(reasoning, "Let me think about the weather. Two cities.");
    let text: String = chunks.iter().filter_map(|c| c["choices"][0]["delta"]["content"].as_str()).collect();
    assert_eq!(text, "Checking 你好 now.");
    let tool_args: Vec<&str> = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .filter(|a| !a.is_empty())
        .collect();
    assert_eq!(tool_args, vec!["{\"city\":\"Oslo\"}", "{\"city\":\"Rome\"}"]);
    assert!(chunks.iter().any(|c| c["choices"][0]["finish_reason"] == "tool_calls"));
    assert_eq!(
        chunks.last().unwrap()["usage"],
        json!({"prompt_tokens": 120, "completion_tokens": 20, "total_tokens": 140, "prompt_tokens_details": {"cached_tokens": 100}})
    );

    // Anthropic
    let frames = run_encoder(&mut AnthropicStreamEncoder::new(), events.clone());
    let names: Vec<&str> = frames.iter().map(|f| f.event.as_deref().unwrap()).collect();
    assert_eq!(names.first(), Some(&"message_start"));
    assert_eq!(&names[names.len() - 2..], &["message_delta", "message_stop"]);
    let delta = json_of(&frames[frames.len() - 2]);
    assert_eq!(delta["delta"]["stop_reason"], "tool_use");
    assert_eq!(delta["usage"]["output_tokens"], 20);
    let start = json_of(&frames[0]);
    assert_eq!(start["message"]["usage"]["input_tokens"], 20, "Anthropic input excludes the 100 cached tokens");
    assert_eq!(start["message"]["usage"]["cache_read_input_tokens"], 100);

    // Responses：只含签名的 Thinking 块不会产生空的 reasoning item
    let frames = run_encoder(&mut ResponsesStreamEncoder::new(), events.clone());
    let last = json_of(frames.last().unwrap());
    assert_eq!(last["type"], "response.completed");
    let kinds: Vec<&str> =
        last["response"]["output"].as_array().unwrap().iter().map(|i| i["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["reasoning", "message", "function_call", "function_call"]);
    assert_eq!(last["response"]["usage"]["input_tokens"], 120);
    assert_eq!(last["response"]["usage"]["input_tokens_details"]["cached_tokens"], 100);

    // Ollama
    let mut enc = OllamaStreamEncoder::new();
    let lines: Vec<Value> = events
        .iter()
        .cloned()
        .flat_map(|e| enc.push(e).unwrap())
        .map(|l| serde_json::from_str(l.trim()).unwrap())
        .collect();
    let done = lines.last().unwrap();
    assert_eq!((done["done"].as_bool(), done["done_reason"].as_str()), (Some(true), Some("stop")));
    assert_eq!((done["prompt_eval_count"].as_u64(), done["eval_count"].as_u64()), (Some(120), Some(20)));
    let calls: Vec<&Value> = lines.iter().filter(|l| l["message"].get("tool_calls").is_some()).collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["message"]["tool_calls"][0]["function"]["arguments"], json!({"city": "Oslo"}));
}

#[test]
fn gemini_client_to_responses_and_ollama_upstreams() {
    let canonical = gm(&fixture!("gemini_tool_chain_request"));
    let r = openai_responses::encode_request(&canonical).unwrap();
    let items = r.body["input"].as_array().unwrap();
    assert_eq!(
        items.iter().map(|i| i["type"].as_str().unwrap()).collect::<Vec<_>>(),
        vec![
            "message",
            "function_call",
            "function_call",
            "function_call_output",
            "function_call_output",
            "message",
            "function_call",
            "function_call_output"
        ]
    );
    assert_eq!(r.body["tools"][0]["strict"], false);
    assert_eq!(r.body["max_output_tokens"], 512);

    let o = ollama::encode_request(&canonical).unwrap();
    let msgs = o.body["messages"].as_array().unwrap();
    assert_eq!(msgs[2], json!({"role": "tool", "tool_name": "get_weather", "content": "18C sunny"}));
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], json!({"city": "Paris"}));
    assert_eq!(o.body["options"]["num_predict"], 512);
}

// ───────────── Vertex ─────────────

#[test]
fn vertex_drops_call_ids_and_keeps_pairing_by_order() {
    let canonical = oa(&fixture!("openai_tool_chain_request"));
    let v = vertex::encode_request(&canonical).unwrap();
    let c = &v.body["contents"];
    for turn in c.as_array().unwrap() {
        for p in turn["parts"].as_array().unwrap() {
            if let Some(fc) = p.get("functionCall") {
                assert!(fc.get("id").is_none(), "Vertex rejects functionCall.id: {p}");
            }
            if let Some(fr) = p.get("functionResponse") {
                assert!(fr.get("id").is_none(), "Vertex rejects functionResponse.id: {p}");
            }
        }
    }
    assert_eq!(
        c[2]["parts"][0]["functionResponse"],
        json!({"name": "get_weather", "response": {"output": "18C sunny"}})
    );
    // 同一 body 解码回 canonical：id 按顺序重新生成，语义（名称、参数）一致
    let back = gm(&v.body);
    let calls: Vec<(String, Value)> = back
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|p| {
            if let ContentPart::ToolUse { name, input, .. } = p {
                Some((name.clone(), input.clone()))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2], ("get_weather".to_string(), json!({"city": "Berlin"})));
}

#[test]
fn vertex_paths_and_hosts() {
    assert_eq!(vertex::base_url("us-central1"), "https://us-central1-aiplatform.googleapis.com");
    assert_eq!(vertex::base_url("global"), "https://aiplatform.googleapis.com");
    assert_eq!(vertex::base_url(""), "https://aiplatform.googleapis.com");
    assert_eq!(
        vertex::request_path("my-proj", "us-central1", "google", "gemini-2.5-pro", false),
        "v1/projects/my-proj/locations/us-central1/publishers/google/models/gemini-2.5-pro:generateContent"
    );
    assert_eq!(
        vertex::request_path("p", "global", "", "publishers/google/models/gemini-2.5-flash", true),
        "v1/projects/p/locations/global/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );
    assert_eq!(
        vertex::express_request_path("models/gemini-2.5-flash", false),
        "v1/publishers/google/models/gemini-2.5-flash:generateContent"
    );
}

#[test]
fn vertex_shares_the_body_format_with_gemini() {
    let canonical = gm(&fixture!("gemini_text_request"));
    let studio = gemini::encode_request(&canonical).unwrap();
    let v = vertex::encode_request(&canonical).unwrap();
    assert_eq!(studio.body, v.body, "plain text bodies are identical");
    assert_eq!(v.body["contents"][0]["role"], "user", "Vertex requires roles on every content");
    let resp = vertex::decode_response(&fixture!("gemini_tool_response")).unwrap();
    assert_eq!(resp.usage.input_tokens, Some(120));
    assert_eq!(vertex::encode_response(&resp), gemini::encode_response(&resp));
}

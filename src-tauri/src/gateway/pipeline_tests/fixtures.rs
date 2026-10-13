//! 新协议（Gemini / Ollama / Responses）的上游响应夹具与客户端响应解析小工具。

use serde_json::{json, Value};

use crate::protocols::sse::parse_sse;

// ───────────── 通用解析 ─────────────

/// NDJSON 文本 → 每行一个 JSON。
#[allow(dead_code)]
pub(super) fn parse_ndjson(text: &str) -> Vec<Value> {
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad NDJSON line {l:?}: {e}"))).collect()
}

/// SSE 文本 → `(event 名, data JSON)`；`[DONE]` 保留为 `Value::String`。
pub(super) fn sse_events(text: &str) -> Vec<(Option<String>, Value)> {
    parse_sse(text)
        .into_iter()
        .map(|f| {
            let v = if f.data.trim() == "[DONE]" { Value::String("[DONE]".into()) } else { serde_json::from_str(&f.data).unwrap_or_else(|e| panic!("bad SSE data {:?}: {e}", f.data)) };
            (f.event, v)
        })
        .collect()
}

/// SSE 文本 → 只要 data JSON（忽略 `[DONE]`）。
pub(super) fn sse_data_json(text: &str) -> Vec<Value> {
    sse_events(text).into_iter().map(|(_, v)| v).filter(|v| !v.is_string()).collect()
}

// ───────────── Gemini ─────────────

/// 一个文本候选 + 用量：promptTokenCount 21（含缓存 5），candidates 8 + thoughts 3 → output 11。
pub(super) fn gm_response(text: &str) -> Value {
    json!({
        "candidates": [{"content": {"role": "model", "parts": [{"text": text}]}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 21, "candidatesTokenCount": 8, "thoughtsTokenCount": 3, "cachedContentTokenCount": 5, "totalTokenCount": 32},
        "modelVersion": "gem-up-001",
        "responseId": "resp-gm-1"
    })
}

pub(super) fn gm_tool_response(name: &str, args: Value) -> Value {
    json!({
        "candidates": [{"content": {"role": "model", "parts": [{"functionCall": {"name": name, "args": args}}]}, "finishReason": "STOP", "index": 0}],
        "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 12, "totalTokenCount": 52},
        "modelVersion": "gem-up-001"
    })
}

pub(super) fn gm_chunk(parts: Value, finish: Option<&str>, usage: Option<Value>) -> Value {
    let mut cand = json!({"content": {"role": "model", "parts": parts}, "index": 0});
    if let Some(f) = finish {
        cand["finishReason"] = json!(f);
    }
    let mut v = json!({"candidates": [cand], "modelVersion": "gem-up-001", "responseId": "resp-gm-2"});
    if let Some(u) = usage {
        v["usageMetadata"] = u;
    }
    v
}

/// Google 的 SSE 帧以 `\r\n\r\n` 结尾。
pub(super) fn gm_sse(v: &Value) -> String {
    format!("data: {v}\r\n\r\n")
}

/// 文本流：两个文本增量 + 带 finishReason 与最终用量的末帧。
pub(super) fn gm_text_stream() -> Vec<Value> {
    vec![
        gm_chunk(json!([{"text": "Hel"}]), None, Some(json!({"promptTokenCount": 25, "cachedContentTokenCount": 10, "totalTokenCount": 25}))),
        gm_chunk(json!([{"text": "lo "}]), None, None),
        gm_chunk(
            json!([{"text": "world"}]),
            Some("STOP"),
            Some(json!({"promptTokenCount": 25, "candidatesTokenCount": 6, "thoughtsTokenCount": 2, "cachedContentTokenCount": 10, "totalTokenCount": 33})),
        ),
    ]
}

/// 工具调用流：文本 + functionCall（名字是 Gemini 侧的合法名）。
pub(super) fn gm_tool_stream(wire_name: &str, args: Value) -> Vec<Value> {
    vec![
        gm_chunk(json!([{"text": "Checking."}]), None, Some(json!({"promptTokenCount": 30, "totalTokenCount": 30}))),
        gm_chunk(
            json!([{"functionCall": {"name": wire_name, "args": args}}]),
            Some("STOP"),
            Some(json!({"promptTokenCount": 30, "candidatesTokenCount": 9, "totalTokenCount": 39})),
        ),
    ]
}

// ───────────── Ollama ─────────────

pub(super) fn ol_response(text: &str) -> Value {
    json!({
        "model": "llama-up", "created_at": "2025-01-01T00:00:00Z",
        "message": {"role": "assistant", "content": text},
        "done": true, "done_reason": "stop",
        "total_duration": 1, "prompt_eval_count": 21, "eval_count": 8
    })
}

#[allow(dead_code)]
pub(super) fn ol_tool_response(name: &str, args: Value) -> Value {
    json!({
        "model": "llama-up", "created_at": "2025-01-01T00:00:00Z",
        "message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": name, "arguments": args}}]},
        "done": true, "done_reason": "stop", "prompt_eval_count": 40, "eval_count": 12
    })
}

pub(super) fn ol_line(v: &Value) -> String {
    format!("{v}\n")
}

pub(super) fn ol_chunk(content: &str) -> Value {
    json!({"model": "llama-up", "created_at": "2025-01-01T00:00:00Z", "message": {"role": "assistant", "content": content}, "done": false})
}

pub(super) fn ol_done(prompt: u32, eval: u32) -> Value {
    json!({"model": "llama-up", "created_at": "2025-01-01T00:00:00Z", "message": {"role": "assistant", "content": ""},
           "done": true, "done_reason": "stop", "prompt_eval_count": prompt, "eval_count": eval})
}

/// 文本流：思考 + 两个文本增量 + 终止行（含用量）。
pub(super) fn ol_text_stream() -> Vec<Value> {
    vec![
        json!({"model": "llama-up", "created_at": "t", "message": {"role": "assistant", "content": "", "thinking": "Hmm. "}, "done": false}),
        ol_chunk("Hello"),
        ol_chunk(" world"),
        ol_done(42, 11),
    ]
}

/// 工具调用流（Ollama 的 tool call 整块到达）。
pub(super) fn ol_tool_stream(name: &str, args: Value) -> Vec<Value> {
    vec![
        ol_chunk("Checking."),
        json!({"model": "llama-up", "created_at": "t", "message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": name, "arguments": args}}]}, "done": false}),
        ol_done(50, 14),
    ]
}

// ───────────── OpenAI Responses ─────────────

/// 非流式 response 对象：input 21（含缓存 5），output 8（含推理 3）。
pub(super) fn rs_response(text: &str) -> Value {
    json!({
        "id": "resp_up_1", "object": "response", "created_at": 1_700_000_000, "status": "completed", "model": "gpt-up",
        "output": [{"type": "message", "id": "msg_up_1", "status": "completed", "role": "assistant",
                    "content": [{"type": "output_text", "text": text, "annotations": []}]}],
        "usage": {"input_tokens": 21, "input_tokens_details": {"cached_tokens": 5},
                  "output_tokens": 8, "output_tokens_details": {"reasoning_tokens": 3}, "total_tokens": 29}
    })
}

#[allow(dead_code)]
pub(super) fn rs_tool_response(call_id: &str, name: &str, args: &str) -> Value {
    json!({
        "id": "resp_up_2", "object": "response", "created_at": 1_700_000_000, "status": "completed", "model": "gpt-up",
        "output": [{"type": "function_call", "id": "fc_up_1", "call_id": call_id, "name": name, "arguments": args, "status": "completed"}],
        "usage": {"input_tokens": 40, "output_tokens": 12, "total_tokens": 52}
    })
}

pub(super) fn rs_event(v: &Value) -> String {
    format!("event: {}\ndata: {v}\n\n", v["type"].as_str().expect("event needs a type"))
}

fn rs_resp_shell(status: &str) -> Value {
    json!({"id": "resp_up_s", "object": "response", "created_at": 1_700_000_000, "status": status, "model": "gpt-up", "output": []})
}

/// 文本流：created → in_progress → message item → 两个 delta → done → completed（带用量）。
pub(super) fn rs_text_stream() -> Vec<String> {
    let done_item = json!({"type": "message", "id": "msg_s1", "status": "completed", "role": "assistant",
                           "content": [{"type": "output_text", "text": "Hello world", "annotations": []}]});
    let mut completed = rs_resp_shell("completed");
    completed["output"] = json!([done_item.clone()]);
    completed["usage"] = json!({"input_tokens": 42, "input_tokens_details": {"cached_tokens": 10}, "output_tokens": 11,
                                "output_tokens_details": {"reasoning_tokens": 4}, "total_tokens": 53});
    [
        json!({"type": "response.created", "sequence_number": 0, "response": rs_resp_shell("in_progress")}),
        json!({"type": "response.in_progress", "sequence_number": 1, "response": rs_resp_shell("in_progress")}),
        json!({"type": "response.output_item.added", "sequence_number": 2, "output_index": 0,
               "item": {"type": "message", "id": "msg_s1", "status": "in_progress", "role": "assistant", "content": []}}),
        json!({"type": "response.content_part.added", "sequence_number": 3, "output_index": 0, "content_index": 0, "item_id": "msg_s1",
               "part": {"type": "output_text", "text": "", "annotations": []}}),
        json!({"type": "response.output_text.delta", "sequence_number": 4, "output_index": 0, "content_index": 0, "item_id": "msg_s1", "delta": "Hello"}),
        json!({"type": "response.output_text.delta", "sequence_number": 5, "output_index": 0, "content_index": 0, "item_id": "msg_s1", "delta": " world"}),
        json!({"type": "response.output_text.done", "sequence_number": 6, "output_index": 0, "content_index": 0, "item_id": "msg_s1", "text": "Hello world"}),
        json!({"type": "response.content_part.done", "sequence_number": 7, "output_index": 0, "content_index": 0, "item_id": "msg_s1",
               "part": {"type": "output_text", "text": "Hello world", "annotations": []}}),
        json!({"type": "response.output_item.done", "sequence_number": 8, "output_index": 0, "item": done_item}),
        json!({"type": "response.completed", "sequence_number": 9, "response": completed}),
    ]
    .iter()
    .map(rs_event)
    .collect()
}

/// 函数调用流：function_call item + 两段参数增量 + completed。
pub(super) fn rs_tool_stream(call_id: &str, name: &str, arg_parts: [&str; 2]) -> Vec<String> {
    let args = format!("{}{}", arg_parts[0], arg_parts[1]);
    let done_item = json!({"type": "function_call", "id": "fc_s1", "call_id": call_id, "name": name, "arguments": args, "status": "completed"});
    let mut completed = rs_resp_shell("completed");
    completed["output"] = json!([done_item.clone()]);
    completed["usage"] = json!({"input_tokens": 60, "output_tokens": 15, "total_tokens": 75});
    [
        json!({"type": "response.created", "sequence_number": 0, "response": rs_resp_shell("in_progress")}),
        json!({"type": "response.output_item.added", "sequence_number": 1, "output_index": 0,
               "item": {"type": "function_call", "id": "fc_s1", "call_id": call_id, "name": name, "arguments": "", "status": "in_progress"}}),
        json!({"type": "response.function_call_arguments.delta", "sequence_number": 2, "output_index": 0, "item_id": "fc_s1", "delta": arg_parts[0]}),
        json!({"type": "response.function_call_arguments.delta", "sequence_number": 3, "output_index": 0, "item_id": "fc_s1", "delta": arg_parts[1]}),
        json!({"type": "response.function_call_arguments.done", "sequence_number": 4, "output_index": 0, "item_id": "fc_s1", "arguments": args}),
        json!({"type": "response.output_item.done", "sequence_number": 5, "output_index": 0, "item": done_item}),
        json!({"type": "response.completed", "sequence_number": 6, "response": completed}),
    ]
    .iter()
    .map(rs_event)
    .collect()
}

// ───────────── 常用客户端请求体 ─────────────

/// 1×1 PNG 的 base64（足够真实，且很短）。
pub(super) const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// 带工具定义的天气函数（OpenAI 形状）。
pub(super) fn chat_weather_tool() -> Value {
    json!({"type": "function", "function": {"name": "get_weather", "description": "Weather by city",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}})
}

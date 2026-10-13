//! Vertex AI 上的 Gemini 模型。请求 / 响应 / 流式的 body 格式与 [`super::gemini`] 相同，这里只放差异：
//!
//! | 方面 | AI Studio（`generativelanguage.googleapis.com`） | Vertex AI |
//! |---|---|---|
//! | 路径 | `v1beta/models/<m>:generateContent` | `v1/projects/<p>/locations/<l>/publishers/google/models/<m>:generateContent`（express 模式：`v1/publishers/google/models/<m>:...`） |
//! | 主机 | 固定 | `<location>-aiplatform.googleapis.com`，`global` 为 `aiplatform.googleapis.com` |
//! | `functionCall.id` / `functionResponse.id` | 支持 | **拒绝**（未知字段），编码时去掉，靠顺序配对 |
//! | `fileData.mimeType` | 可选 | 必填，无法从 URL 推断时默认 `image/jpeg` 并告警 |
//! | 文件引用 | Files API URI | `gs://` 或 https URL；canonical 的 `FileId` 无法表达，丢弃并告警 |
//! | 历史里缺签名的 functionCall | 不处理 | Gemini 3 要求签名，默认给首个 functionCall 补占位签名 |
//! | 错误体 | `{"error":{...}}` | 流式 / 部分接口会包成数组 `[{"error":{...}}]`，[`decode_error`] 已兼容 |
//! | 非 SSE 流 | JSON 数组 | JSON 数组，[`super::gemini::GeminiStreamDecoder`] 自动识别 |
//!
//! `systemInstruction` 在 Vertex 上与 AI Studio 一样是请求体顶层字段（其 `role` 被忽略，编码时省略）。
//! Cloud Code Assist / Gemini CLI 那种 `{project, request: {...}}` 信封不是 Vertex，不在此处理。
//! 鉴权（OAuth / ADC / express API key）由网关负责，不在协议层。

pub use super::gemini::{
    decode_error, decode_request, decode_request_for, decode_response, decode_response_mapped, decode_usage,
    encode_error, encode_response, GeminiStreamDecoder, GeminiStreamEncoder, ToolNameMap,
};
use super::gemini::{encode_request_with, Dialect};
use super::{CanonicalRequest, Encoded, ProtocolResult};

/// Vertex 方言。`dummy_thought_signature` 见 [`super::gemini::DUMMY_THOUGHT_SIGNATURE`]。
pub fn dialect(dummy_thought_signature: bool) -> Dialect {
    Dialect { name: "vertex", send_call_ids: false, require_file_mime: true, dummy_thought_signature }
}

/// canonical → Vertex 请求体（默认补占位签名）。
pub fn encode_request(req: &CanonicalRequest) -> ProtocolResult<Encoded> {
    let (mut enc, map) = encode_request_with(req, &dialect(true))?;
    if !map.is_identity() {
        enc.warnings
            .push("tool names were rewritten for Vertex; use encode_request_mapped to obtain the ToolNameMap".into());
    }
    Ok(enc)
}

/// 同 [`encode_request`]，并返回工具名映射（解码响应时传给 `decode_response_mapped` / `GeminiStreamDecoder::with_name_map`）。
pub fn encode_request_mapped(
    req: &CanonicalRequest,
    dummy_thought_signature: bool,
) -> ProtocolResult<(Encoded, ToolNameMap)> {
    encode_request_with(req, &dialect(dummy_thought_signature))
}

/// 区域对应的主机根地址。
pub fn base_url(location: &str) -> String {
    if location.is_empty() || location == "global" {
        "https://aiplatform.googleapis.com".to_string()
    } else {
        format!("https://{location}-aiplatform.googleapis.com")
    }
}

fn short_model(model: &str) -> &str {
    let m = model.rsplit_once("/models/").map(|(_, m)| m).unwrap_or(model);
    m.strip_prefix("models/").unwrap_or(m)
}

fn method(stream: bool) -> &'static str {
    if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    }
}

/// 标准模式路径：`v1/projects/<p>/locations/<l>/publishers/<publisher>/models/<m>:generateContent`。
pub fn request_path(project: &str, location: &str, publisher: &str, model: &str, stream: bool) -> String {
    let publisher = if publisher.is_empty() { "google" } else { publisher };
    format!(
        "v1/projects/{project}/locations/{location}/publishers/{publisher}/models/{}:{}",
        short_model(model),
        method(stream)
    )
}

/// Express 模式（API key）路径：`v1/publishers/google/models/<m>:generateContent`。
pub fn express_request_path(model: &str, stream: bool) -> String {
    format!("v1/publishers/google/models/{}:{}", short_model(model), method(stream))
}

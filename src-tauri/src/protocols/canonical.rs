//! 规范化模型。所有协议先解码到这里，再编码到目标协议。
//!
//! 约定：
//! - 角色只有 `User` / `Assistant`。系统提示词独立放在 `CanonicalRequest::system`；
//!   OpenAI 的 `role=tool` 消息表示为 `User` 消息里的 `ToolResult` 片段。
//! - 二进制 / 多模态数据只存在于 `ContentPart::Image`（base64 字符串不会进入 `Text`）。
//! - `Usage::input_tokens` 是**总**输入 token（已包含缓存读取与缓存写入）。
//!   Anthropic 的 `input_tokens` 不含缓存，解码时在 `anthropic.rs` 中显式相加，编码时显式相减；
//!   这是全库唯一一处缓存与输入 token 的换算，其余地方只做原样搬运。
//! - `Usage::output_tokens` 是**总**输出 token（已包含推理 / 思考 token）。Gemini 的 `candidatesTokenCount`
//!   不含 `thoughtsTokenCount`、Responses 的 `output_tokens` 含 `reasoning_tokens`：解码时统一折算为「含推理」。
//! - 跨厂商的思考签名通过 [`signature`] 模块加协议前缀（如 `gemini:`），编码器只接受自己协议前缀的签名，
//!   其它来源的签名一律丢弃（并告警），避免把 A 厂商的签名发给 B 厂商导致 400。
//!   「仅含签名、文本为空」的 `Thinking` 片段表示该签名附着在紧随其后的 Text / ToolUse 上（Gemini `thoughtSignature`）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
    FileId { file_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        /// OpenAI 的 `detail`（auto/low/high）；其它协议为 None。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// 仅允许 `Text` 与 `Image`。
        content: Vec<ContentPart>,
        #[serde(default)]
        is_error: bool,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }

    /// 文本类字节数（文本、工具入参、工具结果里的文本、思考内容），不含图片。
    pub fn text_bytes(&self) -> usize {
        match self {
            ContentPart::Text { text } => text.len(),
            ContentPart::Thinking { text, .. } => text.len(),
            ContentPart::ToolUse { input, name, .. } => name.len() + input.to_string().len(),
            ContentPart::ToolResult { content, .. } => content.iter().map(ContentPart::text_bytes).sum(),
            ContentPart::Image { .. } => 0,
        }
    }

    /// 二进制 / 多模态负载字节数（base64 字符串长度或 URL 长度）。
    pub fn media_bytes(&self) -> usize {
        match self {
            ContentPart::Image { source, .. } => match source {
                ImageSource::Base64 { data, .. } => data.len(),
                ImageSource::Url { url } => url.len(),
                ImageSource::FileId { file_id } => file_id.len(),
            },
            ContentPart::ToolResult { content, .. } => content.iter().map(ContentPart::media_bytes).sum(),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

impl Message {
    pub fn user(content: Vec<ContentPart>) -> Self {
        Self { role: Role::User, content }
    }
    pub fn assistant(content: Vec<ContentPart>) -> Self {
        Self { role: Role::Assistant, content }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema（原样保存；面向具体上游时再用 `schema` 模块清理）。
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    /// 禁止调用工具。
    None,
    /// 必须调用某个工具（OpenAI `required` / Anthropic `any`）。
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationParams {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub stop: Vec<String>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CanonicalRequest {
    pub model: String,
    pub system: Vec<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: Option<ToolChoice>,
    pub parallel_tool_calls: Option<bool>,
    pub params: GenerationParams,
    pub stream: bool,
    /// OpenAI `stream_options.include_usage`，影响向客户端编码流式响应时是否输出 usage chunk。
    pub include_usage: bool,
    /// 最终用户标识（OpenAI `user` / Anthropic `metadata.user_id`）。
    pub user: Option<String>,
    /// 推理 / 思考配置（Responses `reasoning`、Gemini `thinkingConfig`、Ollama `think`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    /// Gemini `safetySettings` 原样保存，仅 Gemini / Vertex 编码器会下发；其它协议的编码器不识别该字段。
    /// 解码时同时会产生 warning，提示调用方它不会跨协议生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_safety_settings: Option<Value>,
    /// 解码时发现但无法表达的字段 / 被忽略的提示。**不含**任何请求正文内容。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// 推理 / 思考请求。三个字段互相独立，各协议按自己能表达的部分映射。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReasoningConfig {
    /// `minimal` / `low` / `medium` / `high` 等档位（原样字符串）。
    pub effort: Option<String>,
    /// 思考 token 预算（Gemini `thinkingBudget`）。`-1` 不会出现在这里：动态预算表示为 `None`。
    pub budget_tokens: Option<u32>,
    /// 是否希望上游返回思考摘要 / 内容（Responses `reasoning.summary`、Gemini `includeThoughts`、Ollama `think`）。
    pub include_thoughts: Option<bool>,
}

/// 思考签名的协议命名空间。
pub mod signature {
    pub const GEMINI: &str = "gemini";
    pub const OPENAI_RESPONSES: &str = "openai-responses";
    const KNOWN: &[&str] = &[GEMINI, OPENAI_RESPONSES];

    /// `provider` + 原始签名 → 带前缀的签名（`gemini:<raw>`）。
    pub fn tag(provider: &str, raw: &str) -> String {
        format!("{provider}:{raw}")
    }

    /// 取出属于 `provider` 的原始签名；前缀不匹配（含无前缀的 Anthropic 签名）返回 `None`。
    pub fn untag<'a>(provider: &str, sig: &'a str) -> Option<&'a str> {
        sig.strip_prefix(provider).and_then(|r| r.strip_prefix(':')).filter(|r| !r.is_empty())
    }

    /// 带有本库已知的「外来」协议前缀（Anthropic 编码器可据此丢弃）。
    pub fn is_foreign(sig: &str) -> bool {
        KNOWN.iter().any(|p| untag(p, sig).is_some())
    }
}

impl CanonicalRequest {
    pub fn text_bytes(&self) -> usize {
        self.system.iter().map(String::len).sum::<usize>()
            + self.messages.iter().flat_map(|m| &m.content).map(ContentPart::text_bytes).sum::<usize>()
    }

    pub fn media_bytes(&self) -> usize {
        self.messages.iter().flat_map(|m| &m.content).map(ContentPart::media_bytes).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    ContentFilter,
    Other(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// 总输入 token（含缓存）。None 表示上游未提供。
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// 命中缓存的输入 token（`input_tokens` 的子集）。
    pub cache_read_tokens: Option<u32>,
    /// 写入缓存的输入 token（`input_tokens` 的子集，Anthropic 独有）。
    pub cache_creation_tokens: Option<u32>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_creation_tokens.is_none()
    }

    /// 用 `other` 中已提供的字段覆盖自身（流式中后到的 usage 为累计值）。
    pub fn merge(&mut self, other: &Usage) {
        if other.input_tokens.is_some() {
            self.input_tokens = other.input_tokens;
        }
        if other.output_tokens.is_some() {
            self.output_tokens = other.output_tokens;
        }
        if other.cache_read_tokens.is_some() {
            self.cache_read_tokens = other.cache_read_tokens;
        }
        if other.cache_creation_tokens.is_some() {
            self.cache_creation_tokens = other.cache_creation_tokens;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalResponse {
    pub id: String,
    pub model: String,
    /// Unix 秒；None 时 OpenAI 编码取当前时间。
    pub created: Option<i64>,
    /// 助手内容：Text / ToolUse / Thinking。
    pub content: Vec<ContentPart>,
    pub stop_reason: StopReason,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}

/// 流式内容块起始类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockStart {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
}

/// 文本 / 思考类增量（工具入参增量使用 `StreamEvent::ToolCallDelta`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockDelta {
    Text { text: String },
    Thinking { text: String },
    Signature { signature: String },
}

/// 流式事件。顺序约定：
/// `MessageStart` → (`ContentBlockStart` → `ContentBlockDelta`/`ToolCallDelta`* → `ContentBlockStop`)* →
/// `Usage`* → `MessageStop`；任意时刻可出现 `Error` 并终止流。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        id: String,
        model: String,
        /// 通常只含输入侧 usage。
        usage: Option<Usage>,
    },
    ContentBlockStart {
        index: u32,
        block: BlockStart,
    },
    ContentBlockDelta {
        index: u32,
        delta: BlockDelta,
    },
    /// 工具调用入参的 JSON 片段（Anthropic `input_json_delta` / OpenAI `tool_calls[].function.arguments`）。
    ToolCallDelta {
        index: u32,
        partial_json: String,
    },
    ContentBlockStop {
        index: u32,
    },
    /// 累计 usage（后到的覆盖先到的）。必须在 `MessageStop` 之前。
    Usage(Usage),
    MessageStop {
        stop_reason: StopReason,
        stop_sequence: Option<String>,
    },
    Error {
        kind: super::ApiErrorKind,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_separate_from_text() {
        let big = "A".repeat(2 * 1024 * 1024);
        let req = CanonicalRequest {
            messages: vec![Message::user(vec![
                ContentPart::text("hi"),
                ContentPart::Image {
                    source: ImageSource::Base64 { media_type: "image/png".into(), data: big.clone() },
                    detail: None,
                },
            ])],
            ..Default::default()
        };
        assert_eq!(req.text_bytes(), 2);
        assert_eq!(req.media_bytes(), big.len());
    }

    #[test]
    fn signature_tags_are_namespaced() {
        let t = signature::tag(signature::GEMINI, "abc");
        assert_eq!(t, "gemini:abc");
        assert_eq!(signature::untag(signature::GEMINI, &t), Some("abc"));
        assert_eq!(signature::untag(signature::OPENAI_RESPONSES, &t), None);
        assert_eq!(signature::untag(signature::GEMINI, "EqQBCkYI"), None);
        assert!(signature::is_foreign(&t));
        assert!(!signature::is_foreign("EqQBCkYI"));
    }

    #[test]
    fn usage_merge_overrides_present_fields_only() {
        let mut u = Usage { input_tokens: Some(10), output_tokens: Some(1), ..Default::default() };
        u.merge(&Usage { output_tokens: Some(7), ..Default::default() });
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(7));
    }
}

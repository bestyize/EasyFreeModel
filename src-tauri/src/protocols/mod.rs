//! 协议层：各 API 协议与「规范化模型（canonical）」之间的解码 / 编码。归属：gateway agent。
//!
//! 设计约束（PRODUCT_DESIGN §5.2、§10）：
//! - 先解析成 [`canonical`] 结构，再编码成目标协议；**禁止**对原始 JSON 做字段替换式透传。
//! - 每个协议（OpenAI Chat、Anthropic Messages、Responses、Gemini）各自独立实现与测试。
//! - 无法表达的字段不会被静默丢弃：解码时进入 `CanonicalRequest::warnings`，
//!   编码时进入 [`Encoded::warnings`]，由调用方决定记录或拒绝。
//! - 图片等二进制 / 多模态数据位于 `ContentPart::Image`，不会混入文本字段。
//!
//! 已实现：`openai_chat`、`anthropic`、`openai_responses`、`gemini`（含 `vertex` 的差异部分）、`ollama`，
//! 每个协议都有请求/响应编解码与流式编解码；`schema` 负责工具 schema 清理，`wire` 是增量 SSE / NDJSON 解析器。
//! 未实现：CommandCode 的私有 NDJSON 格式。

pub mod anthropic;
pub mod canonical;
mod common;
pub mod errors;
pub mod gemini;
pub mod openai_chat;
pub mod ollama;
pub mod openai_responses;
pub mod schema;
pub mod sse;
pub mod vertex;
pub mod wire;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests_gemini;
#[cfg(test)]
mod tests_ollama;
#[cfg(test)]
mod tests_responses;

use serde_json::Value;

pub use canonical::{
    CanonicalRequest, CanonicalResponse, ContentPart, GenerationParams, ImageSource, Message, Role, StopReason,
    StreamEvent, ToolChoice, ToolDefinition, Usage,
};
pub use errors::{ApiError, ApiErrorKind};
pub use sse::SseFrame;

/// 对外协议。`id()` 与前端 `Protocol` 类型及 `VerifiedKey::allowed_protocols` 的取值一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    Gemini,
    /// Ollama `/api/chat`（NDJSON 流）。既是入口协议也是上游协议。
    Ollama,
    /// Vertex AI 上的 Gemini（express 模式）。**只作上游协议**：body 与 Gemini 相同，但方言不同
    /// （不带 functionCall id、fileData.mimeType 必填……），因此与 `Gemini` 不能互相透传。
    Vertex,
}

impl ProtocolKind {
    /// 前端 `Protocol` 类型与 API 密钥「允许协议」可选的四种协议。`Ollama` 入口与上游专用的 `Vertex`
    /// 不在其中（见 [`ProtocolKind::from_id`]）。
    pub const ALL: [ProtocolKind; 4] = [
        ProtocolKind::OpenAiChat,
        ProtocolKind::OpenAiResponses,
        ProtocolKind::AnthropicMessages,
        ProtocolKind::Gemini,
    ];

    pub fn id(self) -> &'static str {
        match self {
            ProtocolKind::OpenAiChat => "openai-chat",
            ProtocolKind::OpenAiResponses => "openai-responses",
            ProtocolKind::AnthropicMessages => "anthropic-messages",
            ProtocolKind::Gemini => "gemini",
            ProtocolKind::Ollama => "ollama",
            ProtocolKind::Vertex => "vertex",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().chain([ProtocolKind::Ollama, ProtocolKind::Vertex]).find(|p| p.id() == id)
    }

    /// 面向用户的显示名，用于错误信息。
    pub fn display_name(self) -> &'static str {
        match self {
            ProtocolKind::OpenAiChat => "OpenAI Chat Completions",
            ProtocolKind::OpenAiResponses => "OpenAI Responses",
            ProtocolKind::AnthropicMessages => "Anthropic Messages",
            ProtocolKind::Gemini => "Gemini",
            ProtocolKind::Ollama => "Ollama",
            ProtocolKind::Vertex => "Vertex AI",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ProtocolError {
    /// 客户端请求不合法（缺字段、类型错误等）。
    #[error("{0}")]
    InvalidRequest(String),
    /// 上游响应不合法或无法解析。
    #[error("{0}")]
    InvalidResponse(String),
    /// 协议能力尚未实现或无法在规范化模型中表达。
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl ProtocolError {
    pub fn to_api_error(&self) -> ApiError {
        match self {
            ProtocolError::InvalidRequest(m) => ApiError::new(ApiErrorKind::InvalidRequest, m.clone()),
            ProtocolError::InvalidResponse(m) => ApiError::new(ApiErrorKind::BadGateway, m.clone()),
            ProtocolError::Unsupported(m) => ApiError::new(ApiErrorKind::InvalidRequest, format!("unsupported: {m}")),
        }
    }
}

pub type ProtocolResult<T> = Result<T, ProtocolError>;

/// 编码结果：目标协议 JSON + 无法无损表达而被丢弃 / 改写的字段说明。
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub body: Value,
    pub warnings: Vec<String>,
}

/// 流式编码器：把规范化流事件逐个转成目标协议的 SSE 帧。
pub trait StreamEncoder {
    /// 事件顺序不合法时返回 `InvalidResponse`（例如在 `content_block_start` 之前出现 delta）。
    fn push(&mut self, event: StreamEvent) -> ProtocolResult<Vec<SseFrame>>;
}

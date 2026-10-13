//! 适配器 trait 与各适配器共用的类型。
//!
//! 适配器按「格式标识」（如 `openai-images`、`elevenlabs-tts`）归一，而不是每家供应商一套：
//! 同一格式的多家供应商共用同一个适配器，差异（baseUrl、鉴权方式、默认模型、可选参数）由
//! `resources/media_providers.json` 数据驱动。

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use async_trait::async_trait;
use reqwest::{Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::http::MediaHttp;
use super::registry::{Capability, CapabilityConfig};

/// API Key 包装：没有 Display/Serialize，Debug 恒为打码，避免意外写进日志或错误信息。
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// 适配器错误。`Display` 文本会写进 MediaJob.error，因此不得包含密钥/请求正文
/// （任务执行层还会再做一次脱敏兜底）。
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum MediaError {
    #[error("未配置 API Key")]
    NoCredential,
    #[error("{0}")]
    InvalidParams(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("上游返回 HTTP {status}：{message}")]
    Upstream { status: u16, message: String },
    #[error("网络错误：{0}")]
    Network(String),
    #[error("请求超时")]
    Timeout,
    /// 上游任务自己报告失败（含内容审核拒绝）。
    #[error("{0}")]
    Failed(String),
    #[error("上游响应无法解析：{0}")]
    BadResponse(String),
    /// URL 被安全策略拦截（非 https、回环/内网地址、不受信任的域名等）。
    #[error("已拦截不安全的地址：{0}")]
    Blocked(String),
    #[error("超过大小上限（{0}）")]
    TooLarge(String),
    #[error("文件处理失败：{0}")]
    Io(String),
    #[error("任务已取消")]
    Cancelled,
}

impl MediaError {
    /// 换一个账户重试可能成功：鉴权失败或被限流。
    pub fn is_failover(&self) -> bool {
        matches!(self, MediaError::Upstream { status: 401 | 403 | 429, .. })
    }

    /// 轮询阶段可容忍的瞬时错误（网络抖动、5xx、限流）。
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            MediaError::Network(_) | MediaError::Timeout | MediaError::Upstream { status: 429 | 500 | 502 | 503 | 504, .. }
        )
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::Io(e.kind().to_string())
    }
}

/// 已落盘的上传文件（路径由应用生成，文件名不来自用户）。
#[derive(Debug, Clone)]
pub struct UploadedFile {
    pub path: PathBuf,
    pub mime: String,
    pub ext: String,
    pub size: u64,
}

impl UploadedFile {
    pub async fn read(&self) -> Result<Vec<u8>, MediaError> {
        Ok(tokio::fs::read(&self.path).await?)
    }

    /// `data:<mime>;base64,...`（Runway、OpenRouter 等以 data URI 接收图片）。
    pub async fn data_uri(&self) -> Result<String, MediaError> {
        use base64::Engine;
        let bytes = self.read().await?;
        Ok(format!("data:{};base64,{}", self.mime, base64::engine::general_purpose::STANDARD.encode(bytes)))
    }

    /// 发给上游的 multipart 文件名：固定前缀 + 白名单扩展名，不泄露本地原文件名。
    pub fn upload_name(&self, stem: &str) -> String {
        format!("{stem}.{}", self.ext)
    }
}

/// 一次媒体请求。`params` 已按能力表白名单过滤，只含该上游支持的字段。
#[derive(Debug, Clone)]
pub struct MediaRequest {
    pub job_id: String,
    pub provider_id: String,
    pub model: String,
    pub params: Map<String, Value>,
    pub files: BTreeMap<String, UploadedFile>,
}

impl MediaRequest {
    pub fn str_param(&self, key: &str) -> Option<&str> {
        self.params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    }

    pub fn f64_param(&self, key: &str) -> Option<f64> {
        match self.params.get(key)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn u64_param(&self, key: &str) -> Option<u64> {
        self.f64_param(key).filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64)
    }

    pub fn bool_param(&self, key: &str) -> bool {
        match self.params.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
            _ => false,
        }
    }

    pub fn require_str(&self, key: &str, label: &str) -> Result<&str, MediaError> {
        self.str_param(key).ok_or_else(|| MediaError::InvalidParams(format!("缺少{label}")))
    }

    pub fn file(&self, key: &str) -> Option<&UploadedFile> {
        self.files.get(key)
    }

    pub fn require_model(&self) -> Result<&str, MediaError> {
        let m = self.model.trim();
        if m.is_empty() {
            Err(MediaError::InvalidParams("未指定模型".into()))
        } else {
            Ok(m)
        }
    }
}

/// 适配器调用上下文：HTTP 客户端 + 该能力的数据驱动配置 + 凭据。
pub struct AdapterCtx<'a> {
    pub http: &'a MediaHttp,
    pub cfg: &'a CapabilityConfig,
    pub secret: &'a Secret,
    pub provider_id: &'a str,
}

impl fmt::Debug for AdapterCtx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdapterCtx").field("provider_id", &self.provider_id).field("format", &self.cfg.format).finish()
    }
}

impl AdapterCtx<'_> {
    pub fn request(&self, method: Method, url: &str) -> Result<RequestBuilder, MediaError> {
        self.http.request(method, url, self.cfg, self.secret)
    }

    pub fn post(&self, url: &str) -> Result<RequestBuilder, MediaError> {
        self.request(Method::POST, url)
    }

    pub fn get(&self, url: &str) -> Result<RequestBuilder, MediaError> {
        self.request(Method::GET, url)
    }

    /// 上游响应里带回的后续地址（轮询/取消地址）要带上凭据访问，所以必须属于该供应商的受信域名。
    pub fn trusted_url(&self, url: &str) -> Result<String, MediaError> {
        self.http.check_trusted_url(url, self.cfg).map(String::from)
    }

    /// 配置里的可选端点（如 `edit`），缺省为空。
    pub fn endpoint(&self, name: &str) -> Option<&str> {
        self.cfg.endpoints.get(name).map(String::as_str)
    }
}

/// 上游长任务的句柄；写进数据库以便应用重启后继续轮询。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTask {
    pub format: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_url: Option<String>,
    /// 格式私有数据（例如 fal 的 response_url）。
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl RemoteTask {
    pub fn new(format: &str, id: impl Into<String>) -> Self {
        RemoteTask { format: format.to_string(), id: id.into(), poll_url: None, cancel_url: None, data: Value::Null }
    }
}

/// 需要保存的产物。大文件都走这里，由任务执行层统一写盘并限制大小。
#[derive(Debug, Clone)]
pub enum OutputItem {
    /// 已在内存里的二进制（如 base64 解码后的图片、TTS 音频）。
    Bytes { data: Vec<u8>, ext: String },
    /// 需要下载的地址。`authed` 为 true 时用该供应商凭据下载（仅同域才会真正带上）。
    Remote { url: String, ext: String, authed: bool },
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub outputs: Vec<OutputItem>,
    /// 文本结果：ASR 转写、搜索结果 JSON、嵌入向量 JSON。会写入 MediaJob.summary。
    pub text: Option<String>,
}

impl Outcome {
    pub fn files(outputs: Vec<OutputItem>) -> Self {
        Outcome { outputs, text: None }
    }

    pub fn text(text: String) -> Self {
        Outcome { outputs: Vec::new(), text: Some(text) }
    }
}

#[derive(Debug)]
pub enum Submission {
    Done(Outcome),
    Pending(RemoteTask),
}

#[derive(Debug)]
pub enum PollState {
    Pending { progress: Option<u8> },
    Done(Outcome),
}

#[async_trait]
pub trait MediaAdapter: Send + Sync {
    /// 格式标识，与 media_providers.json 里的 `format` 对应。
    fn format(&self) -> &'static str;

    fn capability(&self) -> Capability;

    /// 提交请求。同步接口直接返回 `Done`；长任务返回 `Pending(RemoteTask)`。
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError>;

    /// 轮询长任务。上游报告失败时返回 `Err(MediaError::Failed(..))`。
    async fn poll(&self, _cx: &AdapterCtx<'_>, _task: &RemoteTask) -> Result<PollState, MediaError> {
        Err(MediaError::Unsupported("该格式不支持轮询".into()))
    }

    /// 尽力取消上游任务；上游没有取消接口的格式保持默认（仅本地停止轮询）。
    async fn cancel(&self, _cx: &AdapterCtx<'_>, _task: &RemoteTask) -> Result<(), MediaError> {
        Ok(())
    }
}

/// 一些通用小工具，供各格式适配器共用。
pub mod util {
    use super::*;

    /// 取 JSON 里的字符串字段。
    pub fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
        let mut cur = v;
        for p in path {
            cur = cur.get(*p)?;
        }
        cur.as_str()
    }

    /// 从 `WxH` 解析宽高。
    pub fn parse_size(size: &str) -> Option<(u32, u32)> {
        let (w, h) = size.split_once(['x', 'X', '×'])?;
        let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
        (w > 0 && h > 0).then_some((w, h))
    }

    /// 在候选比例中取与 `w:h` 最接近的一个（按对数比，避免宽/高偏向）。
    pub fn closest_ratio<'a>(w: u32, h: u32, candidates: &[&'a str]) -> Option<&'a str> {
        let target = (w as f64 / h as f64).ln();
        candidates
            .iter()
            .filter_map(|c| {
                let (a, b) = c.split_once([':', 'x'])?;
                let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
                (a > 0.0 && b > 0.0).then(|| (*c, ((a / b).ln() - target).abs()))
            })
            .min_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(c, _)| c)
    }

    /// 把 `16:9` 这类比例字符串拆成数值比。
    pub fn parse_ratio(ratio: &str) -> Option<(u32, u32)> {
        let (a, b) = ratio.split_once(':')?;
        let (a, b) = (a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?);
        (a > 0 && b > 0).then_some((a, b))
    }

    /// 路径片段（模型 ID、音色 ID）只允许安全字符，防止拼进 URL 时发生路径注入。
    pub fn safe_path_segment(s: &str, what: &str) -> Result<(), MediaError> {
        let ok = !s.is_empty()
            && s.len() <= 128
            && s != "."
            && s != ".."
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
        if ok {
            Ok(())
        } else {
            Err(MediaError::InvalidParams(format!("{what}包含不支持的字符")))
        }
    }

    /// 同上，但允许 `/` 分隔的多段路径（fal 模型 ID，如 `fal-ai/flux/schnell`）。
    pub fn safe_path(s: &str, what: &str) -> Result<(), MediaError> {
        if s.is_empty() || s.len() > 160 || s.starts_with('/') || s.ends_with('/') {
            return Err(MediaError::InvalidParams(format!("{what}无效")));
        }
        for seg in s.split('/') {
            safe_path_segment(seg, what)?;
        }
        Ok(())
    }

    /// 去掉文本里的 HTML 标签（Brave 等搜索接口会在摘要里带 `<strong>`）。
    pub fn strip_tags(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_tag = false;
        for c in s.chars() {
            match c {
                '<' => in_tag = true,
                '>' if in_tag => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    }

    pub fn truncate_chars(s: &str, max: usize) -> String {
        if s.chars().count() <= max {
            s.to_string()
        } else {
            let mut t: String = s.chars().take(max).collect();
            t.push('…');
            t
        }
    }
}

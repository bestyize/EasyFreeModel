//! 多模态作业（图像 / ASR / TTS / 视频 / 搜索 / 抓取）与嵌入。
//!
//! 适配器按「格式标识」归一（见 `registry.rs` 与 `resources/media_providers.json`），长任务走
//! 「提交 → 轮询 → 下载 → 写盘 → 更新 MediaJob」，状态全部写库；大文件只存受控目录里的路径。
//! 未接入的供应商如实返回 failed（「适配器尚未接入」），绝不伪造结果。

pub mod adapter;
pub mod embedding;
pub mod files;
pub mod http;
pub mod image;
pub mod jobs;
pub mod registry;
mod runway;
pub mod search;
pub mod stt;
pub mod tts;
pub mod video;

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use serde_json::{Map, Value};

use crate::error::AppResult;
use crate::models::{MediaJob, MediaKind};

pub use adapter::{MediaAdapter, MediaError};
pub use embedding::EmbeddingResult;
pub use jobs::{MediaContext, RecoverReport, ADAPTER_NOT_READY, INTERRUPTED};
pub use registry::{Capability, MediaProviderInfo, MediaRegistry};

static ADAPTERS: OnceLock<Vec<Box<dyn MediaAdapter>>> = OnceLock::new();

/// 全部已实现的适配器（每个格式标识一个）。
pub fn all_adapters() -> &'static [Box<dyn MediaAdapter>] {
    ADAPTERS.get_or_init(|| {
        let mut v: Vec<Box<dyn MediaAdapter>> = Vec::new();
        v.extend(image::adapters());
        v.extend(tts::adapters());
        v.extend(stt::adapters());
        v.extend(video::adapters());
        v.extend(search::adapters());
        v.extend(embedding::adapters());
        v
    })
}

pub fn find_adapter(cap: Capability, format: &str) -> Option<&'static dyn MediaAdapter> {
    all_adapters().iter().find(|a| a.capability() == cap && a.format() == format).map(|b| b.as_ref())
}

pub async fn list(ctx: &MediaContext, kind: MediaKind) -> AppResult<Vec<MediaJob>> {
    jobs::list(ctx, kind).await
}

/// 提交任务。没有 ready 的供应商、缺少凭据或参数无效时，返回一条 failed 的作业并写明原因。
pub async fn submit(ctx: &MediaContext, kind: MediaKind, params: Map<String, Value>) -> AppResult<MediaJob> {
    jobs::submit(ctx, kind, params).await
}

/// 取消仍在排队/运行的作业（真正中止轮询并尽力取消上游任务）；已结束的作业原样返回。
pub async fn cancel(ctx: &MediaContext, id: &str) -> AppResult<MediaJob> {
    jobs::cancel(ctx, id).await
}

/// 应用启动后调用一次（也会在首次使用媒体功能时自动触发一次）：恢复或终结上次遗留的运行中任务。
pub async fn recover_interrupted(ctx: &MediaContext) -> AppResult<RecoverReport> {
    jobs::recover_interrupted(ctx).await
}

/// 文本嵌入：内部/网关直接调用，不产生 MediaJob。
pub async fn embed(
    ctx: &MediaContext,
    provider_id: &str,
    model: Option<&str>,
    inputs: Vec<String>,
    dimensions: Option<u32>,
) -> Result<EmbeddingResult, MediaError> {
    jobs::embed(ctx, provider_id, model, inputs, dimensions).await
}

/// 媒体供应商能力视图（`capability` 为空时返回全部）。
pub fn provider_infos(registry: &MediaRegistry, capability: Option<Capability>) -> Vec<MediaProviderInfo> {
    registry.infos(capability)
}

/// 至少有一种媒体能力已接通适配器的供应商 id，供 providers 模块把 `adapter_ready` 置位。
pub fn ready_provider_ids() -> Vec<&'static str> {
    MediaRegistry::builtin().all_ready_provider_ids()
}

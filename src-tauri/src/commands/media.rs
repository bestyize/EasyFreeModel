use serde_json::{Map, Value};
use tauri::{AppHandle, Manager, State};

use crate::error::{AppError, AppResult};
use crate::media::{self, Capability, EmbeddingResult, MediaContext, MediaProviderInfo, MediaRegistry, RecoverReport};
use crate::models::{MediaJob, MediaKind};
use crate::state::AppState;

/// 由 Tauri 注入的 `AppHandle` 提供应用数据目录（媒体文件存放在其下的 `media/`）。
fn context(app: &AppHandle, state: &AppState) -> AppResult<MediaContext> {
    let dir = app.path().app_data_dir().map_err(|e| AppError::msg(format!("无法获取应用数据目录：{e}")))?;
    MediaContext::production(state.db.clone(), state.vault.clone(), &dir)
}

#[tauri::command]
pub async fn list_media_jobs(app: AppHandle, state: State<'_, AppState>, kind: MediaKind) -> AppResult<Vec<MediaJob>> {
    media::list(&context(&app, &state)?, kind).await
}

#[tauri::command]
pub async fn submit_media_job(
    app: AppHandle,
    state: State<'_, AppState>,
    kind: MediaKind,
    params: Map<String, Value>,
) -> AppResult<MediaJob> {
    media::submit(&context(&app, &state)?, kind, params).await
}

#[tauri::command]
pub async fn cancel_media_job(app: AppHandle, state: State<'_, AppState>, id: String) -> AppResult<MediaJob> {
    media::cancel(&context(&app, &state)?, &id).await
}

/// 各供应商的媒体能力表（格式、默认模型、可选参数、特性标记、ready/deferred 及原因）。
/// `kind` 为空时返回全部。前端据此只展示真正接通的供应商，并对不支持的字段禁用。
#[tauri::command]
pub fn list_media_providers(kind: Option<MediaKind>) -> Vec<MediaProviderInfo> {
    media::provider_infos(MediaRegistry::builtin(), kind.map(Capability::for_kind))
}

/// 恢复上次运行遗留的任务（启动时调用一次；首次使用媒体功能时也会自动触发）。
#[tauri::command]
pub async fn recover_media_jobs(app: AppHandle, state: State<'_, AppState>) -> AppResult<RecoverReport> {
    media::recover_interrupted(&context(&app, &state)?).await
}

/// 文本嵌入（不产生任务记录）。
#[tauri::command]
pub async fn create_embeddings(
    app: AppHandle,
    state: State<'_, AppState>,
    provider_id: String,
    model: Option<String>,
    input: Vec<String>,
    dimensions: Option<u32>,
) -> AppResult<EmbeddingResult> {
    let ctx = context(&app, &state)?;
    media::embed(&ctx, &provider_id, model.as_deref(), input, dimensions).await.map_err(|e| AppError::msg(e.to_string()))
}

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

use crate::error::{AppError, AppResult};
use crate::integrations::{self, ClientKind, ImportInput, ImportOutcome, ImportPreview};

fn config_path(app: &AppHandle, kind: ClientKind) -> AppResult<PathBuf> {
    let home = app.path().home_dir().map_err(|e| AppError::msg(format!("无法确定用户主目录：{e}")))?;
    Ok(integrations::config_path(kind, &home))
}

/// 只读预览：目标文件、会改哪些字段。解析失败等问题放在返回值的 `error` 里，不抛错。
#[tauri::command]
pub async fn preview_client_import(app: AppHandle, input: ImportInput) -> AppResult<ImportPreview> {
    let path = config_path(&app, input.client)?;
    tokio::task::spawn_blocking(move || integrations::preview(&path, &input))
        .await
        .map_err(|e| AppError::msg(format!("预览任务失败：{e}")))
}

/// 用户确认后写入：先备份再原子替换；内容已是最新则不写。
#[tauri::command]
pub async fn apply_client_import(app: AppHandle, input: ImportInput) -> AppResult<ImportOutcome> {
    let path = config_path(&app, input.client)?;
    tokio::task::spawn_blocking(move || integrations::apply(&path, &input))
        .await
        .map_err(|e| AppError::msg(format!("写入任务失败：{e}")))?
        .map_err(AppError::msg)
}

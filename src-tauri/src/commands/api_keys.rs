use tauri::State;

use crate::error::AppResult;
use crate::models::{ApiKeyInfo, ApiKeySecretResult, CreateApiKeyInput};
use crate::security::api_keys;
use crate::state::AppState;

#[tauri::command]
pub async fn list_api_keys(state: State<'_, AppState>) -> AppResult<Vec<ApiKeyInfo>> {
    api_keys::list(&state.db).await
}

/// 完整密钥只在这里（以及 rotate）返回一次。
#[tauri::command]
pub async fn create_api_key(state: State<'_, AppState>, input: CreateApiKeyInput) -> AppResult<ApiKeySecretResult> {
    api_keys::create(&state.db, input).await
}

#[tauri::command]
pub async fn rotate_api_key(state: State<'_, AppState>, id: String) -> AppResult<ApiKeySecretResult> {
    api_keys::rotate(&state.db, &id).await
}

#[tauri::command]
pub async fn set_api_key_disabled(state: State<'_, AppState>, id: String, disabled: bool) -> AppResult<ApiKeyInfo> {
    api_keys::set_disabled(&state.db, &id, disabled).await
}

#[tauri::command]
pub async fn delete_api_key(state: State<'_, AppState>, id: String) -> AppResult<()> {
    api_keys::delete(&state.db, &id).await
}

use tauri::State;

use crate::error::AppResult;
use crate::models::QuotaAccount;
use crate::quota;
use crate::state::AppState;

#[tauri::command]
pub async fn list_quota_accounts(state: State<'_, AppState>) -> AppResult<Vec<QuotaAccount>> {
    quota::list_accounts(&state.db).await
}

#[tauri::command]
pub async fn refresh_quota(state: State<'_, AppState>, connection_id: Option<String>) -> AppResult<Vec<QuotaAccount>> {
    quota::refresh(&state.db, &state.vault, &state.oauth, connection_id.as_deref()).await
}

#[tauri::command]
pub async fn set_quota_account_enabled(state: State<'_, AppState>, connection_id: String, enabled: bool) -> AppResult<()> {
    quota::set_enabled(&state.db, &connection_id, enabled).await
}

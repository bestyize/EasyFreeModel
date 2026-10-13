use tauri::State;

use crate::error::AppResult;
use crate::models::{UsageFilter, UsageRange, UsageRequest, UsageSummary};
use crate::state::AppState;
use crate::usage;

#[tauri::command]
pub async fn usage_summary(state: State<'_, AppState>, range: UsageRange) -> AppResult<UsageSummary> {
    usage::summary(&state.db, range).await
}

#[tauri::command]
pub async fn list_usage_requests(state: State<'_, AppState>, filter: UsageFilter) -> AppResult<Vec<UsageRequest>> {
    usage::list_requests(&state.db, filter).await
}

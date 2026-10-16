use tauri::State;

use crate::error::AppResult;
use crate::models::{Connection, Protocol, ProviderDetail, ProviderSummary, TestResult};
use crate::providers;
use crate::state::AppState;

#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> AppResult<Vec<ProviderSummary>> {
    providers::list(&state.db).await
}

#[tauri::command]
pub async fn get_provider(state: State<'_, AppState>, id: String) -> AppResult<ProviderDetail> {
    providers::get_detail(&state.db, &id).await
}

#[tauri::command]
pub async fn save_provider(state: State<'_, AppState>, detail: ProviderDetail) -> AppResult<ProviderDetail> {
    providers::save_detail(&state.db, &state.vault, detail).await
}

#[tauri::command]
pub async fn test_connection(state: State<'_, AppState>, connection_id: String) -> AppResult<TestResult> {
    providers::test_connection(&state.db, &state.vault, &connection_id).await
}

/// 批量测试：逐个提供商返回 TestResult（`providerId` 归组）。适配器未就绪的如实返回 ok=false 与原因。
#[tauri::command]
pub async fn test_providers(state: State<'_, AppState>, ids: Vec<String>) -> AppResult<Vec<TestResult>> {
    providers::test_providers(&state.db, &state.vault, &ids).await
}

/// 创建自定义提供商。`protocol`：openai-chat | openai-responses | anthropic-messages | gemini；
/// `base_url` 必须是 https，或回环地址（localhost / 127.0.0.1 / ::1）上的 http。
#[tauri::command]
pub async fn create_custom_provider(
    state: State<'_, AppState>,
    name: String,
    protocol: Protocol,
    base_url: String,
) -> AppResult<ProviderDetail> {
    providers::create_custom_provider(&state.db, &name, protocol, &base_url).await
}

/// 删除自定义提供商并清理其全部凭据；内置提供商会被拒绝。
#[tauri::command]
pub async fn delete_custom_provider(state: State<'_, AppState>, id: String) -> AppResult<()> {
    providers::delete_custom_provider(&state.db, &state.vault, &id).await
}

/// 录入 API Key 并创建连接。密钥只进系统凭据库，返回值不含密钥。
#[tauri::command]
pub async fn add_connection(
    state: State<'_, AppState>,
    provider_id: String,
    account_label: String,
    api_key: String,
) -> AppResult<Connection> {
    providers::add_connection(&state.db, &state.vault, &provider_id, &account_label, &api_key).await
}

/// 替换已有 API Key 连接的密钥。
#[tauri::command]
pub async fn update_connection_credential(
    state: State<'_, AppState>,
    connection_id: String,
    api_key: String,
) -> AppResult<()> {
    providers::update_connection_credential(&state.db, &state.vault, &connection_id, &api_key).await
}

/// 从上游（或内置模型库）拉取模型列表并同步入库
#[tauri::command]
pub async fn fetch_provider_models(
    state: State<'_, AppState>,
    provider_id: String,
) -> AppResult<Vec<crate::models::ModelEntry>> {
    providers::fetch_models(&state.db, &state.vault, &state.oauth, &provider_id).await
}

/// 模型级可用性探测：对单个已保存模型发起一次最小真实上游请求，如实报告结果。
#[tauri::command]
pub async fn test_provider_model(
    state: State<'_, AppState>,
    provider_id: String,
    model_id: String,
) -> AppResult<TestResult> {
    match state.gateway.runtime() {
        Some(rt) => rt.probe_model(&provider_id, &model_id).await,
        None => Ok(TestResult {
            connection_id: String::new(),
            provider_id: Some(provider_id),
            ok: false,
            latency_ms: None,
            error: Some("转发运行时尚未就绪，无法探测".to_string()),
        }),
    }
}

use tauri::State;

use crate::error::AppResult;
use crate::proxy::{ImportResult, PoolConfig, ProxyNode, ProxyNodeInput, ProxyPoolState};
use crate::state::AppState;

#[tauri::command]
pub async fn get_proxy_pool(state: State<'_, AppState>) -> AppResult<ProxyPoolState> {
    Ok(state.proxy_pool.state())
}

#[tauri::command]
pub async fn save_proxy_pool_config(state: State<'_, AppState>, config: PoolConfig) -> AppResult<ProxyPoolState> {
    state.proxy_pool.save_config(config).await
}

/// 新增（无 id）或更新（有 id）节点。`password`：缺省 = 保持，空串 = 清除。
#[tauri::command]
pub async fn save_proxy_node(state: State<'_, AppState>, input: ProxyNodeInput) -> AppResult<ProxyPoolState> {
    state.proxy_pool.save_node(input).await
}

#[tauri::command]
pub async fn delete_proxy_node(state: State<'_, AppState>, id: String) -> AppResult<ProxyPoolState> {
    state.proxy_pool.delete_node(&id).await
}

#[tauri::command]
pub async fn set_proxy_node_enabled(state: State<'_, AppState>, id: String, enabled: bool) -> AppResult<ProxyPoolState> {
    state.proxy_pool.set_enabled(&id, enabled).await
}

#[tauri::command]
pub async fn move_proxy_node(state: State<'_, AppState>, id: String, delta: i32) -> AppResult<ProxyPoolState> {
    state.proxy_pool.move_node(&id, delta).await
}

/// 批量导入，每行一个代理。返回新增数量与被跳过的行号及原因（不回显原文，避免泄露密码）。
#[tauri::command]
pub async fn import_proxy_nodes(state: State<'_, AppState>, text: String) -> AppResult<ImportResult> {
    state.proxy_pool.import(&text).await
}

/// 测速：`ids` 缺省 = 全部节点。返回测速后的全部节点。
#[tauri::command]
pub async fn test_proxy_nodes(state: State<'_, AppState>, ids: Option<Vec<String>>) -> AppResult<Vec<ProxyNode>> {
    Ok(state.proxy_pool.test(ids).await)
}

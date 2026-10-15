use std::collections::HashSet;
use std::io::ErrorKind;
use std::net::IpAddr;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::gateway::GatewayStatus;
use crate::gateway::VerifiedKey;
use crate::models::{AppSettings, GatewayModel, SystemStatus};
use crate::state::AppState;
use crate::store::settings as settings_repo;
use crate::usage;

/// 校验并规整设置。监听地址只校验格式；非回环地址的二次确认由前端负责。
pub fn validate_settings(mut s: AppSettings) -> AppResult<AppSettings> {
    let addr = s.listen_address.trim().to_string();
    let valid_addr = addr.eq_ignore_ascii_case("localhost") || addr.parse::<IpAddr>().is_ok();
    if !valid_addr {
        return Err(AppError::msg(format!("监听地址格式无效：{addr}（需为 IPv4/IPv6 地址或 localhost）")));
    }
    s.listen_address = addr;
    if s.port == 0 {
        return Err(AppError::msg("端口需在 1–65535 之间"));
    }
    if !(1..=3600).contains(&s.timeout_seconds) {
        return Err(AppError::msg("超时时间需在 1–3600 秒之间"));
    }
    if s.retry_count > 10 {
        return Err(AppError::msg("重试次数需在 0–10 之间"));
    }
    if !s.font_scale.is_finite() || !(0.5..=2.0).contains(&s.font_scale) {
        return Err(AppError::msg("字体缩放需在 0.5–2.0 之间"));
    }
    Ok(s)
}

/// 启动前探测端口，把常见失败转成可读的错误信息。探测用的监听器会立刻释放。
async fn probe_listen(address: &str, port: u16) -> AppResult<()> {
    match tokio::net::TcpListener::bind((address, port)).await {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) => Err(listen_error(address, port, &e)),
    }
}

fn listen_error(address: &str, port: u16, e: &std::io::Error) -> AppError {
    let msg = match e.kind() {
        ErrorKind::AddrInUse => {
            format!("端口 {port} 已被占用（{address}:{port}）。请关闭占用该端口的程序，或在设置中更换端口后重试")
        }
        ErrorKind::PermissionDenied => {
            format!("没有权限监听 {address}:{port}。1024 以下的端口通常需要管理员权限，请改用更大的端口")
        }
        ErrorKind::AddrNotAvailable => format!("地址 {address} 不是本机可用的网络地址，请检查监听地址设置"),
        _ => format!("无法监听 {address}:{port}：{e}"),
    };
    AppError::msg(msg)
}

pub async fn start_gateway_inner(state: &AppState) -> AppResult<GatewayStatus> {
    let current = state.gateway.status().await;
    if current.running {
        return Ok(current);
    }
    let s = settings_repo::load(&state.db).await?;
    probe_listen(&s.listen_address, s.port).await?;
    match state.gateway.start(&s.listen_address, s.port, state.key_verifier()).await {
        Err(AppError::Io(e)) => Err(listen_error(&s.listen_address, s.port, &e)),
        other => other,
    }
}

#[tauri::command]
pub async fn system_status(state: State<'_, AppState>) -> AppResult<SystemStatus> {
    let gateway = state.gateway.status().await;
    let (api_requests, success_rate, avg_latency_ms) = usage::today_stats(&state.db).await?;
    Ok(SystemStatus { gateway, api_requests, success_rate, avg_latency_ms })
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    settings_repo::load(&state.db).await
}

#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, settings: AppSettings) -> AppResult<AppSettings> {
    let s = validate_settings(settings)?;
    settings_repo::save(&state.db, &s).await?;
    Ok(s)
}

/// 导入弹窗的模型候选：已启用、适配器就绪、至少有一个可调度连接的供应商下的已启用模型。
/// 候选名是网关实际接受的对外模型名；未连账号的内置预设不会误入列表。
pub async fn list_gateway_models_inner(state: &AppState) -> AppResult<Vec<GatewayModel>> {
    let Some(rt) = state.gateway.runtime() else {
        return Ok(Vec::new());
    };
    let unrestricted = VerifiedKey { id: String::new(), allowed_protocols: Vec::new(), allowed_models: Vec::new(), rpm_limit: None, tpm_limit: None };
    let connected: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT provider_id FROM connections WHERE enabled = 1 AND status IN ('active', 'error')",
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let mut models: Vec<GatewayModel> = rt
        .list_public_models(&unrestricted)
        .await?
        .into_iter()
        .filter(|m| connected.contains(&m.provider_id))
        .map(|m| GatewayModel { id: m.id, provider_id: m.provider_id, provider_name: m.provider_name })
        .collect();
    models.sort_by(|a, b| a.provider_name.cmp(&b.provider_name).then(a.id.cmp(&b.id)));
    Ok(models)
}

#[tauri::command]
pub async fn list_gateway_models(state: State<'_, AppState>) -> AppResult<Vec<GatewayModel>> {
    list_gateway_models_inner(&state).await
}

#[tauri::command]
pub async fn start_gateway(state: State<'_, AppState>) -> AppResult<GatewayStatus> {
    start_gateway_inner(&state).await
}

#[tauri::command]
pub async fn stop_gateway(state: State<'_, AppState>) -> AppResult<GatewayStatus> {
    Ok(state.gateway.stop().await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::MemoryVault;
    use crate::store;
    use std::sync::Arc;

    #[test]
    fn settings_validation() {
        let ok = validate_settings(AppSettings { listen_address: " 127.0.0.1 ".into(), ..Default::default() }).unwrap();
        assert_eq!(ok.listen_address, "127.0.0.1");
        for addr in ["::1", "0.0.0.0", "localhost", "192.168.1.5"] {
            assert!(validate_settings(AppSettings { listen_address: addr.into(), ..Default::default() }).is_ok(), "{addr}");
        }
        for addr in ["", "999.1.1.1", "http://x", "a b", "127.0.0.1:80"] {
            assert!(validate_settings(AppSettings { listen_address: addr.into(), ..Default::default() }).is_err(), "{addr}");
        }
        assert!(validate_settings(AppSettings { port: 0, ..Default::default() }).is_err());
        assert!(validate_settings(AppSettings { timeout_seconds: 0, ..Default::default() }).is_err());
        assert!(validate_settings(AppSettings { retry_count: 11, ..Default::default() }).is_err());
        assert!(validate_settings(AppSettings { font_scale: f64::NAN, ..Default::default() }).is_err());
        assert!(validate_settings(AppSettings { font_scale: 3.0, ..Default::default() }).is_err());
    }

    #[tokio::test]
    async fn port_in_use_gives_clear_error() {
        let held = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let state = AppState::new(store::open_memory().await.unwrap(), Arc::new(MemoryVault::default()));
        let s = AppSettings { port, ..Default::default() };
        settings_repo::save(&state.db, &s).await.unwrap();

        let err = start_gateway_inner(&state).await.unwrap_err().to_string();
        assert!(err.contains("已被占用") && err.contains(&port.to_string()), "{err}");
    }

    #[tokio::test]
    async fn gateway_models_list_only_enabled_ready_models() {
        let state = AppState::new(store::open_memory().await.unwrap(), Arc::new(MemoryVault::default()));
        for (id, enabled) in [("ready", 1), ("other", 1), ("off", 0), ("unconfigured", 1)] {
            sqlx::query(
                "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, protocols_json, enabled, sort_order, \
                    round_robin, max_concurrency, max_retries, created_at, updated_at, base_url) \
                 VALUES (?1, 'custom', ?2, 'custom', 'openai-compatible', 1, '[]', ?3, 0, 0, 4, 2, 't', 't', 'https://api.example.com/v1')",
            )
            .bind(id)
            .bind(format!("Provider {id}"))
            .bind(enabled)
            .execute(&state.db)
            .await
            .unwrap();
            if id != "unconfigured" {
                sqlx::query("INSERT INTO connections (id, provider_id, account_label, kind, status, enabled, created_at, updated_at) VALUES (?1, ?2, 'Account', 'apiKey', 'active', 1, 't', 't')")
                    .bind(format!("{id}-conn"))
                    .bind(id)
                    .execute(&state.db)
                    .await
                    .unwrap();
            }
            for alias in ["zeta", "alpha"] {
                sqlx::query(
                    "INSERT INTO models (id, provider_id, upstream_model_id, public_alias, enabled, position) VALUES (?1, ?2, ?3, ?4, 1, 0)",
                )
                .bind(format!("{id}/{alias}"))
                .bind(id)
                .bind(alias)
                .bind(format!("{id}-{alias}"))
                .execute(&state.db)
                .await
                .unwrap();
            }
        }
        let models = list_gateway_models_inner(&state).await.unwrap();
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["other-alpha", "other-zeta", "ready-alpha", "ready-zeta"], "所有已连接的可用提供商模型都出现；停用或未配置连接的不出现");
        assert_eq!(models.iter().map(|m| m.provider_name.as_str()).collect::<Vec<_>>(), ["Provider other", "Provider other", "Provider ready", "Provider ready"]);
    }

    #[tokio::test]
    async fn settings_default_to_loopback() {
        let pool = store::open_memory().await.unwrap();
        let s = settings_repo::load(&pool).await.unwrap();
        assert_eq!(s.listen_address, "127.0.0.1");
    }
}

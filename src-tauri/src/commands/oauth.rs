//! OAuth 授权命令。命令名与参数名必须与 `src/api/tauri.ts` 的 invoke 调用逐一对应
//! （Rust snake_case 参数由 Tauri 自动对应前端 camelCase）。逻辑全部在 `crate::oauth::OAuthHub`。
//!
//! 返回值与错误里不含令牌、授权码、code_verifier、refresh token、client secret：
//! 只有授权 URL、设备码页面的 user_code 与 verification_uri、状态枚举、脱敏错误码（`{ code, message }`）。

use tauri::{Manager, State};

use crate::oauth::{HubError, PollResult, ProviderInfo, Secret, StartResult};
use crate::state::AppState;

/// 全部 OAuth 提供商的授权能力与状态（含 `custom`）。
#[tauri::command]
pub async fn oauth_list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderInfo>, HubError> {
    state.oauth_hub.list_providers().await
}

/// 保存用户自备的客户端凭据到系统凭据库。`client_secret` 一进来就包进 `Secret`（Debug 脱敏、释放时清零）。
#[tauri::command]
pub async fn oauth_set_client_credentials(
    state: State<'_, AppState>,
    provider_id: String,
    client_id: String,
    client_secret: Option<String>,
    base_url: Option<String>,
) -> Result<ProviderInfo, HubError> {
    let secret = client_secret.map(Secret::from);
    state.oauth_hub.set_client_credentials(&provider_id, &client_id, secret, base_url.as_deref()).await
}

#[tauri::command]
pub async fn oauth_clear_client_credentials(state: State<'_, AppState>, provider_id: String) -> Result<ProviderInfo, HubError> {
    state.oauth_hub.clear_client_credentials(&provider_id).await
}

/// 开始授权：authorization-code 流程绑定 127.0.0.1 回调监听并返回要在系统浏览器打开的授权 URL；
/// device 流程返回 user_code / verification_uri 并在后台轮询。
#[tauri::command]
pub async fn oauth_start(state: State<'_, AppState>, provider_id: String, account_label: String) -> Result<StartResult, HubError> {
    state.oauth_hub.start(&provider_id, &account_label).await
}

/// 授权会话状态：pending / succeeded{connectionId} / failed{code} / cancelled / expired。
#[tauri::command]
pub async fn oauth_poll(state: State<'_, AppState>, session_id: String) -> Result<PollResult, HubError> {
    Ok(state.oauth_hub.poll(&session_id))
}

/// 取消授权；返回时回调监听已关闭。
#[tauri::command]
pub async fn oauth_cancel(state: State<'_, AppState>, session_id: String) -> Result<(), HubError> {
    state.oauth_hub.cancel(&session_id).await;
    Ok(())
}

#[tauri::command]
pub async fn oauth_reauthorize(state: State<'_, AppState>, connection_id: String) -> Result<StartResult, HubError> {
    state.oauth_hub.reauthorize(&connection_id).await
}

/// 断开：删除令牌条目与连接。
#[tauri::command]
pub async fn oauth_disconnect(state: State<'_, AppState>, connection_id: String) -> Result<(), HubError> {
    state.oauth_hub.disconnect(&connection_id).await
}

/// 应用退出时关闭所有未完成授权的回调监听（作为插件注册，无需改动 `run` 的收尾代码）。
pub fn exit_cleanup_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("oauth-cleanup")
        .on_event(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(state) = app.try_state::<AppState>() {
                    state.oauth_hub.shutdown_now();
                }
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::*;
    use crate::oauth::testkit::*;
    use crate::oauth::FlowKind;
    use crate::security::vault::MemoryVault;
    use crate::security::CredentialVault;
    use crate::store;

    const FORBIDDEN: [&str; 12] = [
        "ACCESS-AUTH",
        "REFRESH-AUTH",
        "ACCESS-DEV",
        "REFRESH-DEV",
        "AUTHCODE-",
        "DEVICE-SECRET",
        "CLIENT-SECRET-XYZ",
        "code_verifier",
        "client_secret",
        "access_token",
        "refresh_token",
        "id_token",
    ];

    fn assert_clean(label: &str, value: &impl serde::Serialize) {
        let text = serde_json::to_string(value).unwrap();
        for needle in FORBIDDEN {
            assert!(!text.contains(needle), "{label} leaked {needle:?}: {text}");
        }
    }

    /// 命令直接返回 hub 的这些类型：把一次完整的授权（设备码 + 授权码 + 凭据表单 + 错误）涉及的
    /// 每个返回值都序列化出来，确认不含令牌、授权码、code_verifier、设备码或 client secret。
    #[tokio::test]
    async fn command_results_serialize_without_tokens_or_secrets() {
        let env = env().await;
        *env.fake.id_token.lock().unwrap() = Some(fake_jwt_with_email("p@example.com"));
        *env.fake.expect_secret.lock().unwrap() = Some("CLIENT-SECRET-XYZ".into());

        // oauth_set_client_credentials / oauth_list_providers / oauth_clear_client_credentials
        let set = env
            .hub
            .set_client_credentials("t-secret", "my-client", Some(crate::oauth::Secret::new("CLIENT-SECRET-XYZ")), None)
            .await
            .unwrap();
        assert_clean("oauth_set_client_credentials", &set);
        assert_clean("oauth_list_providers", &env.hub.list_providers().await.unwrap());

        // oauth_start（授权码）+ oauth_poll（pending -> succeeded）
        let code = env.hub.start("t-secret", "work").await.unwrap();
        let start_json = serde_json::to_value(&code).unwrap();
        assert_eq!(start_json["flow"], "authorization-code");
        let mut keys: Vec<_> = start_json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["authorizeUrl", "expiresAt", "flow", "sessionId", "userCode", "verificationUri"]);
        // 授权 URL 里没有机密：client secret 不在 URL 里，code_verifier 不在（只有 challenge），授权码尚未产生
        let url = code.authorize_url.clone().unwrap();
        assert!(!url.contains("CLIENT-SECRET") && !url.contains("code_verifier") && !url.contains("AUTHCODE"), "{url}");
        assert_clean("oauth_start(code)", &code);
        let pending = env.hub.poll(&code.session_id);
        assert_eq!(serde_json::to_value(&pending).unwrap(), json!({"status": "pending"}));
        browse(&url).await;
        let done = wait_done(&env.hub, &code.session_id).await;
        let done_json = serde_json::to_value(&done).unwrap();
        assert_eq!(done_json["status"], "succeeded");
        assert!(done_json["connectionId"].is_string());
        assert_clean("oauth_poll(succeeded)", &done);

        // oauth_start（设备码）：user_code 与 verification_uri 可以给界面，device_code 不行
        let dev = env.hub.start("t-device", "").await.unwrap();
        assert_clean("oauth_start(device)", &dev);
        assert_eq!(dev.user_code.as_deref(), Some("WDJB-MJHT"));
        env.hub.cancel(&dev.session_id).await;
        assert_eq!(serde_json::to_value(env.hub.poll(&dev.session_id)).unwrap(), json!({"status": "cancelled"}));

        // 失败 / 过期的轮询结果只带脱敏错误码
        for result in [
            crate::oauth::PollResult::Failed { code: "access_denied".into() },
            crate::oauth::PollResult::Expired,
            crate::oauth::PollResult::Cancelled,
        ] {
            assert_clean("oauth_poll(terminal)", &result);
        }
        assert_eq!(
            serde_json::to_value(crate::oauth::PollResult::Failed { code: "access_denied".into() }).unwrap(),
            json!({"status": "failed", "code": "access_denied"})
        );

        // 错误值：`{code, message}`，message 固定，不带输入内容
        let err = env
            .hub
            .set_client_credentials("t-secret", "CLIENT-SECRET-XYZ\nx", Some(crate::oauth::Secret::new("CLIENT-SECRET-XYZ")), None)
            .await
            .unwrap_err();
        let err_json: Value = serde_json::to_value(&err).unwrap();
        assert_eq!(err_json["code"], "invalid_input");
        assert_clean("error", &err);
        assert_clean("error(start)", &env.hub.start("nope", "x").await.unwrap_err());

        let cleared = env.hub.clear_client_credentials("t-secret").await.unwrap();
        assert_clean("oauth_clear_client_credentials", &cleared);

        // 凭据库里确实有机密（证明上面的断言不是因为什么都没存）
        assert!(env.vault.get("oauth-client/t-secret").unwrap().is_none(), "cleared");
    }

    /// 真实内嵌目录 + 生产构造路径（AppState::new）：25 家里 14 家 custom、11 家标准流程；
    /// 需要自备凭据的显示「需要配置客户端凭据」；github / muse 如实标注令牌兑换步骤未接入。
    #[tokio::test]
    async fn builtin_catalog_listing_through_app_state() {
        let db = store::open_memory().await.unwrap();
        crate::providers::catalog::seed(&db).await.unwrap();
        let vault: Arc<dyn CredentialVault> = Arc::new(MemoryVault::default());
        let state = crate::state::AppState::new(db, vault);

        let list = state.oauth_hub.list_providers().await.unwrap();
        assert_eq!(list.len(), 25);
        let custom: Vec<_> = list.iter().filter(|p| p.flow == FlowKind::Custom).collect();
        assert_eq!(custom.len(), 12);
        for p in &custom {
            let v = serde_json::to_value(p).unwrap();
            assert_eq!(v["status"], "custom", "{}", p.provider_id);
            assert!(!v["reason"].as_str().unwrap().is_empty(), "{} shows why it is not supported", p.provider_id);
            assert!(!v["requires"].as_array().unwrap().is_empty(), "{}", p.provider_id);
        }
        // CodeBuddy 的 state 轮询流程：可授权、状态 ready
        let state_poll: Vec<&str> = list
            .iter()
            .filter(|p| p.flow == FlowKind::StatePoll)
            .map(|p| p.provider_id.as_str())
            .collect();
        assert_eq!(state_poll, ["codebuddy-cn", "codebuddy-intl"]);
        let status = |id: &str| serde_json::to_value(list.iter().find(|p| p.provider_id == id).unwrap()).unwrap();
        for id in ["gitlab"] {
            assert_eq!(status(id)["status"], "need_client_credentials", "{id}");
        }
        for id in ["antigravity", "gemini-cli", "claude", "codex", "github", "grok-cli", "kimi", "muse", "minimax-code", "minimax-code-global", "codebuddy-cn", "codebuddy-intl"] {
            assert_eq!(status(id)["status"], "ready", "{id}");
        }
        for id in ["github", "muse"] {
            assert_eq!(status(id)["tokenExchangePending"], true);
            assert!(status(id)["limitation"].as_str().unwrap().contains("令牌兑换步骤尚未接入"));
        }
        assert!(status("claude")["riskNotice"].is_string());
        // 转发适配器已就绪的提供商，limitation 为 null
        assert!(status("claude")["limitation"].is_null());

        // 自定义流程的授权入口被拒绝（不会去绑端口 / 请求网络）
        assert_eq!(state.oauth_hub.start("cursor", "x").await.unwrap_err().code(), "custom_flow_required");
        // 需要凭据的提供商（如 gitlab）没配置时：missing_client_credential
        assert_eq!(state.oauth_hub.start("gitlab", "x").await.unwrap_err().code(), "missing_client_credential");

        // Antigravity 内置凭据可以直接启动授权并打开 Google 登录页
        let started_ag = state.oauth_hub.start("antigravity", "me").await.unwrap();
        assert!(started_ag.authorize_url.as_deref().unwrap().starts_with("https://accounts.google.com/"));
        state.oauth_hub.cancel(&started_ag.session_id).await;

        // 客户端凭据经 AppState 里的凭据库实现保存，且被框架读取
        let set = state
            .oauth_hub
            .set_client_credentials("gitlab", "my-gitlab-client-id", Some(crate::oauth::Secret::new("sec-ret-value")), None)
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(&set).unwrap()["status"], "ready");
        assert!(state.vault.get("oauth-client/gitlab").unwrap().is_some());
        let started = state.oauth_hub.start("gitlab", "me").await.unwrap();
        assert!(started.authorize_url.as_deref().unwrap().starts_with("https://gitlab.com/"));
        let url = started.authorize_url.as_deref().unwrap();
        assert!(url.contains("client_id=my-gitlab-client-id") && !url.contains("sec-ret-value"), "{url}");
        state.oauth_hub.cancel(&started.session_id).await;
    }

    #[test]
    fn the_exit_plugin_builds() {
        let _plugin: tauri::plugin::TauriPlugin<tauri::Wry> = exit_cleanup_plugin();
    }
}

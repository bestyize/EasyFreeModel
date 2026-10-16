pub mod commands;
pub mod desktop;
pub mod error;
pub mod gateway;
pub mod integrations;
pub mod media;
pub mod models;
pub mod oauth;
pub mod protocols;
pub mod providers;
pub mod proxy;
pub mod quota;
pub mod routing;
pub mod security;
pub mod state;
pub mod store;
pub mod upstream;
pub mod usage;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(commands::oauth::exit_cleanup_plugin())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            // setup 在主线程同步执行；这里阻塞到数据库就绪后再 manage，命令不会看到半初始化状态
            let state = tauri::async_runtime::block_on(state::AppState::init(&data_dir))?;
            // Load saved preference before the first close event; a new
            // install defaults to tray mode and an existing opt-out persists.
            let tray_enabled = tauri::async_runtime::block_on(store::settings::load(&state.db))?.tray_enabled;
            app.manage(desktop::TrayPreference::new(tray_enabled));
            desktop::setup_tray(app, tray_enabled)?;
            app.manage(state);
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let tray_enabled = app
                    .try_state::<desktop::TrayPreference>()
                    .is_some_and(|p| p.enabled());
                if tray_enabled && app.tray_by_id(desktop::TRAY_ID).is_some() {
                    api.prevent_close();
                    // Hide rather than destroy the webview: the active gateway,
                    // authenticated sessions and in-memory state stay alive.
                    if let Err(error) = window.hide() {
                        tracing::error!(%error, "could not hide window to tray; exiting safely");
                        app.exit(0);
                    }
                } else {
                    // Explicitly disabled tray mode: close really quits,
                    // including on macOS where closing a window may not exit.
                    app.exit(0);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::providers::list_providers,
            commands::providers::get_provider,
            commands::providers::save_provider,
            commands::providers::test_connection,
            commands::providers::test_providers,
            commands::providers::create_custom_provider,
            commands::providers::delete_custom_provider,
            commands::providers::add_connection,
            commands::providers::update_connection_credential,
            commands::providers::fetch_provider_models,
            commands::providers::test_provider_model,
            commands::oauth::oauth_list_providers,
            commands::oauth::oauth_set_client_credentials,
            commands::oauth::oauth_clear_client_credentials,
            commands::oauth::oauth_start,
            commands::oauth::oauth_poll,
            commands::oauth::oauth_submit_callback,
            commands::oauth::oauth_cancel,
            commands::oauth::oauth_reauthorize,
            commands::oauth::oauth_disconnect,
            commands::api_keys::list_api_keys,
            commands::api_keys::create_api_key,
            commands::api_keys::rotate_api_key,
            commands::api_keys::set_api_key_disabled,
            commands::api_keys::delete_api_key,
            commands::usage::usage_summary,
            commands::usage::list_usage_requests,
            commands::quota::list_quota_accounts,
            commands::quota::refresh_quota,
            commands::quota::set_quota_account_enabled,
            commands::system::system_status,
            commands::system::get_settings,
            commands::system::save_settings,
            commands::system::start_gateway,
            commands::system::stop_gateway,
            commands::system::list_gateway_models,
            commands::media::list_media_jobs,
            commands::media::submit_media_job,
            commands::media::cancel_media_job,
            commands::media::list_media_providers,
            commands::media::recover_media_jobs,
            commands::media::create_embeddings,
            commands::integrations::preview_client_import,
            commands::integrations::apply_client_import,
            commands::proxy::get_proxy_pool,
            commands::proxy::save_proxy_pool_config,
            commands::proxy::save_proxy_node,
            commands::proxy::delete_proxy_node,
            commands::proxy::set_proxy_node_enabled,
            commands::proxy::move_proxy_node,
            commands::proxy::import_proxy_nodes,
            commands::proxy::test_proxy_nodes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running FreeApiBox");
}

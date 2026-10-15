//! 全局应用状态：在 Tauri `setup` 中异步初始化后交给 `manage`。

use std::path::Path;
use std::sync::Arc;

use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};
use crate::gateway::{GatewayManager, KeyVerifier};
use crate::oauth::{ClientCredentialAdmin, OAuthHub, OAuthService};
use crate::providers::catalog;
use crate::proxy::ProxyPool;
use crate::routing::Runtime;
use crate::security::{api_keys, CredentialVault, DbKeyVerifier, LocalVault, VaultClientCredentials, VaultTokenStore};
use crate::store;

/// 系统凭据库中使用的 service 名，与 tauri.conf.json 的 identifier 保持一致。
pub const VAULT_SERVICE: &str = "app.routerdesktop.client";
pub const DB_FILE: &str = "router.db";

pub struct AppState {
    pub db: SqlitePool,
    pub gateway: GatewayManager,
    pub vault: Arc<dyn CredentialVault>,
    /// OAuth 令牌服务：转发管线据此取得（必要时先刷新）OAuth 连接的访问令牌。
    pub oauth: Arc<OAuthService>,
    /// OAuth 授权的应用层：授权会话、连接创建 / 重新授权 / 断开、客户端凭据（命令层入口）。
    pub oauth_hub: Arc<OAuthHub>,
    /// 出站代理池：转发管线与提供商探测据此选路。
    pub proxy_pool: Arc<ProxyPool>,
}

impl AppState {
    /// 生产构造：令牌与客户端凭据都存在系统凭据库（`vault`）。
    pub fn new(db: SqlitePool, vault: Arc<dyn CredentialVault>) -> Self {
        Self::try_new(db, vault).expect("内置 OAuth 目录由单元测试校验，HTTP 客户端创建不应失败")
    }

    pub fn try_new(db: SqlitePool, vault: Arc<dyn CredentialVault>) -> AppResult<Self> {
        let clients = Arc::new(VaultClientCredentials::new(vault.clone()));
        let tokens = Arc::new(VaultTokenStore::new(vault.clone()));
        let oauth = OAuthService::with_builtin_catalog(clients.clone(), tokens)
            .map_err(|e| AppError::msg(format!("OAuth 服务初始化失败：{e}")))?;
        Ok(Self::with_oauth(db, vault, Arc::new(oauth), clients))
    }

    /// 注入 OAuth 服务与客户端凭据存储（测试里可换成内存实现）。
    pub fn with_oauth(
        db: SqlitePool,
        vault: Arc<dyn CredentialVault>,
        oauth: Arc<OAuthService>,
        clients: Arc<dyn ClientCredentialAdmin>,
    ) -> Self {
        let proxy_pool = Arc::new(ProxyPool::new(db.clone(), vault.clone()));
        // 运行时在网关启动时读取；运行中的网关需重启才会使用新构造的运行时
        let gateway = match Runtime::new(db.clone(), vault.clone()) {
            Ok(runtime) => GatewayManager::with_runtime(Arc::new(runtime.with_oauth(oauth.clone()).with_proxy_pool(proxy_pool.clone()))),
            Err(e) => {
                tracing::error!("上游转发运行时初始化失败，聊天端点将返回 501：{e}");
                GatewayManager::new()
            }
        };
        let oauth_hub = Arc::new(OAuthHub::new(oauth.clone(), clients, db.clone()));
        Self { db, gateway, vault, oauth, oauth_hub, proxy_pool }
    }

    /// 打开数据库（含迁移）、写入提供商目录、接上本地凭据库（对齐 9Router，秒级静默读取）。
    pub async fn init(data_dir: &Path) -> AppResult<Self> {
        let db = store::open(&data_dir.join(DB_FILE)).await?;
        catalog::seed(&db).await?;
        api_keys::ensure_default(&db).await?;
        let vault = Arc::new(LocalVault::new(data_dir.join("vault.json")));
        let state = Self::try_new(db, vault)?;
        state.proxy_pool.reload().await?;
        crate::proxy::install_global(state.proxy_pool.clone());
        Ok(state)
    }

    /// 网关用来校验 Bearer 密钥的校验器。
    pub fn key_verifier(&self) -> Arc<dyn KeyVerifier> {
        Arc::new(DbKeyVerifier::new(self.db.clone()))
    }
}

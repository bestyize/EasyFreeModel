//! 路由层读库函数（只读 + 连接状态回写）。
//!
//! 这里不复用 `store/providers.rs`：路由需要的是「按请求热路径」的窄查询，且该文件归其它模块所有。

use sqlx::{FromRow, SqlitePool};

use crate::error::AppResult;

#[derive(Debug, Clone, Default, FromRow)]
pub struct ProviderCfg {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub adapter: String,
    pub adapter_ready: bool,
    pub pending_reason: Option<String>,
    /// 自定义提供商的 Base URL（内置提供商的地址来自 provider_status.json）。
    pub base_url: Option<String>,
    pub enabled: bool,
    pub round_robin: bool,
    pub max_concurrency: i64,
    pub max_retries: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct ModelCfg {
    pub id: String,
    pub provider_id: String,
    pub upstream_model_id: String,
    pub public_alias: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct ConnCfg {
    pub id: String,
    pub provider_id: String,
    pub account_label: String,
    pub kind: String,
    pub status: String,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct HeaderCfg {
    pub header_key: String,
    pub value: Option<String>,
    pub secret: bool,
    pub credential_ref: Option<String>,
}

const PROVIDER_COLS: &str = "id, display_name, kind, adapter, adapter_ready, pending_reason, base_url, enabled, \
     round_robin, max_concurrency, max_retries";

pub async fn list_enabled_providers(pool: &SqlitePool) -> AppResult<Vec<ProviderCfg>> {
    let sql = format!("SELECT {PROVIDER_COLS} FROM providers WHERE enabled = 1 ORDER BY sort_order, created_at, id");
    Ok(sqlx::query_as::<_, ProviderCfg>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?)
}

pub async fn get_provider(pool: &SqlitePool, id: &str) -> AppResult<Option<ProviderCfg>> {
    let sql = format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?1");
    Ok(sqlx::query_as::<_, ProviderCfg>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?)
}

/// 已启用提供商下所有已启用的模型，按提供商顺序、模型顺序排列（输出顺序因此是确定的）。
pub async fn list_enabled_models(pool: &SqlitePool) -> AppResult<Vec<ModelCfg>> {
    Ok(sqlx::query_as::<_, ModelCfg>(
        "SELECT m.id, m.provider_id, m.upstream_model_id, m.public_alias \
         FROM models m JOIN providers p ON p.id = m.provider_id \
         WHERE m.enabled = 1 AND p.enabled = 1 \
         ORDER BY p.sort_order, p.created_at, p.id, m.position, m.id",
    )
    .fetch_all(pool)
    .await?)
}

/// 单个模型（用于模型级探测）；不存在返回 None。
pub async fn get_model(pool: &SqlitePool, provider_id: &str, id: &str) -> AppResult<Option<ModelCfg>> {
    Ok(sqlx::query_as::<_, ModelCfg>(
        "SELECT id, provider_id, upstream_model_id, public_alias FROM models WHERE id = ?1 AND provider_id = ?2",
    )
    .bind(id)
    .bind(provider_id)
    .fetch_optional(pool)
    .await?)
}

/// 连接按 priority、创建时间排序；是否可用由调度器判断。
pub async fn list_connections(pool: &SqlitePool, provider_id: &str) -> AppResult<Vec<ConnCfg>> {
    Ok(sqlx::query_as::<_, ConnCfg>(
        "SELECT id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref \
         FROM connections WHERE provider_id = ?1 ORDER BY priority, created_at, id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_headers(pool: &SqlitePool, provider_id: &str) -> AppResult<Vec<HeaderCfg>> {
    Ok(sqlx::query_as::<_, HeaderCfg>(
        "SELECT header_key, value, secret, credential_ref FROM provider_headers \
         WHERE provider_id = ?1 ORDER BY position, header_key",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

/// 回写连接状态。只在 `active` ↔ `error` 之间切换，绝不覆盖用户设置的 `paused`。
pub async fn set_connection_error(pool: &SqlitePool, id: &str, error: bool) -> AppResult<()> {
    let (from, to) = if error { ("active", "error") } else { ("error", "active") };
    sqlx::query("UPDATE connections SET status = ?3, updated_at = ?4 WHERE id = ?1 AND status = ?2")
        .bind(id)
        .bind(from)
        .bind(to)
        .bind(crate::store::now_ts())
        .execute(pool)
        .await?;
    Ok(())
}

/// OAuth 令牌失效：标记为 `needsReauth`（调度器之后不再尝试它，直到用户重新授权）。
/// 用户手动暂停的 `paused` 保持不变。
pub async fn set_connection_needs_reauth(pool: &SqlitePool, id: &str) -> AppResult<()> {
    sqlx::query("UPDATE connections SET status = 'needsReauth', updated_at = ?2 WHERE id = ?1 AND status IN ('active', 'error')")
        .bind(id)
        .bind(crate::store::now_ts())
        .execute(pool)
        .await?;
    Ok(())
}

//! 代理池 repo：节点表 + 池配置（app_settings 中的单个 JSON 键）。

use sqlx::{FromRow, SqlitePool};

use crate::error::AppResult;
use crate::proxy::PoolConfig;

const CONFIG_KEY: &str = "proxyPoolConfig";

#[derive(Debug, Clone, FromRow)]
pub struct NodeRow {
    pub id: String,
    pub name: String,
    pub scheme: String,
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub credential_ref: Option<String>,
    pub enabled: bool,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
}

const COLS: &str = "id, name, scheme, host, port, username, credential_ref, enabled, sort_order, created_at, updated_at";

pub async fn list_nodes(pool: &SqlitePool) -> AppResult<Vec<NodeRow>> {
    let sql = format!("SELECT {COLS} FROM proxy_nodes ORDER BY sort_order, created_at, id");
    Ok(sqlx::query_as::<_, NodeRow>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?)
}

pub async fn get_node(pool: &SqlitePool, id: &str) -> AppResult<Option<NodeRow>> {
    let sql = format!("SELECT {COLS} FROM proxy_nodes WHERE id = ?1");
    Ok(sqlx::query_as::<_, NodeRow>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?)
}

pub async fn next_sort_order(pool: &SqlitePool) -> AppResult<i64> {
    let max: Option<i64> = sqlx::query_scalar("SELECT MAX(sort_order) FROM proxy_nodes").fetch_one(pool).await?;
    Ok(max.map_or(0, |m| m + 1))
}

pub async fn upsert_node(pool: &SqlitePool, row: &NodeRow) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO proxy_nodes (id, name, scheme, host, port, username, credential_ref, enabled, sort_order, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, scheme = excluded.scheme, host = excluded.host, \
             port = excluded.port, username = excluded.username, credential_ref = excluded.credential_ref, \
             enabled = excluded.enabled, updated_at = excluded.updated_at",
    )
    .bind(&row.id)
    .bind(&row.name)
    .bind(&row.scheme)
    .bind(&row.host)
    .bind(row.port)
    .bind(row.username.as_deref())
    .bind(row.credential_ref.as_deref())
    .bind(row.enabled)
    .bind(row.sort_order)
    .bind(&row.created_at)
    .bind(&row.updated_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_enabled(pool: &SqlitePool, id: &str, enabled: bool, now: &str) -> AppResult<bool> {
    let r = sqlx::query("UPDATE proxy_nodes SET enabled = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(id)
        .bind(enabled)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn delete_node(pool: &SqlitePool, id: &str) -> AppResult<bool> {
    Ok(sqlx::query("DELETE FROM proxy_nodes WHERE id = ?1").bind(id).execute(pool).await?.rows_affected() > 0)
}

pub async fn load_config(pool: &SqlitePool) -> AppResult<PoolConfig> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_settings WHERE key = ?1").bind(CONFIG_KEY).fetch_optional(pool).await?;
    // 损坏的配置回落到默认（默认是关闭状态，不会让流量意外走代理）
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

pub async fn save_config(pool: &SqlitePool, config: &PoolConfig) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(CONFIG_KEY)
    .bind(serde_json::to_string(config)?)
    .execute(pool)
    .await?;
    Ok(())
}

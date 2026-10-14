//! ApiKey 表 repo。库里只有 SHA-256 哈希与展示用后缀，没有完整密钥。

use sqlx::{FromRow, SqlitePool};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub key_hash: String,
    pub key_suffix: String,
    pub key_secret: Option<String>,
    pub protocols_json: String,
    pub allowed_models_json: String,
    pub rpm_limit: Option<i64>,
    pub tpm_limit: Option<i64>,
    pub total_limit: Option<i64>,
    pub call_count: i64,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub disabled_at: Option<String>,
}

const COLS: &str = "id, name, key_hash, key_suffix, key_secret, protocols_json, allowed_models_json, rpm_limit, tpm_limit, \
     total_limit, call_count, created_at, last_used_at, expires_at, disabled_at";

pub async fn insert(pool: &SqlitePool, row: &ApiKeyRow) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_suffix, key_secret, protocols_json, allowed_models_json, rpm_limit, \
                               tpm_limit, total_limit, call_count, created_at, last_used_at, expires_at, disabled_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )
    .bind(&row.id)
    .bind(&row.name)
    .bind(&row.key_hash)
    .bind(&row.key_suffix)
    .bind(row.key_secret.as_deref())
    .bind(&row.protocols_json)
    .bind(&row.allowed_models_json)
    .bind(row.rpm_limit)
    .bind(row.tpm_limit)
    .bind(row.total_limit)
    .bind(row.call_count)
    .bind(&row.created_at)
    .bind(row.last_used_at.as_deref())
    .bind(row.expires_at.as_deref())
    .bind(row.disabled_at.as_deref())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list(pool: &SqlitePool) -> AppResult<Vec<ApiKeyRow>> {
    let sql = format!("SELECT {COLS} FROM api_keys ORDER BY created_at DESC, id");
    Ok(sqlx::query_as::<_, ApiKeyRow>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?)
}

pub async fn get(pool: &SqlitePool, id: &str) -> AppResult<Option<ApiKeyRow>> {
    let sql = format!("SELECT {COLS} FROM api_keys WHERE id = ?1");
    Ok(sqlx::query_as::<_, ApiKeyRow>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?)
}

pub async fn replace_secret(pool: &SqlitePool, id: &str, key_hash: &str, key_suffix: &str, key_secret: Option<&str>) -> AppResult<()> {
    let r = sqlx::query("UPDATE api_keys SET key_hash = ?2, key_suffix = ?3, key_secret = ?4 WHERE id = ?1")
        .bind(id)
        .bind(key_hash)
        .bind(key_suffix)
        .bind(key_secret)
        .execute(pool)
        .await?;
    ensure_found(r.rows_affected(), id)
}

pub async fn set_disabled_at(pool: &SqlitePool, id: &str, disabled_at: Option<&str>) -> AppResult<()> {
    let r = sqlx::query("UPDATE api_keys SET disabled_at = ?2 WHERE id = ?1")
        .bind(id)
        .bind(disabled_at)
        .execute(pool)
        .await?;
    ensure_found(r.rows_affected(), id)
}

pub async fn delete(pool: &SqlitePool, id: &str) -> AppResult<()> {
    let r = sqlx::query("DELETE FROM api_keys WHERE id = ?1").bind(id).execute(pool).await?;
    ensure_found(r.rows_affected(), id)
}

/// 校验通过后记录使用：last_used_at 与 call_count 自增（单条原子语句）。
pub async fn touch(pool: &SqlitePool, id: &str, now: &str) -> AppResult<()> {
    sqlx::query("UPDATE api_keys SET last_used_at = ?2, call_count = call_count + 1 WHERE id = ?1")
        .bind(id)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

fn ensure_found(affected: u64, id: &str) -> AppResult<()> {
    if affected == 0 {
        Err(AppError::NotFound(format!("api key {id}")))
    } else {
        Ok(())
    }
}

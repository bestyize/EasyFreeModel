//! Provider / Connection / Model / 自定义请求头 的 repo。

use std::collections::HashMap;

use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, FromRow)]
pub struct ProviderRow {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub grp: String,
    pub adapter_ready: bool,
    pub protocols_json: String,
    pub risk_notice: Option<String>,
    pub enabled: bool,
    pub round_robin: bool,
    pub max_concurrency: i64,
    pub max_retries: i64,
    pub thinking_level: String,
    pub adapter: String,
    pub service_kinds_json: String,
    pub website: Option<String>,
    pub color: Option<String>,
    pub text_icon: Option<String>,
    pub auth_modes_json: String,
    pub pending_reason: Option<String>,
    pub base_url: Option<String>,
}

const PROVIDER_COLS: &str = "id, kind, display_name, grp, adapter_ready, protocols_json, risk_notice, \
     enabled, round_robin, max_concurrency, max_retries, thinking_level, adapter, service_kinds_json, \
     website, color, text_icon, auth_modes_json, pending_reason, base_url";

#[derive(Debug, Clone, FromRow)]
pub struct ConnectionRow {
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

const CONNECTION_COLS: &str = "id, provider_id, account_label, kind, status, priority, weight, enabled, credential_ref";

#[derive(Debug, Clone, FromRow)]
pub struct ModelRow {
    pub id: String,
    pub provider_id: String,
    pub upstream_model_id: String,
    pub public_alias: String,
    pub description: String,
    pub supports_thinking: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct HeaderRow {
    pub header_key: String,
    pub value: Option<String>,
    pub secret: bool,
    pub credential_ref: Option<String>,
}

/// 目录条目写入参数（均已规范化）。
pub struct CatalogWrite<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub grp: &'a str,
    pub adapter: &'a str,
    pub adapter_ready: bool,
    pub protocols_json: String,
    pub risk_notice: Option<&'a str>,
    pub sort_order: i64,
    pub category: &'a str,
    pub service_kinds_json: String,
    pub website: Option<&'a str>,
    pub color: Option<&'a str>,
    pub text_icon: Option<&'a str>,
    pub auth_modes_json: String,
    pub pending_reason: Option<&'a str>,
    pub base_url: Option<&'a str>,
}

/// 幂等写入一条内置目录项。已存在时只刷新目录字段，不触碰用户数据
/// （enabled / 分发策略 / thinking_level / 连接 / 模型 / 请求头）。
/// 与同 id 的自定义提供商冲突时不覆盖它，返回 false。
pub async fn upsert_catalog_entry(tx: &mut Transaction<'_, Sqlite>, w: &CatalogWrite<'_>, now: &str) -> AppResult<bool> {
    let r = sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, protocols_json, risk_notice, \
                                sort_order, created_at, updated_at, category, service_kinds_json, website, color, \
                                text_icon, auth_modes_json, pending_reason, base_url) \
         VALUES (?1, 'builtin', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
         ON CONFLICT(id) DO UPDATE SET \
            display_name = excluded.display_name, grp = excluded.grp, adapter = excluded.adapter, \
            adapter_ready = excluded.adapter_ready, protocols_json = excluded.protocols_json, \
            risk_notice = excluded.risk_notice, sort_order = excluded.sort_order, category = excluded.category, \
            service_kinds_json = excluded.service_kinds_json, website = excluded.website, color = excluded.color, \
            text_icon = excluded.text_icon, auth_modes_json = excluded.auth_modes_json, \
            pending_reason = excluded.pending_reason, base_url = excluded.base_url \
         WHERE providers.kind = 'builtin'",
    )
    .bind(w.id)
    .bind(w.name)
    .bind(w.grp)
    .bind(w.adapter)
    .bind(w.adapter_ready)
    .bind(&w.protocols_json)
    .bind(w.risk_notice)
    .bind(w.sort_order)
    .bind(now)
    .bind(w.category)
    .bind(&w.service_kinds_json)
    .bind(w.website)
    .bind(w.color)
    .bind(w.text_icon)
    .bind(&w.auth_modes_json)
    .bind(w.pending_reason)
    .bind(w.base_url)
    .execute(&mut **tx)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// 自定义 → OAuth → 免费 → API Key；同组内按 sort_order、创建时间。
pub async fn list_providers(pool: &SqlitePool) -> AppResult<Vec<ProviderRow>> {
    let sql = format!(
        "SELECT {PROVIDER_COLS} FROM providers \
         ORDER BY CASE grp WHEN 'custom' THEN 0 WHEN 'oauth' THEN 1 WHEN 'free' THEN 2 ELSE 3 END, \
                  sort_order, created_at, id"
    );
    Ok(sqlx::query_as::<_, ProviderRow>(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await?)
}

pub async fn get_provider(pool: &SqlitePool, id: &str) -> AppResult<Option<ProviderRow>> {
    let sql = format!("SELECT {PROVIDER_COLS} FROM providers WHERE id = ?1");
    Ok(sqlx::query_as::<_, ProviderRow>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?)
}

/// provider_id → (连接总数, 其中 enabled 且 status=active 的数量)
pub async fn connection_counts(pool: &SqlitePool) -> AppResult<HashMap<String, (i64, i64)>> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT provider_id, COUNT(*), COALESCE(SUM(CASE WHEN enabled = 1 AND status = 'active' THEN 1 ELSE 0 END), 0) \
         FROM connections GROUP BY provider_id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(p, t, a)| (p, (t, a))).collect())
}

pub async fn list_connections(pool: &SqlitePool, provider_id: &str) -> AppResult<Vec<ConnectionRow>> {
    let sql = format!(
        "SELECT {CONNECTION_COLS} FROM connections WHERE provider_id = ?1 ORDER BY priority, created_at, id"
    );
    Ok(sqlx::query_as::<_, ConnectionRow>(sqlx::AssertSqlSafe(sql)).bind(provider_id).fetch_all(pool).await?)
}

pub async fn get_connection(pool: &SqlitePool, id: &str) -> AppResult<Option<ConnectionRow>> {
    let sql = format!("SELECT {CONNECTION_COLS} FROM connections WHERE id = ?1");
    Ok(sqlx::query_as::<_, ConnectionRow>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?)
}

pub async fn list_models(pool: &SqlitePool, provider_id: &str) -> AppResult<Vec<ModelRow>> {
    Ok(sqlx::query_as::<_, ModelRow>(
        "SELECT id, provider_id, upstream_model_id, public_alias, description, supports_thinking, enabled \
         FROM models WHERE provider_id = ?1 ORDER BY position, id",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

pub async fn list_headers(pool: &SqlitePool, provider_id: &str) -> AppResult<Vec<HeaderRow>> {
    Ok(sqlx::query_as::<_, HeaderRow>(
        "SELECT header_key, value, secret, credential_ref FROM provider_headers \
         WHERE provider_id = ?1 ORDER BY position, header_key",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?)
}

/// 所有"已启用"模型的别名及所属 provider，用于别名冲突检测。
pub async fn enabled_aliases(pool: &SqlitePool) -> AppResult<Vec<(String, String, String)>> {
    Ok(sqlx::query_as("SELECT public_alias, provider_id, id FROM models WHERE enabled = 1").fetch_all(pool).await?)
}

/// id → provider_id，用于检测客户端提供的 id 是否撞了别的 provider。
pub async fn connection_owner(pool: &SqlitePool, id: &str) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT provider_id FROM connections WHERE id = ?1").bind(id).fetch_optional(pool).await?)
}

pub async fn model_owner(pool: &SqlitePool, id: &str) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT provider_id FROM models WHERE id = ?1").bind(id).fetch_optional(pool).await?)
}

pub async fn set_connection_credential_ref(pool: &SqlitePool, id: &str, credential_ref: Option<&str>) -> AppResult<()> {
    let r = sqlx::query("UPDATE connections SET credential_ref = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(id)
        .bind(credential_ref)
        .bind(super::now_ts())
        .execute(pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("connection {id}")));
    }
    Ok(())
}

// ---- 保存（单事务） ----

pub struct ProviderWrite {
    pub id: String,
    /// 仅自定义提供商允许改名/改协议
    pub display_name: Option<String>,
    pub protocols_json: Option<String>,
    /// 改协议时同步的适配器名（与 protocols_json 同时给出）
    pub adapter: Option<&'static str>,
    pub round_robin: bool,
    pub max_concurrency: i64,
    pub max_retries: i64,
    pub thinking_level: &'static str,
}

pub struct ConnectionWrite {
    pub id: String,
    pub account_label: String,
    pub kind: &'static str,
    pub status: &'static str,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
}

pub struct ModelWrite {
    pub id: String,
    pub upstream_model_id: String,
    pub public_alias: String,
    pub description: String,
    pub supports_thinking: bool,
    pub enabled: bool,
}

pub struct HeaderWrite {
    pub key: String,
    pub value: Option<String>,
    pub secret: bool,
    pub credential_ref: Option<String>,
}

pub struct SavePlan {
    pub provider: ProviderWrite,
    pub connections: Vec<ConnectionWrite>,
    pub models: Vec<ModelWrite>,
    pub headers: Vec<HeaderWrite>,
}

/// 在一个事务内整体替换 provider 的连接/模型/请求头。返回被移除对象的 credential_ref，
/// 由调用方在提交成功后清理凭据库。
pub async fn apply_save(pool: &SqlitePool, plan: &SavePlan) -> AppResult<Vec<String>> {
    let now = super::now_ts();
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;
    let p = &plan.provider;

    sqlx::query(
        "UPDATE providers SET round_robin = ?2, max_concurrency = ?3, max_retries = ?4, thinking_level = ?5, \
                display_name = COALESCE(?6, display_name), protocols_json = COALESCE(?7, protocols_json), \
                adapter = COALESCE(?8, adapter), updated_at = ?9 WHERE id = ?1",
    )
    .bind(&p.id)
    .bind(p.round_robin)
    .bind(p.max_concurrency)
    .bind(p.max_retries)
    .bind(p.thinking_level)
    .bind(p.display_name.as_deref())
    .bind(p.protocols_json.as_deref())
    .bind(p.adapter)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    let mut removed_refs: Vec<String> = Vec::new();

    // 连接：删除不在列表里的，其余 upsert；credential_ref 永远不被此处覆盖
    let keep_ids: Vec<&str> = plan.connections.iter().map(|c| c.id.as_str()).collect();
    let existing: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, credential_ref FROM connections WHERE provider_id = ?1")
            .bind(&p.id)
            .fetch_all(&mut *tx)
            .await?;
    for (id, cref) in existing {
        if !keep_ids.contains(&id.as_str()) {
            sqlx::query("DELETE FROM connections WHERE id = ?1").bind(&id).execute(&mut *tx).await?;
            removed_refs.extend(cref);
        }
    }
    for c in &plan.connections {
        sqlx::query(
            "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, \
                                      created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
             ON CONFLICT(id) DO UPDATE SET account_label = excluded.account_label, kind = excluded.kind, \
                status = excluded.status, priority = excluded.priority, weight = excluded.weight, \
                enabled = excluded.enabled, updated_at = excluded.updated_at \
             WHERE connections.provider_id = excluded.provider_id",
        )
        .bind(&c.id)
        .bind(&p.id)
        .bind(&c.account_label)
        .bind(c.kind)
        .bind(c.status)
        .bind(c.priority)
        .bind(c.weight)
        .bind(c.enabled)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    }

    // 模型：整体替换
    sqlx::query("DELETE FROM models WHERE provider_id = ?1").bind(&p.id).execute(&mut *tx).await?;
    for (pos, m) in plan.models.iter().enumerate() {
        sqlx::query(
            "INSERT INTO models (id, provider_id, upstream_model_id, public_alias, description, supports_thinking, \
                                 enabled, position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(&m.id)
        .bind(&p.id)
        .bind(&m.upstream_model_id)
        .bind(&m.public_alias)
        .bind(&m.description)
        .bind(m.supports_thinking)
        .bind(m.enabled)
        .bind(pos as i64)
        .execute(&mut *tx)
        .await?;
    }

    // 请求头：整体替换；被丢弃的秘密头凭据交给调用方清理
    let old_headers: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT header_key, credential_ref FROM provider_headers WHERE provider_id = ?1")
            .bind(&p.id)
            .fetch_all(&mut *tx)
            .await?;
    let new_refs: Vec<&str> = plan.headers.iter().filter_map(|h| h.credential_ref.as_deref()).collect();
    for (_, cref) in old_headers {
        if let Some(r) = cref {
            if !new_refs.contains(&r.as_str()) {
                removed_refs.push(r);
            }
        }
    }
    sqlx::query("DELETE FROM provider_headers WHERE provider_id = ?1").bind(&p.id).execute(&mut *tx).await?;
    for (pos, h) in plan.headers.iter().enumerate() {
        sqlx::query(
            "INSERT INTO provider_headers (provider_id, header_key, value, secret, credential_ref, position) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(&p.id)
        .bind(&h.key)
        .bind(h.value.as_deref())
        .bind(h.secret)
        .bind(h.credential_ref.as_deref())
        .bind(pos as i64)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(removed_refs)
}

/// 连接启停（配额页的账户开关）。同步 status：停用 → paused，启用 → active（error 状态保持）。
pub async fn set_connection_enabled(pool: &SqlitePool, id: &str, enabled: bool) -> AppResult<()> {
    let r = sqlx::query(
        "UPDATE connections SET enabled = ?2, \
            status = CASE WHEN ?2 = 0 THEN 'paused' WHEN status = 'paused' THEN 'active' ELSE status END, \
            updated_at = ?3 WHERE id = ?1",
    )
    .bind(id)
    .bind(enabled)
    .bind(super::now_ts())
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("connection {id}")));
    }
    Ok(())
}

// ---- 自定义提供商 / 连接的创建与删除 ----

pub struct CustomProviderInsert<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub protocols_json: String,
    pub adapter: &'a str,
    pub base_url: &'a str,
}

/// 新建自定义提供商：适配器随协议确定，创建即就绪（Base URL 已校验）。
pub async fn insert_custom_provider(pool: &SqlitePool, w: &CustomProviderInsert<'_>) -> AppResult<()> {
    let now = super::now_ts();
    sqlx::query(
        "INSERT INTO providers (id, kind, display_name, grp, adapter, adapter_ready, protocols_json, sort_order, \
                                category, service_kinds_json, auth_modes_json, base_url, created_at, updated_at) \
         VALUES (?1, 'custom', ?2, 'custom', ?3, 1, ?4, 0, 'custom', '[\"llm\"]', '[\"apiKey\"]', ?5, ?6, ?6)",
    )
    .bind(w.id)
    .bind(w.name)
    .bind(w.adapter)
    .bind(&w.protocols_json)
    .bind(w.base_url)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// 在一个事务内删除提供商及其连接/模型/请求头，返回需要清理的全部 credential_ref。
/// 用量与媒体历史是历史事实，保留。
pub async fn delete_provider(pool: &SqlitePool, id: &str) -> AppResult<Vec<String>> {
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;
    let mut refs: Vec<String> = sqlx::query_scalar(
        "SELECT credential_ref FROM connections WHERE provider_id = ?1 AND credential_ref IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let header_refs: Vec<String> = sqlx::query_scalar(
        "SELECT credential_ref FROM provider_headers WHERE provider_id = ?1 AND credential_ref IS NOT NULL",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    refs.extend(header_refs);
    sqlx::query("DELETE FROM connections WHERE provider_id = ?1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM models WHERE provider_id = ?1").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM provider_headers WHERE provider_id = ?1").bind(id).execute(&mut *tx).await?;
    let r = sqlx::query("DELETE FROM providers WHERE id = ?1").bind(id).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("provider {id}")));
    }
    tx.commit().await?;
    Ok(refs)
}

/// 新增一条 API Key 连接（凭据已先写入系统凭据库，这里只落 credential_ref）。
/// priority 取现有最大值 + 1，排在最后。
pub async fn insert_api_key_connection(
    pool: &SqlitePool,
    id: &str,
    provider_id: &str,
    account_label: &str,
    credential_ref: &str,
) -> AppResult<ConnectionRow> {
    let now = super::now_ts();
    sqlx::query(
        "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, \
                                  credential_ref, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'apiKey', 'active', \
                 (SELECT COALESCE(MAX(priority), 0) + 1 FROM connections WHERE provider_id = ?2), 1, 1, ?4, ?5, ?5)",
    )
    .bind(id)
    .bind(provider_id)
    .bind(account_label)
    .bind(credential_ref)
    .bind(&now)
    .execute(pool)
    .await?;
    get_connection(pool, id).await?.ok_or_else(|| AppError::NotFound(format!("connection {id}")))
}

/// 新增一条 OAuth 连接（令牌已先写入系统凭据库，这里只落 credential_ref）。priority 排在最后。
pub async fn insert_oauth_connection(
    pool: &SqlitePool,
    id: &str,
    provider_id: &str,
    account_label: &str,
    credential_ref: &str,
) -> AppResult<ConnectionRow> {
    let now = super::now_ts();
    sqlx::query(
        "INSERT INTO connections (id, provider_id, account_label, kind, status, priority, weight, enabled, \
                                  credential_ref, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'oauth', 'active', \
                 (SELECT COALESCE(MAX(priority), 0) + 1 FROM connections WHERE provider_id = ?2), 1, 1, ?4, ?5, ?5)",
    )
    .bind(id)
    .bind(provider_id)
    .bind(account_label)
    .bind(credential_ref)
    .bind(&now)
    .execute(pool)
    .await?;
    get_connection(pool, id).await?.ok_or_else(|| AppError::NotFound(format!("connection {id}")))
}

/// 重新授权成功：令牌已原位更新，连接从 needsReauth / error 恢复为 active（用户手动暂停的 paused 保持不变）。
pub async fn mark_connection_reauthorized(pool: &SqlitePool, id: &str, credential_ref: &str) -> AppResult<()> {
    let r = sqlx::query(
        "UPDATE connections SET credential_ref = ?2, \
            status = CASE WHEN status IN ('needsReauth', 'error') THEN 'active' ELSE status END, \
            updated_at = ?3 WHERE id = ?1",
    )
    .bind(id)
    .bind(credential_ref)
    .bind(super::now_ts())
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("connection {id}")));
    }
    Ok(())
}

/// 删除单个连接（其配额快照随外键级联删除；用量历史保留）。返回被删除连接的 credential_ref，
/// 由调用方在提交成功后清理凭据库。
pub async fn delete_connection(pool: &SqlitePool, id: &str) -> AppResult<Option<String>> {
    let mut tx: Transaction<'_, Sqlite> = pool.begin().await?;
    let cref: Option<Option<String>> = sqlx::query_scalar("SELECT credential_ref FROM connections WHERE id = ?1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(cref) = cref else {
        return Err(AppError::NotFound(format!("connection {id}")));
    };
    sqlx::query("DELETE FROM connections WHERE id = ?1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(cref)
}

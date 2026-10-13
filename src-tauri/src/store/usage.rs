//! UsageRequest 表 repo：写入与聚合查询。时间戳均为 `store::ts` 格式的 UTC 字符串。

use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};

use crate::error::AppResult;
use crate::models::{TokenSource, UsageRequest, UsageStatus};

#[derive(Debug, Clone)]
pub struct NewUsage {
    pub id: String,
    pub api_key_id: Option<String>,
    pub provider_id: String,
    pub connection_id: String,
    pub model_id: String,
    pub protocol: String,
    pub status: UsageStatus,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub estimated_cost: f64,
    pub duration_ms: i64,
    pub token_source: TokenSource,
    pub created_at: String,
}

pub async fn insert(pool: &SqlitePool, u: &NewUsage) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO usage_requests (id, api_key_id, provider_id, connection_id, model_id, protocol, status, \
            input_tokens, output_tokens, cached_tokens, estimated_cost, duration_ms, token_source, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )
    .bind(&u.id)
    .bind(u.api_key_id.as_deref())
    .bind(&u.provider_id)
    .bind(&u.connection_id)
    .bind(&u.model_id)
    .bind(&u.protocol)
    .bind(u.status.as_str())
    .bind(u.input_tokens)
    .bind(u.output_tokens)
    .bind(u.cached_tokens)
    .bind(u.estimated_cost)
    .bind(u.duration_ms)
    .bind(u.token_source.as_str())
    .bind(&u.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, FromRow)]
pub struct Totals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
}

/// 区间 [start, end)；任一端为 None 表示不限制。
pub async fn totals(pool: &SqlitePool, start: Option<&str>, end: Option<&str>) -> AppResult<Totals> {
    Ok(sqlx::query_as::<_, Totals>(
        "SELECT COUNT(*) AS requests, \
                COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                COALESCE(SUM(cached_tokens), 0) AS cached_tokens, \
                CAST(COALESCE(SUM(estimated_cost), 0.0) AS REAL) AS cost \
         FROM usage_requests \
         WHERE (?1 IS NULL OR created_at >= ?1) AND (?2 IS NULL OR created_at < ?2)",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?)
}

#[derive(Debug, Clone, FromRow)]
pub struct BucketRow {
    pub bucket: String,
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
}

/// 按本地时区分桶：`strftime(fmt, created_at, tz_modifier)`，tz_modifier 形如 "+28800 seconds"。
pub async fn buckets(pool: &SqlitePool, start: Option<&str>, fmt: &str, tz_modifier: &str) -> AppResult<Vec<BucketRow>> {
    Ok(sqlx::query_as::<_, BucketRow>(
        "SELECT strftime(?2, created_at, ?3) AS bucket, COUNT(*) AS requests, \
                COALESCE(SUM(input_tokens), 0) AS input_tokens, \
                COALESCE(SUM(output_tokens), 0) AS output_tokens, \
                COALESCE(SUM(cached_tokens), 0) AS cached_tokens, \
                CAST(COALESCE(SUM(estimated_cost), 0.0) AS REAL) AS cost \
         FROM usage_requests WHERE (?1 IS NULL OR created_at >= ?1) GROUP BY bucket",
    )
    .bind(start)
    .bind(fmt)
    .bind(tz_modifier)
    .fetch_all(pool)
    .await?)
}

#[derive(Debug, Clone, FromRow)]
pub struct ProviderAgg {
    pub provider_id: String,
    pub name: String,
    pub requests: i64,
    pub tokens: i64,
}

/// tokens = input + output（缓存 Token 单独统计，不与输入相加）
pub async fn by_provider(pool: &SqlitePool, start: Option<&str>) -> AppResult<Vec<ProviderAgg>> {
    Ok(sqlx::query_as::<_, ProviderAgg>(
        "SELECT u.provider_id AS provider_id, COALESCE(p.display_name, u.provider_id) AS name, \
                COUNT(*) AS requests, COALESCE(SUM(u.input_tokens + u.output_tokens), 0) AS tokens \
         FROM usage_requests u LEFT JOIN providers p ON p.id = u.provider_id \
         WHERE (?1 IS NULL OR u.created_at >= ?1) \
         GROUP BY u.provider_id ORDER BY tokens DESC, requests DESC, u.provider_id",
    )
    .bind(start)
    .fetch_all(pool)
    .await?)
}

#[derive(Debug, Clone, FromRow)]
pub struct ModelAgg {
    pub model: String,
    pub provider_id: String,
    pub tokens: i64,
}

pub async fn by_model(pool: &SqlitePool, start: Option<&str>, limit: i64) -> AppResult<Vec<ModelAgg>> {
    Ok(sqlx::query_as::<_, ModelAgg>(
        "SELECT model_id AS model, provider_id, COALESCE(SUM(input_tokens + output_tokens), 0) AS tokens \
         FROM usage_requests WHERE (?1 IS NULL OR created_at >= ?1) \
         GROUP BY model_id, provider_id ORDER BY tokens DESC, model_id LIMIT ?2",
    )
    .bind(start)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn first_timestamp(pool: &SqlitePool) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar("SELECT MIN(created_at) FROM usage_requests").fetch_one(pool).await?)
}

#[derive(Debug, Clone, Copy, FromRow)]
pub struct Stats {
    pub requests: i64,
    pub successes: i64,
    pub avg_latency_ms: Option<f64>,
}

pub async fn stats_since(pool: &SqlitePool, start: &str) -> AppResult<Stats> {
    Ok(sqlx::query_as::<_, Stats>(
        "SELECT COUNT(*) AS requests, COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0) AS successes, \
                AVG(duration_ms) AS avg_latency_ms FROM usage_requests WHERE created_at >= ?1",
    )
    .bind(start)
    .fetch_one(pool)
    .await?)
}

#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub start: Option<String>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub status: Option<UsageStatus>,
    pub protocol: Option<String>,
    pub text: Option<String>,
    pub limit: i64,
}

#[derive(Debug, FromRow)]
struct ListRow {
    id: String,
    provider_id: String,
    connection_id: String,
    model_id: String,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    duration_ms: i64,
    status: String,
    estimated_cost: f64,
    token_source: String,
    created_at: String,
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub async fn list(pool: &SqlitePool, q: &ListQuery) -> AppResult<Vec<UsageRequest>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT id, provider_id, connection_id, model_id, input_tokens, output_tokens, cached_tokens, duration_ms, \
         status, estimated_cost, token_source, created_at FROM usage_requests WHERE 1 = 1",
    );
    if let Some(s) = &q.start {
        qb.push(" AND created_at >= ").push_bind(s.clone());
    }
    if let Some(p) = &q.provider_id {
        qb.push(" AND provider_id = ").push_bind(p.clone());
    }
    if let Some(m) = &q.model {
        qb.push(" AND model_id = ").push_bind(m.clone());
    }
    if let Some(s) = q.status {
        qb.push(" AND status = ").push_bind(s.as_str());
    }
    if let Some(p) = &q.protocol {
        qb.push(" AND protocol = ").push_bind(p.clone());
    }
    if let Some(t) = q.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let like = format!("%{}%", escape_like(t));
        qb.push(" AND (id LIKE ").push_bind(like.clone());
        qb.push(" ESCAPE '\\' OR model_id LIKE ").push_bind(like.clone());
        qb.push(" ESCAPE '\\' OR provider_id LIKE ").push_bind(like);
        qb.push(" ESCAPE '\\')");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(q.limit);

    let rows: Vec<ListRow> = qb.build_query_as().fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|r| UsageRequest {
            request_id: r.id,
            provider_id: r.provider_id,
            account_id: r.connection_id,
            model: r.model_id,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cached_tokens: r.cached_tokens,
            latency_ms: r.duration_ms,
            status: UsageStatus::parse(&r.status).unwrap_or(UsageStatus::Error),
            estimated_cost: r.estimated_cost,
            token_source: TokenSource::parse(&r.token_source).unwrap_or(TokenSource::Estimated),
            timestamp: r.created_at,
        })
        .collect())
}

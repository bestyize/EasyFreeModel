//! MediaJob 表 repo。只存元数据与输出文件路径，不存大文件/base64。

use sqlx::{FromRow, SqlitePool};

use crate::error::AppResult;
use crate::models::{JobStatus, MediaJob, MediaKind};

#[derive(Debug, Clone, FromRow)]
struct Row {
    id: String,
    kind: String,
    provider_id: String,
    model_id: String,
    summary: String,
    output_path: Option<String>,
    status: String,
    error: Option<String>,
    created_at: String,
    finished_at: Option<String>,
}

const COLS: &str = "id, kind, provider_id, model_id, summary, output_path, status, error, created_at, finished_at";

impl Row {
    fn into_job(self) -> MediaJob {
        MediaJob {
            id: self.id,
            kind: MediaKind::parse(&self.kind).unwrap_or(MediaKind::Image),
            provider_id: self.provider_id,
            model: self.model_id,
            status: JobStatus::parse(&self.status).unwrap_or(JobStatus::Failed),
            summary: self.summary,
            output_path: self.output_path,
            error: self.error,
            created_at: self.created_at,
            finished_at: self.finished_at,
        }
    }
}

pub struct NewJob<'a> {
    pub id: &'a str,
    pub kind: MediaKind,
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub summary: &'a str,
    pub input_metadata_json: &'a str,
    pub status: JobStatus,
    pub error: Option<&'a str>,
    pub created_at: &'a str,
    pub finished_at: Option<&'a str>,
}

pub async fn insert(pool: &SqlitePool, j: &NewJob<'_>) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO media_jobs (id, kind, provider_id, model_id, summary, input_metadata_json, status, error, \
                                 created_at, finished_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(j.id)
    .bind(j.kind.as_str())
    .bind(j.provider_id)
    .bind(j.model_id)
    .bind(j.summary)
    .bind(j.input_metadata_json)
    .bind(j.status.as_str())
    .bind(j.error)
    .bind(j.created_at)
    .bind(j.finished_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list(pool: &SqlitePool, kind: MediaKind, limit: i64) -> AppResult<Vec<MediaJob>> {
    let sql = format!("SELECT {COLS} FROM media_jobs WHERE kind = ?1 ORDER BY created_at DESC, id LIMIT ?2");
    let rows = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql)).bind(kind.as_str()).bind(limit).fetch_all(pool).await?;
    Ok(rows.into_iter().map(Row::into_job).collect())
}

pub async fn get(pool: &SqlitePool, id: &str) -> AppResult<Option<MediaJob>> {
    let sql = format!("SELECT {COLS} FROM media_jobs WHERE id = ?1");
    Ok(sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(pool).await?.map(Row::into_job))
}

/// 仅当任务仍处于 queued/running 时才改为 cancelled；返回是否发生了变更。
pub async fn cancel_if_active(pool: &SqlitePool, id: &str, now: &str) -> AppResult<bool> {
    let r = sqlx::query(
        "UPDATE media_jobs SET status = 'cancelled', finished_at = ?2 \
         WHERE id = ?1 AND status IN ('queued', 'running')",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() > 0)
}

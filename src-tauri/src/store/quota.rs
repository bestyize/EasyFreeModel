//! QuotaSnapshot 表 repo。

use sqlx::{FromRow, SqlitePool};

use crate::error::AppResult;
use crate::models::QuotaSource;

#[derive(Debug, Clone, FromRow)]
pub struct AccountRow {
    pub connection_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub account_label: String,
    pub enabled: bool,
}

/// 所有连接（即所有"配额账户"），按提供商顺序排列。
pub async fn list_accounts(pool: &SqlitePool) -> AppResult<Vec<AccountRow>> {
    Ok(sqlx::query_as::<_, AccountRow>(
        "SELECT c.id AS connection_id, c.provider_id AS provider_id, p.display_name AS provider_name, \
                c.account_label AS account_label, c.enabled AS enabled \
         FROM connections c JOIN providers p ON p.id = c.provider_id \
         ORDER BY CASE p.grp WHEN 'custom' THEN 0 WHEN 'oauth' THEN 1 ELSE 2 END, p.sort_order, c.priority, \
                  c.created_at, c.id",
    )
    .fetch_all(pool)
    .await?)
}

#[derive(Debug, Clone, FromRow)]
pub struct SnapshotRow {
    pub connection_id: String,
    pub kind: String,
    pub quota_name: String,
    pub unit: String,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub resets_at: Option<String>,
    pub checked_at: String,
    pub source: String,
    pub error: Option<String>,
}

/// 每个 (连接, 种类, 配额名) 的最新一条快照。
pub async fn latest_snapshots(pool: &SqlitePool) -> AppResult<Vec<SnapshotRow>> {
    Ok(sqlx::query_as::<_, SnapshotRow>(
        "SELECT s.connection_id, s.kind, s.quota_name, s.unit, s.used, s.total, s.remaining, s.resets_at, \
                s.checked_at, s.source, s.error \
         FROM quota_snapshots s \
         JOIN (SELECT connection_id, kind, quota_name, MAX(id) AS max_id FROM quota_snapshots \
               GROUP BY connection_id, kind, quota_name) l ON l.max_id = s.id \
         ORDER BY s.id",
    )
    .fetch_all(pool)
    .await?)
}

/// 保存成功抓取到的一批快照：清空该连接之前的错误，并写入新的配额周期/余额。
pub async fn save_snapshots(
    pool: &SqlitePool,
    connection_id: &str,
    snapshots: &[SnapshotRow],
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    // 成功同步时清除错误快照
    sqlx::query("DELETE FROM quota_snapshots WHERE connection_id = ?1 AND kind = 'error'")
        .bind(connection_id)
        .execute(&mut *tx)
        .await?;
    // 如果有新的配额行，先清除旧的 period / balance 快照以保证最新状态一致
    if !snapshots.is_empty() {
        sqlx::query("DELETE FROM quota_snapshots WHERE connection_id = ?1 AND kind IN ('period', 'balance')")
            .bind(connection_id)
            .execute(&mut *tx)
            .await?;
        for s in snapshots {
            sqlx::query(
                "INSERT INTO quota_snapshots (connection_id, kind, quota_name, unit, used, total, remaining, resets_at, checked_at, source, error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .bind(&s.connection_id)
            .bind(&s.kind)
            .bind(&s.quota_name)
            .bind(&s.unit)
            .bind(s.used)
            .bind(s.total)
            .bind(s.remaining)
            .bind(s.resets_at.as_deref())
            .bind(&s.checked_at)
            .bind(&s.source)
            .bind(s.error.as_deref())
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// 记录一次刷新失败。同一连接只保留最近一条错误快照，避免反复点击刷新使表无限增长。
pub async fn record_error(pool: &SqlitePool, connection_id: &str, error: &str, checked_at: &str) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM quota_snapshots WHERE connection_id = ?1 AND kind = 'error'")
        .bind(connection_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO quota_snapshots (connection_id, kind, quota_name, unit, checked_at, source, error) \
         VALUES (?1, 'error', 'sync', '', ?2, ?3, ?4)",
    )
    .bind(connection_id)
    .bind(checked_at)
    .bind(QuotaSource::Unknown.as_str())
    .bind(error)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
pub async fn insert_period(pool: &SqlitePool, s: &SnapshotRow) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO quota_snapshots (connection_id, kind, quota_name, unit, used, total, remaining, resets_at, \
                                      checked_at, source) VALUES (?1, 'period', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&s.connection_id)
    .bind(&s.quota_name)
    .bind(&s.unit)
    .bind(s.used)
    .bind(s.total)
    .bind(s.remaining)
    .bind(s.resets_at.as_deref())
    .bind(&s.checked_at)
    .bind(&s.source)
    .execute(pool)
    .await?;
    Ok(())
}

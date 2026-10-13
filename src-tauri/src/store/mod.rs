//! SQLite 存储层：连接池、迁移与各实体的 repo 函数。命令层不直接写 SQL。

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::error::AppResult;

pub mod api_keys;
pub mod media;
pub mod providers;
pub mod quota;
pub mod settings;
pub mod usage;

/// 迁移内嵌进二进制。追加新迁移文件即可升级，已发布的迁移不得修改。
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// 打开（必要时创建）磁盘数据库并执行迁移。
pub async fn open(path: &Path) -> AppResult<SqlitePool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// 可重复执行：已应用的迁移会被跳过。
pub async fn migrate(pool: &SqlitePool) -> AppResult<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

/// 内存数据库（单连接，避免各连接各自拥有独立内存库）。仅用于测试。
#[cfg(test)]
pub async fn open_memory() -> AppResult<SqlitePool> {
    let opts = SqliteConnectOptions::new().in_memory(true).foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(opts)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

/// 统一时间戳格式（UTC、毫秒、Z 后缀）。字符串字典序即时间序，范围查询依赖这一点。
pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now_ts() -> String {
    ts(Utc::now())
}

/// 解析 ISO-8601/RFC3339 时间，统一转 UTC。
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc))
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_are_repeatable() {
        let pool = open_memory().await.unwrap();
        // 再执行两次不应报错也不应重复建表
        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(applied, MIGRATOR.iter().count() as i64);
    }

    #[tokio::test]
    async fn required_indexes_exist() {
        let pool = open_memory().await.unwrap();
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(names.contains(&"idx_usage_created_provider_model".to_string()));
        assert!(names.contains(&"idx_quota_connection_checked".to_string()));
    }

    #[tokio::test]
    async fn reopen_keeps_accounts() {
        let dir = std::env::temp_dir().join(format!("router-desktop-test-{}", new_id()));
        let path = dir.join("router.db");
        {
            let pool = open(&path).await.unwrap();
            crate::providers::catalog::seed(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO connections (id, provider_id, account_label, kind, created_at, updated_at) \
                 VALUES ('c1', 'openrouter', 'me@example.com', 'apiKey', 't', 't')",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        }
        let pool = open(&path).await.unwrap();
        crate::providers::catalog::seed(&pool).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connections WHERE id = 'c1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1, "重新打开并重复 seed/迁移后账户必须保留");
        pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

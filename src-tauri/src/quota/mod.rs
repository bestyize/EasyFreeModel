//! 配额跟踪：按连接汇总最新快照。
//!
//! 上游额度抓取尚未实现：`refresh` 如实记录一条带 error 的快照，绝不伪造数值。
//! 0 余额与未知余额严格区分——未知一律是 `None`/`null`。

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};
use crate::models::*;
use crate::store::providers as providers_repo;
use crate::store::quota::{self as repo, SnapshotRow};
use crate::store::now_ts;

pub const FETCH_NOT_IMPLEMENTED: &str = "额度抓取尚未实现：该提供商的适配器未接入，暂无法获取额度";

pub async fn list_accounts(pool: &SqlitePool) -> AppResult<Vec<QuotaAccount>> {
    let accounts = repo::list_accounts(pool).await?;
    let mut by_conn: HashMap<String, Vec<SnapshotRow>> = HashMap::new();
    for s in repo::latest_snapshots(pool).await? {
        by_conn.entry(s.connection_id.clone()).or_default().push(s);
    }

    Ok(accounts
        .into_iter()
        .map(|a| {
            let snaps = by_conn.remove(&a.connection_id).unwrap_or_default();
            let periods: Vec<QuotaPeriod> = snaps
                .iter()
                .filter(|s| s.kind == "period")
                .map(|s| QuotaPeriod {
                    name: s.quota_name.clone(),
                    unit: s.unit.clone(),
                    used: s.used,
                    total: s.total,
                    remaining: s.remaining,
                    resets_at: s.resets_at.clone(),
                    source: QuotaSource::parse(&s.source).unwrap_or(QuotaSource::Unknown),
                })
                .collect();
            let balances: Vec<QuotaBalance> = snaps
                .iter()
                .filter(|s| s.kind == "balance")
                .map(|s| QuotaBalance { currency: s.unit.clone(), amount: s.remaining })
                .collect();
            let last_ok = snaps.iter().filter(|s| s.kind != "error").map(|s| s.checked_at.as_str()).max();
            // 只有比最后一次成功更新的错误才展示；成功之后错误自动消失，但最后成功时间始终保留
            let error = snaps
                .iter()
                .filter(|s| s.kind == "error" && last_ok.is_none_or(|ok| s.checked_at.as_str() > ok))
                .max_by(|x, y| x.checked_at.cmp(&y.checked_at))
                .and_then(|s| s.error.clone());
            QuotaAccount {
                connection_id: a.connection_id,
                provider_id: a.provider_id,
                provider_name: a.provider_name,
                account_label: a.account_label,
                enabled: a.enabled,
                periods,
                balances,
                last_synced_at: last_ok.map(str::to_string),
                error,
            }
        })
        .collect())
}

/// 刷新指定连接（None = 全部启用的连接）。返回刷新后的全部配额账户。
pub async fn refresh(pool: &SqlitePool, connection_id: Option<&str>) -> AppResult<Vec<QuotaAccount>> {
    let targets: Vec<String> = match connection_id {
        Some(id) => {
            providers_repo::get_connection(pool, id)
                .await?
                .ok_or_else(|| AppError::NotFound(format!("connection {id}")))?;
            vec![id.to_string()]
        }
        None => repo::list_accounts(pool).await?.into_iter().filter(|a| a.enabled).map(|a| a.connection_id).collect(),
    };
    let checked_at = now_ts();
    for id in targets {
        // 适配器接入后在此按 provider 分发抓取；目前没有任何适配器
        repo::record_error(pool, &id, FETCH_NOT_IMPLEMENTED, &checked_at).await?;
    }
    list_accounts(pool).await
}

pub async fn set_enabled(pool: &SqlitePool, connection_id: &str, enabled: bool) -> AppResult<()> {
    providers_repo::set_connection_enabled(pool, connection_id, enabled).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::catalog;
    use crate::store;

    async fn setup() -> SqlitePool {
        let pool = store::open_memory().await.unwrap();
        catalog::seed(&pool).await.unwrap();
        for (id, provider) in [("c1", "openrouter"), ("c2", "gemini")] {
            sqlx::query(
                "INSERT INTO connections (id, provider_id, account_label, kind, created_at, updated_at) \
                 VALUES (?1, ?2, ?1, 'apiKey', 't', 't')",
            )
            .bind(id)
            .bind(provider)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    fn period(name: &str, used: f64, remaining: f64, checked_at: &str) -> SnapshotRow {
        SnapshotRow {
            connection_id: "c1".into(),
            kind: "period".into(),
            quota_name: name.into(),
            unit: "requests".into(),
            used: Some(used),
            total: Some(10.0),
            remaining: Some(remaining),
            resets_at: None,
            checked_at: checked_at.into(),
            source: "live".into(),
            error: None,
        }
    }

    #[tokio::test]
    async fn empty_snapshots_mean_unknown_not_zero() {
        let pool = setup().await;
        let list = list_accounts(&pool).await.unwrap();
        assert_eq!(list.len(), 2);
        for a in &list {
            assert!(a.periods.is_empty() && a.balances.is_empty());
            assert!(a.last_synced_at.is_none() && a.error.is_none());
        }
    }

    #[tokio::test]
    async fn refresh_reports_honest_error_without_fabricating_numbers() {
        let pool = setup().await;
        let list = refresh(&pool, Some("c1")).await.unwrap();
        let c1 = list.iter().find(|a| a.connection_id == "c1").unwrap();
        assert_eq!(c1.error.as_deref(), Some(FETCH_NOT_IMPLEMENTED));
        assert!(c1.periods.is_empty() && c1.last_synced_at.is_none());
        let c2 = list.iter().find(|a| a.connection_id == "c2").unwrap();
        assert!(c2.error.is_none(), "只刷新了 c1");

        // 重复刷新不会累积错误行
        refresh(&pool, None).await.unwrap();
        refresh(&pool, None).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_snapshots").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 2);

        assert!(matches!(refresh(&pool, Some("nope")).await, Err(AppError::NotFound(_))));
    }

    #[tokio::test]
    async fn last_success_survives_a_later_error_and_zero_stays_zero() {
        let pool = setup().await;
        repo::insert_period(&pool, &period("Weekly", 10.0, 0.0, "2026-10-14T00:00:00.000Z")).await.unwrap();
        repo::record_error(&pool, "c1", "boom", "2026-10-14T01:00:00.000Z").await.unwrap();
        let a = list_accounts(&pool).await.unwrap().into_iter().find(|a| a.connection_id == "c1").unwrap();
        assert_eq!(a.periods[0].remaining, Some(0.0), "0 余额不能被当成未知");
        assert_eq!(a.periods[0].source, QuotaSource::Live);
        assert_eq!(a.last_synced_at.as_deref(), Some("2026-10-14T00:00:00.000Z"));
        assert_eq!(a.error.as_deref(), Some("boom"));

        // 之后又成功一次 → 错误不再展示
        repo::insert_period(&pool, &period("Weekly", 1.0, 9.0, "2026-10-14T02:00:00.000Z")).await.unwrap();
        let a = list_accounts(&pool).await.unwrap().into_iter().find(|a| a.connection_id == "c1").unwrap();
        assert_eq!(a.periods[0].remaining, Some(9.0));
        assert!(a.error.is_none());
    }

    #[tokio::test]
    async fn set_enabled_toggles_connection() {
        let pool = setup().await;
        set_enabled(&pool, "c1", false).await.unwrap();
        let a = list_accounts(&pool).await.unwrap().into_iter().find(|a| a.connection_id == "c1").unwrap();
        assert!(!a.enabled);
        let status: String = sqlx::query_scalar("SELECT status FROM connections WHERE id = 'c1'").fetch_one(&pool).await.unwrap();
        assert_eq!(status, "paused");
        set_enabled(&pool, "c1", true).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM connections WHERE id = 'c1'").fetch_one(&pool).await.unwrap();
        assert_eq!(status, "active");
        assert!(set_enabled(&pool, "nope", true).await.is_err());

        // 停用的账户不参与"全部刷新"
        set_enabled(&pool, "c1", false).await.unwrap();
        refresh(&pool, None).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_snapshots WHERE connection_id = 'c1'").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0);
    }
}

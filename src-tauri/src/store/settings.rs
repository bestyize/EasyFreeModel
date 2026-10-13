//! AppSetting(key, value) 键值表。每个字段单独一行，值为 JSON 编码；缺失的字段回落到默认值。

use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::error::AppResult;
use crate::models::AppSettings;

pub async fn load(pool: &SqlitePool) -> AppResult<AppSettings> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM app_settings").fetch_all(pool).await?;
    let mut map = match serde_json::to_value(AppSettings::default())? {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    for (k, v) in rows {
        // 只覆盖已知字段；库里损坏/过期的值直接忽略，使用默认值
        if map.contains_key(&k) {
            if let Ok(parsed) = serde_json::from_str::<Value>(&v) {
                map.insert(k, parsed);
            }
        }
    }
    // 逐字段回退：整体反序列化失败时尽量保留其余字段
    let defaults = serde_json::to_value(AppSettings::default())?;
    match serde_json::from_value::<AppSettings>(Value::Object(map.clone())) {
        Ok(s) => Ok(s),
        Err(_) => {
            let mut safe = defaults.as_object().cloned().unwrap_or_default();
            for (k, v) in map {
                let mut trial = safe.clone();
                trial.insert(k.clone(), v.clone());
                if serde_json::from_value::<AppSettings>(Value::Object(trial)).is_ok() {
                    safe.insert(k, v);
                }
            }
            Ok(serde_json::from_value(Value::Object(safe))?)
        }
    }
}

pub async fn save(pool: &SqlitePool, settings: &AppSettings) -> AppResult<()> {
    let Value::Object(map) = serde_json::to_value(settings)? else {
        return Ok(());
    };
    let mut tx = pool.begin().await?;
    for (k, v) in map {
        sqlx::query(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(k)
        .bind(serde_json::to_string(&v)?)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Theme;

    #[tokio::test]
    async fn defaults_then_roundtrip() {
        let pool = crate::store::open_memory().await.unwrap();
        let d = load(&pool).await.unwrap();
        assert_eq!(d, AppSettings::default());
        assert_eq!(d.listen_address, "127.0.0.1");

        let mut s = d.clone();
        s.port = 31337;
        s.theme = Theme::Dark;
        s.font_scale = 1.25;
        save(&pool, &s).await.unwrap();
        assert_eq!(load(&pool).await.unwrap(), s);
    }

    #[tokio::test]
    async fn corrupted_value_falls_back_per_field() {
        let pool = crate::store::open_memory().await.unwrap();
        sqlx::query("INSERT INTO app_settings (key, value) VALUES ('port', '\"oops\"'), ('retryCount', '5')")
            .execute(&pool)
            .await
            .unwrap();
        let s = load(&pool).await.unwrap();
        assert_eq!(s.port, AppSettings::default().port);
        assert_eq!(s.retry_count, 5);
    }
}

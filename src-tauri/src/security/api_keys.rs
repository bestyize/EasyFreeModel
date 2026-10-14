//! 本地代理 API 密钥：生成、哈希、恒定时间校验、增删改查。
//!
//! - 密钥格式：`sk-rd-` + 64 位十六进制（32 字节 CSPRNG，256 bit 熵）。
//! - 数据库只保存 SHA-256(完整密钥) 的十六进制与末 4 位（展示掩码用）。
//! - 完整密钥仅在 create / rotate 的返回值里出现一次。
//! - 密钥本身是高熵随机串，所以无需加盐/慢哈希；校验时遍历全部密钥做恒定时间比较，不按内容提前退出。

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;

use crate::error::{AppError, AppResult};
use crate::gateway::{KeyVerifier, VerifiedKey, VerifyFuture};
use crate::models::{ApiKeyInfo, ApiKeySecretResult, CreateApiKeyInput, Protocol};
use crate::store::api_keys::{self as repo, ApiKeyRow};
use crate::store::{self, new_id, parse_ts, ts};

pub const SECRET_PREFIX: &str = "sk-rd-";
const SECRET_BODY_BYTES: usize = 32;
pub const SECRET_LEN: usize = SECRET_PREFIX.len() + SECRET_BODY_BYTES * 2;

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

pub fn generate_secret() -> String {
    let mut buf = [0u8; SECRET_BODY_BYTES];
    rand::fill(&mut buf[..]);
    format!("{SECRET_PREFIX}{}", to_hex(&buf))
}

pub fn hash_secret(secret: &str) -> String {
    to_hex(Sha256::digest(secret.as_bytes()).as_slice())
}

fn hashes_equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn suffix_of(secret: &str) -> String {
    secret.chars().skip(secret.chars().count().saturating_sub(4)).collect()
}

fn mask(suffix: &str) -> String {
    format!("{SECRET_PREFIX}…{suffix}")
}

fn parse_list<T: serde::de::DeserializeOwned>(json: &str) -> Vec<T> {
    serde_json::from_str(json).unwrap_or_default()
}

pub fn to_info(row: &ApiKeyRow) -> ApiKeyInfo {
    let protocols: Vec<String> = parse_list(&row.protocols_json);
    ApiKeyInfo {
        id: row.id.clone(),
        name: row.name.clone(),
        masked: mask(&row.key_suffix),
        secret: row.key_secret.clone(),
        protocols: protocols.iter().filter_map(|p| Protocol::parse(p)).collect(),
        allowed_models: parse_list(&row.allowed_models_json),
        rpm_limit: row.rpm_limit,
        tpm_limit: row.tpm_limit,
        total_limit: row.total_limit,
        call_count: row.call_count,
        created_at: row.created_at.clone(),
        last_used_at: row.last_used_at.clone(),
        expires_at: row.expires_at.clone(),
        disabled: row.disabled_at.is_some(),
    }
}

pub async fn list(pool: &SqlitePool) -> AppResult<Vec<ApiKeyInfo>> {
    Ok(repo::list(pool).await?.iter().map(to_info).collect())
}

fn validate_limit(name: &str, v: Option<i64>) -> AppResult<()> {
    match v {
        Some(n) if n < 1 => Err(AppError::msg(format!("{name} 必须为正整数或留空"))),
        _ => Ok(()),
    }
}

pub async fn create(pool: &SqlitePool, input: CreateApiKeyInput) -> AppResult<ApiKeySecretResult> {
    create_at(pool, input, Utc::now()).await
}

async fn create_at(pool: &SqlitePool, input: CreateApiKeyInput, now: DateTime<Utc>) -> AppResult<ApiKeySecretResult> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(AppError::msg("密钥名称需为 1–64 个字符"));
    }
    let expires_at = match input.expires_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => {
            let t = parse_ts(s).ok_or_else(|| AppError::msg("过期时间格式无效，需为 ISO-8601（如 2026-12-31T00:00:00Z）"))?;
            if t <= now {
                return Err(AppError::msg("过期时间必须晚于当前时间"));
            }
            Some(ts(t))
        }
    };
    validate_limit("RPM 限制", input.rpm_limit)?;
    validate_limit("TPM 限制", input.tpm_limit)?;
    validate_limit("累计限额", input.total_limit)?;

    let mut protocols: Vec<&str> = Vec::new();
    for p in &input.protocols {
        if !protocols.contains(&p.as_str()) {
            protocols.push(p.as_str());
        }
    }
    let mut models: Vec<String> = Vec::new();
    for m in input.allowed_models.iter().map(|m| m.trim()).filter(|m| !m.is_empty()) {
        if m.len() > 256 || m.chars().any(|c| c.is_control()) {
            return Err(AppError::msg("允许模型名称无效"));
        }
        if !models.iter().any(|x| x == m) {
            models.push(m.to_string());
        }
    }
    if models.len() > 500 {
        return Err(AppError::msg("允许模型最多 500 个"));
    }

    let secret = generate_secret();
    let row = ApiKeyRow {
        id: new_id(),
        name,
        key_hash: hash_secret(&secret),
        key_suffix: suffix_of(&secret),
        key_secret: Some(secret.clone()),
        protocols_json: serde_json::to_string(&protocols)?,
        allowed_models_json: serde_json::to_string(&models)?,
        rpm_limit: input.rpm_limit,
        tpm_limit: input.tpm_limit,
        total_limit: input.total_limit,
        call_count: 0,
        created_at: ts(now),
        last_used_at: None,
        expires_at,
        disabled_at: None,
    };
    repo::insert(pool, &row).await?;
    Ok(ApiKeySecretResult { key: to_info(&row), secret })
}

/// 轮换：生成新密钥，旧密钥立即失效；名称、权限、调用次数等保留。
pub async fn rotate(pool: &SqlitePool, id: &str) -> AppResult<ApiKeySecretResult> {
    let secret = generate_secret();
    repo::replace_secret(pool, id, &hash_secret(&secret), &suffix_of(&secret), Some(&secret)).await?;
    let row = repo::get(pool, id).await?.ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
    Ok(ApiKeySecretResult { key: to_info(&row), secret })
}

pub async fn set_disabled(pool: &SqlitePool, id: &str, disabled: bool) -> AppResult<ApiKeyInfo> {
    let now = store::now_ts();
    repo::set_disabled_at(pool, id, disabled.then_some(now.as_str())).await?;
    let row = repo::get(pool, id).await?.ok_or_else(|| AppError::NotFound(format!("api key {id}")))?;
    Ok(to_info(&row))
}

pub async fn delete(pool: &SqlitePool, id: &str) -> AppResult<()> {
    repo::delete(pool, id).await
}

/// 校验 Bearer 明文密钥。已停用、已过期、不存在、格式错误一律返回 None。
/// 通过时更新 last_used_at 与 call_count。
pub async fn verify_at(pool: &SqlitePool, bearer: &str, now: DateTime<Utc>) -> Option<VerifiedKey> {
    // 长度与前缀不是秘密信息，可以提前拒绝
    if bearer.len() != SECRET_LEN || !bearer.starts_with(SECRET_PREFIX) {
        return None;
    }
    let candidate = hash_secret(bearer);
    let rows = match repo::list(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("读取 API 密钥失败：{e}");
            return None;
        }
    };
    // 对每一条都做恒定时间比较，不因命中而提前退出
    let mut matched: Option<&ApiKeyRow> = None;
    for row in &rows {
        if hashes_equal(&candidate, &row.key_hash) {
            matched = Some(row);
        }
    }
    let row = matched?;

    if row.disabled_at.is_some() {
        return None;
    }
    if let Some(exp) = &row.expires_at {
        match parse_ts(exp) {
            Some(t) if t > now => {}
            _ => return None, // 已过期；无法解析的过期时间按失效处理
        }
    }
    if let Err(e) = repo::touch(pool, &row.id, &ts(now)).await {
        tracing::warn!("更新密钥使用记录失败（id={}）：{e}", row.id);
    }

    let protocols: Vec<String> = parse_list::<String>(&row.protocols_json);
    Some(VerifiedKey {
        id: row.id.clone(),
        // 约定：未限制协议（空列表）= 允许全部协议；allowed_models 为空 = 不限制模型
        allowed_protocols: if protocols.is_empty() {
            Protocol::ALL.iter().map(|p| p.as_str().to_string()).collect()
        } else {
            protocols
        },
        allowed_models: parse_list(&row.allowed_models_json),
        rpm_limit: row.rpm_limit.and_then(|v| u32::try_from(v).ok()),
        tpm_limit: row.tpm_limit.and_then(|v| u32::try_from(v).ok()),
    })
}

/// 供网关使用的校验器。
pub struct DbKeyVerifier {
    pool: SqlitePool,
}

impl DbKeyVerifier {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl KeyVerifier for DbKeyVerifier {
    fn verify(&self, bearer: String) -> VerifyFuture {
        let pool = self.pool.clone();
        Box::pin(async move { verify_at(&pool, &bearer, Utc::now()).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn input(name: &str) -> CreateApiKeyInput {
        CreateApiKeyInput {
            name: name.into(),
            expires_at: None,
            protocols: vec![Protocol::OpenaiChat],
            allowed_models: vec!["gpt-x".into()],
            rpm_limit: Some(60),
            tpm_limit: None,
            total_limit: None,
        }
    }

    #[test]
    fn secret_format_and_hash() {
        let s = generate_secret();
        assert_eq!(s.len(), SECRET_LEN);
        assert!(s.starts_with(SECRET_PREFIX));
        assert_ne!(s, generate_secret());
        let h = hash_secret(&s);
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_secret(&s));
        assert!(!h.contains(&s));
        // 已知向量：SHA-256("abc")
        assert_eq!(hash_secret("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn constant_time_compare_semantics() {
        assert!(hashes_equal("abcd", "abcd"));
        assert!(!hashes_equal("abcd", "abce"));
        assert!(!hashes_equal("abcd", "abc"));
    }

    #[tokio::test]
    async fn create_verify_and_usage_tracking() {
        let pool = store::open_memory().await.unwrap();
        let created = create(&pool, input("dev")).await.unwrap();
        assert!(created.secret.starts_with(SECRET_PREFIX));
        assert_eq!(created.key.call_count, 0);

        let v = verify_at(&pool, &created.secret, Utc::now()).await.expect("valid key");
        assert_eq!(v.id, created.key.id);
        assert_eq!(v.allowed_protocols, vec!["openai-chat".to_string()]);
        assert_eq!(v.allowed_models, vec!["gpt-x".to_string()]);
        assert_eq!(v.rpm_limit, Some(60));

        verify_at(&pool, &created.secret, Utc::now()).await.unwrap();
        let info = &list(&pool).await.unwrap()[0];
        assert_eq!(info.call_count, 2);
        assert!(info.last_used_at.is_some());
    }

    #[tokio::test]
    async fn wrong_or_malformed_keys_are_rejected_without_side_effects() {
        let pool = store::open_memory().await.unwrap();
        let created = create(&pool, input("dev")).await.unwrap();
        let mut wrong = created.secret.clone();
        wrong.replace_range(wrong.len() - 1.., if wrong.ends_with('0') { "1" } else { "0" });
        for bad in [wrong.as_str(), "", "sk-rd-", "Bearer x", &created.secret[..20], &created.secret.to_uppercase()] {
            assert!(verify_at(&pool, bad, Utc::now()).await.is_none(), "{bad}");
        }
        assert_eq!(list(&pool).await.unwrap()[0].call_count, 0);
    }

    #[tokio::test]
    async fn disabled_and_expired_keys_return_none() {
        let pool = store::open_memory().await.unwrap();
        let mut inp = input("exp");
        let exp = Utc::now() + Duration::hours(1);
        inp.expires_at = Some(exp.to_rfc3339());
        let k = create(&pool, inp).await.unwrap();

        assert!(verify_at(&pool, &k.secret, Utc::now()).await.is_some());
        // 过期
        assert!(verify_at(&pool, &k.secret, exp + Duration::seconds(1)).await.is_none());
        // 停用 / 重新启用
        assert!(set_disabled(&pool, &k.key.id, true).await.unwrap().disabled);
        assert!(verify_at(&pool, &k.secret, Utc::now()).await.is_none());
        assert!(!set_disabled(&pool, &k.key.id, false).await.unwrap().disabled);
        assert!(verify_at(&pool, &k.secret, Utc::now()).await.is_some());
    }

    #[tokio::test]
    async fn rotate_invalidates_old_secret_and_keeps_settings() {
        let pool = store::open_memory().await.unwrap();
        let k = create(&pool, input("dev")).await.unwrap();
        verify_at(&pool, &k.secret, Utc::now()).await.unwrap();
        let r = rotate(&pool, &k.key.id).await.unwrap();
        assert_ne!(r.secret, k.secret);
        assert_eq!(r.key.id, k.key.id);
        assert_eq!(r.key.call_count, 1);
        assert!(verify_at(&pool, &k.secret, Utc::now()).await.is_none());
        assert!(verify_at(&pool, &r.secret, Utc::now()).await.is_some());
        assert!(rotate(&pool, "missing").await.is_err());
    }

    #[tokio::test]
    async fn list_and_database_persist_secret_for_re_viewing() {
        let pool = store::open_memory().await.unwrap();
        let k = create(&pool, input("dev")).await.unwrap();
        let list_res = list(&pool).await.unwrap();
        assert_eq!(list_res[0].secret.as_deref(), Some(k.secret.as_str()));
        assert!(list_res[0].masked.ends_with(&k.secret[k.secret.len() - 4..]));
        assert_eq!(list_res[0].masked, k.key.masked);

        // 密钥字段在持久化表中有效保存
        let row = repo::get(&pool, &k.key.id).await.unwrap().unwrap();
        assert_eq!(row.key_secret.as_deref(), Some(k.secret.as_str()));
    }

    #[tokio::test]
    async fn create_validates_input_and_delete_works() {
        let pool = store::open_memory().await.unwrap();
        assert!(create(&pool, input("  ")).await.is_err());
        let mut bad = input("x");
        bad.expires_at = Some("not-a-date".into());
        assert!(create(&pool, bad).await.is_err());
        let mut past = input("x");
        past.expires_at = Some((Utc::now() - Duration::days(1)).to_rfc3339());
        assert!(create(&pool, past).await.is_err());
        let mut lim = input("x");
        lim.rpm_limit = Some(0);
        assert!(create(&pool, lim).await.is_err());

        let k = create(&pool, input("ok")).await.unwrap();
        delete(&pool, &k.key.id).await.unwrap();
        assert!(list(&pool).await.unwrap().is_empty());
        assert!(delete(&pool, &k.key.id).await.is_err());
        assert!(verify_at(&pool, &k.secret, Utc::now()).await.is_none());
    }

    #[tokio::test]
    async fn db_verifier_implements_gateway_contract() {
        let pool = store::open_memory().await.unwrap();
        let k = create(&pool, input("gw")).await.unwrap();
        let verifier: std::sync::Arc<dyn KeyVerifier> = std::sync::Arc::new(DbKeyVerifier::new(pool));
        assert!(verifier.verify(k.secret.clone()).await.is_some());
        assert!(verifier.verify("sk-rd-nope".into()).await.is_none());
    }

    #[tokio::test]
    async fn empty_protocols_expand_to_all() {
        let pool = store::open_memory().await.unwrap();
        let mut inp = input("all");
        inp.protocols.clear();
        inp.allowed_models.clear();
        let k = create(&pool, inp).await.unwrap();
        let v = verify_at(&pool, &k.secret, Utc::now()).await.unwrap();
        assert_eq!(v.allowed_protocols.len(), Protocol::ALL.len());
        assert!(v.allowed_models.is_empty());
    }
}

//! 网关用量记录：每个转发请求（成功 / 失败 / 取消）恰好写一条 `usage_requests`。
//!
//! - [`UsageGuard`] 在请求开始时创建；调用 [`UsageGuard::finish`] 写入终态。若 guard 在 `finish` 之前被丢弃
//!   （客户端断开导致请求 future / 响应流被 drop），Drop 会写一条 `cancelled` 记录。
//! - token：上游返回了 usage 就用上游值（`token_source = upstream`）；缺失的部分用本地粗略估算
//!   （`token_source = estimated`）。`input_tokens` 沿用规范化模型的口径：**含**缓存命中的总输入，
//!   `cached_tokens` 是其中命中缓存的子集——展示层不应把两者相加。
//! - `estimated_cost` 恒为 0：当前没有价目表，不凭空估价。
//! - 日志与记录里只有 id、模型别名、状态码类信息，没有正文或密钥。

use std::time::Instant;

use serde_json::Value;
use sqlx::SqlitePool;

use crate::models::{TokenSource, UsageStatus};
use crate::protocols::canonical::Usage;
use crate::store::usage::NewUsage;
use crate::store::{new_id, now_ts};

/// 文本字节数 → 粗略 token 数：ASCII 约 4 字节 1 token，非 ASCII 字符约 1 字符 1 token。
pub fn estimate_text_tokens(text: &str) -> u32 {
    let (mut ascii, mut wide) = (0u64, 0u64);
    for c in text.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    (ascii.div_ceil(4) + wide).min(u64::from(u32::MAX)) as u32
}

/// 粗略估算按字节计的输出体量（流式累计的是字节数）。
pub fn estimate_bytes_tokens(bytes: usize) -> u32 {
    (bytes as u64).div_ceil(4).min(u64::from(u32::MAX)) as u32
}

/// 估算一个请求 JSON 的输入 token：只数文本类字符串，跳过 data URL 与 base64 图片负载。
pub fn estimate_request_tokens(body: &Value) -> u32 {
    fn walk(v: &Value, acc: &mut (u64, u64)) {
        match v {
            Value::String(s) => {
                if !s.starts_with("data:") {
                    for c in s.chars() {
                        if c.is_ascii() {
                            acc.0 += 1;
                        } else {
                            acc.1 += 1;
                        }
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, acc)),
            Value::Object(o) => {
                let is_media = o.contains_key("media_type");
                for (k, x) in o {
                    if is_media && k == "data" {
                        continue;
                    }
                    walk(x, acc);
                }
            }
            _ => {}
        }
    }
    let mut acc = (0u64, 0u64);
    walk(body, &mut acc);
    (acc.0.div_ceil(4) + acc.1).min(u64::from(u32::MAX)) as u32
}

struct Pending {
    id: String,
    api_key_id: Option<String>,
    provider_id: String,
    connection_id: String,
    model: String,
    protocol: &'static str,
    started: Instant,
    estimated_input: u32,
}

pub struct UsageGuard {
    pool: SqlitePool,
    pending: Option<Pending>,
}

impl UsageGuard {
    pub fn new(
        pool: SqlitePool,
        api_key_id: Option<String>,
        provider_id: &str,
        model: &str,
        protocol: &'static str,
        estimated_input: u32,
    ) -> Self {
        Self {
            pool,
            pending: Some(Pending {
                id: new_id(),
                api_key_id,
                provider_id: provider_id.to_string(),
                connection_id: String::new(),
                model: model.to_string(),
                protocol,
                started: Instant::now(),
                estimated_input,
            }),
        }
    }

    pub fn request_id(&self) -> &str {
        self.pending.as_ref().map(|p| p.id.as_str()).unwrap_or("")
    }

    pub fn set_connection(&mut self, connection_id: &str) {
        if let Some(p) = &mut self.pending {
            p.connection_id = connection_id.to_string();
        }
    }

    /// 写入终态。`output_bytes` 仅在上游没有给出 output_tokens 时用于估算。
    pub fn finish(mut self, status: UsageStatus, usage: &Usage, output_bytes: usize) {
        if let Some(p) = self.pending.take() {
            Self::write(self.pool.clone(), p, status, usage, output_bytes);
        }
    }

    fn write(pool: SqlitePool, p: Pending, status: UsageStatus, usage: &Usage, output_bytes: usize) {
        let row = build_row(p, status, usage, output_bytes);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("no async runtime available; usage record dropped");
            return;
        };
        handle.spawn(async move {
            if let Err(e) = super::record_request(&pool, row).await {
                tracing::warn!(error = %e, "failed to write the usage record");
            }
        });
    }
}

impl Drop for UsageGuard {
    fn drop(&mut self) {
        if let Some(p) = self.pending.take() {
            Self::write(self.pool.clone(), p, UsageStatus::Cancelled, &Usage::default(), 0);
        }
    }
}

fn build_row(p: Pending, status: UsageStatus, usage: &Usage, output_bytes: usize) -> NewUsage {
    let duration_ms = p.started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let (input, output, source) = match status {
        UsageStatus::Error if usage.is_empty() => (0, 0, TokenSource::Estimated),
        _ => {
            let input = usage.input_tokens.unwrap_or(p.estimated_input);
            let output = usage.output_tokens.unwrap_or_else(|| estimate_bytes_tokens(output_bytes));
            let upstream = usage.input_tokens.is_some() && usage.output_tokens.is_some();
            (input, output, if upstream { TokenSource::Upstream } else { TokenSource::Estimated })
        }
    };
    NewUsage {
        id: p.id,
        api_key_id: p.api_key_id,
        provider_id: p.provider_id,
        connection_id: p.connection_id,
        model_id: p.model,
        protocol: p.protocol.to_string(),
        status,
        input_tokens: i64::from(input),
        output_tokens: i64::from(output),
        cached_tokens: i64::from(usage.cache_read_tokens.unwrap_or(0)),
        estimated_cost: 0.0,
        duration_ms,
        token_source: source,
        created_at: now_ts(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pending() -> Pending {
        Pending {
            id: "u1".into(),
            api_key_id: Some("k".into()),
            provider_id: "p".into(),
            connection_id: "c".into(),
            model: "m".into(),
            protocol: "openai-chat",
            started: Instant::now(),
            estimated_input: 100,
        }
    }

    #[test]
    fn upstream_usage_is_used_verbatim() {
        let u = Usage { input_tokens: Some(30), output_tokens: Some(9), cache_read_tokens: Some(12), cache_creation_tokens: None };
        let r = build_row(pending(), UsageStatus::Success, &u, 4000);
        assert_eq!((r.input_tokens, r.output_tokens, r.cached_tokens), (30, 9, 12));
        assert_eq!(r.token_source, TokenSource::Upstream);
        assert_eq!(r.estimated_cost, 0.0);
    }

    #[test]
    fn missing_usage_is_estimated_and_labelled() {
        let r = build_row(pending(), UsageStatus::Success, &Usage::default(), 400);
        assert_eq!((r.input_tokens, r.output_tokens), (100, 100));
        assert_eq!(r.token_source, TokenSource::Estimated);
        // 只给了一半：缺的那半估算，整体标 estimated
        let u = Usage { input_tokens: Some(7), ..Default::default() };
        let r = build_row(pending(), UsageStatus::Success, &u, 8);
        assert_eq!((r.input_tokens, r.output_tokens, r.token_source), (7, 2, TokenSource::Estimated));
    }

    #[test]
    fn failed_requests_record_no_tokens_and_cancelled_estimate() {
        let r = build_row(pending(), UsageStatus::Error, &Usage::default(), 0);
        assert_eq!((r.input_tokens, r.output_tokens, r.token_source), (0, 0, TokenSource::Estimated));
        let r = build_row(pending(), UsageStatus::Cancelled, &Usage::default(), 40);
        assert_eq!((r.input_tokens, r.output_tokens, r.token_source), (100, 10, TokenSource::Estimated));
    }

    #[test]
    fn request_estimate_skips_images() {
        let big = "A".repeat(500_000);
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "hello world!"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": big}},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{big}")}},
        ]}]});
        assert!(estimate_request_tokens(&body) < 50, "{}", estimate_request_tokens(&body));
        assert_eq!(estimate_text_tokens("你好"), 2);
        assert_eq!(estimate_text_tokens("abcdefgh"), 2);
    }
}

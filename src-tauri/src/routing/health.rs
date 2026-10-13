//! 账号健康状态（内存）。
//!
//! - 401/402/403：硬冷却（默认 5 分钟），调度器完全跳过。
//! - 429：硬冷却，时长取 `Retry-After`（夹在 1 秒–1 小时之间）；没有该头时 30 秒起按连续次数翻倍，上限 5 分钟。
//! - 5xx / 网络错误：软降级（2、4、8… 秒，上限 60 秒）——只是排到最后，不排除；只剩它时仍会被尝试，
//!   这样单账号用户遇到偶发 5xx 不会被自己的冷却挡在门外。
//! - 成功：清除全部状态。
//!
//! 所有时间都由调用方传入（`*_at`），便于确定性测试。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const AUTH_COOLDOWN: Duration = Duration::from_secs(300);
pub const RATE_LIMIT_DEFAULT: Duration = Duration::from_secs(30);
pub const RATE_LIMIT_MAX_BACKOFF: Duration = Duration::from_secs(300);
pub const RETRY_AFTER_MIN: Duration = Duration::from_secs(1);
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(3600);
pub const SOFT_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Auth,
    RateLimited,
    Transient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// 可以正常使用。
    Ready,
    /// 最近有瞬时故障：可用，但排在健康账号之后。
    Degraded,
    /// 硬冷却中，剩余时长。
    Cooling(Duration),
}

#[derive(Debug, Default, Clone)]
struct ConnHealth {
    hard_until: Option<Instant>,
    soft_until: Option<Instant>,
    consecutive_failures: u32,
    rate_streak: u32,
    last_failure: Option<FailureKind>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSnapshot {
    pub availability: Availability,
    pub consecutive_failures: u32,
    pub last_failure: Option<FailureKind>,
}

#[derive(Debug, Default)]
pub struct Health {
    inner: Mutex<HashMap<String, ConnHealth>>,
}

impl Health {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut HashMap<String, ConnHealth>) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    pub fn availability(&self, id: &str) -> Availability {
        self.availability_at(id, Instant::now())
    }

    pub fn availability_at(&self, id: &str, now: Instant) -> Availability {
        self.with(|m| match m.get(id) {
            None => Availability::Ready,
            Some(h) => match (h.hard_until, h.soft_until) {
                (Some(t), _) if t > now => Availability::Cooling(t - now),
                (_, Some(t)) if t > now => Availability::Degraded,
                _ => Availability::Ready,
            },
        })
    }

    pub fn snapshot(&self, id: &str) -> HealthSnapshot {
        let availability = self.availability(id);
        self.with(|m| {
            let h = m.get(id).cloned().unwrap_or_default();
            HealthSnapshot { availability, consecutive_failures: h.consecutive_failures, last_failure: h.last_failure }
        })
    }

    pub fn record_success(&self, id: &str) {
        self.with(|m| {
            m.remove(id);
        });
    }

    /// 清除状态（用户修复了凭据 / 手动重试）。
    pub fn reset(&self, id: &str) {
        self.record_success(id);
    }

    pub fn record_auth_failure(&self, id: &str) -> Duration {
        self.record_auth_failure_at(id, Instant::now())
    }

    pub fn record_auth_failure_at(&self, id: &str, now: Instant) -> Duration {
        self.with(|m| {
            let h = m.entry(id.to_string()).or_default();
            h.consecutive_failures += 1;
            h.last_failure = Some(FailureKind::Auth);
            h.hard_until = Some(now + AUTH_COOLDOWN);
            AUTH_COOLDOWN
        })
    }

    pub fn record_rate_limited(&self, id: &str, retry_after: Option<Duration>) -> Duration {
        self.record_rate_limited_at(id, retry_after, Instant::now())
    }

    pub fn record_rate_limited_at(&self, id: &str, retry_after: Option<Duration>, now: Instant) -> Duration {
        self.with(|m| {
            let h = m.entry(id.to_string()).or_default();
            h.consecutive_failures += 1;
            h.rate_streak += 1;
            h.last_failure = Some(FailureKind::RateLimited);
            let cooldown = match retry_after {
                Some(d) => d.clamp(RETRY_AFTER_MIN, RETRY_AFTER_MAX),
                None => {
                    let factor = 1u32 << (h.rate_streak - 1).min(4);
                    (RATE_LIMIT_DEFAULT * factor).min(RATE_LIMIT_MAX_BACKOFF)
                }
            };
            h.hard_until = Some(now + cooldown);
            cooldown
        })
    }

    pub fn record_transient(&self, id: &str) {
        self.record_transient_at(id, Instant::now());
    }

    pub fn record_transient_at(&self, id: &str, now: Instant) {
        self.with(|m| {
            let h = m.entry(id.to_string()).or_default();
            h.consecutive_failures += 1;
            h.last_failure = Some(FailureKind::Transient);
            let secs = 1u64 << h.consecutive_failures.min(6);
            h.soft_until = Some(now + Duration::from_secs(secs).min(SOFT_MAX));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_connection_is_ready() {
        assert_eq!(Health::new().availability("x"), Availability::Ready);
    }

    #[test]
    fn rate_limit_uses_retry_after_clamped_and_expires() {
        let h = Health::new();
        let t0 = Instant::now();
        assert_eq!(h.record_rate_limited_at("a", Some(Duration::from_secs(10)), t0), Duration::from_secs(10));
        assert_eq!(h.availability_at("a", t0 + Duration::from_secs(3)), Availability::Cooling(Duration::from_secs(7)));
        assert_eq!(h.availability_at("a", t0 + Duration::from_secs(10)), Availability::Ready);
        // 夹取
        assert_eq!(h.record_rate_limited_at("b", Some(Duration::ZERO), t0), RETRY_AFTER_MIN);
        assert_eq!(h.record_rate_limited_at("c", Some(Duration::from_secs(999_999)), t0), RETRY_AFTER_MAX);
    }

    #[test]
    fn rate_limit_without_header_backs_off_exponentially() {
        let h = Health::new();
        let t0 = Instant::now();
        let seq: Vec<u64> = (0..7).map(|_| h.record_rate_limited_at("a", None, t0).as_secs()).collect();
        assert_eq!(seq, vec![30, 60, 120, 240, 300, 300, 300]);
        h.record_success("a");
        assert_eq!(h.record_rate_limited_at("a", None, t0).as_secs(), 30, "success resets the streak");
    }

    #[test]
    fn auth_failure_is_hard_cooldown() {
        let h = Health::new();
        let t0 = Instant::now();
        h.record_auth_failure_at("a", t0);
        assert!(matches!(h.availability_at("a", t0 + Duration::from_secs(60)), Availability::Cooling(_)));
        assert_eq!(h.availability_at("a", t0 + AUTH_COOLDOWN), Availability::Ready);
        assert_eq!(h.snapshot("a").last_failure, Some(FailureKind::Auth));
    }

    #[test]
    fn transient_failures_only_degrade() {
        let h = Health::new();
        let t0 = Instant::now();
        h.record_transient_at("a", t0);
        assert_eq!(h.availability_at("a", t0 + Duration::from_millis(500)), Availability::Degraded);
        assert_eq!(h.availability_at("a", t0 + Duration::from_secs(3)), Availability::Ready);
        for _ in 0..20 {
            h.record_transient_at("a", t0);
        }
        assert_eq!(h.availability_at("a", t0 + SOFT_MAX), Availability::Ready);
        h.record_success("a");
        assert_eq!(h.snapshot("a").consecutive_failures, 0);
    }
}

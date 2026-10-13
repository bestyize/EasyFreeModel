//! 内存滑动窗口限流（按 key id）。
//!
//! - 窗口内保留每次放行的时间戳；窗口内数量达到上限即拒绝，并给出需等待的时长。
//! - `check_at` 允许注入时间，便于确定性测试。
//! - 仅实现 RPM。`VerifiedKey::tpm_limit` 需要上游返回的 token 用量才能结算，留待路由层接入后实现。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 超过该数量的 key 时顺带清理已过期的条目，避免长期运行后内存增长。
const PRUNE_THRESHOLD: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// 距离最早一次请求滑出窗口还需等待的时间。
    pub retry_after: Duration,
}

impl RateLimited {
    /// `Retry-After` 响应头使用的整数秒（向上取整，至少 1）。
    pub fn retry_after_secs(&self) -> u64 {
        let secs = self.retry_after.as_secs() + u64::from(self.retry_after.subsec_nanos() > 0);
        secs.max(1)
    }
}

#[derive(Debug)]
pub struct RateLimiter {
    window: Duration,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::per_minute()
    }
}

impl RateLimiter {
    pub fn new(window: Duration) -> Self {
        Self { window, hits: Mutex::new(HashMap::new()) }
    }

    pub fn per_minute() -> Self {
        Self::new(Duration::from_secs(60))
    }

    /// 以当前时间检查并记录一次请求。`limit == 0` 视为不限流。
    pub fn check(&self, key_id: &str, limit: u32) -> Result<(), RateLimited> {
        self.check_at(key_id, limit, Instant::now())
    }

    pub fn check_at(&self, key_id: &str, limit: u32, now: Instant) -> Result<(), RateLimited> {
        if limit == 0 {
            return Ok(());
        }
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        if hits.len() > PRUNE_THRESHOLD {
            let window = self.window;
            hits.retain(|_, q| q.back().is_some_and(|t| now.saturating_duration_since(*t) < window));
        }
        let q = hits.entry(key_id.to_string()).or_default();
        while q.front().is_some_and(|t| now.saturating_duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= limit as usize {
            // 队首是最早的放行记录；它滑出窗口后才会腾出名额。
            let oldest = *q.front().expect("len >= limit >= 1");
            let retry_after = (oldest + self.window).saturating_duration_since(now);
            return Err(RateLimited { retry_after });
        }
        q.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_rejects_with_retry_after() {
        let rl = RateLimiter::new(Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..3 {
            assert!(rl.check_at("k", 3, t0 + Duration::from_secs(i)).is_ok());
        }
        let err = rl.check_at("k", 3, t0 + Duration::from_secs(10)).unwrap_err();
        // 最早一次在 t0，于 t0+60 滑出窗口；现在是 t0+10。
        assert_eq!(err.retry_after, Duration::from_secs(50));
        assert_eq!(err.retry_after_secs(), 50);
    }

    #[test]
    fn window_slides() {
        let rl = RateLimiter::new(Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(rl.check_at("k", 2, t0).is_ok());
        assert!(rl.check_at("k", 2, t0 + Duration::from_secs(30)).is_ok());
        assert!(rl.check_at("k", 2, t0 + Duration::from_secs(59)).is_err());
        // t0 的记录在 t0+60 滑出，腾出一个名额（t0+30 的仍在窗口内）。
        assert!(rl.check_at("k", 2, t0 + Duration::from_secs(60)).is_ok());
        assert!(rl.check_at("k", 2, t0 + Duration::from_secs(61)).is_err());
        // 全部滑出后恢复。
        assert!(rl.check_at("k", 2, t0 + Duration::from_secs(200)).is_ok());
    }

    #[test]
    fn rejected_requests_do_not_consume_quota() {
        let rl = RateLimiter::new(Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(rl.check_at("k", 1, t0).is_ok());
        for i in 1..9 {
            assert!(rl.check_at("k", 1, t0 + Duration::from_secs(i)).is_err());
        }
        assert!(rl.check_at("k", 1, t0 + Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn keys_are_independent_and_zero_means_unlimited() {
        let rl = RateLimiter::per_minute();
        let t0 = Instant::now();
        assert!(rl.check_at("a", 1, t0).is_ok());
        assert!(rl.check_at("a", 1, t0).is_err());
        assert!(rl.check_at("b", 1, t0).is_ok());
        for _ in 0..1000 {
            assert!(rl.check_at("c", 0, t0).is_ok());
        }
    }

    #[test]
    fn retry_after_rounds_up_and_is_at_least_one_second() {
        assert_eq!(RateLimited { retry_after: Duration::from_millis(1) }.retry_after_secs(), 1);
        assert_eq!(RateLimited { retry_after: Duration::from_millis(1500) }.retry_after_secs(), 2);
        assert_eq!(RateLimited { retry_after: Duration::ZERO }.retry_after_secs(), 1);
    }

    #[test]
    fn idle_keys_are_pruned() {
        let rl = RateLimiter::new(Duration::from_secs(1));
        let t0 = Instant::now();
        for i in 0..(PRUNE_THRESHOLD + 10) {
            rl.check_at(&format!("k{i}"), 1, t0).unwrap();
        }
        rl.check_at("late", 1, t0 + Duration::from_secs(5)).unwrap();
        assert!(rl.hits.lock().unwrap().len() <= 2);
    }
}

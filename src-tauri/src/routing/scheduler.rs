//! 账号调度：把一个提供商的连接排成「本次请求的尝试顺序」。
//!
//! 规则：
//! - 只考虑 `enabled` 且状态不是 `paused` 的连接；`needsReauth`（OAuth 令牌失效）的连接同样不参与，直到用户重新授权。
//! - 硬冷却中的连接直接排除（并统计剩余时长，用于 503 的 `Retry-After`）。
//! - 健康的连接排在前面，降级的（最近瞬时故障，或库里状态为 `error`）排在最后，按 priority 分层，数值小的先试。
//! - 同一 priority 层内：`roundRobin` 开启时按 weight 加权轮询（每个请求推进一次游标，轮到的连接排第一，
//!   其余顺次作为回退）；关闭时严格按 priority、创建顺序，即「主备」语义。

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::health::{Availability, Health};
use super::repo::{ConnCfg, ProviderCfg};

#[derive(Debug, Default)]
pub struct Scheduler {
    cursors: Mutex<HashMap<String, u64>>,
}

#[derive(Debug, Default)]
pub struct Plan {
    /// 本次请求依次尝试的连接。
    pub order: Vec<ConnCfg>,
    /// 因硬冷却被排除的连接数与其中最短的剩余时长。
    pub cooling: usize,
    pub shortest_cooldown: Option<Duration>,
    /// 因停用 / 暂停而不参与调度的连接数。
    pub inactive: usize,
    /// 因 OAuth 令牌失效（needsReauth）而不参与调度的连接数。
    pub needs_reauth: usize,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_cursor(&self, key: &str) -> u64 {
        let mut g = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
        let c = g.entry(key.to_string()).or_insert(0);
        let v = *c;
        *c = c.wrapping_add(1);
        v
    }

    pub fn plan(&self, provider: &ProviderCfg, conns: Vec<ConnCfg>, health: &Health) -> Plan {
        self.plan_at(provider, conns, health, Instant::now())
    }

    pub fn plan_at(&self, provider: &ProviderCfg, conns: Vec<ConnCfg>, health: &Health, now: Instant) -> Plan {
        let mut plan = Plan::default();
        // priority → (healthy, degraded)
        let mut tiers: BTreeMap<i64, (Vec<ConnCfg>, Vec<ConnCfg>)> = BTreeMap::new();
        for c in conns {
            if !c.enabled || c.status == "paused" {
                plan.inactive += 1;
                continue;
            }
            if c.status == "needsReauth" {
                plan.needs_reauth += 1;
                continue;
            }
            match health.availability_at(&c.id, now) {
                Availability::Cooling(rem) => {
                    plan.cooling += 1;
                    plan.shortest_cooldown = Some(plan.shortest_cooldown.map_or(rem, |s| s.min(rem)));
                }
                Availability::Degraded => tiers.entry(c.priority).or_default().1.push(c),
                Availability::Ready if c.status == "error" => tiers.entry(c.priority).or_default().1.push(c),
                Availability::Ready => tiers.entry(c.priority).or_default().0.push(c),
            }
        }
        let mut degraded_all: Vec<ConnCfg> = Vec::new();
        for (priority, (mut healthy, degraded)) in tiers {
            if provider.round_robin && healthy.len() > 1 {
                let total: u64 = healthy.iter().map(|c| c.weight.max(1) as u64).sum();
                let slot = self.next_cursor(&format!("{}:{priority}", provider.id)) % total;
                let mut acc = 0u64;
                let mut start = 0usize;
                for (i, c) in healthy.iter().enumerate() {
                    acc += c.weight.max(1) as u64;
                    if slot < acc {
                        start = i;
                        break;
                    }
                }
                healthy.rotate_left(start);
            }
            plan.order.extend(healthy);
            degraded_all.extend(degraded);
        }
        plan.order.extend(degraded_all);
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(rr: bool) -> ProviderCfg {
        ProviderCfg {
            id: "p".into(),
            display_name: "P".into(),
            kind: "custom".into(),
            adapter: "openai-compatible".into(),
            enabled: true,
            round_robin: rr,
            max_concurrency: 4,
            max_retries: 2,
            ..Default::default()
        }
    }

    fn conn(id: &str, priority: i64, weight: i64) -> ConnCfg {
        ConnCfg {
            id: id.into(),
            provider_id: "p".into(),
            account_label: id.into(),
            kind: "apiKey".into(),
            status: "active".into(),
            priority,
            weight,
            enabled: true,
            credential_ref: Some("ref".into()),
        }
    }

    fn ids(p: &Plan) -> Vec<&str> {
        p.order.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn weighted_round_robin_distribution_is_exact_over_a_cycle() {
        let s = Scheduler::new();
        let h = Health::new();
        let conns = vec![conn("a", 0, 1), conn("b", 0, 3)];
        let mut firsts: HashMap<String, u32> = HashMap::new();
        for _ in 0..40 {
            let plan = s.plan(&provider(true), conns.clone(), &h);
            assert_eq!(plan.order.len(), 2, "the other account stays in the plan as a fallback");
            *firsts.entry(plan.order[0].id.clone()).or_default() += 1;
        }
        assert_eq!(firsts["a"], 10);
        assert_eq!(firsts["b"], 30);
    }

    #[test]
    fn round_robin_off_is_primary_backup() {
        let s = Scheduler::new();
        let h = Health::new();
        let conns = vec![conn("a", 0, 1), conn("b", 0, 1), conn("c", 1, 1)];
        for _ in 0..5 {
            assert_eq!(ids(&s.plan(&provider(false), conns.clone(), &h)), vec!["a", "b", "c"]);
        }
    }

    #[test]
    fn priority_tiers_come_first_even_with_round_robin() {
        let s = Scheduler::new();
        let h = Health::new();
        let conns = vec![conn("a", 0, 1), conn("b", 0, 1), conn("z", 5, 1)];
        for _ in 0..6 {
            let plan = s.plan(&provider(true), conns.clone(), &h);
            assert_eq!(plan.order.last().unwrap().id, "z");
        }
    }

    #[test]
    fn disabled_paused_and_cooling_are_excluded_degraded_go_last() {
        let s = Scheduler::new();
        let h = Health::new();
        let now = Instant::now();
        let mut disabled = conn("off", 0, 1);
        disabled.enabled = false;
        let mut paused = conn("paused", 0, 1);
        paused.status = "paused".into();
        let mut errored = conn("err", 0, 1);
        errored.status = "error".into();
        h.record_rate_limited_at("cool", Some(Duration::from_secs(20)), now);
        h.record_transient_at("flaky", now);
        let conns = vec![errored, conn("flaky", 0, 1), disabled, paused, conn("cool", 0, 1), conn("good", 3, 1)];
        let plan = s.plan_at(&provider(false), conns, &h, now);
        assert_eq!(ids(&plan), vec!["good", "err", "flaky"]);
        assert_eq!(plan.cooling, 1);
        assert_eq!(plan.inactive, 2);
        assert_eq!(plan.shortest_cooldown, Some(Duration::from_secs(20)));
    }

    #[test]
    fn needs_reauth_connections_are_skipped_and_counted() {
        let s = Scheduler::new();
        let h = Health::new();
        let mut stale = conn("stale", 0, 1);
        stale.kind = "oauth".into();
        stale.status = "needsReauth".into();
        let plan = s.plan(&provider(false), vec![stale, conn("good", 1, 1)], &h);
        assert_eq!(ids(&plan), vec!["good"]);
        assert_eq!(plan.needs_reauth, 1);
        assert_eq!(plan.inactive, 0);
    }

    #[test]
    fn nothing_available_gives_empty_plan() {
        let s = Scheduler::new();
        let h = Health::new();
        assert!(s.plan(&provider(true), vec![], &h).order.is_empty());
    }
}

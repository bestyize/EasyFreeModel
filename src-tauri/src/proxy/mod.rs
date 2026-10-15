//! 代理池：出站代理节点、选路策略、被动健康检查与冷却。
//!
//! - 节点（HTTP / HTTPS / SOCKS5 / SOCKS5h，可带账号密码）存在 `proxy_nodes` 表，密码只存凭据库。
//! - 池配置（开关、策略、生效的提供商范围、失败阈值与冷却、回退直连）是 `app_settings` 里的一个 JSON 键。
//! - 转发管线每次出站前调用 [`ProxyPool::plan`] 得到按策略排好的尝试顺序；代理连不上（`is_connect`）时
//!   自动换下一个节点，连续失败达到阈值的节点进入冷却，冷却结束后半开重试（再失败一次立即重新冷却）。
//! - 默认关闭；开启后若没有可用节点，默认**拒绝出站**而不是悄悄直连（可在配置里打开“回退直连”）。
//! - 范围：网关转发的上游请求，以及提供商连接测试 / 拉取模型 / 配额查询（经 [`probe_builder`]）。
//!   OAuth 授权与多模态媒体请求暂不经过代理池。

pub mod parse;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use url::Url;

use crate::error::{AppError, AppResult};
use crate::security::vault::{self, CredentialVault};
use crate::store::proxy::{self as repo, NodeRow};
use crate::store::{new_id, now_ts};
use crate::upstream::client::user_agent;
use crate::upstream::Secret;

pub const DEFAULT_TEST_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";
const MAX_NODES: usize = 500;
const MAX_IMPORT_LINES: usize = 1000;
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_CONCURRENCY: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

// ───────────────────────── 配置与对外类型 ─────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Socks5 => "socks5",
            Scheme::Socks5h => "socks5h",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Some(Scheme::Http),
            "https" => Some(Scheme::Https),
            "socks5" => Some(Scheme::Socks5),
            "socks5h" => Some(Scheme::Socks5h),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Strategy {
    /// 轮流使用各节点。
    #[default]
    RoundRobin,
    /// 随机起点。
    Random,
    /// 按列表顺序，永远优先第一个可用节点（主备）。
    Failover,
    /// 按最近一次测速结果由快到慢；未测速的排在最后。
    LowestLatency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderScope {
    /// 所有提供商都走代理池。
    #[default]
    All,
    /// 只有 `providerIds` 里的提供商走代理池，其余直连。
    Include,
    /// `providerIds` 里的提供商直连，其余走代理池。
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PoolConfig {
    pub enabled: bool,
    pub strategy: Strategy,
    pub provider_scope: ProviderScope,
    pub provider_ids: Vec<String>,
    /// 连续失败多少次后让节点进入冷却。
    pub failure_threshold: u32,
    pub cooldown_seconds: u32,
    /// 单个请求最多换几个不同的代理重试。
    pub max_attempts: u32,
    /// 没有可用节点（或都失败）时是否回退直连。
    pub fallback_direct: bool,
    pub test_url: String,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strategy: Strategy::default(),
            provider_scope: ProviderScope::default(),
            provider_ids: Vec::new(),
            failure_threshold: 3,
            cooldown_seconds: 60,
            max_attempts: 2,
            fallback_direct: false,
            test_url: DEFAULT_TEST_URL.to_string(),
        }
    }
}

impl PoolConfig {
    pub fn applies_to(&self, provider_id: &str) -> bool {
        let listed = self.provider_ids.iter().any(|p| p == provider_id);
        match self.provider_scope {
            ProviderScope::All => true,
            ProviderScope::Include => listed,
            ProviderScope::Exclude => !listed,
        }
    }
}

pub fn validate_config(mut c: PoolConfig) -> AppResult<PoolConfig> {
    if !(1..=20).contains(&c.failure_threshold) {
        return Err(AppError::msg("失败阈值需在 1–20 之间"));
    }
    if !(5..=3600).contains(&c.cooldown_seconds) {
        return Err(AppError::msg("冷却时间需在 5–3600 秒之间"));
    }
    if !(1..=5).contains(&c.max_attempts) {
        return Err(AppError::msg("单次请求最多尝试的代理数需在 1–5 之间"));
    }
    c.test_url = c.test_url.trim().to_string();
    match Url::parse(&c.test_url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => {}
        _ => return Err(AppError::msg("测速地址需为有效的 http/https 地址")),
    }
    let mut seen = HashSet::new();
    c.provider_ids = c
        .provider_ids
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty() && seen.insert(p.clone()))
        .collect();
    Ok(c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Untested,
    Healthy,
    Unhealthy,
    Cooldown,
}

/// 返回前端的节点（含运行期健康状态，不含密码）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyNode {
    pub id: String,
    pub name: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub has_password: bool,
    pub enabled: bool,
    pub status: NodeStatus,
    pub latency_ms: Option<u64>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub exit_ip: Option<String>,
    pub success_count: u64,
    pub failure_count: u64,
    pub cooldown_remaining_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyPoolState {
    pub config: PoolConfig,
    pub nodes: Vec<ProxyNode>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyNodeInput {
    pub id: Option<String>,
    pub name: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    /// None = 保持原密码不变；空字符串 = 清除密码；其它 = 设置新密码。
    pub password: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkip {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub added: usize,
    pub skipped: Vec<ImportSkip>,
}

// ───────────────────────── 运行期结构 ─────────────────────────

struct NodeSpec {
    id: String,
    name: String,
    scheme: Scheme,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<Secret>,
    has_password: bool,
    enabled: bool,
}

impl NodeSpec {
    fn proxy(&self) -> Result<reqwest::Proxy, ()> {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let mut url = Url::parse(&format!("{}://{host}:{}", self.scheme.as_str(), self.port)).map_err(|_| ())?;
        if let Some(u) = &self.username {
            url.set_username(u).map_err(|_| ())?;
            if let Some(p) = &self.password {
                url.set_password(Some(p.expose())).map_err(|_| ())?;
            }
        }
        reqwest::Proxy::all(url.as_str()).map_err(|_| ())
    }

    fn client(&self, timeout: Option<Duration>) -> Result<reqwest::Client, ()> {
        let mut b = reqwest::Client::builder()
            .user_agent(user_agent())
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .pool_idle_timeout(Duration::from_secs(60))
            .no_proxy()
            .proxy(self.proxy()?);
        if let Some(t) = timeout {
            b = b.timeout(t);
        }
        b.build().map_err(|_| ())
    }
}

struct Snapshot {
    config: PoolConfig,
    nodes: Vec<Arc<NodeSpec>>,
}

#[derive(Default, Clone)]
struct Health {
    consecutive_failures: u32,
    cooldown_until: Option<Instant>,
    latency_ms: Option<u64>,
    last_checked_at: Option<String>,
    last_error: Option<String>,
    exit_ip: Option<String>,
    ok: u64,
    fail: u64,
    seen_ok: bool,
}

impl Health {
    fn cooling(&self, now: Instant) -> bool {
        self.cooldown_until.is_some_and(|t| t > now)
    }

    fn status(&self, now: Instant) -> NodeStatus {
        if self.cooling(now) {
            NodeStatus::Cooldown
        } else if self.consecutive_failures > 0 {
            NodeStatus::Unhealthy
        } else if self.seen_ok {
            NodeStatus::Healthy
        } else {
            NodeStatus::Untested
        }
    }
}

/// 一次出站的单个尝试。
#[derive(Clone)]
pub enum Route {
    Direct,
    Proxy { node_id: String, client: reqwest::Client },
}

/// 一次性探测客户端的选路结果。
pub enum ProbeRoute {
    Direct,
    Proxy(reqwest::Proxy),
    /// 代理池已开启但没有可用节点，且不允许回退直连。
    Blocked,
}

pub struct ProxyPool {
    db: SqlitePool,
    vault: Arc<dyn CredentialVault>,
    snap: RwLock<Arc<Snapshot>>,
    health: Mutex<HashMap<String, Health>>,
    clients: Mutex<HashMap<String, reqwest::Client>>,
    cursor: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random_index() -> usize {
    let mut b = [0u8; 4];
    rand::fill(&mut b[..]);
    u32::from_le_bytes(b) as usize
}

/// 按策略给出尝试顺序（下标）。`latencies[i]` 为第 i 个候选的最近测速。
pub fn order_indices(strategy: Strategy, latencies: &[Option<u64>], cursor: usize, random: usize) -> Vec<usize> {
    let n = latencies.len();
    if n == 0 {
        return Vec::new();
    }
    let rotate = |start: usize| (0..n).map(|i| (start % n + i) % n).collect::<Vec<_>>();
    match strategy {
        Strategy::RoundRobin => rotate(cursor),
        Strategy::Random => rotate(random),
        Strategy::Failover => (0..n).collect(),
        Strategy::LowestLatency => {
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by_key(|&i| (latencies[i].unwrap_or(u64::MAX), i));
            idx
        }
    }
}

fn describe_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "连接或响应超时".into()
    } else if e.is_connect() {
        "无法连接到代理，或代理拒绝了连接（请检查地址、端口与认证信息）".into()
    } else {
        "请求失败（代理或测速地址异常）".into()
    }
}

impl ProxyPool {
    pub fn new(db: SqlitePool, vault: Arc<dyn CredentialVault>) -> Self {
        Self {
            db,
            vault,
            snap: RwLock::new(Arc::new(Snapshot { config: PoolConfig::default(), nodes: Vec::new() })),
            health: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
            cursor: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 从数据库与凭据库重建内存快照。任何写操作之后都要调用。
    pub async fn reload(&self) -> AppResult<()> {
        let config = repo::load_config(&self.db).await?;
        let rows = repo::list_nodes(&self.db).await?;
        let mut nodes = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(scheme) = Scheme::parse(&row.scheme) else {
                tracing::warn!(node = %row.id, "代理节点协议无效，已忽略");
                continue;
            };
            let Ok(port) = u16::try_from(row.port) else { continue };
            let password = match row.credential_ref.as_deref() {
                Some(r) => vault::get(&self.vault, r).await.ok().flatten().filter(|p| !p.is_empty()).map(Secret::new),
                None => None,
            };
            nodes.push(Arc::new(NodeSpec {
                id: row.id,
                name: row.name,
                scheme,
                host: row.host,
                port,
                username: row.username,
                has_password: row.credential_ref.is_some(),
                password,
                enabled: row.enabled,
            }));
        }
        let ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        lock(&self.health).retain(|k, _| ids.contains(k.as_str()));
        lock(&self.clients).clear();
        *self.snap.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Snapshot { config, nodes });
        Ok(())
    }

    pub fn config(&self) -> PoolConfig {
        self.snapshot().config.clone()
    }

    pub fn state(&self) -> ProxyPoolState {
        let snap = self.snapshot();
        let now = Instant::now();
        let health = lock(&self.health);
        let nodes = snap
            .nodes
            .iter()
            .map(|n| {
                let h = health.get(&n.id).cloned().unwrap_or_default();
                ProxyNode {
                    id: n.id.clone(),
                    name: n.name.clone(),
                    scheme: n.scheme.as_str().into(),
                    host: n.host.clone(),
                    port: n.port,
                    username: n.username.clone(),
                    has_password: n.has_password,
                    enabled: n.enabled,
                    status: h.status(now),
                    latency_ms: h.latency_ms,
                    last_checked_at: h.last_checked_at.clone(),
                    last_error: h.last_error.clone(),
                    exit_ip: h.exit_ip.clone(),
                    success_count: h.ok,
                    failure_count: h.fail,
                    cooldown_remaining_seconds: h
                        .cooldown_until
                        .filter(|t| *t > now)
                        .map_or(0, |t| t.saturating_duration_since(now).as_secs() + 1),
                }
            })
            .collect();
        ProxyPoolState { config: snap.config.clone(), nodes }
    }

    // ───────── 选路 ─────────

    fn client_for(&self, spec: &NodeSpec) -> Option<reqwest::Client> {
        if let Some(c) = lock(&self.clients).get(&spec.id) {
            return Some(c.clone());
        }
        match spec.client(None) {
            Ok(c) => {
                lock(&self.clients).insert(spec.id.clone(), c.clone());
                Some(c)
            }
            Err(()) => {
                tracing::warn!(node = %spec.id, "无法为代理节点创建 HTTP 客户端");
                None
            }
        }
    }

    /// 候选节点（启用且不在冷却中），已按策略排序，并截断到 `max_attempts`。
    fn ordered_candidates(&self, snap: &Snapshot, now: Instant) -> Vec<Arc<NodeSpec>> {
        let cfg = &snap.config;
        let health = lock(&self.health);
        let cands: Vec<&Arc<NodeSpec>> =
            snap.nodes.iter().filter(|n| n.enabled && !health.get(&n.id).is_some_and(|h| h.cooling(now))).collect();
        let latencies: Vec<Option<u64>> = cands.iter().map(|n| health.get(&n.id).and_then(|h| h.latency_ms)).collect();
        drop(health);
        let cursor = if cfg.strategy == Strategy::RoundRobin { self.cursor.fetch_add(1, Ordering::Relaxed) } else { 0 };
        let random = if cfg.strategy == Strategy::Random { random_index() } else { 0 };
        order_indices(cfg.strategy, &latencies, cursor, random)
            .into_iter()
            .take(cfg.max_attempts as usize)
            .map(|i| cands[i].clone())
            .collect()
    }

    /// 一次出站请求的尝试顺序。空列表 = 代理池开启但无路可走，且不允许直连（应拒绝出站）。
    pub fn plan(&self, provider_id: &str) -> Vec<Route> {
        let snap = self.snapshot();
        let cfg = &snap.config;
        if !cfg.enabled || !cfg.applies_to(provider_id) {
            return vec![Route::Direct];
        }
        let mut routes: Vec<Route> = self
            .ordered_candidates(&snap, Instant::now())
            .into_iter()
            .filter_map(|n| self.client_for(&n).map(|client| Route::Proxy { node_id: n.id.clone(), client }))
            .collect();
        if cfg.fallback_direct {
            routes.push(Route::Direct);
        }
        routes
    }

    /// 给提测 / 拉模型 / 查配额这类一次性客户端选一个出口（不做故障转移，也不记录健康）。
    pub fn probe_route(&self, provider_id: &str) -> ProbeRoute {
        let snap = self.snapshot();
        let cfg = &snap.config;
        if !cfg.enabled || !cfg.applies_to(provider_id) {
            return ProbeRoute::Direct;
        }
        let first = self.ordered_candidates(&snap, Instant::now()).into_iter().find_map(|n| n.proxy().ok());
        match first {
            Some(p) => ProbeRoute::Proxy(p),
            None if cfg.fallback_direct => ProbeRoute::Direct,
            None => ProbeRoute::Blocked,
        }
    }

    // ───────── 健康 ─────────

    pub fn report_success(&self, node_id: &str) {
        let mut g = lock(&self.health);
        let h = g.entry(node_id.to_string()).or_default();
        h.consecutive_failures = 0;
        h.cooldown_until = None;
        h.last_error = None;
        h.ok += 1;
        h.seen_ok = true;
        h.last_checked_at = Some(now_ts());
    }

    pub fn report_failure(&self, node_id: &str, reason: &str) {
        let snap = self.snapshot();
        let mut g = lock(&self.health);
        let h = g.entry(node_id.to_string()).or_default();
        h.consecutive_failures = h.consecutive_failures.saturating_add(1);
        h.fail += 1;
        h.last_error = Some(reason.to_string());
        h.last_checked_at = Some(now_ts());
        if h.consecutive_failures >= snap.config.failure_threshold {
            h.cooldown_until = Some(Instant::now() + Duration::from_secs(u64::from(snap.config.cooldown_seconds)));
            tracing::warn!(node = %node_id, "代理节点连续失败，进入冷却");
        }
    }

    // ───────── 测速 ─────────

    async fn probe(spec: &NodeSpec, test_url: &str) -> Result<(u64, Option<String>), String> {
        let client = spec.client(Some(TEST_TIMEOUT)).map_err(|()| "代理地址无法用于创建客户端".to_string())?;
        let started = Instant::now();
        let mut resp = client.get(test_url).send().await.map_err(|e| describe_error(&e))?;
        let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = resp.status().as_u16();
        if status == 407 {
            return Err("代理要求认证（HTTP 407），请检查用户名和密码".into());
        }
        if status >= 500 {
            return Err(format!("测速地址返回 HTTP {status}"));
        }
        let mut body = Vec::new();
        while body.len() < 4096 {
            match tokio::time::timeout(Duration::from_secs(3), resp.chunk()).await {
                Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
                _ => break,
            }
        }
        let text = String::from_utf8_lossy(&body);
        let exit_ip = text
            .lines()
            .find_map(|l| l.strip_prefix("ip="))
            .map(str::trim)
            .filter(|ip| ip.parse::<IpAddr>().is_ok())
            .map(str::to_string);
        Ok((latency, exit_ip))
    }

    /// 测速：`ids` 为空 = 全部节点（含已停用的）。返回测速后的全部节点。
    pub async fn test(&self, ids: Option<Vec<String>>) -> Vec<ProxyNode> {
        let snap = self.snapshot();
        let wanted: Option<HashSet<String>> = ids.map(|v| v.into_iter().collect());
        let targets: Vec<Arc<NodeSpec>> =
            snap.nodes.iter().filter(|n| wanted.as_ref().is_none_or(|w| w.contains(&n.id))).cloned().collect();
        let sem = Arc::new(Semaphore::new(TEST_CONCURRENCY));
        let url = snap.config.test_url.clone();
        let jobs = targets.into_iter().map(|spec| {
            let sem = sem.clone();
            let url = url.clone();
            async move {
                let _permit = sem.acquire().await.ok();
                let result = Self::probe(&spec, &url).await;
                (spec.id.clone(), result)
            }
        });
        for (id, result) in futures_util::future::join_all(jobs).await {
            match result {
                Ok((latency, exit_ip)) => {
                    self.report_success(&id);
                    let mut g = lock(&self.health);
                    let h = g.entry(id).or_default();
                    h.latency_ms = Some(latency);
                    h.exit_ip = exit_ip;
                }
                Err(reason) => {
                    self.report_failure(&id, &reason);
                    let mut g = lock(&self.health);
                    let h = g.entry(id).or_default();
                    h.latency_ms = None;
                    h.exit_ip = None;
                }
            }
        }
        self.state().nodes
    }

    // ───────── 增删改 ─────────

    pub async fn save_config(&self, config: PoolConfig) -> AppResult<ProxyPoolState> {
        let config = validate_config(config)?;
        repo::save_config(&self.db, &config).await?;
        self.reload().await?;
        Ok(self.state())
    }

    fn credential_ref(id: &str) -> String {
        format!("proxy:{id}")
    }

    pub async fn save_node(&self, input: ProxyNodeInput) -> AppResult<ProxyPoolState> {
        let name = input.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(AppError::msg("名称不能为空且不超过 64 个字符"));
        }
        let scheme = Scheme::parse(&input.scheme).ok_or_else(|| AppError::msg("协议需为 http / https / socks5 / socks5h"))?;
        let host = parse::normalize_host(&input.host).map_err(AppError::msg)?;
        if input.port == 0 {
            return Err(AppError::msg("端口需在 1–65535 之间"));
        }
        let username = input.username.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
        if username.as_deref().is_some_and(|u| u.len() > 256) {
            return Err(AppError::msg("用户名过长"));
        }
        let now = now_ts();
        let existing = match input.id.as_deref() {
            Some(id) => Some(repo::get_node(&self.db, id).await?.ok_or_else(|| AppError::NotFound(format!("proxy node {id}")))?),
            None => None,
        };
        if existing.is_none() && repo::list_nodes(&self.db).await?.len() >= MAX_NODES {
            return Err(AppError::msg(format!("代理节点最多 {MAX_NODES} 个")));
        }
        let id = existing.as_ref().map_or_else(new_id, |e| e.id.clone());
        let cref = Self::credential_ref(&id);

        // 去掉用户名等于“没有账号”，同时丢弃密码
        let mut has_password = existing.as_ref().is_some_and(|e| e.credential_ref.is_some());
        match input.password.as_deref() {
            Some("") => has_password = false,
            Some(p) => {
                vault::set(&self.vault, &cref, p).await?;
                has_password = true;
            }
            None => {}
        }
        if username.is_none() {
            has_password = false;
        }
        if !has_password {
            vault::delete(&self.vault, &cref).await?;
        }

        let row = NodeRow {
            id: id.clone(),
            name,
            scheme: scheme.as_str().into(),
            host,
            port: i64::from(input.port),
            username,
            credential_ref: has_password.then_some(cref),
            enabled: input.enabled,
            sort_order: match &existing {
                Some(e) => e.sort_order,
                None => repo::next_sort_order(&self.db).await?,
            },
            created_at: existing.as_ref().map_or_else(|| now.clone(), |e| e.created_at.clone()),
            updated_at: now,
        };
        repo::upsert_node(&self.db, &row).await?;
        // 地址或认证变了，旧的健康记录不再可信
        lock(&self.health).remove(&id);
        self.reload().await?;
        Ok(self.state())
    }

    pub async fn delete_node(&self, id: &str) -> AppResult<ProxyPoolState> {
        if !repo::delete_node(&self.db, id).await? {
            return Err(AppError::NotFound(format!("proxy node {id}")));
        }
        vault::delete(&self.vault, &Self::credential_ref(id)).await?;
        self.reload().await?;
        Ok(self.state())
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> AppResult<ProxyPoolState> {
        if !repo::set_enabled(&self.db, id, enabled, &now_ts()).await? {
            return Err(AppError::NotFound(format!("proxy node {id}")));
        }
        self.reload().await?;
        Ok(self.state())
    }

    /// 在列表顺序里上移（delta < 0）/ 下移（delta > 0）一格。故障转移策略按这个顺序选路。
    pub async fn move_node(&self, id: &str, delta: i32) -> AppResult<ProxyPoolState> {
        let rows = repo::list_nodes(&self.db).await?;
        let pos = rows.iter().position(|r| r.id == id).ok_or_else(|| AppError::NotFound(format!("proxy node {id}")))?;
        let target = pos as i64 + i64::from(delta.signum());
        if target >= 0 && (target as usize) < rows.len() {
            let mut order: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
            order.swap(pos, target as usize);
            let mut tx = self.db.begin().await?;
            for (i, rid) in order.iter().enumerate() {
                sqlx::query("UPDATE proxy_nodes SET sort_order = ?2 WHERE id = ?1").bind(rid).bind(i as i64).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            self.reload().await?;
        }
        Ok(self.state())
    }

    /// 批量导入：每行一个代理。已存在（协议 + 主机 + 端口 + 用户名相同）的会被跳过。
    pub async fn import(&self, text: &str) -> AppResult<ImportResult> {
        let lines: Vec<(usize, &str)> =
            text.lines().enumerate().map(|(i, l)| (i + 1, l.trim())).filter(|(_, l)| !l.is_empty() && !l.starts_with('#')).collect();
        if lines.len() > MAX_IMPORT_LINES {
            return Err(AppError::msg(format!("一次最多导入 {MAX_IMPORT_LINES} 行")));
        }
        let existing = repo::list_nodes(&self.db).await?;
        let mut total = existing.len();
        let mut seen: HashSet<(String, String, i64, Option<String>)> =
            existing.iter().map(|r| (r.scheme.clone(), r.host.to_ascii_lowercase(), r.port, r.username.clone())).collect();
        let mut result = ImportResult { added: 0, skipped: Vec::new() };
        for (line_no, line) in lines {
            let skip = |reason: &str| ImportSkip { line: line_no, reason: reason.to_string() };
            let parsed = match parse::parse_line(line) {
                Ok(p) => p,
                Err(reason) => {
                    result.skipped.push(skip(&reason));
                    continue;
                }
            };
            let key = (parsed.scheme.as_str().to_string(), parsed.host.clone(), i64::from(parsed.port), parsed.username.clone());
            if !seen.insert(key) {
                result.skipped.push(skip("已存在相同的代理"));
                continue;
            }
            if total >= MAX_NODES {
                result.skipped.push(skip(&format!("代理节点已达上限 {MAX_NODES}")));
                continue;
            }
            self.save_node_unchecked(&parsed).await?;
            total += 1;
            result.added += 1;
        }
        self.reload().await?;
        Ok(result)
    }

    async fn save_node_unchecked(&self, p: &parse::ParsedNode) -> AppResult<()> {
        let id = new_id();
        let now = now_ts();
        let cref = (p.username.is_some() && p.password.is_some()).then(|| Self::credential_ref(&id));
        if let (Some(r), Some(pw)) = (&cref, &p.password) {
            vault::set(&self.vault, r, pw).await?;
        }
        let row = NodeRow {
            id,
            name: format!("{}:{}", p.host, p.port),
            scheme: p.scheme.as_str().into(),
            host: p.host.clone(),
            port: i64::from(p.port),
            username: p.username.clone(),
            credential_ref: cref,
            enabled: true,
            sort_order: repo::next_sort_order(&self.db).await?,
            created_at: now.clone(),
            updated_at: now,
        };
        repo::upsert_node(&self.db, &row).await
    }
}

// ───────────────────────── 全局入口（一次性探测客户端用） ─────────────────────────

static GLOBAL: OnceLock<Arc<ProxyPool>> = OnceLock::new();

/// 生产环境在 `AppState::init` 里安装；测试不安装，一次性客户端保持直连。
pub fn install_global(pool: Arc<ProxyPool>) {
    let _ = GLOBAL.set(pool);
}

/// 给提供商相关的一次性 HTTP 客户端（连接测试、拉取模型、配额）套上代理池选出的出口。
/// 返回 Err 表示代理池开启但无可用节点且不允许直连，调用方应报告“无法确认”而不是直连。
pub fn probe_builder(builder: reqwest::ClientBuilder, provider_id: &str) -> Result<reqwest::ClientBuilder, String> {
    let Some(pool) = GLOBAL.get() else { return Ok(builder) };
    match pool.probe_route(provider_id) {
        ProbeRoute::Direct => Ok(builder),
        ProbeRoute::Proxy(p) => Ok(builder.proxy(p)),
        ProbeRoute::Blocked => Err("代理池已开启但没有可用的代理节点，已阻止直连".to_string()),
    }
}

#[cfg(test)]
mod tests;

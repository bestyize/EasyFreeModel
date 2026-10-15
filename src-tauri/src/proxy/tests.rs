use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::security::MemoryVault;
use crate::upstream::{ClientOptions, UpstreamClient};

async fn setup() -> (Arc<ProxyPool>, Arc<MemoryVault>, SqlitePool) {
    let db = crate::store::open_memory().await.unwrap();
    let vault = Arc::new(MemoryVault::default());
    let pool = Arc::new(ProxyPool::new(db.clone(), vault.clone()));
    pool.reload().await.unwrap();
    (pool, vault, db)
}

fn input(name: &str, host: &str, port: u16) -> ProxyNodeInput {
    ProxyNodeInput {
        id: None,
        name: name.into(),
        scheme: "http".into(),
        host: host.into(),
        port,
        username: None,
        password: None,
        enabled: true,
    }
}

async fn enable(pool: &ProxyPool, f: impl FnOnce(&mut PoolConfig)) {
    let mut c = PoolConfig { enabled: true, ..Default::default() };
    f(&mut c);
    pool.save_config(c).await.unwrap();
}

fn proxy_ids(routes: &[Route]) -> Vec<String> {
    routes
        .iter()
        .map(|r| match r {
            Route::Direct => "direct".to_string(),
            Route::Proxy { node_id, .. } => node_id.clone(),
        })
        .collect()
}

/// 一个最小的 HTTP 代理替身：对任何请求回应指定状态与正文，并记录请求头。
async fn fake_proxy(body: &'static str, status: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut read = 0;
                loop {
                    let Ok(n) = sock.read(&mut buf[read..]).await else { return };
                    if n == 0 {
                        return;
                    }
                    read += n;
                    if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                log.lock().unwrap().push(String::from_utf8_lossy(&buf[..read]).to_string());
                let resp = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, seen)
}

async fn dead_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

// ───────── 纯函数 ─────────

#[test]
fn ordering_strategies() {
    let lat = [Some(300), None, Some(80)];
    assert_eq!(order_indices(Strategy::Failover, &lat, 7, 7), vec![0, 1, 2]);
    assert_eq!(order_indices(Strategy::RoundRobin, &lat, 0, 0), vec![0, 1, 2]);
    assert_eq!(order_indices(Strategy::RoundRobin, &lat, 1, 0), vec![1, 2, 0]);
    assert_eq!(order_indices(Strategy::RoundRobin, &lat, 5, 0), vec![2, 0, 1]);
    assert_eq!(order_indices(Strategy::Random, &lat, 0, 4), vec![1, 2, 0]);
    // 最低延迟：未测速的排最后
    assert_eq!(order_indices(Strategy::LowestLatency, &lat, 0, 0), vec![2, 0, 1]);
    assert!(order_indices(Strategy::RoundRobin, &[], 3, 3).is_empty());
}

#[test]
fn config_validation_and_scope() {
    assert!(validate_config(PoolConfig::default()).is_ok());
    for bad in [
        PoolConfig { failure_threshold: 0, ..Default::default() },
        PoolConfig { cooldown_seconds: 1, ..Default::default() },
        PoolConfig { max_attempts: 0, ..Default::default() },
        PoolConfig { max_attempts: 6, ..Default::default() },
        PoolConfig { test_url: "ftp://x".into(), ..Default::default() },
        PoolConfig { test_url: "not a url".into(), ..Default::default() },
    ] {
        assert!(validate_config(bad).is_err());
    }
    let c = validate_config(PoolConfig { provider_ids: vec![" a ".into(), "a".into(), "".into(), "b".into()], ..Default::default() }).unwrap();
    assert_eq!(c.provider_ids, vec!["a", "b"]);

    let inc = PoolConfig { provider_scope: ProviderScope::Include, provider_ids: vec!["openai".into()], ..Default::default() };
    assert!(inc.applies_to("openai") && !inc.applies_to("deepseek"));
    let exc = PoolConfig { provider_scope: ProviderScope::Exclude, provider_ids: vec!["deepseek".into()], ..Default::default() };
    assert!(!exc.applies_to("deepseek") && exc.applies_to("openai"));
    assert!(PoolConfig::default().applies_to("anything"));
}

#[test]
fn config_json_is_camel_case_and_tolerates_missing_fields() {
    let v = serde_json::to_value(PoolConfig::default()).unwrap();
    assert_eq!(v["strategy"], "roundRobin");
    assert_eq!(v["providerScope"], "all");
    let c: PoolConfig = serde_json::from_str(r#"{"enabled":true,"strategy":"lowestLatency"}"#).unwrap();
    assert!(c.enabled && c.strategy == Strategy::LowestLatency && c.cooldown_seconds == 60);
}

// ───────── 增删改与凭据 ─────────

#[tokio::test]
async fn node_crud_keeps_password_in_vault_only() {
    let (pool, vault, db) = setup().await;
    let mut inp = input("主力", "Proxy.Example.com", 8080);
    inp.username = Some("alice".into());
    inp.password = Some("s3cret".into());
    let st = pool.save_node(inp).await.unwrap();
    assert_eq!(st.nodes.len(), 1);
    let n = &st.nodes[0];
    assert_eq!((n.host.as_str(), n.has_password, n.status), ("proxy.example.com", true, NodeStatus::Untested));
    let json = serde_json::to_string(&st).unwrap();
    assert!(!json.contains("s3cret"), "密码不得出现在返回给前端的数据里");

    // 数据库里没有密码，只有引用；凭据库里有
    let row = repo::get_node(&db, &n.id).await.unwrap().unwrap();
    let cref = row.credential_ref.clone().unwrap();
    assert_eq!(vault.get(&cref).unwrap().as_deref(), Some("s3cret"));
    assert!(!format!("{row:?}").contains("s3cret"));

    // password = None 保持不变
    let mut upd = input("主力2", "proxy.example.com", 8081);
    upd.id = Some(n.id.clone());
    upd.username = Some("alice".into());
    let st = pool.save_node(upd).await.unwrap();
    assert_eq!((st.nodes[0].name.as_str(), st.nodes[0].port, st.nodes[0].has_password), ("主力2", 8081, true));
    assert_eq!(vault.get(&cref).unwrap().as_deref(), Some("s3cret"));

    // 清除用户名 = 没有认证，密码一并删除
    let mut upd = input("主力2", "proxy.example.com", 8081);
    upd.id = Some(n.id.clone());
    let st = pool.save_node(upd).await.unwrap();
    assert!(!st.nodes[0].has_password);
    assert_eq!(vault.get(&cref).unwrap(), None);

    // 删除
    let st = pool.delete_node(&n.id).await.unwrap();
    assert!(st.nodes.is_empty());
    assert!(pool.delete_node(&n.id).await.is_err());
}

#[tokio::test]
async fn node_validation() {
    let (pool, _, _) = setup().await;
    assert!(pool.save_node(input("", "h", 1)).await.is_err());
    assert!(pool.save_node(input("x", "bad host", 1)).await.is_err());
    assert!(pool.save_node(input("x", "h", 0)).await.is_err());
    let mut bad = input("x", "h", 1);
    bad.scheme = "ftp".into();
    assert!(pool.save_node(bad).await.is_err());
    let mut missing = input("x", "h", 1);
    missing.id = Some("nope".into());
    assert!(pool.save_node(missing).await.is_err());
}

#[tokio::test]
async fn move_and_enable() {
    let (pool, _, _) = setup().await;
    for (i, name) in ["a", "b", "c"].iter().enumerate() {
        pool.save_node(input(name, "h", 1000 + i as u16)).await.unwrap();
    }
    let names = |s: &ProxyPoolState| s.nodes.iter().map(|n| n.name.clone()).collect::<Vec<_>>();
    let st = pool.state();
    assert_eq!(names(&st), ["a", "b", "c"]);
    let c = st.nodes[2].id.clone();
    assert_eq!(names(&pool.move_node(&c, -1).await.unwrap()), ["a", "c", "b"]);
    assert_eq!(names(&pool.move_node(&c, -1).await.unwrap()), ["c", "a", "b"]);
    assert_eq!(names(&pool.move_node(&c, -1).await.unwrap()), ["c", "a", "b"], "到顶后不再移动");
    let st = pool.set_enabled(&c, false).await.unwrap();
    assert!(!st.nodes[0].enabled);
    assert!(pool.set_enabled("nope", true).await.is_err());
}

#[tokio::test]
async fn import_dedups_and_never_echoes_input() {
    let (pool, vault, _) = setup().await;
    pool.save_node(input("existing", "1.1.1.1", 80)).await.unwrap();
    let text = "\
# 注释
socks5://bob:hunter2@2.2.2.2:1080
1.1.1.1:80
3.3.3.3:3128:carol:pw
garbage-line-with-secret
socks5://bob:other@2.2.2.2:1080
";
    let r = pool.import(text).await.unwrap();
    assert_eq!(r.added, 2, "{:?}", r.skipped);
    assert_eq!(r.skipped.len(), 3);
    assert_eq!(r.skipped.iter().map(|s| s.line).collect::<Vec<_>>(), vec![3, 5, 6]);
    assert!(r.skipped[0].reason.contains("已存在"));
    let dump = serde_json::to_string(&r).unwrap();
    assert!(!dump.contains("secret") && !dump.contains("hunter2"));

    let st = pool.state();
    assert_eq!(st.nodes.len(), 3);
    let bob = st.nodes.iter().find(|n| n.username.as_deref() == Some("bob")).unwrap();
    assert_eq!((bob.scheme.as_str(), bob.has_password), ("socks5", true));
    assert_eq!(vault.get(&format!("proxy:{}", bob.id)).unwrap().as_deref(), Some("hunter2"));
}

// ───────── 选路与健康 ─────────

#[tokio::test]
async fn plan_respects_enabled_scope_and_fallback() {
    let (pool, _, _) = setup().await;
    let a = pool.save_node(input("a", "10.0.0.1", 1)).await.unwrap().nodes[0].id.clone();

    // 默认关闭 -> 直连
    assert_eq!(proxy_ids(&pool.plan("openai")), ["direct"]);

    enable(&pool, |_| {}).await;
    assert_eq!(proxy_ids(&pool.plan("openai")), [a.clone()]);

    enable(&pool, |c| {
        c.provider_scope = ProviderScope::Exclude;
        c.provider_ids = vec!["deepseek".into()];
    })
    .await;
    assert_eq!(proxy_ids(&pool.plan("deepseek")), ["direct"]);
    assert_eq!(proxy_ids(&pool.plan("openai")), [a.clone()]);

    // 节点被停用：默认拒绝出站（空），打开回退才直连
    pool.set_enabled(&a, false).await.unwrap();
    assert!(pool.plan("openai").is_empty());
    enable(&pool, |c| c.fallback_direct = true).await;
    assert_eq!(proxy_ids(&pool.plan("openai")), ["direct"]);
}

#[tokio::test]
async fn round_robin_failover_and_attempt_cap() {
    let (pool, _, _) = setup().await;
    let mut ids = Vec::new();
    for i in 0..3u16 {
        let st = pool.save_node(input(&format!("n{i}"), "10.0.0.1", 100 + i)).await.unwrap();
        ids.push(st.nodes[i as usize].id.clone());
    }
    enable(&pool, |c| {
        c.max_attempts = 3;
        c.fallback_direct = true;
    })
    .await;
    let first = |r: Vec<Route>| proxy_ids(&r)[0].clone();
    let order: Vec<String> = (0..4).map(|_| first(pool.plan("p"))).collect();
    assert_eq!(order, [ids[0].clone(), ids[1].clone(), ids[2].clone(), ids[0].clone()]);

    enable(&pool, |c| {
        c.strategy = Strategy::Failover;
        c.max_attempts = 2;
    })
    .await;
    assert_eq!(proxy_ids(&pool.plan("p")), [ids[0].clone(), ids[1].clone()], "按列表顺序，截断到 maxAttempts");
}

#[tokio::test]
async fn cooldown_after_threshold_then_half_open() {
    let (pool, _, _) = setup().await;
    let a = pool.save_node(input("a", "10.0.0.1", 1)).await.unwrap().nodes[0].id.clone();
    let b = pool.save_node(input("b", "10.0.0.2", 2)).await.unwrap().nodes[1].id.clone();
    enable(&pool, |c| {
        c.strategy = Strategy::Failover;
        c.failure_threshold = 2;
        c.cooldown_seconds = 60;
    })
    .await;

    pool.report_failure(&a, "x");
    assert_eq!(proxy_ids(&pool.plan("p"))[0], a, "未到阈值仍在轮换里");
    assert_eq!(pool.state().nodes[0].status, NodeStatus::Unhealthy);
    pool.report_failure(&a, "x");
    let st = pool.state();
    assert_eq!(st.nodes[0].status, NodeStatus::Cooldown);
    assert!(st.nodes[0].cooldown_remaining_seconds > 0);
    assert_eq!(proxy_ids(&pool.plan("p")), [b.clone()], "冷却中的节点被跳过");

    // 成功一次即恢复
    pool.report_success(&a);
    assert_eq!(pool.state().nodes[0].status, NodeStatus::Healthy);
    assert_eq!(proxy_ids(&pool.plan("p"))[0], a);

    // 半开：冷却到期后再失败一次立刻重新冷却
    pool.report_failure(&a, "x");
    pool.report_failure(&a, "x");
    {
        let mut g = lock(&pool.health);
        g.get_mut(&a).unwrap().cooldown_until = Some(Instant::now() - Duration::from_secs(1));
    }
    assert_eq!(proxy_ids(&pool.plan("p"))[0], a, "冷却结束后重新进入候选");
    pool.report_failure(&a, "x");
    assert_eq!(pool.state().nodes[0].status, NodeStatus::Cooldown);
}

#[tokio::test]
async fn lowest_latency_uses_measured_order() {
    let (pool, _, _) = setup().await;
    let a = pool.save_node(input("a", "10.0.0.1", 1)).await.unwrap().nodes[0].id.clone();
    let b = pool.save_node(input("b", "10.0.0.2", 2)).await.unwrap().nodes[1].id.clone();
    enable(&pool, |c| c.strategy = Strategy::LowestLatency).await;
    {
        let mut g = lock(&pool.health);
        g.entry(a.clone()).or_default().latency_ms = Some(500);
        g.entry(b.clone()).or_default().latency_ms = Some(40);
    }
    assert_eq!(proxy_ids(&pool.plan("p"))[0], b);
}

// ───────── 通过替身代理的端到端 ─────────

fn http_client(pool: &Arc<ProxyPool>) -> UpstreamClient {
    UpstreamClient::with_options(ClientOptions { connect_timeout: Duration::from_secs(2), no_proxy: true }).unwrap().with_pool(pool.clone())
}

#[tokio::test]
async fn upstream_client_fails_over_to_next_proxy() {
    let (pool, _, _) = setup().await;
    let (good_port, seen) = fake_proxy("pong", "200 OK").await;
    let a = pool.save_node(input("dead", "127.0.0.1", dead_port().await)).await.unwrap().nodes[0].id.clone();
    let b = pool.save_node(input("good", "127.0.0.1", good_port)).await.unwrap().nodes[1].id.clone();
    enable(&pool, |c| {
        c.strategy = Strategy::Failover;
        c.failure_threshold = 5;
    })
    .await;

    let client = http_client(&pool);
    let url = Url::parse("http://upstream.test/v1/models").unwrap();
    let resp = client.get("p", url, reqwest::header::HeaderMap::new(), Duration::from_secs(5)).await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "pong");

    let st = pool.state();
    let (na, nb) = (&st.nodes[0], &st.nodes[1]);
    assert_eq!((na.id.as_str(), na.failure_count, na.status), (a.as_str(), 1, NodeStatus::Unhealthy));
    assert_eq!((nb.id.as_str(), nb.success_count, nb.status), (b.as_str(), 1, NodeStatus::Healthy));
    let head = seen.lock().unwrap()[0].clone();
    assert!(head.starts_with("GET http://upstream.test/v1/models"), "经代理时请求行应是绝对地址: {head}");
}

#[tokio::test]
async fn upstream_client_blocks_direct_when_all_proxies_down() {
    let (pool, _, _) = setup().await;
    // 一个直连就能成功的“上游”，用来证明没有悄悄直连
    let (direct_port, direct_seen) = fake_proxy("direct", "200 OK").await;
    pool.save_node(input("dead", "127.0.0.1", dead_port().await)).await.unwrap();
    enable(&pool, |_| {}).await;

    let client = http_client(&pool);
    let url = Url::parse(&format!("http://127.0.0.1:{direct_port}/x")).unwrap();
    let err = client.get("p", url.clone(), reqwest::header::HeaderMap::new(), Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(err, crate::upstream::UpstreamError::Connect), "{err:?}");
    assert!(direct_seen.lock().unwrap().is_empty(), "不允许回退直连时不得直连");

    // 打开回退直连后才会直连成功
    enable(&pool, |c| c.fallback_direct = true).await;
    let resp = client.get("p", url, reqwest::header::HeaderMap::new(), Duration::from_secs(5)).await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "direct");
}

#[tokio::test]
async fn upstream_client_without_pool_is_direct() {
    let (port, seen) = fake_proxy("hi", "200 OK").await;
    let client = UpstreamClient::with_options(ClientOptions { connect_timeout: Duration::from_secs(2), no_proxy: true }).unwrap();
    let url = Url::parse(&format!("http://127.0.0.1:{port}/x")).unwrap();
    let resp = client.get("p", url, reqwest::header::HeaderMap::new(), Duration::from_secs(5)).await.unwrap();
    assert_eq!(resp.text().await.unwrap(), "hi");
    assert!(seen.lock().unwrap()[0].starts_with("GET /x"));
}

#[tokio::test]
async fn test_measures_latency_and_exit_ip() {
    let (pool, _, _) = setup().await;
    let (good, _) = fake_proxy("fl=1\nip=203.0.113.9\nloc=US\n", "200 OK").await;
    let (auth, _) = fake_proxy("", "407 Proxy Authentication Required").await;
    pool.save_node(input("dead", "127.0.0.1", dead_port().await)).await.unwrap();
    pool.save_node(input("good", "127.0.0.1", good)).await.unwrap();
    pool.save_node(input("auth", "127.0.0.1", auth)).await.unwrap();
    let mut c = PoolConfig { enabled: true, test_url: "http://probe.test/trace".into(), ..Default::default() };
    c.failure_threshold = 1;
    pool.save_config(c).await.unwrap();

    let nodes = pool.test(None).await;
    assert_eq!(nodes.len(), 3);
    let dead = &nodes[0];
    assert_eq!(dead.status, NodeStatus::Cooldown, "阈值为 1，测速失败即冷却");
    assert!(dead.last_error.as_deref().unwrap().contains("无法连接"));
    let good = &nodes[1];
    assert_eq!((good.status, good.exit_ip.as_deref()), (NodeStatus::Healthy, Some("203.0.113.9")));
    assert!(good.latency_ms.is_some() && good.last_checked_at.is_some());
    assert!(nodes[2].last_error.as_deref().unwrap().contains("407"));

    // 只测指定节点
    let only = pool.test(Some(vec![nodes[1].id.clone()])).await;
    assert_eq!(only.len(), 3);
}

#[tokio::test]
async fn reload_prunes_health_of_deleted_nodes() {
    let (pool, _, db) = setup().await;
    let a = pool.save_node(input("a", "10.0.0.1", 1)).await.unwrap().nodes[0].id.clone();
    pool.report_failure(&a, "x");
    repo::delete_node(&db, &a).await.unwrap();
    pool.reload().await.unwrap();
    assert!(lock(&pool.health).is_empty());
}

#[tokio::test]
async fn corrupted_stored_config_falls_back_to_disabled_default() {
    let (pool, _, db) = setup().await;
    sqlx::query("INSERT INTO app_settings (key, value) VALUES ('proxyPoolConfig', 'not json')").execute(&db).await.unwrap();
    pool.reload().await.unwrap();
    assert!(!pool.config().enabled);
}

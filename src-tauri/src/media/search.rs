//! 搜索与网页抓取适配器。
//!
//! 搜索结果统一成 `{origin, provider, results:[{title,url,snippet}]}`，序列化后放进 MediaJob.summary
//! （前端按此约定解析）。只保留 http/https 链接；`origin` 为 `native`（上游搜索接口直接返回）或
//! `aggregated`（本应用合并多个来源）。抓取（Firecrawl / Jina Reader / Tavily Extract / Exa Contents）
//! 把正文存成受控目录里的 `.md` 文件，summary 里放标题/地址/开头摘要。

use async_trait::async_trait;
use reqwest::Url;
use serde::Serialize;
use serde_json::{json, Value};

use super::adapter::util::{strip_tags, truncate_chars};
use super::adapter::{AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, OutputItem, Submission};
use super::http::{read_limited, MAX_JSON_BYTES};
use super::registry::Capability;

pub const DEFAULT_COUNT: usize = 5;
pub const MAX_COUNT: usize = 20;
const MAX_TITLE_CHARS: usize = 300;
const MAX_SNIPPET_CHARS: usize = 1000;
const MAX_FETCH_BYTES: usize = 4 * 1024 * 1024;
const FETCH_SNIPPET_CHARS: usize = 300;

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![
        Box::new(SerperSearch),
        Box::new(BraveSearch),
        Box::new(TavilySearch),
        Box::new(ExaSearch),
        Box::new(LinkupSearch),
        Box::new(PerplexitySearch),
        Box::new(YouComSearch),
        Box::new(SearchApiSearch),
        Box::new(OllamaSearch),
        Box::new(FirecrawlScrape),
        Box::new(JinaReader),
        Box::new(TavilyExtract),
        Box::new(ExaContents),
    ]
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl SearchHit {
    /// 非 http(s) 链接（`javascript:`、`file:` 等）返回 None。
    pub fn new(title: &str, url: &str, snippet: &str) -> Option<SearchHit> {
        let parsed = Url::parse(url.trim()).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return None;
        }
        let clean = |s: &str, max: usize| {
            let flat = strip_tags(s).split_whitespace().collect::<Vec<_>>().join(" ");
            truncate_chars(&flat, max)
        };
        let title = clean(title, MAX_TITLE_CHARS);
        Some(SearchHit {
            title: if title.is_empty() { parsed.to_string() } else { title },
            url: parsed.to_string(),
            snippet: clean(snippet, MAX_SNIPPET_CHARS),
        })
    }
}

#[derive(Serialize)]
struct Payload<'a> {
    origin: &'a str,
    provider: &'a str,
    results: &'a [SearchHit],
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<FailedProvider>,
}

#[derive(Serialize, Clone)]
pub struct FailedProvider {
    pub provider: String,
    pub error: String,
}

/// 序列化成放进 summary 的 JSON。
pub fn payload_json(origin: &str, provider: &str, results: &[SearchHit], failed: Vec<FailedProvider>) -> String {
    serde_json::to_string(&Payload { origin, provider, results, failed }).unwrap_or_else(|_| "{}".into())
}

pub fn native_outcome(provider: &str, hits: Vec<SearchHit>, count: usize) -> Outcome {
    let hits: Vec<SearchHit> = hits.into_iter().take(count).collect();
    Outcome::text(payload_json("native", provider, &hits, Vec::new()))
}

/// 合并多个来源的结果：按 URL 去重，各来源轮流取，保持各自的相对顺序。
pub fn merge_hits(lists: Vec<Vec<SearchHit>>, count: usize) -> Vec<SearchHit> {
    let mut out: Vec<SearchHit> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let max_len = lists.iter().map(Vec::len).max().unwrap_or(0);
    for i in 0..max_len {
        for l in &lists {
            if let Some(h) = l.get(i) {
                let key = h.url.trim_end_matches('/').to_ascii_lowercase();
                if seen.insert(key) {
                    out.push(h.clone());
                }
            }
        }
    }
    out.truncate(count);
    out
}

struct Query<'a> {
    text: &'a str,
    count: usize,
    region: Option<String>,
    time_range: Option<&'static str>,
}

fn query<'a>(cx: &AdapterCtx<'_>, req: &'a MediaRequest) -> Result<Query<'a>, MediaError> {
    let text = req.require_str("query", "查询内容")?;
    if text.chars().count() > 500 {
        return Err(MediaError::InvalidParams("查询内容不能超过 500 个字符".into()));
    }
    let count = if cx.cfg.has_param("count") {
        match req.f64_param("count") {
            None => DEFAULT_COUNT,
            Some(c) if c >= 1.0 && c <= MAX_COUNT as f64 && c.fract() == 0.0 => c as usize,
            Some(_) => return Err(MediaError::InvalidParams(format!("结果数量需在 1–{MAX_COUNT} 之间"))),
        }
    } else {
        DEFAULT_COUNT
    };
    let region = if cx.cfg.has_param("region") {
        match req.str_param("region") {
            Some(r) if r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()) => Some(r.to_ascii_uppercase()),
            Some(_) => return Err(MediaError::InvalidParams("地域应为两位国家/地区代码，例如 US".into())),
            None => None,
        }
    } else {
        None
    };
    let time_range = if cx.cfg.has_param("timeRange") {
        match req.str_param("timeRange") {
            Some("day") => Some("day"),
            Some("week") => Some("week"),
            Some("month") => Some("month"),
            Some("year") => Some("year"),
            Some(_) => return Err(MediaError::InvalidParams("时间范围无效".into())),
            None => None,
        }
    } else {
        None
    };
    Ok(Query { text, count, region, time_range })
}

fn with_query(base: &str, pairs: &[(&str, String)]) -> Result<String, MediaError> {
    let mut u = Url::parse(base).map_err(|_| MediaError::InvalidParams("供应商 baseUrl 无效".into()))?;
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(k, v);
        }
    }
    Ok(u.to_string())
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn hits_from<'a>(items: Option<&'a Vec<Value>>, f: impl Fn(&'a Value) -> Option<SearchHit>) -> Vec<SearchHit> {
    items.map(|a| a.iter().filter_map(f).collect()).unwrap_or_default()
}

fn done(provider_id: &str, hits: Vec<SearchHit>, q: &Query<'_>) -> Result<Submission, MediaError> {
    Ok(Submission::Done(native_outcome(provider_id, hits, q.count)))
}

// ---------------------------------------------------------------------------------------------
// 搜索
// ---------------------------------------------------------------------------------------------

pub struct SerperSearch;

#[async_trait]
impl MediaAdapter for SerperSearch {
    fn format(&self) -> &'static str {
        "serper-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut body = json!({"q": q.text, "num": q.count});
        if let Some(r) = &q.region {
            body["gl"] = json!(r.to_ascii_lowercase());
        }
        if let Some(t) = q.time_range {
            body["tbs"] = json!(match t {
                "day" => "qdr:d",
                "week" => "qdr:w",
                "month" => "qdr:m",
                _ => "qdr:y",
            });
        }
        let url = format!("{}/search", cx.cfg.base_url.trim_end_matches('/'));
        let v = cx.http.send_json(cx.provider_id, cx.post(&url)?.json(&body)).await?;
        let hits = hits_from(v.get("organic").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "link"), s(i, "snippet")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct BraveSearch;

#[async_trait]
impl MediaAdapter for BraveSearch {
    fn format(&self) -> &'static str {
        "brave-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut pairs = vec![("q", q.text.to_string()), ("count", q.count.to_string())];
        if let Some(r) = &q.region {
            pairs.push(("country", r.clone()));
        }
        if let Some(t) = q.time_range {
            pairs.push(("freshness", match t {
                "day" => "pd",
                "week" => "pw",
                "month" => "pm",
                _ => "py",
            }
            .to_string()));
        }
        let url = with_query(&format!("{}/web/search", cx.cfg.base_url.trim_end_matches('/')), &pairs)?;
        let v = cx.http.send_json(cx.provider_id, cx.get(&url)?.header(reqwest::header::ACCEPT, "application/json")).await?;
        let hits = hits_from(v.pointer("/web/results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "url"), s(i, "description")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct TavilySearch;

#[async_trait]
impl MediaAdapter for TavilySearch {
    fn format(&self) -> &'static str {
        "tavily-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut body = json!({"query": q.text, "max_results": q.count, "topic": "general"});
        if let Some(t) = q.time_range {
            body["time_range"] = json!(t);
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let hits = hits_from(v.get("results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "url"), s(i, "content")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct ExaSearch;

#[async_trait]
impl MediaAdapter for ExaSearch {
    fn format(&self) -> &'static str {
        "exa-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let body = json!({"query": q.text, "numResults": q.count, "type": "auto", "contents": {"text": {"maxCharacters": 500}}});
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let hits = hits_from(v.get("results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "url"), s(i, "text")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct LinkupSearch;

#[async_trait]
impl MediaAdapter for LinkupSearch {
    fn format(&self) -> &'static str {
        "linkup-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut body = json!({"q": q.text, "depth": "standard", "outputType": "searchResults", "maxResults": q.count});
        if let Some(t) = q.time_range {
            let now = chrono::Utc::now();
            let days = match t {
                "day" => 1,
                "week" => 7,
                "month" => 30,
                _ => 365,
            };
            body["fromDate"] = json!((now - chrono::Duration::days(days)).format("%Y-%m-%d").to_string());
            body["toDate"] = json!(now.format("%Y-%m-%d").to_string());
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let hits = hits_from(v.get("results").and_then(Value::as_array), |i| {
            let title = if s(i, "name").is_empty() { s(i, "title") } else { s(i, "name") };
            SearchHit::new(title, s(i, "url"), s(i, "content"))
        });
        done(cx.provider_id, hits, &q)
    }
}

pub struct PerplexitySearch;

#[async_trait]
impl MediaAdapter for PerplexitySearch {
    fn format(&self) -> &'static str {
        "perplexity-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut body = json!({"query": q.text, "max_results": q.count});
        if let Some(r) = &q.region {
            body["country"] = json!(r);
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let hits = hits_from(v.get("results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "url"), s(i, "snippet")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct YouComSearch;

#[async_trait]
impl MediaAdapter for YouComSearch {
    fn format(&self) -> &'static str {
        "youcom-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut pairs = vec![("query", q.text.to_string()), ("count", q.count.to_string())];
        if let Some(r) = &q.region {
            pairs.push(("country", r.clone()));
        }
        if let Some(t) = q.time_range {
            pairs.push(("freshness", t.to_string()));
        }
        let url = with_query(&cx.cfg.base_url, &pairs)?;
        let v = cx.http.send_json(cx.provider_id, cx.get(&url)?).await?;
        let hits = hits_from(v.pointer("/results/web").and_then(Value::as_array), |i| {
            let snippet = i.get("snippets").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str).unwrap_or_else(|| s(i, "description"));
            SearchHit::new(s(i, "title"), s(i, "url"), snippet)
        });
        done(cx.provider_id, hits, &q)
    }
}

pub struct SearchApiSearch;

#[async_trait]
impl MediaAdapter for SearchApiSearch {
    fn format(&self) -> &'static str {
        "searchapi-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let mut pairs = vec![("engine", "google".to_string()), ("q", q.text.to_string()), ("num", q.count.to_string())];
        if let Some(r) = &q.region {
            pairs.push(("gl", r.to_ascii_lowercase()));
        }
        if let Some(t) = q.time_range {
            pairs.push(("time_period", format!("last_{t}")));
        }
        let url = with_query(&cx.cfg.base_url, &pairs)?; // api_key 由鉴权方式 query 并入
        let v = cx.http.send_json(cx.provider_id, cx.get(&url)?).await?;
        let hits = hits_from(v.get("organic_results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "link"), s(i, "snippet")));
        done(cx.provider_id, hits, &q)
    }
}

pub struct OllamaSearch;

#[async_trait]
impl MediaAdapter for OllamaSearch {
    fn format(&self) -> &'static str {
        "ollama-search"
    }
    fn capability(&self) -> Capability {
        Capability::Search
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let q = query(cx, req)?;
        let body = json!({"query": q.text, "max_results": q.count.min(10)});
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let hits = hits_from(v.get("results").and_then(Value::as_array), |i| SearchHit::new(s(i, "title"), s(i, "url"), s(i, "content")));
        done(cx.provider_id, hits, &q)
    }
}

// ---------------------------------------------------------------------------------------------
// 网页抓取
// ---------------------------------------------------------------------------------------------

fn fetch_url(req: &MediaRequest) -> Result<String, MediaError> {
    let raw = req.require_str("url", "网址")?;
    if raw.len() > 2048 {
        return Err(MediaError::InvalidParams("网址过长".into()));
    }
    let u = Url::parse(raw).map_err(|_| MediaError::InvalidParams("网址格式无效".into()))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(MediaError::InvalidParams("只支持 http/https 网址".into()));
    }
    Ok(u.to_string())
}

/// 抓取结果：正文存成 `.md` 文件，summary 里放标题/地址/开头摘要。
fn fetch_outcome(provider_id: &str, url: &str, title: &str, content: &str) -> Result<Submission, MediaError> {
    let content = content.trim();
    if content.is_empty() {
        return Err(MediaError::Failed("供应商没有返回可提取的正文".into()));
    }
    let mut bytes = content.as_bytes().to_vec();
    if bytes.len() > MAX_FETCH_BYTES {
        let mut cut = MAX_FETCH_BYTES;
        while !content.is_char_boundary(cut) {
            cut -= 1;
        }
        bytes.truncate(cut);
    }
    let hit = SearchHit::new(title, url, &truncate_chars(content, FETCH_SNIPPET_CHARS))
        .ok_or_else(|| MediaError::BadResponse("抓取目标不是 http/https 地址".into()))?;
    Ok(Submission::Done(Outcome {
        outputs: vec![OutputItem::Bytes { data: bytes, ext: "md".into() }],
        text: Some(payload_json("native", provider_id, &[hit], Vec::new())),
    }))
}

pub struct FirecrawlScrape;

#[async_trait]
impl MediaAdapter for FirecrawlScrape {
    fn format(&self) -> &'static str {
        "firecrawl-scrape"
    }
    fn capability(&self) -> Capability {
        Capability::Fetch
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let url = fetch_url(req)?;
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&json!({"url": url, "formats": ["markdown"]}))).await?;
        if v.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(MediaError::Failed(format!("Firecrawl 抓取失败：{}", truncate_chars(s(&v, "error"), 200))));
        }
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        fetch_outcome(cx.provider_id, &url, data.pointer("/metadata/title").and_then(Value::as_str).unwrap_or(""), s(&data, "markdown"))
    }
}

pub struct JinaReader;

#[async_trait]
impl MediaAdapter for JinaReader {
    fn format(&self) -> &'static str {
        "jina-reader"
    }
    fn capability(&self) -> Capability {
        Capability::Fetch
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let url = fetch_url(req)?;
        let rb = cx.post(&cx.cfg.base_url)?.header(reqwest::header::ACCEPT, "application/json").json(&json!({"url": url}));
        let resp = cx.http.send(cx.provider_id, rb).await?;
        let bytes = read_limited(resp, MAX_JSON_BYTES).await?;
        // JSON 模式：{ data: { title, content } }；否则按纯文本（Markdown）处理
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            let data = v.get("data").cloned().unwrap_or(v);
            return fetch_outcome(cx.provider_id, &url, s(&data, "title"), s(&data, "content"));
        }
        fetch_outcome(cx.provider_id, &url, "", &String::from_utf8_lossy(&bytes))
    }
}

pub struct TavilyExtract;

#[async_trait]
impl MediaAdapter for TavilyExtract {
    fn format(&self) -> &'static str {
        "tavily-extract"
    }
    fn capability(&self) -> Capability {
        Capability::Fetch
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let url = fetch_url(req)?;
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&json!({"urls": [url], "extract_depth": "basic"}))).await?;
        if let Some(first) = v.pointer("/results/0") {
            return fetch_outcome(cx.provider_id, &url, "", s(first, "raw_content"));
        }
        let why = v.pointer("/failed_results/0/error").and_then(Value::as_str).unwrap_or("未返回结果");
        Err(MediaError::Failed(format!("Tavily 抓取失败：{}", truncate_chars(why, 200))))
    }
}

pub struct ExaContents;

#[async_trait]
impl MediaAdapter for ExaContents {
    fn format(&self) -> &'static str {
        "exa-contents"
    }
    fn capability(&self) -> Capability {
        Capability::Fetch
    }
    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let url = fetch_url(req)?;
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&json!({"urls": [url], "text": true}))).await?;
        if let Some(first) = v.pointer("/results/0") {
            return fetch_outcome(cx.provider_id, &url, s(first, "title"), s(first, "text"));
        }
        let why = v.pointer("/statuses/0/error/tag").and_then(Value::as_str).unwrap_or("未返回结果");
        Err(MediaError::Failed(format!("Exa 抓取失败：{}", truncate_chars(why, 200))))
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn hits_keep_only_http_links_and_clean_text() {
        assert!(SearchHit::new("t", "javascript:alert(1)", "s").is_none());
        assert!(SearchHit::new("t", "file:///etc/passwd", "s").is_none());
        assert!(SearchHit::new("t", "data:text/html;base64,AAAA", "s").is_none());
        assert!(SearchHit::new("t", "ftp://example.com/a", "s").is_none());
        assert!(SearchHit::new("t", "not a url", "s").is_none());
        let h = SearchHit::new("  <b>Hello</b>\n world ", "https://example.com/a?b=1", "a <strong>b</strong>\t c").unwrap();
        assert_eq!(h.title, "Hello world");
        assert_eq!(h.snippet, "a b c");
        assert_eq!(h.url, "https://example.com/a?b=1");
        // 标题为空时回落到 URL
        assert_eq!(SearchHit::new("", "http://example.com/", "").unwrap().title, "http://example.com/");
        assert!(SearchHit::new(&"长".repeat(1000), "https://e.com", "").unwrap().title.chars().count() <= MAX_TITLE_CHARS + 1);
    }

    #[test]
    fn payload_matches_frontend_contract() {
        let hit = SearchHit::new("T", "https://e.com/", "S").unwrap();
        let v: Value = serde_json::from_str(&payload_json("native", "tavily", std::slice::from_ref(&hit), vec![])).unwrap();
        assert_eq!(v["origin"], "native");
        assert_eq!(v["provider"], "tavily");
        assert_eq!(v["results"][0], json!({"title": "T", "url": "https://e.com/", "snippet": "S"}));
        assert!(v.get("failed").is_none());
        let v: Value = serde_json::from_str(&payload_json("aggregated", "a+b", &[hit], vec![FailedProvider { provider: "c".into(), error: "x".into() }])).unwrap();
        assert_eq!(v["origin"], "aggregated");
        assert_eq!(v["failed"][0]["provider"], "c");
    }

    #[test]
    fn merge_dedupes_by_url_and_interleaves() {
        let h = |u: &str| SearchHit::new(u, &format!("https://{u}.com/"), "").unwrap();
        let merged = merge_hits(vec![vec![h("a"), h("b"), h("c")], vec![h("b"), h("d")]], 10);
        let urls: Vec<_> = merged.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(urls, ["a", "b", "d", "c"]);
        assert_eq!(merge_hits(vec![vec![h("a"), h("b")], vec![h("c")]], 2).len(), 2);
        assert!(merge_hits(vec![], 5).is_empty());
    }
}

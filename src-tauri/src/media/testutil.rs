//! 测试辅助：用 axum 在随机端口起一个假上游，记录请求并按脚本回复。仅编译进测试。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::http::MediaHttp;

/// 退出作用域时删除的临时目录。
pub struct TempDir(PathBuf);

impl Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn tempdir() -> TempDir {
    let p = std::env::temp_dir().join(format!("router-media-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    TempDir(p)
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    /// 请求头名一律小写。
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

impl Part {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|i| &hay[*i..*i + needle.len()] == needle)
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("请求体不是 JSON：{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    pub fn query(&self, key: &str) -> Option<String> {
        let q = self.query.as_deref()?;
        let u = reqwest::Url::parse(&format!("http://x/?{q}")).ok()?;
        let found = u.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.to_string());
        found
    }

    /// 解析 multipart/form-data 请求体。
    pub fn multipart(&self) -> Vec<Part> {
        let ct = self.header("content-type").unwrap_or("");
        let boundary = ct.split("boundary=").nth(1).unwrap_or("").trim_matches('"').to_string();
        assert!(!boundary.is_empty(), "不是 multipart 请求：{ct}");
        let delim = format!("--{boundary}").into_bytes();
        let mut parts = Vec::new();
        let mut pos = find(&self.body, &delim, 0).expect("缺少 boundary");
        loop {
            let start = pos + delim.len();
            if self.body[start..].starts_with(b"--") {
                break;
            }
            let start = start + 2; // 跳过 \r\n
            let next = find(&self.body, &delim, start).expect("multipart 未结束");
            let seg = &self.body[start..next - 2]; // 去掉结尾 \r\n
            let split = find(seg, b"\r\n\r\n", 0).expect("multipart 缺少头部");
            let head = String::from_utf8_lossy(&seg[..split]).to_string();
            let mut part = Part { name: String::new(), filename: None, content_type: None, data: seg[split + 4..].to_vec() };
            for line in head.lines() {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("content-disposition:") {
                    for kv in line.split(';').skip(1) {
                        if let Some((k, v)) = kv.trim().split_once('=') {
                            let v = v.trim_matches('"').to_string();
                            match k {
                                "name" => part.name = v,
                                "filename" => part.filename = Some(v),
                                _ => {}
                            }
                        }
                    }
                } else if lower.starts_with("content-type:") {
                    part.content_type = Some(line["content-type:".len()..].trim().to_string());
                }
            }
            parts.push(part);
            pos = next;
        }
        parts
    }

    pub fn field(&self, name: &str) -> Option<Part> {
        self.multipart().into_iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Duration,
}

impl Reply {
    pub fn json(status: u16, v: Value) -> Reply {
        Reply { status, headers: vec![("content-type".into(), "application/json".into())], body: v.to_string().into_bytes(), delay: Duration::ZERO }
    }

    pub fn ok(v: Value) -> Reply {
        Reply::json(200, v)
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
        Reply { status, headers: vec![("content-type".into(), content_type.into())], body, delay: Duration::ZERO }
    }

    pub fn text(status: u16, body: &str) -> Reply {
        Reply::bytes(status, "text/plain", body.as_bytes().to_vec())
    }

    pub fn redirect(location: &str) -> Reply {
        Reply { status: 302, headers: vec![("location".into(), location.into())], body: Vec::new(), delay: Duration::ZERO }
    }

    pub fn delayed(mut self, d: Duration) -> Reply {
        self.delay = d;
        self
    }
}

type Script = dyn Fn(&Recorded) -> Reply + Send + Sync + 'static;

struct Inner {
    script: Box<Script>,
    records: Mutex<Vec<Recorded>>,
}

async fn handle(State(inner): State<Arc<Inner>>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 128 * 1024 * 1024).await.unwrap_or_default().to_vec();
    let rec = Recorded {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(str::to_string),
        headers: parts.headers.iter().map(|(k, v)| (k.as_str().to_ascii_lowercase(), v.to_str().unwrap_or("").to_string())).collect(),
        body,
    };
    inner.records.lock().unwrap().push(rec.clone());
    let reply = (inner.script)(&rec);
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    let mut resp = Response::new(Body::from(reply.body));
    *resp.status_mut() = StatusCode::from_u16(reply.status).unwrap();
    for (k, v) in reply.headers {
        resp.headers_mut().insert(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(&v).unwrap());
    }
    resp
}

pub struct Fake {
    pub addr: SocketAddr,
    inner: Arc<Inner>,
    handle: JoinHandle<()>,
}

impl Fake {
    pub async fn start(script: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Fake {
        let inner = Arc::new(Inner { script: Box::new(script), records: Mutex::new(Vec::new()) });
        let app = Router::new().fallback(handle).with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Fake { addr, inner, handle }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.inner.records.lock().unwrap().clone()
    }

    /// 豁免本假上游地址的 HTTP 客户端（其余 SSRF 规则照常）。
    pub fn http(&self) -> MediaHttp {
        MediaHttp::for_test(vec![self.addr])
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

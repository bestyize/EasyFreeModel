//! 媒体适配器与任务流程的测试：用 axum 假上游验证请求构造（鉴权头、multipart 字段、JSON 体）、
//! 响应解析、轮询到成功/失败/取消、文件落盘与大小限制、SSRF 拦截、脱敏与重启恢复。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use super::adapter::*;
use super::jobs::{self, MediaContext};
use super::registry::*;
use super::testutil::{tempdir, Fake, Recorded, Reply, TempDir};
use super::{find_adapter, MediaKind};
use crate::models::{JobStatus, MediaJob};
use crate::security::vault::{CredentialVault, MemoryVault};
use crate::store;

const KEY: &str = "sk-test-KEY-0123456789abcdef";
const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const MP3: [u8; 8] = [b'I', b'D', b'3', 4, 0, 0, 0, 0];

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------------------------
// 适配器级辅助
// ---------------------------------------------------------------------------------------------

fn cfg_for(provider: &str, cap: Capability, base: &str) -> CapabilityConfig {
    MediaRegistry::builtin()
        .clone()
        .with_base_url(provider, cap, base)
        .capability(provider, cap)
        .cloned()
        .unwrap_or_else(|| panic!("registry 缺少 {provider}/{cap:?}"))
}

fn adapter(cap: Capability, format: &str) -> &'static dyn MediaAdapter {
    find_adapter(cap, format).unwrap_or_else(|| panic!("缺少适配器 {format}"))
}

fn req(model: &str, params: Value) -> MediaRequest {
    MediaRequest {
        job_id: "job-1".into(),
        provider_id: "p".into(),
        model: model.into(),
        params: params.as_object().cloned().unwrap_or_default(),
        files: Default::default(),
    }
}

fn with_file(mut r: MediaRequest, key: &str, dir: &std::path::Path, bytes: &[u8], ext: &str) -> MediaRequest {
    let path = dir.join(format!("input-{key}.{ext}"));
    std::fs::write(&path, bytes).unwrap();
    r.files.insert(key.into(), UploadedFile { path, mime: super::files::mime_for_ext(ext).into(), ext: ext.into(), size: bytes.len() as u64 });
    r
}

async fn call(fake: &Fake, cfg: &CapabilityConfig, a: &dyn MediaAdapter, r: &MediaRequest) -> Result<Submission, MediaError> {
    let http = fake.http();
    let secret = Secret::new(KEY);
    let cx = AdapterCtx { http: &http, cfg, secret: &secret, provider_id: "p" };
    a.submit(&cx, r).await
}

async fn poll(fake: &Fake, cfg: &CapabilityConfig, a: &dyn MediaAdapter, t: &RemoteTask) -> Result<PollState, MediaError> {
    let http = fake.http();
    let secret = Secret::new(KEY);
    let cx = AdapterCtx { http: &http, cfg, secret: &secret, provider_id: "p" };
    a.poll(&cx, t).await
}

async fn cancel_remote(fake: &Fake, cfg: &CapabilityConfig, a: &dyn MediaAdapter, t: &RemoteTask) -> Result<(), MediaError> {
    let http = fake.http();
    let secret = Secret::new(KEY);
    let cx = AdapterCtx { http: &http, cfg, secret: &secret, provider_id: "p" };
    a.cancel(&cx, t).await
}

fn done(s: Result<Submission, MediaError>) -> Outcome {
    match s.expect("提交失败") {
        Submission::Done(o) => o,
        Submission::Pending(t) => panic!("预期同步完成，得到 Pending({t:?})"),
    }
}

fn pending(s: Result<Submission, MediaError>) -> RemoteTask {
    match s.expect("提交失败") {
        Submission::Pending(t) => t,
        Submission::Done(o) => panic!("预期 Pending，得到 Done({o:?})"),
    }
}

fn poll_done(s: Result<PollState, MediaError>) -> Outcome {
    match s.expect("轮询失败") {
        PollState::Done(o) => o,
        PollState::Pending { .. } => panic!("预期完成"),
    }
}

fn bytes_of(item: &OutputItem) -> (&[u8], &str) {
    match item {
        OutputItem::Bytes { data, ext } => (data, ext),
        other => panic!("预期 Bytes：{other:?}"),
    }
}

fn remote_of(item: &OutputItem) -> (&str, &str, bool) {
    match item {
        OutputItem::Remote { url, ext, authed } => (url, ext, *authed),
        other => panic!("预期 Remote：{other:?}"),
    }
}

fn assert_common_headers(r: &Recorded) {
    assert_eq!(r.header("user-agent").unwrap(), format!("RouterDesktop/{}", env!("CARGO_PKG_VERSION")));
}

// ---------------------------------------------------------------------------------------------
// 注册表数据
// ---------------------------------------------------------------------------------------------

#[test]
fn registry_json_loads_and_every_ready_capability_has_an_adapter() {
    let reg = MediaRegistry::from_json(MEDIA_PROVIDERS_JSON).expect("media_providers.json 必须能解析");
    assert!(reg.providers().len() > 30);
    let known_params = ["size", "n", "seed", "speed", "format", "language", "dimensions", "duration", "aspectRatio", "resolution", "count", "region", "timeRange"];
    let known_features = ["image-edit", "video-image2video", "asr-timestamps", "asr-diarization", "tts-pitch"];
    for p in reg.providers() {
        for (cap, c) in &p.capabilities {
            if c.is_ready() {
                assert!(find_adapter(*cap, &c.format).is_some(), "{} / {:?}: 格式 {} 没有适配器", p.id, cap, c.format);
                assert!(c.base_url.starts_with("https://"), "{}: baseUrl 必须是 https", p.id);
                for prm in &c.params {
                    assert!(known_params.contains(&prm.as_str()), "{}: 未知参数 {prm}", p.id);
                }
                for f in &c.features {
                    assert!(known_features.contains(&f.as_str()), "{}: 未知特性 {f}", p.id);
                }
                for (k, v) in &c.extra_headers {
                    assert!(!k.to_ascii_lowercase().contains("key") && !k.to_ascii_lowercase().contains("auth"), "{}: 固定头不得是凭据 {k} {v}", p.id);
                }
                if let AuthSpec::Header { name, .. } | AuthSpec::Query { name } = &c.auth {
                    assert!(!name.is_empty());
                }
            } else {
                assert!(!c.reason.as_deref().unwrap_or("").is_empty(), "{} / {:?}: deferred 必须写明原因", p.id, cap);
            }
        }
    }
}

#[test]
fn every_adapter_format_is_used_by_some_provider() {
    let reg = MediaRegistry::builtin();
    let mut seen = std::collections::HashSet::new();
    for a in super::all_adapters() {
        let used = reg.providers().iter().any(|p| p.capabilities.get(&a.capability()).is_some_and(|c| c.format == a.format() && c.is_ready()));
        assert!(used, "适配器 {} 没有被任何供应商使用", a.format());
        assert!(seen.insert((a.capability(), a.format())), "重复的适配器 {}", a.format());
    }
}

#[test]
fn registry_covers_the_provider_catalog_and_web_cookie_providers_are_deferred() {
    let catalog: Vec<Value> = serde_json::from_str(include_str!("../../resources/provider_catalog.json")).unwrap();
    let reg = MediaRegistry::builtin();
    let catalog_ids: std::collections::HashSet<&str> = catalog.iter().filter_map(|p| p["id"].as_str()).collect();
    for p in reg.providers() {
        assert!(catalog_ids.contains(p.id.as_str()), "{} 不在 provider_catalog.json 里", p.id);
    }
    for p in &catalog {
        let id = p["id"].as_str().unwrap();
        for kind in p["serviceKinds"].as_array().unwrap().iter().filter_map(Value::as_str) {
            let cap = match kind {
                "image" => Capability::Image,
                "stt" => Capability::Stt,
                "tts" => Capability::Tts,
                "video" => Capability::Video,
                "webSearch" => Capability::Search,
                "webFetch" => Capability::Fetch,
                "embedding" => Capability::Embedding,
                _ => continue,
            };
            assert!(reg.capability(id, cap).is_some(), "目录里的 {id} / {kind} 没有出现在 media_providers.json（应为 ready 或 deferred）");
        }
    }
    for id in ["grok-web", "perplexity-web"] {
        let e = reg.get(id).unwrap();
        assert_eq!(e.status, Some(Status::Deferred));
        assert!(e.reason.as_deref().unwrap().contains("私有接口"));
        assert!(!e.is_ready());
        assert!(reg.ready_capability(id, Capability::Search).is_none());
    }
}

#[test]
fn registry_rejects_incomplete_entries() {
    assert!(MediaRegistry::from_json(r#"{"version":1,"providers":[{"id":"a","capabilities":{"image":{"format":"x"}}}]}"#).is_err(), "ready 缺 baseUrl");
    assert!(MediaRegistry::from_json(r#"{"version":1,"providers":[{"id":"a","capabilities":{"image":{"status":"deferred"}}}]}"#).is_err(), "deferred 缺原因");
    assert!(MediaRegistry::from_json(r#"{"version":1,"providers":[{"id":"a"},{"id":"a"}]}"#).is_err(), "id 重复");
    assert!(MediaRegistry::from_json(r#"{"version":1,"providers":[{"id":"a","status":"deferred"}]}"#).is_err());
    assert!(MediaRegistry::from_json("not json").is_err());
}

#[test]
fn provider_infos_expose_capability_tables_without_urls_or_secrets() {
    let reg = MediaRegistry::builtin();
    let tts = reg.infos(Some(Capability::Tts));
    let eleven = tts.iter().find(|p| p.id == "elevenlabs").unwrap();
    assert_eq!(eleven.status, Status::Ready);
    assert_eq!(eleven.capabilities.len(), 1);
    assert_eq!(eleven.capabilities[0].format, "elevenlabs-tts");
    let edge = tts.iter().find(|p| p.id == "edge-tts").unwrap();
    assert_eq!(edge.status, Status::Deferred);
    assert!(edge.reason.as_deref().unwrap().contains("私有接口"));
    let text = serde_json::to_string(&tts).unwrap();
    assert!(!text.contains("https://"), "视图不暴露 baseUrl");
    assert!(reg.infos(None).iter().any(|p| p.id == "grok-web"));
    let ready = reg.all_ready_provider_ids();
    assert!(ready.contains(&"openai") && ready.contains(&"tavily") && !ready.contains(&"grok-web"));
}

// ---------------------------------------------------------------------------------------------
// 图像
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn openai_images_builds_request_and_parses_b64_and_url_results() {
    let png = b64(&PNG);
    let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"b64_json": png}, {"url": "https://cdn.example.com/a.png"}]}))).await;
    let cfg = cfg_for("openai", Capability::Image, &format!("{}/v1/images/generations", fake.url()));
    let out = done(call(&fake, &cfg, adapter(Capability::Image, "openai-images"), &req("gpt-image-1", json!({"prompt": "a fox", "size": "1024x1536", "n": 2, "seed": 42, "mode": "generate"}))).await);

    let rec = &fake.requests()[0];
    assert_eq!((rec.method.as_str(), rec.path.as_str()), ("POST", "/v1/images/generations"));
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_common_headers(rec);
    assert!(rec.header("content-type").unwrap().starts_with("application/json"));
    assert_eq!(rec.json(), json!({"model": "gpt-image-1", "prompt": "a fox", "n": 2, "size": "1024x1536"}), "不在能力表里的 seed 不得发给上游");

    assert_eq!(out.outputs.len(), 2);
    let (data, ext) = bytes_of(&out.outputs[0]);
    assert_eq!((data, ext), (&PNG[..], "png"));
    assert_eq!(remote_of(&out.outputs[1]), ("https://cdn.example.com/a.png", "png", false));
}

#[tokio::test]
async fn openai_images_validates_params_and_surfaces_upstream_errors() {
    let fake = Fake::start(|r| match r.json()["prompt"].as_str() {
        Some("bad-key") => Reply::json(401, json!({"error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}})),
        Some("empty") => Reply::ok(json!({"data": []})),
        Some("garbage") => Reply::ok(json!({"data": [{"b64_json": "###"}]})),
        _ => Reply::ok(json!({"unexpected": true})),
    })
    .await;
    let cfg = cfg_for("openai", Capability::Image, &format!("{}/v1/images/generations", fake.url()));
    let a = adapter(Capability::Image, "openai-images");
    let err = call(&fake, &cfg, a, &req("m", json!({"prompt": "bad-key"}))).await.err().unwrap();
    assert_eq!(err, MediaError::Upstream { status: 401, message: "Incorrect API key provided".into() });
    assert!(err.is_failover());
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"prompt": "empty"}))).await, Err(MediaError::BadResponse(_))));
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"prompt": "garbage"}))).await, Err(MediaError::BadResponse(_))));
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"prompt": "x"}))).await, Err(MediaError::BadResponse(_))));
    let before = fake.requests().len();
    for bad in [json!({"prompt": "x", "n": 0}), json!({"prompt": "x", "n": 11}), json!({"prompt": "x", "size": "huge"}), json!({"prompt": "x", "size": "99999x1"}), json!({"prompt": ""}), json!({})] {
        assert!(matches!(call(&fake, &cfg, a, &req("m", bad.clone())).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
    assert!(matches!(call(&fake, &cfg, a, &req("", json!({"prompt": "x"}))).await, Err(MediaError::InvalidParams(_))));
    assert_eq!(fake.requests().len(), before, "参数无效时不应发出请求");
}

#[tokio::test]
async fn openai_images_edit_uses_multipart_with_generated_file_name() {
    let tmp = tempdir();
    let png = b64(&PNG);
    let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"b64_json": png}]}))).await;
    let cfg = cfg_for("openai", Capability::Image, &format!("{}/v1/images/generations", fake.url()));
    let r = with_file(req("gpt-image-1", json!({"prompt": "add a hat", "mode": "edit", "size": "1024x1024", "n": 1})), "image", &tmp, &PNG, "png");
    done(call(&fake, &cfg, adapter(Capability::Image, "openai-images"), &r).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v1/images/generations/__endpoint");
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    let parts = rec.multipart();
    let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["model", "prompt", "image", "n", "size"]);
    assert_eq!(rec.field("prompt").unwrap().text(), "add a hat");
    let image = rec.field("image").unwrap();
    assert_eq!(image.filename.as_deref(), Some("image.png"), "不泄露本地文件名");
    assert_eq!(image.content_type.as_deref(), Some("image/png"));
    assert_eq!(image.data, PNG);

    // 没有图片 / 供应商不支持编辑
    let no_file = req("m", json!({"prompt": "x", "mode": "edit"}));
    assert!(matches!(call(&fake, &cfg, adapter(Capability::Image, "openai-images"), &no_file).await, Err(MediaError::InvalidParams(_))));
    let xai = cfg_for("xai", Capability::Image, &fake.url());
    assert!(matches!(call(&fake, &xai, adapter(Capability::Image, "openai-images"), &no_file).await, Err(MediaError::Unsupported(_))));
}

#[tokio::test]
async fn stability_v2_sends_multipart_and_maps_size_to_aspect_ratio() {
    let png = b64(&PNG);
    let fake = Fake::start(move |r| {
        if r.path.ends_with("/sd3") && r.field("prompt").unwrap().text() == "filtered" {
            Reply::ok(json!({"image": "AAAA", "finish_reason": "CONTENT_FILTERED"}))
        } else {
            Reply::ok(json!({"image": png, "finish_reason": "SUCCESS", "seed": 7}))
        }
    })
    .await;
    let cfg = cfg_for("stability-ai", Capability::Image, &format!("{}/v2beta/stable-image/generate", fake.url()));
    let a = adapter(Capability::Image, "stability-v2");
    let out = done(call(&fake, &cfg, a, &req("stable-image-ultra", json!({"prompt": "a cat", "size": "1536x1024", "seed": 9, "n": 4}))).await);
    assert_eq!(bytes_of(&out.outputs[0]).0, PNG);
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v2beta/stable-image/generate/ultra");
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.header("accept").unwrap(), "application/json");
    assert_common_headers(rec);
    let names: Vec<_> = rec.multipart().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["prompt", "output_format", "aspect_ratio", "seed"], "n 不在能力表里，不发送");
    assert_eq!(rec.field("aspect_ratio").unwrap().text(), "3:2");
    assert_eq!(rec.field("seed").unwrap().text(), "9");

    done(call(&fake, &cfg, a, &req("sd3.5-large", json!({"prompt": "x"}))).await);
    let rec = &fake.requests()[1];
    assert_eq!(rec.path, "/v2beta/stable-image/generate/sd3");
    assert_eq!(rec.field("model").unwrap().text(), "sd3.5-large");
    done(call(&fake, &cfg, a, &req("stable-image-core", json!({"prompt": "x"}))).await);
    assert!(fake.requests()[2].path.ends_with("/core"));

    let err = call(&fake, &cfg, a, &req("sd3.5-large", json!({"prompt": "filtered"}))).await.err().unwrap();
    assert!(matches!(err, MediaError::Failed(ref m) if m.contains("CONTENT_FILTERED")), "{err:?}");
}

#[tokio::test]
async fn bfl_flux_submits_then_polls_until_ready_or_moderated() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (p2, base) = (polls.clone(), Arc::new(Mutex::new(String::new())));
    let base2 = base.clone();
    let fake = Fake::start(move |r| {
        if r.method == "POST" {
            let b = base2.lock().unwrap().clone();
            return Reply::ok(json!({"id": "task-1", "polling_url": format!("{b}/v1/get_result?id=task-1")}));
        }
        match p2.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::ok(json!({"status": "Pending", "progress": 0.4})),
            1 => Reply::ok(json!({"status": "Ready", "result": {"sample": "https://delivery.bfl.ai/x/sample.png?sig=1"}})),
            _ => Reply::ok(json!({"status": "Content Moderated"})),
        }
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let cfg = cfg_for("black-forest-labs", Capability::Image, &format!("{}/v1", fake.url()));
    let a = adapter(Capability::Image, "bfl-flux");
    let task = pending(call(&fake, &cfg, a, &req("flux-pro-1.1", json!({"prompt": "mountain", "size": "1024x768", "seed": 5}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v1/flux-pro-1.1");
    assert_eq!(rec.header("x-key").unwrap(), KEY);
    assert!(rec.header("authorization").is_none());
    assert_eq!(rec.json(), json!({"prompt": "mountain", "output_format": "png", "width": 1024, "height": 768, "seed": 5}));
    assert_eq!(task.id, "task-1");

    assert!(matches!(poll(&fake, &cfg, a, &task).await.unwrap(), PollState::Pending { progress: Some(40) }));
    let out = poll_done(poll(&fake, &cfg, a, &task).await);
    assert_eq!(remote_of(&out.outputs[0]), ("https://delivery.bfl.ai/x/sample.png?sig=1", "png", false));
    let poll_rec = &fake.requests()[1];
    assert_eq!((poll_rec.method.as_str(), poll_rec.path.as_str(), poll_rec.query.as_deref()), ("GET", "/v1/get_result", Some("id=task-1")));
    assert_eq!(poll_rec.header("x-key").unwrap(), KEY);
    let err = poll(&fake, &cfg, a, &task).await.err().unwrap();
    assert!(matches!(err, MediaError::Failed(ref m) if m.contains("审核")), "{err:?}");
}

#[tokio::test]
async fn polling_urls_outside_the_trusted_hosts_are_never_called_with_credentials() {
    let fake = Fake::start(|_| Reply::ok(json!({"id": "t", "polling_url": "https://evil.example.com/steal"}))).await;
    let cfg = cfg_for("black-forest-labs", Capability::Image, &format!("{}/v1", fake.url()));
    let a = adapter(Capability::Image, "bfl-flux");
    let err = call(&fake, &cfg, a, &req("flux-dev", json!({"prompt": "x"}))).await.err().unwrap();
    assert!(matches!(err, MediaError::Blocked(_)), "{err:?}");
    // 持久化的任务句柄被篡改成内网地址时，轮询同样拒绝
    let mut task = RemoteTask::new("bfl-flux", "t");
    task.poll_url = Some("http://169.254.169.254/latest/meta-data".into());
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Blocked(_))));
    assert_eq!(fake.requests().len(), 1);
    // 模型 ID 不允许路径穿越
    assert!(matches!(call(&fake, &cfg, a, &req("../admin", json!({"prompt": "x"}))).await, Err(MediaError::InvalidParams(_))));
}

#[tokio::test]
async fn fal_queue_flow_with_status_response_and_cancel_urls() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (p2, base) = (polls.clone(), Arc::new(Mutex::new(String::new())));
    let base2 = base.clone();
    let fake = Fake::start(move |r| {
        let b = base2.lock().unwrap().clone();
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/fal-ai/flux/schnell") => Reply::ok(json!({
                "request_id": "req-1",
                "status_url": format!("{b}/fal-ai/flux/requests/req-1/status"),
                "response_url": format!("{b}/fal-ai/flux/requests/req-1"),
                "cancel_url": format!("{b}/fal-ai/flux/requests/req-1/cancel"),
            })),
            ("GET", p) if p.ends_with("/status") => match p2.fetch_add(1, Ordering::SeqCst) {
                0 => Reply::ok(json!({"status": "IN_QUEUE", "queue_position": 1})),
                1 => Reply::ok(json!({"status": "COMPLETED"})),
                _ => Reply::ok(json!({"status": "COMPLETED", "error": "boom"})),
            },
            ("GET", p) if p.ends_with("/req-1") => Reply::ok(json!({"images": [{"url": "https://v3.fal.media/a.webp", "content_type": "image/webp"}]})),
            ("PUT", p) if p.ends_with("/cancel") => Reply::ok(json!({"status": "CANCELLATION_REQUESTED"})),
            _ => Reply::text(404, "no"),
        }
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let cfg = cfg_for("fal-ai", Capability::Image, &fake.url());
    let a = adapter(Capability::Image, "fal-queue");
    let task = pending(call(&fake, &cfg, a, &req("fal-ai/flux/schnell", json!({"prompt": "robot", "size": "512x768", "n": 2, "seed": 3}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Key {KEY}"));
    assert_eq!(rec.json(), json!({"prompt": "robot", "num_images": 2, "image_size": {"width": 512, "height": 768}, "seed": 3}));

    assert!(matches!(poll(&fake, &cfg, a, &task).await.unwrap(), PollState::Pending { .. }));
    let out = poll_done(poll(&fake, &cfg, a, &task).await);
    assert_eq!(remote_of(&out.outputs[0]), ("https://v3.fal.media/a.webp", "webp", false));
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Failed(ref m)) if m.contains("boom")));

    cancel_remote(&fake, &cfg, a, &task).await.unwrap();
    let last = fake.requests().pop().unwrap();
    assert_eq!((last.method.as_str(), last.path.as_str()), ("PUT", "/fal-ai/flux/requests/req-1/cancel"));
    assert_eq!(last.header("authorization").unwrap(), format!("Key {KEY}"));

    // 路径穿越的模型 ID
    for bad in ["../x", "a//b", "/abs", "a/../b", "a b"] {
        assert!(matches!(call(&fake, &cfg, a, &req(bad, json!({"prompt": "x"}))).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
}

#[tokio::test]
async fn runway_image_task_lifecycle_and_cancel() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p2 = polls.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/text_to_image") => Reply::ok(json!({"id": "task-9"})),
        ("GET", "/v1/tasks/task-9") => match p2.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::ok(json!({"status": "RUNNING", "progress": 0.5})),
            1 => Reply::ok(json!({"status": "SUCCEEDED", "output": ["https://dnznrvs05pmza.cloudfront.net/a.png"]})),
            2 => Reply::ok(json!({"status": "FAILED", "failure": "content policy", "failureCode": "SAFETY"})),
            _ => Reply::ok(json!({"status": "CANCELLED"})),
        },
        ("DELETE", "/v1/tasks/task-9") => Reply::bytes(204, "text/plain", vec![]),
        _ => Reply::text(404, "no"),
    })
    .await;
    let cfg = cfg_for("runwayml", Capability::Image, &format!("{}/v1", fake.url()));
    let a = adapter(Capability::Image, "runway-image");
    let task = pending(call(&fake, &cfg, a, &req("gen4_image", json!({"prompt": "city", "size": "1792x1024", "seed": 1}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.header("x-runway-version").unwrap(), "2024-11-06");
    assert_eq!(rec.json(), json!({"model": "gen4_image", "ratio": "1360:768", "promptText": "city", "seed": 1}));

    assert!(matches!(poll(&fake, &cfg, a, &task).await.unwrap(), PollState::Pending { progress: Some(50) }));
    assert_eq!(remote_of(&poll_done(poll(&fake, &cfg, a, &task).await).outputs[0]).0, "https://dnznrvs05pmza.cloudfront.net/a.png");
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Failed(ref m)) if m.contains("content policy") && m.contains("SAFETY")));
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Failed(ref m)) if m.contains("取消")));
    cancel_remote(&fake, &cfg, a, &task).await.unwrap();
    assert_eq!(fake.requests().last().unwrap().method, "DELETE");

    let mut tampered = task.clone();
    tampered.id = "../../x".into();
    assert!(matches!(poll(&fake, &cfg, a, &tampered).await, Err(MediaError::InvalidParams(_))));
}

// ---------------------------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn openai_speech_returns_audio_and_validates_inputs() {
    let fake = Fake::start(|r| {
        if r.json()["input"] == "boom" {
            Reply::ok(json!({"error": "oops"}))
        } else {
            Reply::bytes(200, "audio/mpeg", MP3.to_vec())
        }
    })
    .await;
    let cfg = cfg_for("openai", Capability::Tts, &format!("{}/v1/audio/speech", fake.url()));
    let a = adapter(Capability::Tts, "openai-speech");
    let out = done(call(&fake, &cfg, a, &req("gpt-4o-mini-tts", json!({"text": "你好", "voice": "alloy", "speed": 1.25, "format": "mp3", "pitch": 3}))).await);
    let (data, ext) = bytes_of(&out.outputs[0]);
    assert_eq!((data, ext), (&MP3[..], "mp3"));
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.json(), json!({"model": "gpt-4o-mini-tts", "input": "你好", "voice": "alloy", "response_format": "mp3", "speed": 1.25}), "pitch 不被支持，不发送");

    // JSON 当成音频保存是错误的
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"text": "boom", "voice": "v"}))).await, Err(MediaError::BadResponse(_))));
    for bad in [json!({"voice": "v"}), json!({"text": "x"}), json!({"text": "x", "voice": "v", "speed": 9}), json!({"text": "x", "voice": "v", "format": "exe"}), json!({"text": "字".repeat(4097), "voice": "v"})] {
        assert!(matches!(call(&fake, &cfg, a, &req("m", bad.clone())).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
}

#[tokio::test]
async fn elevenlabs_tts_uses_header_auth_and_safe_voice_path() {
    let fake = Fake::start(|_| Reply::bytes(200, "audio/mpeg", MP3.to_vec())).await;
    let cfg = cfg_for("elevenlabs", Capability::Tts, &format!("{}/v1/text-to-speech", fake.url()));
    let a = adapter(Capability::Tts, "elevenlabs-tts");
    done(call(&fake, &cfg, a, &req("eleven_multilingual_v2", json!({"text": "hello", "voice": "21m00Tcm4TlvDq8ikWAM", "speed": 1.1}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v1/text-to-speech/21m00Tcm4TlvDq8ikWAM");
    assert_eq!(rec.query.as_deref(), Some("output_format=mp3_44100_128"));
    assert_eq!(rec.header("xi-api-key").unwrap(), KEY);
    assert!(rec.header("authorization").is_none());
    assert_eq!(rec.json(), json!({"text": "hello", "model_id": "eleven_multilingual_v2", "voice_settings": {"speed": 1.1}}));
    done(call(&fake, &cfg, a, &req("eleven_multilingual_v2", json!({"text": "hello", "voice": "v1", "speed": 1}))).await);
    assert!(fake.requests()[1].json().get("voice_settings").is_none(), "默认语速不发送 voice_settings");

    for bad in ["../v1/admin", "a/b", "a?b=1", "a b", "", ".."] {
        let r = call(&fake, &cfg, a, &req("m", json!({"text": "x", "voice": bad}))).await;
        assert!(matches!(r, Err(MediaError::InvalidParams(_))), "{bad}");
    }
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"text": "x", "voice": "v", "speed": 2.0}))).await, Err(MediaError::InvalidParams(_))), "超出 ElevenLabs 范围的语速不静默截断");
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn cartesia_tts_sends_version_header_and_voice_reference() {
    let fake = Fake::start(|_| Reply::bytes(200, "audio/mpeg", MP3.to_vec())).await;
    let cfg = cfg_for("cartesia", Capability::Tts, &format!("{}/tts/bytes", fake.url()));
    done(call(&fake, &cfg, adapter(Capability::Tts, "cartesia-tts"), &req("sonic-2", json!({"text": "hi", "voice": "a0e99841-438c-4a64-b679-ae501e7d6091"}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("x-api-key").unwrap(), KEY);
    assert_eq!(rec.header("cartesia-version").unwrap(), "2024-06-10");
    let body = rec.json();
    assert_eq!(body["model_id"], "sonic-2");
    assert_eq!(body["transcript"], "hi");
    assert_eq!(body["voice"], json!({"mode": "id", "id": "a0e99841-438c-4a64-b679-ae501e7d6091"}));
    assert_eq!(body["output_format"]["container"], "mp3");
}

// ---------------------------------------------------------------------------------------------
// STT
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn openai_transcriptions_multipart_and_timestamps() {
    let tmp = tempdir();
    let fake = Fake::start(|r| {
        if r.field("response_format").unwrap().text() == "verbose_json" {
            Reply::ok(json!({"text": "hello world", "segments": [{"start": 0.0, "end": 1.2, "text": " hello"}, {"start": 61.5, "end": 63.0, "text": " world"}]}))
        } else {
            Reply::ok(json!({"text": "  hello world "}))
        }
    })
    .await;
    let cfg = cfg_for("openai", Capability::Stt, &format!("{}/v1/audio/transcriptions", fake.url()));
    let a = adapter(Capability::Stt, "openai-transcriptions");
    let r = with_file(req("whisper-1", json!({"language": "en", "timestamps": false})), "audio", &tmp, &MP3, "mp3");
    assert_eq!(done(call(&fake, &cfg, a, &r).await).text.unwrap(), "hello world");
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_common_headers(rec);
    let names: Vec<_> = rec.multipart().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["file", "model", "language", "response_format"]);
    let file = rec.field("file").unwrap();
    assert_eq!((file.filename.as_deref(), file.content_type.as_deref(), file.data.as_slice()), (Some("audio.mp3"), Some("audio/mpeg"), &MP3[..]));
    assert_eq!(rec.field("model").unwrap().text(), "whisper-1");
    assert_eq!(rec.field("language").unwrap().text(), "en");
    assert_eq!(rec.field("response_format").unwrap().text(), "json");

    let r = with_file(req("whisper-1", json!({"timestamps": true})), "audio", &tmp, &MP3, "mp3");
    assert_eq!(done(call(&fake, &cfg, a, &r).await).text.unwrap(), "[00:00.0] hello\n[01:01.5] world");
    assert_eq!(fake.requests()[1].field("timestamp_granularities[]").unwrap().text(), "segment");

    // 没有文件 / 语言字段不合法（不会原样拼进请求）
    assert!(matches!(call(&fake, &cfg, a, &req("whisper-1", json!({}))).await, Err(MediaError::InvalidParams(_))));
    let r = with_file(req("whisper-1", json!({"language": "en\r\nX-Evil: 1"})), "audio", &tmp, &MP3, "mp3");
    done(call(&fake, &cfg, a, &r).await);
    assert!(fake.requests()[2].field("language").is_none());
}

#[tokio::test]
async fn deepgram_listen_posts_raw_audio_with_token_auth() {
    let tmp = tempdir();
    let fake = Fake::start(|_| {
        Reply::ok(json!({"results": {
            "channels": [{"alternatives": [{"transcript": "hi there bye"}]}],
            "utterances": [
                {"start": 0.4, "end": 1.0, "speaker": 0, "transcript": "hi there"},
                {"start": 2.0, "end": 2.5, "speaker": 1, "transcript": "bye"}
            ]
        }}))
    })
    .await;
    let cfg = cfg_for("deepgram", Capability::Stt, &format!("{}/v1/listen", fake.url()));
    let a = adapter(Capability::Stt, "deepgram-listen");
    let r = with_file(req("nova-3", json!({"timestamps": true, "diarization": true})), "audio", &tmp, &MP3, "mp3");
    let text = done(call(&fake, &cfg, a, &r).await).text.unwrap();
    assert_eq!(text, "[00:00.4] 说话人 0：hi there\n[00:02.0] 说话人 1：bye");
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Token {KEY}"));
    assert_eq!(rec.header("content-type").unwrap(), "audio/mpeg");
    assert_eq!(rec.body, MP3, "请求体就是原始音频字节");
    assert_eq!(rec.path, "/v1/listen");
    for (k, v) in [("model", "nova-3"), ("smart_format", "true"), ("punctuate", "true"), ("detect_language", "true"), ("diarize", "true"), ("utterances", "true")] {
        assert_eq!(rec.query(k).as_deref(), Some(v), "{k}");
    }
    assert!(rec.query("language").is_none());

    // 指定语言、不要时间戳：纯文本，且不请求 utterances
    let r = with_file(req("nova-3", json!({"language": "zh"})), "audio", &tmp, &MP3, "mp3");
    assert_eq!(done(call(&fake, &cfg, a, &r).await).text.unwrap(), "hi there bye");
    let rec = &fake.requests()[1];
    assert_eq!(rec.query("language").as_deref(), Some("zh"));
    assert!(rec.query("detect_language").is_none() && rec.query("utterances").is_none() && rec.query("diarize").is_none());
}

#[tokio::test]
async fn elevenlabs_stt_multipart_with_diarization() {
    let tmp = tempdir();
    let fake = Fake::start(|_| {
        Reply::ok(json!({"text": "Hello. Hi!", "language_code": "eng", "words": [
            {"text": "Hello.", "type": "word", "start": 0.0, "end": 0.5, "speaker_id": "speaker_0"},
            {"text": "Hi!", "type": "word", "start": 1.0, "end": 1.4, "speaker_id": "speaker_1"}
        ]}))
    })
    .await;
    let cfg = cfg_for("elevenlabs", Capability::Stt, &format!("{}/v1/speech-to-text", fake.url()));
    let a = adapter(Capability::Stt, "elevenlabs-stt");
    let r = with_file(req("scribe_v1", json!({"diarization": true, "timestamps": true, "language": "en"})), "audio", &tmp, &MP3, "mp3");
    let text = done(call(&fake, &cfg, a, &r).await).text.unwrap();
    assert_eq!(text, "[00:00.0] 说话人 0：Hello.\n[00:01.0] 说话人 1：Hi!");
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("xi-api-key").unwrap(), KEY);
    let names: Vec<_> = rec.multipart().iter().map(|p| p.name.clone()).collect();
    assert_eq!(names, ["file", "model_id", "language_code", "diarize", "timestamps_granularity"]);
    assert_eq!(rec.field("model_id").unwrap().text(), "scribe_v1");

    let r = with_file(req("scribe_v1", json!({})), "audio", &tmp, &MP3, "mp3");
    assert_eq!(done(call(&fake, &cfg, a, &r).await).text.unwrap(), "Hello. Hi!");
}

#[tokio::test]
async fn empty_transcript_is_reported_honestly() {
    let tmp = tempdir();
    let fake = Fake::start(|_| Reply::ok(json!({"text": "  "}))).await;
    let cfg = cfg_for("openai", Capability::Stt, &fake.url());
    let r = with_file(req("whisper-1", json!({})), "audio", &tmp, &MP3, "mp3");
    assert_eq!(done(call(&fake, &cfg, adapter(Capability::Stt, "openai-transcriptions"), &r).await).text.unwrap(), super::stt::NO_SPEECH);
}

// ---------------------------------------------------------------------------------------------
// 视频
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn xai_video_text_and_image_modes_and_polling() {
    let tmp = tempdir();
    let polls = Arc::new(AtomicUsize::new(0));
    let p2 = polls.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/videos/generations") => Reply::ok(json!({"request_id": "vid-1"})),
        ("GET", "/v1/videos/vid-1") => match p2.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::ok(json!({"status": "pending"})),
            1 => Reply::ok(json!({"status": "done", "video": {"url": "https://vidgen.x.ai/v/1.mp4"}})),
            2 => Reply::ok(json!({"status": "failed", "error": {"message": "moderation"}})),
            _ => Reply::ok(json!({"status": "expired"})),
        },
        _ => Reply::text(404, "no"),
    })
    .await;
    let cfg = cfg_for("xai", Capability::Video, &format!("{}/v1/videos", fake.url()));
    let a = adapter(Capability::Video, "xai-video");
    let task = pending(call(&fake, &cfg, a, &req("grok-imagine-video", json!({"prompt": "waves", "mode": "text2video", "duration": 5, "aspectRatio": "16:9", "resolution": "720p", "fps": 24}))).await);
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.json(), json!({"model": "grok-imagine-video", "prompt": "waves", "duration": 5, "aspect_ratio": "16:9", "resolution": "720p"}), "fps 不被支持");
    assert!(matches!(poll(&fake, &cfg, a, &task).await.unwrap(), PollState::Pending { .. }));
    assert_eq!(remote_of(&poll_done(poll(&fake, &cfg, a, &task).await).outputs[0]), ("https://vidgen.x.ai/v/1.mp4", "mp4", false));
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Failed(ref m)) if m.contains("moderation")));
    assert!(matches!(poll(&fake, &cfg, a, &task).await, Err(MediaError::Failed(ref m)) if m.contains("过期")));

    let r = with_file(req("grok-imagine-video", json!({"mode": "image2video"})), "image", &tmp, &PNG, "png");
    pending(call(&fake, &cfg, a, &r).await);
    let body = fake.requests().last().unwrap().json();
    assert_eq!(body["image"]["url"], format!("data:image/png;base64,{}", b64(&PNG)));
    assert!(body.get("prompt").is_none());

    // 缺提示词 / 缺首帧 / 非法参数
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"mode": "text2video"}))).await, Err(MediaError::InvalidParams(_))));
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"mode": "image2video"}))).await, Err(MediaError::InvalidParams(_))));
    for bad in [json!({"prompt": "x", "duration": 0}), json!({"prompt": "x", "duration": 999}), json!({"prompt": "x", "aspectRatio": "wide"}), json!({"prompt": "x", "resolution": "ultra"})] {
        assert!(matches!(call(&fake, &cfg, a, &req("m", bad.clone())).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
}

#[tokio::test]
async fn openrouter_video_uses_polling_url_and_authed_download_of_unsigned_urls() {
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let fake = Fake::start(move |r| {
        let b = b2.lock().unwrap().clone();
        match (r.method.as_str(), r.path.as_str()) {
            ("POST", "/api/v1/videos") => Reply::ok(json!({"id": "or-1", "polling_url": format!("{b}/api/v1/videos/or-1"), "status": "pending"})),
            ("GET", "/api/v1/videos/or-1") => Reply::ok(json!({"id": "or-1", "status": "completed", "unsigned_urls": [format!("{b}/api/v1/videos/or-1/content?index=0")]})),
            _ => Reply::text(404, "no"),
        }
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let cfg = cfg_for("openrouter", Capability::Video, &format!("{}/api/v1/videos", fake.url()));
    let a = adapter(Capability::Video, "openrouter-video");
    let task = pending(call(&fake, &cfg, a, &req("google/veo-3.1", json!({"prompt": "paper boat", "mode": "text2video", "duration": 8, "aspectRatio": "9:16", "resolution": "1080p"}))).await);
    assert_eq!(fake.requests()[0].json(), json!({"model": "google/veo-3.1", "prompt": "paper boat", "duration": 8, "aspect_ratio": "9:16", "resolution": "1080p"}));
    assert_eq!(task.poll_url.as_deref(), Some(format!("{}/api/v1/videos/or-1", fake.url()).as_str()));
    let out = poll_done(poll(&fake, &cfg, a, &task).await);
    let (url, ext, authed) = remote_of(&out.outputs[0]);
    assert!(url.ends_with("/or-1/content?index=0") && ext == "mp4" && authed, "OpenRouter 的 unsigned_urls 需要凭据下载");
}

#[tokio::test]
async fn runway_video_requires_a_first_frame_and_validates_duration() {
    let tmp = tempdir();
    let fake = Fake::start(|_| Reply::ok(json!({"id": "rv-1"}))).await;
    let cfg = cfg_for("runwayml", Capability::Video, &format!("{}/v1", fake.url()));
    let a = adapter(Capability::Video, "runway-video");
    let r = with_file(req("gen4_turbo", json!({"mode": "image2video", "prompt": "pan left", "duration": 5, "aspectRatio": "9:16"})), "image", &tmp, &PNG, "png");
    let task = pending(call(&fake, &cfg, a, &r).await);
    assert_eq!(task.format, "runway-video");
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v1/image_to_video");
    assert_eq!(rec.header("x-runway-version").unwrap(), "2024-11-06");
    assert_eq!(
        rec.json(),
        json!({"model": "gen4_turbo", "ratio": "720:1280", "promptText": "pan left", "promptImage": format!("data:image/png;base64,{}", b64(&PNG)), "duration": 5})
    );
    // 文生视频不支持，不能悄悄改成别的
    assert!(matches!(call(&fake, &cfg, a, &req("gen4_turbo", json!({"mode": "text2video", "prompt": "x"}))).await, Err(MediaError::Unsupported(_))));
    let long = with_file(req("gen4_turbo", json!({"mode": "image2video", "duration": 15})), "image", &tmp, &PNG, "png");
    assert!(matches!(call(&fake, &cfg, a, &long).await, Err(MediaError::InvalidParams(_))));
    assert_eq!(fake.requests().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// 搜索 / 抓取
// ---------------------------------------------------------------------------------------------

fn search_params() -> Value {
    json!({"query": "rust lang", "count": 3, "region": "us", "timeRange": "week", "resultMode": "native"})
}

fn results_of(out: &Outcome) -> Value {
    serde_json::from_str(out.text.as_deref().expect("搜索结果文本")).unwrap()
}

fn assert_search(out: &Outcome, provider: &str, title: &str, url: &str, snippet: &str) {
    let v = results_of(out);
    assert_eq!(v["origin"], "native");
    assert_eq!(v["provider"], provider);
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "javascript:/file: 链接必须被丢弃：{results:?}");
    assert_eq!(results[0], json!({"title": title, "url": url, "snippet": snippet}));
}

async fn search_once(provider: &str, format: &str, base_suffix: &str, reply: Value) -> (Outcome, Recorded) {
    let fake = Fake::start(move |_| Reply::ok(reply.clone())).await;
    let cfg = cfg_for(provider, Capability::Search, &format!("{}{base_suffix}", fake.url()));
    let out = done(call(&fake, &cfg, adapter(Capability::Search, format), &req("", search_params())).await);
    let rec = fake.requests().remove(0);
    assert_common_headers(&rec);
    (out, rec)
}

#[tokio::test]
async fn search_serper_and_brave_request_mapping() {
    let (out, rec) = search_once(
        "serper",
        "serper-search",
        "",
        json!({"organic": [{"title": "Rust", "link": "https://www.rust-lang.org/", "snippet": "A language"}, {"title": "x", "link": "javascript:alert(1)", "snippet": "y"}]}),
    )
    .await;
    assert_search(&out, "p", "Rust", "https://www.rust-lang.org/", "A language");
    assert_eq!((rec.method.as_str(), rec.path.as_str()), ("POST", "/search"));
    assert_eq!(rec.header("x-api-key").unwrap(), KEY);
    assert_eq!(rec.json(), json!({"q": "rust lang", "num": 3, "gl": "us", "tbs": "qdr:w"}));

    let (out, rec) = search_once(
        "brave-search",
        "brave-search",
        "/res/v1",
        json!({"web": {"results": [{"title": "<b>Rust</b> lang", "url": "https://www.rust-lang.org/", "description": "A <strong>fast</strong> language"}, {"title": "bad", "url": "file:///etc/passwd", "description": ""}]}}),
    )
    .await;
    assert_search(&out, "p", "Rust lang", "https://www.rust-lang.org/", "A fast language");
    assert_eq!((rec.method.as_str(), rec.path.as_str()), ("GET", "/res/v1/web/search"));
    assert_eq!(rec.header("x-subscription-token").unwrap(), KEY);
    assert!(rec.header("authorization").is_none());
    for (k, v) in [("q", "rust lang"), ("count", "3"), ("country", "US"), ("freshness", "pw")] {
        assert_eq!(rec.query(k).as_deref(), Some(v), "{k}");
    }
}

#[tokio::test]
async fn search_tavily_exa_linkup_perplexity_request_mapping() {
    let (out, rec) = search_once(
        "tavily",
        "tavily-search",
        "/search",
        json!({"results": [{"title": "T", "url": "https://t.example/a", "content": "C"}, {"title": "bad", "url": "data:text/html,x", "content": ""}]}),
    )
    .await;
    assert_search(&out, "p", "T", "https://t.example/a", "C");
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.json(), json!({"query": "rust lang", "max_results": 3, "topic": "general", "time_range": "week"}), "Tavily 不支持 region，不发送");

    let (out, rec) = search_once(
        "exa",
        "exa-search",
        "/search",
        json!({"results": [{"title": "E", "url": "https://e.example/", "text": "body text"}, {"title": "bad", "url": "javascript:1", "text": ""}]}),
    )
    .await;
    assert_search(&out, "p", "E", "https://e.example/", "body text");
    assert_eq!(rec.header("x-api-key").unwrap(), KEY);
    assert_eq!(rec.json(), json!({"query": "rust lang", "numResults": 3, "type": "auto", "contents": {"text": {"maxCharacters": 500}}}));

    let (out, rec) = search_once(
        "linkup",
        "linkup-search",
        "/v1/search",
        json!({"results": [{"type": "text", "name": "L", "url": "https://l.example/", "content": "lc"}, {"type": "image", "url": "ftp://x/y"}]}),
    )
    .await;
    assert_search(&out, "p", "L", "https://l.example/", "lc");
    let body = rec.json();
    assert_eq!((body["q"].clone(), body["outputType"].clone(), body["maxResults"].clone()), (json!("rust lang"), json!("searchResults"), json!(3)));
    assert!(body["fromDate"].is_string() && body["toDate"].is_string());
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));

    let (out, rec) = search_once(
        "perplexity",
        "perplexity-search",
        "/search",
        json!({"results": [{"title": "P", "url": "https://p.example/", "snippet": "ps"}, {"title": "bad", "url": "javascript:2", "snippet": ""}]}),
    )
    .await;
    assert_search(&out, "p", "P", "https://p.example/", "ps");
    assert_eq!(rec.json(), json!({"query": "rust lang", "max_results": 3, "country": "US"}));
}

#[tokio::test]
async fn search_youcom_searchapi_ollama_request_mapping() {
    let (out, rec) = search_once(
        "youcom",
        "youcom-search",
        "/v1/search",
        json!({"results": {"web": [{"title": "Y", "url": "https://y.example/", "snippets": ["first", "second"]}, {"title": "bad", "url": "javascript:3", "description": ""}]}}),
    )
    .await;
    assert_search(&out, "p", "Y", "https://y.example/", "first");
    assert_eq!(rec.header("x-api-key").unwrap(), KEY);
    for (k, v) in [("query", "rust lang"), ("count", "3"), ("country", "US"), ("freshness", "week")] {
        assert_eq!(rec.query(k).as_deref(), Some(v), "{k}");
    }

    let (out, rec) = search_once(
        "searchapi",
        "searchapi-search",
        "/api/v1/search",
        json!({"organic_results": [{"title": "S", "link": "https://s.example/", "snippet": "ss"}, {"title": "bad", "link": "javascript:4"}]}),
    )
    .await;
    assert_search(&out, "p", "S", "https://s.example/", "ss");
    assert_eq!(rec.query("api_key").as_deref(), Some(KEY), "SearchAPI 的密钥走查询参数");
    assert!(rec.header("authorization").is_none());
    for (k, v) in [("engine", "google"), ("q", "rust lang"), ("num", "3"), ("gl", "us"), ("time_period", "last_week")] {
        assert_eq!(rec.query(k).as_deref(), Some(v), "{k}");
    }

    let (out, rec) = search_once(
        "ollama-search",
        "ollama-search",
        "/api/web_search",
        json!({"results": [{"title": "O", "url": "https://o.example/", "content": "oc"}, {"title": "bad", "url": "javascript:5", "content": ""}]}),
    )
    .await;
    assert_search(&out, "p", "O", "https://o.example/", "oc");
    assert_eq!(rec.json(), json!({"query": "rust lang", "max_results": 3}));
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
}

#[tokio::test]
async fn search_validates_query_and_options() {
    let fake = Fake::start(|_| Reply::ok(json!({"organic": []}))).await;
    let cfg = cfg_for("serper", Capability::Search, &fake.url());
    let a = adapter(Capability::Search, "serper-search");
    for bad in [json!({}), json!({"query": "x", "count": 0}), json!({"query": "x", "count": 21}), json!({"query": "x", "region": "USA"}), json!({"query": "x", "timeRange": "decade"}), json!({"query": "x".repeat(501)})] {
        assert!(matches!(call(&fake, &cfg, a, &req("", bad.clone())).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
    assert!(fake.requests().is_empty());
    // 空结果是合法的成功，不会被伪造
    let out = done(call(&fake, &cfg, a, &req("", json!({"query": "nothing"}))).await);
    assert_eq!(results_of(&out)["results"], json!([]));
}

#[tokio::test]
async fn fetch_adapters_return_markdown_file_and_summary() {
    let page = "# Title\n\nHello page body";
    let cases: Vec<(&str, &str, &str, Value)> = vec![
        ("firecrawl", "firecrawl-scrape", "/v1/scrape", json!({"success": true, "data": {"markdown": page, "metadata": {"title": "FC Title"}}})),
        ("jina-reader", "jina-reader", "/", json!({"code": 200, "data": {"title": "Jina Title", "content": page}})),
        ("tavily", "tavily-extract", "/extract", json!({"results": [{"url": "https://example.com/a", "raw_content": page}]})),
        ("exa", "exa-contents", "/contents", json!({"results": [{"url": "https://example.com/a", "title": "Exa Title", "text": page}]})),
    ];
    for (provider, format, path, reply) in cases {
        let fake = Fake::start(move |_| Reply::ok(reply.clone())).await;
        let cfg = cfg_for(provider, Capability::Fetch, &format!("{}{path}", fake.url()));
        let out = done(call(&fake, &cfg, adapter(Capability::Fetch, format), &req("", json!({"url": "https://example.com/a"}))).await);
        let (data, ext) = bytes_of(&out.outputs[0]);
        assert_eq!((data, ext), (page.as_bytes(), "md"), "{provider}");
        let v = results_of(&out);
        assert_eq!(v["results"][0]["url"], "https://example.com/a");
        assert!(v["results"][0]["snippet"].as_str().unwrap().contains("Hello page body"));
        let rec = &fake.requests()[0];
        assert_common_headers(rec);
        match provider {
            "exa" => assert_eq!(rec.header("x-api-key").unwrap(), KEY),
            _ => assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"), "{provider}"),
        }
    }
    // Jina 纯文本回退；非 http(s) 网址、空正文如实报错
    let fake = Fake::start(|_| Reply::text(200, "plain markdown body")).await;
    let cfg = cfg_for("jina-reader", Capability::Fetch, &fake.url());
    let a = adapter(Capability::Fetch, "jina-reader");
    let out = done(call(&fake, &cfg, a, &req("", json!({"url": "https://example.com/"}))).await);
    assert_eq!(bytes_of(&out.outputs[0]).0, b"plain markdown body");
    for bad in ["javascript:alert(1)", "file:///etc/passwd", "ftp://x/y", "not a url", ""] {
        assert!(matches!(call(&fake, &cfg, a, &req("", json!({"url": bad}))).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
    let empty = Fake::start(|_| Reply::ok(json!({"success": true, "data": {"markdown": "  "}}))).await;
    let cfg = cfg_for("firecrawl", Capability::Fetch, &empty.url());
    assert!(matches!(call(&empty, &cfg, adapter(Capability::Fetch, "firecrawl-scrape"), &req("", json!({"url": "https://example.com/"}))).await, Err(MediaError::Failed(_))));
}

// ---------------------------------------------------------------------------------------------
// 嵌入
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn openai_embeddings_sorts_by_index_and_reports_usage() {
    let fake = Fake::start(|_| {
        Reply::ok(json!({"data": [{"index": 1, "embedding": [0.3, 0.4]}, {"index": 0, "embedding": [0.1, 0.2]}], "usage": {"prompt_tokens": 7, "total_tokens": 7}}))
    })
    .await;
    let cfg = cfg_for("openai", Capability::Embedding, &format!("{}/v1/embeddings", fake.url()));
    let a = adapter(Capability::Embedding, "openai-embeddings");
    let out = done(call(&fake, &cfg, a, &req("text-embedding-3-small", json!({"input": ["a", "b"], "dimensions": 2}))).await);
    let parsed = super::embedding::parse_result(out.text.as_deref().unwrap()).unwrap();
    assert_eq!(parsed.vectors, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
    assert_eq!((parsed.prompt_tokens, parsed.total_tokens), (Some(7), Some(7)));
    let rec = &fake.requests()[0];
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    assert_eq!(rec.json(), json!({"model": "text-embedding-3-small", "input": ["a", "b"], "encoding_format": "float", "dimensions": 2}));

    // 向量个数不符 / 输入无效
    assert!(matches!(call(&fake, &cfg, a, &req("m", json!({"input": ["a", "b", "c"]}))).await, Err(MediaError::BadResponse(_))));
    for bad in [json!({}), json!({"input": []}), json!({"input": [""]}), json!({"input": [1]}), json!({"input": "str"}), json!({"input": ["a"], "dimensions": 0})] {
        assert!(matches!(call(&fake, &cfg, a, &req("m", bad.clone())).await, Err(MediaError::InvalidParams(_))), "{bad}");
    }
}

#[tokio::test]
async fn gemini_embeddings_use_header_key_and_model_path() {
    let fake = Fake::start(|_| Reply::ok(json!({"embeddings": [{"values": [1.0, 2.0]}]}))).await;
    let cfg = cfg_for("gemini", Capability::Embedding, &format!("{}/v1beta/models", fake.url()));
    let a = adapter(Capability::Embedding, "gemini-embeddings");
    let out = done(call(&fake, &cfg, a, &req("gemini-embedding-001", json!({"input": ["hello"], "dimensions": 256}))).await);
    assert_eq!(super::embedding::parse_result(out.text.as_deref().unwrap()).unwrap().vectors, vec![vec![1.0, 2.0]]);
    let rec = &fake.requests()[0];
    assert_eq!(rec.path, "/v1beta/models/gemini-embedding-001:batchEmbedContents");
    assert_eq!(rec.header("x-goog-api-key").unwrap(), KEY);
    assert!(rec.query.is_none(), "密钥不进 URL");
    assert_eq!(rec.json()["requests"][0], json!({"model": "models/gemini-embedding-001", "content": {"parts": [{"text": "hello"}]}, "outputDimensionality": 256}));
    assert!(matches!(call(&fake, &cfg, a, &req("../x", json!({"input": ["a"]}))).await, Err(MediaError::InvalidParams(_))));
}

// ---------------------------------------------------------------------------------------------
// 任务级（端到端）：凭据、落盘、轮询、取消、脱敏、恢复
// ---------------------------------------------------------------------------------------------

struct Env {
    ctx: MediaContext,
    vault: Arc<MemoryVault>,
    tmp: TempDir,
}

impl Env {
    async fn new(fake: &Fake, patches: &[(&str, Capability, &str)]) -> Env {
        let pool = store::open_memory().await.unwrap();
        let vault = Arc::new(MemoryVault::default());
        let tmp = tempdir();
        let mut reg = MediaRegistry::builtin().clone();
        for (p, c, path) in patches {
            reg = reg.with_base_url(p, *c, &format!("{}{path}", fake.url()));
        }
        let dyn_vault: Arc<dyn CredentialVault> = vault.clone();
        let ctx = MediaContext::for_test(pool, dyn_vault, &tmp, fake.http(), reg);
        Env { ctx, vault, tmp }
    }

    async fn add_key(&self, provider: &str, secret: &str) {
        add_key(&self.ctx.pool, &self.vault, provider, secret).await;
    }

    async fn submit(&self, kind: MediaKind, params: Value) -> MediaJob {
        super::submit(&self.ctx, kind, params.as_object().cloned().unwrap_or_default()).await.unwrap()
    }

    async fn wait(&self, id: &str) -> MediaJob {
        for _ in 0..1000 {
            let j = store::media::get(&self.ctx.pool, id).await.unwrap().unwrap();
            if !matches!(j.status, JobStatus::Queued | JobStatus::Running) {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("任务 {id} 没有在 10 秒内结束");
    }

    async fn metadata(&self, id: &str) -> Value {
        let s: String = sqlx::query_scalar("SELECT input_metadata_json FROM media_jobs WHERE id = ?1").bind(id).fetch_one(&self.ctx.pool).await.unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn files_under(&self, sub: &str) -> Vec<std::path::PathBuf> {
        let dir = self.tmp.join("media").join(sub);
        std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect()).unwrap_or_default()
    }
}

static KEY_COUNTER: AtomicUsize = AtomicUsize::new(0);

async fn add_key(pool: &SqlitePool, vault: &MemoryVault, provider: &str, secret: &str) {
    let n = KEY_COUNTER.fetch_add(1, Ordering::SeqCst);
    sqlx::query("INSERT OR IGNORE INTO providers (id, kind, display_name, grp, created_at, updated_at) VALUES (?1, 'builtin', ?1, 'free', 't', 't')")
        .bind(provider)
        .execute(pool)
        .await
        .unwrap();
    let cref = format!("test/{provider}/{n}");
    vault.set(&cref, secret).unwrap();
    sqlx::query("INSERT INTO connections (id, provider_id, account_label, kind, priority, credential_ref, created_at, updated_at) VALUES (?1, ?2, 'acc', 'apiKey', ?3, ?4, 't', 't')")
        .bind(format!("conn-{n}"))
        .bind(provider)
        .bind(n as i64)
        .bind(cref)
        .execute(pool)
        .await
        .unwrap();
}

fn file_payload(name: &str, bytes: &[u8]) -> Value {
    json!({"name": name, "size": bytes.len(), "mimeType": "audio/mpeg", "dataBase64": b64(bytes)})
}

#[tokio::test]
async fn job_without_provider_or_with_deferred_provider_fails_with_reason() {
    let fake = Fake::start(|_| Reply::text(500, "unreachable")).await;
    let env = Env::new(&fake, &[]).await;
    let j = env.submit(MediaKind::Image, json!({"prompt": "fox"})).await;
    assert_eq!((j.status, j.error.as_deref()), (JobStatus::Failed, Some("未选择供应商")));

    for (kind, provider, reason) in [
        (MediaKind::Tts, "edge-tts", "私有接口"),
        (MediaKind::Image, "gemini", "尚未接入"),
        (MediaKind::Asr, "assemblyai", "尚未验证"),
        (MediaKind::Search, "grok-web", "私有接口"),
        (MediaKind::Image, "no-such-provider", "未知供应商"),
        (MediaKind::Video, "openai", "不提供视频生成能力"),
    ] {
        env.add_key(provider, KEY).await; // 有凭据也不能把 deferred 当成 ready
        let j = env.submit(kind, json!({"providerId": provider, "prompt": "p", "text": "t", "query": "q"})).await;
        assert_eq!(j.status, JobStatus::Failed, "{provider}");
        let err = j.error.unwrap();
        assert!(err.contains("适配器尚未接入") && err.contains(reason), "{provider}: {err}");
        assert!(j.finished_at.is_some() && j.output_path.is_none());
    }
    assert!(fake.requests().is_empty(), "未接入的供应商不应发出任何请求");
}

#[tokio::test]
async fn job_without_credentials_reports_missing_api_key() {
    let fake = Fake::start(|_| Reply::text(500, "unreachable")).await;
    let env = Env::new(&fake, &[]).await;
    let params = json!({"providerId": "openai", "prompt": "fox"});
    let j = env.submit(MediaKind::Image, params.clone()).await;
    assert_eq!((j.status, j.error.as_deref()), (JobStatus::Failed, Some("未配置 API Key")));

    // 账户被停用 / 暂停 / 凭据库里没有条目：都等同于没有 Key
    env.add_key("openai", KEY).await;
    sqlx::query("UPDATE connections SET enabled = 0").execute(&env.ctx.pool).await.unwrap();
    assert_eq!(env.submit(MediaKind::Image, params.clone()).await.error.as_deref(), Some("未配置 API Key"));
    sqlx::query("UPDATE connections SET enabled = 1, status = 'paused'").execute(&env.ctx.pool).await.unwrap();
    assert_eq!(env.submit(MediaKind::Image, params.clone()).await.error.as_deref(), Some("未配置 API Key"));
    sqlx::query("UPDATE connections SET status = 'active'").execute(&env.ctx.pool).await.unwrap();
    let refs: Vec<String> = sqlx::query_scalar("SELECT credential_ref FROM connections").fetch_all(&env.ctx.pool).await.unwrap();
    for r in refs {
        env.vault.delete(&r).unwrap();
    }
    assert_eq!(env.submit(MediaKind::Image, params).await.error.as_deref(), Some("未配置 API Key"));
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn image_job_saves_file_in_media_dir_and_stores_only_metadata() {
    let png = b64(&PNG);
    let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"b64_json": png}]}))).await;
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "a red fox", "size": "1024x1024", "n": 1, "seed": 7, "engine": "x"})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    assert_eq!((j.summary.as_str(), j.model.as_str(), j.provider_id.as_str()), ("a red fox", "gpt-image-1", "openai"));
    assert!(j.finished_at.is_some() && j.error.is_none());

    let path = std::path::PathBuf::from(j.output_path.as_deref().unwrap());
    assert!(path.starts_with(env.tmp.join("media").join("image")), "{path:?}");
    assert_eq!(path.extension().unwrap(), "png");
    assert!(uuid::Uuid::parse_str(path.file_stem().unwrap().to_str().unwrap()).is_ok(), "文件名是生成的 uuid");
    assert_eq!(std::fs::read(&path).unwrap(), PNG);

    let rec = &fake.requests()[0];
    assert_common_headers(rec);
    assert_eq!(rec.header("authorization").unwrap(), format!("Bearer {KEY}"));
    let meta = env.metadata(&j.id).await;
    assert_eq!(meta["ignoredParams"], json!(["seed"]), "被能力表过滤掉的参数要留痕");
    assert_eq!(meta["format"], "openai-images");
    assert!(!meta.to_string().contains(&b64(&PNG)));
    assert!(!super::jobs::is_running(&env.ctx, &j.id), "结束后从运行表移除");
    let listed = super::list(&env.ctx, MediaKind::Image).await.unwrap();
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn image_job_downloads_from_exempt_host_with_uuid_file_names() {
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let fake = Fake::start(move |r| {
        if r.path == "/v1/images/generations" {
            let b = b2.lock().unwrap().clone();
            Reply::ok(json!({"data": [{"url": format!("{b}/files/..%2f..%2fevil.sh?x=1")}, {"url": format!("{b}/files/second.jpg")}]}))
        } else if r.path.ends_with("second.jpg") {
            Reply::bytes(200, "image/jpeg", vec![0xFF, 0xD8, 0xFF, 0xE0, 9, 9])
        } else {
            Reply::bytes(200, "text/html", [&PNG[..], b"rest"].concat())
        }
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x", "n": 2})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let files = env.files_under("image");
    assert_eq!(files.len(), 2);
    for f in &files {
        assert!(f.starts_with(env.tmp.join("media").join("image")));
        assert!(uuid::Uuid::parse_str(f.file_stem().unwrap().to_str().unwrap()).is_ok(), "{f:?}");
        assert!(!f.to_string_lossy().contains("evil"));
    }
    // 扩展名由真实文件头决定：html Content-Type 的 PNG 仍是 .png，jpg 仍是 .jpg
    let mut exts: Vec<_> = files.iter().map(|f| f.extension().unwrap().to_string_lossy().to_string()).collect();
    exts.sort();
    assert_eq!(exts, ["jpg", "png"]);
    // 下载请求不携带 API Key（authed=false）
    for rec in fake.requests().iter().filter(|r| r.method == "GET") {
        assert!(rec.header("authorization").is_none());
        assert_common_headers(rec);
    }
}

#[tokio::test]
async fn upstream_urls_pointing_to_internal_addresses_are_blocked() {
    for target in ["http://169.254.169.254/latest/meta-data/", "https://127.0.0.1/x.png", "https://10.0.0.8/x.png", "https://localhost/x.png", "file:///etc/passwd", "http://example.com/plain-http.png"] {
        let target = target.to_string();
        let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"url": target.clone()}]}))).await;
        let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
        env.add_key("openai", KEY).await;
        let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
        assert_eq!(j.status, JobStatus::Failed);
        assert!(j.error.as_deref().unwrap().contains("已拦截不安全的地址"), "{:?}", j.error);
        assert_eq!(fake.requests().len(), 1, "只有生成请求，没有对内网目标的任何访问");
        assert!(env.files_under("image").is_empty());
    }
}

#[tokio::test]
async fn redirect_from_download_host_to_internal_address_is_blocked() {
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let fake = Fake::start(move |r| match r.path.as_str() {
        "/v1/images/generations" => Reply::ok(json!({"data": [{"url": format!("{}/dl", b2.lock().unwrap())}]})),
        _ => Reply::redirect("http://169.254.169.254/latest/meta-data/"),
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("已拦截"), "{:?}", j.error);
    assert!(env.files_under("image").is_empty());
}

#[tokio::test]
async fn oversized_download_is_rejected_and_leaves_no_file() {
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let fake = Fake::start(move |r| match r.path.as_str() {
        "/v1/images/generations" => Reply::ok(json!({"data": [{"url": format!("{}/big", b2.lock().unwrap())}]})),
        _ => Reply::bytes(200, "image/png", vec![0x89; 51 * 1024 * 1024]),
    })
    .await;
    *base.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("超过大小上限"), "{:?}", j.error);
    assert!(env.files_under("image").is_empty());
}

#[tokio::test]
async fn account_failover_on_auth_error_uses_the_next_key() {
    let png = b64(&PNG);
    let fake = Fake::start(move |r| {
        if r.header("authorization") == Some("Bearer sk-first-bad-key-000000") {
            Reply::json(401, json!({"error": {"message": "Incorrect API key provided: sk-first-bad-key-000000"}}))
        } else {
            Reply::ok(json!({"data": [{"b64_json": png.clone()}]}))
        }
    })
    .await;
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", "sk-first-bad-key-000000").await;
    env.add_key("openai", "sk-second-good-key-111111").await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let auths: Vec<_> = fake.requests().iter().map(|r| r.header("authorization").unwrap().to_string()).collect();
    assert_eq!(auths, ["Bearer sk-first-bad-key-000000", "Bearer sk-second-good-key-111111"]);

    // 所有账户都失败：报错且不回显任何 Key（上游错误文本里带了 Key 也会被抹掉）
    let env2 = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env2.add_key("openai", "sk-first-bad-key-000000").await;
    let j = env2.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    let err = j.error.unwrap();
    assert!(err.contains("401") && err.contains("Incorrect API key"), "{err}");
    assert!(!err.contains("sk-first-bad-key-000000"), "{err}");
}

#[tokio::test]
async fn tts_job_writes_audio_with_extension_from_content() {
    let fake = Fake::start(|_| Reply::bytes(200, "audio/mpeg", MP3.to_vec())).await;
    let env = Env::new(&fake, &[("elevenlabs", Capability::Tts, "/v1/text-to-speech")]).await;
    env.add_key("elevenlabs", KEY).await;
    let j = env.submit(MediaKind::Tts, json!({"providerId": "elevenlabs", "text": "你好，世界", "voice": "voice123", "speed": 1, "format": "wav", "pitch": 2, "engine": "cloud", "intent": "save"})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let path = std::path::PathBuf::from(j.output_path.unwrap());
    assert!(path.starts_with(env.tmp.join("media").join("tts")));
    assert_eq!(path.extension().unwrap(), "mp3", "ElevenLabs 实际返回 mp3，扩展名跟随真实内容而不是请求的 wav");
    assert_eq!(std::fs::read(path).unwrap(), MP3);
    assert_eq!(j.summary, "你好，世界");
    assert_eq!(fake.requests()[0].header("xi-api-key").unwrap(), KEY);
}

#[tokio::test]
async fn asr_job_uploads_file_returns_transcript_and_cleans_up() {
    let fake = Fake::start(|_| Reply::ok(json!({"text": "你好世界"}))).await;
    let env = Env::new(&fake, &[("openai", Capability::Stt, "/v1/audio/transcriptions")]).await;
    env.add_key("openai", KEY).await;
    let audio = [&MP3[..], &[1u8; 300]].concat();
    let j = env.submit(MediaKind::Asr, json!({"providerId": "openai", "audio": file_payload("../../my voice.mp3", &audio), "language": "zh", "timestamps": false})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    assert_eq!(j.summary, "你好世界", "转写文本放在 summary");
    assert!(j.output_path.is_none());
    let rec = &fake.requests()[0];
    let file = rec.field("file").unwrap();
    assert_eq!((file.filename.as_deref(), file.data.as_slice()), (Some("audio.mp3"), audio.as_slice()));
    // 上传的输入已删除；元数据里没有音频内容和原始文件名路径
    assert!(!env.tmp.join("media").join("uploads").join(&j.id).exists());
    let meta = env.metadata(&j.id).await;
    let text = meta.to_string();
    assert!(!text.contains(&b64(&audio)) && !text.contains("dataBase64") && !text.contains("../"), "{text}");
    assert_eq!(meta["files"]["audio"]["ext"], "mp3");
}

#[tokio::test]
async fn upload_over_25mb_or_wrong_type_fails_without_calling_upstream() {
    let fake = Fake::start(|_| Reply::ok(json!({"text": "x"}))).await;
    let env = Env::new(&fake, &[("openai", Capability::Stt, "/v1/audio/transcriptions")]).await;
    env.add_key("openai", KEY).await;
    let big = vec![0u8; 25 * 1024 * 1024 + 1];
    let j = env.submit(MediaKind::Asr, json!({"providerId": "openai", "audio": file_payload("big.mp3", &big)})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("25 MB"), "{:?}", j.error);
    let j = env.submit(MediaKind::Asr, json!({"providerId": "openai", "audio": file_payload("run.exe", b"MZ")})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("不支持的文件类型"), "{:?}", j.error);
    let j = env.submit(MediaKind::Asr, json!({"providerId": "openai"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(fake.requests().is_empty());
    let uploads = env.tmp.join("media").join("uploads");
    assert!(!uploads.exists() || std::fs::read_dir(uploads).unwrap().next().is_none(), "失败后不留上传文件");
    // 数据库里没有上传内容
    let metas: Vec<String> = sqlx::query_scalar("SELECT input_metadata_json FROM media_jobs").fetch_all(&env.ctx.pool).await.unwrap();
    assert!(metas.iter().all(|m| m.len() < 2000), "元数据不应包含文件内容");
}

#[tokio::test]
async fn search_job_summary_is_structured_json_with_http_links_only() {
    let fake = Fake::start(|_| {
        Reply::ok(json!({"results": [
            {"title": "A", "url": "https://a.example/", "content": "alpha"},
            {"title": "evil", "url": "javascript:alert(1)", "content": "x"},
            {"title": "B", "url": "http://b.example/page", "content": "beta"}
        ]}))
    })
    .await;
    let env = Env::new(&fake, &[("tavily", Capability::Search, "/search")]).await;
    env.add_key("tavily", KEY).await;
    let j = env.submit(MediaKind::Search, json!({"providerId": "tavily", "query": "rust", "resultMode": "native", "count": 5, "region": "US", "timeRange": ""})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let v: Value = serde_json::from_str(&j.summary).unwrap();
    assert_eq!((v["origin"].as_str(), v["provider"].as_str()), (Some("native"), Some("tavily")));
    assert_eq!(v["results"], json!([{"title": "A", "url": "https://a.example/", "snippet": "alpha"}, {"title": "B", "url": "http://b.example/page", "snippet": "beta"}]));
    assert!(j.output_path.is_none());
}

#[tokio::test]
async fn fetch_job_saves_markdown_file() {
    let fake = Fake::start(|_| Reply::ok(json!({"success": true, "data": {"markdown": "# Doc\n\nbody", "metadata": {"title": "Doc"}}}))).await;
    let env = Env::new(&fake, &[("firecrawl", Capability::Fetch, "/v1/scrape")]).await;
    env.add_key("firecrawl", KEY).await;
    let j = env.submit(MediaKind::Search, json!({"providerId": "firecrawl", "url": "https://example.com/doc"})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    assert_eq!(std::fs::read_to_string(j.output_path.unwrap()).unwrap(), "# Doc\n\nbody");
    let v: Value = serde_json::from_str(&j.summary).unwrap();
    assert_eq!(v["results"][0]["title"], "Doc");
    assert_eq!(v["results"][0]["url"], "https://example.com/doc");
}

#[tokio::test]
async fn aggregated_search_merges_sources_and_reports_failures() {
    let fake = Fake::start(|r| match r.path.as_str() {
        "/tavily" => Reply::ok(json!({"results": [{"title": "T1", "url": "https://one.example/", "content": "a"}, {"title": "T2", "url": "https://two.example/", "content": "b"}]})),
        "/serper/search" => Reply::ok(json!({"organic": [{"title": "S1", "link": "https://two.example/", "snippet": "dup"}, {"title": "S2", "link": "https://three.example/", "snippet": "c"}]})),
        _ => Reply::json(500, json!({"error": "down"})),
    })
    .await;
    let env = Env::new(
        &fake,
        &[("tavily", Capability::Search, "/tavily"), ("serper", Capability::Search, "/serper"), ("exa", Capability::Search, "/exa"), ("brave-search", Capability::Search, "/brave")],
    )
    .await;
    env.add_key("tavily", "sk-tavily-key-aaaaaaaa").await;
    env.add_key("serper", "sk-serper-key-bbbbbbbb").await;
    let j = env.submit(MediaKind::Search, json!({"providerId": "tavily", "query": "rust", "resultMode": "aggregated", "count": 10})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let v: Value = serde_json::from_str(&j.summary).unwrap();
    assert_eq!(v["origin"], "aggregated");
    assert_eq!(v["provider"], "tavily+serper");
    let urls: Vec<_> = v["results"].as_array().unwrap().iter().map(|r| r["url"].as_str().unwrap()).collect();
    assert_eq!(urls, ["https://one.example/", "https://two.example/", "https://three.example/"], "按 URL 去重，来源轮流");
    // 每个来源只用自己的 Key
    for rec in fake.requests() {
        let auth = rec.header("authorization").or(rec.header("x-api-key")).unwrap();
        assert!(auth.contains(if rec.path.starts_with("/tavily") { "tavily" } else { "serper" }), "{} {auth}", rec.path);
    }

    // 只有一个来源成功：如实标 native，并列出失败的来源
    let env2 = Env::new(&fake, &[("tavily", Capability::Search, "/tavily"), ("exa", Capability::Search, "/exa")]).await;
    env2.add_key("tavily", KEY).await;
    env2.add_key("exa", KEY).await;
    let j = env2.submit(MediaKind::Search, json!({"providerId": "tavily", "query": "rust", "resultMode": "aggregated"})).await;
    let v: Value = serde_json::from_str(&j.summary).unwrap();
    assert_eq!((v["origin"].as_str(), v["provider"].as_str()), (Some("native"), Some("tavily")));
    assert_eq!(v["failed"][0]["provider"], "exa");
    assert!(v["failed"][0]["error"].as_str().unwrap().contains("500"));

    // 全部失败
    let env3 = Env::new(&fake, &[("exa", Capability::Search, "/exa")]).await;
    env3.add_key("exa", KEY).await;
    let j = env3.submit(MediaKind::Search, json!({"providerId": "exa", "query": "rust", "resultMode": "aggregated"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.unwrap().contains("所有搜索来源均失败"));
    // 没有任何凭据
    let env4 = Env::new(&fake, &[]).await;
    let j = env4.submit(MediaKind::Search, json!({"providerId": "tavily", "query": "rust", "resultMode": "aggregated"})).await;
    assert_eq!(j.error.as_deref(), Some("未配置 API Key"));
}

fn video_fake(statuses: Vec<Value>, base: Arc<Mutex<String>>) -> impl Fn(&Recorded) -> Reply + Send + Sync + 'static {
    let polls = Arc::new(AtomicUsize::new(0));
    let statuses = Arc::new(statuses);
    move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/videos/generations") => Reply::ok(json!({"request_id": "vid-1"})),
        ("GET", "/v1/videos/vid-1") => {
            let i = polls.fetch_add(1, Ordering::SeqCst).min(statuses.len() - 1);
            let mut s = statuses[i].clone();
            if let Some(u) = s.pointer_mut("/video/url") {
                *u = json!(format!("{}/media/v.mp4", base.lock().unwrap()));
            }
            Reply::ok(s)
        }
        ("GET", "/media/v.mp4") => Reply::bytes(200, "video/mp4", [b"\0\0\0\x18ftypmp42".as_slice(), &[0u8; 64]].concat()),
        _ => Reply::text(404, "no"),
    }
}

#[tokio::test]
async fn video_job_polls_with_progress_then_downloads_the_result() {
    let base = Arc::new(Mutex::new(String::new()));
    let fake = Fake::start(video_fake(
        vec![json!({"status": "pending", "progress": 10}), json!({"status": "pending", "progress": 60}), json!({"status": "done", "video": {"url": "x"}})],
        base.clone(),
    ))
    .await;
    *base.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("xai", Capability::Video, "/v1/videos")]).await;
    env.add_key("xai", KEY).await;
    let j = env.submit(MediaKind::Video, json!({"providerId": "xai", "prompt": "waves", "mode": "text2video", "duration": 5, "aspectRatio": "16:9", "resolution": "720p", "fps": 24})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
    let path = std::path::PathBuf::from(j.output_path.unwrap());
    assert!(path.starts_with(env.tmp.join("media").join("video")) && path.extension().unwrap() == "mp4");
    assert!(std::fs::metadata(&path).unwrap().len() > 64);
    assert_eq!(fake.requests().iter().filter(|r| r.path == "/v1/videos/vid-1").count(), 3);
    let meta = env.metadata(&j.id).await;
    assert_eq!(meta["progress"], 60, "进度写库");
    assert_eq!(meta["remote"]["id"], "vid-1", "上游任务句柄写库，供重启后恢复");
    assert_eq!(meta["remote"]["format"], "xai-video");
}

#[tokio::test]
async fn video_job_reports_upstream_failure_and_poll_timeout_and_transient_errors() {
    // 上游失败
    let base = Arc::new(Mutex::new(String::new()));
    let fake = Fake::start(video_fake(vec![json!({"status": "pending"}), json!({"status": "failed", "error": {"message": "moderation blocked"}})], base.clone())).await;
    let env = Env::new(&fake, &[("xai", Capability::Video, "/v1/videos")]).await;
    env.add_key("xai", KEY).await;
    let j = env.submit(MediaKind::Video, json!({"providerId": "xai", "prompt": "x", "mode": "text2video"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("moderation blocked"), "{:?}", j.error);
    assert!(j.output_path.is_none() && env.files_under("video").is_empty());

    // 一直 pending → 超时
    let fake = Fake::start(video_fake(vec![json!({"status": "pending"})], base.clone())).await;
    let mut env = Env::new(&fake, &[("xai", Capability::Video, "/v1/videos")]).await;
    env.ctx.timing.poll_timeout = Some(Duration::from_millis(200));
    env.add_key("xai", KEY).await;
    let j = env.submit(MediaKind::Video, json!({"providerId": "xai", "prompt": "x", "mode": "text2video"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("超时"), "{:?}", j.error);

    // 瞬时 503 不会让任务失败
    let n = Arc::new(AtomicUsize::new(0));
    let (n2, base2) = (n.clone(), Arc::new(Mutex::new(String::new())));
    let b3 = base2.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", _) => Reply::ok(json!({"request_id": "vid-1"})),
        ("GET", "/v1/videos/vid-1") => {
            if n2.fetch_add(1, Ordering::SeqCst) < 2 {
                Reply::json(503, json!({"error": "busy"}))
            } else {
                Reply::ok(json!({"status": "done", "video": {"url": format!("{}/media/v.mp4", b3.lock().unwrap())}}))
            }
        }
        _ => Reply::bytes(200, "video/mp4", [b"\0\0\0\x18ftypmp42".as_slice(), &[0u8; 8]].concat()),
    })
    .await;
    *base2.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("xai", Capability::Video, "/v1/videos")]).await;
    env.add_key("xai", KEY).await;
    let j = env.submit(MediaKind::Video, json!({"providerId": "xai", "prompt": "x", "mode": "text2video"})).await;
    assert_eq!(j.status, JobStatus::Succeeded, "{:?}", j.error);
}

#[tokio::test]
async fn cancel_really_stops_polling_and_cancels_upstream_task() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p2 = polls.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/v1/image_to_video") => Reply::ok(json!({"id": "task-c"})),
        ("GET", "/v1/tasks/task-c") => {
            p2.fetch_add(1, Ordering::SeqCst);
            Reply::ok(json!({"status": "RUNNING", "progress": 0.2}))
        }
        ("DELETE", "/v1/tasks/task-c") => Reply::bytes(204, "text/plain", vec![]),
        _ => Reply::text(404, "no"),
    })
    .await;
    let mut env = Env::new(&fake, &[("runwayml", Capability::Video, "/v1")]).await;
    env.ctx.timing.inline_wait = Duration::ZERO;
    env.add_key("runwayml", KEY).await;
    let j = env
        .submit(
            MediaKind::Video,
            json!({"providerId": "runwayml", "mode": "image2video", "prompt": "pan", "image": {"name": "a.png", "size": 8, "mimeType": "image/png", "dataBase64": b64(&PNG)}}),
        )
        .await;
    assert!(matches!(j.status, JobStatus::Running | JobStatus::Queued), "提交后立即返回运行中的任务");

    // 等待上游句柄入库、开始轮询
    for _ in 0..500 {
        if polls.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(polls.load(Ordering::SeqCst) >= 2);
    assert_eq!(env.metadata(&j.id).await["remote"]["id"], "task-c");
    assert_eq!(env.metadata(&j.id).await["progress"], 20);

    let cancelled = super::cancel(&env.ctx, &j.id).await.unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(cancelled.finished_at.is_some());
    let del = fake.requests().into_iter().find(|r| r.method == "DELETE").expect("应向上游发出取消请求");
    assert_eq!(del.path, "/v1/tasks/task-c");
    assert_eq!(del.header("authorization").unwrap(), format!("Bearer {KEY}"));

    // 之后不再轮询，状态不会被任务线程改回去
    tokio::time::sleep(Duration::from_millis(80)).await;
    let after = polls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(polls.load(Ordering::SeqCst), after, "取消后必须停止轮询");
    let job = store::media::get(&env.ctx.pool, &j.id).await.unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.output_path.is_none() && job.error.is_none());
    assert!(!super::jobs::is_running(&env.ctx, &j.id));
    assert!(!env.tmp.join("media").join("uploads").join(&j.id).exists(), "取消后删除上传的输入");
    // 幂等
    assert_eq!(super::cancel(&env.ctx, &j.id).await.unwrap().status, JobStatus::Cancelled);
    assert!(matches!(super::cancel(&env.ctx, "nope").await, Err(crate::error::AppError::NotFound(_))));
}

#[tokio::test]
async fn cancelling_a_finished_job_changes_nothing() {
    let png = b64(&PNG);
    let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"b64_json": png.clone()}]}))).await;
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Succeeded);
    let again = super::cancel(&env.ctx, &j.id).await.unwrap();
    assert_eq!(again.status, JobStatus::Succeeded);
    assert!(std::path::Path::new(again.output_path.as_deref().unwrap()).exists(), "已完成任务的产物不被清理");
}

#[tokio::test]
async fn cancel_during_slow_synchronous_request_wins_over_late_result() {
    let png = b64(&PNG);
    let fake = Fake::start(move |_| Reply::ok(json!({"data": [{"b64_json": png.clone()}]})).delayed(Duration::from_millis(400))).await;
    let mut env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.ctx.timing.inline_wait = Duration::ZERO;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Running);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(super::cancel(&env.ctx, &j.id).await.unwrap().status, JobStatus::Cancelled);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let job = store::media::get(&env.ctx.pool, &j.id).await.unwrap().unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert!(job.output_path.is_none());
    assert!(env.files_under("image").is_empty());
}

// ---- 脱敏 ----

#[tokio::test]
async fn api_key_never_appears_in_errors_metadata_or_database() {
    let leaky = Fake::start(|r| {
        let auth = r.header("authorization").unwrap_or("").to_string();
        // 恶意/糟糕的上游把收到的凭据回显在错误里
        Reply::json(400, json!({"error": {"message": format!("bad request, you sent {auth} and key={}", auth.trim_start_matches("Bearer "))}}))
    })
    .await;
    let env = Env::new(&leaky, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    let secret = "sk-LEAKTEST-9f8e7d6c5b4a3210";
    env.add_key("openai", secret).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    let err = j.error.unwrap();
    assert!(err.contains("400") && !err.contains(secret) && !err.contains("9f8e7d6c"), "{err}");

    // 整张表序列化后也不含 Key
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as("SELECT input_metadata_json, summary, error FROM media_jobs").fetch_all(&env.ctx.pool).await.unwrap();
    assert!(!format!("{rows:?}").contains(secret));
}

#[test]
fn debug_output_of_secrets_and_contexts_is_redacted() {
    let s = Secret::new(KEY);
    assert_eq!(format!("{s:?}"), "Secret(***)");
    assert!(!format!("{:#?}", Some(&s)).contains("sk-test"));
    let cfg = cfg_for("openai", Capability::Image, "https://x.example");
    let http = super::http::MediaHttp::new().unwrap();
    let cx = AdapterCtx { http: &http, cfg: &cfg, secret: &s, provider_id: "openai" };
    assert!(!format!("{cx:?}").contains("sk-test"));
    let rq = req("m", json!({"prompt": "x"}));
    assert!(!format!("{rq:?}").contains("sk-test"));
}

#[test]
fn redact_text_removes_keys_bearer_tokens_and_truncates() {
    let s = Secret::new("sk-abcdef-123456");
    let out = jobs::redact_text("auth sk-abcdef-123456 failed; Authorization: Bearer abcdefghijklmnop.qrs-tuv==\nnext", &[&s]);
    assert!(!out.contains("sk-abcdef") && !out.contains("abcdefghijklmnop"), "{out}");
    assert!(out.contains("[已隐藏]") && !out.contains('\n'));
    assert_eq!(jobs::redact_text("Bearer short", &[]), "Bearer short", "太短的不是令牌，不乱改");
    assert!(jobs::redact_text(&"x".repeat(5000), &[]).chars().count() <= 401);
    // 太短的「密钥」不参与替换，避免把普通文字抹掉
    assert_eq!(jobs::redact_text("abc", &[&Secret::new("abc")]), "abc");
}

#[derive(Clone)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn logs_never_contain_api_keys_or_uploaded_content() {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sub = {
        let b = buf.clone();
        tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_ansi(false).with_writer(move || LogBuf(b.clone())).finish()
    };
    let _guard = tracing::subscriber::set_default(sub);

    let secret = "sk-LOGLEAK-1a2b3c4d5e6f7a8b";
    let audio = [&MP3[..], b"UNIQUE-AUDIO-PAYLOAD-0123456789"].concat();
    let fake = Fake::start(|r| match r.path.as_str() {
        "/v1/audio/transcriptions" => Reply::json(500, json!({"error": {"message": "upstream exploded"}})),
        _ => Reply::ok(json!({"results": [{"title": "T", "url": "https://t.example/", "content": "c"}]})),
    })
    .await;
    let env = Env::new(&fake, &[("openai", Capability::Stt, "/v1/audio/transcriptions"), ("tavily", Capability::Search, "/search")]).await;
    env.add_key("openai", secret).await;
    env.add_key("tavily", secret).await;
    let a = env.submit(MediaKind::Asr, json!({"providerId": "openai", "audio": file_payload("a.mp3", &audio)})).await;
    assert_eq!(a.status, JobStatus::Failed);
    let b = env.submit(MediaKind::Search, json!({"providerId": "tavily", "query": "q"})).await;
    assert_eq!(b.status, JobStatus::Succeeded);
    super::cancel(&env.ctx, &a.id).await.unwrap();

    let logs = String::from_utf8_lossy(&buf.lock().unwrap()).to_string();
    assert!(!logs.contains(secret) && !logs.contains("LOGLEAK"), "日志含密钥：{logs}");
    assert!(!logs.contains("UNIQUE-AUDIO-PAYLOAD") && !logs.contains(&b64(&audio)), "日志含上传内容");
}

// ---- 路径与参数安全（任务级）----

#[tokio::test]
async fn unsafe_model_and_voice_ids_fail_before_any_request() {
    let fake = Fake::start(|_| Reply::bytes(200, "audio/mpeg", MP3.to_vec())).await;
    let env = Env::new(&fake, &[("elevenlabs", Capability::Tts, "/v1/text-to-speech"), ("fal-ai", Capability::Image, "")]).await;
    env.add_key("elevenlabs", KEY).await;
    env.add_key("fal-ai", KEY).await;
    let j = env.submit(MediaKind::Tts, json!({"providerId": "elevenlabs", "text": "hi", "voice": "../../admin"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("音色 ID"));
    let j = env.submit(MediaKind::Image, json!({"providerId": "fal-ai", "prompt": "x", "model": "../../etc/passwd"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn malformed_upstream_responses_fail_the_job_instead_of_leaving_it_running() {
    let fake = Fake::start(|_| Reply::bytes(200, "application/json", b"\xff\xfe not json".to_vec())).await;
    let env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().contains("无法解析"), "{:?}", j.error);
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_jobs WHERE status IN ('queued','running')").fetch_one(&env.ctx.pool).await.unwrap();
    assert_eq!(active, 0);
}

// ---- 重启恢复 ----

async fn insert_active(env: &Env, id: &str, kind: MediaKind, provider: &str, meta: Value) {
    store::media::insert(
        &env.ctx.pool,
        &store::media::NewJob {
            id,
            kind,
            provider_id: provider,
            model_id: "",
            summary: "left over",
            input_metadata_json: &meta.to_string(),
            status: JobStatus::Running,
            error: None,
            created_at: &store::now_ts(),
            finished_at: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn restart_recovery_resumes_pollable_jobs_and_fails_the_rest() {
    let base = Arc::new(Mutex::new(String::new()));
    let fake = Fake::start(video_fake(vec![json!({"status": "pending"}), json!({"status": "done", "video": {"url": "x"}})], base.clone())).await;
    *base.lock().unwrap() = fake.url();
    let env = Env::new(&fake, &[("xai", Capability::Video, "/v1/videos")]).await;
    env.add_key("xai", KEY).await;

    // 1) 有上游句柄且凭据仍在：重新轮询并完成
    let remote = serde_json::to_value(RemoteTask::new("xai-video", "vid-1")).unwrap();
    insert_active(&env, "resume-me", MediaKind::Video, "xai", json!({"params": {}, "remote": remote})).await;
    // 2) 没有上游句柄（同步请求中途被打断）：标记为中断
    insert_active(&env, "no-remote", MediaKind::Image, "openai", json!({"params": {"prompt": "x"}})).await;
    // 3) 有句柄但凭据没了：标记为中断并说明原因
    let remote2 = serde_json::to_value(RemoteTask::new("runway-image", "t-9")).unwrap();
    insert_active(&env, "no-key", MediaKind::Image, "runwayml", json!({"remote": remote2})).await;
    // 4) 元数据损坏
    insert_active(&env, "corrupt", MediaKind::Tts, "elevenlabs", json!("garbage")).await;
    // 孤儿上传目录会被清掉，仍在恢复的任务保留
    for id in ["resume-me", "orphan"] {
        let dir = env.tmp.join("media").join("uploads").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x.mp3"), b"x").unwrap();
    }

    let report = super::recover_interrupted(&env.ctx).await.unwrap();
    assert_eq!((report.resumed, report.interrupted), (1, 3));

    let resumed = env.wait("resume-me").await;
    assert_eq!(resumed.status, JobStatus::Succeeded, "{:?}", resumed.error);
    assert!(std::path::Path::new(resumed.output_path.as_deref().unwrap()).starts_with(env.tmp.join("media").join("video")));

    let j = store::media::get(&env.ctx.pool, "no-remote").await.unwrap().unwrap();
    assert_eq!((j.status, j.error.as_deref()), (JobStatus::Failed, Some("应用重启中断")));
    assert!(j.finished_at.is_some());
    let j = store::media::get(&env.ctx.pool, "no-key").await.unwrap().unwrap();
    assert_eq!(j.status, JobStatus::Failed);
    assert!(j.error.as_deref().unwrap().starts_with("应用重启中断") && j.error.as_deref().unwrap().contains("未配置 API Key"), "{:?}", j.error);
    let j = store::media::get(&env.ctx.pool, "corrupt").await.unwrap().unwrap();
    assert_eq!((j.status, j.error.as_deref()), (JobStatus::Failed, Some("应用重启中断")));

    assert!(!env.tmp.join("media").join("uploads").join("orphan").exists());
    // 恢复是幂等的：再跑一次不会改动已结束的任务
    let again = super::recover_interrupted(&env.ctx).await.unwrap();
    assert_eq!((again.resumed, again.interrupted), (0, 0));
}

#[tokio::test]
async fn recovery_does_not_touch_jobs_running_in_this_process() {
    let fake = Fake::start(|_| Reply::ok(json!({"data": [{"b64_json": b64(&PNG)}]})).delayed(Duration::from_millis(300))).await;
    let mut env = Env::new(&fake, &[("openai", Capability::Image, "/v1/images/generations")]).await;
    env.ctx.timing.inline_wait = Duration::ZERO;
    env.add_key("openai", KEY).await;
    let j = env.submit(MediaKind::Image, json!({"providerId": "openai", "prompt": "x"})).await;
    let report = super::recover_interrupted(&env.ctx).await.unwrap();
    assert_eq!((report.resumed, report.interrupted), (0, 0));
    assert_eq!(env.wait(&j.id).await.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn resumed_job_can_be_cancelled() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p2 = polls.clone();
    let fake = Fake::start(move |r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/v1/tasks/t-1") => {
            p2.fetch_add(1, Ordering::SeqCst);
            Reply::ok(json!({"status": "RUNNING"}))
        }
        ("DELETE", "/v1/tasks/t-1") => Reply::bytes(204, "text/plain", vec![]),
        _ => Reply::text(404, "no"),
    })
    .await;
    let env = Env::new(&fake, &[("runwayml", Capability::Image, "/v1")]).await;
    env.add_key("runwayml", KEY).await;
    insert_active(&env, "r1", MediaKind::Image, "runwayml", json!({"remote": serde_json::to_value(RemoteTask::new("runway-image", "t-1")).unwrap()})).await;
    assert_eq!(super::recover_interrupted(&env.ctx).await.unwrap().resumed, 1);
    for _ in 0..300 {
        if polls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(super::cancel(&env.ctx, "r1").await.unwrap().status, JobStatus::Cancelled);
    assert!(fake.requests().iter().any(|r| r.method == "DELETE"));
}

// ---- 嵌入（对外函数）----

#[tokio::test]
async fn embed_function_resolves_credentials_and_fails_over() {
    let fake = Fake::start(|r| {
        if r.header("authorization") == Some("Bearer sk-bad-embed-key-00000") {
            Reply::json(429, json!({"error": {"message": "rate limited for sk-bad-embed-key-00000"}}))
        } else {
            Reply::ok(json!({"data": [{"index": 0, "embedding": [0.5, 0.25]}], "usage": {"prompt_tokens": 3, "total_tokens": 3}}))
        }
    })
    .await;
    let env = Env::new(&fake, &[("jina-ai", Capability::Embedding, "/v1/embeddings")]).await;
    // 没有 Key
    assert_eq!(super::embed(&env.ctx, "jina-ai", None, vec!["hi".into()], None).await.unwrap_err(), MediaError::NoCredential);
    env.add_key("jina-ai", "sk-bad-embed-key-00000").await;
    env.add_key("jina-ai", "sk-good-embed-key-1111").await;
    let r = super::embed(&env.ctx, "jina-ai", None, vec!["hi".into()], Some(2)).await.unwrap();
    assert_eq!((r.vectors, r.total_tokens), (vec![vec![0.5, 0.25]], Some(3)));
    assert_eq!(fake.requests()[0].json()["model"], "jina-embeddings-v3", "默认模型来自 media_providers.json");
    // deferred / 不具备该能力
    assert!(matches!(super::embed(&env.ctx, "nvidia", None, vec!["x".into()], None).await, Err(MediaError::Unsupported(ref m)) if m.contains("尚未接入")));
    assert!(matches!(super::embed(&env.ctx, "elevenlabs", None, vec!["x".into()], None).await, Err(MediaError::Unsupported(_))));
    // 全部账户限流：错误里不含 Key
    let env2 = Env::new(&fake, &[("jina-ai", Capability::Embedding, "/v1/embeddings")]).await;
    env2.add_key("jina-ai", "sk-bad-embed-key-00000").await;
    let err = super::embed(&env2.ctx, "jina-ai", None, vec!["hi".into()], None).await.unwrap_err().to_string();
    assert!(err.contains("429") && !err.contains("sk-bad-embed-key-00000"), "{err}");
}

#[tokio::test]
async fn long_summary_and_metadata_are_truncated_and_prompts_are_summarized() {
    let fake = Fake::start(|_| Reply::text(500, "unreachable")).await;
    let env = Env::new(&fake, &[]).await;
    let j = env.submit(MediaKind::Tts, json!({"text": "字".repeat(300)})).await;
    assert_eq!(j.summary.chars().count(), 81);
    assert_eq!(env.submit(MediaKind::Video, json!({})).await.summary, "video 任务");
    let big = "A".repeat(50_000);
    let j = env.submit(MediaKind::Asr, json!({"note": big, "nested": {"img": "B".repeat(5000)}})).await;
    let stored: String = sqlx::query_scalar("SELECT input_metadata_json FROM media_jobs WHERE id = ?1").bind(&j.id).fetch_one(&env.ctx.pool).await.unwrap();
    assert!(stored.len() < 600, "{stored}");
    assert!(stored.contains("omitted 50000 chars"));
}

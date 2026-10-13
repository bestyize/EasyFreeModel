//! 图像生成适配器。
//!
//! 格式：`openai-images`（OpenAI 兼容 `/images/generations`，含 `/images/edits`）、`stability-v2`、
//! `bfl-flux`（提交 + polling_url 轮询）、`fal-queue`（队列：status_url / response_url / cancel_url）、
//! `runway-image`（/tasks/{id} 轮询）。请求字段由 media_providers.json 的 `params` 白名单控制。

use async_trait::async_trait;
use base64::Engine;
use reqwest::multipart::{Form, Part};
use reqwest::Method;
use serde_json::{json, Map, Value};

use super::adapter::util::{closest_ratio, parse_size, safe_path, safe_path_segment, str_at};
use super::adapter::{
    AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, OutputItem, PollState, RemoteTask, Submission,
};
use super::files::{ext_from_mime, sniff_ext};
use super::registry::Capability;
use super::runway;

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![Box::new(OpenAiImages), Box::new(StabilityV2), Box::new(BflFlux), Box::new(FalQueue), Box::new(RunwayImage)]
}

/// 能力表允许的可选参数才会被取出。
fn opt<'a>(cx: &AdapterCtx<'_>, req: &'a MediaRequest, key: &str) -> Option<&'a Value> {
    if cx.cfg.has_param(key) {
        req.params.get(key).filter(|v| !v.is_null() && v.as_str().map(|s| !s.trim().is_empty()).unwrap_or(true))
    } else {
        None
    }
}

fn opt_u64(cx: &AdapterCtx<'_>, req: &MediaRequest, key: &str) -> Option<u64> {
    if cx.cfg.has_param(key) {
        req.u64_param(key)
    } else {
        None
    }
}

fn opt_size(cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Option<(u32, u32, String)>, MediaError> {
    match opt(cx, req, "size").and_then(Value::as_str) {
        None => Ok(None),
        Some(s) => {
            let (w, h) = parse_size(s).ok_or_else(|| MediaError::InvalidParams("尺寸格式应为 宽x高，例如 1024x1024".into()))?;
            if w > 8192 || h > 8192 {
                return Err(MediaError::InvalidParams("尺寸过大".into()));
            }
            Ok(Some((w, h, s.trim().to_string())))
        }
    }
}

fn check_n(n: Option<u64>) -> Result<Option<u64>, MediaError> {
    match n {
        Some(0) => Err(MediaError::InvalidParams("张数至少为 1".into())),
        Some(v) if v > 10 => Err(MediaError::InvalidParams("张数最多为 10".into())),
        other => Ok(other),
    }
}

fn b64_item(b64: &str) -> Result<OutputItem, MediaError> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| MediaError::BadResponse("上游返回的图片不是有效的 base64".into()))?;
    if data.is_empty() {
        return Err(MediaError::BadResponse("上游返回了空图片".into()));
    }
    let ext = sniff_ext(&data).unwrap_or("png").to_string();
    Ok(OutputItem::Bytes { data, ext })
}

fn url_item(url: &str, content_type: Option<&str>) -> OutputItem {
    OutputItem::Remote { url: url.to_string(), ext: content_type.and_then(ext_from_mime).unwrap_or("png").to_string(), authed: false }
}

// ---------------------------------------------------------------------------------------------
// OpenAI 兼容
// ---------------------------------------------------------------------------------------------

pub struct OpenAiImages;

#[async_trait]
impl MediaAdapter for OpenAiImages {
    fn format(&self) -> &'static str {
        "openai-images"
    }

    fn capability(&self) -> Capability {
        Capability::Image
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let prompt = req.require_str("prompt", "提示词")?;
        let n = check_n(opt_u64(cx, req, "n"))?;
        let size = opt_size(cx, req)?;
        let edit = req.str_param("mode") == Some("edit");

        let v = if edit {
            let Some(url) = cx.endpoint("edit") else {
                return Err(MediaError::Unsupported("该供应商不支持图片编辑".into()));
            };
            let file = req.file("image").ok_or_else(|| MediaError::InvalidParams("图片编辑需要提供待编辑的图片".into()))?;
            let part = Part::bytes(file.read().await?)
                .file_name(file.upload_name("image"))
                .mime_str(&file.mime)
                .map_err(|_| MediaError::InvalidParams("图片类型无效".into()))?;
            let mut form = Form::new().text("model", model.to_string()).text("prompt", prompt.to_string()).part("image", part);
            if let Some(n) = n {
                form = form.text("n", n.to_string());
            }
            if let Some((_, _, s)) = &size {
                form = form.text("size", s.clone());
            }
            cx.http.send_json(cx.post(url)?.multipart(form)).await?
        } else {
            let mut body = Map::new();
            body.insert("model".into(), json!(model));
            body.insert("prompt".into(), json!(prompt));
            if let Some(n) = n {
                body.insert("n".into(), json!(n));
            }
            if let Some((_, _, s)) = &size {
                body.insert("size".into(), json!(s));
            }
            cx.http.send_json(cx.post(&cx.cfg.base_url)?.json(&Value::Object(body))).await?
        };

        let data = v.get("data").and_then(Value::as_array).ok_or_else(|| MediaError::BadResponse("缺少 data 数组".into()))?;
        let mut outputs = Vec::new();
        for item in data {
            if let Some(b64) = item.get("b64_json").and_then(Value::as_str) {
                outputs.push(b64_item(b64)?);
            } else if let Some(url) = item.get("url").and_then(Value::as_str) {
                outputs.push(url_item(url, None));
            }
        }
        if outputs.is_empty() {
            return Err(MediaError::BadResponse("上游没有返回图片".into()));
        }
        Ok(Submission::Done(Outcome::files(outputs)))
    }
}

// ---------------------------------------------------------------------------------------------
// Stability AI v2beta（multipart，JSON 响应）
// ---------------------------------------------------------------------------------------------

pub struct StabilityV2;

const STABILITY_RATIOS: &[&str] = &["16:9", "1:1", "21:9", "2:3", "3:2", "4:5", "5:4", "9:16", "9:21"];

#[async_trait]
impl MediaAdapter for StabilityV2 {
    fn format(&self) -> &'static str {
        "stability-v2"
    }

    fn capability(&self) -> Capability {
        Capability::Image
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let prompt = req.require_str("prompt", "提示词")?;
        if req.str_param("mode") == Some("edit") {
            return Err(MediaError::Unsupported("该供应商不支持图片编辑".into()));
        }
        let segment = if model.contains("ultra") {
            "ultra"
        } else if model.contains("sd3") {
            "sd3"
        } else {
            "core"
        };
        let mut form = Form::new().text("prompt", prompt.to_string()).text("output_format", "png");
        if segment == "sd3" {
            safe_path_segment(model, "模型 ID")?;
            form = form.text("model", model.to_string());
        }
        if let Some((w, h, _)) = opt_size(cx, req)? {
            if let Some(r) = closest_ratio(w, h, STABILITY_RATIOS) {
                form = form.text("aspect_ratio", r.to_string());
            }
        }
        if let Some(seed) = opt_u64(cx, req, "seed") {
            form = form.text("seed", seed.min(4_294_967_294).to_string());
        }
        let url = format!("{}/{segment}", cx.cfg.base_url.trim_end_matches('/'));
        let rb = cx.post(&url)?.header(reqwest::header::ACCEPT, "application/json").multipart(form);
        let v = cx.http.send_json(rb).await?;
        if let Some(reason) = str_at(&v, &["finish_reason"]) {
            if reason != "SUCCESS" {
                return Err(MediaError::Failed(format!("Stability 未成功生成图片（{reason}），可能被内容审核拦截")));
            }
        }
        let b64 = str_at(&v, &["image"]).ok_or_else(|| MediaError::BadResponse("缺少 image 字段".into()))?;
        Ok(Submission::Done(Outcome::files(vec![b64_item(b64)?])))
    }
}

// ---------------------------------------------------------------------------------------------
// Black Forest Labs（FLUX）：提交返回 polling_url
// ---------------------------------------------------------------------------------------------

pub struct BflFlux;

#[async_trait]
impl MediaAdapter for BflFlux {
    fn format(&self) -> &'static str {
        "bfl-flux"
    }

    fn capability(&self) -> Capability {
        Capability::Image
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        safe_path_segment(model, "模型 ID")?;
        let prompt = req.require_str("prompt", "提示词")?;
        if req.str_param("mode") == Some("edit") {
            return Err(MediaError::Unsupported("该供应商暂不支持图片编辑".into()));
        }
        let mut body = json!({"prompt": prompt, "output_format": "png"});
        if let Some((w, h, _)) = opt_size(cx, req)? {
            body["width"] = json!(w);
            body["height"] = json!(h);
        }
        if let Some(seed) = opt_u64(cx, req, "seed") {
            body["seed"] = json!(seed);
        }
        let url = format!("{}/{model}", cx.cfg.base_url.trim_end_matches('/'));
        let v = cx.http.send_json(cx.post(&url)?.json(&body)).await?;
        let id = str_at(&v, &["id"]).unwrap_or("").to_string();
        let poll = str_at(&v, &["polling_url"]).ok_or_else(|| MediaError::BadResponse("缺少 polling_url".into()))?;
        let mut task = RemoteTask::new("bfl-flux", id);
        task.poll_url = Some(cx.trusted_url(poll)?);
        Ok(Submission::Pending(task))
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        let url = cx.trusted_url(task.poll_url.as_deref().ok_or_else(|| MediaError::BadResponse("缺少轮询地址".into()))?)?;
        let v = cx.http.send_json(cx.get(&url)?).await?;
        match str_at(&v, &["status"]).unwrap_or("") {
            "Ready" => {
                let sample = str_at(&v, &["result", "sample"]).ok_or_else(|| MediaError::BadResponse("任务完成但缺少 result.sample".into()))?;
                Ok(PollState::Done(Outcome::files(vec![url_item(sample, None)])))
            }
            s @ ("Error" | "Failed") => {
                let why = str_at(&v, &["error"]).or_else(|| str_at(&v, &["details"])).unwrap_or("上游未说明原因");
                Err(MediaError::Failed(format!("BFL 生成失败（{s}）：{why}")))
            }
            s @ ("Request Moderated" | "Content Moderated") => Err(MediaError::Failed(format!("BFL 内容审核未通过（{s}）"))),
            _ => {
                let progress = v.get("progress").and_then(Value::as_f64).map(|p| (p.clamp(0.0, 1.0) * 100.0) as u8);
                Ok(PollState::Pending { progress })
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// fal.ai 队列
// ---------------------------------------------------------------------------------------------

pub struct FalQueue;

fn fal_images(v: &Value) -> Vec<OutputItem> {
    let mut out = Vec::new();
    let mut push = |img: &Value| {
        if let Some(u) = img.get("url").and_then(Value::as_str) {
            out.push(url_item(u, img.get("content_type").and_then(Value::as_str)));
        }
    };
    if let Some(arr) = v.get("images").and_then(Value::as_array) {
        arr.iter().for_each(&mut push);
    } else if let Some(img) = v.get("image") {
        push(img);
    }
    out
}

#[async_trait]
impl MediaAdapter for FalQueue {
    fn format(&self) -> &'static str {
        "fal-queue"
    }

    fn capability(&self) -> Capability {
        Capability::Image
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        safe_path(model, "模型 ID")?;
        let prompt = req.require_str("prompt", "提示词")?;
        if req.str_param("mode") == Some("edit") {
            return Err(MediaError::Unsupported("该供应商暂不支持图片编辑".into()));
        }
        let mut body = json!({"prompt": prompt});
        if let Some(n) = check_n(opt_u64(cx, req, "n"))? {
            body["num_images"] = json!(n);
        }
        if let Some((w, h, _)) = opt_size(cx, req)? {
            body["image_size"] = json!({"width": w, "height": h});
        }
        if let Some(seed) = opt_u64(cx, req, "seed") {
            body["seed"] = json!(seed);
        }
        let url = format!("{}/{model}", cx.cfg.base_url.trim_end_matches('/'));
        let v = cx.http.send_json(cx.post(&url)?.json(&body)).await?;
        let direct = fal_images(&v);
        if !direct.is_empty() {
            return Ok(Submission::Done(Outcome::files(direct)));
        }
        let id = str_at(&v, &["request_id"]).ok_or_else(|| MediaError::BadResponse("缺少 request_id".into()))?;
        let status_url = str_at(&v, &["status_url"]).ok_or_else(|| MediaError::BadResponse("缺少 status_url".into()))?;
        let response_url = str_at(&v, &["response_url"]).ok_or_else(|| MediaError::BadResponse("缺少 response_url".into()))?;
        let mut task = RemoteTask::new("fal-queue", id);
        task.poll_url = Some(cx.trusted_url(status_url)?);
        task.cancel_url = str_at(&v, &["cancel_url"]).map(|u| cx.trusted_url(u)).transpose()?;
        task.data = json!({"responseUrl": cx.trusted_url(response_url)?});
        Ok(Submission::Pending(task))
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        let status_url = cx.trusted_url(task.poll_url.as_deref().ok_or_else(|| MediaError::BadResponse("缺少轮询地址".into()))?)?;
        let s = cx.http.send_json(cx.get(&status_url)?).await?;
        match str_at(&s, &["status"]).unwrap_or("") {
            "COMPLETED" => {
                if let Some(err) = str_at(&s, &["error"]).filter(|e| !e.is_empty()) {
                    return Err(MediaError::Failed(format!("fal 任务失败：{err}")));
                }
                let response_url =
                    task.data.get("responseUrl").and_then(Value::as_str).ok_or_else(|| MediaError::BadResponse("缺少 response_url".into()))?;
                let result = cx.http.send_json(cx.get(&cx.trusted_url(response_url)?)?).await?;
                let items = fal_images(&result);
                if items.is_empty() {
                    Err(MediaError::BadResponse("fal 任务完成但没有返回图片".into()))
                } else {
                    Ok(PollState::Done(Outcome::files(items)))
                }
            }
            "FAILED" | "ERROR" => Err(MediaError::Failed(format!("fal 任务失败：{}", str_at(&s, &["error"]).unwrap_or("上游未说明原因")))),
            _ => Ok(PollState::Pending { progress: None }),
        }
    }

    async fn cancel(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<(), MediaError> {
        let Some(url) = task.cancel_url.as_deref() else { return Ok(()) };
        let rb = cx.request(Method::PUT, &cx.trusted_url(url)?)?;
        cx.http.send(rb).await.map(|_| ())
    }
}

// ---------------------------------------------------------------------------------------------
// Runway 图像
// ---------------------------------------------------------------------------------------------

pub struct RunwayImage;

const RUNWAY_IMAGE_RATIOS: &[&str] = &[
    "1920:1080", "1080:1920", "1024:1024", "1360:768", "1080:1080", "1168:880", "1440:1080", "1080:1440", "1808:768", "2112:912", "1280:720",
    "720:1280", "720:720", "960:720", "720:960", "1680:720",
];

#[async_trait]
impl MediaAdapter for RunwayImage {
    fn format(&self) -> &'static str {
        "runway-image"
    }

    fn capability(&self) -> Capability {
        Capability::Image
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        safe_path_segment(model, "模型 ID")?;
        let prompt = req.require_str("prompt", "提示词")?;
        if req.str_param("mode") == Some("edit") {
            return Err(MediaError::Unsupported("该供应商暂不支持图片编辑".into()));
        }
        let ratio = match opt_size(cx, req)? {
            Some((w, h, _)) => closest_ratio(w, h, RUNWAY_IMAGE_RATIOS).unwrap_or("1024:1024"),
            None => "1024:1024",
        };
        let mut body = runway::ratio_body(model, Some(prompt), ratio);
        if let Some(seed) = opt_u64(cx, req, "seed") {
            body["seed"] = json!(seed.min(4_294_967_295));
        }
        runway::create_task(cx, "runway-image", "text_to_image", &body).await
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        runway::poll_task(cx, task, "png").await
    }

    async fn cancel(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<(), MediaError> {
        runway::cancel_task(cx, task).await
    }
}

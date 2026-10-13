//! 视频生成适配器（长任务：提交 → 轮询 → 下载）：`xai-video`、`openrouter-video`、`runway-video`。

use async_trait::async_trait;
use serde_json::{json, Value};

use super::adapter::util::{parse_ratio, safe_path_segment, str_at};
use super::adapter::{
    AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, OutputItem, PollState, RemoteTask, Submission, UploadedFile,
};
use super::registry::Capability;
use super::runway;

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![Box::new(XaiVideo), Box::new(OpenRouterVideo), Box::new(RunwayVideo)]
}

struct Common {
    duration: Option<u64>,
    aspect: Option<String>,
    resolution: Option<String>,
}

fn common(cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Common, MediaError> {
    let duration = if cx.cfg.has_param("duration") { req.u64_param("duration") } else { None };
    if duration.is_some_and(|d| d == 0 || d > 60) {
        return Err(MediaError::InvalidParams("时长需在 1–60 秒之间".into()));
    }
    let aspect = if cx.cfg.has_param("aspectRatio") { req.str_param("aspectRatio").map(str::to_string) } else { None };
    if aspect.as_deref().is_some_and(|a| parse_ratio(a).is_none()) {
        return Err(MediaError::InvalidParams("画面比例格式应为 宽:高，例如 16:9".into()));
    }
    let resolution = if cx.cfg.has_param("resolution") { req.str_param("resolution").map(str::to_string) } else { None };
    if resolution.as_deref().is_some_and(|r| !(r.ends_with('p') && r.len() <= 6 && r[..r.len() - 1].chars().all(|c| c.is_ascii_digit()))) {
        return Err(MediaError::InvalidParams("分辨率格式应为 720p 这类".into()));
    }
    Ok(Common { duration, aspect, resolution })
}

/// 文生视频需要提示词；图生视频提示词可选但必须有首帧图片（返回该图片）。
fn prompt_and_image<'a>(cx: &AdapterCtx<'_>, req: &'a MediaRequest) -> Result<(Option<&'a str>, Option<&'a UploadedFile>), MediaError> {
    let prompt = req.str_param("prompt");
    if req.str_param("mode") == Some("image2video") {
        if !cx.cfg.has_feature("video-image2video") {
            return Err(MediaError::Unsupported("该供应商不支持图生视频".into()));
        }
        let image = req.file("image").ok_or_else(|| MediaError::InvalidParams("图生视频需要提供首帧图片".into()))?;
        Ok((prompt, Some(image)))
    } else if prompt.is_none() {
        Err(MediaError::InvalidParams("缺少提示词".into()))
    } else {
        Ok((prompt, None))
    }
}

fn video_output(url: &str, authed: bool) -> Outcome {
    Outcome::files(vec![OutputItem::Remote { url: url.to_string(), ext: "mp4".into(), authed }])
}

fn error_text(v: &Value) -> String {
    if let Some(s) = v.get("error").and_then(Value::as_str) {
        return s.to_string();
    }
    str_at(v, &["error", "message"]).or_else(|| str_at(v, &["message"])).unwrap_or("上游未说明原因").to_string()
}

// ---------------------------------------------------------------------------------------------
// xAI：POST {base}/generations → { request_id }；GET {base}/{id} → { status, video:{url} }
// ---------------------------------------------------------------------------------------------

pub struct XaiVideo;

#[async_trait]
impl MediaAdapter for XaiVideo {
    fn format(&self) -> &'static str {
        "xai-video"
    }

    fn capability(&self) -> Capability {
        Capability::Video
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let (prompt, image) = prompt_and_image(cx, req)?;
        let c = common(cx, req)?;
        let mut body = json!({"model": model});
        if let Some(p) = prompt {
            body["prompt"] = json!(p);
        }
        if let Some(d) = c.duration {
            body["duration"] = json!(d);
        }
        if let Some(a) = c.aspect {
            body["aspect_ratio"] = json!(a);
        }
        if let Some(r) = c.resolution {
            body["resolution"] = json!(r);
        }
        if let Some(img) = image {
            body["image"] = json!({"url": img.data_uri().await?});
        }
        let url = format!("{}/generations", cx.cfg.base_url.trim_end_matches('/'));
        let v = cx.http.send_json(cx.post(&url)?.json(&body)).await?;
        let id = str_at(&v, &["request_id"]).ok_or_else(|| MediaError::BadResponse("缺少 request_id".into()))?;
        safe_path_segment(id, "任务 ID").map_err(|_| MediaError::BadResponse("上游返回的任务 ID 不合法".into()))?;
        Ok(Submission::Pending(RemoteTask::new("xai-video", id)))
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        safe_path_segment(&task.id, "任务 ID")?;
        let url = format!("{}/{}", cx.cfg.base_url.trim_end_matches('/'), task.id);
        let v = cx.http.send_json(cx.get(&url)?).await?;
        match str_at(&v, &["status"]).unwrap_or("").to_ascii_lowercase().as_str() {
            "done" | "completed" => {
                let u = str_at(&v, &["video", "url"]).ok_or_else(|| MediaError::BadResponse("任务完成但缺少 video.url".into()))?;
                Ok(PollState::Done(video_output(u, false)))
            }
            s @ ("failed" | "error" | "expired" | "cancelled") => Err(MediaError::Failed(format!("xAI 视频任务{}：{}", status_cn(s), error_text(&v)))),
            _ => Ok(PollState::Pending { progress: v.get("progress").and_then(Value::as_u64).map(|p| p.min(100) as u8) }),
        }
    }
}

fn status_cn(s: &str) -> &'static str {
    match s {
        "expired" => "已过期",
        "cancelled" => "已取消",
        _ => "失败",
    }
}

// ---------------------------------------------------------------------------------------------
// OpenRouter：POST {base} → { id, polling_url }；GET polling_url → { status, unsigned_urls }
// ---------------------------------------------------------------------------------------------

pub struct OpenRouterVideo;

#[async_trait]
impl MediaAdapter for OpenRouterVideo {
    fn format(&self) -> &'static str {
        "openrouter-video"
    }

    fn capability(&self) -> Capability {
        Capability::Video
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let (prompt, image) = prompt_and_image(cx, req)?;
        let c = common(cx, req)?;
        let mut body = json!({"model": model});
        if let Some(p) = prompt {
            body["prompt"] = json!(p);
        }
        if let Some(d) = c.duration {
            body["duration"] = json!(d);
        }
        if let Some(a) = c.aspect {
            body["aspect_ratio"] = json!(a);
        }
        if let Some(r) = c.resolution {
            body["resolution"] = json!(r);
        }
        if let Some(img) = image {
            let uri = img.data_uri().await?;
            body["frame_images"] = json!([{"type": "image_url", "image_url": {"url": uri}, "frame_type": "first_frame"}]);
        }
        let v = cx.http.send_json(cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let id = str_at(&v, &["id"]).ok_or_else(|| MediaError::BadResponse("缺少任务 id".into()))?;
        safe_path_segment(id, "任务 ID").map_err(|_| MediaError::BadResponse("上游返回的任务 ID 不合法".into()))?;
        let mut task = RemoteTask::new("openrouter-video", id);
        if let Some(p) = str_at(&v, &["polling_url"]) {
            task.poll_url = Some(cx.trusted_url(p)?);
        }
        Ok(Submission::Pending(task))
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        let url = match &task.poll_url {
            Some(u) => cx.trusted_url(u)?,
            None => {
                safe_path_segment(&task.id, "任务 ID")?;
                format!("{}/{}", cx.cfg.base_url.trim_end_matches('/'), task.id)
            }
        };
        let v = cx.http.send_json(cx.get(&url)?).await?;
        match str_at(&v, &["status"]).unwrap_or("").to_ascii_lowercase().as_str() {
            "completed" => {
                let u = v
                    .get("unsigned_urls")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(Value::as_str)
                    .ok_or_else(|| MediaError::BadResponse("任务完成但缺少 unsigned_urls".into()))?;
                // 该地址需要凭据；下载层只会在同一受信域名下才附带
                Ok(PollState::Done(video_output(u, true)))
            }
            s @ ("failed" | "cancelled" | "expired") => Err(MediaError::Failed(format!("OpenRouter 视频任务{}：{}", status_cn(s), error_text(&v)))),
            _ => Ok(PollState::Pending { progress: None }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Runway：image_to_video
// ---------------------------------------------------------------------------------------------

pub struct RunwayVideo;

fn runway_ratio(aspect: Option<&str>) -> &'static str {
    match aspect {
        Some("9:16") => "720:1280",
        Some("1:1") => "960:960",
        Some("4:3") => "1104:832",
        _ => "1280:720",
    }
}

#[async_trait]
impl MediaAdapter for RunwayVideo {
    fn format(&self) -> &'static str {
        "runway-video"
    }

    fn capability(&self) -> Capability {
        Capability::Video
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        safe_path_segment(model, "模型 ID")?;
        if req.str_param("mode") != Some("image2video") {
            return Err(MediaError::Unsupported("Runway 目前只支持图生视频，请在「图生视频」中提供首帧图片".into()));
        }
        let (prompt, image) = prompt_and_image(cx, req)?;
        let image = image.ok_or_else(|| MediaError::InvalidParams("图生视频需要提供首帧图片".into()))?;
        let c = common(cx, req)?;
        let mut body = runway::ratio_body(model, prompt, runway_ratio(c.aspect.as_deref()));
        body["promptImage"] = json!(image.data_uri().await?);
        if let Some(d) = c.duration {
            if !(2..=10).contains(&d) {
                return Err(MediaError::InvalidParams("Runway 视频时长需在 2–10 秒之间".into()));
            }
            body["duration"] = json!(d);
        }
        runway::create_task(cx, "runway-video", "image_to_video", &body).await
    }

    async fn poll(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<PollState, MediaError> {
        runway::poll_task(cx, task, "mp4").await
    }

    async fn cancel(&self, cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<(), MediaError> {
        runway::cancel_task(cx, task).await
    }
}

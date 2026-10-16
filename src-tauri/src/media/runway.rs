//! Runway 的任务接口（图像 `text_to_image`、视频 `image_to_video` 共用同一套「提交 → /tasks/{id} 轮询 → 取消」）。

use reqwest::Method;
use serde_json::{json, Value};

use super::adapter::util::{safe_path_segment, str_at};
use super::adapter::{AdapterCtx, MediaError, Outcome, OutputItem, PollState, RemoteTask, Submission};

fn base(cx: &AdapterCtx<'_>) -> String {
    cx.cfg.base_url.trim_end_matches('/').to_string()
}

fn task_url(cx: &AdapterCtx<'_>, id: &str) -> Result<String, MediaError> {
    safe_path_segment(id, "任务 ID")?;
    Ok(format!("{}/tasks/{id}", base(cx)))
}

/// `POST {base}/{path}` 创建任务，返回待轮询的句柄。
pub(super) async fn create_task(cx: &AdapterCtx<'_>, format: &str, path: &str, body: &Value) -> Result<Submission, MediaError> {
    let rb = cx.post(&format!("{}/{path}", base(cx)))?.json(body);
    let v = cx.http.send_json(cx.provider_id, rb).await?;
    let id = v.get("id").and_then(Value::as_str).ok_or_else(|| MediaError::BadResponse("Runway 未返回任务 id".into()))?;
    safe_path_segment(id, "任务 ID").map_err(|_| MediaError::BadResponse("Runway 返回的任务 id 不合法".into()))?;
    Ok(Submission::Pending(RemoteTask::new(format, id)))
}

pub(super) async fn poll_task(cx: &AdapterCtx<'_>, task: &RemoteTask, ext: &str) -> Result<PollState, MediaError> {
    let v = cx.http.send_json(cx.provider_id, cx.get(&task_url(cx, &task.id)?)?).await?;
    match str_at(&v, &["status"]).unwrap_or("") {
        "SUCCEEDED" => {
            let outputs: Vec<OutputItem> = v
                .get("output")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(|u| OutputItem::Remote { url: u.to_string(), ext: ext.to_string(), authed: false }).collect())
                .unwrap_or_default();
            if outputs.is_empty() {
                Err(MediaError::BadResponse("Runway 任务成功但没有返回产物".into()))
            } else {
                Ok(PollState::Done(Outcome::files(outputs)))
            }
        }
        "FAILED" => {
            let why = str_at(&v, &["failure"]).unwrap_or("上游未说明原因");
            let code = str_at(&v, &["failureCode"]).map(|c| format!("（{c}）")).unwrap_or_default();
            Err(MediaError::Failed(format!("Runway 任务失败：{why}{code}")))
        }
        "CANCELLED" => Err(MediaError::Failed("Runway 任务已在上游被取消".into())),
        _ => {
            let progress = v.get("progress").and_then(Value::as_f64).map(|p| (p.clamp(0.0, 1.0) * 100.0) as u8);
            Ok(PollState::Pending { progress })
        }
    }
}

pub(super) async fn cancel_task(cx: &AdapterCtx<'_>, task: &RemoteTask) -> Result<(), MediaError> {
    let rb = cx.request(Method::DELETE, &task_url(cx, &task.id)?)?;
    cx.http.send(cx.provider_id, rb).await.map(|_| ())
}

pub(super) fn ratio_body(model: &str, prompt: Option<&str>, ratio: &str) -> Value {
    let mut b = json!({"model": model, "ratio": ratio});
    if let Some(p) = prompt {
        b["promptText"] = json!(p);
    }
    b
}

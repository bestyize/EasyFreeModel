//! Gemini CLI / Antigravity 的 Code Assist OAuth 协议。它和公开的 Gemini API 使用相同的
//! `request` / `response` 内容，但额外要求 project、模型及 v1internal 信封。

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};
use url::Url;

use super::sse_decode::{FrameSplitter, RawFrame};
use super::{client, Adapter, AdapterKind, Capabilities, StreamFraming, StreamOpen, UpstreamClient, UpstreamError, UpstreamReply, UpstreamRequest, UpstreamStream, UpstreamTarget, MAX_ERROR_BODY_BYTES};

pub struct CodeAssistAdapter;

fn method_url(base: &Url, method: &str, streaming: bool) -> Url {
    let mut u = base.clone();
    u.set_path(&format!("{}:{method}", base.path().trim_end_matches('/')));
    if streaming { u.set_query(Some("alt=sse")); }
    u
}

fn unwrap_response(v: Value) -> Result<Value, UpstreamError> {
    if v.get("error").is_some() { return Ok(v); }
    v.get("response").filter(|r| r.is_object()).cloned()
        .ok_or_else(|| UpstreamError::Malformed("Code Assist did not return a response".into()))
}

fn unwrap_frame(frame: RawFrame) -> Result<Bytes, UpstreamError> {
    if frame.is_empty() { return Ok(frame.raw); }
    let v: Value = serde_json::from_str(&frame.data)
        .map_err(|_| UpstreamError::Malformed("Code Assist sent invalid stream JSON".into()))?;
    let body = unwrap_response(v)?;
    Ok(Bytes::from(format!("data: {body}\n\n")))
}

impl CodeAssistAdapter {
    async fn project(&self, http: &UpstreamClient, target: &UpstreamTarget) -> Result<String, UpstreamError> {
        let mut headers = super::build_headers(self, target, None, false)?;
        headers.insert(CONTENT_TYPE, reqwest::header::HeaderValue::from_static("application/json"));
        let body = if target.provider_id == "antigravity" {
            json!({"metadata":{"ideType":9,"platform":2,"pluginType":2},"mode":1})
        } else {
            json!({"metadata":{"ideType":"IDE_UNSPECIFIED","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}})
        };
        let resp = http.post_json(&target.provider_id, method_url(&target.endpoint.base_url, "loadCodeAssist", false),
            headers, Bytes::from(body.to_string()), Some(target.timeout), target.timeout).await?;
        let status = resp.status();
        let bytes = client::read_body(resp, MAX_ERROR_BODY_BYTES, true).await?;
        if !status.is_success() {
            return Err(UpstreamError::Network(format!("Code Assist project lookup returned HTTP {}", status.as_u16())));
        }
        let v: Value = serde_json::from_slice(&bytes)
            .map_err(|_| UpstreamError::Malformed("Code Assist project lookup returned invalid JSON".into()))?;
        v.get("cloudaicompanionProject").and_then(Value::as_str).filter(|s| !s.is_empty())
            .map(str::to_string).ok_or_else(|| UpstreamError::Config("Code Assist did not provide a project; finish OAuth onboarding or configure a Google Cloud project".into()))
    }

    async fn wrapped(&self, http: &UpstreamClient, target: &UpstreamTarget, mut req: UpstreamRequest) -> Result<UpstreamRequest, UpstreamError> {
        let project = self.project(http, target).await?;
        let antigravity = target.provider_id == "antigravity";
        req.body = json!({
            "project": project,
            "model": req.model.clone(),
            "request": req.body,
            "requestType": "agent",
            "userAgent": if antigravity { "antigravity" } else { "gemini-cli" },
            "requestId": format!("agent-{}", uuid::Uuid::new_v4()),
        });
        Ok(req)
    }
}

#[async_trait::async_trait]
impl Adapter for CodeAssistAdapter {
    fn kind(&self) -> AdapterKind { AdapterKind::CodeAssist }
    fn capabilities(&self) -> Capabilities {
        Capabilities { streaming: true, tools: true, vision: true, reasoning: true, list_models: true }
    }
    fn endpoint_url(&self, target: &UpstreamTarget) -> Url {
        method_url(&target.endpoint.base_url, "generateContent", false)
    }
    fn request_url(&self, target: &UpstreamTarget, req: &UpstreamRequest) -> Url {
        method_url(&target.endpoint.base_url, if req.stream { "streamGenerateContent" } else { "generateContent" }, req.stream)
    }
    fn models_url(&self, target: &UpstreamTarget) -> Url {
        method_url(&target.endpoint.base_url, "fetchAvailableModels", false)
    }
    async fn list_models(&self, http: &UpstreamClient, target: &UpstreamTarget) -> Result<Vec<String>, UpstreamError> {
        let project = self.project(http, target).await?;
        let headers = super::build_headers(self, target, None, false)?;
        let resp = http.post_json(&target.provider_id, self.models_url(target), headers,
            Bytes::from(json!({"project":project}).to_string()), Some(target.timeout), target.timeout).await?;
        let status = resp.status();
        let bytes = client::read_body(resp, MAX_ERROR_BODY_BYTES * 4, true).await?;
        if !status.is_success() {
            return Err(UpstreamError::Network(format!("Code Assist model lookup returned HTTP {}", status.as_u16())));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| UpstreamError::Malformed("Code Assist model lookup returned invalid JSON".into()))?;
        Ok(value.get("models").and_then(Value::as_object)
            .map(|models| models.keys().cloned().collect()).unwrap_or_default())
    }
    async fn invoke(&self, http: &UpstreamClient, target: &UpstreamTarget, req: UpstreamRequest) -> Result<UpstreamReply, UpstreamError> {
        let req = self.wrapped(http, target, req).await?;
        let mut reply = super::reply_from(super::send(self, http, target, req, false).await?).await?;
        if reply.is_success() {
            let v: Value = serde_json::from_slice(&reply.body)
                .map_err(|_| UpstreamError::Malformed("Code Assist returned invalid JSON".into()))?;
            if let Some(error) = v.get("error").filter(|e| e.is_object()) {
                reply.status = error.get("code").and_then(Value::as_u64)
                    .filter(|c| (400..600).contains(c)).unwrap_or(502) as u16;
                return Ok(reply);
            }
            reply.body = Bytes::from(unwrap_response(v)?.to_string());
        }
        Ok(reply)
    }
    async fn stream(&self, http: &UpstreamClient, target: &UpstreamTarget, req: UpstreamRequest) -> Result<StreamOpen, UpstreamError> {
        let req = self.wrapped(http, target, req).await?;
        let resp = super::send(self, http, target, req, true).await?;
        if !resp.status().is_success() {
            return Ok(StreamOpen::Rejected(super::reply_from(resp).await?));
        }
        let status = resp.status().as_u16();
        let source = resp.bytes_stream().map(|r| r.map_err(|e| client::classify_error(&e))).boxed();
        let body = stream::try_unfold((source, FrameSplitter::new(StreamFraming::Sse), std::collections::VecDeque::new(), false),
            |(mut source, mut splitter, mut pending, mut eof)| async move {
                loop {
                    if let Some(frame) = pending.pop_front() {
                        return Ok(Some((unwrap_frame(frame)?, (source, splitter, pending, eof))));
                    }
                    if eof { return Ok(None); }
                    match source.next().await {
                        Some(Ok(chunk)) => pending.extend(splitter.push(&chunk).map_err(|e| UpstreamError::Malformed(e.to_string()))?),
                        Some(Err(e)) => return Err(e),
                        None => { pending.extend(splitter.finish()); eof = true; }
                    }
                }
            }).boxed();
        Ok(StreamOpen::Stream(UpstreamStream { status, body }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_assist_uses_internal_paths_and_unwraps_response() {
        let base = Url::parse("https://cloudcode-pa.googleapis.com/v1internal").unwrap();
        assert_eq!(method_url(&base, "streamGenerateContent", true).as_str(),
            "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse");
        assert_eq!(unwrap_response(json!({"response":{"candidates":[]}})).unwrap(), json!({"candidates":[]}));
        assert!(unwrap_response(json!({"candidates":[]})).is_err());
        let mut s = FrameSplitter::new(StreamFraming::Sse);
        let frame = s.push(b"data: {\"response\":{\"candidates\":[]}}\n\n").unwrap().remove(0);
        assert_eq!(unwrap_frame(frame).unwrap(), Bytes::from("data: {\"candidates\":[]}\n\n"));
    }
}

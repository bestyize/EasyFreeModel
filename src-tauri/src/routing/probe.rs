//! 模型级可用性探测：对某个已保存的模型发起一次最小真实请求，如实报告结果。
//!
//! 复用生产转发路径（[`Runtime::dispatch`]）：真实适配器、真实凭据（含 OAuth 刷新）、
//! 并发限制与失败回退。因此与真实请求同等地影响账号健康（401/403 会标记连接为 error、
//! 429/5xx 会触发冷却）——这正是"测出问题就反映出来"的预期行为。与真实请求的唯一区别是不写用量记录。
//!
//! 诚实原则不变：适配器未接入 / 没有连接 / 凭据缺失 / 上游拒绝，都如实返回 `ok=false` 与原因，绝不伪造成功。

use std::sync::Arc;
use std::time::Instant;

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::error::AppResult;
use crate::gateway::convert::{build_outbound, scrub};
use crate::gateway::collect::probe_first_content;
use crate::gateway::stream::{prime, PrimeOutcome};
use crate::models::TestResult;
use crate::protocols::{anthropic, gemini, ollama, openai_chat, openai_responses, ProtocolKind};
use crate::upstream::{StreamOpen, UpstreamReply, UpstreamRequest, NOT_READY_MARKER};

use super::dispatch::{Attempt, DispatchError, DispatchFailure};
use super::endpoint::endpoint_for;
use super::repo;
use super::{describe_upstream_error, Runtime};

/// 探测请求的最大输出 token：取小值以尽量少消耗额度，又要足够大以免被上游判为无效请求。
const PROBE_MAX_TOKENS: u32 = 16;

enum ProbeReply {
    Json(UpstreamReply),
    Stream(bool),
}

impl Runtime {
    /// 模型级探测：对 `provider_id` 下的 `model_id` 发起一次最小真实请求。
    ///
    /// - 适配器未接入 / 无连接 / 无凭据：如实返回原因；
    /// - 2xx：`ok=true` 且带往返延迟；
    /// - 非 2xx 或传输失败：`ok=false` 且带已擦除密钥的原因。
    pub async fn probe_model(self: &Arc<Self>, provider_id: &str, model_id: &str) -> AppResult<TestResult> {
        let fail = |msg: String| TestResult {
            connection_id: String::new(),
            provider_id: Some(provider_id.to_string()),
            ok: false,
            latency_ms: None,
            error: Some(msg),
        };

        let provider = match repo::get_provider(&self.db, provider_id).await? {
            Some(p) => p,
            None => return Ok(fail("提供商不存在，请刷新后重试".to_string())),
        };
        if !provider.adapter_ready {
            let reason = provider.pending_reason.clone().unwrap_or_else(|| "适配器尚未接入".into());
            return Ok(fail(format!("该提供商的适配器尚未接入：{reason}")));
        }
        let mut endpoint = match endpoint_for(&provider) {
            Ok(e) => e,
            Err(e) => return Ok(fail(e.0)),
        };
        let model = match repo::get_model(&self.db, provider_id, model_id).await? {
            Some(m) => m,
            None => return Ok(fail("模型不存在，可能已被删除，请刷新后重试".to_string())),
        };
        super::endpoint::apply_opencode_model_endpoint(&provider, &model.upstream_model_id, &mut endpoint);

        // 统一用 OpenAI Chat 的最小请求经规范化转换：一条代码路径即可为 6 种上游协议生成合法请求体。
        // 流式专用端点必须请求增量；读到第一个真实内容后即可关闭探测流。
        let force_stream = endpoint.force_stream;
        let body = json!({
            "model": model.upstream_model_id,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": PROBE_MAX_TOKENS,
            "stream": force_stream,
        });
        let upstream_proto = endpoint.kind.native_protocol();
        let outbound = match build_outbound(
            ProtocolKind::OpenAiChat,
            upstream_proto,
            body,
            &model.upstream_model_id,
            &HeaderMap::new(),
            force_stream,
        ) {
            Ok(o) => o,
            Err(e) => return Ok(fail(format!("无法构造探测请求：{}", e.message))),
        };
        let mut upstream_body = outbound.body;
        if force_stream {
            if let Some(obj) = upstream_body.as_object_mut() {
                obj.insert("stream".into(), Value::Bool(true));
                obj.remove("stream_options");
            }
        }
        let req = UpstreamRequest {
            body: upstream_body,
            model: model.upstream_model_id.clone(),
            stream: force_stream,
            passthrough: outbound.passthrough && !force_stream,
            extra_headers: outbound.extra_headers,
        };

        let adapter = endpoint.kind.adapter();
        let client = self.client.clone();
        let idle = self.request_timeout().await;
        let started = Instant::now();
        let outcome = self
            .dispatch(&provider, &endpoint, |target| {
                let req = req.clone();
                let client = client.clone();
                async move {
                    if force_stream {
                        return match adapter.stream(&client, &target, req).await {
                            Err(e) => Attempt::Failed(e),
                            Ok(StreamOpen::Rejected(reply)) => Attempt::Rejected(reply),
                            Ok(StreamOpen::Stream(us)) => match prime(us.body, adapter.kind(), idle).await {
                                PrimeOutcome::InBandError(reply) => Attempt::Rejected(reply),
                                PrimeOutcome::Failed(e) => Attempt::Failed(e),
                                PrimeOutcome::Ready(p) => match probe_first_content(p, adapter.kind(), idle).await {
                                    Ok(found) => Attempt::Ok(ProbeReply::Stream(found)),
                                    Err(e) => Attempt::Failed(e),
                                },
                            },
                        };
                    }
                    match adapter.invoke(&client, &target, req).await {
                        Ok(reply) if reply.is_success() => Attempt::Ok(ProbeReply::Json(reply)),
                        Ok(reply) => Attempt::Rejected(reply),
                        Err(e) => Attempt::Failed(e),
                    }
                }
            })
            .await;
        let latency_ms = started.elapsed().as_millis() as u64;

        match outcome {
            // 2xx 还不够：上游可能在 200 里塞一个错误对象或空结果（9Router 同样会检查响应体）。
            Ok(d) => {
                let validated = match &d.value {
                    ProbeReply::Json(reply) => validate_reply(reply, upstream_proto),
                    ProbeReply::Stream(true) => Ok(()),
                    ProbeReply::Stream(false) => Err("上游流未返回任何内容（该模型可能不可用）"),
                };
                match validated {
                    Ok(()) => Ok(TestResult {
                        connection_id: String::new(),
                        provider_id: Some(provider_id.to_string()),
                        ok: true,
                        latency_ms: Some(latency_ms),
                        error: None,
                    }),
                    Err(reason) => Ok(fail(reason.to_string())),
                }
            }
            Err(failure) => Ok(fail(describe_failure(&failure))),
        }
    }
}

/// 按上游协议解码 2xx 响应体，确认确实拿到了内容。
fn validate_reply(reply: &UpstreamReply, upstream: ProtocolKind) -> Result<(), &'static str> {
    let value: Value = serde_json::from_slice(&reply.body)
        .map_err(|_| "上游返回了 200，但响应体不是合法 JSON（可能被网关或代理拦截）")?;
    let canonical = match upstream {
        ProtocolKind::OpenAiChat => openai_chat::decode_response(&value),
        ProtocolKind::AnthropicMessages => anthropic::decode_response(&value),
        ProtocolKind::OpenAiResponses => openai_responses::decode_response(&value),
        ProtocolKind::Gemini | ProtocolKind::Vertex => gemini::decode_response(&value),
        ProtocolKind::Ollama => ollama::decode_response(&value),
    };
    match canonical {
        Ok(c) if !c.content.is_empty() => Ok(()),
        Ok(_) => Err("上游返回了 200，但没有任何内容（该模型可能不可用或已被下架）"),
        Err(_) => Err("上游返回了 200，但响应体不符合该协议（可能不是模型接口）"),
    }
}

/// 把调度失败翻译成面向用户的单行原因（已擦除密钥，不含请求正文）。
fn describe_failure(f: &DispatchFailure) -> String {
    match &f.error {
        DispatchError::Busy => "该提供商已达并发上限，请稍后再试".to_string(),
        DispatchError::NoConnection(n) => {
            if n.total == 0 {
                "该提供商还没有添加任何连接".to_string()
            } else if n.missing_credential > 0 {
                "连接的凭据缺失，请重新录入凭据".to_string()
            } else if n.needs_reauth > 0 {
                "授权已失效，需要重新授权".to_string()
            } else if n.inactive == n.total {
                "全部连接都已停用，请先启用".to_string()
            } else {
                "当前没有可用连接（可能都在冷却中），请稍后再试".to_string()
            }
        }
        DispatchError::Upstream { reply, secrets } => upstream_reason(reply, &secrets.0),
        DispatchError::Transport(e) => describe_upstream_error(e).1.to_string(),
        DispatchError::Local(msg) => match msg.split_once(NOT_READY_MARKER) {
            Some((_, reason)) => format!("该模型暂不可用：{}", reason.trim()),
            None => format!("本地错误：{msg}"),
        },
        DispatchError::NotImplemented(msg) => msg.clone(),
        DispatchError::TokenUnavailable(msg) => msg.clone(),
    }
}

/// 从上游错误响应里取一条人话；同时擦除我们发出的密钥。
fn upstream_reason(reply: &UpstreamReply, secrets: &[String]) -> String {
    let status = reply.status;
    let text = String::from_utf8_lossy(&reply.body);
    let message = scrub(&extract_message(&text), secrets);
    match status {
        401 | 403 => format!("上游拒绝该凭据（HTTP {status}）：{message}"),
        404 => format!("上游找不到该模型（HTTP 404）：{message}"),
        429 => format!("已触发上游频率限制（HTTP 429）：{message}"),
        400..=499 => format!("上游拒绝了该请求（HTTP {status}）：{message}"),
        _ => format!("上游返回 HTTP {status}：{message}"),
    }
}

/// 尽力从错误体里取出一条消息；取不到就回退到截断后的原始文本。
fn extract_message(text: &str) -> String {
    fn clip(s: &str) -> String {
        let s = s.trim();
        if s.chars().count() > 300 {
            let head: String = s.chars().take(300).collect();
            format!("{head}…")
        } else {
            s.to_string()
        }
    }
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if let Some(m) = v.get("error").and_then(|e| e.get("message")).and_then(Value::as_str) {
            return clip(m);
        }
        if let Some(m) = v.get("error").and_then(Value::as_str) {
            return clip(m);
        }
        if let Some(m) = v.get("message").and_then(Value::as_str) {
            return clip(m);
        }
    }
    if trimmed.is_empty() {
        "上游未返回错误详情".to_string()
    } else {
        clip(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_message_handles_common_shapes() {
        assert_eq!(extract_message(r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(extract_message(r#"{"error":"nope"}"#), "nope");
        assert_eq!(extract_message(r#"{"message":"slow down"}"#), "slow down");
        assert_eq!(extract_message("plain text"), "plain text");
        assert_eq!(extract_message("   "), "上游未返回错误详情");
        assert_eq!(extract_message("<html>502</html>"), "<html>502</html>");
    }

    #[test]
    fn extract_message_truncates_long_bodies() {
        let long = "x".repeat(1000);
        let out = extract_message(&long);
        assert!(out.chars().count() <= 301);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn upstream_reason_scrubs_secrets() {
        let reply = UpstreamReply {
            status: 401,
            retry_after: None,
            content_type: None,
            body: bytes::Bytes::from(r#"{"error":{"message":"invalid key sk-secret-123"}}"#),
        };
        let out = upstream_reason(&reply, &["sk-secret-123".to_string()]);
        assert!(!out.contains("sk-secret-123"), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
        assert!(out.contains("401"), "{out}");
    }

    #[test]
    fn upstream_reason_explains_missing_model() {
        let reply = UpstreamReply {
            status: 404,
            retry_after: None,
            content_type: None,
            body: bytes::Bytes::from(r#"{"error":{"message":"model not found"}}"#),
        };
        let out = upstream_reason(&reply, &[]);
        assert!(out.contains("找不到该模型"), "{out}");
        assert!(out.contains("model not found"), "{out}");
    }
}

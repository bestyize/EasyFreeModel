//! 文本嵌入适配器：`openai-embeddings`（OpenAI 兼容，多家共用）、`gemini-embeddings`（batchEmbedContents）。
//!
//! 嵌入不是 MediaKind（没有任务历史），由网关/内部通过 `media::embed` 直接调用。适配器把结果序列化成
//! `{"vectors":[[..]],"usage":{"promptTokens":..,"totalTokens":..}}` 放进 `Outcome.text`，`parse_result` 再还原。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::adapter::util::safe_path_segment;
use super::adapter::{AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, Submission};
use super::registry::Capability;

pub const MAX_INPUTS: usize = 2048;
pub const MAX_TOTAL_CHARS: usize = 2_000_000;

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![Box::new(OpenAiEmbeddings), Box::new(GeminiEmbeddings)]
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingResult {
    pub vectors: Vec<Vec<f32>>,
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

pub fn parse_result(text: &str) -> Result<EmbeddingResult, MediaError> {
    #[derive(Deserialize)]
    struct Wire {
        vectors: Vec<Vec<f32>>,
        #[serde(default)]
        usage: Usage,
    }
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Usage {
        prompt_tokens: Option<u64>,
        total_tokens: Option<u64>,
    }
    let w: Wire = serde_json::from_str(text).map_err(|_| MediaError::BadResponse("嵌入结果无法解析".into()))?;
    Ok(EmbeddingResult { vectors: w.vectors, prompt_tokens: w.usage.prompt_tokens, total_tokens: w.usage.total_tokens })
}

fn inputs(req: &MediaRequest) -> Result<Vec<String>, MediaError> {
    let arr = req.params.get("input").and_then(Value::as_array).ok_or_else(|| MediaError::InvalidParams("缺少 input".into()))?;
    let list: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if list.is_empty() || list.len() != arr.len() {
        return Err(MediaError::InvalidParams("input 必须是非空的字符串数组".into()));
    }
    if list.len() > MAX_INPUTS {
        return Err(MediaError::InvalidParams(format!("input 最多 {MAX_INPUTS} 条")));
    }
    if list.iter().any(|s| s.is_empty()) {
        return Err(MediaError::InvalidParams("input 不能包含空字符串".into()));
    }
    if list.iter().map(|s| s.chars().count()).sum::<usize>() > MAX_TOTAL_CHARS {
        return Err(MediaError::InvalidParams("input 总长度超出上限".into()));
    }
    Ok(list)
}

fn dimensions(cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Option<u64>, MediaError> {
    if !cx.cfg.has_param("dimensions") {
        return Ok(None);
    }
    match req.u64_param("dimensions") {
        None => Ok(None),
        Some(d) if (1..=16384).contains(&d) => Ok(Some(d)),
        Some(_) => Err(MediaError::InvalidParams("dimensions 超出范围".into())),
    }
}

fn result_outcome(vectors: Vec<Vec<f32>>, expected: usize, prompt: Option<u64>, total: Option<u64>) -> Result<Submission, MediaError> {
    if vectors.len() != expected || vectors.iter().any(Vec::is_empty) {
        return Err(MediaError::BadResponse(format!("上游返回了 {} 个向量，期望 {expected} 个", vectors.len())));
    }
    let text = json!({"vectors": vectors, "usage": {"promptTokens": prompt, "totalTokens": total}}).to_string();
    Ok(Submission::Done(Outcome::text(text)))
}

pub struct OpenAiEmbeddings;

#[async_trait]
impl MediaAdapter for OpenAiEmbeddings {
    fn format(&self) -> &'static str {
        "openai-embeddings"
    }

    fn capability(&self) -> Capability {
        Capability::Embedding
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let list = inputs(req)?;
        let mut body = json!({"model": model, "input": list, "encoding_format": "float"});
        if let Some(d) = dimensions(cx, req)? {
            body["dimensions"] = json!(d);
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        let data = v.get("data").and_then(Value::as_array).ok_or_else(|| MediaError::BadResponse("缺少 data 数组".into()))?;
        // 按 index 排序，不信任返回顺序
        let mut rows: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
        for (i, item) in data.iter().enumerate() {
            let idx = item.get("index").and_then(Value::as_u64).unwrap_or(i as u64);
            let emb = item
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| MediaError::BadResponse("向量格式不是浮点数组".into()))?
                .iter()
                .map(|x| x.as_f64().map(|f| f as f32))
                .collect::<Option<Vec<f32>>>()
                .ok_or_else(|| MediaError::BadResponse("向量格式不是浮点数组".into()))?;
            rows.push((idx, emb));
        }
        rows.sort_by_key(|r| r.0);
        let usage = v.get("usage");
        let prompt = usage.and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64);
        let total = usage.and_then(|u| u.get("total_tokens")).and_then(Value::as_u64);
        result_outcome(rows.into_iter().map(|r| r.1).collect(), list.len(), prompt, total)
    }
}

pub struct GeminiEmbeddings;

#[async_trait]
impl MediaAdapter for GeminiEmbeddings {
    fn format(&self) -> &'static str {
        "gemini-embeddings"
    }

    fn capability(&self) -> Capability {
        Capability::Embedding
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        safe_path_segment(model, "模型 ID")?;
        let list = inputs(req)?;
        let dim = dimensions(cx, req)?;
        let requests: Vec<Value> = list
            .iter()
            .map(|t| {
                let mut r = json!({"model": format!("models/{model}"), "content": {"parts": [{"text": t}]}});
                if let Some(d) = dim {
                    r["outputDimensionality"] = json!(d);
                }
                r
            })
            .collect();
        let url = format!("{}/{model}:batchEmbedContents", cx.cfg.base_url.trim_end_matches('/'));
        let v = cx.http.send_json(cx.provider_id, cx.post(&url)?.json(&json!({"requests": requests}))).await?;
        let vectors = v
            .get("embeddings")
            .and_then(Value::as_array)
            .ok_or_else(|| MediaError::BadResponse("缺少 embeddings 数组".into()))?
            .iter()
            .map(|e| {
                e.get("values")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().map(|x| x.as_f64().map(|f| f as f32)).collect::<Option<Vec<f32>>>())
                    .ok_or_else(|| MediaError::BadResponse("向量格式不是浮点数组".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        result_outcome(vectors, list.len(), None, None)
    }
}

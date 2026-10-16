//! 文字转语音适配器：`openai-speech`、`elevenlabs-tts`、`cartesia-tts`。
//!
//! 三者都是「POST JSON → 直接返回音频字节」。产物扩展名取自响应的真实类型/文件头，而不是请求的格式。

use async_trait::async_trait;
use reqwest::Response;
use serde_json::{json, Value};

use super::adapter::util::safe_path_segment;
use super::adapter::{AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, OutputItem, Submission};
use super::files::{ext_from_mime, sniff_ext};
use super::http::read_limited;
use super::registry::Capability;

pub const MAX_AUDIO_BYTES: u64 = 100 * 1024 * 1024;

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![Box::new(OpenAiSpeech), Box::new(ElevenLabsTts), Box::new(CartesiaTts)]
}

fn check_text(text: &str, max_chars: usize) -> Result<(), MediaError> {
    let n = text.chars().count();
    if n > max_chars {
        return Err(MediaError::InvalidParams(format!("文本过长（{n} 字符），该供应商上限 {max_chars} 字符")));
    }
    Ok(())
}

fn check_speed(cx: &AdapterCtx<'_>, req: &MediaRequest, min: f64, max: f64) -> Result<Option<f64>, MediaError> {
    if !cx.cfg.has_param("speed") {
        return Ok(None);
    }
    match req.f64_param("speed") {
        None => Ok(None),
        Some(s) if (min..=max).contains(&s) => Ok(Some(s)),
        Some(_) => Err(MediaError::InvalidParams(format!("语速需在 {min}–{max} 之间（该供应商的限制）"))),
    }
}

/// 读取音频响应：拒绝把 JSON/HTML 错误页当成音频保存。
async fn audio_outcome(resp: Response, fallback_ext: &str) -> Result<Outcome, MediaError> {
    let ctype = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if ctype.contains("json") || ctype.contains("text/") {
        return Err(MediaError::BadResponse("上游返回的不是音频数据".into()));
    }
    let data = read_limited(resp, MAX_AUDIO_BYTES).await?;
    if data.len() < 4 {
        return Err(MediaError::BadResponse("上游返回了空音频".into()));
    }
    let ext = sniff_ext(&data).or_else(|| ext_from_mime(&ctype)).unwrap_or(match fallback_ext {
        "wav" => "wav",
        "opus" => "opus",
        "flac" => "flac",
        "aac" => "aac",
        _ => "mp3",
    });
    Ok(Outcome::files(vec![OutputItem::Bytes { data, ext: ext.to_string() }]))
}

pub struct OpenAiSpeech;

#[async_trait]
impl MediaAdapter for OpenAiSpeech {
    fn format(&self) -> &'static str {
        "openai-speech"
    }

    fn capability(&self) -> Capability {
        Capability::Tts
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let text = req.require_str("text", "文本")?;
        check_text(text, 4096)?;
        let voice = req.require_str("voice", "音色")?;
        let mut body = json!({"model": model, "input": text, "voice": voice});
        let mut fmt = "mp3";
        if cx.cfg.has_param("format") {
            if let Some(f) = req.str_param("format") {
                if !["mp3", "wav", "opus", "flac", "aac"].contains(&f) {
                    return Err(MediaError::InvalidParams(format!("不支持的音频格式：{f}")));
                }
                fmt = f;
                body["response_format"] = Value::String(f.to_string());
            }
        }
        if let Some(s) = check_speed(cx, req, 0.25, 4.0)? {
            body["speed"] = json!(s);
        }
        let resp = cx.http.send(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        Ok(Submission::Done(audio_outcome(resp, fmt).await?))
    }
}

pub struct ElevenLabsTts;

#[async_trait]
impl MediaAdapter for ElevenLabsTts {
    fn format(&self) -> &'static str {
        "elevenlabs-tts"
    }

    fn capability(&self) -> Capability {
        Capability::Tts
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let text = req.require_str("text", "文本")?;
        check_text(text, 5000)?;
        // 音色 ID 会拼进 URL 路径，只允许安全字符
        let voice = req.require_str("voice", "音色")?;
        safe_path_segment(voice, "音色 ID")?;
        let mut body = json!({"text": text, "model_id": model});
        if let Some(s) = check_speed(cx, req, 0.7, 1.2)? {
            if (s - 1.0).abs() > 1e-9 {
                body["voice_settings"] = json!({"speed": s});
            }
        }
        let url = format!("{}/{voice}?output_format=mp3_44100_128", cx.cfg.base_url.trim_end_matches('/'));
        let resp = cx.http.send(cx.provider_id, cx.post(&url)?.json(&body)).await?;
        Ok(Submission::Done(audio_outcome(resp, "mp3").await?))
    }
}

pub struct CartesiaTts;

#[async_trait]
impl MediaAdapter for CartesiaTts {
    fn format(&self) -> &'static str {
        "cartesia-tts"
    }

    fn capability(&self) -> Capability {
        Capability::Tts
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let text = req.require_str("text", "文本")?;
        check_text(text, 5000)?;
        let voice = req.require_str("voice", "音色")?;
        safe_path_segment(voice, "音色 ID")?;
        let mut body = json!({
            "model_id": model,
            "transcript": text,
            "voice": {"mode": "id", "id": voice},
            "output_format": {"container": "mp3", "bit_rate": 128000, "sample_rate": 44100},
        });
        if cx.cfg.has_param("language") {
            if let Some(l) = req.str_param("language") {
                body["language"] = Value::String(l.to_string());
            }
        }
        let resp = cx.http.send(cx.provider_id, cx.post(&cx.cfg.base_url)?.json(&body)).await?;
        Ok(Submission::Done(audio_outcome(resp, "mp3").await?))
    }
}

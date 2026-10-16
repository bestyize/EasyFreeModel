//! 语音转文字适配器：`openai-transcriptions`（OpenAI / Groq 的 multipart 接口）、
//! `deepgram-listen`（原始音频字节 + 查询参数）、`elevenlabs-stt`（Scribe，multipart）。
//!
//! 转写文本放进 `Outcome.text`（最终写入 MediaJob.summary）。上传的音频内容不进日志。

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use reqwest::Url;
use serde_json::Value;

use super::adapter::util::str_at;
use super::adapter::{AdapterCtx, MediaAdapter, MediaError, MediaRequest, Outcome, Submission, UploadedFile};
use super::registry::Capability;

pub const NO_SPEECH: &str = "（未识别到语音内容）";

pub fn adapters() -> Vec<Box<dyn MediaAdapter>> {
    vec![Box::new(OpenAiTranscriptions), Box::new(DeepgramListen), Box::new(ElevenLabsStt)]
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: Option<f64>,
    pub speaker: Option<String>,
    pub text: String,
}

fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0);
    let m = (total / 60.0).floor() as u64;
    let s = total - (m as f64) * 60.0;
    format!("{m:02}:{s:04.1}")
}

/// 按用户勾选的选项渲染转写文本；上游没有给出分段/说话人时回落到纯文本，不编造时间或说话人。
pub fn render_transcript(plain: &str, segments: &[Segment], timestamps: bool, diarization: bool) -> String {
    let usable = segments.iter().any(|s| (timestamps && s.start.is_some()) || (diarization && s.speaker.is_some()));
    if !usable {
        return plain.trim().to_string();
    }
    segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| {
            let mut line = String::new();
            if timestamps {
                if let Some(t) = s.start {
                    line.push_str(&format!("[{}] ", fmt_time(t)));
                }
            }
            if diarization {
                if let Some(sp) = &s.speaker {
                    line.push_str(&format!("说话人 {sp}：", sp = sp));
                }
            }
            line.push_str(s.text.trim());
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn finish(text: String) -> Submission {
    let t = text.trim().to_string();
    Submission::Done(Outcome::text(if t.is_empty() { NO_SPEECH.to_string() } else { t }))
}

fn audio_file(req: &MediaRequest) -> Result<&UploadedFile, MediaError> {
    req.file("audio").ok_or_else(|| MediaError::InvalidParams("请先选择或录制音频文件".into()))
}

async fn file_part(f: &UploadedFile) -> Result<Part, MediaError> {
    Part::bytes(f.read().await?)
        .file_name(f.upload_name("audio"))
        .mime_str(&f.mime)
        .map_err(|_| MediaError::InvalidParams("音频类型无效".into()))
}

fn language<'a>(cx: &AdapterCtx<'_>, req: &'a MediaRequest) -> Option<&'a str> {
    if cx.cfg.has_param("language") {
        req.str_param("language").filter(|l| l.len() <= 16 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    } else {
        None
    }
}

/// 用户勾选且该供应商声明支持的特性才生效。
fn wants(cx: &AdapterCtx<'_>, req: &MediaRequest, param: &str, feature: &str) -> bool {
    req.bool_param(param) && cx.cfg.has_feature(feature)
}

// ---------------------------------------------------------------------------------------------

pub struct OpenAiTranscriptions;

#[async_trait]
impl MediaAdapter for OpenAiTranscriptions {
    fn format(&self) -> &'static str {
        "openai-transcriptions"
    }

    fn capability(&self) -> Capability {
        Capability::Stt
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let file = audio_file(req)?;
        let timestamps = wants(cx, req, "timestamps", "asr-timestamps");
        let mut form = Form::new().part("file", file_part(file).await?).text("model", model.to_string());
        if let Some(l) = language(cx, req) {
            form = form.text("language", l.to_string());
        }
        if timestamps {
            form = form.text("response_format", "verbose_json").text("timestamp_granularities[]", "segment");
        } else {
            form = form.text("response_format", "json");
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.multipart(form)).await?;
        let plain = str_at(&v, &["text"]).ok_or_else(|| MediaError::BadResponse("缺少 text 字段".into()))?;
        let segments: Vec<Segment> = v
            .get("segments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| Segment {
                        start: s.get("start").and_then(Value::as_f64),
                        speaker: None,
                        text: s.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(finish(render_transcript(plain, &segments, timestamps, false)))
    }
}

// ---------------------------------------------------------------------------------------------

pub struct DeepgramListen;

#[async_trait]
impl MediaAdapter for DeepgramListen {
    fn format(&self) -> &'static str {
        "deepgram-listen"
    }

    fn capability(&self) -> Capability {
        Capability::Stt
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let file = audio_file(req)?;
        let timestamps = wants(cx, req, "timestamps", "asr-timestamps");
        let diarization = wants(cx, req, "diarization", "asr-diarization");

        let mut url = Url::parse(&cx.cfg.base_url).map_err(|_| MediaError::InvalidParams("供应商 baseUrl 无效".into()))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("model", model).append_pair("smart_format", "true").append_pair("punctuate", "true");
            match language(cx, req) {
                Some(l) => q.append_pair("language", l),
                None => q.append_pair("detect_language", "true"),
            };
            if diarization {
                q.append_pair("diarize", "true");
            }
            if timestamps || diarization {
                q.append_pair("utterances", "true");
            }
        }
        let rb = cx.post(url.as_str())?.header(reqwest::header::CONTENT_TYPE, file.mime.as_str()).body(file.read().await?);
        let v = cx.http.send_json(cx.provider_id, rb).await?;
        let plain = v
            .pointer("/results/channels/0/alternatives/0/transcript")
            .and_then(Value::as_str)
            .ok_or_else(|| MediaError::BadResponse("缺少 transcript 字段".into()))?;
        let segments: Vec<Segment> = v
            .pointer("/results/utterances")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|u| Segment {
                        start: u.get("start").and_then(Value::as_f64),
                        speaker: u.get("speaker").and_then(Value::as_i64).map(|s| s.to_string()),
                        text: u.get("transcript").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(finish(render_transcript(plain, &segments, timestamps, diarization)))
    }
}

// ---------------------------------------------------------------------------------------------

pub struct ElevenLabsStt;

/// 把逐词结果按说话人/句末标点聚合成分段。
fn group_words(words: &[Value], by_speaker: bool) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut cur: Option<(Segment, usize)> = None; // (分段, 词数)
    let flush = |cur: &mut Option<(Segment, usize)>, out: &mut Vec<Segment>| {
        if let Some((seg, _)) = cur.take() {
            out.push(seg);
        }
    };
    for w in words {
        let kind = w.get("type").and_then(Value::as_str).unwrap_or("word");
        let text = w.get("text").and_then(Value::as_str).unwrap_or("");
        if kind == "audio_event" {
            continue;
        }
        let speaker = w.get("speaker_id").and_then(Value::as_str).map(|s| s.trim_start_matches("speaker_").to_string());
        if by_speaker && cur.as_ref().is_some_and(|(s, _)| s.speaker != speaker) {
            flush(&mut cur, &mut out);
        }
        let entry = cur.get_or_insert_with(|| {
            (Segment { start: w.get("start").and_then(Value::as_f64), speaker: if by_speaker { speaker.clone() } else { None }, text: String::new() }, 0)
        });
        entry.0.text.push_str(text);
        if kind == "word" {
            entry.1 += 1;
        }
        let sentence_end = kind == "word" && text.ends_with(['.', '!', '?', '。', '！', '？']);
        if !by_speaker && (sentence_end || entry.1 >= 40) {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out
}

#[async_trait]
impl MediaAdapter for ElevenLabsStt {
    fn format(&self) -> &'static str {
        "elevenlabs-stt"
    }

    fn capability(&self) -> Capability {
        Capability::Stt
    }

    async fn submit(&self, cx: &AdapterCtx<'_>, req: &MediaRequest) -> Result<Submission, MediaError> {
        let model = req.require_model()?;
        let file = audio_file(req)?;
        let timestamps = wants(cx, req, "timestamps", "asr-timestamps");
        let diarization = wants(cx, req, "diarization", "asr-diarization");
        let mut form = Form::new().part("file", file_part(file).await?).text("model_id", model.to_string());
        if let Some(l) = language(cx, req) {
            form = form.text("language_code", l.to_string());
        }
        if diarization {
            form = form.text("diarize", "true");
        }
        if timestamps {
            form = form.text("timestamps_granularity", "word");
        }
        let v = cx.http.send_json(cx.provider_id, cx.post(&cx.cfg.base_url)?.multipart(form)).await?;
        let plain = str_at(&v, &["text"]).ok_or_else(|| MediaError::BadResponse("缺少 text 字段".into()))?;
        let words = v.get("words").and_then(Value::as_array).cloned().unwrap_or_default();
        let segments = group_words(&words, diarization);
        Ok(finish(render_transcript(plain, &segments, timestamps, diarization)))
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_transcript_respects_flags_and_never_invents() {
        let segs = vec![
            Segment { start: Some(0.0), speaker: Some("0".into()), text: "你好".into() },
            Segment { start: Some(65.5), speaker: Some("1".into()), text: " 再见 ".into() },
        ];
        assert_eq!(render_transcript("你好 再见", &segs, false, false), "你好 再见");
        assert_eq!(render_transcript("x", &segs, true, false), "[00:00.0] 你好\n[01:05.5] 再见");
        assert_eq!(render_transcript("x", &segs, false, true), "说话人 0：你好\n说话人 1：再见");
        assert_eq!(render_transcript("x", &segs, true, true), "[00:00.0] 说话人 0：你好\n[01:05.5] 说话人 1：再见");
        // 上游没有分段/说话人：保持纯文本
        let bare = vec![Segment { start: None, speaker: None, text: "t".into() }];
        assert_eq!(render_transcript("  纯文本 ", &bare, true, true), "纯文本");
        assert_eq!(render_transcript("纯文本", &[], true, true), "纯文本");
    }

    #[test]
    fn group_words_by_speaker_and_sentence() {
        let words: Vec<Value> = vec![
            json!({"text": "Hello", "type": "word", "start": 0.1, "speaker_id": "speaker_0"}),
            json!({"text": " ", "type": "spacing", "speaker_id": "speaker_0"}),
            json!({"text": "world.", "type": "word", "start": 0.5, "speaker_id": "speaker_0"}),
            json!({"text": "(laughter)", "type": "audio_event", "start": 1.0}),
            json!({"text": "Hi", "type": "word", "start": 2.0, "speaker_id": "speaker_1"}),
        ];
        let by_speaker = group_words(&words, true);
        assert_eq!(by_speaker.len(), 2);
        assert_eq!(by_speaker[0].speaker.as_deref(), Some("0"));
        assert_eq!(by_speaker[0].text, "Hello world.");
        assert_eq!(by_speaker[1].speaker.as_deref(), Some("1"));
        let by_sentence = group_words(&words, false);
        assert_eq!(by_sentence.len(), 2);
        assert_eq!(by_sentence[0].text, "Hello world.");
        assert!(by_sentence[0].speaker.is_none());
    }
}

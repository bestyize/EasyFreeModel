//! 受控的媒体文件目录与上传文件处理。
//!
//! - 所有文件都在 `<应用数据目录>/media/` 下；文件名一律是我们生成的 uuid + 白名单扩展名，
//!   不信任上游文件名/URL/用户文件名（防路径穿越）；
//! - 数据库只存路径与元数据，不存 BLOB；
//! - 前端传来的文件载荷（`{name,size,mimeType,dataBase64}`，上限 25 MB）先解码落盘再使用。

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;

use super::adapter::{MediaError, UploadedFile};
use crate::models::MediaKind;

pub const MAX_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];
pub const AUDIO_EXTS: &[&str] = &["mp3", "wav", "m4a", "flac", "ogg", "webm", "mp4", "mpeg", "mpga", "opus", "aac"];
/// 允许作为输出文件扩展名的全集。
const OUTPUT_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "mp3", "wav", "m4a", "flac", "ogg", "opus", "aac", "mp4", "webm", "mov", "json", "md", "txt",
    "srt", "vtt", "pcm",
];

/// 只接受白名单内的扩展名，其余一律 `bin`。
pub fn safe_ext(candidate: &str) -> String {
    let c = candidate.trim().trim_start_matches('.').to_ascii_lowercase();
    if OUTPUT_EXTS.contains(&c.as_str()) {
        c
    } else {
        "bin".to_string()
    }
}

/// 常见 MIME → 扩展名。
pub fn ext_from_mime(mime: &str) -> Option<&'static str> {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match m.as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/aac" => "aac",
        "audio/webm" | "video/webm" => "webm",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        _ => return None,
    })
}

/// 通过文件头判断真实格式（上游返回的 Content-Type/文件名不可信）。
pub fn sniff_ext(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("webp")
    } else if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        Some("wav")
    } else if head.starts_with(b"GIF8") {
        Some("gif")
    } else if head.starts_with(b"OggS") {
        Some("ogg")
    } else if head.starts_with(b"fLaC") {
        Some("flac")
    } else if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0) {
        Some("mp3")
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        Some("mp4")
    } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        Some("webm")
    } else {
        None
    }
}

/// 把用户/上游给的文件名清洗成不含路径成分的短名字（仅用于展示/元数据，绝不用来拼路径）。
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'))
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let cleaned: String = cleaned.chars().take(80).collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

fn ext_of_name(name: &str) -> Option<String> {
    let base = sanitize_file_name(name);
    let (_, ext) = base.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric())).then_some(ext)
}

/// 文件在数据目录里的布局。
#[derive(Debug, Clone)]
pub struct MediaDirs {
    root: PathBuf,
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl MediaDirs {
    /// `app_data_dir` 下的 `media/` 子目录。
    pub fn new(app_data_dir: &Path) -> Self {
        MediaDirs { root: app_data_dir.join("media") }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn output_dir(&self, kind: MediaKind) -> PathBuf {
        self.root.join(match kind {
            MediaKind::Image => "image",
            MediaKind::Tts => "tts",
            MediaKind::Video => "video",
            MediaKind::Asr => "asr",
            MediaKind::Search => "search",
        })
    }

    /// 新输出文件路径：`media/<kind>/<uuid>.<ext>`。
    pub async fn new_output_path(&self, kind: MediaKind, ext: &str) -> Result<PathBuf, MediaError> {
        let dir = self.output_dir(kind);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(dir.join(format!("{}.{}", uuid::Uuid::new_v4(), safe_ext(ext))))
    }

    /// 每个任务一个上传目录：`media/uploads/<job_id>/`。
    pub fn upload_dir(&self, job_id: &str) -> Result<PathBuf, MediaError> {
        if !is_safe_id(job_id) {
            return Err(MediaError::Io("任务 ID 不合法".into()));
        }
        Ok(self.root.join("uploads").join(job_id))
    }

    /// 任务结束后删除上传的输入文件（隐私：不长期保留用户上传的音频/图片）。
    pub async fn remove_uploads(&self, job_id: &str) {
        if let Ok(dir) = self.upload_dir(job_id) {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
    }

    /// 清理没有对应活动任务的上传目录（崩溃/强退留下的）。
    pub async fn sweep_uploads(&self, keep_job_ids: &[String]) {
        let dir = self.root.join("uploads");
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !keep_job_ids.contains(&name) {
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
        }
    }

    /// 把前端文件载荷解码并写盘。`allowed_exts` 限定该字段允许的文件类型。
    pub async fn save_upload(&self, job_id: &str, field: &str, payload: &Value, allowed_exts: &[&str]) -> Result<UploadedFile, MediaError> {
        let obj = payload.as_object().ok_or_else(|| MediaError::InvalidParams(format!("{field} 不是有效的文件载荷")))?;
        let b64 = obj
            .get("dataBase64")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MediaError::InvalidParams("文件内容为空".into()))?;
        // 先按 base64 长度粗估，超限就不解码，避免为恶意大载荷分配内存
        if b64.len() / 4 * 3 > MAX_UPLOAD_BYTES + 3 {
            return Err(MediaError::TooLarge("25 MB".into()));
        }
        let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
        let declared_mime = obj.get("mimeType").and_then(Value::as_str).unwrap_or("");
        let data = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|_| MediaError::InvalidParams("文件内容不是有效的 base64".into()))?;
        if data.is_empty() {
            return Err(MediaError::InvalidParams("文件为空".into()));
        }
        if data.len() > MAX_UPLOAD_BYTES {
            return Err(MediaError::TooLarge("25 MB".into()));
        }
        // 扩展名：先看真实文件头（必须在该字段白名单内）；否则看文件名——名字带了不在白名单的扩展名就拒绝，
        // 没有扩展名时才退回声明的 MIME。
        let allowed = |e: &str| allowed_exts.contains(&e);
        let unsupported = || MediaError::InvalidParams(format!("不支持的文件类型，仅支持：{}", allowed_exts.join(" / ")));
        let ext = match sniff_ext(&data).filter(|e| allowed(e)) {
            Some(e) => e.to_string(),
            None => match ext_of_name(name) {
                Some(e) if allowed(&e) => e,
                Some(_) => return Err(unsupported()),
                None => ext_from_mime(declared_mime).filter(|e| allowed(e)).map(str::to_string).ok_or_else(unsupported)?,
            },
        };
        let mime = mime_for_ext(&ext).to_string();

        let dir = self.upload_dir(job_id)?;
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(format!("{}.{}", uuid::Uuid::new_v4(), ext));
        tokio::fs::write(&path, &data).await?;
        Ok(UploadedFile { path, mime, ext, size: data.len() as u64 })
    }
}

pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp3" | "mpeg" | "mpga" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "aac" => "audio/aac",
        "webm" => "audio/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// 下载/写入完成后，用真实文件头校正扩展名（上游给的类型不可信）；返回最终路径。
pub async fn finalize_ext(path: &Path) -> PathBuf {
    let Ok(mut f) = tokio::fs::File::open(path).await else { return path.to_path_buf() };
    let mut head = [0u8; 16];
    let n = tokio::io::AsyncReadExt::read(&mut f, &mut head).await.unwrap_or(0);
    drop(f);
    let Some(real) = sniff_ext(&head[..n]) else { return path.to_path_buf() };
    let current = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    // jpg/jpeg、mp3 等同族不需要改
    if current.eq_ignore_ascii_case(real) || (real == "jpg" && current.eq_ignore_ascii_case("jpeg")) {
        return path.to_path_buf();
    }
    // mp4 容器也常见于 m4a/mov，不强改
    if real == "mp4" && matches!(current, "m4a" | "mov" | "mp4") {
        return path.to_path_buf();
    }
    let new_path = path.with_extension(real);
    match tokio::fs::rename(path, &new_path).await {
        Ok(()) => new_path,
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::testutil::tempdir;
    use serde_json::json;

    fn payload(name: &str, bytes: &[u8]) -> Value {
        json!({"name": name, "size": bytes.len(), "mimeType": "", "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes)})
    }

    #[test]
    fn sanitize_strips_path_components_and_control_chars() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("..\\..\\windows\\system32\\evil.dll"), "evil.dll");
        assert_eq!(sanitize_file_name("/abs/path/a.png"), "a.png");
        assert_eq!(sanitize_file_name("a\0b\n.png"), "ab.png");
        assert_eq!(sanitize_file_name("..."), "file");
        assert_eq!(sanitize_file_name(""), "file");
        assert_eq!(sanitize_file_name("CON:*?.txt"), "CON.txt");
        assert!(sanitize_file_name(&"a".repeat(500)).len() <= 80);
    }

    #[test]
    fn safe_ext_is_a_whitelist() {
        assert_eq!(safe_ext("PNG"), "png");
        assert_eq!(safe_ext(".mp3"), "mp3");
        for bad in ["exe", "sh", "../x", "png/../../etc", "html", "", "a b"] {
            assert_eq!(safe_ext(bad), "bin", "{bad}");
        }
    }

    #[test]
    fn sniff_recognizes_common_formats() {
        assert_eq!(sniff_ext(&[0x89, b'P', b'N', b'G', 0, 0]), Some("png"));
        assert_eq!(sniff_ext(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_ext(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff_ext(b"RIFF\0\0\0\0WAVEfmt "), Some("wav"));
        assert_eq!(sniff_ext(b"ID3\x04"), Some("mp3"));
        assert_eq!(sniff_ext(b"\0\0\0\x18ftypmp42"), Some("mp4"));
        assert_eq!(sniff_ext(b"<html>"), None);
    }

    #[tokio::test]
    async fn output_paths_are_uuid_names_inside_media_dir() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        let p = dirs.new_output_path(MediaKind::Image, "../../etc/passwd").await.unwrap();
        assert!(p.starts_with(dirs.root()));
        assert_eq!(p.extension().unwrap(), "bin");
        assert!(uuid::Uuid::parse_str(p.file_stem().unwrap().to_str().unwrap()).is_ok());
        let q = dirs.new_output_path(MediaKind::Video, "MP4").await.unwrap();
        assert_eq!(q.extension().unwrap(), "mp4");
        assert_ne!(p, q);
    }

    #[tokio::test]
    async fn upload_dir_rejects_unsafe_job_ids() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        for bad in ["", "../x", "a/b", "a\\b", "..", &"x".repeat(100)] {
            assert!(dirs.upload_dir(bad).is_err(), "{bad}");
        }
        assert!(dirs.upload_dir("3f2a9c1e-0000-4000-8000-000000000001").is_ok());
    }

    #[tokio::test]
    async fn save_upload_writes_uuid_named_file_ignoring_user_name() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        let png = [0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        let f = dirs.save_upload("job-1", "image", &payload("../../../evil.sh", &png), IMAGE_EXTS).await.unwrap();
        assert!(f.path.starts_with(dirs.upload_dir("job-1").unwrap()));
        assert_eq!(f.ext, "png", "按真实文件头判断，而不是用户给的名字");
        assert_eq!(f.mime, "image/png");
        assert_eq!(std::fs::read(&f.path).unwrap(), png);
        assert!(!f.path.to_string_lossy().contains("evil"));
        assert_eq!(f.upload_name("image"), "image.png");

        dirs.remove_uploads("job-1").await;
        assert!(!f.path.exists());
    }

    #[tokio::test]
    async fn save_upload_falls_back_to_name_then_mime_and_enforces_whitelist() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        let f = dirs.save_upload("j", "audio", &payload("talk.M4A", b"not-sniffable"), AUDIO_EXTS).await.unwrap();
        assert_eq!(f.ext, "m4a");
        let by_mime = json!({"name": "noext", "mimeType": "audio/wav", "dataBase64": base64::engine::general_purpose::STANDARD.encode(b"xx")});
        assert_eq!(dirs.save_upload("j", "audio", &by_mime, AUDIO_EXTS).await.unwrap().ext, "wav");

        let err = dirs.save_upload("j", "audio", &payload("a.exe", b"MZ"), AUDIO_EXTS).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidParams(_)), "{err:?}");
        // 图片头出现在音频字段里：不在白名单内且名字不合法 → 拒绝
        let err = dirs.save_upload("j", "audio", &payload("a.txt", &[0x89, b'P', b'N', b'G']), AUDIO_EXTS).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn save_upload_enforces_25mb_limit_and_validates_payload() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        let big = vec![0u8; MAX_UPLOAD_BYTES + 1];
        let err = dirs.save_upload("j", "audio", &payload("big.mp3", &big), AUDIO_EXTS).await.unwrap_err();
        assert!(matches!(err, MediaError::TooLarge(_)), "{err:?}");
        let exactly = vec![0u8; MAX_UPLOAD_BYTES];
        assert!(dirs.save_upload("j", "audio", &payload("ok.mp3", &exactly), AUDIO_EXTS).await.is_ok());

        // 超大 base64 字符串在解码前就被拒绝
        let huge = json!({"name": "x.mp3", "dataBase64": "A".repeat(MAX_UPLOAD_BYTES * 2)});
        assert!(matches!(dirs.save_upload("j", "audio", &huge, AUDIO_EXTS).await, Err(MediaError::TooLarge(_))));

        for bad in [json!("string"), json!({"name": "a.mp3"}), json!({"name": "a.mp3", "dataBase64": ""}), json!({"name": "a.mp3", "dataBase64": "!!!not-base64!!!"})] {
            assert!(matches!(dirs.save_upload("j", "audio", &bad, AUDIO_EXTS).await, Err(MediaError::InvalidParams(_))), "{bad}");
        }
    }

    #[tokio::test]
    async fn finalize_ext_corrects_extension_from_magic_bytes() {
        let dir = tempdir();
        let p = dir.join("a.png");
        std::fs::write(&p, [0xFF, 0xD8, 0xFF, 0xE0, 0, 0]).unwrap();
        let fixed = finalize_ext(&p).await;
        assert_eq!(fixed.extension().unwrap(), "jpg");
        assert!(fixed.exists() && !p.exists());

        let q = dir.join("b.mp3");
        std::fs::write(&q, b"unknown-binary-data").unwrap();
        assert_eq!(finalize_ext(&q).await, q, "无法识别时保持原样");
    }

    #[tokio::test]
    async fn sweep_removes_orphan_upload_dirs() {
        let tmp = tempdir();
        let dirs = MediaDirs::new(&tmp);
        dirs.save_upload("keep", "audio", &payload("a.mp3", b"x"), AUDIO_EXTS).await.unwrap();
        dirs.save_upload("orphan", "audio", &payload("a.mp3", b"x"), AUDIO_EXTS).await.unwrap();
        dirs.sweep_uploads(&["keep".to_string()]).await;
        assert!(dirs.upload_dir("keep").unwrap().exists());
        assert!(!dirs.upload_dir("orphan").unwrap().exists());
    }
}

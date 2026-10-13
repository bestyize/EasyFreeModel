//! 各协议编解码共用的小工具。

use serde_json::{Map, Value};

use super::canonical::ImageSource;
use super::{ProtocolError, ProtocolResult};

pub(crate) fn invalid(msg: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidRequest(msg.into())
}

pub(crate) fn invalid_resp(msg: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidResponse(msg.into())
}

/// 记录请求里未被识别的顶层字段（只记录字段名，不记录值，避免把正文带进日志）。
pub(crate) fn warn_unknown_fields(obj: &Map<String, Value>, known: &[&str], warnings: &mut Vec<String>) {
    let mut unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !known.contains(k)).collect();
    unknown.sort_unstable();
    for k in unknown {
        warnings.push(format!("unsupported field ignored: {k}"));
    }
}

pub(crate) fn as_object<'a>(v: &'a Value, what: &str) -> ProtocolResult<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| invalid(format!("{what} must be an object")))
}

pub(crate) fn required_str<'a>(obj: &'a Map<String, Value>, key: &str, what: &str) -> ProtocolResult<&'a str> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(invalid(format!("{what}.{key} must be a string"))),
        None => Err(invalid(format!("{what}.{key} is required"))),
    }
}

pub(crate) fn opt_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

pub(crate) fn opt_f64(obj: &Map<String, Value>, key: &str, what: &str) -> ProtocolResult<Option<f64>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| invalid(format!("{what}.{key} must be a number"))),
    }
}

pub(crate) fn opt_u32(obj: &Map<String, Value>, key: &str, what: &str) -> ProtocolResult<Option<u32>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("{what}.{key} must be a non-negative integer"))),
    }
}

pub(crate) fn opt_bool(obj: &Map<String, Value>, key: &str, what: &str) -> ProtocolResult<Option<bool>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid(format!("{what}.{key} must be a boolean"))),
    }
}

/// 解析 `data:<mime>;base64,<payload>`。非 data URL 返回 `Ok(None)`。
pub(crate) fn parse_data_url(url: &str) -> ProtocolResult<Option<(String, String)>> {
    let Some(rest) = url.strip_prefix("data:") else {
        return Ok(None);
    };
    let (meta, payload) = rest.split_once(',').ok_or_else(|| invalid("malformed data URL: missing ','"))?;
    let mut parts = meta.split(';');
    let mime = parts.next().unwrap_or("");
    if !parts.any(|p| p.eq_ignore_ascii_case("base64")) {
        return Err(invalid("data URL must be base64-encoded"));
    }
    let mime = if mime.is_empty() { "application/octet-stream" } else { mime };
    Ok(Some((mime.to_string(), payload.to_string())))
}

/// URL 或 data URL → ImageSource。
pub(crate) fn image_source_from_url(url: &str) -> ProtocolResult<ImageSource> {
    match parse_data_url(url)? {
        Some((media_type, data)) => Ok(ImageSource::Base64 { media_type, data }),
        None => Ok(ImageSource::Url { url: url.to_string() }),
    }
}

/// 工具参数 JSON 字符串 → Value。空串视为 `{}`。
pub(crate) fn parse_json_arguments(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(raw).map_err(|e| format!("invalid JSON ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_parsing() {
        let (m, d) = parse_data_url("data:image/png;base64,AAAA").unwrap().unwrap();
        assert_eq!((m.as_str(), d.as_str()), ("image/png", "AAAA"));
        assert!(parse_data_url("https://x/y.png").unwrap().is_none());
        assert!(parse_data_url("data:image/png,AAAA").is_err());
        assert!(parse_data_url("data:image/png;base64").is_err());
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        assert_eq!(parse_json_arguments("  ").unwrap(), serde_json::json!({}));
        assert!(parse_json_arguments("{oops").is_err());
    }
}

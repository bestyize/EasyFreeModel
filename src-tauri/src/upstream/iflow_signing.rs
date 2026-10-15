//! iFlow request signing, matching 9Router's HMAC-SHA256 wire format.
//! Uses FreeApiBox's own User-Agent consistently for signing and transport.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{client, UpstreamError};

fn hmac_sha256_hex(key: &[u8], payload: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut normalized = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hash = Sha256::digest(key);
        normalized[..hash.len()].copy_from_slice(&hash);
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_key = normalized;
    let mut outer_key = normalized;
    for byte in &mut inner_key { *byte ^= 0x36; }
    for byte in &mut outer_key { *byte ^= 0x5c; }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(payload);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner_hash);
    outer.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn add_signature_headers(headers: &mut HeaderMap, secret: &str) -> Result<(), UpstreamError> {
    let session = format!("session-{}", Uuid::new_v4());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UpstreamError::Config("system clock is earlier than UNIX epoch".into()))?
        .as_millis()
        .to_string();
    let payload = format!("{}:{session}:{timestamp}", client::user_agent());
    let signature = hmac_sha256_hex(secret.as_bytes(), payload.as_bytes());
    headers.insert("session-id", HeaderValue::from_str(&session).expect("UUID session is a valid header"));
    headers.insert("x-iflow-timestamp", HeaderValue::from_str(&timestamp).expect("numeric timestamp is a valid header"));
    let mut signature = HeaderValue::from_str(&signature).expect("hex signature is a valid header");
    signature.set_sensitive(true);
    headers.insert("x-iflow-signature", signature);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_rfc4231_case_one() {
        assert_eq!(hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    }

    #[test]
    fn signs_user_agent_session_and_millis() {
        let mut headers = HeaderMap::new();
        add_signature_headers(&mut headers, "example-secret").unwrap();
        let session = headers.get("session-id").unwrap().to_str().unwrap();
        let timestamp = headers.get("x-iflow-timestamp").unwrap().to_str().unwrap();
        assert!(session.starts_with("session-"));
        assert!(timestamp.parse::<u128>().is_ok());
        let payload = format!("{}:{session}:{timestamp}", client::user_agent());
        assert_eq!(headers.get("x-iflow-signature").unwrap().to_str().unwrap(),
            hmac_sha256_hex(b"example-secret", payload.as_bytes()));
        assert!(headers.get("x-iflow-signature").unwrap().is_sensitive());
    }
}

//! PKCE (RFC 7636, S256) 与 state 生成。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

use super::secret::Secret;

pub const CHALLENGE_METHOD: &str = "S256";

#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: Secret,
    pub challenge: String,
}

fn random_b64url(byte_len: usize) -> String {
    let mut buf = vec![0u8; byte_len];
    rand::fill(&mut buf[..]);
    URL_SAFE_NO_PAD.encode(&buf)
}

/// `BASE64URL(SHA256(ASCII(code_verifier)))`
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 32 字节随机数 -> 43 字符 verifier（RFC 7636 要求 43..=128）。
pub fn generate() -> Pkce {
    let verifier = random_b64url(32);
    let challenge = challenge_for(&verifier);
    Pkce { verifier: Secret::new(verifier), challenge }
}

/// 高熵 state（256 bit）。
pub fn random_state() -> String {
    random_b64url(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(challenge_for(verifier), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn generated_pair_is_consistent_and_well_formed() {
        let p = generate();
        let v = p.verifier.expose();
        assert!((43..=128).contains(&v.len()));
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~')));
        assert_eq!(p.challenge, challenge_for(v));
        assert_eq!(p.challenge.len(), 43);
    }

    #[test]
    fn state_and_verifier_are_unique_and_high_entropy() {
        let a = random_state();
        let b = random_state();
        assert_ne!(a, b);
        assert!(a.len() >= 43);
        assert_ne!(generate().verifier.expose(), generate().verifier.expose());
    }
}

//! RFC 7636 PKCE helpers shared by both frontend flavors. Verifier
//! generation is target-specific (needs a random source), so it lives in
//! `frontend_web`/`frontend_native`; only the pure `verifier -> challenge`
//! hashing step (identical on every target) lives here.

use base64::Engine;
use sha2::{Digest, Sha256};

/// `code_challenge = BASE64URL(SHA-256(ASCII(code_verifier)))`, method S256.
pub fn challenge_from_verifier(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

/// Base64url-encode raw random bytes into a PKCE code_verifier (43-128
/// unreserved characters per the RFC; 32 random bytes -> 43 base64url chars,
/// the minimum, which is plenty of entropy).
pub fn verifier_from_bytes(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

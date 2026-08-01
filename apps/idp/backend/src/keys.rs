use anyhow::Context;
use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use rsa::{
    pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding},
    traits::PublicKeyParts,
    RsaPrivateKey, RsaPublicKey,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::db::{self, SigningKey};

/// Loaded signing keys ready for token operations.
#[derive(Clone)]
pub struct JwtKeys {
    pub kid: String,
    pub encoding: EncodingKey,
    pub decoding: DecodingKey,
    /// Base64url-encoded RSA modulus for JWKS.
    pub n_b64url: String,
    /// Base64url-encoded RSA public exponent for JWKS.
    pub e_b64url: String,
}

impl JwtKeys {
    /// Load the active RS256 key from the database, generating one (and
    /// persisting it) if none exists yet -- this is the only way this IDP
    /// gets a signing key; there's no sops secret for it.
    pub async fn load_or_generate(pool: &PgPool) -> anyhow::Result<Self> {
        if let Some(key) = db::get_active_signing_key(pool).await? {
            return Self::from_db_key(&key);
        }

        tracing::info!("generating new RS256 signing key...");
        let new_key = tokio::task::spawn_blocking(generate_rsa_key)
            .await
            .context("key generation task panicked")??;

        db::save_signing_key(pool, &new_key).await?;
        tracing::info!(kid = %new_key.kid, "RS256 key saved to database");
        Self::from_db_key(&new_key)
    }

    fn from_db_key(key: &SigningKey) -> anyhow::Result<Self> {
        let encoding = EncodingKey::from_rsa_pem(key.private_key_pem.as_bytes())
            .context("failed to load RS256 private key")?;
        let decoding = DecodingKey::from_rsa_pem(key.public_key_pem.as_bytes())
            .context("failed to load RS256 public key")?;
        Ok(Self {
            kid: key.kid.clone(),
            encoding,
            decoding,
            n_b64url: key.n_b64url.clone(),
            e_b64url: key.e_b64url.clone(),
        })
    }

    pub fn signing_header(&self) -> Header {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some(self.kid.clone());
        h
    }
}

fn generate_rsa_key() -> anyhow::Result<SigningKey> {
    let mut rng = rand::thread_rng();
    let private_key = RsaPrivateKey::new(&mut rng, 2048).context("RSA key generation failed")?;
    let public_key = RsaPublicKey::from(&private_key);

    let private_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .context("PKCS#8 PEM serialisation failed")?;
    let public_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .context("SPKI PEM serialisation failed")?;

    let n_b64url = b64url(public_key.n().to_bytes_be());
    let e_b64url = b64url(public_key.e().to_bytes_be());

    let kid = uuid::Uuid::new_v4().to_string();
    Ok(SigningKey {
        kid,
        private_key_pem: private_pem.to_string(),
        public_key_pem: public_pem,
        n_b64url,
        e_b64url,
    })
}

fn b64url(bytes: Vec<u8>) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ── JWT claims ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Roles the user was granted for *this* client_id only -- never other
    /// apps' roles (see `routes::oauth::issue_tokens`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_time: Option<i64>,
}

impl Claims {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        iss: &str,
        sub: &str,
        aud: &str,
        ttl_secs: i64,
        nonce: Option<&str>,
        username: &str,
        display_name: &str,
        scope: &str,
        roles: Vec<String>,
        auth_time: Option<i64>,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        let scopes: std::collections::HashSet<&str> = scope.split_whitespace().collect();
        Self {
            iss: iss.to_string(),
            sub: sub.to_string(),
            aud: aud.to_string(),
            exp: now + ttl_secs,
            iat: now,
            nonce: nonce.map(str::to_string),
            scope: scope.to_string(),
            preferred_username: scopes.contains("profile").then(|| username.to_string()),
            name: scopes.contains("profile").then(|| display_name.to_string()),
            roles,
            auth_time,
        }
    }
}

/// Verify PKCE S256 challenge (RFC 7636 §4.6):
/// `code_challenge = BASE64URL(SHA-256(ASCII(code_verifier)))`.
pub fn verify_pkce_s256(verifier: &str, challenge: &str) -> bool {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(verifier.as_bytes());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    encoded == challenge
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, Validation};

    fn test_keys() -> JwtKeys {
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pub_key = RsaPublicKey::from(&priv_key);

        let priv_pem = priv_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let pub_pem = pub_key.to_public_key_pem(LineEnding::LF).unwrap();

        JwtKeys {
            kid: "test-kid".to_string(),
            encoding: EncodingKey::from_rsa_pem(priv_pem.as_bytes()).unwrap(),
            decoding: DecodingKey::from_rsa_pem(pub_pem.as_bytes()).unwrap(),
            n_b64url: b64url(pub_key.n().to_bytes_be()),
            e_b64url: b64url(pub_key.e().to_bytes_be()),
        }
    }

    #[test]
    fn encode_then_decode_claims_with_roles() {
        let keys = test_keys();
        let claims = Claims::new(
            "https://idp.test",
            "user123",
            "client456",
            3600,
            None,
            "alice",
            "Alice",
            "openid profile",
            vec!["operator".to_string()],
            None,
        );
        let token = jsonwebtoken::encode(&keys.signing_header(), &claims, &keys.encoding).unwrap();

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["client456"]);
        let decoded = jsonwebtoken::decode::<Claims>(&token, &keys.decoding, &validation).unwrap();

        assert_eq!(decoded.claims.sub, "user123");
        assert_eq!(decoded.claims.preferred_username, Some("alice".to_string()));
        assert_eq!(decoded.claims.roles, vec!["operator".to_string()]);
    }

    #[test]
    fn roles_absent_when_empty() {
        let claims = Claims::new(
            "https://idp.test",
            "u",
            "c",
            3600,
            None,
            "u",
            "U",
            "openid",
            vec![],
            None,
        );
        let value = serde_json::to_value(&claims).unwrap();
        assert!(value.get("roles").is_none());
    }

    #[test]
    fn expired_token_is_rejected() {
        let keys = test_keys();
        let claims = Claims::new(
            "https://idp.test",
            "u",
            "c",
            -7200,
            None,
            "u",
            "U",
            "openid",
            vec![],
            None,
        );
        let token = jsonwebtoken::encode(&keys.signing_header(), &claims, &keys.encoding).unwrap();

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["c"]);
        assert!(jsonwebtoken::decode::<Claims>(&token, &keys.decoding, &validation).is_err());
    }

    #[test]
    fn wrong_audience_is_rejected() {
        let keys = test_keys();
        let claims = Claims::new(
            "https://idp.test",
            "u",
            "correct-client",
            3600,
            None,
            "u",
            "U",
            "openid",
            vec![],
            None,
        );
        let token = jsonwebtoken::encode(&keys.signing_header(), &claims, &keys.encoding).unwrap();

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["wrong-client"]);
        assert!(jsonwebtoken::decode::<Claims>(&token, &keys.decoding, &validation).is_err());
    }

    #[test]
    fn pkce_s256_valid() {
        // From RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(verify_pkce_s256(verifier, challenge));
    }

    #[test]
    fn pkce_s256_invalid_verifier() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(!verify_pkce_s256("wrong-verifier", challenge));
    }
}

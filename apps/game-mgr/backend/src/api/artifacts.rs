//! `/artifacts` -- the desktop client never holds bucket credentials (PLAN.md
//! §4.3): it browses and downloads bucket content entirely through these two
//! endpoints instead of talking to S3 directly.
//!
//! - `scan`: lists objects under a prefix and resolves each one's
//!   `<key>.sha256` sidecar server-side, so the client gets back a ready-to-use
//!   file list for the Add/Edit Game picker without ever listing the bucket
//!   itself.
//! - `download_url`: hands out a short-lived presigned GET for exactly one
//!   object. A presigned URL is a plain HTTPS GET (with an optional `Range`
//!   header for resuming) that needs no further authentication -- the
//!   scoping is "can read this one key for a few minutes", not "can read the
//!   whole bucket forever" the way a long-lived credential would be.

use aws_sdk_s3::presigning::PresigningConfig;
use axum::extract::{Query, State};
use axum::response::Json;
use game_mgr_api_types::{DownloadUrlResponse, ScannedObjectDto};
use serde::Deserialize;
use std::time::Duration;

use super::AppState;
use crate::auth::Authed;
use crate::error::ApiError;

const MAX_PREFIX_LEN: usize = 512;
const MAX_KEY_LEN: usize = 1024;
/// Long enough for a slow home connection to grab a multi-GB installer
/// without needing a mid-download refresh in the common case; the client
/// still handles expiry by requesting a fresh URL and resuming (see
/// `game_mgr_core::s3`).
const DOWNLOAD_URL_TTL: Duration = Duration::from_secs(15 * 60);
/// `.sha256` sidecars are a couple hundred bytes; anything bigger isn't one.
const MAX_SIDECAR_LEN: i64 = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct ScanQuery {
    pub prefix: String,
}

fn validate_prefix(prefix: &str) -> Result<&str, ApiError> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_PREFIX_LEN {
        return Err(ApiError::Unprocessable(format!(
            "prefix must be between 1 and {MAX_PREFIX_LEN} characters"
        )));
    }
    Ok(trimmed)
}

/// List a bucket prefix and resolve each file's sha256 from a sidecar object
/// next to it, if one exists -- mirrors what `game_mgr_core::scan` used to
/// do directly against S3.
pub async fn scan(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Query(query): Query<ScanQuery>,
) -> Result<Json<Vec<ScannedObjectDto>>, ApiError> {
    let prefix = validate_prefix(&query.prefix)?;

    let mut entries: Vec<(String, i64)> = Vec::new();
    let mut pages = state
        .s3
        .list_objects_v2()
        .bucket(&state.bucket)
        .prefix(prefix)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|err| anyhow::anyhow!("listing {prefix}: {err}"))?;
        for object in page.contents() {
            if let Some(key) = object.key() {
                entries.push((key.to_string(), object.size().unwrap_or(0)));
            }
        }
    }
    entries.sort();

    let mut sidecars: std::collections::HashMap<String, String> = Default::default();
    let mut files: Vec<(String, i64)> = Vec::new();
    for (key, size) in entries {
        match key.strip_suffix(".sha256") {
            Some(target) => {
                sidecars.insert(target.to_string(), key);
            }
            None => files.push((key, size)),
        }
    }

    let mut scanned = Vec::with_capacity(files.len());
    for (key, size) in files {
        let sha256 = match sidecars.get(&key) {
            Some(sidecar_key) => match read_sidecar(&state, sidecar_key).await {
                Ok(Some(hash)) => Some(hash),
                Ok(None) => {
                    tracing::warn!(key = %sidecar_key, "sidecar exists but holds no sha256");
                    None
                }
                Err(err) => {
                    tracing::warn!(key = %sidecar_key, %err, "failed to read sidecar");
                    None
                }
            },
            None => None,
        };
        scanned.push(ScannedObjectDto { key, size, sha256 });
    }
    Ok(Json(scanned))
}

async fn read_sidecar(state: &AppState, key: &str) -> anyhow::Result<Option<String>> {
    let response = state
        .s3
        .get_object()
        .bucket(&state.bucket)
        .key(key)
        .send()
        .await
        .map_err(|err| anyhow::anyhow!("GetObject {key}: {err}"))?;
    if response.content_length().unwrap_or(0) > MAX_SIDECAR_LEN {
        anyhow::bail!("{key} is larger than {MAX_SIDECAR_LEN} bytes -- not a sidecar?");
    }
    let bytes = response
        .body
        .collect()
        .await
        .map_err(|err| anyhow::anyhow!("reading {key}: {err}"))?
        .into_bytes();
    Ok(parse_sha256_sidecar(&bytes))
}

/// Parse `sha256sum`-style sidecar content: first 64-hex token wins. Kept in
/// sync with (but independent of) `game_mgr_core::scan`'s identical parser --
/// the backend doesn't depend on `game-mgr-core`, which pulls in the desktop
/// client's process-launch/Syncthing/Windows-registry code, just for this
/// one pure string helper.
fn parse_sha256_sidecar(content: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(content);
    text.split_whitespace()
        .find(|token| token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_lowercase)
}

#[derive(Debug, Deserialize)]
pub struct DownloadUrlQuery {
    pub key: String,
}

fn validate_key(key: &str) -> Result<&str, ApiError> {
    let trimmed = key.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_KEY_LEN {
        return Err(ApiError::Unprocessable(format!(
            "key must be between 1 and {MAX_KEY_LEN} characters"
        )));
    }
    Ok(trimmed)
}

/// Presign a `GetObject` for exactly one key. Any authenticated user may
/// request one for any key -- same household-trust model as the rest of
/// this API (PLAN.md §8.0's "any authenticated user reads everything").
pub async fn download_url(
    State(state): State<AppState>,
    Authed(_user): Authed,
    Query(query): Query<DownloadUrlQuery>,
) -> Result<Json<DownloadUrlResponse>, ApiError> {
    let key = validate_key(&query.key)?;
    let presigning = PresigningConfig::expires_in(DOWNLOAD_URL_TTL)
        .map_err(|err| anyhow::anyhow!("building presigning config: {err}"))?;
    let presigned = state
        .s3
        .get_object()
        .bucket(&state.bucket)
        .key(key)
        .presigned(presigning)
        .await
        .map_err(|err| anyhow::anyhow!("presigning {key}: {err}"))?;
    Ok(Json(DownloadUrlResponse {
        url: presigned.uri().to_string(),
        expires_in_s: DOWNLOAD_URL_TTL.as_secs(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_validation_rules() {
        assert!(validate_prefix("gog/bg3/").is_ok());
        assert!(validate_prefix("").is_err());
        assert!(validate_prefix("   ").is_err());
        assert!(validate_prefix(&"x".repeat(MAX_PREFIX_LEN + 1)).is_err());
    }

    #[test]
    fn key_validation_rules() {
        assert!(validate_key("gog/bg3/setup.exe").is_ok());
        assert!(validate_key("").is_err());
        assert!(validate_key(&"x".repeat(MAX_KEY_LEN + 1)).is_err());
    }

    #[test]
    fn sidecar_parsing_takes_the_hex_token() {
        let hash = "ab".repeat(32);
        let content = format!("{hash}  setup_baldurs_gate_3_(89470)-1.bin\n");
        assert_eq!(parse_sha256_sidecar(content.as_bytes()), Some(hash.clone()));
        assert_eq!(
            parse_sha256_sidecar(hash.to_uppercase().as_bytes()),
            Some(hash)
        );
        assert_eq!(parse_sha256_sidecar(b"not a hash at all"), None);
        assert_eq!(parse_sha256_sidecar(b""), None);
    }
}

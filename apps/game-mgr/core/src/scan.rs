//! Bucket prefix scanning for the Add/Edit Game UI: list the objects, read
//! their `.sha256` sidecars (instant — no multi-GB streaming) and suggest a
//! role per file. The user adjusts roles in the picker before submitting.

use anyhow::Result;
use game_mgr_api_types::ArtifactRole;

use crate::s3::S3Client;

/// Role suggestion for the picker; `Ignore` files are not submitted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestedRole {
    Base,
    Patch,
    Dlc,
    /// Not part of an install (Linux `.sh` installers when targeting
    /// Proton, readmes, …).
    Ignore,
}

impl SuggestedRole {
    pub fn as_role(self) -> Option<ArtifactRole> {
        match self {
            SuggestedRole::Base => Some(ArtifactRole::Base),
            SuggestedRole::Patch => Some(ArtifactRole::Patch),
            SuggestedRole::Dlc => Some(ArtifactRole::Dlc),
            SuggestedRole::Ignore => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub bucket_key: String,
    pub size: i64,
    /// From the `<key>.sha256` sidecar; `None` = no sidecar (submit falls
    /// back to streaming + hashing that file).
    pub sha256: Option<String>,
    pub suggested: SuggestedRole,
}

/// Parse `sha256sum`-style sidecar content: first 64-hex token wins.
pub fn parse_sha256_sidecar(content: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(content);
    text.split_whitespace()
        .find(|token| token.len() == 64 && token.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_lowercase)
}

/// Heuristic role suggestion from the key path relative to the scan prefix.
/// Only `.exe`/`.bin` are installable by the gog class; everything else
/// (mojosetup `.sh`, readmes, …) defaults to Ignore — the Farlanders case:
/// a Linux `.sh` installer must never reach innoextract.
pub fn suggest_role(prefix: &str, key: &str) -> SuggestedRole {
    let relative = key.strip_prefix(prefix).unwrap_or(key).to_lowercase();
    let filename = relative.rsplit('/').next().unwrap_or(&relative).to_string();
    let extension = filename.rsplit('.').next().unwrap_or_default().to_string();

    if extension != "exe" && extension != "bin" {
        return SuggestedRole::Ignore;
    }
    if relative.contains("dlc") {
        return SuggestedRole::Dlc;
    }
    if relative.starts_with("patches/")
        || relative.contains("/patches/")
        || filename.starts_with("patch_")
    {
        return SuggestedRole::Patch;
    }
    SuggestedRole::Base
}

/// List a prefix and resolve hashes from sidecars. Fast: one listing plus
/// one tiny GET per sidecar.
pub async fn scan_prefix(s3: &S3Client, prefix: &str) -> Result<Vec<ScannedFile>> {
    let keys = s3.list_keys(prefix).await?;

    let mut sidecars: std::collections::HashMap<String, String> = Default::default();
    let mut files: Vec<(String, i64)> = Vec::new();
    for (key, size) in keys {
        if let Some(target) = key.strip_suffix(".sha256") {
            sidecars.insert(target.to_string(), key.clone());
        } else {
            files.push((key, size));
        }
    }

    let mut scanned = Vec::with_capacity(files.len());
    for (key, size) in files {
        let sha256 = match sidecars.get(&key) {
            Some(sidecar_key) => match s3.read_small(sidecar_key, 64 * 1024).await {
                Ok(content) => {
                    let parsed = parse_sha256_sidecar(&content);
                    if parsed.is_none() {
                        tracing::warn!(key = %sidecar_key, "sidecar exists but holds no sha256");
                    }
                    parsed
                }
                Err(err) => {
                    tracing::warn!(key = %sidecar_key, %err, "failed to read sidecar");
                    None
                }
            },
            None => None,
        };
        scanned.push(ScannedFile {
            suggested: suggest_role(prefix, &key),
            bucket_key: key,
            size,
            sha256,
        });
    }
    Ok(scanned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_parsing_takes_the_hex_token() {
        let hash = "ab".repeat(32);
        // sha256sum format: "<hash>  <filename>"
        let content = format!("{hash}  setup_baldurs_gate_3_(89470)-1.bin\n");
        assert_eq!(parse_sha256_sidecar(content.as_bytes()), Some(hash.clone()));
        // hash only, uppercase normalised
        assert_eq!(
            parse_sha256_sidecar(hash.to_uppercase().as_bytes()),
            Some(hash)
        );
        assert_eq!(parse_sha256_sidecar(b"not a hash at all"), None);
        assert_eq!(parse_sha256_sidecar(b""), None);
    }

    #[test]
    fn role_suggestions_match_the_bg3_layout() {
        let p = "gog/baldurs_gate_iii/";
        assert_eq!(
            suggest_role(p, "gog/baldurs_gate_iii/setup_baldurs_gate_3_(89470).exe"),
            SuggestedRole::Base
        );
        assert_eq!(
            suggest_role(
                p,
                "gog/baldurs_gate_iii/setup_baldurs_gate_3_(89470)-17.bin"
            ),
            SuggestedRole::Base
        );
        assert_eq!(
            suggest_role(
                p,
                "gog/baldurs_gate_iii/patches/patch_baldurs_gate_3_Live_(85464).exe"
            ),
            SuggestedRole::Patch
        );
        assert_eq!(
            suggest_role(p, "gog/baldurs_gate_iii/patch_hotfix6.exe"),
            SuggestedRole::Patch
        );
        assert_eq!(
            suggest_role(p, "gog/some-game/dlc/setup_some_dlc.exe"),
            SuggestedRole::Dlc
        );
    }

    #[test]
    fn non_installer_files_are_ignored_by_default() {
        let p = "gog/farlanders/";
        // the Farlanders case: a native Linux installer must not be
        // suggested for the Proton pipeline
        assert_eq!(
            suggest_role(p, "gog/farlanders/farlanders_prologue.sh"),
            SuggestedRole::Ignore
        );
        assert_eq!(
            suggest_role(p, "gog/farlanders/readme.txt"),
            SuggestedRole::Ignore
        );
        assert_eq!(
            suggest_role(p, "gog/farlanders/cover.png"),
            SuggestedRole::Ignore
        );
        assert_eq!(
            suggest_role(p, "gog/farlanders/setup_farlanders.exe"),
            SuggestedRole::Base
        );
    }
}

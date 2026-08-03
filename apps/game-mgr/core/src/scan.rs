//! Bucket prefix scanning for the Add/Edit Game UI: list the objects (via
//! the backend's `/api/v1/artifacts/scan`, which also resolves each file's
//! `.sha256` sidecar server-side -- instant, no multi-GB streaming, and no
//! bucket credentials on this side) and suggest a role per file. The user
//! adjusts roles in the picker before submitting.

use anyhow::Result;
use game_mgr_api_types::ArtifactRole;

use crate::stats::ServerClient;

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

/// List a prefix through the backend, which already resolved each file's
/// sidecar hash server-side, and apply the role heuristic client-side.
pub async fn scan_prefix(server: &ServerClient, prefix: &str) -> Result<Vec<ScannedFile>> {
    let objects = server.scan(prefix).await?;
    Ok(objects
        .into_iter()
        .map(|obj| ScannedFile {
            suggested: suggest_role(prefix, &obj.key),
            bucket_key: obj.key,
            size: obj.size,
            sha256: obj.sha256,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn scan_prefix_applies_role_heuristic_to_the_backend_response() {
        use crate::oidc::StaticToken;
        use axum::routing::get;
        use std::sync::Arc;

        let app = axum::Router::new().route(
            "/api/v1/artifacts/scan",
            get(|| async {
                axum::Json(vec![
                    game_mgr_api_types::ScannedObjectDto {
                        key: "gog/bg3/setup.exe".into(),
                        size: 100,
                        sha256: Some("ab".repeat(32)),
                    },
                    game_mgr_api_types::ScannedObjectDto {
                        key: "gog/bg3/readme.txt".into(),
                        size: 5,
                        sha256: None,
                    },
                ])
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let server = ServerClient::new(
            &format!("http://{addr}"),
            Arc::new(StaticToken("tok".into())),
        )
        .unwrap();
        let files = scan_prefix(&server, "gog/bg3/").await.unwrap();

        let setup = files
            .iter()
            .find(|f| f.bucket_key == "gog/bg3/setup.exe")
            .unwrap();
        assert_eq!(setup.sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(setup.suggested, SuggestedRole::Base);

        let readme = files
            .iter()
            .find(|f| f.bucket_key == "gog/bg3/readme.txt")
            .unwrap();
        assert!(readme.sha256.is_none());
        assert_eq!(readme.suggested, SuggestedRole::Ignore);
    }
}

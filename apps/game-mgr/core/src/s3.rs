//! Bucket downloads via the backend's presigned URLs (PLAN.md §4.3
//! S3Fetch) -- the desktop client never holds bucket credentials or talks
//! to S3 directly; every access goes through `apps/game-mgr/backend/src/api/
//! artifacts.rs`'s two endpoints instead (`ServerClient::scan`/
//! `download_url`). A presigned URL is a plain HTTPS GET, so downloading one
//! needs nothing more than the shared `reqwest::Client` every other outbound
//! request in this crate already uses (`Services::http`).

use std::path::Path;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::header::RANGE;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::game::{Progress, ProgressSink};
use crate::stats::ServerClient;

/// A presigned URL is short-lived (15 minutes server-side, see
/// `DOWNLOAD_URL_TTL` in the backend); requesting a fresh one on every
/// attempt -- rather than caching and hoping it's still valid -- means a
/// stalled connection or a retry after a long pause never fails on an
/// expired signature.
const MAX_ATTEMPTS: u32 = 5;

/// Download `key` to `dest`, resuming a partial `.part` file via a `Range`
/// request and verifying the whole file against `sha256`. The `.part` is
/// renamed into place only after verification.
pub async fn download(
    http: &reqwest::Client,
    server: &ServerClient,
    key: &str,
    dest: &Path,
    sha256: &str,
    progress: &ProgressSink,
    cancel: &CancellationToken,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part_path = part_path(dest);

    let mut last_err = None;
    for attempt in 1..=MAX_ATTEMPTS {
        if cancel.is_cancelled() {
            bail!("download cancelled");
        }
        match try_download_once(
            http, server, key, dest, &part_path, sha256, progress, cancel,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(err) => {
                tracing::warn!(key = %key, attempt, %err, "download attempt failed");
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("download failed: {key}")))
}

#[allow(clippy::too_many_arguments)]
async fn try_download_once(
    http: &reqwest::Client,
    server: &ServerClient,
    key: &str,
    dest: &Path,
    part_path: &Path,
    sha256: &str,
    progress: &ProgressSink,
    cancel: &CancellationToken,
) -> Result<()> {
    // hash whatever we already have, then continue from its end
    let mut hasher = Sha256::new();
    let mut offset: u64 = 0;
    if let Ok(meta) = tokio::fs::metadata(part_path).await {
        offset = meta.len();
        let mut existing = tokio::fs::File::open(part_path).await?;
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = existing.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }

    let download = server
        .download_url(key)
        .await
        .context("requesting download URL")?;
    let mut request = http.get(&download.url);
    if offset > 0 {
        request = request.header(RANGE, format!("bytes={offset}-"));
    }
    let mut response = request.send().await.with_context(|| format!("GET {key}"))?;
    let status = response.status();

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // the whole object is already in the .part file
    } else if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("GET {key} answered {status}: {text}");
    } else {
        let total = response
            .content_length()
            .map(|remaining| offset + remaining);
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(part_path)
            .await?;
        let mut done = offset;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => bail!("download cancelled"),
                chunk = response.chunk() => chunk.context("reading response body")?,
            };
            let Some(chunk) = chunk else { break };
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            progress.send(Progress::Bytes { done, total });
        }
        file.flush().await?;
    }

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(sha256) {
        // a corrupt part would resume corrupt forever -- start over next time
        let _ = tokio::fs::remove_file(part_path).await;
        bail!("sha256 mismatch for {key}: expected {sha256}, got {actual}");
    }
    tokio::fs::rename(part_path, dest).await?;
    Ok(())
}

/// Stream an object and return its sha256 + size without writing to disk --
/// fallback when an artifact has no `.sha256` sidecar (PLAN.md §4.1's Add
/// Game flow). One attempt only: a stream this transient failing mid-way
/// just fails the submission, unlike the resumable install-time `download`
/// above, which has a `.part` file worth resuming across attempts.
pub async fn stream_and_hash(
    http: &reqwest::Client,
    server: &ServerClient,
    key: &str,
    progress: &ProgressSink,
    cancel: &CancellationToken,
) -> Result<(String, u64)> {
    let download = server
        .download_url(key)
        .await
        .context("requesting download URL")?;
    let mut response = http
        .get(&download.url)
        .send()
        .await
        .with_context(|| format!("GET {key}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        bail!("GET {key} answered {status}: {text}");
    }
    let total = response.content_length();
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => bail!("hashing cancelled"),
            chunk = response.chunk() => chunk.context("reading response body")?,
        };
        let Some(chunk) = chunk else { break };
        hasher.update(&chunk);
        done += chunk.len() as u64;
        progress.send(Progress::Bytes { done, total });
    }
    Ok((hex::encode(hasher.finalize()), done))
}

fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oidc::StaticToken;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("/tmp/x/setup.exe")),
            Path::new("/tmp/x/setup.exe.part")
        );
    }

    #[derive(Clone)]
    struct FileServer {
        content: Bytes,
        object_url: String,
        get_calls: Arc<AtomicUsize>,
    }

    /// Backend stub serving `/api/v1/artifacts/download-url` (pointing back
    /// at this same server's own `/object`) and the object itself, honouring
    /// `Range` like a real presigned S3 GET would.
    async fn spawn_file_server(content: &'static [u8]) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let get_calls = Arc::new(AtomicUsize::new(0));
        let state = FileServer {
            content: Bytes::from_static(content),
            object_url: format!("http://{addr}/object"),
            get_calls: get_calls.clone(),
        };
        let app = Router::new()
            .route(
                "/api/v1/artifacts/download-url",
                get(|State(state): State<FileServer>| async move {
                    axum::Json(game_mgr_api_types::DownloadUrlResponse {
                        url: state.object_url.clone(),
                        expires_in_s: 900,
                    })
                }),
            )
            .route(
                "/object",
                get(
                    |State(state): State<FileServer>, headers: HeaderMap| async move {
                        state.get_calls.fetch_add(1, Ordering::SeqCst);
                        object_handler(state, headers)
                    },
                ),
            )
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), get_calls)
    }

    fn object_handler(state: FileServer, headers: HeaderMap) -> Response {
        let range = headers
            .get(RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.strip_suffix('-'))
            .and_then(|v| v.parse::<usize>().ok());
        match range {
            Some(start) if start >= state.content.len() => {
                StatusCode::RANGE_NOT_SATISFIABLE.into_response()
            }
            Some(start) => {
                (StatusCode::PARTIAL_CONTENT, state.content.slice(start..)).into_response()
            }
            None => (StatusCode::OK, state.content).into_response(),
        }
    }

    #[tokio::test]
    async fn download_verifies_hash_and_writes_dest() {
        let content = b"the actual installer bytes";
        let (base, _calls) = spawn_file_server(content).await;
        let server = ServerClient::new(&base, Arc::new(StaticToken("tok".into()))).unwrap();
        let http = reqwest::Client::new();
        let expected_sha = hex::encode(Sha256::digest(content));

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("setup.exe");
        download(
            &http,
            &server,
            "object",
            &dest,
            &expected_sha,
            &ProgressSink::new(|_| {}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), content);
        assert!(!part_path(&dest).exists());
    }

    #[tokio::test]
    async fn download_rejects_hash_mismatch_and_cleans_up_part() {
        let content = b"the actual installer bytes";
        let (base, _calls) = spawn_file_server(content).await;
        let server = ServerClient::new(&base, Arc::new(StaticToken("tok".into()))).unwrap();
        let http = reqwest::Client::new();

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("setup.exe");
        let err = download(
            &http,
            &server,
            "object",
            &dest,
            &"00".repeat(32),
            &ProgressSink::new(|_| {}),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("sha256 mismatch"), "{err:#}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists(), "corrupt .part is cleaned up");
    }

    #[tokio::test]
    async fn stream_and_hash_matches_download() {
        let content = b"hash me without touching disk";
        let (base, _calls) = spawn_file_server(content).await;
        let server = ServerClient::new(&base, Arc::new(StaticToken("tok".into()))).unwrap();
        let http = reqwest::Client::new();

        let (sha, size) = stream_and_hash(
            &http,
            &server,
            "object",
            &ProgressSink::new(|_| {}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(sha, hex::encode(Sha256::digest(content)));
        assert_eq!(size, content.len() as u64);
    }
}

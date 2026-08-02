//! S3 client integration tests against a real S3-compatible server
//! (MinIO in CI, your RGW locally). Gated like the DB tests (PLAN.md §15):
//! skipped without `GM_S3_TEST_ENDPOINT`; `GM_REQUIRE_S3_TESTS` makes
//! skipping a failure (CI).
//!
//! Required env: GM_S3_TEST_ENDPOINT, GM_S3_TEST_ACCESS_KEY, GM_S3_TEST_SECRET_KEY

use game_mgr_core::config::S3Config;
use game_mgr_core::game::ProgressSink;
use game_mgr_core::s3::S3Client;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn test_client() -> Option<(S3Client, String)> {
    let Ok(endpoint) = std::env::var("GM_S3_TEST_ENDPOINT") else {
        if std::env::var("GM_REQUIRE_S3_TESTS").is_ok() {
            panic!("GM_REQUIRE_S3_TESTS is set but GM_S3_TEST_ENDPOINT is missing");
        }
        eprintln!("skipping S3 test: GM_S3_TEST_ENDPOINT not set (see PLAN.md §15)");
        return None;
    };
    let bucket = format!("gm-test-{}", Uuid::new_v4().simple());
    let cfg = S3Config {
        endpoint: Some(endpoint),
        bucket: Some(bucket.clone()),
        region: Some("us-east-1".into()),
        access_key_id: Some(
            std::env::var("GM_S3_TEST_ACCESS_KEY").unwrap_or_else(|_| "gamemgr".into()),
        ),
        secret_access_key: Some(
            std::env::var("GM_S3_TEST_SECRET_KEY").unwrap_or_else(|_| "gamemgr-secret".into()),
        ),
    };
    Some((
        S3Client::from_config(&cfg).expect("complete S3 config"),
        bucket,
    ))
}

async fn with_bucket() -> Option<S3Client> {
    let (client, bucket) = test_client()?;
    client
        .raw()
        .create_bucket()
        .bucket(&bucket)
        .send()
        .await
        .expect("create test bucket");
    Some(client)
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[tokio::test]
async fn download_verifies_and_renames_into_place() {
    let Some(client) = with_bucket().await else {
        return;
    };
    let body: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
    let hash = sha256_hex(&body);
    client
        .put("gog/test/setup.exe", body.clone())
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("setup.exe");
    let progressed = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let seen = progressed.clone();
    client
        .download(
            "gog/test/setup.exe",
            &dest,
            &hash,
            &ProgressSink::new(move |p| {
                if let game_mgr_core::game::Progress::Bytes { done, .. } = p {
                    seen.store(done, std::sync::atomic::Ordering::SeqCst);
                }
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert!(
        !dest.with_file_name("setup.exe.part").exists(),
        "part renamed away"
    );
    assert_eq!(
        progressed.load(std::sync::atomic::Ordering::SeqCst),
        body.len() as u64,
        "progress reached the full size"
    );
}

#[tokio::test]
async fn download_resumes_a_partial_file() {
    let Some(client) = with_bucket().await else {
        return;
    };
    let body: Vec<u8> = (0..1024 * 1024).map(|i| (i % 173) as u8).collect();
    let hash = sha256_hex(&body);
    client
        .put("gog/test/resume.bin", body.clone())
        .await
        .unwrap();

    // simulate an interrupted download: first half already in the .part
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("resume.bin");
    std::fs::write(dir.path().join("resume.bin.part"), &body[..body.len() / 2]).unwrap();

    let first_progress = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let seen = first_progress.clone();
    client
        .download(
            "gog/test/resume.bin",
            &dest,
            &hash,
            &ProgressSink::new(move |p| {
                if let game_mgr_core::game::Progress::Bytes { done, .. } = p {
                    let _ = seen.fetch_min(done, std::sync::atomic::Ordering::SeqCst);
                }
            }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body,
        "resumed file is intact"
    );
    let first = first_progress.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        first > body.len() as u64 / 2,
        "first progress event ({first}) must start beyond the resumed half — \
         otherwise the download restarted instead of resuming"
    );
}

#[tokio::test]
async fn corrupt_download_fails_and_clears_the_part() {
    let Some(client) = with_bucket().await else {
        return;
    };
    let body = b"actual-content".to_vec();
    client.put("gog/test/corrupt.bin", body).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("corrupt.bin");
    let err = client
        .download(
            "gog/test/corrupt.bin",
            &dest,
            &sha256_hex(b"some-other-content"),
            &ProgressSink::noop(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("sha256 mismatch"), "{err}");
    assert!(!dest.exists());
    assert!(
        !dir.path().join("corrupt.bin.part").exists(),
        "corrupt part must be cleared so the retry starts clean"
    );
}

#[tokio::test]
async fn list_and_hash_drive_game_definition() {
    // the Add Game flow: scan a prefix, hash every object from the bucket
    let Some(client) = with_bucket().await else {
        return;
    };
    let exe = b"installer-exe-bytes".to_vec();
    let bin = vec![3u8; 300 * 1024];
    client
        .put("gog/bg3/setup_bg3.exe", exe.clone())
        .await
        .unwrap();
    client
        .put("gog/bg3/setup_bg3-1.bin", bin.clone())
        .await
        .unwrap();
    client
        .put("gog/other/unrelated.bin", b"x".to_vec())
        .await
        .unwrap();

    let keys = client.list_keys("gog/bg3/").await.unwrap();
    assert_eq!(
        keys.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        vec!["gog/bg3/setup_bg3-1.bin", "gog/bg3/setup_bg3.exe"],
        "only the prefix, sorted"
    );
    assert_eq!(keys[0].1, bin.len() as i64, "listing reports sizes");

    let (sha, size) = client
        .hash_object(
            "gog/bg3/setup_bg3.exe",
            &ProgressSink::noop(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(sha, sha256_hex(&exe));
    assert_eq!(size, exe.len() as u64);

    assert!(client.list_keys("gog/empty/").await.unwrap().is_empty());
}

#[tokio::test]
async fn scan_reads_sidecar_hashes_without_streaming() {
    // the BG3 layout: every file has a <name>.sha256 sidecar — scanning
    // must resolve hashes from those tiny files, not the multi-GB objects
    let Some(client) = with_bucket().await else {
        return;
    };
    let exe = b"installer-bytes".to_vec();
    let bin = vec![9u8; 200 * 1024];
    let exe_hash = sha256_hex(&exe);
    let bin_hash = sha256_hex(&bin);
    client.put("gog/bg3/setup_bg3.exe", exe).await.unwrap();
    client
        .put(
            "gog/bg3/setup_bg3.exe.sha256",
            format!("{exe_hash}  setup_bg3.exe\n").into_bytes(),
        )
        .await
        .unwrap();
    client
        .put("gog/bg3/setup_bg3-1.bin", bin.clone())
        .await
        .unwrap();
    client
        .put(
            "gog/bg3/setup_bg3-1.bin.sha256",
            bin_hash.clone().into_bytes(),
        )
        .await
        .unwrap();
    // patch with sidecar, a Linux installer and a file without a sidecar
    client
        .put("gog/bg3/patches/patch_v2.exe", b"p".to_vec())
        .await
        .unwrap();
    client
        .put(
            "gog/bg3/patches/patch_v2.exe.sha256",
            format!("{}  patch_v2.exe", sha256_hex(b"p")).into_bytes(),
        )
        .await
        .unwrap();
    client
        .put("gog/bg3/native_installer.sh", b"#!/bin/sh".to_vec())
        .await
        .unwrap();
    client
        .put("gog/bg3/no_sidecar.bin", b"orphan".to_vec())
        .await
        .unwrap();

    let files = game_mgr_core::scan::scan_prefix(&client, "gog/bg3/")
        .await
        .unwrap();
    let find = |key: &str| files.iter().find(|f| f.bucket_key == key).unwrap();

    // sidecars are consumed, not listed
    assert_eq!(files.len(), 5, "{files:?}");
    assert!(!files.iter().any(|f| f.bucket_key.ends_with(".sha256")));

    assert_eq!(
        find("gog/bg3/setup_bg3.exe").sha256.as_deref(),
        Some(exe_hash.as_str())
    );
    assert_eq!(
        find("gog/bg3/setup_bg3-1.bin").sha256.as_deref(),
        Some(bin_hash.as_str())
    );
    assert_eq!(find("gog/bg3/setup_bg3-1.bin").size, bin.len() as i64);
    assert_eq!(
        find("gog/bg3/patches/patch_v2.exe").suggested,
        game_mgr_core::scan::SuggestedRole::Patch
    );
    // the Farlanders case: native installers default to Ignore
    assert_eq!(
        find("gog/bg3/native_installer.sh").suggested,
        game_mgr_core::scan::SuggestedRole::Ignore
    );
    assert_eq!(
        find("gog/bg3/no_sidecar.bin").sha256,
        None,
        "missing sidecar -> None"
    );
}

#[tokio::test]
async fn cancellation_aborts_the_download() {
    let Some(client) = with_bucket().await else {
        return;
    };
    let body: Vec<u8> = vec![7u8; 4 * 1024 * 1024];
    let hash = sha256_hex(&body);
    client.put("gog/test/cancel.bin", body).await.unwrap();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("cancel.bin");
    let cancel = CancellationToken::new();
    cancel.cancel(); // cancelled before the first chunk lands
    let err = client
        .download(
            "gog/test/cancel.bin",
            &dest,
            &hash,
            &ProgressSink::noop(),
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("cancelled"), "{err}");
    assert!(!dest.exists());
}

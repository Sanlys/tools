//! Ceph RGW (S3) client: resumable, checksum-verified downloads
//! (PLAN.md §4.3 S3Fetch).

use std::path::Path;

use anyhow::{Context, Result, bail};
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::config::S3Config;
use crate::game::{Progress, ProgressSink};

pub struct S3Client {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl S3Client {
    /// `None` when the config is incomplete (S3 not set up on this machine).
    pub fn from_config(cfg: &S3Config) -> Option<S3Client> {
        let (endpoint, bucket, key_id, secret) = (
            cfg.endpoint.as_ref()?,
            cfg.bucket.as_ref()?,
            cfg.access_key_id.as_ref()?,
            cfg.secret_access_key.as_ref()?,
        );
        let credentials = Credentials::from_keys(key_id, secret, None);
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(
                cfg.region.clone().unwrap_or_else(|| "us-east-1".into()),
            ))
            .endpoint_url(endpoint)
            .credentials_provider(credentials)
            // RGW/MinIO style addressing
            .force_path_style(true)
            .build();
        Some(S3Client {
            client: aws_sdk_s3::Client::from_conf(config),
            bucket: bucket.clone(),
        })
    }

    pub fn raw(&self) -> &aws_sdk_s3::Client {
        &self.client
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// List object keys (with sizes) under a prefix — drives the Add Game
    /// UI's "scan bucket prefix" flow.
    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<(String, i64)>> {
        let mut keys = Vec::new();
        let mut pages = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(prefix)
            .into_paginator()
            .send();
        while let Some(page) = pages.next().await {
            let page = page.with_context(|| format!("listing {prefix}"))?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    keys.push((key.to_string(), object.size().unwrap_or(0)));
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// Fetch a small object fully into memory (sha256 sidecars). Errors if
    /// the object exceeds `max_len` — sidecars are a couple hundred bytes.
    pub async fn read_small(&self, key: &str, max_len: usize) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("GetObject {key}"))?;
        let mut body = response.body;
        let mut data = Vec::new();
        while let Some(chunk) = body.try_next().await.context("reading object body")? {
            data.extend_from_slice(&chunk);
            if data.len() > max_len {
                bail!("{key} is larger than {max_len} bytes — not a sidecar?");
            }
        }
        Ok(data)
    }

    /// Stream an object and return its sha256 + size without writing to
    /// disk — fallback when an artifact has no `.sha256` sidecar.
    pub async fn hash_object(
        &self,
        key: &str,
        progress: &ProgressSink,
        cancel: &CancellationToken,
    ) -> Result<(String, u64)> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("GetObject {key}"))?;
        let total = response
            .content_length()
            .and_then(|len| u64::try_from(len).ok());
        let mut body = response.body;
        let mut hasher = Sha256::new();
        let mut done = 0u64;
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => bail!("hashing cancelled"),
                chunk = body.try_next() => chunk.context("reading object body")?,
            };
            let Some(chunk) = chunk else { break };
            hasher.update(&chunk);
            done += chunk.len() as u64;
            progress.send(Progress::Bytes { done, total });
        }
        Ok((hex::encode(hasher.finalize()), done))
    }

    /// Upload helper (test fixtures; future artifact tooling).
    pub async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(bytes.into())
            .send()
            .await
            .with_context(|| format!("PutObject {key}"))?;
        Ok(())
    }

    /// Download `key` to `dest`, resuming a partial `.part` file via a
    /// `Range` request and verifying the whole file against `sha256`.
    /// The `.part` is renamed into place only after verification.
    pub async fn download(
        &self,
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

        // hash whatever we already have, then continue from its end
        let mut hasher = Sha256::new();
        let mut offset: u64 = 0;
        if let Ok(meta) = tokio::fs::metadata(&part_path).await {
            offset = meta.len();
            let mut existing = tokio::fs::File::open(&part_path).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = existing.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
        }

        let mut request = self.client.get_object().bucket(&self.bucket).key(key);
        if offset > 0 {
            request = request.range(format!("bytes={offset}-"));
        }
        let response = match request.send().await {
            Ok(r) => Some(r),
            // the whole object is already in the .part file
            Err(err) if offset > 0 && is_range_not_satisfiable(&err) => None,
            Err(err) => return Err(err).with_context(|| format!("GetObject {key}")),
        };

        if let Some(response) = response {
            let total = response
                .content_length()
                .and_then(|len| u64::try_from(len).ok())
                .map(|remaining| offset + remaining);
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part_path)
                .await?;
            let mut body = response.body;
            let mut done = offset;
            loop {
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => bail!("download cancelled"),
                    chunk = body.try_next() => chunk.context("reading object body")?,
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
            // a corrupt part would resume corrupt forever — start over next time
            let _ = tokio::fs::remove_file(&part_path).await;
            bail!("sha256 mismatch for {key}: expected {sha256}, got {actual}");
        }
        tokio::fs::rename(&part_path, dest).await?;
        Ok(())
    }
}

fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

fn is_range_not_satisfiable<E, R>(err: &aws_sdk_s3::error::SdkError<E, R>) -> bool
where
    E: std::fmt::Debug,
    R: std::fmt::Debug,
{
    match err.raw_response() {
        Some(_) => format!("{err:?}").contains("416"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_path_appends_suffix() {
        assert_eq!(
            part_path(Path::new("/tmp/x/setup.exe")),
            Path::new("/tmp/x/setup.exe.part")
        );
    }
}

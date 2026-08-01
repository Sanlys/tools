//! S3 client wired up from the environment variables a rook-ceph
//! `ObjectBucketClaim` (OBC) produces.
//!
//! A tool declares a bucket by including the `bucket` block in its Helm
//! values (see `deploy/charts/tool-library/templates/objectbucketclaim.yaml`
//! and `docs/s3-buckets.md`). That renders an `ObjectBucketClaim` custom
//! resource; rook-ceph's bucket provisioner watches it, creates the actual
//! bucket + a dedicated Ceph user scoped to just that bucket, and writes:
//!
//! - a ConfigMap (same name as the OBC) with `BUCKET_HOST`, `BUCKET_PORT`,
//!   `BUCKET_NAME`, `BUCKET_REGION`, `BUCKET_SUBREGION`
//! - a Secret (same name) with `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`
//!
//! The library chart projects both into the container's environment via
//! `envFrom`, so this module just reads those exact, well-known names. There
//! is no separate "platform API" service involved in provisioning -- the
//! CRD and rook's own operator do all of it.

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::Client;
use std::env;

#[derive(Debug, thiserror::Error)]
pub enum S3ConfigError {
    #[error("missing required env var {0} (expected from the rook-ceph ObjectBucketClaim output)")]
    MissingEnv(&'static str),
    #[error("BUCKET_PORT is not a valid port number: {0}")]
    InvalidPort(String),
}

#[derive(Debug, Clone)]
pub struct S3Config {
    pub bucket_name: String,
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl S3Config {
    /// Reads `BUCKET_HOST`/`BUCKET_PORT`/`BUCKET_NAME`/`BUCKET_REGION` and
    /// `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` from the environment.
    ///
    /// `BUCKET_REGION` is optional (Ceph RGW ignores region in practice); it
    /// defaults to `"us-east-1"`, which is also the aws-sdk-s3 default and
    /// avoids surprising clients that assume a region is always set.
    ///
    /// Scheme is chosen from `BUCKET_PORT`: 443 implies `https`, anything
    /// else implies `http` (rook-ceph's in-cluster RGW service is plain HTTP
    /// by default). Set `S3_FORCE_HTTPS=1` to override.
    pub fn from_env() -> Result<Self, S3ConfigError> {
        let host = require_env("BUCKET_HOST")?;
        let port_str = require_env("BUCKET_PORT")?;
        let port: u16 = port_str
            .parse()
            .map_err(|_| S3ConfigError::InvalidPort(port_str.clone()))?;
        let bucket_name = require_env("BUCKET_NAME")?;
        let access_key_id = require_env("AWS_ACCESS_KEY_ID")?;
        let secret_access_key = require_env("AWS_SECRET_ACCESS_KEY")?;
        let region = env::var("BUCKET_REGION").unwrap_or_else(|_| "us-east-1".to_string());

        let force_https = env::var("S3_FORCE_HTTPS").as_deref() == Ok("1");
        let scheme = if force_https || port == 443 {
            "https"
        } else {
            "http"
        };
        let endpoint = format!("{scheme}://{host}:{port}");

        Ok(Self {
            bucket_name,
            endpoint,
            region,
            access_key_id,
            secret_access_key,
        })
    }
}

fn require_env(key: &'static str) -> Result<String, S3ConfigError> {
    env::var(key).map_err(|_| S3ConfigError::MissingEnv(key))
}

/// Builds an `aws-sdk-s3` client from an [`S3Config`]. Ceph RGW requires
/// path-style bucket addressing, which is set unconditionally here.
pub fn build_client(cfg: &S3Config) -> Client {
    let credentials = Credentials::new(
        cfg.access_key_id.clone(),
        cfg.secret_access_key.clone(),
        None,
        None,
        "rook-ceph-obc",
    );
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(cfg.region.clone()))
        .endpoint_url(&cfg.endpoint)
        .credentials_provider(credentials)
        .force_path_style(true)
        .build();
    Client::from_conf(config)
}

/// Convenience: read config from the environment and build a client in one
/// call. What most tools want in `main()`.
pub fn client_from_env() -> Result<(Client, S3Config), S3ConfigError> {
    let cfg = S3Config::from_env()?;
    let client = build_client(&cfg);
    Ok((client, cfg))
}

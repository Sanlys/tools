# S3 buckets

A tool declares a bucket by setting `bucket.enabled: true` in its
`values.yaml` -- there's no separate provisioning API or platform
controller. Mechanically:

1. `deploy/charts/tool-library`'s `objectbucketclaim.yaml` template renders
   a rook-ceph **`ObjectBucketClaim`** (OBC) named `<fullname>-bucket`.
2. Rook-ceph's own bucket provisioner (already running as part of your
   rook-ceph install) watches OBCs, creates the real bucket in Ceph RGW
   plus a **dedicated Ceph user scoped to just that bucket**, and writes:
   - a **ConfigMap** (same name as the OBC) with `BUCKET_HOST`,
     `BUCKET_PORT`, `BUCKET_NAME`, `BUCKET_REGION`
   - a **Secret** (same name) with `AWS_ACCESS_KEY_ID`,
     `AWS_SECRET_ACCESS_KEY`
3. The chart's Deployment template projects both into the container via
   `envFrom`.
4. `crates/adapters/s3::S3Config::from_env()` reads those exact env var
   names and builds an `aws-sdk-s3::Client` pointed at Ceph RGW
   (path-style addressing, which RGW requires).

So "declare a bucket's existence" is: add the CRD to the chart (already
done, gated by `bucket.enabled`), and the real bucket + scoped credentials
appear automatically once that CRD lands in the cluster. Nothing else to
run or configure.

## Values

```yaml
bucket:
  enabled: true
  bucketName: tools-my-tool   # fixed name -- one namespace per tool already makes it unique
  storageClassName: rook-ceph-bucket   # must match your rook-ceph bucket StorageClass
```

## Credential scoping

One bucket-scoped identity per app, per bucket -- this is rook's default
OBC behavior, not something this platform adds on top. There's no shared
platform-wide S3 credential anywhere.

## Local development

There's no bucket outside the cluster. `docker-compose.yml` at the repo
root runs a local MinIO with a `hello` bucket already created --
`.env.example` has the matching `BUCKET_HOST`/`BUCKET_PORT`/`BUCKET_NAME`/
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`. See
`docs/local-development.md`. `s3_adapter::S3Config::from_env()`
will just return a clear "missing env var" error if they're unset, rather
than hanging or connecting somewhere unexpected.

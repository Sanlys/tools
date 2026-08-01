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
  bucketName: my-tool       # generateBucketName prefix -- rook appends a random suffix
  storageClassName: rook-ceph-bucket   # must match your rook-ceph bucket StorageClass
```

`generateBucketName` (rather than a fixed `bucketName` in the OBC spec) is
used deliberately so bucket names don't collide across namespaces/
environments. If a tool genuinely needs a stable, predictable bucket name,
change `templates/_objectbucketclaim.tpl` to use `bucketName` instead for
that one chart.

## Credential scoping

One bucket-scoped identity per app, per bucket -- this is rook's default
OBC behavior, not something this platform adds on top. There's no shared
platform-wide S3 credential anywhere.

## Local development

There's no bucket outside the cluster. Either run against a local
MinIO/localstack and export `BUCKET_HOST`/`BUCKET_PORT`/`BUCKET_NAME`/
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` yourself, or stub out the S3
calls in your tool while developing -- `s3_adapter::S3Config::from_env()`
will just return a clear "missing env var" error if they're unset, rather
than hanging or connecting somewhere unexpected.

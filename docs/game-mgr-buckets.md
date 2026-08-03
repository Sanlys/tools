# game-mgr: uploading game data

Games are **server-stored definitions** created from the client UI — no
title ever appears in the codebase. Adding a game is two steps: upload its
files **with `.sha256` sidecars** to the bucket, then fill in the client's
**➕ Add game** form. The scan reads the sidecars instead of streaming
gigabytes, so registration is virtually instant; every other machine picks
the game up on the next catalog refresh.

This doc covers the upload side. See `docs/s3-buckets.md` for how
`game-mgr`'s bucket itself is provisioned (a rook-ceph `ObjectBucketClaim`,
same mechanism every bucket-owning tool in this repo uses) and why the
desktop client never holds its own bucket credentials.

## Getting admin credentials to upload with

Unlike the original standalone `game-mgr` (where you ran your own Ceph RGW
and generated credentials by hand), the bucket here is provisioned
automatically and its credentials live in a Kubernetes Secret rook-ceph
creates for you — `tools-game-mgr-bucket` in the `tools-game-mgr` namespace
(same name as the `ObjectBucketClaim`, see `docs/s3-buckets.md`):

```sh
kubectl -n tools-game-mgr get secret tools-game-mgr-bucket -o json \
  | jq -r '.data | map_values(@base64d)'
# -> AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY
kubectl -n tools-game-mgr get configmap tools-game-mgr-bucket -o json \
  | jq -r '.data'
# -> BUCKET_HOST, BUCKET_PORT, BUCKET_NAME, BUCKET_REGION
```

Point `mc` (the MinIO client, works fine against Ceph RGW) at those:

```sh
mc alias set gm http://<BUCKET_HOST>:<BUCKET_PORT> <AWS_ACCESS_KEY_ID> <AWS_SECRET_ACCESS_KEY>
```

This is the *only* place bucket credentials exist outside the cluster --
keep them off gaming machines. The desktop client never needs them: it
lists prefixes and downloads files through `game-mgr-backend`'s
`/api/v1/artifacts/scan` and `/api/v1/artifacts/download-url` endpoints
(short-lived presigned URLs), which is also why installing/scanning always
needs `server_url` configured and a signed-in session.

## Bucket layout convention

```
gog/<game-id>/                      # GOG offline installers, stored AS-IS
├── setup_<name>_<ver>.exe
├── setup_<name>_<ver>-1.bin
└── setup_<name>_<ver>-2.bin
gog/skyrim/skse/<skse>.7z           # SKSE archive for the skyrim-modded class
switch/                             # arrives with M4
├── keys/{prod.keys,title.keys}
├── firmware/<version>.zip
└── roms/<game-id>.nsp
```

`<game-id>` is the slug you choose in the form (`bg3`, `skyrim`, …) — lowercase
letters, digits, dashes; never renamed once shipped (it names Syncthing
folders, install dirs and stats).

## Example: Baldur's Gate 3

1. **Get the offline installers** from your GOG library (gog.com → BG3 →
   download offline backup installers): one `.exe` plus `.bin` parts. Do not
   run or repack them — they are stored as-is and extracted with
   `innoextract` at install time.

2. **Hash + upload** under `gog/bg3/`, keeping the original filenames, with a
   `.sha256` sidecar next to every file (`sha256sum` output format):

   ```sh
   for f in setup_baldurs_gate_3_* patches/*.exe; do sha256sum "$f" > "$f.sha256"; done
   mc cp --recursive setup_baldurs_gate_3_* patches gm/tools-game-mgr/gog/bg3/
   ```

   Files without a sidecar still work — the client falls back to streaming
   + hashing them at submit time (slow for big files) via the same
   presigned-download mechanism, not a direct bucket read.

3. **Define the game in the client**: ➕ Add game → fill in class/id/title/
   version/executable/watch-exes/saves-path, enter the bucket prefix
   (`gog/bg3/`) and **Scan**: every file under the prefix appears (via
   `/api/v1/artifacts/scan`) with its size, sidecar status and a suggested
   role — **base** (main installer, always installed), **patch** /
   **dlc** (optional at install time), or **ignore** (not part of the
   install). Adjust roles as needed and **Submit**.

## Editing a definition / updating a game

Every game row has an **✏ Edit** button: the form opens pre-filled (id is
locked), existing artifacts and roles included. Change fields, optionally
re-scan a prefix to replace the file list, and **Save changes** — the
definition is replaced wholesale (PUT upsert). For a new game version:
upload the new installer parts (+ sidecars), Edit the game, bump
**version**, re-scan, save. Machines on the old version show "update
available"; saves live in Syncthing folders and survive. Old objects can be
deleted once no machine needs them.

## Notes

- Verification is end-to-end: installs re-hash every downloaded file
  against the pinned sha256, resume partial downloads (via `Range` against
  a fresh presigned URL if the previous one expired), and abort + clear the
  partial file on mismatch.
- Clients cache the catalog locally, so the library still works offline
  with the last-known definitions.
- A definition whose class this client doesn't know (e.g. created by a
  newer build) is skipped with a warning, never an error.

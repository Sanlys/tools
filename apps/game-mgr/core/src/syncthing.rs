//! Driver for the local Syncthing instance's REST API (PLAN.md §5).
//!
//! M2 scope: ensure folders (id/label/path/devices/ignores), pause/resume,
//! completion and conflict reporting. Peers are the devices already present
//! in the local Syncthing config ("manual peers"); server-registry mesh
//! auto-add arrives in M3.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use crate::config::SyncthingConfig;
use crate::game::SyncFolderSpec;

pub struct SyncthingClient {
    base: reqwest::Url,
    api_key: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SystemStatus {
    #[serde(rename = "myID")]
    pub my_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceConfig {
    #[serde(rename = "deviceID")]
    pub device_id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FolderConfig {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub path: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub devices: Vec<FolderDevice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FolderDevice {
    #[serde(rename = "deviceID")]
    pub device_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Completion {
    pub completion: f64,
}

#[derive(Debug, Clone)]
pub struct FolderHealth {
    pub folder_id: String,
    pub completion_pct: f64,
    pub paused: bool,
    pub conflict_files: Vec<String>,
}

impl SyncthingClient {
    pub fn new(url: &str, api_key: &str) -> Result<Self> {
        Ok(Self {
            base: reqwest::Url::parse(url).context("parsing syncthing url")?,
            api_key: api_key.to_string(),
            http: reqwest::Client::new(),
        })
    }

    /// Explicit config, or autodetect address + API key from the local
    /// Syncthing `config.xml`; verifies connectivity.
    pub async fn connect(cfg: &SyncthingConfig) -> Result<Self> {
        let client = match (&cfg.url, &cfg.api_key) {
            (Some(url), Some(key)) => Self::new(url, key)?,
            _ => {
                let (url, key) = autodetect_config()
                    .context("autodetecting syncthing config.xml (set GM_SYNCTHING_URL and GM_SYNCTHING_API_KEY to skip)")?;
                Self::new(&url, &key)?
            }
        };
        client
            .status()
            .await
            .context("contacting local syncthing")?;
        Ok(client)
    }

    fn url(&self, path: &str) -> reqwest::Url {
        let mut url = self.base.clone();
        url.set_path(path);
        url
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .get(self.url(path))
            .header("X-API-Key", &self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json().await?)
    }

    async fn send_json(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<(&str, &str)>,
        body: serde_json::Value,
    ) -> Result<()> {
        let mut url = self.url(path);
        if let Some((k, v)) = query {
            url.query_pairs_mut().append_pair(k, v);
        }
        self.http
            .request(method, url)
            .header("X-API-Key", &self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn status(&self) -> Result<SystemStatus> {
        self.get_json("/rest/system/status").await
    }

    pub async fn devices(&self) -> Result<Vec<DeviceConfig>> {
        self.get_json("/rest/config/devices").await
    }

    /// Peer device IDs = configured devices minus ourselves (manual peers).
    pub async fn peer_device_ids(&self) -> Result<Vec<String>> {
        let my_id = self.status().await?.my_id;
        Ok(self
            .devices()
            .await?
            .into_iter()
            .map(|d| d.device_id)
            .filter(|id| *id != my_id)
            .collect())
    }

    pub async fn folder(&self, id: &str) -> Result<Option<FolderConfig>> {
        let response = self
            .http
            .get(self.url(&format!("/rest/config/folders/{id}")))
            .header("X-API-Key", &self.api_key)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(response.error_for_status()?.json().await?))
    }

    /// Create or update a folder: same `folder_id` everywhere, per-device
    /// `local_path`, shared with all peers, ignore patterns asserted.
    pub async fn ensure_folder(&self, spec: &SyncFolderSpec) -> Result<()> {
        // A repointed local path (a game-mgr code update changing how a
        // path is computed -- see e.g. GogGame::resolve_save_path -- or a
        // settings edit) must not silently start over: without migrating
        // the actual files first, Syncthing sees an empty directory at the
        // new path and re-downloads everything from peers, even though the
        // same content already exists locally at the old one.
        let existing = self.folder(&spec.folder_id).await?;
        if let Some(existing) = &existing {
            let existing_path = PathBuf::from(&existing.path);
            if existing_path != spec.local_path {
                tracing::warn!(
                    folder = %spec.folder_id,
                    old = %existing.path,
                    new = %spec.local_path.display(),
                    "moving syncthing folder to a new local path",
                );
                let (old, new) = (existing_path, spec.local_path.clone());
                if let Err(err) =
                    tokio::task::spawn_blocking(move || migrate_folder_contents(&old, &new))
                        .await
                        .context("migrate task panicked")?
                {
                    // Best-effort: log and continue rather than fail the
                    // whole install/launch over it -- Syncthing will just
                    // fall back to re-transferring from a peer, which is
                    // the pre-existing (if wasteful) behavior this is
                    // trying to improve on, not a new failure mode.
                    tracing::warn!(
                        folder = %spec.folder_id,
                        %err,
                        "could not migrate synced folder contents to its new path -- \
                         Syncthing will likely re-download instead",
                    );
                }
            }
        }

        std::fs::create_dir_all(&spec.local_path)
            .with_context(|| format!("creating {}", spec.local_path.display()))?;

        let my_id = self.status().await?.my_id;
        let mut device_ids = self.peer_device_ids().await?;
        device_ids.insert(0, my_id);
        let devices: Vec<serde_json::Value> = device_ids
            .iter()
            .map(|id| json!({ "deviceID": id }))
            .collect();

        let body = json!({
            "id": spec.folder_id,
            "label": spec.label,
            "path": spec.local_path.to_string_lossy(),
            "type": "sendreceive",
            "devices": devices,
        });
        match existing {
            None => {
                self.send_json(reqwest::Method::POST, "/rest/config/folders", None, body)
                    .await
                    .context("creating syncthing folder")?;
            }
            Some(_) => {
                self.send_json(
                    reqwest::Method::PATCH,
                    &format!("/rest/config/folders/{}", spec.folder_id),
                    None,
                    json!({
                        "label": spec.label,
                        "path": spec.local_path.to_string_lossy(),
                        "devices": devices,
                    }),
                )
                .await
                .context("updating syncthing folder")?;
            }
        }

        if !spec.stignore.is_empty() {
            self.send_json(
                reqwest::Method::POST,
                "/rest/db/ignores",
                Some(("folder", &spec.folder_id)),
                json!({ "ignore": spec.stignore }),
            )
            .await
            .context("writing folder ignore patterns")?;
        }
        Ok(())
    }

    pub async fn completion(&self, folder_id: &str) -> Result<f64> {
        let mut url = self.url("/rest/db/completion");
        url.query_pairs_mut().append_pair("folder", folder_id);
        let response = self
            .http
            .get(url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<Completion>().await?.completion)
    }

    pub async fn set_paused(&self, folder_id: &str, paused: bool) -> Result<()> {
        self.send_json(
            reqwest::Method::PATCH,
            &format!("/rest/config/folders/{folder_id}"),
            None,
            json!({ "paused": paused }),
        )
        .await
    }

    /// Folder health for sync-status reporting: completion + conflicts.
    /// Not called from `apps/game-mgr/backend` yet -- the `sync_status`
    /// table it would feed (`apps/game-mgr/backend/migrations/0001_init.sql`)
    /// has no ingest or read route wired up on either side of the API yet,
    /// see `apps/game-mgr/frontend`'s module doc comment.
    pub async fn folder_health(&self, spec: &SyncFolderSpec) -> Result<FolderHealth> {
        let folder = self.folder(&spec.folder_id).await?;
        let completion = self.completion(&spec.folder_id).await.unwrap_or(0.0);
        Ok(FolderHealth {
            folder_id: spec.folder_id.clone(),
            completion_pct: completion,
            paused: folder.map(|f| f.paused).unwrap_or(false),
            conflict_files: scan_conflicts(&spec.local_path),
        })
    }
}

/// Move `old`'s contents into `new` when a sync folder's local path just
/// changed, so Syncthing finds the same content already present at the new
/// path instead of an empty directory it would otherwise have to
/// re-transfer wholesale from a peer. Best-effort and conservative:
/// - no-op if `old` doesn't exist or is empty (nothing to migrate)
/// - refuses (rather than guessing which is authoritative) if `new` already
///   has content of its own
/// - a plain rename when possible (instant, same filesystem); falls back to
///   copy + remove when `old` and `new` are on different filesystems/drives
///   (a rename can't cross those)
fn migrate_folder_contents(old: &Path, new: &Path) -> anyhow::Result<()> {
    if !old.is_dir() || !dir_has_entries(old)? {
        return Ok(());
    }
    if new.is_dir() {
        if dir_has_entries(new)? {
            anyhow::bail!(
                "both {} and {} already have content -- not migrating automatically; \
                 move the data by hand and remove whichever copy is stale",
                old.display(),
                new.display()
            );
        }
        // empty -- remove so the rename below can claim the (otherwise
        // already-occupied) destination path
        std::fs::remove_dir(new)
            .with_context(|| format!("removing empty destination {}", new.display()))?;
    } else if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    if std::fs::rename(old, new).is_ok() {
        return Ok(());
    }
    copy_dir_recursive(old, new)
        .with_context(|| format!("copying {} to {}", old.display(), new.display()))?;
    std::fs::remove_dir_all(old)
        .with_context(|| format!("removing migrated-from {}", old.display()))?;
    Ok(())
}

fn dir_has_entries(dir: &Path) -> anyhow::Result<bool> {
    Ok(std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .next()
        .is_some())
}

/// Recursively copy `src`'s contents into `dst` -- the cross-filesystem
/// fallback for [`migrate_folder_contents`], which a plain rename can't
/// handle. `dst` is created if it doesn't exist; existing files under it are
/// overwritten (mirrors `steps::extract::merge_dir`'s semantics, though this
/// one doesn't need to merge onto a populated tree in practice since
/// `migrate_folder_contents` only calls it once `dst` is confirmed empty).
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Find `*.sync-conflict-*` files under a folder (bounded walk).
pub fn scan_conflicts(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 50_000 {
                return found; // bounded: huge mod folders
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != ".stversions" {
                    stack.push(path);
                }
            } else if name.contains(".sync-conflict-") {
                found.push(path.to_string_lossy().into_owned());
            }
        }
    }
    found
}

/// Parse gui address + api key out of the local Syncthing config.xml.
pub fn autodetect_config() -> Result<(String, String)> {
    let candidates = config_xml_candidates();
    for candidate in &candidates {
        if candidate.is_file() {
            let xml = std::fs::read_to_string(candidate)?;
            return parse_config_xml(&xml)
                .with_context(|| format!("parsing {}", candidate.display()));
        }
    }
    // List every path actually checked -- "no syncthing config.xml found"
    // alone gives no way to tell a real absence apart from this list simply
    // being wrong for how Syncthing was installed (see this fn's other
    // comments); GM_SYNCTHING_URL/GM_SYNCTHING_API_KEY (SyncthingConfig)
    // remain the escape hatch either way.
    let tried = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "no syncthing config.xml found (checked: {tried}) -- is syncthing installed and \
         started once? set GM_SYNCTHING_URL and GM_SYNCTHING_API_KEY to skip autodetection"
    )
}

/// Every path Syncthing's own installers/packages are known to use for
/// `config.xml`, across platforms and packaging -- listed low-confidence
/// (least common) to high, since [`autodetect_config`] returns the first
/// hit. Not just XDG on Linux: the current stable installer default
/// per-OS is `%LocalAppData%\Syncthing` on Windows (`config_local_dir`,
/// *not* `config_dir`, which is Roaming AppData and never holds it --
/// `dirs::state_dir()` is also unconditionally `None` on Windows, so the
/// pre-existing `state_dir`-based candidate silently checked nothing
/// there at all) and `~/Library/Application Support/Syncthing` on macOS.
fn config_xml_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(state) = dirs::state_dir() {
        candidates.push(state.join("syncthing/config.xml"));
    }
    if let Some(config) = dirs::config_dir() {
        candidates.push(config.join("syncthing/config.xml"));
    }
    if let Some(config_local) = dirs::config_local_dir() {
        candidates.push(config_local.join("Syncthing/config.xml"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/state/syncthing/config.xml"));
        candidates.push(home.join(".config/syncthing/config.xml"));
    }
    candidates
}

/// Extract `<gui><address>` and `<gui><apikey>` (quick-xml event walk).
pub fn parse_config_xml(xml: &str) -> Result<(String, String)> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut in_gui = false;
    let mut current: Option<String> = None;
    let (mut address, mut apikey) = (None, None);

    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match name.as_str() {
                    "gui" => in_gui = true,
                    "address" | "apikey" if in_gui => current = Some(name),
                    _ => current = None,
                }
            }
            Event::Text(t) if in_gui => {
                let text = t.unescape()?.into_owned();
                match current.as_deref() {
                    Some("address") => address = Some(text),
                    Some("apikey") => apikey = Some(text),
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "gui" {
                    in_gui = false;
                }
                current = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let address = address.ok_or_else(|| anyhow::anyhow!("config.xml has no <gui><address>"))?;
    let apikey = apikey.ok_or_else(|| anyhow::anyhow!("config.xml has no <gui><apikey>"))?;
    Ok((format!("http://{address}"), apikey))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gui_address_and_apikey() {
        let xml = r#"<configuration version="37">
            <folder id="x"><device id="ignored"></device></folder>
            <gui enabled="true" tls="false">
                <address>127.0.0.1:8384</address>
                <apikey>secret-key</apikey>
                <theme>default</theme>
            </gui>
        </configuration>"#;
        let (url, key) = parse_config_xml(xml).unwrap();
        assert_eq!(url, "http://127.0.0.1:8384");
        assert_eq!(key, "secret-key");
    }

    #[test]
    fn missing_apikey_is_an_error() {
        let xml = "<configuration><gui><address>127.0.0.1:8384</address></gui></configuration>";
        assert!(parse_config_xml(xml).is_err());
    }

    #[test]
    fn conflict_scan_finds_marker_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("save.dat"), b"x").unwrap();
        std::fs::write(
            dir.path()
                .join("sub/save.sync-conflict-20260610-101010-ABCDEFG.dat"),
            b"x",
        )
        .unwrap();
        let conflicts = scan_conflicts(dir.path());
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("sync-conflict"));
    }

    #[test]
    fn migrate_moves_content_to_the_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("save.dat"), b"hello").unwrap();
        std::fs::write(old.join("sub/nested.dat"), b"world").unwrap();

        migrate_folder_contents(&old, &new).unwrap();

        assert!(!old.exists(), "old path should be gone after migration");
        assert_eq!(std::fs::read(new.join("save.dat")).unwrap(), b"hello");
        assert_eq!(std::fs::read(new.join("sub/nested.dat")).unwrap(), b"world");
    }

    #[test]
    fn migrate_is_a_noop_when_old_path_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("never-existed");
        let new = dir.path().join("new");
        migrate_folder_contents(&old, &new).unwrap();
        assert!(!new.exists(), "nothing to migrate -- new shouldn't appear");
    }

    #[test]
    fn migrate_is_a_noop_when_old_path_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        migrate_folder_contents(&old, &new).unwrap();
        assert!(!new.exists());
        assert!(old.exists(), "an empty old dir is left alone, not deleted");
    }

    #[test]
    fn migrate_refuses_to_clobber_content_already_at_the_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.dat"), b"old-content").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("b.dat"), b"new-content").unwrap();

        let err = migrate_folder_contents(&old, &new).unwrap_err().to_string();
        assert!(err.contains("already have content"), "{err}");
        // neither side touched -- caller (ensure_folder) falls back to
        // letting Syncthing re-transfer rather than losing anything
        assert_eq!(std::fs::read(old.join("a.dat")).unwrap(), b"old-content");
        assert_eq!(std::fs::read(new.join("b.dat")).unwrap(), b"new-content");
    }

    #[test]
    fn migrate_replaces_an_empty_destination() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.dat"), b"content").unwrap();
        std::fs::create_dir_all(&new).unwrap(); // pre-created empty, e.g. by an earlier ensure_folder call

        migrate_folder_contents(&old, &new).unwrap();
        assert_eq!(std::fs::read(new.join("a.dat")).unwrap(), b"content");
    }

    #[test]
    fn copy_dir_recursive_fallback_matches_rename() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(src.join("a/b")).unwrap();
        std::fs::write(src.join("top.dat"), b"1").unwrap();
        std::fs::write(src.join("a/mid.dat"), b"2").unwrap();
        std::fs::write(src.join("a/b/deep.dat"), b"3").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();

        assert_eq!(std::fs::read(dst.join("top.dat")).unwrap(), b"1");
        assert_eq!(std::fs::read(dst.join("a/mid.dat")).unwrap(), b"2");
        assert_eq!(std::fs::read(dst.join("a/b/deep.dat")).unwrap(), b"3");
        // src untouched -- migrate_folder_contents itself does the removal
        assert!(src.join("top.dat").is_file());
    }
}

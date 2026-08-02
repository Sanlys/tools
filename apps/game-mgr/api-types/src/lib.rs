//! Shared wire types between `game-mgr-backend`, `game-mgr-core`,
//! `game-mgr-client` and `game-mgr-frontend`'s `GameMgrPanel`.
//!
//! Every request/response body exchanged over `/api/v1` lives here so no
//! consumer can drift from the backend. Auth config discovery is handled by
//! `auth_adapter::AuthConfig`/`config_route` instead of a type here -- there
//! is no bespoke SPA config endpoint anymore now that the frontend is an
//! egui panel using the same platform-wide auth pattern every other tool
//! uses.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Users & profiles (PLAN.md §8.0)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserDto {
    pub id: Uuid,
    pub sub: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileDto {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeResponse {
    pub user: UserDto,
    /// Profiles owned by the calling user.
    pub profiles: Vec<ProfileDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateProfileRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameProfileRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferProfileRequest {
    pub to_user_id: Uuid,
}

// ---------------------------------------------------------------------------
// Machines & catalog (PLAN.md §8.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterMachineRequest {
    pub name: String,
    pub os: Option<String>,
    pub client_version: Option<String>,
    /// Local Syncthing device ID — drives mesh peer discovery (PLAN.md §5).
    pub syncthing_device_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineDto {
    pub id: Uuid,
    pub name: String,
    pub os: Option<String>,
    pub client_version: Option<String>,
    pub syncthing_device_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------
// Game definitions — server-stored data, created from the client UI.
// Classes (gog, switch, …) are code; titles are rows (PLAN.md §4.1).
// ---------------------------------------------------------------------------

/// What an artifact contributes to an install. `base` is mandatory;
/// `patch`/`dlc` groups are user-selectable at install time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactRole {
    #[default]
    Base,
    Patch,
    Dlc,
}

impl ArtifactRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactRole::Base => "base",
            ArtifactRole::Patch => "patch",
            ArtifactRole::Dlc => "dlc",
        }
    }
}

/// One bucket object a game needs, with its pinned hash. Hashes come from
/// `<key>.sha256` sidecar objects in the bucket (uploaded next to each
/// file); the client streams + hashes only as a fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDto {
    pub bucket_key: String,
    /// Lowercase hex sha256 of the object.
    pub sha256: String,
    pub size: Option<i64>,
    #[serde(default)]
    pub role: ArtifactRole,
    /// Distinct DLC name (set in the picker). Meaningful only when
    /// `role == Dlc`; lets the install dialog list each DLC separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dlc_name: Option<String>,
}

/// A title as stored on the server and consumed by clients. `config` is the
/// class-specific block (e.g. `GogConfig`), opaque to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameDefinition {
    pub id: String,
    pub title: String,
    pub class: String,
    pub version: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub artifacts: Vec<ArtifactDto>,
}

/// `PUT /games/{id}` body — full upsert, the id comes from the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertGameRequest {
    pub title: String,
    pub class: String,
    pub version: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub artifacts: Vec<ArtifactDto>,
}

/// Definition + aggregates derived from sessions (`GET /games`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameDto {
    #[serde(flatten)]
    pub definition: GameDefinition,
    pub total_playtime_s: i64,
    pub session_count: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_played: Option<OffsetDateTime>,
}

// ---------------------------------------------------------------------------
// Stats ingest (PLAN.md §8.2) — spool-safe, idempotent per item
// ---------------------------------------------------------------------------

/// How a play session ended — used to spot crashes in the session browser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    /// The spawned process exited and we observed its status.
    #[default]
    Exited,
    /// The watched process tree drained (launcher/Wine detachment) — no
    /// direct exit status available.
    TreeDrained,
    /// Recovered from a client crash: closed at the last persisted tick.
    Recovered,
}

impl SessionEndReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionEndReason::Exited => "exited",
            SessionEndReason::TreeDrained => "tree_drained",
            SessionEndReason::Recovered => "recovered",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "tree_drained" => SessionEndReason::TreeDrained,
            "recovered" => SessionEndReason::Recovered,
            _ => SessionEndReason::Exited,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionDto {
    /// Client-generated UUID — the idempotency key.
    pub id: Uuid,
    pub machine_id: Uuid,
    pub profile_id: Uuid,
    pub game_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ended_at: OffsetDateTime,
    pub duration_s: i32,
    /// Exit code of the directly spawned process, when observable.
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub end_reason: SessionEndReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionsBatchRequest {
    pub sessions: Vec<SessionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDto {
    /// Client-generated UUID — the idempotency key.
    pub client_event_id: Uuid,
    pub machine_id: Uuid,
    pub profile_id: Option<Uuid>,
    pub game_id: Option<String>,
    /// `launch`, `install_started`, `install_finished`, `install_failed`, …
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsBatchRequest {
    pub events: Vec<EventDto>,
}

/// One bad row must never poison a spool batch: outcomes are per item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchResponse {
    pub inserted: u32,
    pub duplicates: u32,
    #[serde(default)]
    pub errors: Vec<BatchItemError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchItemError {
    /// The item's idempotency key (session id / client_event_id).
    pub id: Uuid,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_role_defaults_to_base_on_old_payloads() {
        let a: ArtifactDto =
            serde_json::from_str(r#"{"bucket_key":"gog/x/setup.exe","sha256":"ab","size":1}"#)
                .unwrap();
        assert_eq!(a.role, ArtifactRole::Base);
        // old payloads have no dlc_name; it must default to None and not
        // serialize back out when absent.
        assert_eq!(a.dlc_name, None);
        assert!(!serde_json::to_string(&a).unwrap().contains("dlc_name"));
        for role in [ArtifactRole::Base, ArtifactRole::Patch, ArtifactRole::Dlc] {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json.trim_matches('"'), role.as_str());
            let back: ArtifactRole = serde_json::from_str(&json).unwrap();
            assert_eq!(back, role);
        }
    }

    #[test]
    fn end_reason_roundtrips_and_defaults() {
        assert_eq!(
            SessionEndReason::parse("tree_drained"),
            SessionEndReason::TreeDrained
        );
        assert_eq!(
            SessionEndReason::parse("recovered"),
            SessionEndReason::Recovered
        );
        assert_eq!(
            SessionEndReason::parse("anything"),
            SessionEndReason::Exited
        );
        for r in [
            SessionEndReason::Exited,
            SessionEndReason::TreeDrained,
            SessionEndReason::Recovered,
        ] {
            assert_eq!(SessionEndReason::parse(r.as_str()), r);
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(json.trim_matches('"'), r.as_str());
        }
    }

    #[test]
    fn session_dto_uses_rfc3339_timestamps() {
        let json = r#"{
            "id": "6f2e1a3e-1111-4222-8333-444455556666",
            "machine_id": "6f2e1a3e-1111-4222-8333-444455557777",
            "profile_id": "6f2e1a3e-1111-4222-8333-444455558888",
            "game_id": "bg3",
            "started_at": "2026-06-10T18:00:00Z",
            "ended_at": "2026-06-10T19:30:00Z",
            "duration_s": 5400
        }"#;
        let s: SessionDto = serde_json::from_str(json).unwrap();
        assert_eq!(s.game_id, "bg3");
        assert_eq!(s.duration_s, 5400);
        let back = serde_json::to_value(&s).unwrap();
        assert!(back["started_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-06-10T18:00:00"));
    }

    #[test]
    fn event_payload_defaults_to_null() {
        let json = r#"{
            "client_event_id": "6f2e1a3e-1111-4222-8333-444455556666",
            "machine_id": "6f2e1a3e-1111-4222-8333-444455557777",
            "kind": "launch",
            "occurred_at": "2026-06-10T18:00:00Z"
        }"#;
        let e: EventDto = serde_json::from_str(json).unwrap();
        assert!(e.payload.is_null());
        assert!(e.profile_id.is_none());
        assert!(e.game_id.is_none());
    }

    #[test]
    fn batch_response_roundtrips_with_errors() {
        let r = BatchResponse {
            inserted: 2,
            duplicates: 1,
            errors: vec![BatchItemError {
                id: Uuid::nil(),
                reason: "unknown game".into(),
            }],
        };
        let back: BatchResponse =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}

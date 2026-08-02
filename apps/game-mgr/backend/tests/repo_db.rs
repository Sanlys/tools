//! Granular DB-backed tests for the repo layer (PLAN.md §15).

mod common;

use common::TestDb;
use game_mgr_api_types::{ArtifactDto, RegisterMachineRequest, SessionDto, UpsertGameRequest};
use game_mgr_backend::repo;
use game_mgr_backend::repo::ingest::Outcome;
use game_mgr_backend::repo::profiles::{OwnedUpdate, TransferOutcome};
use time::OffsetDateTime;
use uuid::Uuid;

#[tokio::test]
async fn user_upsert_is_idempotent() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let first = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let second = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    assert_eq!(first.id, second.id, "same sub must map to the same user");

    let other = repo::users::upsert_by_sub(&db.pool, "bob").await.unwrap();
    assert_ne!(first.id, other.id);
    assert_eq!(repo::users::list(&db.pool).await.unwrap().len(), 2);

    db.cleanup().await;
}

#[tokio::test]
async fn machine_upsert_updates_fields_but_keeps_registrar() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let alice = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let bob = repo::users::upsert_by_sub(&db.pool, "bob").await.unwrap();
    let machine_id = Uuid::new_v4();

    let m1 = repo::machines::upsert(
        &db.pool,
        machine_id,
        &RegisterMachineRequest {
            name: "desktop".into(),
            os: Some("arch".into()),
            client_version: Some("0.1.0".into()),
            syncthing_device_id: Some("DEVICE-A".into()),
        },
        alice.id,
    )
    .await
    .unwrap();
    assert_eq!(m1.registered_by, alice.id);
    assert!(m1.last_seen_at.is_some());

    // re-register from another user with new details: fields update,
    // registrar sticks
    let m2 = repo::machines::upsert(
        &db.pool,
        machine_id,
        &RegisterMachineRequest {
            name: "desktop-renamed".into(),
            os: Some("nixos".into()),
            client_version: Some("0.2.0".into()),
            syncthing_device_id: Some("DEVICE-B".into()),
        },
        bob.id,
    )
    .await
    .unwrap();
    assert_eq!(m2.id, m1.id);
    assert_eq!(m2.name, "desktop-renamed");
    assert_eq!(m2.syncthing_device_id.as_deref(), Some("DEVICE-B"));
    assert_eq!(m2.registered_by, alice.id, "registrar must not change");

    assert_eq!(repo::machines::list(&db.pool).await.unwrap().len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn game_definition_upsert_replaces_and_roundtrips() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let request = UpsertGameRequest {
        title: "Baldur's Gate 3".into(),
        class: "gog".into(),
        version: "1.0.0".into(),
        config: serde_json::json!({ "umu_id": "umu-1086940", "exe_rel": "app/bin/bg3.exe" }),
        artifacts: vec![ArtifactDto {
            bucket_key: "gog/bg3/setup.exe".into(),
            sha256: "ab".repeat(32),
            size: Some(123),
            role: game_mgr_api_types::ArtifactRole::Base,
            dlc_name: None,
        }],
    };
    repo::games::upsert(&db.pool, "bg3", &request)
        .await
        .unwrap();

    // full replace on re-upsert (server is the single source of truth)
    let updated = UpsertGameRequest {
        version: "1.1.0".into(),
        artifacts: vec![],
        ..request.clone()
    };
    repo::games::upsert(&db.pool, "bg3", &updated)
        .await
        .unwrap();

    let games = repo::games::list_with_stats(&db.pool).await.unwrap();
    assert_eq!(games.len(), 1);
    let def = &games[0].definition;
    assert_eq!(def.version, "1.1.0");
    assert_eq!(def.config["umu_id"], "umu-1086940");
    assert!(def.artifacts.is_empty(), "artifact list was replaced");

    let fetched = repo::games::get(&db.pool, "bg3").await.unwrap().unwrap();
    assert_eq!(fetched.definition.title, "Baldur's Gate 3");
    assert!(repo::games::get(&db.pool, "nope").await.unwrap().is_none());

    db.cleanup().await;
}

#[tokio::test]
async fn profile_delete_cascades_sessions_and_history() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let alice = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let bob = repo::users::upsert_by_sub(&db.pool, "bob").await.unwrap();
    let profile = repo::profiles::create(&db.pool, alice.id, "Doomed")
        .await
        .unwrap();

    // give the profile a session, an event and a transfer history entry
    let machine = repo::machines::upsert(
        &db.pool,
        Uuid::new_v4(),
        &RegisterMachineRequest {
            name: "desktop".into(),
            os: None,
            client_version: None,
            syncthing_device_id: None,
        },
        alice.id,
    )
    .await
    .unwrap();
    repo::games::upsert(
        &db.pool,
        "bg3",
        &UpsertGameRequest {
            title: "BG3".into(),
            class: "gog".into(),
            version: "1.0.0".into(),
            config: serde_json::json!({}),
            artifacts: vec![],
        },
    )
    .await
    .unwrap();
    let now = OffsetDateTime::now_utc();
    repo::ingest::insert_session(
        &db.pool,
        &SessionDto {
            id: Uuid::new_v4(),
            machine_id: machine.id,
            profile_id: profile.id,
            game_id: "bg3".into(),
            started_at: now - time::Duration::hours(1),
            ended_at: now,
            duration_s: 3600,
            exit_code: Some(0),
            end_reason: game_mgr_api_types::SessionEndReason::Exited,
        },
    )
    .await
    .unwrap();
    repo::ingest::insert_event(
        &db.pool,
        &game_mgr_api_types::EventDto {
            client_event_id: Uuid::new_v4(),
            machine_id: machine.id,
            profile_id: Some(profile.id),
            game_id: Some("bg3".into()),
            kind: "launch".into(),
            payload: serde_json::json!({}),
            occurred_at: now,
        },
    )
    .await
    .unwrap();
    let TransferOutcome::Done(profile_now_bobs) =
        repo::profiles::transfer(&db.pool, profile.id, alice.id, bob.id)
            .await
            .unwrap()
    else {
        panic!("transfer must succeed");
    };

    // only the owner may delete
    assert!(matches!(
        repo::profiles::delete(&db.pool, profile_now_bobs.id, alice.id)
            .await
            .unwrap(),
        OwnedUpdate::NotOwner
    ));
    assert!(matches!(
        repo::profiles::delete(&db.pool, Uuid::new_v4(), bob.id)
            .await
            .unwrap(),
        OwnedUpdate::NotFound
    ));
    assert!(matches!(
        repo::profiles::delete(&db.pool, profile_now_bobs.id, bob.id)
            .await
            .unwrap(),
        OwnedUpdate::Updated(_)
    ));

    // sessions + transfer history cascaded; the event survives with a
    // nulled profile
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let transfers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profile_transfers")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(transfers, 0);
    let (events, null_profiles): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(*) FILTER (WHERE profile_id IS NULL) FROM events")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!((events, null_profiles), (1, 1));

    db.cleanup().await;
}

#[tokio::test]
async fn profile_rename_enforces_ownership() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let alice = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let bob = repo::users::upsert_by_sub(&db.pool, "bob").await.unwrap();
    let profile = repo::profiles::create(&db.pool, alice.id, "Sander")
        .await
        .unwrap();

    assert!(matches!(
        repo::profiles::rename(&db.pool, profile.id, bob.id, "Hijacked")
            .await
            .unwrap(),
        OwnedUpdate::NotOwner
    ));
    assert!(matches!(
        repo::profiles::rename(&db.pool, Uuid::new_v4(), alice.id, "Ghost")
            .await
            .unwrap(),
        OwnedUpdate::NotFound
    ));
    let OwnedUpdate::Updated(renamed) =
        repo::profiles::rename(&db.pool, profile.id, alice.id, "Sander II")
            .await
            .unwrap()
    else {
        panic!("owner rename must succeed");
    };
    assert_eq!(renamed.name, "Sander II");

    db.cleanup().await;
}

#[tokio::test]
async fn profile_transfer_moves_ownership_and_writes_audit_row() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let alice = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let bob = repo::users::upsert_by_sub(&db.pool, "bob").await.unwrap();
    let profile = repo::profiles::create(&db.pool, alice.id, "Kid")
        .await
        .unwrap();

    // ownership is checked before anything else: a non-owner gets NotOwner
    // even when "transferring" to the current owner or to themselves
    assert!(matches!(
        repo::profiles::transfer(&db.pool, profile.id, bob.id, alice.id)
            .await
            .unwrap(),
        TransferOutcome::NotOwner
    ));
    assert!(matches!(
        repo::profiles::transfer(&db.pool, profile.id, bob.id, bob.id)
            .await
            .unwrap(),
        TransferOutcome::NotOwner
    ));
    // owner self-transfer is a no-op error, missing profile is NotFound
    assert!(matches!(
        repo::profiles::transfer(&db.pool, profile.id, alice.id, alice.id)
            .await
            .unwrap(),
        TransferOutcome::AlreadyOwner
    ));
    assert!(matches!(
        repo::profiles::transfer(&db.pool, Uuid::new_v4(), alice.id, bob.id)
            .await
            .unwrap(),
        TransferOutcome::NotFound
    ));

    let TransferOutcome::Done(moved) =
        repo::profiles::transfer(&db.pool, profile.id, alice.id, bob.id)
            .await
            .unwrap()
    else {
        panic!("owner transfer must succeed");
    };
    assert_eq!(moved.owner_user_id, bob.id);

    // audit row exists and is correct
    let (from, to): (Uuid, Uuid) = sqlx::query_as(
        "SELECT from_user_id, to_user_id FROM profile_transfers WHERE profile_id = $1",
    )
    .bind(profile.id)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((from, to), (alice.id, bob.id));

    // stats keyed by profile follow it: ownership change, same profile id
    let owned_by_bob = repo::profiles::list_owned(&db.pool, bob.id).await.unwrap();
    assert_eq!(owned_by_bob.len(), 1);
    assert_eq!(owned_by_bob[0].id, profile.id);

    // unknown target user -> FK violation surfaced as an error
    let err = repo::profiles::transfer(&db.pool, profile.id, bob.id, Uuid::new_v4())
        .await
        .unwrap_err();
    assert!(
        repo::is_fk_violation(&err),
        "expected FK violation, got {err:?}"
    );

    db.cleanup().await;
}

#[tokio::test]
async fn session_insert_reports_per_item_outcomes() {
    let Some(db) = TestDb::create().await else {
        return;
    };

    let alice = repo::users::upsert_by_sub(&db.pool, "alice").await.unwrap();
    let profile = repo::profiles::create(&db.pool, alice.id, "Sander")
        .await
        .unwrap();
    let machine = repo::machines::upsert(
        &db.pool,
        Uuid::new_v4(),
        &RegisterMachineRequest {
            name: "desktop".into(),
            os: None,
            client_version: None,
            syncthing_device_id: None,
        },
        alice.id,
    )
    .await
    .unwrap();
    repo::games::upsert(
        &db.pool,
        "bg3",
        &UpsertGameRequest {
            title: "BG3".into(),
            class: "gog".into(),
            version: "1.0.0".into(),
            config: serde_json::json!({}),
            artifacts: vec![],
        },
    )
    .await
    .unwrap();

    let now = OffsetDateTime::now_utc();
    let session = SessionDto {
        id: Uuid::new_v4(),
        machine_id: machine.id,
        profile_id: profile.id,
        game_id: "bg3".into(),
        started_at: now - time::Duration::hours(1),
        ended_at: now,
        duration_s: 3600,
        exit_code: Some(0),
        end_reason: game_mgr_api_types::SessionEndReason::Exited,
    };

    assert_eq!(
        repo::ingest::insert_session(&db.pool, &session)
            .await
            .unwrap(),
        Outcome::Inserted
    );
    assert_eq!(
        repo::ingest::insert_session(&db.pool, &session)
            .await
            .unwrap(),
        Outcome::Duplicate
    );

    // unknown game -> rejected with a reason, not an aborted batch
    let bad = SessionDto {
        id: Uuid::new_v4(),
        game_id: "unknown-game".into(),
        ..session.clone()
    };
    let Outcome::Rejected(reason) = repo::ingest::insert_session(&db.pool, &bad).await.unwrap()
    else {
        panic!("expected rejection for unknown game");
    };
    assert!(reason.contains("foreign key"), "reason was: {reason}");

    // negative duration violates the CHECK constraint
    let negative = SessionDto {
        id: Uuid::new_v4(),
        duration_s: -5,
        ..session.clone()
    };
    assert!(matches!(
        repo::ingest::insert_session(&db.pool, &negative)
            .await
            .unwrap(),
        Outcome::Rejected(_)
    ));

    // aggregates reflect exactly one stored session
    let games = repo::games::list_with_stats(&db.pool).await.unwrap();
    assert_eq!(games[0].total_playtime_s, 3600);
    assert_eq!(games[0].session_count, 1);
    assert!(games[0].last_played.is_some());

    db.cleanup().await;
}

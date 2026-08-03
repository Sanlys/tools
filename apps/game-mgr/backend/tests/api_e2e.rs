//! End-to-end API tests: whole router over HTTP against a real postgres,
//! multi-user via the fake backend's `x-fake-sub` header (PLAN.md §15).

mod common;

use axum::http::StatusCode;
use common::{TestDb, app, req, user_id_of};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn me_auto_provisions_users_per_sub() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    let (status, body) = req(&app, "GET", "/api/v1/me", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["user"]["sub"], "dev-user");
    assert_eq!(body["profiles"], json!([]));

    let dev = user_id_of(&app, "dev-user").await;
    let alice = user_id_of(&app, "alice").await;
    assert_ne!(dev, alice, "distinct subs get distinct users");
    // repeated requests keep the same identity
    assert_eq!(alice, user_id_of(&app, "alice").await);

    let (status, users) = req(&app, "GET", "/api/v1/users", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(users.as_array().unwrap().len(), 2);

    db.cleanup().await;
}

#[tokio::test]
async fn profile_lifecycle_create_rename_transfer() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    let alice = user_id_of(&app, "alice").await;
    let bob = user_id_of(&app, "bob").await;

    // create (alice)
    let (status, profile) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        Some("alice"),
        Some(json!({ "name": "Sander" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{profile}");
    assert_eq!(profile["owner_user_id"], json!(alice.to_string()));
    let profile_id = profile["id"].as_str().unwrap().to_string();

    // empty name is rejected
    let (status, _) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        Some("alice"),
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // rename by non-owner -> 403; by owner -> 200
    let (status, _) = req(
        &app,
        "PATCH",
        &format!("/api/v1/profiles/{profile_id}"),
        Some("bob"),
        Some(json!({ "name": "Hijacked" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, renamed) = req(
        &app,
        "PATCH",
        &format!("/api/v1/profiles/{profile_id}"),
        Some("alice"),
        Some(json!({ "name": "Sander II" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "Sander II");

    // rename of a missing profile -> 404
    let (status, _) = req(
        &app,
        "PATCH",
        &format!("/api/v1/profiles/{}", Uuid::new_v4()),
        Some("alice"),
        Some(json!({ "name": "Ghost" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // transfer: non-owner -> 403, unknown target -> 422, owner -> 200
    let transfer_uri = format!("/api/v1/profiles/{profile_id}/transfer");
    let (status, _) = req(
        &app,
        "POST",
        &transfer_uri,
        Some("bob"),
        Some(json!({ "to_user_id": bob.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = req(
        &app,
        "POST",
        &transfer_uri,
        Some("alice"),
        Some(json!({ "to_user_id": Uuid::new_v4().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, moved) = req(
        &app,
        "POST",
        &transfer_uri,
        Some("alice"),
        Some(json!({ "to_user_id": bob.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["owner_user_id"], json!(bob.to_string()));

    // history followed the profile: bob owns it now, alice owns nothing
    let (_, me_bob) = req(&app, "GET", "/api/v1/me", Some("bob"), None).await;
    assert_eq!(me_bob["profiles"].as_array().unwrap().len(), 1);
    let (_, me_alice) = req(&app, "GET", "/api/v1/me", Some("alice"), None).await;
    assert_eq!(me_alice["profiles"], json!([]));

    // old owner lost mutation rights; self-transfer is rejected
    let (status, _) = req(
        &app,
        "PATCH",
        &format!("/api/v1/profiles/{profile_id}"),
        Some("alice"),
        Some(json!({ "name": "Mine Again" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = req(
        &app,
        "POST",
        &transfer_uri,
        Some("bob"),
        Some(json!({ "to_user_id": bob.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // household visibility: everyone sees all profiles
    let (status, all) = req(&app, "GET", "/api/v1/profiles", Some("alice"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all.as_array().unwrap().len(), 1);

    db.cleanup().await;
}

#[tokio::test]
async fn game_definitions_crud_over_http() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    // validation: bad slug, bad version, bad artifact hash
    let body = json!({ "title": "X", "class": "gog", "version": "1.0.0" });
    let (status, _) = req(
        &app,
        "PUT",
        "/api/v1/games/Bad_Slug",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = req(
        &app,
        "PUT",
        "/api/v1/games/ok",
        None,
        Some(json!({ "title": "X", "class": "gog", "version": "latest" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = req(
        &app,
        "PUT",
        "/api/v1/games/ok",
        None,
        Some(json!({
            "title": "X", "class": "gog", "version": "1.0.0",
            "artifacts": [{ "bucket_key": "k", "sha256": "tooshort" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // create with full config + artifacts, read back, list
    let definition = json!({
        "title": "Baldur's Gate 3",
        "class": "gog",
        "version": "1.0.0",
        "config": {
            "umu_id": "umu-1086940",
            "exe_rel": "app/bin/bg3.exe",
            "watch_exes": ["bg3.exe"],
            "saves_in_prefix": "drive_c/saves"
        },
        "artifacts": [
            { "bucket_key": "gog/bg3/setup.exe", "sha256": "ab".repeat(32), "size": 42 },
            { "bucket_key": "gog/bg3/patches/patch1.exe", "sha256": "cd".repeat(32),
              "size": 7, "role": "patch" }
        ]
    });
    let (status, created) = req(&app, "PUT", "/api/v1/games/bg3", None, Some(definition)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["config"]["umu_id"], "umu-1086940");

    let (status, fetched) = req(&app, "GET", "/api/v1/games/bg3", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["artifacts"][0]["bucket_key"], "gog/bg3/setup.exe");
    assert_eq!(
        fetched["artifacts"][0]["role"], "base",
        "role defaults to base"
    );
    assert_eq!(fetched["artifacts"][1]["role"], "patch", "roles roundtrip");
    assert_eq!(fetched["total_playtime_s"], 0);

    let (_, listed) = req(&app, "GET", "/api/v1/games", None, None).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["config"]["exe_rel"], "app/bin/bg3.exe");

    let (status, _) = req(&app, "GET", "/api/v1/games/missing", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    db.cleanup().await;
}

#[tokio::test]
async fn profile_delete_over_http_enforces_ownership() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    let (_, profile) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        Some("alice"),
        Some(json!({ "name": "Doomed" })),
    )
    .await;
    let profile_id = profile["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v1/profiles/{profile_id}");

    let (status, _) = req(&app, "DELETE", &uri, Some("bob"), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = req(&app, "DELETE", &uri, Some("alice"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = req(&app, "DELETE", &uri, Some("alice"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already gone");

    let (_, me) = req(&app, "GET", "/api/v1/me", Some("alice"), None).await;
    assert_eq!(me["profiles"], json!([]));

    db.cleanup().await;
}

#[tokio::test]
async fn machines_register_heartbeat_and_list() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());
    let machine_id = Uuid::new_v4();
    let uri = format!("/api/v1/machines/{machine_id}");

    let (status, m) = req(
        &app,
        "PUT",
        &uri,
        None,
        Some(json!({
            "name": "desktop",
            "os": "arch",
            "client_version": "0.1.0",
            "syncthing_device_id": "DEVICE-AAAA"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["syncthing_device_id"], "DEVICE-AAAA");

    // heartbeat/update: same id, changed fields, still one machine
    let (status, _) = req(
        &app,
        "PUT",
        &uri,
        None,
        Some(json!({ "name": "desktop", "syncthing_device_id": "DEVICE-BBBB" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, machines) = req(&app, "GET", "/api/v1/machines", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let machines = machines.as_array().unwrap().clone();
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0]["syncthing_device_id"], "DEVICE-BBBB");
    assert!(machines[0]["last_seen_at"].is_string());

    db.cleanup().await;
}

#[tokio::test]
async fn sessions_batch_is_idempotent_with_per_item_errors() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    // setup: machine + profile + catalog entry, all through the API
    let machine_id = Uuid::new_v4();
    req(
        &app,
        "PUT",
        &format!("/api/v1/machines/{machine_id}"),
        None,
        Some(json!({ "name": "desktop" })),
    )
    .await;
    let (_, profile) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        None,
        Some(json!({ "name": "Sander" })),
    )
    .await;
    let profile_id = profile["id"].as_str().unwrap().to_string();
    let (status, push) = req(
        &app,
        "PUT",
        "/api/v1/games/bg3",
        None,
        Some(json!({ "title": "Baldur's Gate 3", "class": "gog", "version": "1.0.0" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{push}");

    let session = |id: Uuid, game: &str| {
        json!({
            "id": id.to_string(),
            "machine_id": machine_id.to_string(),
            "profile_id": profile_id,
            "game_id": game,
            "started_at": "2026-06-10T18:00:00Z",
            "ended_at": "2026-06-10T19:00:00Z",
            "duration_s": 3600
        })
    };
    let (s1, s2) = (Uuid::new_v4(), Uuid::new_v4());
    let batch = json!({ "sessions": [session(s1, "bg3"), session(s2, "bg3")] });

    // first push inserts both
    let (status, r) = req(
        &app,
        "POST",
        "/api/v1/sessions:batch",
        None,
        Some(batch.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(
        (r["inserted"].as_u64(), r["duplicates"].as_u64()),
        (Some(2), Some(0))
    );

    // identical retry (spool re-upload) inserts nothing
    let (_, r) = req(&app, "POST", "/api/v1/sessions:batch", None, Some(batch)).await;
    assert_eq!(
        (r["inserted"].as_u64(), r["duplicates"].as_u64()),
        (Some(0), Some(2))
    );

    // mixed batch: one valid, one referencing an unknown game
    let good = Uuid::new_v4();
    let bad = Uuid::new_v4();
    let (_, r) = req(
        &app,
        "POST",
        "/api/v1/sessions:batch",
        None,
        Some(json!({ "sessions": [session(good, "bg3"), session(bad, "nope")] })),
    )
    .await;
    assert_eq!(r["inserted"], 1);
    let errors = r["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["id"], json!(bad.to_string()));

    // aggregates: 3 sessions × 1h
    let (_, games) = req(&app, "GET", "/api/v1/games", None, None).await;
    assert_eq!(games[0]["total_playtime_s"], 3 * 3600);
    assert_eq!(games[0]["session_count"], 3);

    db.cleanup().await;
}

#[tokio::test]
async fn session_browser_lists_with_filters_and_exit_codes() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    // setup: machine + two profiles + game
    let machine_id = Uuid::new_v4();
    req(
        &app,
        "PUT",
        &format!("/api/v1/machines/{machine_id}"),
        None,
        Some(json!({ "name": "desktop" })),
    )
    .await;
    let (_, p1) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        None,
        Some(json!({ "name": "P1" })),
    )
    .await;
    let (_, p2) = req(
        &app,
        "POST",
        "/api/v1/profiles",
        None,
        Some(json!({ "name": "P2" })),
    )
    .await;
    let (p1, p2) = (
        p1["id"].as_str().unwrap().to_string(),
        p2["id"].as_str().unwrap().to_string(),
    );
    req(
        &app,
        "PUT",
        "/api/v1/games/bg3",
        None,
        Some(json!({ "title": "BG3", "class": "gog", "version": "1.0.0" })),
    )
    .await;

    // one clean session (p1), one crash (p2), one wine-style drained (p1)
    let batch = json!({ "sessions": [
        { "id": Uuid::new_v4().to_string(), "machine_id": machine_id.to_string(),
          "profile_id": p1, "game_id": "bg3",
          "started_at": "2026-06-10T10:00:00Z", "ended_at": "2026-06-10T11:00:00Z",
          "duration_s": 3600, "exit_code": 0, "end_reason": "exited" },
        { "id": Uuid::new_v4().to_string(), "machine_id": machine_id.to_string(),
          "profile_id": p2, "game_id": "bg3",
          "started_at": "2026-06-10T12:00:00Z", "ended_at": "2026-06-10T12:00:30Z",
          "duration_s": 30, "exit_code": 134, "end_reason": "exited" },
        { "id": Uuid::new_v4().to_string(), "machine_id": machine_id.to_string(),
          "profile_id": p1, "game_id": "bg3",
          "started_at": "2026-06-10T13:00:00Z", "ended_at": "2026-06-10T14:00:00Z",
          "duration_s": 3600, "end_reason": "tree_drained" }
    ]});
    let (status, r) = req(&app, "POST", "/api/v1/sessions:batch", None, Some(batch)).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["inserted"], 3);

    // newest first, exit data preserved
    let (status, all) = req(&app, "GET", "/api/v1/sessions", None, None).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let all = all.as_array().unwrap().clone();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["end_reason"], "tree_drained");
    assert_eq!(all[0]["exit_code"], json!(null));
    assert_eq!(all[1]["exit_code"], 134, "the crash is visible");
    assert_eq!(all[2]["exit_code"], 0);

    // filter by profile
    let (_, by_profile) = req(
        &app,
        "GET",
        &format!("/api/v1/sessions?profile_id={p2}"),
        None,
        None,
    )
    .await;
    assert_eq!(by_profile.as_array().unwrap().len(), 1);
    assert_eq!(by_profile[0]["exit_code"], 134);

    // pagination: before the second session's start -> only the first
    let (_, page) = req(
        &app,
        "GET",
        "/api/v1/sessions?before=2026-06-10T12:00:00Z&limit=10",
        None,
        None,
    )
    .await;
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["exit_code"], 0);

    db.cleanup().await;
}

#[tokio::test]
async fn events_batch_is_idempotent() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    let machine_id = Uuid::new_v4();
    req(
        &app,
        "PUT",
        &format!("/api/v1/machines/{machine_id}"),
        None,
        Some(json!({ "name": "desktop" })),
    )
    .await;

    let event_id = Uuid::new_v4();
    let batch = json!({ "events": [{
        "client_event_id": event_id.to_string(),
        "machine_id": machine_id.to_string(),
        "kind": "launch",
        "payload": { "via": "test" },
        "occurred_at": "2026-06-10T18:00:00Z"
    }]});

    let (status, r) = req(
        &app,
        "POST",
        "/api/v1/events:batch",
        None,
        Some(batch.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["inserted"], 1);

    let (_, r) = req(&app, "POST", "/api/v1/events:batch", None, Some(batch)).await;
    assert_eq!(r["duplicates"], 1);

    db.cleanup().await;
}

#[tokio::test]
async fn readyz_is_ok_with_database() {
    let Some(db) = TestDb::create().await else {
        return;
    };
    let app = app(db.pool.clone());

    let (status, body) = req(&app, "GET", "/readyz", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    db.cleanup().await;
}

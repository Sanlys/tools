//! `game-mgr-backend` library: HTTP API, auth, persistence.
//!
//! Exposed as a library so integration tests (`tests/`) can build the full
//! router against their own isolated databases (PLAN.md §15).

pub mod api;
pub mod auth;
pub mod db;
pub mod error;
pub mod repo;

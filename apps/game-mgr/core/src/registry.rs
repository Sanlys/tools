//! Runtime registry: titles arrive as server-stored definitions and are
//! instantiated through their class implementations (PLAN.md §4.1).
//! Adding a *class* is code; adding a *game* is data from the client UI.

use std::sync::Arc;

use game_mgr_api_types::GameDefinition;

use crate::classes::{GogGame, SkyrimModded};
use crate::game::GameClass;

#[derive(Default, Clone)]
pub struct Registry {
    games: Vec<Arc<dyn GameClass>>,
}

impl Registry {
    pub fn add(&mut self, game: impl GameClass) {
        self.games.push(Arc::new(game));
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn GameClass>> {
        self.games.iter()
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn GameClass>> {
        self.games.iter().find(|g| g.meta().id == id)
    }
}

/// Class slugs the client can instantiate — the Add Game UI offers these.
pub const KNOWN_CLASSES: &[&str] = &["gog", "skyrim-modded"];

/// Instantiate one definition through its class.
pub fn instantiate(def: &GameDefinition) -> anyhow::Result<Arc<dyn GameClass>> {
    match def.class.as_str() {
        "gog" => Ok(Arc::new(GogGame::from_definition(def)?)),
        "skyrim-modded" => Ok(Arc::new(SkyrimModded::from_definition(def)?)),
        other => anyhow::bail!(
            "unknown game class '{other}' (this client knows: {})",
            KNOWN_CLASSES.join(", ")
        ),
    }
}

/// Build the registry from server definitions. Definitions this client
/// can't instantiate (unknown class, bad config — e.g. created by a newer
/// client) are skipped with a warning instead of poisoning the library.
pub fn registry_from_definitions(defs: &[GameDefinition]) -> Registry {
    let mut registry = Registry::default();
    for def in defs {
        match instantiate(def) {
            Ok(game) => registry.games.push(game),
            Err(err) => tracing::warn!(game = %def.id, %err, "skipping game definition"),
        }
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gog_def(id: &str) -> GameDefinition {
        GameDefinition {
            id: id.into(),
            title: format!("Game {id}"),
            class: "gog".into(),
            version: "1.0.0".into(),
            config: serde_json::json!({
                "umu_id": "umu-1",
                "exe_rel": "app/game.exe",
                "saves_in_prefix": "drive_c/saves"
            }),
            artifacts: vec![],
        }
    }

    #[test]
    fn registry_builds_from_definitions() {
        let defs = vec![gog_def("bg3"), gog_def("witcher3")];
        let registry = registry_from_definitions(&defs);
        assert_eq!(registry.len(), 2);
        assert!(registry.get("bg3").is_some());
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn broken_definitions_are_skipped_not_fatal() {
        let mut unknown_class = gog_def("future-game");
        unknown_class.class = "playstation9".into();
        let mut bad_config = gog_def("broken");
        // config fields are optional now; a type mismatch is what's invalid
        bad_config.config = serde_json::json!({ "umu_id": 123 });

        let registry = registry_from_definitions(&[gog_def("ok"), unknown_class, bad_config]);
        assert_eq!(registry.len(), 1, "only the valid definition survives");
        assert!(registry.get("ok").is_some());
    }

    #[test]
    fn instantiate_reports_known_classes() {
        let mut def = gog_def("x");
        def.class = "switch".into(); // arrives in M4
        let err = match instantiate(&def) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("unknown class must be rejected"),
        };
        assert!(err.contains("gog"), "{err}");
    }
}

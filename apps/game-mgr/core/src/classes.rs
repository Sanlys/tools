//! Game classes: one implementation per *kind* of game (PLAN.md §4).
//! `SwitchGame` (M4) joins later.

pub mod gog;
pub mod skyrim;

pub use gog::GogGame;
pub use skyrim::SkyrimModded;

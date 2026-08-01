//! `apps/idp` -- the tools platform's own OIDC provider + WebAuthn passkey
//! login. Ported from the design proven out in `sanlys/manager`'s `idp/`
//! (see that repo for the original, more heavily-commented version of the
//! WebAuthn ceremony code), trimmed for this platform: no consent screen,
//! no dynamic OAuth client registration (clients + their role vocabularies
//! are declared statically, see [`clients`]), and per-app role grants
//! (`user_app_roles`) added on top so a tool can gate itself per-user, not
//! just require-any-login.

pub mod clients;
pub mod db;
pub mod error;
pub mod keys;
pub mod metrics;
pub mod rate_limit;
pub mod routes;
pub mod state;

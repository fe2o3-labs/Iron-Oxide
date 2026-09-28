//! Plan and entitlements server functions (#21).

use dioxus::prelude::*;
use iron_oxide_domain::entitlements::Entitlements;

#[cfg(feature = "server")]
use crate::server::{AppState, auth::AuthUser, entitlements};
#[cfg(feature = "server")]
use dioxus::server::axum::Extension;

/// The signed-in user's plan and everything it includes, for the UI (locks, remaining slots,
/// upgrade prompts). The server checks every gate itself; this is for display only.
///
/// Read from `users.plan` on every call. A `POST`, like every call that reads the signed-in user,
/// so it is never cached. Fails with 401 when signed out.
#[allow(dead_code, reason = "shown by the account and settings screens (#34)")]
#[post("/api/billing/entitlements", state: Extension<AppState>, user: AuthUser)]
pub async fn my_entitlements() -> Result<Entitlements, ServerFnError> {
    Ok(entitlements::entitlements_of(&state.db, user).await?)
}

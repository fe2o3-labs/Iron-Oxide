//! Server functions: called like async functions from the client, executed on the server.
//!
//! One module per area, holding its server functions and the types they exchange (DTOs). The
//! logic behind them is server-only, in `crate::server::api`. Conventions (errors, idempotency,
//! isolation tests): `docs/api.md`. Sign-in has its own module, `crate::auth::api`.
//!
//! Adding an area is one line here; Dioxus registers every server function it finds.

pub mod account;
pub mod billing;
#[allow(
    dead_code,
    reason = "used by the UI screens that call the server functions (#29-#32)"
)]
pub mod error;
#[cfg_attr(
    not(feature = "server"),
    allow(
        dead_code,
        reason = "the client only decodes them until the history screens (#33) land"
    )
)]
pub mod history;
pub mod programs;
pub mod sessions;
pub mod settings;
pub mod time;

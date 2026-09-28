//! Typed ids used by the repository.
//!
//! Stand-ins for the domain ids of `iron-oxide-domain` (#48), which have the same shape
//! (`from_uuid`/`as_uuid`); the repository switches to them now that #48 is merged, in a follow-up.
//! Ids created on the client use the domain's UUIDv7 constructor (#65); ids created in the
//! database default to `uuidv7()`. Distinct types
//! mean a program id can never be passed where a session id is expected, and every repository
//! function takes a [`UserId`] that the caller must get from the authenticated session.

use std::fmt;

use sqlx::types::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(Uuid);

        impl $name {
            /// Wraps an existing UUID.
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }
    };
}

uuid_id!(
    /// A user account (`users.id`).
    UserId
);
uuid_id!(
    /// A program, across all of its versions (`programs.id`).
    ProgramId
);
uuid_id!(
    /// One immutable version of a program (`program_versions.id`).
    ProgramVersionId
);
uuid_id!(
    /// The client's idempotency key for a request that creates a program (create or copy), so a
    /// retried request returns the program it already created. Unique per user.
    CreationId
);
uuid_id!(
    /// A workout session, generated on the client (`workout_sessions.id`).
    SessionId
);
uuid_id!(
    /// A logged set, generated on the client (`workout_sets.id`).
    SetId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_their_uuid_and_debug_names_the_type() {
        let uuid = Uuid::from_u128(42);
        let id = SetId::from_uuid(uuid);
        assert_eq!(id.as_uuid(), uuid);
        assert_eq!(
            format!("{id:?}"),
            "SetId(00000000-0000-0000-0000-00000000002a)"
        );
        assert_eq!(UserId::from_uuid(uuid), UserId::from_uuid(uuid));
    }
}

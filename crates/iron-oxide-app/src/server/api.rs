//! The server side of the server functions in `crate::api` (#68, conventions in `docs/api.md`).
//!
//! Each area has a module here with the logic behind its server functions: plain async functions
//! that take the pool and the caller's `db::ids::UserId` (from `AuthUser::owner`, never from the client)
//! and return [`ApiError`]. The `#[post]` functions in `crate::api::<area>` only extract the state
//! and the user and call them.

pub mod error;
pub mod errors_layer;
pub mod programs;
pub mod sessions;
#[cfg(test)]
pub(crate) mod testing;

use iron_oxide_domain::time::Timestamp;
use sqlx::types::time::OffsetDateTime;

pub use self::error::ApiError;

/// Nanoseconds in a millisecond.
const NANOS_PER_MILLI: i128 = 1_000_000;

/// A stored time as the API's [`Timestamp`] (milliseconds since the epoch). Sub-millisecond
/// digits are dropped; times written through the API have none.
pub fn timestamp(time: OffsetDateTime) -> Result<Timestamp, ApiError> {
    let millis = time.unix_timestamp_nanos().div_euclid(NANOS_PER_MILLI);
    i64::try_from(millis)
        .map(Timestamp::from_epoch_millis)
        .map_err(|_| ApiError::internal("stored time out of range"))
}

/// A time sent by the client, for the database. `422` if it is outside years -9999 to 9999.
#[allow(dead_code, reason = "used by the session writes (#18)")]
pub fn offset_date_time(time: Timestamp) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(time.epoch_millis()) * NANOS_PER_MILLI)
        .map_err(|_| ApiError::invalid("Invalid time."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_the_database_type() {
        for millis in [0, 1, -1, 1_790_000_000_123, -62_000_000_000_000] {
            let time = Timestamp::from_epoch_millis(millis);
            assert_eq!(timestamp(offset_date_time(time).unwrap()).unwrap(), time);
        }
    }

    #[test]
    fn sub_millisecond_digits_are_dropped_towards_the_past() {
        let time = OffsetDateTime::from_unix_timestamp_nanos(1_999_999).unwrap();
        assert_eq!(timestamp(time).unwrap(), Timestamp::from_epoch_millis(1));
        let before = OffsetDateTime::from_unix_timestamp_nanos(-1).unwrap();
        assert_eq!(timestamp(before).unwrap(), Timestamp::from_epoch_millis(-1));
    }

    #[test]
    fn out_of_range_client_times_are_invalid() {
        let error = offset_date_time(Timestamp::from_epoch_millis(i64::MAX)).unwrap_err();
        assert_eq!(error.public(), (422, "Invalid time."));
    }
}

//! Server time: the first server function, kept as the smallest round-trip example.

use dioxus::prelude::*;

/// Returns the server's current time, in whole seconds since the Unix epoch.
#[get("/api/server-time")]
pub async fn server_time() -> Result<u64, ServerFnError> {
    unix_seconds(std::time::SystemTime::now())
}

/// Converts a wall-clock time into whole seconds since the Unix epoch.
///
/// Fails if `time` is before the epoch, which would mean the server clock is badly wrong.
#[cfg(any(feature = "server", test))]
fn unix_seconds(time: std::time::SystemTime) -> Result<u64, ServerFnError> {
    time.duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| {
            ServerFnError::new(format!("server clock is before the Unix epoch: {error}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn unix_seconds_is_zero_at_the_epoch() {
        assert_eq!(unix_seconds(UNIX_EPOCH).unwrap(), 0);
    }

    #[test]
    fn unix_seconds_truncates_sub_second_precision() {
        let time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_999);
        assert_eq!(unix_seconds(time).unwrap(), 1_700_000_000);
    }

    #[test]
    fn unix_seconds_fails_before_the_epoch() {
        let time = UNIX_EPOCH - Duration::from_secs(1);
        let error = unix_seconds(time).unwrap_err();
        assert!(
            error.to_string().contains("before the Unix epoch"),
            "{error}"
        );
    }
}

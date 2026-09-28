//! Which program day comes next: see [`next_day`].

use std::collections::HashMap;

use crate::ids::DayId;

use super::error::RotationError;
use super::model::Session;
#[cfg(doc)]
use super::model::SessionStatus;

/// Picks the next day to train from `rotation`, given the user's past sessions (in any order).
///
/// # The rule
///
/// A program's days are trained in a fixed rotation (`A, B, C, A, B, C, ...`):
///
/// 1. Only sessions that **advance the rotation** count: [`Completed`] and [`Skipped`] ones.
///    [`Abandoned`] sessions are ignored, so a day given up part-way comes up again; in-progress
///    sessions are ignored too.
/// 2. Among those, the most recent one (latest `finished_at`) whose day **is still in the
///    rotation** is the anchor. The next day is the one after the anchor's, wrapping from the last
///    day back to the first. When two sessions finished at the same time, the one later in
///    `history` wins.
/// 3. Sessions whose day is no longer in the rotation (a newer version of the program dropped or
///    renamed it) are passed over, so the rotation carries on from the most recent day it still
///    knows.
/// 4. With no anchor (no history, only abandoned sessions, or only days that were dropped), the
///    rotation starts at its first day.
///
/// Days are matched by [`DayId`] only, whatever program version a session was run from.
///
/// # Precondition
///
/// `history` must contain only sessions of the **active program** (every version of it, but no
/// other program). The caller filters by program before calling: another program may reuse the
/// same day slugs (`a`, `b`, ...), and its sessions would then wrongly set the position in this
/// rotation. Rule 3 only covers days that disappeared from the active program's own rotation.
///
/// # Errors
/// - [`RotationError::EmptyRotation`] when `rotation` is empty.
/// - [`RotationError::DuplicateDay`] when a day appears twice in `rotation`.
///
/// [`Completed`]: SessionStatus::Completed
/// [`Skipped`]: SessionStatus::Skipped
/// [`Abandoned`]: SessionStatus::Abandoned
pub fn next_day<'r, T: Ord + Copy>(
    rotation: &'r [DayId],
    history: &[Session<T>],
) -> Result<&'r DayId, RotationError> {
    let first = rotation.first().ok_or(RotationError::EmptyRotation)?;

    let mut positions = HashMap::with_capacity(rotation.len());
    for (position, day) in rotation.iter().enumerate() {
        if positions.insert(day, position).is_some() {
            return Err(RotationError::DuplicateDay(day.clone()));
        }
    }

    let anchor = history
        .iter()
        .enumerate()
        .filter(|(_, session)| session.status().advances_rotation())
        .filter_map(|(index, session)| {
            let finished_at = session.finished_at()?;
            let position = *positions.get(session.day())?;
            Some(((finished_at, index), position))
        })
        .max_by_key(|&(key, _)| key);

    Ok(match anchor {
        None => first,
        Some((_, position)) => rotation.get(position + 1).unwrap_or(first),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ProgramVersionId, SessionId};
    use crate::session::SessionStatus;
    use uuid::Uuid;

    fn day(slug: &str) -> DayId {
        DayId::new(slug).unwrap()
    }

    fn days(slugs: &[&str]) -> Vec<DayId> {
        slugs.iter().map(|slug| day(slug)).collect()
    }

    fn ended(slug: &str, status: SessionStatus, finished_at: i64) -> Session<i64> {
        Session::from_parts(
            SessionId::from_uuid(Uuid::from_u128(u128::from(finished_at.unsigned_abs()))),
            ProgramVersionId::from_uuid(Uuid::from_u128(1)),
            day(slug),
            0,
            status,
            Some(finished_at),
        )
        .unwrap()
    }

    fn done(slug: &str, finished_at: i64) -> Session<i64> {
        ended(slug, SessionStatus::Completed, finished_at)
    }

    fn next(rotation: &[DayId], history: &[Session<i64>]) -> String {
        next_day(rotation, history).unwrap().to_string()
    }

    #[test]
    fn empty_history_starts_at_the_first_day() {
        assert_eq!(next(&days(&["a", "b", "c"]), &[]), "a");
    }

    #[test]
    fn follows_the_last_completed_day() {
        let rotation = days(&["a", "b", "c"]);
        assert_eq!(next(&rotation, &[done("a", 10)]), "b");
        assert_eq!(next(&rotation, &[done("a", 10), done("b", 20)]), "c");
    }

    #[test]
    fn wraps_from_the_last_day_to_the_first() {
        let rotation = days(&["a", "b", "c"]);
        let history = [done("a", 10), done("b", 20), done("c", 30)];
        assert_eq!(next(&rotation, &history), "a");
    }

    #[test]
    fn single_day_rotation_always_repeats_it() {
        let rotation = days(&["full-body"]);
        assert_eq!(next(&rotation, &[]), "full-body");
        assert_eq!(next(&rotation, &[done("full-body", 10)]), "full-body");
    }

    #[test]
    fn orders_history_by_finish_time_not_slice_order() {
        let rotation = days(&["a", "b", "c"]);
        let history = [done("b", 20), done("c", 5), done("a", 10)];
        assert_eq!(next(&rotation, &history), "c");
    }

    #[test]
    fn ties_on_finish_time_go_to_the_later_history_entry() {
        let rotation = days(&["a", "b", "c"]);
        assert_eq!(next(&rotation, &[done("a", 10), done("b", 10)]), "c");
        assert_eq!(next(&rotation, &[done("b", 10), done("a", 10)]), "b");
    }

    #[test]
    fn skipped_sessions_advance_the_rotation() {
        let rotation = days(&["a", "b", "c"]);
        let history = [done("a", 10), ended("b", SessionStatus::Skipped, 20)];
        assert_eq!(next(&rotation, &history), "c");
    }

    #[test]
    fn abandoned_sessions_do_not_advance_the_rotation() {
        let rotation = days(&["a", "b", "c"]);
        let history = [done("a", 10), ended("b", SessionStatus::Abandoned, 20)];
        assert_eq!(next(&rotation, &history), "b");
        let only_abandoned = [ended("a", SessionStatus::Abandoned, 20)];
        assert_eq!(next(&rotation, &only_abandoned), "a");
    }

    #[test]
    fn in_progress_sessions_are_ignored() {
        let rotation = days(&["a", "b", "c"]);
        let in_progress = Session::start(
            SessionId::from_uuid(Uuid::from_u128(99)),
            ProgramVersionId::from_uuid(Uuid::from_u128(1)),
            day("b"),
            30,
        );
        assert_eq!(next(&rotation, &[done("a", 10), in_progress]), "b");
    }

    #[test]
    fn a_last_day_no_longer_in_the_rotation_falls_back_to_the_latest_known_day() {
        // The program went from A, B, C to A, C: the user last did B, before that A.
        let rotation = days(&["a", "c"]);
        let history = [done("a", 10), done("b", 20)];
        assert_eq!(next(&rotation, &history), "c");
    }

    #[test]
    fn a_history_of_only_dropped_days_starts_at_the_first_day() {
        // Every day trained so far was removed by a newer version of the program.
        let rotation = days(&["push", "pull", "legs"]);
        let history = [done("a", 10), done("b", 20)];
        assert_eq!(next(&rotation, &history), "push");
    }

    #[test]
    fn rejects_an_empty_rotation() {
        assert_eq!(
            next_day::<i64>(&[], &[done("a", 1)]),
            Err(RotationError::EmptyRotation)
        );
        assert_eq!(
            RotationError::EmptyRotation.to_string(),
            "the rotation has no days"
        );
    }

    #[test]
    fn rejects_a_duplicate_day() {
        let err = next_day::<i64>(&days(&["a", "b", "a"]), &[]).unwrap_err();
        assert_eq!(err, RotationError::DuplicateDay(day("a")));
        assert_eq!(
            err.to_string(),
            "day `a` appears more than once in the rotation"
        );
    }
}

//! The engine's input: the working sets of one exercise in past sessions.

use serde::{Deserialize, Serialize};

use crate::session::{LoggedSet, SessionLog, SessionStatus};
use crate::{ExerciseId, Reps, Seconds, Weight};

/// One working set as it was performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkingSet {
    /// Repetitions done. Zero is a failed attempt.
    pub reps: Reps,
    /// Load, or `None` for body-weight work.
    pub weight: Option<Weight>,
    /// Time of a timed set, or `None`.
    pub duration: Option<Seconds>,
}

impl WorkingSet {
    /// A set of `reps` at `weight`.
    #[must_use]
    pub const fn new(weight: Weight, reps: Reps) -> Self {
        Self {
            reps,
            weight: Some(weight),
            duration: None,
        }
    }

    /// A body-weight set of `reps`.
    #[must_use]
    pub const fn bodyweight(reps: Reps) -> Self {
        Self {
            reps,
            weight: None,
            duration: None,
        }
    }
}

impl<T> From<&LoggedSet<T>> for WorkingSet {
    fn from(set: &LoggedSet<T>) -> Self {
        Self {
            reps: set.reps,
            weight: set.weight,
            duration: set.duration,
        }
    }
}

/// The working sets of one exercise in one past session, in set order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PastSession {
    /// The working sets (never warm-ups), in the order they were done.
    pub sets: Vec<WorkingSet>,
}

impl PastSession {
    /// A session with these working sets.
    #[must_use]
    pub const fn new(sets: Vec<WorkingSet>) -> Self {
        Self { sets }
    }

    /// Whether no working set was done: the exercise was skipped.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
}

impl FromIterator<WorkingSet> for PastSession {
    fn from_iter<I: IntoIterator<Item = WorkingSet>>(iter: I) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

/// The history of `exercise` for [`next_targets`](super::next_targets), oldest session first.
///
/// From `logs` (in any order), keeps the [`Completed`](SessionStatus::Completed) sessions only
/// (an abandoned session would read as a failure, a skipped one has no sets), and in each the
/// working sets of `exercise` (warm-ups left out), sorted by `set_index`. Sessions where the
/// exercise has no working set are dropped. Sessions are sorted by start time; equal start times
/// keep their order in `logs`.
///
/// It does **not** filter by program: pass the logs of the active program only, and for a load
/// that is a percentage of the training max, only the sessions since the training max was last
/// entered (see the [module documentation](super)).
pub fn exercise_history<'a, T, I>(exercise: &ExerciseId, logs: I) -> Vec<PastSession>
where
    T: Ord + Copy + 'a,
    I: IntoIterator<Item = &'a SessionLog<T>>,
{
    let mut completed: Vec<&SessionLog<T>> = logs
        .into_iter()
        .filter(|log| log.session().status() == SessionStatus::Completed)
        .collect();
    completed.sort_by_key(|log| log.session().started_at());
    completed
        .into_iter()
        .filter_map(|log| {
            let mut sets: Vec<&LoggedSet<T>> = log
                .sets()
                .iter()
                .filter(|set| !set.warm_up && &set.exercise == exercise)
                .collect();
            sets.sort_by_key(|set| set.set_index);
            let session: PastSession = sets.into_iter().map(WorkingSet::from).collect();
            (!session.is_empty()).then_some(session)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::session::Session;
    use crate::{DayId, ProgramVersionId, SessionId, SetId};

    fn squat() -> ExerciseId {
        ExerciseId::new("squat").unwrap()
    }

    fn set(n: u128, exercise: &ExerciseId, index: u16, reps: u16, warm_up: bool) -> LoggedSet<i64> {
        LoggedSet {
            id: SetId::from_uuid(Uuid::from_u128(n)),
            exercise: exercise.clone(),
            set_index: index,
            reps: Reps::new(reps),
            weight: Some(Weight::from_kg(100.0).unwrap()),
            duration: None,
            warm_up,
            completed_at: 10,
        }
    }

    fn log(
        n: u128,
        started_at: i64,
        status: SessionStatus,
        sets: Vec<LoggedSet<i64>>,
    ) -> SessionLog<i64> {
        let session = Session::from_parts(
            SessionId::from_uuid(Uuid::from_u128(n)),
            ProgramVersionId::from_uuid(Uuid::from_u128(1)),
            DayId::new("a").unwrap(),
            started_at,
            status,
            status.is_ended().then_some(started_at + 100),
        )
        .unwrap();
        // Shift the sets into the session's time span.
        let sets = sets
            .into_iter()
            .map(|set| LoggedSet {
                completed_at: started_at + set.completed_at,
                ..set
            })
            .collect();
        SessionLog::from_parts(session, sets).unwrap()
    }

    #[test]
    fn working_set_constructors() {
        let weight = Weight::from_kg(60.0).unwrap();
        assert_eq!(
            WorkingSet::new(weight, Reps::new(5)),
            WorkingSet {
                reps: Reps::new(5),
                weight: Some(weight),
                duration: None
            }
        );
        assert_eq!(WorkingSet::bodyweight(Reps::new(12)).weight, None);
        let logged = set(1, &squat(), 0, 5, false);
        let working = WorkingSet::from(&logged);
        assert_eq!(working.reps, Reps::new(5));
        assert_eq!(working.weight, logged.weight);
        assert_eq!(working.duration, None);
        assert!(PastSession::default().is_empty());
        assert!(!PastSession::new(vec![working]).is_empty());
    }

    #[test]
    fn keeps_completed_sessions_and_working_sets_of_the_exercise_in_order() {
        let bench = ExerciseId::new("bench").unwrap();
        let logs = [
            // Newest first in the input: sorted by start time.
            log(
                1,
                2_000,
                SessionStatus::Completed,
                vec![
                    set(10, &squat(), 1, 4, false),
                    set(11, &squat(), 0, 5, false),
                    set(12, &squat(), 0, 5, true),
                    set(13, &bench, 0, 8, false),
                ],
            ),
            log(
                2,
                1_000,
                SessionStatus::Completed,
                vec![set(20, &squat(), 0, 3, false)],
            ),
            log(
                3,
                3_000,
                SessionStatus::Abandoned,
                vec![set(30, &squat(), 0, 1, false)],
            ),
            log(4, 4_000, SessionStatus::Skipped, vec![]),
            log(
                5,
                5_000,
                SessionStatus::InProgress,
                vec![set(50, &squat(), 0, 9, false)],
            ),
            // The exercise was skipped: dropped.
            log(
                6,
                6_000,
                SessionStatus::Completed,
                vec![set(60, &bench, 0, 8, false)],
            ),
        ];
        let history = exercise_history(&squat(), &logs);
        let reps: Vec<Vec<u16>> = history
            .iter()
            .map(|session| session.sets.iter().map(|set| set.reps.get()).collect())
            .collect();
        assert_eq!(reps, vec![vec![3], vec![5, 4]]);
    }

    #[test]
    fn equal_start_times_keep_input_order() {
        let logs = [
            log(
                1,
                1_000,
                SessionStatus::Completed,
                vec![set(10, &squat(), 0, 1, false)],
            ),
            log(
                2,
                1_000,
                SessionStatus::Completed,
                vec![set(20, &squat(), 0, 2, false)],
            ),
        ];
        let history = exercise_history(&squat(), logs.iter());
        assert_eq!(history[0].sets[0].reps, Reps::new(1));
        assert_eq!(history[1].sets[0].reps, Reps::new(2));
        assert!(exercise_history::<i64, _>(&squat(), []).is_empty());
    }
}

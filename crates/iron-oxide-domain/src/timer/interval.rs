use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::time::{Timestamp, duration_to_millis};

/// Why an [`IntervalPlan`] is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum IntervalPlanError {
    /// The work phase is shorter than one millisecond.
    #[error("the work phase must last at least one millisecond")]
    ZeroWork,
    /// There is no round.
    #[error("an interval plan needs at least one round")]
    ZeroRounds,
    /// The whole plan does not fit in `u64` milliseconds.
    #[error("the interval plan is too long")]
    TooLong,
}

/// Work/rest intervals repeated for a number of rounds (treadmill intervals, a circuit).
///
/// A plan runs `work, rest, work, rest, …, work`: the last round has no trailing rest, since the
/// exercise's own rest timer takes over once the intervals are done. A zero rest is allowed and
/// chains the work phases back to back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "IntervalPlanRepr")]
pub struct IntervalPlan {
    work_ms: NonZeroU64,
    rest_ms: u64,
    rounds: NonZeroU32,
}

/// The serialized shape of an [`IntervalPlan`], validated on the way in.
#[derive(Deserialize)]
struct IntervalPlanRepr {
    work_ms: u64,
    rest_ms: u64,
    rounds: u32,
}

impl TryFrom<IntervalPlanRepr> for IntervalPlan {
    type Error = IntervalPlanError;

    fn try_from(repr: IntervalPlanRepr) -> Result<Self, Self::Error> {
        Self::from_millis(repr.work_ms, repr.rest_ms, repr.rounds)
    }
}

impl IntervalPlan {
    /// A plan of `rounds` rounds of `work` then `rest`, to the millisecond.
    ///
    /// # Errors
    ///
    /// [`IntervalPlanError::ZeroWork`] if `work` is under a millisecond,
    /// [`IntervalPlanError::ZeroRounds`] if `rounds` is zero and [`IntervalPlanError::TooLong`] if
    /// the whole plan overflows `u64` milliseconds.
    pub fn new(work: Duration, rest: Duration, rounds: u32) -> Result<Self, IntervalPlanError> {
        Self::from_millis(duration_to_millis(work), duration_to_millis(rest), rounds)
    }

    fn from_millis(work_ms: u64, rest_ms: u64, rounds: u32) -> Result<Self, IntervalPlanError> {
        let work_ms = NonZeroU64::new(work_ms).ok_or(IntervalPlanError::ZeroWork)?;
        let rounds = NonZeroU32::new(rounds).ok_or(IntervalPlanError::ZeroRounds)?;
        work_ms
            .get()
            .checked_add(rest_ms)
            .and_then(|cycle| cycle.checked_mul(u64::from(rounds.get())))
            .ok_or(IntervalPlanError::TooLong)?;
        Ok(Self {
            work_ms,
            rest_ms,
            rounds,
        })
    }

    /// The length of one work phase.
    #[must_use]
    pub fn work(&self) -> Duration {
        Duration::from_millis(self.work_ms.get())
    }

    /// The length of one rest phase.
    #[must_use]
    pub fn rest(&self) -> Duration {
        Duration::from_millis(self.rest_ms)
    }

    /// The number of rounds, at least one.
    #[must_use]
    pub fn rounds(&self) -> u32 {
        self.rounds.get()
    }

    /// The whole plan: every work phase and every rest phase but the last.
    #[must_use]
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.total_ms())
    }

    /// One work phase and one rest phase. Never zero, never overflows (checked at construction).
    fn cycle_ms(&self) -> u64 {
        self.work_ms.get().saturating_add(self.rest_ms)
    }

    fn total_ms(&self) -> u64 {
        self.cycle_ms()
            .saturating_mul(u64::from(self.rounds.get()))
            .saturating_sub(self.rest_ms)
    }

    /// Phases are numbered `0, 1, 2, …` as `work 1, rest 1, work 2, …`; the last work phase is
    /// `2 * rounds - 2`, and `2 * rounds - 1` means done.
    fn done_index(&self) -> u64 {
        u64::from(self.rounds.get()) * 2 - 1
    }
}

/// The phase an interval timer is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntervalPhase {
    /// Working (running, holding, pedalling hard).
    Work,
    /// Resting between two work phases.
    Rest,
    /// Every round is done.
    Done,
}

/// Where an interval timer stands at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntervalStatus {
    /// The current phase.
    pub phase: IntervalPhase,
    /// The current round, from 1 to the plan's rounds (the last round once done).
    pub round: u32,
    /// Time left in the current phase, zero once done.
    pub remaining_in_phase: Duration,
}

/// A phase change, raised once when the timer enters a new phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntervalEvent {
    /// A work phase started (never raised for the first one, which starts with the timer).
    WorkStarted {
        /// The round starting, from 2.
        round: u32,
    },
    /// A rest phase started.
    RestStarted {
        /// The round whose rest starts, from 1.
        round: u32,
    },
    /// The last work phase ended.
    Completed,
}

/// Runs an [`IntervalPlan`] from a start instant.
///
/// Phase changes follow the rules of the [module documentation](super): each is raised once, a
/// gap over several phases raises only the phase the timer is in now, and clock skew raises
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IntervalTimer {
    plan: IntervalPlan,
    started_at: Timestamp,
    observed_until: Timestamp,
}

/// A phase number (see [`IntervalPlan::done_index`]) and the time left in it.
struct Position {
    index: u64,
    remaining_ms: u64,
}

impl IntervalTimer {
    /// Starts `plan` at `now`, in the first work phase.
    #[must_use]
    pub fn start(now: Timestamp, plan: IntervalPlan) -> Self {
        Self {
            plan,
            started_at: now,
            observed_until: now,
        }
    }

    /// The plan being run.
    #[must_use]
    pub fn plan(&self) -> IntervalPlan {
        self.plan
    }

    /// When the first work phase started.
    #[must_use]
    pub fn started_at(&self) -> Timestamp {
        self.started_at
    }

    /// When the last work phase ends.
    #[must_use]
    pub fn ends_at(&self) -> Timestamp {
        self.started_at.saturating_add_millis(self.plan.total_ms())
    }

    /// The latest instant events were computed up to.
    #[must_use]
    pub fn observed_until(&self) -> Timestamp {
        self.observed_until
    }

    /// Whether every round is done at `now`.
    #[must_use]
    pub fn is_finished(&self, now: Timestamp) -> bool {
        now >= self.ends_at()
    }

    /// Time left until the end of the last round, zero once done.
    #[must_use]
    pub fn remaining(&self, now: Timestamp) -> Duration {
        self.ends_at().saturating_duration_since(now)
    }

    /// The phase, round and time left in the phase at `now`. Before the start (clock skew) the
    /// timer is at the very beginning of the first work phase.
    #[must_use]
    pub fn status(&self, now: Timestamp) -> IntervalStatus {
        let position = self.position(now);
        let (phase, round) = self.phase_of(position.index);
        IntervalStatus {
            phase,
            round,
            remaining_in_phase: Duration::from_millis(position.remaining_ms),
        }
    }

    /// The phase entered between `previous` (excluded) and `now` (included), if any. Pure:
    /// prefer [`observe`](Self::observe), which remembers `previous`.
    ///
    /// Returns `None` when `now` is not after `previous`. When several phases were entered, only
    /// the latest one is returned.
    #[must_use]
    pub fn event_between(&self, previous: Timestamp, now: Timestamp) -> Option<IntervalEvent> {
        if now <= previous {
            return None;
        }
        let index = self.position(now).index;
        if index <= self.position(previous).index {
            return None;
        }
        Some(match self.phase_of(index) {
            (IntervalPhase::Work, round) => IntervalEvent::WorkStarted { round },
            (IntervalPhase::Rest, round) => IntervalEvent::RestStarted { round },
            (IntervalPhase::Done, _) => IntervalEvent::Completed,
        })
    }

    /// The phase change to announce now, if any, given everything observed so far.
    pub fn observe(&mut self, now: Timestamp) -> Option<IntervalEvent> {
        let previous = self.observed_until;
        self.observed_until = previous.max(now);
        self.event_between(previous, now)
    }

    fn position(&self, at: Timestamp) -> Position {
        let elapsed = at.saturating_millis_since(self.started_at);
        if elapsed >= self.plan.total_ms() {
            return Position {
                index: self.plan.done_index(),
                remaining_ms: 0,
            };
        }
        let cycle = self.plan.cycle_ms();
        let round = elapsed.checked_div(cycle).unwrap_or(0);
        let offset = elapsed.checked_rem(cycle).unwrap_or(0);
        let work = self.plan.work_ms.get();
        if offset < work {
            Position {
                index: round * 2,
                remaining_ms: work - offset,
            }
        } else {
            Position {
                index: round * 2 + 1,
                remaining_ms: cycle - offset,
            }
        }
    }

    fn phase_of(&self, index: u64) -> (IntervalPhase, u32) {
        if index >= self.plan.done_index() {
            return (IntervalPhase::Done, self.plan.rounds());
        }
        let round = u32::try_from(index / 2 + 1).unwrap_or(u32::MAX);
        if index.is_multiple_of(2) {
            (IntervalPhase::Work, round)
        } else {
            (IntervalPhase::Rest, round)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset_ms: i64) -> Timestamp {
        Timestamp::from_epoch_millis(1_700_000_000_000 + offset_ms)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// 3 rounds of 30s work and 15s rest: work 0-30, rest 30-45, work 45-75, rest 75-90,
    /// work 90-120, done at 120.
    fn treadmill() -> IntervalTimer {
        IntervalTimer::start(at(0), IntervalPlan::new(secs(30), secs(15), 3).unwrap())
    }

    fn status(phase: IntervalPhase, round: u32, remaining_ms: u64) -> IntervalStatus {
        IntervalStatus {
            phase,
            round,
            remaining_in_phase: Duration::from_millis(remaining_ms),
        }
    }

    fn observe_all(timer: &mut IntervalTimer, offsets: &[i64]) -> Vec<(i64, IntervalEvent)> {
        offsets
            .iter()
            .filter_map(|&offset| timer.observe(at(offset)).map(|event| (offset, event)))
            .collect()
    }

    #[test]
    fn a_plan_exposes_its_parts() {
        let plan = IntervalPlan::new(secs(30), secs(15), 3).unwrap();
        assert_eq!(plan.work(), secs(30));
        assert_eq!(plan.rest(), secs(15));
        assert_eq!(plan.rounds(), 3);
        assert_eq!(plan.total(), secs(120));
    }

    #[test]
    fn a_single_round_has_no_rest() {
        let plan = IntervalPlan::new(secs(20), secs(10), 1).unwrap();
        assert_eq!(plan.total(), secs(20));
        let mut timer = IntervalTimer::start(at(0), plan);
        assert_eq!(timer.status(at(19_999)), status(IntervalPhase::Work, 1, 1));
        assert_eq!(timer.status(at(20_000)), status(IntervalPhase::Done, 1, 0));
        assert_eq!(
            observe_all(&mut timer, &[10_000, 20_000, 25_000]),
            vec![(20_000, IntervalEvent::Completed)]
        );
    }

    #[test]
    fn rejects_invalid_plans() {
        assert_eq!(
            IntervalPlan::new(Duration::ZERO, secs(10), 3),
            Err(IntervalPlanError::ZeroWork)
        );
        assert_eq!(
            IntervalPlan::new(Duration::from_micros(999), secs(10), 3),
            Err(IntervalPlanError::ZeroWork)
        );
        assert_eq!(
            IntervalPlan::new(secs(30), secs(10), 0),
            Err(IntervalPlanError::ZeroRounds)
        );
        assert_eq!(
            IntervalPlan::new(Duration::MAX, Duration::ZERO, 2),
            Err(IntervalPlanError::TooLong)
        );
        assert_eq!(
            IntervalPlan::new(Duration::MAX, secs(1), 1),
            Err(IntervalPlanError::TooLong)
        );
        assert_eq!(
            IntervalPlan::new(Duration::from_millis(u64::MAX / 2 + 1), Duration::ZERO, 2),
            Err(IntervalPlanError::TooLong)
        );
    }

    #[test]
    fn accepts_the_largest_plans() {
        let plan = IntervalPlan::new(Duration::MAX, Duration::ZERO, 1).unwrap();
        assert_eq!(plan.total(), Duration::from_millis(u64::MAX));
        let timer = IntervalTimer::start(at(0), plan);
        assert_eq!(timer.ends_at(), Timestamp::from_epoch_millis(i64::MAX));
        assert_eq!(timer.status(at(1)).phase, IntervalPhase::Work);

        let plan = IntervalPlan::new(Duration::from_millis(1), Duration::ZERO, u32::MAX).unwrap();
        let timer = IntervalTimer::start(at(0), plan);
        assert_eq!(timer.status(at(5)), status(IntervalPhase::Work, 6, 1));
        let last = i64::from(u32::MAX) - 1;
        assert_eq!(
            timer.status(at(last)),
            status(IntervalPhase::Work, u32::MAX, 1)
        );
        assert_eq!(
            timer.status(at(last + 1)),
            status(IntervalPhase::Done, u32::MAX, 0)
        );
    }

    #[test]
    fn errors_have_messages() {
        assert_eq!(
            IntervalPlanError::ZeroWork.to_string(),
            "the work phase must last at least one millisecond"
        );
        assert_eq!(
            IntervalPlanError::ZeroRounds.to_string(),
            "an interval plan needs at least one round"
        );
        assert_eq!(
            IntervalPlanError::TooLong.to_string(),
            "the interval plan is too long"
        );
    }

    #[test]
    fn exposes_its_start_and_end() {
        let timer = treadmill();
        assert_eq!(
            timer.plan(),
            IntervalPlan::new(secs(30), secs(15), 3).unwrap()
        );
        assert_eq!(timer.started_at(), at(0));
        assert_eq!(timer.ends_at(), at(120_000));
        assert_eq!(timer.observed_until(), at(0));
        assert_eq!(timer.remaining(at(0)), secs(120));
        assert_eq!(timer.remaining(at(100_000)), secs(20));
        assert_eq!(timer.remaining(at(200_000)), Duration::ZERO);
        assert!(!timer.is_finished(at(119_999)));
        assert!(timer.is_finished(at(120_000)));
    }

    #[test]
    fn derives_the_phase_at_every_boundary() {
        let timer = treadmill();
        let expected = [
            (0, status(IntervalPhase::Work, 1, 30_000)),
            (1, status(IntervalPhase::Work, 1, 29_999)),
            (29_999, status(IntervalPhase::Work, 1, 1)),
            (30_000, status(IntervalPhase::Rest, 1, 15_000)),
            (44_999, status(IntervalPhase::Rest, 1, 1)),
            (45_000, status(IntervalPhase::Work, 2, 30_000)),
            (74_999, status(IntervalPhase::Work, 2, 1)),
            (75_000, status(IntervalPhase::Rest, 2, 15_000)),
            (89_999, status(IntervalPhase::Rest, 2, 1)),
            (90_000, status(IntervalPhase::Work, 3, 30_000)),
            (119_999, status(IntervalPhase::Work, 3, 1)),
            (120_000, status(IntervalPhase::Done, 3, 0)),
            (120_001, status(IntervalPhase::Done, 3, 0)),
            (10_000_000, status(IntervalPhase::Done, 3, 0)),
        ];
        for (offset, want) in expected {
            assert_eq!(timer.status(at(offset)), want, "at {offset}ms");
        }
    }

    #[test]
    fn before_the_start_is_the_start_of_the_first_work_phase() {
        let timer = treadmill();
        assert_eq!(
            timer.status(at(-5_000)),
            status(IntervalPhase::Work, 1, 30_000)
        );
        assert_eq!(
            timer.status(Timestamp::from_epoch_millis(i64::MIN)),
            status(IntervalPhase::Work, 1, 30_000)
        );
    }

    #[test]
    fn a_second_by_second_tick_raises_every_phase_change_once() {
        let mut timer = treadmill();
        let ticks: Vec<i64> = (0..=150).map(|s| s * 1_000).collect();
        assert_eq!(
            observe_all(&mut timer, &ticks),
            vec![
                (30_000, IntervalEvent::RestStarted { round: 1 }),
                (45_000, IntervalEvent::WorkStarted { round: 2 }),
                (75_000, IntervalEvent::RestStarted { round: 2 }),
                (90_000, IntervalEvent::WorkStarted { round: 3 }),
                (120_000, IntervalEvent::Completed),
            ]
        );
    }

    #[test]
    fn repeated_observations_at_the_same_instant_raise_once() {
        let mut timer = treadmill();
        assert_eq!(
            observe_all(&mut timer, &[30_000, 30_000, 30_000]),
            vec![(30_000, IntervalEvent::RestStarted { round: 1 })]
        );
    }

    #[test]
    fn a_gap_over_several_phases_raises_only_the_current_one() {
        let mut timer = treadmill();
        assert_eq!(
            observe_all(&mut timer, &[1_000, 80_000, 81_000, 500_000, 600_000]),
            vec![
                (80_000, IntervalEvent::RestStarted { round: 2 }),
                (500_000, IntervalEvent::Completed)
            ]
        );
    }

    #[test]
    fn a_gap_from_the_start_to_the_end_only_completes() {
        let mut timer = treadmill();
        assert_eq!(
            observe_all(&mut timer, &[1_000, 3_600_000]),
            vec![(3_600_000, IntervalEvent::Completed)]
        );
        assert_eq!(timer.observe(Timestamp::from_epoch_millis(i64::MAX)), None);
    }

    #[test]
    fn a_clock_going_backwards_raises_nothing_twice() {
        let mut timer = treadmill();
        assert_eq!(
            timer.observe(at(46_000)),
            Some(IntervalEvent::WorkStarted { round: 2 })
        );
        assert_eq!(timer.observe(at(20_000)), None);
        assert_eq!(timer.observed_until(), at(46_000));
        assert_eq!(timer.observe(at(31_000)), None);
        assert_eq!(timer.observe(at(47_000)), None);
        assert_eq!(
            timer.observe(at(75_000)),
            Some(IntervalEvent::RestStarted { round: 2 })
        );
        assert_eq!(timer.observe(at(-1_000)), None);
    }

    #[test]
    fn a_zero_rest_chains_the_work_phases() {
        let plan = IntervalPlan::new(secs(10), Duration::ZERO, 3).unwrap();
        assert_eq!(plan.total(), secs(30));
        let mut timer = IntervalTimer::start(at(0), plan);
        assert_eq!(timer.status(at(9_999)), status(IntervalPhase::Work, 1, 1));
        assert_eq!(
            timer.status(at(10_000)),
            status(IntervalPhase::Work, 2, 10_000)
        );
        let ticks: Vec<i64> = (0..=40).map(|s| s * 1_000).collect();
        assert_eq!(
            observe_all(&mut timer, &ticks),
            vec![
                (10_000, IntervalEvent::WorkStarted { round: 2 }),
                (20_000, IntervalEvent::WorkStarted { round: 3 }),
                (30_000, IntervalEvent::Completed),
            ]
        );
    }

    #[test]
    fn event_between_is_pure_and_ignores_empty_windows() {
        let timer = treadmill();
        assert_eq!(
            timer.event_between(at(0), at(30_000)),
            Some(IntervalEvent::RestStarted { round: 1 })
        );
        assert_eq!(timer.event_between(at(0), at(29_999)), None);
        assert_eq!(timer.event_between(at(30_000), at(30_000)), None);
        assert_eq!(timer.event_between(at(50_000), at(40_000)), None);
        assert_eq!(timer.event_between(at(-10_000), at(0)), None);
        assert_eq!(timer.observed_until(), at(0));
    }

    #[test]
    fn round_trips_through_serde() {
        let mut timer = treadmill();
        assert_eq!(
            timer.observe(at(31_000)),
            Some(IntervalEvent::RestStarted { round: 1 })
        );
        let json = serde_json::to_string(&timer).unwrap();
        assert_eq!(
            json,
            r#"{"plan":{"work_ms":30000,"rest_ms":15000,"rounds":3},"started_at":1700000000000,"observed_until":1700000031000}"#
        );
        let mut restored: IntervalTimer = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, timer);
        assert_eq!(restored.observe(at(32_000)), None);
    }

    #[test]
    fn deserialization_validates_the_plan() {
        let plan = |work: u64, rest: u64, rounds: u32| {
            serde_json::from_str::<IntervalPlan>(&format!(
                r#"{{"work_ms":{work},"rest_ms":{rest},"rounds":{rounds}}}"#
            ))
        };
        assert_eq!(
            plan(30_000, 15_000, 3).unwrap(),
            IntervalPlan::new(secs(30), secs(15), 3).unwrap()
        );
        assert!(
            plan(0, 15_000, 3)
                .unwrap_err()
                .to_string()
                .contains("one millisecond")
        );
        assert!(
            plan(30_000, 15_000, 0)
                .unwrap_err()
                .to_string()
                .contains("one round")
        );
        assert!(
            plan(u64::MAX, 1, 1)
                .unwrap_err()
                .to_string()
                .contains("too long")
        );
    }
}

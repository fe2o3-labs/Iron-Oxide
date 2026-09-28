//! Personal records (PRs) of one exercise, and the PR events a session produces.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{E1rmFormula, Lift, PerformedSet, best_e1rm, top_set};
use crate::{ExerciseId, Reps, Weight};

/// A personal record set in a session, for the end-of-session summary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrEvent {
    /// The exercise the record is for.
    pub exercise: ExerciseId,
    /// What was beaten.
    pub kind: PrKind,
}

/// The kind of personal record, with the new and the previous best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrKind {
    /// Heavier than any weight lifted before. `lift` is the session's top set.
    HeaviestWeight {
        /// The session's top set.
        lift: Lift,
        /// The heaviest weight lifted before.
        previous: Weight,
    },
    /// A higher estimated one-rep max than ever before.
    BestE1rm {
        /// The new best estimate.
        e1rm: Weight,
        /// The set that produced it.
        lift: Lift,
        /// The best estimate before.
        previous: Weight,
    },
    /// More reps than ever done at this weight or heavier.
    RepsAtWeight {
        /// The set: `lift.reps` at `lift.weight`.
        lift: Lift,
        /// The most reps done before at `lift.weight` or heavier.
        previous: Reps,
    },
}

/// The best performances so far on one exercise, used to find the PRs of a new session.
///
/// Build it from every earlier session's sets ([`ExerciseRecords::from_history`] or
/// [`ExerciseRecords::record`]), ask for the PRs of the new session ([`ExerciseRecords::prs`]), then
/// record that session too. Only working sets with at least one rep are recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExerciseRecords {
    formula: E1rmFormula,
    heaviest: Option<Weight>,
    best_e1rm: Option<Weight>,
    /// For each weight lifted, the most reps done at exactly that weight.
    max_reps: BTreeMap<Weight, Reps>,
}

impl ExerciseRecords {
    /// No history yet. `formula` is used for e1RM records.
    #[must_use]
    pub const fn new(formula: E1rmFormula) -> Self {
        Self {
            formula,
            heaviest: None,
            best_e1rm: None,
            max_reps: BTreeMap::new(),
        }
    }

    /// Records every set of the history. The order does not matter.
    pub fn from_history(
        sets: impl IntoIterator<Item = PerformedSet>,
        formula: E1rmFormula,
    ) -> Self {
        let mut records = Self::new(formula);
        records.extend(sets);
        records
    }

    /// Adds one set to the history. Warm-ups and 0-rep sets are ignored.
    pub fn record(&mut self, set: PerformedSet) {
        if !set.counts() {
            return;
        }
        self.heaviest = self.heaviest.max(Some(set.weight));
        self.best_e1rm = self.best_e1rm.max(set.e1rm(self.formula));
        let reps = self.max_reps.entry(set.weight).or_insert(set.reps);
        *reps = (*reps).max(set.reps);
    }

    /// The formula used for e1RM records.
    #[must_use]
    pub const fn formula(&self) -> E1rmFormula {
        self.formula
    }

    /// Whether no set has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.max_reps.is_empty()
    }

    /// The heaviest weight lifted.
    #[must_use]
    pub const fn heaviest(&self) -> Option<Weight> {
        self.heaviest
    }

    /// The best estimated one-rep max. `None` also when every recorded set had too many reps for an
    /// estimate.
    #[must_use]
    pub const fn best_e1rm(&self) -> Option<Weight> {
        self.best_e1rm
    }

    /// The most reps done at `weight` or heavier, or `None` if nothing that heavy was lifted.
    #[must_use]
    pub fn max_reps_at_least(&self, weight: Weight) -> Option<Reps> {
        self.max_reps.range(weight..).map(|(_, reps)| *reps).max()
    }

    /// The PRs that `session` (the sets of this exercise in one new session) sets against the
    /// recorded history. The session itself is not recorded.
    ///
    /// Rules:
    /// - No PR is reported while the history is empty: the first session sets the baseline.
    /// - [`PrKind::HeaviestWeight`]: the session's top set is strictly heavier than anything before.
    ///   At most one.
    /// - [`PrKind::BestE1rm`]: the session's best estimate is strictly higher than the best before.
    ///   At most one, and none if the history has no estimate to beat.
    /// - [`PrKind::RepsAtWeight`]: a set has more reps than ever done at its weight or heavier. A set
    ///   heavier than anything before is a weight PR, not a rep PR. When one session set beats
    ///   another on both weight and reps, only the better one is reported, so `100 kg × 8` hides
    ///   `95 kg × 8` and `100 kg × 7`.
    ///
    /// Events come in that order; rep PRs from heaviest to lightest.
    #[must_use]
    pub fn prs(
        &self,
        exercise: &ExerciseId,
        session: impl IntoIterator<Item = PerformedSet>,
    ) -> Vec<PrEvent> {
        // `heaviest` is set by the first recorded set, so this also covers an empty history.
        let Some(heaviest) = self.heaviest else {
            return Vec::new();
        };
        let sets: Vec<PerformedSet> = session.into_iter().filter(|set| set.counts()).collect();
        let mut kinds = Vec::new();

        if let Some(top) = top_set(sets.iter().copied())
            && top.weight > heaviest
        {
            kinds.push(PrKind::HeaviestWeight {
                lift: top,
                previous: heaviest,
            });
        }

        if let (Some(previous), Some((e1rm, lift))) = (
            self.best_e1rm,
            best_e1rm(sets.iter().copied(), self.formula),
        ) && e1rm > previous
        {
            kinds.push(PrKind::BestE1rm {
                e1rm,
                lift,
                previous,
            });
        }

        let mut rep_prs: Vec<(Lift, Reps)> = sets
            .iter()
            .map(|set| set.lift())
            .filter_map(|lift| {
                self.max_reps_at_least(lift.weight)
                    .filter(|previous| lift.reps > *previous)
                    .map(|previous| (lift, previous))
            })
            .collect();
        // Heaviest first, then most reps first: a set is dominated exactly when an earlier one has
        // at least as many reps.
        rep_prs.sort_by_key(|(lift, _)| Reverse(*lift));
        let mut most_reps = Reps::ZERO;
        for (lift, previous) in rep_prs {
            if lift.reps > most_reps {
                most_reps = lift.reps;
                kinds.push(PrKind::RepsAtWeight { lift, previous });
            }
        }

        kinds
            .into_iter()
            .map(|kind| PrEvent {
                exercise: exercise.clone(),
                kind,
            })
            .collect()
    }
}

impl Default for ExerciseRecords {
    /// No history, with the default formula.
    fn default() -> Self {
        Self::new(E1rmFormula::default())
    }
}

impl Extend<PerformedSet> for ExerciseRecords {
    fn extend<I: IntoIterator<Item = PerformedSet>>(&mut self, sets: I) {
        for set in sets {
            self.record(set);
        }
    }
}

/// The PRs that `session` sets against `history`, both the sets of `exercise` only. Shorthand for
/// [`ExerciseRecords::from_history`] then [`ExerciseRecords::prs`].
pub fn detect_prs(
    exercise: &ExerciseId,
    history: impl IntoIterator<Item = PerformedSet>,
    session: impl IntoIterator<Item = PerformedSet>,
    formula: E1rmFormula,
) -> Vec<PrEvent> {
    ExerciseRecords::from_history(history, formula).prs(exercise, session)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::stats::test_support::{kg, reps, warm, work};

    fn squat() -> ExerciseId {
        ExerciseId::new("back-squat").unwrap()
    }

    fn lift(weight: f64, count: u16) -> Lift {
        Lift {
            weight: kg(weight),
            reps: reps(count),
        }
    }

    fn kinds(
        history: impl IntoIterator<Item = PerformedSet>,
        session: impl IntoIterator<Item = PerformedSet>,
    ) -> Vec<PrKind> {
        let events = detect_prs(&squat(), history, session, E1rmFormula::Epley);
        assert!(events.iter().all(|event| event.exercise == squat()));
        events.into_iter().map(|event| event.kind).collect()
    }

    #[test]
    fn records_accumulate_working_sets_only() {
        let records = ExerciseRecords::from_history(
            [
                warm(200.0, 1),
                work(180.0, 0),
                work(100.0, 5),
                work(120.0, 2),
                work(100.0, 8),
                work(60.0, 20),
            ],
            E1rmFormula::Epley,
        );
        assert!(!records.is_empty());
        assert_eq!(records.formula(), E1rmFormula::Epley);
        assert_eq!(records.heaviest(), Some(kg(120.0)));
        // Epley: 100 × 5 → 116.67, 120 × 2 → 128, 100 × 8 → 126.67, 60 × 20 → none.
        assert_eq!(records.best_e1rm(), Some(kg(128.0)));
        assert_eq!(records.max_reps_at_least(kg(100.0)), Some(reps(8)));
        assert_eq!(records.max_reps_at_least(kg(100.5)), Some(reps(2)));
        assert_eq!(records.max_reps_at_least(kg(50.0)), Some(reps(20)));
        assert_eq!(records.max_reps_at_least(kg(120.5)), None);
    }

    #[test]
    fn empty_records() {
        let records = ExerciseRecords::default();
        assert!(records.is_empty());
        assert_eq!(records.formula(), E1rmFormula::Epley);
        assert_eq!(records.heaviest(), None);
        assert_eq!(records.best_e1rm(), None);
        assert_eq!(records.max_reps_at_least(Weight::ZERO), None);
        let only_warmups = ExerciseRecords::from_history([warm(60.0, 5)], E1rmFormula::Brzycki);
        assert!(only_warmups.is_empty());
        assert_eq!(only_warmups.formula(), E1rmFormula::Brzycki);
    }

    #[test]
    fn no_prs_without_history() {
        assert_eq!(kinds([], [work(100.0, 5)]), []);
        assert_eq!(kinds([warm(60.0, 5), work(80.0, 0)], [work(100.0, 5)]), []);
    }

    #[test]
    fn no_prs_for_an_empty_or_warmup_only_session() {
        let history = [work(100.0, 5)];
        assert_eq!(kinds(history, []), []);
        assert_eq!(kinds(history, [warm(200.0, 20), work(200.0, 0)]), []);
    }

    #[test]
    fn repeating_the_best_is_not_a_pr() {
        let history = [work(100.0, 5), work(110.0, 1)];
        assert_eq!(kinds(history, [work(100.0, 5), work(110.0, 1)]), []);
    }

    #[test]
    fn heaviest_weight_pr() {
        // 105 × 1 → e1RM 105, below 100 × 5 → 116.67: only the weight PR.
        assert_eq!(
            kinds([work(100.0, 5)], [work(90.0, 5), work(105.0, 1)]),
            [PrKind::HeaviestWeight {
                lift: lift(105.0, 1),
                previous: kg(100.0),
            }]
        );
    }

    #[test]
    fn heaviest_weight_pr_reports_the_top_set() {
        let session = [work(105.0, 1), work(105.0, 2), work(102.5, 3)];
        let events = kinds([work(100.0, 1)], session);
        assert_eq!(
            events[0],
            PrKind::HeaviestWeight {
                lift: lift(105.0, 2),
                previous: kg(100.0),
            }
        );
    }

    #[test]
    fn best_e1rm_pr() {
        // History best: 100 × 5 → 116.67. Session: 90 × 10 → 120.
        assert_eq!(
            kinds([work(100.0, 5)], [work(90.0, 10)]),
            [
                PrKind::BestE1rm {
                    e1rm: kg(120.0),
                    lift: lift(90.0, 10),
                    previous: estimate(100.0, 5),
                },
                PrKind::RepsAtWeight {
                    lift: lift(90.0, 10),
                    previous: reps(5),
                },
            ]
        );
    }

    fn estimate(weight: f64, count: u16) -> Weight {
        E1rmFormula::Epley
            .estimate(kg(weight), reps(count))
            .unwrap()
    }

    #[test]
    fn equal_e1rm_is_not_a_pr() {
        // 120 × 1 and 90 × 10 both estimate 120: only the weight PR.
        assert_eq!(
            kinds([work(90.0, 10)], [work(120.0, 1)]),
            [PrKind::HeaviestWeight {
                lift: lift(120.0, 1),
                previous: kg(90.0),
            }]
        );
    }

    #[test]
    fn no_e1rm_pr_when_history_has_no_estimate() {
        // History only has high-rep sets: no estimate to beat.
        assert_eq!(kinds([work(60.0, 15)], [work(50.0, 10)]), []);
    }

    #[test]
    fn no_e1rm_pr_when_the_session_has_no_estimate() {
        // 60 × 20 has no estimate, but it is a rep PR at 60 kg.
        assert_eq!(
            kinds([work(60.0, 15), work(100.0, 3)], [work(60.0, 20)]),
            [PrKind::RepsAtWeight {
                lift: lift(60.0, 20),
                previous: reps(15),
            }]
        );
    }

    #[test]
    fn all_three_kinds_in_order() {
        let history = [work(100.0, 5), work(80.0, 8)];
        let session = [work(110.0, 5), work(80.0, 10)];
        assert_eq!(
            kinds(history, session),
            [
                PrKind::HeaviestWeight {
                    lift: lift(110.0, 5),
                    previous: kg(100.0),
                },
                PrKind::BestE1rm {
                    e1rm: estimate(110.0, 5),
                    lift: lift(110.0, 5),
                    previous: estimate(100.0, 5),
                },
                PrKind::RepsAtWeight {
                    lift: lift(80.0, 10),
                    previous: reps(8),
                },
            ]
        );
    }

    #[test]
    fn rep_pr_counts_heavier_history() {
        // 10 reps at 95 kg is not a PR: 10 reps were done at 100 kg.
        assert_eq!(kinds([work(100.0, 10)], [work(95.0, 10)]), []);
        // 11 reps at 95 kg beats the 10 at 100 kg.
        assert_eq!(
            kinds([work(100.0, 10)], [work(95.0, 11)])
                .into_iter()
                .filter(|kind| matches!(kind, PrKind::RepsAtWeight { .. }))
                .collect::<Vec<_>>(),
            [PrKind::RepsAtWeight {
                lift: lift(95.0, 11),
                previous: reps(10),
            }]
        );
    }

    #[test]
    fn rep_pr_ignores_lighter_history() {
        // 20 reps at 60 kg says nothing about 100 kg.
        assert_eq!(
            kinds([work(60.0, 20), work(100.0, 5)], [work(100.0, 6)])
                .into_iter()
                .filter(|kind| matches!(kind, PrKind::RepsAtWeight { .. }))
                .collect::<Vec<_>>(),
            [PrKind::RepsAtWeight {
                lift: lift(100.0, 6),
                previous: reps(5),
            }]
        );
    }

    #[test]
    fn a_new_heaviest_weight_is_not_also_a_rep_pr() {
        assert_eq!(
            kinds([work(100.0, 5)], [work(102.5, 1)]),
            [PrKind::HeaviestWeight {
                lift: lift(102.5, 1),
                previous: kg(100.0),
            }]
        );
    }

    #[test]
    fn dominated_rep_prs_are_hidden() {
        // History: 12 × 60, 5 × 100. All session sets beat their history, but 100 × 8 dominates
        // 95 × 8, 100 × 7 and the duplicate, 80 × 10 dominates 70 × 9, and 60 × 13 is not dominated.
        let history = [work(60.0, 12), work(100.0, 5)];
        let session = [
            work(95.0, 8),
            work(100.0, 7),
            work(100.0, 8),
            work(100.0, 8),
            work(80.0, 10),
            work(60.0, 13),
            work(70.0, 9),
        ];
        let rep_prs: Vec<PrKind> = kinds(history, session)
            .into_iter()
            .filter(|kind| matches!(kind, PrKind::RepsAtWeight { .. }))
            .collect();
        assert_eq!(
            rep_prs,
            [
                PrKind::RepsAtWeight {
                    lift: lift(100.0, 8),
                    previous: reps(5),
                },
                PrKind::RepsAtWeight {
                    lift: lift(80.0, 10),
                    previous: reps(5),
                },
                PrKind::RepsAtWeight {
                    lift: lift(60.0, 13),
                    previous: reps(12),
                },
            ]
        );
    }

    #[test]
    fn records_then_detects_session_by_session() {
        let mut records = ExerciseRecords::default();
        let sessions = [
            vec![work(100.0, 5)],
            vec![work(102.5, 5)],
            vec![work(102.5, 5)],
            vec![work(102.5, 6)],
        ];
        let mut counts = Vec::new();
        for session in sessions {
            counts.push(records.prs(&squat(), session.iter().copied()).len());
            records.extend(session);
        }
        // Baseline; weight + e1RM; nothing; e1RM + reps.
        assert_eq!(counts, [0, 2, 0, 2]);
    }

    #[test]
    fn brzycki_records_use_brzycki() {
        let records = ExerciseRecords::from_history([work(100.0, 5)], E1rmFormula::Brzycki);
        assert_eq!(records.best_e1rm(), Some(kg(112.5)));
        // Brzycki 100 × 6 → 116.13 > 112.5.
        let events = records.prs(&squat(), [work(100.0, 6)]);
        assert!(matches!(
            events[0].kind,
            PrKind::BestE1rm { previous, .. } if previous == kg(112.5)
        ));
    }

    #[test]
    fn event_serde() {
        let event = PrEvent {
            exercise: squat(),
            kind: PrKind::RepsAtWeight {
                lift: lift(100.0, 8),
                previous: reps(5),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"exercise":"back-squat","kind":{"kind":"reps_at_weight","lift":{"weight":100.0,"reps":8},"previous":5}}"#
        );
        assert_eq!(serde_json::from_str::<PrEvent>(&json).unwrap(), event);
    }

    fn arb_set() -> impl Strategy<Value = PerformedSet> {
        // Whole 2.5 kg steps up to 300 kg keep ties frequent.
        (0..=120_u32, 0..=15_u16, prop::bool::weighted(0.2)).prop_map(|(steps, count, warmup)| {
            PerformedSet {
                weight: kg(f64::from(steps) * 2.5),
                reps: reps(count),
                warmup,
            }
        })
    }

    proptest! {
        #[test]
        fn prs_match_a_brute_force_check(
            history in prop::collection::vec(arb_set(), 0..12),
            session in prop::collection::vec(arb_set(), 0..8),
        ) {
            let events = kinds(history.iter().copied(), session.iter().copied());
            let past: Vec<Lift> = history.iter().filter(|s| s.counts()).map(|s| s.lift()).collect();
            let now: Vec<Lift> = session.iter().filter(|s| s.counts()).map(|s| s.lift()).collect();
            if past.is_empty() {
                prop_assert!(events.is_empty());
                return Ok(());
            }
            let past_heaviest = past.iter().map(|l| l.weight).max();
            let now_top = now.iter().copied().max();
            let expect_weight = now_top.is_some_and(|top| Some(top.weight) > past_heaviest);
            prop_assert_eq!(
                events.iter().any(|e| matches!(e, PrKind::HeaviestWeight { .. })),
                expect_weight
            );
            let e1rm = |l: &Lift| E1rmFormula::Epley.estimate(l.weight, l.reps);
            let past_e1rm = past.iter().filter_map(e1rm).max();
            let now_e1rm = now.iter().filter_map(e1rm).max();
            let expect_e1rm = past_e1rm.is_some() && now_e1rm > past_e1rm;
            prop_assert_eq!(
                events.iter().any(|e| matches!(e, PrKind::BestE1rm { .. })),
                expect_e1rm
            );
            // Every reported rep PR beats all history at its weight or heavier.
            for event in &events {
                if let PrKind::RepsAtWeight { lift, previous } = event {
                    let best = past.iter().filter(|p| p.weight >= lift.weight).map(|p| p.reps).max();
                    prop_assert_eq!(best, Some(*previous));
                    prop_assert!(lift.reps > *previous);
                    prop_assert!(now.contains(lift));
                }
            }
            // Every session set that beats its history is reported or dominated by a reported one.
            for set in &now {
                let best = past.iter().filter(|p| p.weight >= set.weight).map(|p| p.reps).max();
                if best.is_some_and(|best| set.reps > best) {
                    let covered = events.iter().any(|e| matches!(
                        e,
                        PrKind::RepsAtWeight { lift, .. }
                            if lift.weight >= set.weight && lift.reps >= set.reps
                    ));
                    prop_assert!(covered, "{:?} is not reported", set);
                }
            }
        }
    }
}

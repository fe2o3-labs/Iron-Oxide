//! Per-exercise time series for the history charts.

use serde::{Deserialize, Serialize};

use super::{E1rmFormula, Lift, PerformedSet, best_e1rm, top_set};
use crate::Weight;

/// One session of one exercise on a history chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SeriesPoint<K> {
    /// Where the session sits on the x axis: a date, a timestamp or a session number.
    pub key: K,
    /// The session's top set (heaviest weight, then most reps).
    pub top_set: Lift,
    /// The session's best estimated one-rep max, over all its working sets (not only the top set).
    /// `None` when every set had too many reps for an estimate.
    pub best_e1rm: Option<Weight>,
}

/// Builds the chart series of one exercise: one point per session, sorted by `key`.
///
/// Each item is a session's ordering key (any `Ord` type, so this does not depend on a time type)
/// and the sets of the exercise in that session. Sessions without a completed working set (only
/// warm-ups or failed attempts) have no point. Sessions with equal keys keep their input order.
pub fn exercise_series<K, S>(
    sessions: impl IntoIterator<Item = (K, S)>,
    formula: E1rmFormula,
) -> Vec<SeriesPoint<K>>
where
    K: Ord,
    S: IntoIterator<Item = PerformedSet>,
{
    let mut points: Vec<SeriesPoint<K>> = sessions
        .into_iter()
        .filter_map(|(key, sets)| {
            let sets: Vec<PerformedSet> = sets.into_iter().collect();
            top_set(sets.iter().copied()).map(|top_set| SeriesPoint {
                key,
                top_set,
                best_e1rm: best_e1rm(sets, formula).map(|(e1rm, _)| e1rm),
            })
        })
        .collect();
    points.sort_by(|a, b| a.key.cmp(&b.key));
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::test_support::{kg, reps, warm, work};

    fn lift(weight: f64, count: u16) -> Lift {
        Lift {
            weight: kg(weight),
            reps: reps(count),
        }
    }

    fn e1rm(weight: f64, count: u16) -> Option<Weight> {
        E1rmFormula::Epley.estimate(kg(weight), reps(count))
    }

    #[test]
    fn one_point_per_session_sorted_by_key() {
        let sessions = [
            (3_u32, vec![warm(60.0, 5), work(105.0, 5), work(105.0, 4)]),
            (1, vec![work(100.0, 5)]),
            (2, vec![work(102.5, 5), work(90.0, 10)]),
        ];
        let series = exercise_series(sessions, E1rmFormula::Epley);
        assert_eq!(
            series,
            [
                SeriesPoint {
                    key: 1,
                    top_set: lift(100.0, 5),
                    best_e1rm: e1rm(100.0, 5),
                },
                SeriesPoint {
                    key: 2,
                    top_set: lift(102.5, 5),
                    // 90 × 10 → 120 beats the top set's 102.5 × 5 → 119.58.
                    best_e1rm: e1rm(90.0, 10),
                },
                SeriesPoint {
                    key: 3,
                    top_set: lift(105.0, 5),
                    best_e1rm: e1rm(105.0, 5),
                },
            ]
        );
    }

    #[test]
    fn skips_sessions_without_a_completed_working_set() {
        let sessions = [
            ("2026-01-01", vec![]),
            ("2026-01-02", vec![warm(60.0, 5)]),
            ("2026-01-03", vec![work(100.0, 0)]),
            ("2026-01-04", vec![work(100.0, 3)]),
        ];
        let series = exercise_series(sessions, E1rmFormula::Epley);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].key, "2026-01-04");
    }

    #[test]
    fn best_e1rm_is_none_for_high_rep_sessions() {
        let series = exercise_series([(1, vec![work(60.0, 20)])], E1rmFormula::Epley);
        assert_eq!(
            series,
            [SeriesPoint {
                key: 1,
                top_set: lift(60.0, 20),
                best_e1rm: None,
            }]
        );
    }

    #[test]
    fn equal_keys_keep_input_order() {
        let sessions = [
            (1, vec![work(110.0, 1)]),
            (0, vec![work(90.0, 1)]),
            (1, vec![work(100.0, 1)]),
        ];
        let weights: Vec<Weight> = exercise_series(sessions, E1rmFormula::Epley)
            .into_iter()
            .map(|point| point.top_set.weight)
            .collect();
        assert_eq!(weights, [kg(90.0), kg(110.0), kg(100.0)]);
    }

    #[test]
    fn uses_the_given_formula() {
        let series = exercise_series([(1, vec![work(100.0, 5)])], E1rmFormula::Brzycki);
        assert_eq!(series[0].best_e1rm, Some(kg(112.5)));
    }

    #[test]
    fn empty_input() {
        let sessions: Vec<(u32, Vec<PerformedSet>)> = Vec::new();
        assert!(exercise_series(sessions, E1rmFormula::Epley).is_empty());
    }

    #[test]
    fn point_serde() {
        let point = SeriesPoint {
            key: 7_u32,
            top_set: lift(100.0, 5),
            best_e1rm: Some(kg(116.0)),
        };
        let json = serde_json::to_string(&point).unwrap();
        assert_eq!(
            json,
            r#"{"key":7,"top_set":{"weight":100.0,"reps":5},"best_e1rm":116.0}"#
        );
        assert_eq!(
            serde_json::from_str::<SeriesPoint<u32>>(&json).unwrap(),
            point
        );
    }
}

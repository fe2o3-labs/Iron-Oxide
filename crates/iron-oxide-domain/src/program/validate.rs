//! Semantic checks on a parsed program. Every broken rule is reported, each with its JSON path.

use std::collections::{BTreeMap, BTreeSet};
use std::mem;

use super::error::{JsonPath, ValidationError, ValidationErrorKind as Kind, ValidationErrors};
use super::ids::SupersetId;
use super::limits::*;
use super::model::{Day, Exercise, Program, ProgressionRule, WarmupSet, Work};
use super::values::{Load, RepTarget, UnitWeight, WarmupLoad};
use crate::{ExerciseId, Percent, Seconds, Unit};

/// The document format version this crate reads and writes.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Basis points (hundredths of a percent) in one percent.
const BASIS_POINTS: u32 = 100;

/// How an exercise's load is expressed, for the cross-day consistency check.
#[derive(PartialEq, Eq)]
enum LoadKind {
    Bodyweight,
    Weight,
    PercentOfTrainingMax,
}

impl LoadKind {
    const fn of(load: Option<Load>) -> Self {
        match load {
            None => Self::Bodyweight,
            Some(Load::Weight(_)) => Self::Weight,
            Some(Load::PercentOfTrainingMax(_)) => Self::PercentOfTrainingMax,
        }
    }
}

struct Validator<'a> {
    errors: ValidationErrors,
    /// First occurrence of each exercise id in the program.
    exercises: BTreeMap<&'a ExerciseId, (JsonPath, &'a Exercise)>,
}

pub(super) fn validate(program: &Program) -> Result<(), ValidationErrors> {
    let mut validator = Validator {
        errors: ValidationErrors::default(),
        exercises: BTreeMap::new(),
    };
    validator.program(program);
    if validator.errors.is_empty() {
        Ok(())
    } else {
        Err(validator.errors)
    }
}

impl<'a> Validator<'a> {
    fn push(&mut self, path: JsonPath, kind: Kind) {
        self.errors.push(ValidationError::new(path, kind));
    }

    fn program(&mut self, program: &'a Program) {
        let root = JsonPath::root();
        if program.schema_version != CURRENT_SCHEMA_VERSION {
            self.push(
                root.key("schema_version"),
                Kind::UnsupportedSchemaVersion {
                    found: u64::from(program.schema_version),
                    supported: CURRENT_SCHEMA_VERSION,
                },
            );
        }
        self.name(root.key("name"), &program.name);
        if let Some(description) = &program.description {
            self.text(root.key("description"), description, MAX_TEXT_CHARS);
        }

        let days_path = root.key("days");
        self.list_len(&days_path, program.days.len(), "day", "days", MAX_DAYS);
        // Items past a list's limit are not checked: the limit error is enough, and an upload
        // cannot make validation work (or its output) grow further. Lookups still use every
        // item, so an over-long list does not also cause "unknown day" errors.
        let in_rotation: BTreeSet<_> = program.rotation.iter().collect();
        let all_day_ids: BTreeSet<_> = program.days.iter().map(|day| &day.id).collect();
        let mut day_ids = BTreeMap::new();
        for (index, day) in program.days.iter().enumerate().take(MAX_DAYS) {
            let path = days_path.index(index);
            let id_path = path.key("id");
            if let Some(first) = day_ids.insert(&day.id, id_path.clone()) {
                self.push(
                    id_path.clone(),
                    Kind::DuplicateDayId {
                        id: day.id.to_string(),
                        first,
                    },
                );
            } else if !in_rotation.contains(&day.id) {
                self.push(
                    id_path,
                    Kind::DayNotInRotation {
                        id: day.id.to_string(),
                    },
                );
            }
            self.day(&path, day);
        }

        let rotation_path = root.key("rotation");
        self.list_len(
            &rotation_path,
            program.rotation.len(),
            "day",
            "days",
            MAX_ROTATION,
        );
        for (index, id) in program.rotation.iter().enumerate().take(MAX_ROTATION) {
            if !all_day_ids.contains(id) {
                self.push(
                    rotation_path.index(index),
                    Kind::UnknownDay { id: id.to_string() },
                );
            }
        }
    }

    fn day(&mut self, path: &JsonPath, day: &'a Day) {
        self.name(path.key("name"), &day.name);
        let exercises_path = path.key("exercises");
        self.list_len(
            &exercises_path,
            day.exercises.len(),
            "exercise",
            "exercises",
            MAX_EXERCISES_PER_DAY,
        );
        let exercises = day
            .exercises
            .get(..MAX_EXERCISES_PER_DAY)
            .unwrap_or(&day.exercises);
        let mut in_day = BTreeMap::new();
        for (index, exercise) in exercises.iter().enumerate() {
            let path = exercises_path.index(index);
            if let Some(first) = in_day.insert(&exercise.id, path.clone()) {
                self.push(
                    path.key("id"),
                    Kind::DuplicateExercise {
                        id: exercise.id.to_string(),
                        first,
                    },
                );
            } else {
                self.same_everywhere(&path, exercise);
            }
            self.exercise(&path, exercise);
        }
        self.supersets(&exercises_path, exercises);
    }

    /// An exercise id means the same exercise on every day: same name, rule and kind of load,
    /// since progression history is kept per exercise id.
    fn same_everywhere(&mut self, path: &JsonPath, exercise: &'a Exercise) {
        let Some((first_path, first)) = self.exercises.get(&exercise.id) else {
            self.exercises
                .insert(&exercise.id, (path.clone(), exercise));
            return;
        };
        // Point at the key when this occurrence has it, at the exercise when it is left out.
        let differences = [
            (first.name != exercise.name, Some("name"), "name"),
            (
                first.progression != exercise.progression,
                (!exercise.progression.is_none()).then_some("progression"),
                "progression",
            ),
            (
                LoadKind::of(first.load) != LoadKind::of(exercise.load),
                exercise.load.is_some().then_some("load"),
                "kind of load",
            ),
        ];
        let first_path = first_path.clone();
        for (differs, key, field) in differences {
            if differs {
                self.push(
                    key.map_or_else(|| path.clone(), |key| path.key(key)),
                    Kind::InconsistentExercise {
                        id: exercise.id.to_string(),
                        field,
                        first: first_path.clone(),
                    },
                );
            }
        }
    }

    fn exercise(&mut self, path: &JsonPath, exercise: &Exercise) {
        self.name(path.key("name"), &exercise.name);
        self.work(&path.key("work"), exercise.work);
        self.seconds(path.key("rest"), exercise.rest, 0);
        if let Some(notes) = &exercise.notes {
            self.text(path.key("notes"), notes, MAX_TEXT_CHARS);
        }
        if let Some(load) = exercise.load {
            let load_path = path.key("load");
            match load {
                Load::Weight(weight) => self.positive_weight(unit_key(&load_path, weight), weight),
                Load::PercentOfTrainingMax(percent) => {
                    let percent_path = load_path.key("percent_of_training_max");
                    if exercise.work.is_timed() {
                        self.push(percent_path.clone(), Kind::TrainingMaxOnTimedWork);
                    }
                    self.percent(
                        percent_path,
                        percent,
                        MAX_PERCENT_OF_TRAINING_MAX,
                        "above 0% and at most 150%",
                    );
                }
            }
        }
        self.warmup(&path.key("warmup"), exercise);
        self.progression(&path.key("progression"), exercise);
    }

    fn work(&mut self, path: &JsonPath, work: Work) {
        match work {
            Work::Reps { sets, reps } => {
                let path = path.key("reps");
                self.count(path.key("sets"), sets, 1, MAX_SETS);
                let reps_path = path.key("reps");
                match reps {
                    RepTarget::Fixed(reps) => self.count(reps_path, reps.get(), 1, MAX_REPS),
                    RepTarget::Range(range) => {
                        self.count(reps_path.key("min"), range.min.get(), 1, MAX_REPS);
                        self.count(reps_path.key("max"), range.max.get(), 1, MAX_REPS);
                        if range.min > range.max {
                            self.push(
                                reps_path,
                                Kind::RepRangeInverted {
                                    min: range.min,
                                    max: range.max,
                                },
                            );
                        }
                    }
                }
            }
            Work::Hold { sets, seconds } => {
                let path = path.key("hold");
                self.count(path.key("sets"), sets, 1, MAX_SETS);
                self.seconds(path.key("seconds"), seconds, 1);
            }
            Work::Intervals { work, rest, rounds } => {
                let path = path.key("intervals");
                self.seconds(path.key("work"), work, 1);
                self.seconds(path.key("rest"), rest, 0);
                self.count(path.key("rounds"), rounds, 1, MAX_ROUNDS);
            }
        }
    }

    fn warmup(&mut self, path: &JsonPath, exercise: &Exercise) {
        if exercise.warmup.is_empty() {
            return;
        }
        if exercise.work.is_timed() {
            self.push(path.clone(), Kind::WarmupOnTimedWork);
            return;
        }
        self.list_len(
            path,
            exercise.warmup.len(),
            "warm-up set",
            "warm-up lines",
            MAX_WARMUP_LINES,
        );
        for (index, line) in exercise.warmup.iter().enumerate().take(MAX_WARMUP_LINES) {
            self.warmup_line(&path.index(index), line, exercise.load);
        }
    }

    fn warmup_line(&mut self, path: &JsonPath, line: &WarmupSet, working: Option<Load>) {
        self.count(path.key("sets"), line.sets, 1, MAX_WARMUP_SETS);
        self.count(path.key("reps"), line.reps.get(), 1, MAX_REPS);
        let load_path = path.key("load");
        match line.load {
            WarmupLoad::Weight(weight) => {
                let weight_path = unit_key(&load_path, weight);
                self.positive_weight(weight_path.clone(), weight);
                if let Some(working) = working.and_then(Load::weight)
                    && weight.weight() >= working.weight()
                {
                    self.push(
                        weight_path,
                        Kind::WarmupNotLighter {
                            warmup: weight,
                            working,
                        },
                    );
                }
            }
            WarmupLoad::PercentOfWorkingWeight(percent) => {
                let percent_path = load_path.key("percent_of_working_weight");
                if working.is_none() {
                    self.push(percent_path, Kind::WarmupNeedsWorkingLoad);
                } else if percent == Percent::ZERO
                    || percent.basis_points() >= WARMUP_PERCENT_BELOW * BASIS_POINTS
                {
                    self.push(
                        percent_path,
                        Kind::PercentOutOfRange {
                            value: percent,
                            range: "above 0% and below 100%",
                        },
                    );
                }
            }
        }
    }

    fn progression(&mut self, path: &JsonPath, exercise: &Exercise) {
        let rule = exercise.progression;
        let Some(increment) = rule.increment() else {
            return;
        };
        let name = rule.name();
        if exercise.work.is_timed() {
            self.push(path.clone(), Kind::ProgressionOnTimedWork { rule: name });
            return;
        }
        let rule_path = path.key(name);
        let increment_path = rule_path.key("increment");
        self.positive_weight(unit_key(&increment_path, increment), increment);
        let max = match increment.unit() {
            Unit::Kg => MAX_INCREMENT_KG,
            Unit::Lb => MAX_INCREMENT_LB,
        };
        if increment.weight().value_in(increment.unit()) > f64::from(max) {
            self.push(
                unit_key(&increment_path, increment),
                Kind::IncrementTooLarge {
                    max,
                    unit: increment.unit(),
                },
            );
        }
        match rule {
            ProgressionRule::AddWhenTopOfRange { .. }
            | ProgressionRule::DoubleProgression { .. } => {
                match exercise.load.and_then(Load::weight) {
                    Some(load) if load.unit() != increment.unit() => self.push(
                        increment_path,
                        Kind::IncrementUnitMismatch {
                            increment: increment.unit(),
                            load: load.unit(),
                        },
                    ),
                    Some(_) => {}
                    None => self.push(
                        path.clone(),
                        Kind::ProgressionNeedsWeightLoad { rule: name },
                    ),
                }
                let is_range = matches!(
                    exercise.work,
                    Work::Reps {
                        reps: RepTarget::Range(_),
                        ..
                    }
                );
                if matches!(rule, ProgressionRule::DoubleProgression { .. }) && !is_range {
                    self.push(path.clone(), Kind::ProgressionNeedsRepRange { rule: name });
                }
            }
            ProgressionRule::TrainingMax { .. } => {
                if !exercise.load.is_some_and(Load::is_percent_of_training_max) {
                    self.push(path.clone(), Kind::ProgressionNeedsTrainingMaxLoad);
                }
            }
            ProgressionRule::None => {}
        }
        if let Some(deload) = rule.deload() {
            let deload_path = rule_path.key("deload_after_failures");
            self.count(
                deload_path.key("failures"),
                deload.failures,
                1,
                MAX_DELOAD_FAILURES,
            );
            self.percent(
                deload_path.key("percent"),
                deload.percent,
                MAX_DELOAD_PERCENT,
                "above 0% and at most 50%",
            );
        }
    }

    /// Members of a superset are next to each other, at least two, with the same number of sets
    /// so they can alternate (A1, A2, A1, A2…), and not intervals.
    fn supersets(&mut self, path: &JsonPath, exercises: &[Exercise]) {
        struct Group {
            first: usize,
            sets: u16,
            members: usize,
        }
        let mut groups: BTreeMap<&SupersetId, Group> = BTreeMap::new();
        let mut previous: Option<&SupersetId> = None;
        for (index, exercise) in exercises.iter().enumerate() {
            let label = exercise.superset.as_ref();
            let current = mem::replace(&mut previous, label);
            let Some(label) = label else {
                continue;
            };
            let exercise_path = path.index(index);
            if matches!(exercise.work, Work::Intervals { .. }) {
                self.push(exercise_path.key("superset"), Kind::SupersetWithIntervals);
            }
            let Some(group) = groups.get_mut(label) else {
                groups.insert(
                    label,
                    Group {
                        first: index,
                        sets: exercise.work.sets(),
                        members: 1,
                    },
                );
                continue;
            };
            group.members += 1;
            let (first, expected) = (group.first, group.sets);
            if current != Some(label) {
                self.push(
                    exercise_path.key("superset"),
                    Kind::SupersetNotContiguous {
                        id: label.to_string(),
                    },
                );
            }
            let sets = exercise.work.sets();
            if sets != expected {
                self.push(
                    sets_path(&exercise_path, exercise.work),
                    Kind::SupersetSetsMismatch {
                        id: label.to_string(),
                        sets,
                        expected,
                        first: path.index(first),
                    },
                );
            }
        }
        for (label, group) in groups {
            if group.members < 2 {
                self.push(
                    path.index(group.first).key("superset"),
                    Kind::SupersetTooSmall {
                        id: label.to_string(),
                    },
                );
            }
        }
    }

    fn name(&mut self, path: JsonPath, name: &str) {
        self.text(path, name, MAX_NAME_CHARS);
    }

    fn text(&mut self, path: JsonPath, text: &str, max: usize) {
        let len = text.chars().count();
        if is_blank(text) {
            self.push(path, Kind::Blank);
        } else if len > max {
            self.push(path, Kind::TooLong { max, len });
        }
    }

    fn list_len(
        &mut self,
        path: &JsonPath,
        len: usize,
        item: &'static str,
        items: &'static str,
        max: usize,
    ) {
        if len == 0 {
            self.push(path.clone(), Kind::Empty { item });
        } else if len > max {
            self.push(path.clone(), Kind::TooMany { items, max, len });
        }
    }

    fn count(&mut self, path: JsonPath, value: u16, min: u16, max: u16) {
        if !(min..=max).contains(&value) {
            self.push(
                path,
                Kind::OutOfRange {
                    value: u64::from(value),
                    min: u64::from(min),
                    max: u64::from(max),
                },
            );
        }
    }

    fn seconds(&mut self, path: JsonPath, value: Seconds, min: u32) {
        if !(min..=MAX_SECONDS).contains(&value.get()) {
            self.push(
                path,
                Kind::OutOfRange {
                    value: u64::from(value.get()),
                    min: u64::from(min),
                    max: u64::from(MAX_SECONDS),
                },
            );
        }
    }

    fn percent(&mut self, path: JsonPath, value: Percent, max_percent: u32, range: &'static str) {
        if value == Percent::ZERO || value.basis_points() > max_percent * BASIS_POINTS {
            self.push(path, Kind::PercentOutOfRange { value, range });
        }
    }

    fn positive_weight(&mut self, path: JsonPath, weight: UnitWeight) {
        if weight.weight().is_zero() {
            self.push(path, Kind::ZeroWeight);
        }
    }
}

/// Whether `text` has nothing but whitespace: Unicode `White_Space` plus U+FEFF (the byte order
/// mark), exactly what the schema's `[^\s\u0085]` pattern treats as blank.
fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '\u{feff}')
}

/// `path.kg` or `path.lb`, where the number of a [`UnitWeight`] is written.
fn unit_key(path: &JsonPath, weight: UnitWeight) -> JsonPath {
    path.key(match weight.unit() {
        Unit::Kg => "kg",
        Unit::Lb => "lb",
    })
}

/// Where the set count of `work` is written.
fn sets_path(exercise: &JsonPath, work: Work) -> JsonPath {
    let work_path = exercise.key("work");
    match work {
        Work::Reps { .. } => work_path.key("reps").key("sets"),
        Work::Hold { .. } => work_path.key("hold").key("sets"),
        Work::Intervals { .. } => work_path.key("intervals"),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::*;

    fn exercise(id: &str) -> Value {
        json!({ "id": id, "name": id, "work": { "reps": { "sets": 3, "reps": 5 } }, "rest": 60 })
    }

    fn base() -> Value {
        json!({
            "schema_version": 1,
            "name": "Test",
            "days": [{ "id": "a", "name": "A", "exercises": [exercise("squat")] }],
            "rotation": ["a"],
        })
    }

    /// The validation messages for `base()` after `change`.
    fn errors(change: impl FnOnce(&mut Value)) -> Vec<String> {
        let mut document = base();
        change(&mut document);
        let program: Program = serde_json::from_value(document).unwrap();
        match program.validate() {
            Ok(()) => Vec::new(),
            Err(errors) => errors.as_slice().iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn base_is_valid() {
        assert_eq!(errors(|_| {}), Vec::<String>::new());
    }

    #[test]
    fn list_limits() {
        let too_many = errors(|doc| {
            let days: Vec<_> = (0..=MAX_DAYS)
                .map(|i| json!({ "id": format!("d{i}"), "name": "D", "exercises": [exercise("squat")] }))
                .collect();
            let rotation: Vec<_> = (0..=MAX_DAYS).map(|i| format!("d{i}")).collect();
            doc["days"] = days.into();
            doc["rotation"] = rotation.into();
        });
        assert_eq!(too_many, ["days: must contain at most 14 days (got 15)"]);

        let rotation = errors(|doc| doc["rotation"] = vec!["a"; MAX_ROTATION + 1].into());
        assert_eq!(
            rotation,
            ["rotation: must contain at most 28 days (got 29)"]
        );
        assert!(errors(|doc| doc["rotation"] = vec!["a"; MAX_ROTATION].into()).is_empty());

        let exercises = errors(|doc| {
            doc["days"][0]["exercises"] = (0..=MAX_EXERCISES_PER_DAY)
                .map(|i| exercise(&format!("e{i}")))
                .collect::<Vec<_>>()
                .into();
        });
        assert_eq!(
            exercises,
            ["days[0].exercises: must contain at most 30 exercises (got 31)"]
        );

        let warmup = errors(|doc| {
            doc["days"][0]["exercises"][0]["warmup"] =
                vec![json!({ "reps": 5, "load": { "kg": 20 } }); MAX_WARMUP_LINES + 1].into();
        });
        assert_eq!(
            warmup,
            ["days[0].exercises[0].warmup: must contain at most 10 warm-up lines (got 11)"]
        );
    }

    #[test]
    fn text_limits() {
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let notes = errors(|doc| doc["days"][0]["exercises"][0]["notes"] = long.clone().into());
        assert_eq!(
            notes,
            ["days[0].exercises[0].notes: must be at most 2000 characters (got 2001)"]
        );
        let blank = errors(|doc| {
            doc["days"][0]["name"] = " \t".into();
            doc["days"][0]["exercises"][0]["name"] = "".into();
            doc["days"][0]["exercises"][0]["notes"] = "\n".into();
        });
        assert_eq!(
            blank,
            [
                "days[0].name: must not be blank",
                "days[0].exercises[0].name: must not be blank",
                "days[0].exercises[0].notes: must not be blank",
            ]
        );
        // Characters, not bytes: 100 accented letters are fine.
        assert!(errors(|doc| doc["name"] = "é".repeat(MAX_NAME_CHARS).into()).is_empty());
        assert!(!errors(|doc| doc["name"] = "é".repeat(MAX_NAME_CHARS + 1).into()).is_empty());
    }

    #[test]
    fn boundaries_are_inclusive() {
        let at_limits = errors(|doc| {
            doc["days"][0]["exercises"] = json!([
                {
                    "id": "squat", "name": "Squat",
                    "work": { "reps": { "sets": MAX_SETS, "reps": { "min": 1, "max": MAX_REPS } } },
                    "load": { "percent_of_training_max": 150 },
                    "rest": MAX_SECONDS,
                    "warmup": [{ "sets": MAX_WARMUP_SETS, "reps": MAX_REPS, "load": { "percent_of_working_weight": 99.99 } }],
                    "progression": { "training_max": {
                        "increment": { "kg": 0.25 },
                        "deload_after_failures": { "failures": MAX_DELOAD_FAILURES, "percent": 50 }
                    } }
                },
                {
                    "id": "plank", "name": "Plank",
                    "work": { "hold": { "sets": 1, "seconds": MAX_SECONDS } },
                    "rest": 0
                },
                {
                    "id": "row", "name": "Row",
                    "work": { "intervals": { "work": MAX_SECONDS, "rest": MAX_SECONDS, "rounds": MAX_ROUNDS } },
                    "rest": 0
                },
                {
                    "id": "curl", "name": "Curl",
                    "work": { "reps": { "sets": 1, "reps": { "min": 10, "max": 10 } } },
                    "load": { "kg": 10 },
                    "rest": 0,
                    "warmup": [{ "reps": 1, "load": { "kg": 9.99 } }],
                    "progression": { "double_progression": { "increment": { "kg": 1 } } }
                }
            ]);
        });
        assert_eq!(at_limits, Vec::<String>::new());
    }

    #[test]
    fn percent_edges() {
        let zero = errors(|doc| {
            let squat = &mut doc["days"][0]["exercises"][0];
            squat["load"] = json!({ "percent_of_training_max": 0 });
            squat["warmup"] = json!([{ "reps": 5, "load": { "percent_of_working_weight": 0 } }]);
            squat["progression"] = json!({ "training_max": {
                "increment": { "kg": 2.5 },
                "deload_after_failures": { "failures": 1, "percent": 0 }
            } });
        });
        assert_eq!(
            zero,
            [
                "days[0].exercises[0].load.percent_of_training_max: must be above 0% and at most 150% (got 0%)",
                "days[0].exercises[0].warmup[0].load.percent_of_working_weight: must be above 0% and below 100% (got 0%)",
                "days[0].exercises[0].progression.training_max.deload_after_failures.percent: must be above 0% and at most 50% (got 0%)",
            ]
        );
    }

    #[test]
    fn warmup_lighter_compares_across_units() {
        // 45 lb (20.41 kg) is lighter than 25 kg, 60 lb (27.2 kg) is not.
        let set = |lb: u32| {
            errors(move |doc| {
                let squat = &mut doc["days"][0]["exercises"][0];
                squat["load"] = json!({ "kg": 25 });
                squat["warmup"] = json!([{ "reps": 5, "load": { "lb": lb } }]);
            })
        };
        assert!(set(45).is_empty());
        assert_eq!(
            set(60),
            [
                "days[0].exercises[0].warmup[0].load.lb: warm-up load 60 lb must be lighter than the working load 25 kg"
            ]
        );
        // A fixed warm-up weight on a bodyweight or %TM exercise is not compared.
        assert!(
            errors(|doc| {
                doc["days"][0]["exercises"][0]["warmup"] =
                    json!([{ "reps": 5, "load": { "kg": 20 } }]);
            })
            .is_empty()
        );
    }

    #[test]
    fn exercise_consistency_ignores_the_rest_of_the_prescription() {
        // Heavy on day A, lighter on day B: same id, name, rule and kind of load is fine.
        let result = errors(|doc| {
            let mut light = exercise("squat");
            light["work"] = json!({ "reps": { "sets": 2, "reps": 8 } });
            light["rest"] = 120.into();
            doc["days"] = json!([
                { "id": "a", "name": "A", "exercises": [exercise("squat")] },
                { "id": "b", "name": "B", "exercises": [light] },
            ]);
            doc["rotation"] = json!(["a", "b"]);
        });
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn superset_members_can_be_holds() {
        let result = errors(|doc| {
            doc["days"][0]["exercises"] = json!([
                { "id": "row", "name": "Row", "work": { "reps": { "sets": 3, "reps": 8 } }, "rest": 0, "superset": "a" },
                { "id": "plank", "name": "Plank", "work": { "hold": { "sets": 3, "seconds": 30 } }, "rest": 90, "superset": "a" },
                { "id": "curl", "name": "Curl", "work": { "reps": { "sets": 2, "reps": 8 } }, "rest": 0, "superset": "b" },
                { "id": "dip", "name": "Dip", "work": { "reps": { "sets": 2, "reps": 8 } }, "rest": 60, "superset": "b" },
            ]);
        });
        assert!(result.is_empty(), "{result:?}");
    }

    #[test]
    fn hold_sets_mismatch_points_at_the_hold() {
        let result = errors(|doc| {
            doc["days"][0]["exercises"] = json!([
                { "id": "row", "name": "Row", "work": { "reps": { "sets": 3, "reps": 8 } }, "rest": 0, "superset": "a" },
                { "id": "plank", "name": "Plank", "work": { "hold": { "sets": 2, "seconds": 30 } }, "rest": 90, "superset": "a" },
                { "id": "bike", "name": "Bike", "work": { "intervals": { "work": 30, "rest": 30, "rounds": 3 } }, "rest": 90, "superset": "a" },
            ]);
        });
        assert_eq!(
            result,
            [
                "days[0].exercises[1].work.hold.sets: superset `a` needs the same number of sets for every exercise (2 here, 3 at days[0].exercises[0])",
                "days[0].exercises[2].superset: intervals cannot be part of a superset",
                "days[0].exercises[2].work.intervals: superset `a` needs the same number of sets for every exercise (1 here, 3 at days[0].exercises[0])",
            ]
        );
    }

    #[test]
    fn timed_work_cannot_have_a_training_max_rule_either() {
        let result = errors(|doc| {
            doc["days"][0]["exercises"][0] = json!({
                "id": "bike", "name": "Bike",
                "work": { "intervals": { "work": 30, "rest": 30, "rounds": 3 } },
                "rest": 60,
                "progression": { "training_max": { "increment": { "kg": 1 } } }
            });
        });
        assert_eq!(
            result,
            [
                "days[0].exercises[0].progression: timed work cannot use `training_max`; use \"none\""
            ]
        );
    }

    #[test]
    fn training_max_loads_need_sets_of_reps() {
        let result = errors(|doc| {
            doc["days"][0]["exercises"][0] = json!({
                "id": "plank", "name": "Plank",
                "work": { "hold": { "sets": 3, "seconds": 30 } },
                "load": { "percent_of_training_max": 50 },
                "rest": 60
            });
        });
        assert_eq!(
            result,
            [
                "days[0].exercises[0].load.percent_of_training_max: timed work cannot use a percent_of_training_max load"
            ]
        );
    }

    #[test]
    fn increments_are_capped_per_unit() {
        let with = |increment: Value, load: Value| {
            errors(move |doc| {
                let squat = &mut doc["days"][0]["exercises"][0];
                squat["load"] = load;
                squat["progression"] =
                    json!({ "add_when_top_of_range": { "increment": increment } });
            })
        };
        assert!(with(json!({ "kg": 20 }), json!({ "kg": 100 })).is_empty());
        assert_eq!(
            with(json!({ "kg": 20.25 }), json!({ "kg": 100 })),
            [
                "days[0].exercises[0].progression.add_when_top_of_range.increment.kg: must be at most 20 kg"
            ]
        );
        assert!(with(json!({ "lb": 45 }), json!({ "lb": 225 })).is_empty());
        assert_eq!(
            with(json!({ "lb": 45.5 }), json!({ "lb": 225 })),
            [
                "days[0].exercises[0].progression.add_when_top_of_range.increment.lb: must be at most 45 lb"
            ]
        );
    }

    #[test]
    fn consistency_errors_point_at_keys_that_exist() {
        let result = errors(|doc| {
            let mut loaded = exercise("squat");
            loaded["load"] = json!({ "kg": 60 });
            loaded["progression"] =
                json!({ "add_when_top_of_range": { "increment": { "kg": 2.5 } } });
            doc["days"] = json!([
                { "id": "a", "name": "A", "exercises": [exercise("squat")] },
                { "id": "b", "name": "B", "exercises": [loaded] },
            ]);
            doc["rotation"] = json!(["a", "b"]);
        });
        assert_eq!(
            result,
            [
                "days[1].exercises[0].progression: exercise `squat` must have the same progression everywhere (see days[0].exercises[0])",
                "days[1].exercises[0].load: exercise `squat` must have the same kind of load everywhere (see days[0].exercises[0])",
            ]
        );
    }

    #[test]
    fn blank_means_whitespace_or_a_byte_order_mark() {
        assert!(is_blank(""));
        assert!(is_blank(" \t\n\u{a0}\u{85}\u{2003}\u{3000}\u{feff}"));
        assert!(!is_blank(" x "));
        assert!(!is_blank("\u{200b}"));
    }

    #[test]
    fn warmup_lines_past_the_limit_are_not_checked() {
        let result = errors(|doc| {
            doc["days"][0]["exercises"][0]["warmup"] =
                vec![json!({ "reps": 0, "load": { "kg": 0 } }); MAX_WARMUP_LINES + 5].into();
        });
        assert_eq!(result.len(), 1 + 2 * MAX_WARMUP_LINES, "{result:?}");
        assert!(
            !result
                .iter()
                .any(|e| e.contains(&format!("warmup[{MAX_WARMUP_LINES}]")))
        );
    }

    proptest! {
        #[test]
        fn rep_range_is_rejected_exactly_when_inverted(min in 1_u16..=MAX_REPS, max in 1_u16..=MAX_REPS) {
            let result = errors(|doc| {
                doc["days"][0]["exercises"][0]["work"] =
                    json!({ "reps": { "sets": 3, "reps": { "min": min, "max": max } } });
            });
            if min > max {
                prop_assert_eq!(
                    result,
                    vec![format!("days[0].exercises[0].work.reps.reps: min {min} is greater than max {max}")]
                );
            } else {
                prop_assert!(result.is_empty());
            }
        }

        #[test]
        fn counts_are_checked_against_their_limits(sets in 0_u16..=40, rest in 0_u32..=7_200) {
            let result = errors(|doc| {
                doc["days"][0]["exercises"][0]["work"]["reps"]["sets"] = sets.into();
                doc["days"][0]["exercises"][0]["rest"] = rest.into();
            });
            let expected = usize::from(!(1..=MAX_SETS).contains(&sets))
                + usize::from(rest > MAX_SECONDS);
            prop_assert_eq!(result.len(), expected);
        }
    }
}

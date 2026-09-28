//! Programs shipped with the app, embedded at compile time from `programs/*.json`.

use super::error::ProgramError;
use super::ids::{BuiltinProgramId, InvalidSlug};
use super::model::Program;

/// Id and JSON source of every built-in program, in display order.
const SOURCES: &[(&str, &str)] = &[(
    "full-body-3day",
    include_str!("../../../../programs/full-body-3day.json"),
)];

/// A program shipped with the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinProgram {
    id: BuiltinProgramId,
    json: &'static str,
    program: Program,
}

impl BuiltinProgram {
    /// The stable id, e.g. `full-body-3day`.
    #[must_use]
    pub const fn id(&self) -> &BuiltinProgramId {
        &self.id
    }

    /// The JSON document as written in `programs/`, e.g. to store as a user's first version.
    #[must_use]
    pub const fn json(&self) -> &'static str {
        self.json
    }

    /// The parsed and validated program.
    #[must_use]
    pub const fn program(&self) -> &Program {
        &self.program
    }

    /// Takes the program out.
    #[must_use]
    pub fn into_program(self) -> Program {
        self.program
    }
}

/// A built-in program failed to load. The test suite guarantees this never happens in a build.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuiltinProgramError {
    /// The id is not a valid slug.
    #[error(transparent)]
    Id(#[from] InvalidSlug),
    /// The JSON is not a valid program.
    #[error("built-in program `{id}` is invalid: {source}")]
    Program {
        /// The program's id.
        id: BuiltinProgramId,
        /// What is wrong with it.
        source: ProgramError,
    },
}

fn load(id: &str, json: &'static str) -> Result<BuiltinProgram, BuiltinProgramError> {
    let id = BuiltinProgramId::new(id)?;
    match Program::from_json(json) {
        Ok(program) => Ok(BuiltinProgram { id, json, program }),
        Err(source) => Err(BuiltinProgramError::Program { id, source }),
    }
}

/// Every built-in program, parsed and validated, in display order.
///
/// # Errors
/// The first built-in program that fails to load; a test makes sure none does.
pub fn builtin_programs() -> Result<Vec<BuiltinProgram>, BuiltinProgramError> {
    SOURCES.iter().map(|(id, json)| load(id, json)).collect()
}

/// The built-in program with this id, or `None` if there is none.
///
/// # Errors
/// If that program fails to load; a test makes sure none does.
pub fn builtin_program(
    id: &BuiltinProgramId,
) -> Result<Option<BuiltinProgram>, BuiltinProgramError> {
    SOURCES
        .iter()
        .find(|(source_id, _)| *source_id == id.as_str())
        .map(|(source_id, json)| load(source_id, json))
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::program::{Load, ProgressionRule, Work};
    use crate::{DayId, ExerciseId, Seconds};

    #[test]
    fn every_builtin_program_loads() {
        let programs = builtin_programs().unwrap();
        assert_eq!(programs.len(), SOURCES.len());
        let ids: BTreeSet<_> = programs.iter().map(|p| p.id().clone()).collect();
        assert_eq!(ids.len(), programs.len(), "duplicate built-in ids");
        for builtin in &programs {
            assert_eq!(
                builtin_program(builtin.id()).unwrap().as_ref(),
                Some(builtin)
            );
            assert_eq!(
                Program::from_json(builtin.json()).unwrap(),
                *builtin.program()
            );
            assert_eq!(
                builtin.program().schema.as_deref(),
                Some(crate::program::PROGRAM_SCHEMA_URL)
            );
        }
        let unknown = BuiltinProgramId::new("nope").unwrap();
        assert_eq!(builtin_program(&unknown).unwrap(), None);
    }

    #[test]
    fn load_reports_bad_ids_and_bad_programs() {
        assert!(matches!(
            load("Not A Slug", "{}"),
            Err(BuiltinProgramError::Id(_))
        ));
        let err = load("broken", "{").unwrap_err();
        assert!(
            err.to_string()
                .starts_with("built-in program `broken` is invalid: EOF while parsing"),
            "{err}"
        );
    }

    /// #16: A/B/C days in rotation, warm-ups, rests, progression rules, a superset and a plank.
    #[test]
    fn full_body_3day_has_what_the_ticket_asks_for() {
        let id = BuiltinProgramId::new("full-body-3day").unwrap();
        let program = builtin_program(&id).unwrap().unwrap().into_program();
        let day_ids = |ids: &[&str]| {
            ids.iter()
                .map(|id| DayId::new(*id).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(program.rotation, day_ids(&["a", "b", "c"]));
        assert_eq!(
            program
                .days
                .iter()
                .map(|d| d.id.clone())
                .collect::<Vec<_>>(),
            day_ids(&["a", "b", "c"])
        );
        let exercises: Vec<_> = program.exercises().collect();
        assert!(exercises.iter().any(|e| !e.warmup.is_empty()));
        assert!(
            exercises
                .iter()
                .all(|e| !e.rest.is_zero() || e.superset.is_some())
        );
        assert!(exercises.iter().any(|e| !e.progression.is_none()));
        assert!(exercises.iter().any(|e| e.progression.deload().is_some()));
        assert!(exercises.iter().any(|e| e.superset.is_some()));
        let plank = program
            .exercise(&ExerciseId::new("plank").unwrap())
            .unwrap();
        assert!(matches!(plank.work, Work::Hold { seconds, .. } if seconds >= Seconds::new(20)));
        assert!(plank.progression.is_none());
        let squat = program
            .exercise(&ExerciseId::new("back-squat").unwrap())
            .unwrap();
        assert!(matches!(squat.load, Some(Load::Weight(_))));
        assert!(matches!(
            squat.progression,
            ProgressionRule::AddWhenTopOfRange { .. }
        ));
        // Squat on all three days: the same exercise id keeps one progression history.
        assert_eq!(program.exercises().filter(|e| e.id == squat.id).count(), 3);
        assert!(program.training_max_exercises().is_empty());
    }
}

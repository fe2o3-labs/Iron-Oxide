//! Program documents end to end: fixtures, error snapshots, round trips and the committed JSON
//! Schema.
//!
//! Each `tests/fixtures/programs/invalid/**/NAME.json` has its expected error output next to it in
//! `NAME.errors`. After an intended change to the messages, regenerate them with
//! `UPDATE_SNAPSHOTS=1 cargo test -p iron-oxide-domain --test program_documents` and review the
//! diff.

// Test helpers outside `#[test]` functions are not covered by clippy.toml's test allowances.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use iron_oxide_domain::program::{
    PROGRAM_SCHEMA_JSON, Program, ProgramError, ValidationErrorKind, builtin_programs,
};
use serde_json::Value;

fn fixtures(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/programs")
        .join(dir);
    let mut paths: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures in {}", dir.display());
    paths
}

fn schema_validator() -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(PROGRAM_SCHEMA_JSON).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn schema_errors(validator: &jsonschema::Validator, json: &str) -> Vec<String> {
    let instance: Value = serde_json::from_str(json).unwrap();
    validator
        .iter_errors(&instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect()
}

#[test]
fn valid_fixtures_parse_validate_round_trip_and_match_the_schema() {
    let validator = schema_validator();
    for path in fixtures("valid") {
        let json = fs::read_to_string(&path).unwrap();
        let program = Program::from_json(&json)
            .unwrap_or_else(|error| panic!("{}:\n{error}", path.display()));
        let again = Program::from_json(&program.to_json_pretty().unwrap()).unwrap();
        assert_eq!(again, program, "{}", path.display());
        assert_eq!(
            schema_errors(&validator, &json),
            Vec::<String>::new(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn builtin_programs_round_trip_and_match_the_schema() {
    let validator = schema_validator();
    for builtin in builtin_programs().unwrap() {
        assert_eq!(
            schema_errors(&validator, builtin.json()),
            Vec::<String>::new(),
            "{}",
            builtin.id()
        );
        let written = builtin.program().to_json_pretty().unwrap();
        assert_eq!(schema_errors(&validator, &written), Vec::<String>::new());
        assert_eq!(Program::from_json(&written).unwrap(), *builtin.program());
    }
}

fn check_snapshot(path: &Path, actual: &str) {
    let snapshot = path.with_extension("errors");
    let actual = format!("{actual}\n");
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        fs::write(&snapshot, &actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&snapshot).unwrap_or_default();
    assert!(
        expected == actual,
        "{} does not match.\n--- expected\n{expected}--- actual\n{actual}\
         (run with UPDATE_SNAPSHOTS=1 to accept)",
        snapshot.display()
    );
}

/// Documents that serde rejects. The schema rejects them too, so an editor flags them as well.
#[test]
fn parse_errors_have_a_path_and_a_position() {
    let validator = schema_validator();
    for path in fixtures("invalid/parse") {
        let json = fs::read_to_string(&path).unwrap();
        let error = Program::from_json(&json).unwrap_err();
        let ProgramError::Parse(parse) = &error else {
            panic!("{}: expected a parse error, got {error}", path.display());
        };
        assert!(parse.line > 0 && parse.column > 0, "{}", path.display());
        check_snapshot(&path, &error.to_string());
        if serde_json::from_str::<Value>(&json).is_ok() {
            assert!(
                !schema_errors(&validator, &json).is_empty(),
                "{}: the schema accepts what serde rejects",
                path.display()
            );
        }
    }
}

/// Documents that parse but break rules: every broken rule is reported.
#[test]
fn validation_errors_list_every_problem_with_its_path() {
    for path in fixtures("invalid/rules") {
        let json = fs::read_to_string(&path).unwrap();
        let error = Program::from_json(&json).unwrap_err();
        let ProgramError::Invalid(errors) = &error else {
            panic!(
                "{}: expected validation errors, got {error}",
                path.display()
            );
        };
        assert!(!errors.as_slice().is_empty());
        check_snapshot(&path, &error.to_string());
    }
}

#[test]
fn the_ticket_example_reads_as_expected() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/programs/invalid/rules/rep-range-inverted.json");
    let error = Program::from_json(&fs::read_to_string(path).unwrap()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "days[1].exercises[2].work.reps.reps: min 12 is greater than max 8"
    );
}

#[test]
fn unsupported_version_is_reported_alone() {
    let error = Program::from_json(r#"{"schema_version": 7, "whatever": true}"#).unwrap_err();
    let ProgramError::Invalid(errors) = error else {
        panic!("expected validation errors");
    };
    assert_eq!(errors.as_slice().len(), 1);
    assert_eq!(
        errors.as_slice()[0].kind,
        ValidationErrorKind::UnsupportedSchemaVersion {
            found: 7,
            supported: 1
        }
    );
    // A version that is not a number is left to the full parse.
    let error = Program::from_json(r#"{"schema_version": "1"}"#).unwrap_err();
    assert!(matches!(error, ProgramError::Parse(_)), "{error}");
}

/// Building a program in code and breaking it: validate() is usable without JSON.
#[test]
fn validate_works_on_programs_built_in_code() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/programs/valid/everything.json");
    let mut program = Program::from_json(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(program.validate(), Ok(()));
    program.schema_version = 2;
    program.rotation.clear();
    let errors = program.validate().unwrap_err();
    let messages: Vec<_> = errors.as_slice().iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "schema_version: unsupported schema_version 2 (this app reads version 1)",
            "days[0].id: day `upper` is not in the rotation",
            "days[1].id: day `lower` is not in the rotation",
            "rotation: must contain at least one day",
        ]
    );
}

//! Programs (#19): the built-in programs, the user's own programs and their versions, the active
//! program, and uploading a `program.json`.
//!
//! Every function needs a signed-in user and only ever reads or changes that user's programs.
//! Another user's program id gets the same `404` as an id that does not exist. Built-in programs
//! are read-only: a user trains with a copy ([`copy_builtin_program`]), which is theirs.
//!
//! An uploaded document is untrusted. Its request body is capped before anything reads it
//! ([`UPLOAD_BODY_LIMIT`]), the document itself is capped at [`MAX_DOCUMENT_BYTES`] before it is
//! parsed, and a document that does not parse or breaks a rule is refused with `422` and the
//! domain's errors, each with its JSON path ([`ProgramProblems`]).
//!
//! The logic is in `crate::server::api::programs`; conventions in `docs/api.md`.

// The browser build only calls these functions; the Programs screen that does is still to come.
#![cfg_attr(
    not(feature = "server"),
    allow(dead_code, reason = "used by the Programs screen (#35)")
)]

use dioxus::prelude::*;
use iron_oxide_domain::{
    CreationId, ProgramId, ProgramVersionId,
    program::{BuiltinProgramId, Program, ProgramError, limits::MAX_DOCUMENT_BYTES},
    time::Timestamp,
};
use serde::{Deserialize, Serialize};

#[cfg(feature = "server")]
use {
    crate::server::{AppState, api::programs, auth::AuthUser},
    dioxus::server::axum::Extension,
};

/// A built-in program, with its current document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinProgramView {
    /// The stable id to copy it with, e.g. `full-body-3day`.
    pub builtin_id: BuiltinProgramId,
    pub name: String,
    /// The version of the built-in the app ships: 1, then +1 each time its document changes.
    pub version: u32,
    pub document: Program,
}

/// One of the user's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramView {
    pub id: ProgramId,
    pub name: String,
    /// The built-in it was copied from, if any.
    pub source_builtin_id: Option<BuiltinProgramId>,
    /// Hidden from [`list_programs`] by default. Its versions, and the sessions run from them,
    /// are kept.
    pub archived: bool,
    pub created_at: Timestamp,
}

/// One immutable version of a program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionView {
    pub id: ProgramVersionId,
    /// 1 for the first version, then +1 for each new one.
    pub version: u32,
    pub created_at: Timestamp,
}

/// A program with its latest version and that version's document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramDetail {
    pub program: ProgramView,
    pub version: VersionView,
    pub document: Program,
}

/// Where an uploaded document goes. JSON: `{"kind": "new_program", "creation_id": …}` or
/// `{"kind": "new_version", "program_id": …}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum UploadTarget {
    /// A new program, named after the document. Retrying with the same `creation_id` returns the
    /// program the first upload created instead of creating a second one.
    NewProgram { creation_id: CreationId },
    /// A new version of one of the user's programs. The program keeps its name.
    NewVersion { program_id: ProgramId },
}

/// What an upload did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadOutcome {
    pub program: ProgramView,
    /// The version holding the document: the new one, or the one that already had it.
    pub version: VersionView,
    /// `false` when nothing was saved: the document equals the program's latest version, or the
    /// upload repeats one that was already saved.
    pub saved: bool,
}

/// Why an uploaded document was refused: the `details` of the `422`. At most
/// [`MAX_REPORTED_ERRORS`](iron_oxide_domain::program::limits::MAX_REPORTED_ERRORS) problems, the
/// others counted in `omitted` (the same shape as the domain's `ValidationErrors`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramProblems {
    pub errors: Vec<ProgramProblem>,
    pub omitted: usize,
}

/// One problem in an uploaded document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramProblem {
    /// Where, as `days[1].exercises[2].reps`; empty for the document itself.
    pub path: String,
    pub message: String,
    /// The 1-based line of a document that does not parse (JSON syntax or shape), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// The 1-based column, with `line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
}

impl From<ProgramError> for ProgramProblems {
    /// A parse error is one problem with its line and column; broken rules are all listed.
    fn from(error: ProgramError) -> Self {
        match error {
            ProgramError::Parse(parse) => {
                let position = (parse.line > 0).then_some((parse.line, parse.column));
                Self {
                    errors: vec![ProgramProblem {
                        path: parse.path.to_string(),
                        message: parse.message,
                        line: position.map(|(line, _)| line),
                        column: position.map(|(_, column)| column),
                    }],
                    omitted: 0,
                }
            }
            ProgramError::Invalid(errors) => Self {
                omitted: errors.omitted(),
                errors: errors
                    .into_vec()
                    .into_iter()
                    .map(|error| ProgramProblem {
                        path: error.path.to_string(),
                        message: error.kind.to_string(),
                        line: None,
                        column: None,
                    })
                    .collect(),
            },
        }
    }
}

impl ProgramProblems {
    /// The problems carried by a `422` from [`upload_program`]; `None` for any other error.
    #[must_use]
    pub fn from_error(error: &ServerFnError) -> Option<Self> {
        let ServerFnError::ServerError {
            code: 422,
            details: Some(details),
            ..
        } = error
        else {
            return None;
        };
        // The server sets the problems as the details. The Dioxus client receives the whole
        // server error there instead: `{"ServerError": {"details": problems, ..}}`.
        let problems = details
            .get("ServerError")
            .and_then(|inner| inner.get("details"))
            .unwrap_or(details);
        serde_json::from_value(problems.clone()).ok()
    }
}

/// Largest request body [`upload_program`] reads, in bytes, checked before anything parses it; a
/// larger body is refused with `413`.
///
/// The document travels as a JSON string, where each `"`, `\` and line break takes two bytes, so
/// a valid document of [`MAX_DOCUMENT_BYTES`] can take twice as many bytes in the body. The rest
/// is room for the other arguments.
pub const UPLOAD_BODY_LIMIT: usize = 2 * MAX_DOCUMENT_BYTES + 16 * 1024;

// Server functions read their body with axum's default limit (2 MiB) and panic past it, so the
// upload limit must stay below it (see `server::api::programs::limit_upload_body`).
const _: () = assert!(UPLOAD_BODY_LIMIT < 2 * 1024 * 1024);

/// The built-in programs, by name.
#[post("/api/programs/builtins", state: Extension<AppState>, _user: AuthUser)]
pub async fn list_builtin_programs() -> Result<Vec<BuiltinProgramView>, ServerFnError> {
    Ok(programs::list_builtins(&state.db).await?)
}

/// Copies a built-in program into a new program of the user's, which they can change and train
/// with. Retrying with the same `creation_id` returns the same copy. `builtin_id` is a
/// [`BuiltinProgramView::builtin_id`]; any other text is `404`.
#[post("/api/programs/copy-builtin", state: Extension<AppState>, user: AuthUser)]
pub async fn copy_builtin_program(
    builtin_id: String,
    creation_id: CreationId,
) -> Result<ProgramDetail, ServerFnError> {
    Ok(programs::copy_builtin(&state.db, user, &builtin_id, creation_id).await?)
}

/// The user's programs, oldest first; the archived ones too when `include_archived`.
#[post("/api/programs/list", state: Extension<AppState>, user: AuthUser)]
pub async fn list_programs(include_archived: bool) -> Result<Vec<ProgramView>, ServerFnError> {
    Ok(programs::list(&state.db, user.owner(), include_archived).await?)
}

/// One of the user's programs, with its latest version.
#[post("/api/programs/get", state: Extension<AppState>, user: AuthUser)]
pub async fn get_program(program_id: ProgramId) -> Result<ProgramDetail, ServerFnError> {
    Ok(programs::detail(&state.db, user.owner(), program_id).await?)
}

/// The program the user trains with, with its latest version; `None` until they choose one.
#[post("/api/programs/active", state: Extension<AppState>, user: AuthUser)]
pub async fn get_active_program() -> Result<Option<ProgramDetail>, ServerFnError> {
    Ok(programs::active(&state.db, user.owner()).await?)
}

/// Makes one of the user's programs the one they train with. An archived program must be
/// restored first (`409`).
#[post("/api/programs/active/set", state: Extension<AppState>, user: AuthUser)]
pub async fn set_active_program(program_id: ProgramId) -> Result<ProgramDetail, ServerFnError> {
    Ok(programs::set_active(&state.db, user.owner(), program_id).await?)
}

/// Uploads a `program.json`: validates it and stores it as a new program or as a new version of
/// one of the user's programs. A document equal to the program's latest version adds nothing.
///
/// `413` for a body over [`UPLOAD_BODY_LIMIT`] or a document over [`MAX_DOCUMENT_BYTES`]; `422`
/// with [`ProgramProblems`] for a document that does not parse or breaks a rule.
#[post("/api/programs/upload", state: Extension<AppState>, user: AuthUser)]
#[middleware(dioxus::server::axum::middleware::from_fn(programs::limit_upload_body))]
pub async fn upload_program(
    target: UploadTarget,
    document: String,
) -> Result<UploadOutcome, ServerFnError> {
    Ok(programs::upload(&state.db, user, target, &document).await?)
}

/// Every version of one of the user's programs, oldest first, without their documents.
#[post("/api/programs/versions", state: Extension<AppState>, user: AuthUser)]
pub async fn list_program_versions(
    program_id: ProgramId,
) -> Result<Vec<VersionView>, ServerFnError> {
    Ok(programs::versions(&state.db, user.owner(), program_id).await?)
}

/// Archives (hides) or restores one of the user's programs. Nothing is deleted: its versions and
/// the sessions run from them stay. The active program cannot be archived (`409`).
#[post("/api/programs/archive", state: Extension<AppState>, user: AuthUser)]
pub async fn set_program_archived(
    program_id: ProgramId,
    archived: bool,
) -> Result<(), ServerFnError> {
    Ok(programs::set_archived(&state.db, user, program_id, archived).await?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn problems() -> ProgramProblems {
        ProgramProblems {
            errors: vec![ProgramProblem {
                path: "name".to_owned(),
                message: "must not be blank".to_owned(),
                line: None,
                column: None,
            }],
            omitted: 2,
        }
    }

    fn error(code: u16, details: Option<serde_json::Value>) -> ServerFnError {
        ServerFnError::ServerError {
            message: String::new(),
            code,
            details,
        }
    }

    #[test]
    fn problems_are_read_from_either_shape_of_details() {
        let details = serde_json::to_value(problems()).unwrap();
        assert_eq!(
            ProgramProblems::from_error(&error(422, Some(details.clone()))),
            Some(problems())
        );
        let wrapped = json!({ "ServerError": { "code": 422, "message": "", "details": details } });
        assert_eq!(
            ProgramProblems::from_error(&error(422, Some(wrapped))),
            Some(problems())
        );
        for other in [
            error(409, Some(details)),
            error(422, None),
            error(422, Some(json!("nope"))),
            ServerFnError::new("x"),
        ] {
            assert_eq!(ProgramProblems::from_error(&other), None, "{other:?}");
        }
    }

    #[test]
    fn problems_serialize_like_the_domain_errors() {
        assert_eq!(
            serde_json::to_value(problems()).unwrap(),
            json!({ "errors": [{ "path": "name", "message": "must not be blank" }], "omitted": 2 })
        );
    }

    #[test]
    fn a_parse_error_is_one_problem_with_its_position() {
        let error = Program::from_json("{\n  \"name\": ").unwrap_err();
        let problems = ProgramProblems::from(error);
        assert_eq!(problems.errors.len(), 1);
        assert_eq!(problems.omitted, 0);
        assert_eq!(problems.errors[0].path, "");
        assert_eq!(problems.errors[0].line, Some(2));
        assert!(problems.errors[0].column.is_some());
        // A size error has no position.
        let error = Program::from_json(&" ".repeat(MAX_DOCUMENT_BYTES + 1)).unwrap_err();
        let problems = ProgramProblems::from(error);
        assert_eq!(
            (problems.errors[0].line, problems.errors[0].column),
            (None, None)
        );
    }

    #[test]
    fn broken_rules_are_all_listed_with_their_paths() {
        let error =
            Program::from_json(r#"{"schema_version": 1, "name": " ", "days": [], "rotation": []}"#)
                .unwrap_err();
        let problems = ProgramProblems::from(error);
        let paths: Vec<&str> = problems.errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"name"), "{problems:?}");
        assert!(paths.contains(&"days"), "{problems:?}");
        assert!(problems.errors.iter().all(|e| e.line.is_none()));
    }

    #[test]
    fn the_transport_limit_leaves_room_for_an_escaped_document() {
        // Each byte of a valid document takes at most two once escaped in a JSON string.
        let document = "\"\\\n\t".repeat(MAX_DOCUMENT_BYTES / 4);
        let target = UploadTarget::NewProgram {
            creation_id: CreationId::from_uuid(uuid::Uuid::max()),
        };
        let body = json!({ "target": target, "document": document }).to_string();
        assert!(body.len() <= UPLOAD_BODY_LIMIT, "{}", body.len());
    }

    #[test]
    fn upload_targets_are_tagged() {
        let id = uuid::Uuid::from_u128(1);
        assert_eq!(
            serde_json::to_value(UploadTarget::NewVersion {
                program_id: ProgramId::from_uuid(id)
            })
            .unwrap(),
            json!({ "kind": "new_version", "program_id": id })
        );
        assert_eq!(
            serde_json::to_value(UploadTarget::NewProgram {
                creation_id: CreationId::from_uuid(id)
            })
            .unwrap(),
            json!({ "kind": "new_program", "creation_id": id })
        );
    }
}

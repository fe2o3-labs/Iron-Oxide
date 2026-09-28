//! Programs (#19): the logic behind `crate::api::programs`.

use std::borrow::Cow;

use dioxus::logger::tracing;
use dioxus::prelude::ServerFnError;
use dioxus::server::axum::{
    Json,
    body::{Body, to_bytes},
    extract::Request,
    http::{StatusCode, header::CONTENT_LENGTH},
    middleware::Next,
    response::{IntoResponse, Response},
};
use iron_oxide_domain::{
    CreationId, ProgramId,
    program::{BuiltinProgramId, Program, limits::MAX_DOCUMENT_BYTES},
};
use serde_json::json;
use sqlx::{PgPool, types::JsonValue};

use super::{ApiError, timestamp};
use crate::api::programs::{
    BuiltinProgramView, ProgramDetail, ProgramView, UPLOAD_BODY_LIMIT, UploadOutcome, UploadTarget,
    VersionView,
};
use crate::server::db::{
    active_program,
    error::Change,
    ids::UserId,
    programs::{self, ProgramVersion},
};

/// The built-in programs, by name, with their current document.
pub async fn list_builtins(pool: &PgPool) -> Result<Vec<BuiltinProgramView>, ApiError> {
    programs::list_builtins(pool)
        .await?
        .into_iter()
        .map(|builtin| {
            let builtin_id = builtin
                .program
                .source_builtin_id
                .ok_or_else(|| ApiError::internal("a built-in program without a built-in id"))?;
            Ok(BuiltinProgramView {
                builtin_id: BuiltinProgramId::new(builtin_id).map_err(ApiError::internal)?,
                name: builtin.program.name,
                version: builtin.latest.version,
                document: stored_program(&builtin.latest)?,
            })
        })
        .collect()
}

/// Copies the built-in `builtin_id` into a new program of `owner`'s (idempotent on `creation`).
pub async fn copy_builtin(
    pool: &PgPool,
    owner: UserId,
    builtin_id: &str,
    creation: CreationId,
) -> Result<ProgramDetail, ApiError> {
    // Not a slug, so no built-in has that id.
    let builtin_id = BuiltinProgramId::new(builtin_id).map_err(|_| ApiError::NotFound)?;
    let (_, program, version) =
        programs::copy_builtin(pool, owner, creation.into(), builtin_id.as_str()).await?;
    detail_of(program, &version)
}

/// `owner`'s programs, oldest first.
pub async fn list(
    pool: &PgPool,
    owner: UserId,
    include_archived: bool,
) -> Result<Vec<ProgramView>, ApiError> {
    programs::list(pool, owner, include_archived)
        .await?
        .into_iter()
        .map(view)
        .collect()
}

/// `owner`'s program `id` with its latest version.
pub async fn detail(
    pool: &PgPool,
    owner: UserId,
    id: ProgramId,
) -> Result<ProgramDetail, ApiError> {
    let program = programs::get(pool, owner, id.into()).await?;
    let version = programs::latest_version(pool, owner, id.into()).await?;
    detail_of(program, &version)
}

/// `owner`'s active program with its latest version, if they chose one.
pub async fn active(pool: &PgPool, owner: UserId) -> Result<Option<ProgramDetail>, ApiError> {
    match active_program::get(pool, owner).await? {
        Some(id) => detail(pool, owner, id.into()).await.map(Some),
        None => Ok(None),
    }
}

/// Makes `owner`'s program `id` the active one. `409` if it is archived.
pub async fn set_active(
    pool: &PgPool,
    owner: UserId,
    id: ProgramId,
) -> Result<ProgramDetail, ApiError> {
    active_program::set(pool, owner, id.into()).await?;
    detail(pool, owner, id).await
}

/// Every version of `owner`'s program `id`, oldest first.
pub async fn versions(
    pool: &PgPool,
    owner: UserId,
    id: ProgramId,
) -> Result<Vec<VersionView>, ApiError> {
    programs::list_versions(pool, owner, id.into())
        .await?
        .iter()
        .map(version_view)
        .collect()
}

/// Archives or restores `owner`'s program `id`. `409` when archiving the active program.
pub async fn set_archived(
    pool: &PgPool,
    owner: UserId,
    id: ProgramId,
    archived: bool,
) -> Result<(), ApiError> {
    Ok(programs::set_archived(pool, owner, id.into(), archived).await?)
}

/// Validates an uploaded document and stores it as a new program or a new version.
pub async fn upload(
    pool: &PgPool,
    owner: UserId,
    target: UploadTarget,
    document: &str,
) -> Result<UploadOutcome, ApiError> {
    let program = parse_upload(document)?;
    // Stored as written, like the built-ins, so that uploading the same file again equals the
    // stored version (compared as jsonb: formatting and key order do not matter).
    let stored: JsonValue = serde_json::from_str(document).map_err(ApiError::internal)?;
    let (change, program, version) = match target {
        UploadTarget::NewProgram { creation_id } => {
            programs::create(pool, owner, creation_id.into(), &program.name, &stored).await?
        }
        UploadTarget::NewVersion { program_id } => {
            let (change, version) =
                programs::add_version(pool, owner, program_id.into(), &stored).await?;
            let program = programs::get(pool, owner, program_id.into()).await?;
            (change, program, version)
        }
    };
    Ok(UploadOutcome {
        program: view(program)?,
        version: version_view(&version)?,
        saved: change == Change::Applied,
    })
}

/// Parses and validates an uploaded document, refusing an oversized one before parsing.
fn parse_upload(document: &str) -> Result<Program, ApiError> {
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(too_large());
    }
    Ok(Program::from_json(document)?)
}

fn too_large() -> ApiError {
    ApiError::TooLarge(Cow::Owned(format!(
        "The program file is too large (the limit is {} KiB).",
        MAX_DOCUMENT_BYTES / 1024
    )))
}

/// Middleware of `upload_program`: reads the whole body before the server function does, and
/// refuses it with `413` past [`UPLOAD_BODY_LIMIT`], announced (`Content-Length`) or actually sent.
///
/// Dioxus reads a server function's body itself and panics when that fails (axum's 2 MiB default
/// limit, a broken connection), so the body it gets here is already complete and within the limit.
pub async fn limit_upload_body(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();
    let announced = parts
        .headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if announced.is_some_and(|length| length > UPLOAD_BODY_LIMIT as u64) {
        return refuse("announced body too large");
    }
    match to_bytes(body, UPLOAD_BODY_LIMIT).await {
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        // Over the limit, or unreadable (the client went away: nobody reads the answer).
        Err(error) => {
            tracing::info!(%error, "program upload body not read");
            refuse("body too large or unreadable")
        }
    }
}

/// A `413`, in the shape Dioxus gives the errors a server function returns (so the client decodes
/// it the same way): `{"message", "code", "data": <the ServerFnError>}`.
fn refuse(reason: &'static str) -> Response {
    tracing::info!(reason, "program upload refused");
    let error = ServerFnError::from(too_large());
    let body = json!({ "message": error.to_string(), "code": 413, "data": error });
    (StatusCode::PAYLOAD_TOO_LARGE, Json(body)).into_response()
}

fn view(program: programs::Program) -> Result<ProgramView, ApiError> {
    Ok(ProgramView {
        id: program.id.into(),
        name: program.name,
        source_builtin_id: program
            .source_builtin_id
            .map(BuiltinProgramId::new)
            .transpose()
            .map_err(ApiError::internal)?,
        archived: program.archived,
        created_at: timestamp(program.created_at)?,
    })
}

fn version_view(version: &ProgramVersion) -> Result<VersionView, ApiError> {
    Ok(VersionView {
        id: version.id.into(),
        version: version.version,
        created_at: timestamp(version.created_at)?,
    })
}

/// A stored document as a program. Every stored document was validated when it was saved, so a
/// failure means the stored data is wrong.
fn stored_program(version: &ProgramVersion) -> Result<Program, ApiError> {
    Program::from_json(&version.document.to_string()).map_err(|error| {
        ApiError::internal(format!(
            "stored program version {:?} does not load: {error}",
            version.id
        ))
    })
}

fn detail_of(
    program: programs::Program,
    version: &ProgramVersion,
) -> Result<ProgramDetail, ApiError> {
    Ok(ProgramDetail {
        document: stored_program(version)?,
        program: view(program)?,
        version: version_view(version)?,
    })
}

#[cfg(test)]
mod tests;

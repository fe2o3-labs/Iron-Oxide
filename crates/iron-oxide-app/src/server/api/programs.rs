//! Programs (#19): the logic behind `crate::api::programs`.

use std::borrow::Cow;

use dioxus::server::axum::{extract::Request, middleware::Next, response::Response};
use iron_oxide_domain::{
    CreationId, ProgramId,
    entitlements::Quota,
    program::{BuiltinProgramId, Program, limits::MAX_DOCUMENT_BYTES},
};
use sqlx::{PgConnection, PgPool, types::JsonValue};

use super::{ApiError, body_limit, timestamp};
use crate::api::programs::{
    BuiltinProgramView, ProgramDetail, ProgramView, UPLOAD_BODY_LIMIT, UploadOutcome, UploadTarget,
    VersionView,
};
use crate::server::auth::AuthUser;
use crate::server::db::{
    active_program,
    error::Change,
    ids::UserId,
    programs::{self, ProgramVersion, Reserve},
};
use crate::server::entitlements;

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

/// Copies the built-in `builtin_id` into a new program of `user`'s (idempotent on `creation`).
/// Takes a program slot: `403` at the plan's limit, except for a replay.
pub async fn copy_builtin(
    pool: &PgPool,
    user: AuthUser,
    builtin_id: &str,
    creation: CreationId,
) -> Result<ProgramDetail, ApiError> {
    // Not a slug, so no built-in has that id.
    let builtin_id = BuiltinProgramId::new(builtin_id).map_err(|_| ApiError::NotFound)?;
    let (_, program, version) = programs::copy_builtin(
        pool,
        user.owner(),
        creation.into(),
        builtin_id.as_str(),
        program_slot(user),
    )
    .await?;
    detail_of(program, &version)
}

/// The `reserve` step of the writes that add an unarchived program: one `CustomPrograms` slot,
/// under the user's row lock, in the write's transaction (`403` at the plan's limit).
fn program_slot(
    user: AuthUser,
) -> impl for<'c> FnOnce(&'c mut PgConnection) -> Reserve<'c, ApiError> {
    move |tx| {
        Box::pin(async move {
            entitlements::reserve_quota(tx, user, Quota::CustomPrograms).await?;
            Ok(())
        })
    }
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

/// Archives or restores `user`'s program `id`. `409` when archiving the active program; restoring
/// takes a program slot (`403` at the plan's limit), unless the program is not archived.
pub async fn set_archived(
    pool: &PgPool,
    user: AuthUser,
    id: ProgramId,
    archived: bool,
) -> Result<(), ApiError> {
    if archived {
        Ok(programs::archive(pool, user.owner(), id.into()).await?)
    } else {
        programs::unarchive(pool, user.owner(), id.into(), program_slot(user)).await
    }
}

/// Validates an uploaded document and stores it as a new program or a new version.
///
/// A new program takes a program slot (`403` at the plan's limit, except for a replay); a new
/// version does not.
pub async fn upload(
    pool: &PgPool,
    user: AuthUser,
    target: UploadTarget,
    document: &str,
) -> Result<UploadOutcome, ApiError> {
    let program = parse_upload(document)?;
    // Stored as written, like the built-ins, so that uploading the same file again equals the
    // stored version (compared as jsonb: formatting and key order do not matter).
    let stored: JsonValue = serde_json::from_str(document).map_err(ApiError::internal)?;
    let owner = user.owner();
    let (change, program, version) = match target {
        UploadTarget::NewProgram { creation_id } => {
            let (name, creation) = (&program.name, creation_id.into());
            programs::create(pool, owner, creation, name, &stored, program_slot(user)).await?
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

/// Middleware of `upload_program`: checks the session first, then refuses a body over
/// [`UPLOAD_BODY_LIMIT`] with `413` before anything reads it (see
/// [`body_limit::signed_in_and_capped`]).
pub async fn limit_upload_body(request: Request, next: Next) -> Response {
    body_limit::signed_in_and_capped(request, next, UPLOAD_BODY_LIMIT, too_large, None).await
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

//! Programs (`programs`) and their immutable versions (`program_versions`).
//!
//! A user's program is only ever read or changed through its owner's [`UserId`]. Built-in programs
//! have no owner: they are written by [`seed_builtins`] at startup, read by everyone through
//! [`list_builtins`], and become a user's own program through [`copy_builtin`]. No function here
//! can change a built-in on behalf of a user (every write filters on `user_id = $user`, which a
//! NULL owner never matches).
//!
//! Versions are never updated (a trigger rejects it): an upload adds a new version, and sessions
//! keep pointing at the version they were run from.

use sqlx::{
    PgExecutor, PgPool, Postgres, Transaction,
    types::{JsonValue, Uuid, time::OffsetDateTime},
};

use super::{
    error::{Change, RepoError, narrow},
    ids::{ProgramId, ProgramVersionId, UserId},
};

/// A program's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub id: ProgramId,
    pub name: String,
    /// The built-in this program is (or was copied from).
    pub source_builtin_id: Option<String>,
    pub archived: bool,
    pub created_at: OffsetDateTime,
}

/// One immutable version of a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramVersion {
    pub id: ProgramVersionId,
    pub program_id: ProgramId,
    /// 1 for the first version, then +1 for each new one.
    pub version: u32,
    /// The program JSON document (the domain `Program`, with its `schema_version`).
    pub document: JsonValue,
    pub created_at: OffsetDateTime,
}

/// A built-in program and its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builtin {
    pub program: Program,
    pub latest: ProgramVersion,
}

/// A `programs` row.
struct ProgramRow {
    id: Uuid,
    name: String,
    source_builtin_id: Option<String>,
    archived: bool,
    created_at: OffsetDateTime,
}

impl From<ProgramRow> for Program {
    fn from(row: ProgramRow) -> Self {
        Self {
            id: ProgramId::from_uuid(row.id),
            name: row.name,
            source_builtin_id: row.source_builtin_id,
            archived: row.archived,
            created_at: row.created_at,
        }
    }
}

/// A `program_versions` row.
struct VersionRow {
    id: Uuid,
    program_id: Uuid,
    version: i32,
    document: JsonValue,
    created_at: OffsetDateTime,
}

impl TryFrom<VersionRow> for ProgramVersion {
    type Error = RepoError;

    fn try_from(row: VersionRow) -> Result<Self, RepoError> {
        Ok(Self {
            id: ProgramVersionId::from_uuid(row.id),
            program_id: ProgramId::from_uuid(row.program_id),
            version: narrow(row.version.into(), "program_versions.version")?,
            document: row.document,
            created_at: row.created_at,
        })
    }
}

/// A built-in program to seed: the id, name and JSON of one of the programs in `programs/`.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinSeed {
    /// The stable slug, e.g. `full-body-3day`.
    pub builtin_id: &'static str,
    pub name: &'static str,
    /// The program document, as written in `programs/*.json`.
    pub json: &'static str,
}

/// The built-in programs seeded at startup.
///
/// Empty until the program documents land (#56): they will come from the domain crate's
/// `builtin_programs()` (id, name and `json()` of each), mapped to [`BuiltinSeed`]s.
pub const BUILTIN_PROGRAMS: &[BuiltinSeed] = &[];

/// Advisory lock key serialising concurrent seeds (two instances starting at once).
const SEED_LOCK: i64 = 0x6972_6f6e_7365_6564; // "ironseed"

/// Upserts the built-in programs: creates the missing ones, adds a version to those whose
/// document changed, un-archives the listed ones and archives built-ins no longer listed. Safe to
/// run on every start.
///
/// # Errors
/// [`RepoError::Invalid`] when a seed's JSON does not parse or breaks a constraint.
pub async fn seed_builtins(pool: &PgPool, seeds: &[BuiltinSeed]) -> Result<(), RepoError> {
    let mut tx = pool.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock($1)", SEED_LOCK)
        .execute(&mut *tx)
        .await?;
    for seed in seeds {
        let document: JsonValue =
            serde_json::from_str(seed.json).map_err(|_| RepoError::Invalid { constraint: None })?;
        let program_id = sqlx::query_scalar!(
            "INSERT INTO programs (source_builtin_id, name) VALUES ($1, $2)
             ON CONFLICT (source_builtin_id) WHERE user_id IS NULL
             DO UPDATE SET name = EXCLUDED.name, archived = false
             RETURNING id",
            seed.builtin_id,
            seed.name,
        )
        .fetch_one(&mut *tx)
        .await?;
        let unchanged = sqlx::query_scalar!(
            r#"SELECT document = $2 AS "same!" FROM program_versions
               WHERE program_id = $1 ORDER BY version DESC LIMIT 1"#,
            program_id,
            document,
        )
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
        if !unchanged {
            insert_next_version(&mut tx, ProgramId::from_uuid(program_id), &document).await?;
        }
    }
    let listed: Vec<String> = seeds.iter().map(|s| s.builtin_id.to_owned()).collect();
    sqlx::query!(
        "UPDATE programs SET archived = true
         WHERE user_id IS NULL AND NOT archived AND source_builtin_id <> ALL($1)",
        &listed,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The built-in programs that are not archived, with their latest version, by name. Public data:
/// not scoped to a user.
pub async fn list_builtins(pool: &PgPool) -> Result<Vec<Builtin>, RepoError> {
    sqlx::query!(
        r#"SELECT p.id, p.name, p.source_builtin_id, p.archived, p.created_at,
                  v.id AS "version_id!", v.version AS "version!", v.document AS "document!",
                  v.created_at AS "version_created_at!"
           FROM programs p
           JOIN LATERAL (
               SELECT id, version, document, created_at FROM program_versions
               WHERE program_id = p.id ORDER BY version DESC LIMIT 1
           ) v ON true
           WHERE p.user_id IS NULL AND NOT p.archived
           ORDER BY p.name, p.id"#
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        let program = Program {
            id: ProgramId::from_uuid(row.id),
            name: row.name,
            source_builtin_id: row.source_builtin_id,
            archived: row.archived,
            created_at: row.created_at,
        };
        Ok(Builtin {
            latest: ProgramVersion {
                id: ProgramVersionId::from_uuid(row.version_id),
                program_id: program.id,
                version: narrow(row.version.into(), "program_versions.version")?,
                document: row.document,
                created_at: row.version_created_at,
            },
            program,
        })
    })
    .collect()
}

/// Copies the latest version of a built-in into a new program owned by the user (version 1).
///
/// # Errors
/// [`RepoError::NotFound`] when no built-in with that id is available.
pub async fn copy_builtin(
    pool: &PgPool,
    user: UserId,
    builtin_id: &str,
) -> Result<(Program, ProgramVersion), RepoError> {
    let mut tx = pool.begin().await?;
    let source = sqlx::query!(
        "SELECT p.name, v.document FROM programs p
         JOIN program_versions v ON v.program_id = p.id
         WHERE p.user_id IS NULL AND NOT p.archived AND p.source_builtin_id = $1
         ORDER BY v.version DESC LIMIT 1",
        builtin_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RepoError::NotFound)?;
    let created = insert_program(
        &mut tx,
        user,
        &source.name,
        Some(builtin_id),
        &source.document,
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

/// Creates a program owned by the user, with `document` as version 1.
///
/// # Errors
/// [`RepoError::Invalid`] for an empty or too long name (1 to 100 characters) or a document that
/// is not a JSON object with a numeric `schema_version`.
pub async fn create(
    pool: &PgPool,
    user: UserId,
    name: &str,
    document: &JsonValue,
) -> Result<(Program, ProgramVersion), RepoError> {
    let mut tx = pool.begin().await?;
    let created = insert_program(&mut tx, user, name, None, document).await?;
    tx.commit().await?;
    Ok(created)
}

async fn insert_program(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    name: &str,
    source_builtin_id: Option<&str>,
    document: &JsonValue,
) -> Result<(Program, ProgramVersion), RepoError> {
    let row = sqlx::query_as!(
        ProgramRow,
        "INSERT INTO programs (user_id, source_builtin_id, name) VALUES ($1, $2, $3)
         RETURNING id, name, source_builtin_id, archived, created_at",
        user.as_uuid(),
        source_builtin_id,
        name,
    )
    .fetch_one(&mut **tx)
    .await?;
    let program = Program::from(row);
    let version = insert_next_version(tx, program.id, document).await?;
    Ok((program, version))
}

/// Adds the next version of a program. The caller has checked (or is) the owner.
async fn insert_next_version(
    tx: &mut Transaction<'_, Postgres>,
    program: ProgramId,
    document: &JsonValue,
) -> Result<ProgramVersion, RepoError> {
    let row = sqlx::query_as!(
        VersionRow,
        r#"INSERT INTO program_versions (program_id, version, document)
           SELECT $1, COALESCE(MAX(version), 0) + 1, $2 FROM program_versions WHERE program_id = $1
           RETURNING id, program_id, version, document, created_at"#,
        program.as_uuid(),
        document,
    )
    .fetch_one(&mut **tx)
    .await?;
    ProgramVersion::try_from(row)
}

/// One of the user's programs.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id.
pub async fn get(pool: &PgPool, user: UserId, id: ProgramId) -> Result<Program, RepoError> {
    let row = sqlx::query_as!(
        ProgramRow,
        "SELECT id, name, source_builtin_id, archived, created_at FROM programs
         WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(RepoError::NotFound)?;
    Ok(Program::from(row))
}

/// The user's programs, oldest first; archived ones only when `include_archived`.
pub async fn list(
    pool: &PgPool,
    user: UserId,
    include_archived: bool,
) -> Result<Vec<Program>, RepoError> {
    let rows = sqlx::query_as!(
        ProgramRow,
        "SELECT id, name, source_builtin_id, archived, created_at FROM programs
         WHERE user_id = $1 AND ($2 OR NOT archived)
         ORDER BY created_at, id",
        user.as_uuid(),
        include_archived,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Program::from).collect())
}

/// Renames one of the user's programs.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id; [`RepoError::Invalid`] for
/// an empty or too long name.
pub async fn rename(
    pool: &PgPool,
    user: UserId,
    id: ProgramId,
    name: &str,
) -> Result<(), RepoError> {
    let updated = sqlx::query!(
        "UPDATE programs SET name = $3 WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
        name,
    )
    .execute(pool)
    .await?
    .rows_affected();
    found(updated)
}

/// Archives (hides) or restores one of the user's programs. Archiving keeps its versions and the
/// sessions run from them.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id.
pub async fn set_archived(
    pool: &PgPool,
    user: UserId,
    id: ProgramId,
    archived: bool,
) -> Result<(), RepoError> {
    let updated = sqlx::query!(
        "UPDATE programs SET archived = $3 WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
        archived,
    )
    .execute(pool)
    .await?
    .rows_affected();
    found(updated)
}

/// Adds a version to one of the user's programs, unless `document` equals the latest version, in
/// which case that version is returned with [`Change::Unchanged`] (so a retried upload does not
/// create a second version).
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id; [`RepoError::Invalid`] for a
/// document that is not a JSON object with a numeric `schema_version`.
pub async fn add_version(
    pool: &PgPool,
    user: UserId,
    program: ProgramId,
    document: &JsonValue,
) -> Result<(Change, ProgramVersion), RepoError> {
    let mut tx = pool.begin().await?;
    // Locks the program row: concurrent uploads get consecutive version numbers.
    sqlx::query!(
        "SELECT id FROM programs WHERE id = $1 AND user_id = $2 FOR UPDATE",
        program.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RepoError::NotFound)?;
    let latest = latest_in(&mut *tx, user, program).await?;
    if let Some(latest) = latest.filter(|latest| &latest.document == document) {
        return Ok((Change::Unchanged, latest));
    }
    let version = insert_next_version(&mut tx, program, document).await?;
    tx.commit().await?;
    Ok((Change::Applied, version))
}

async fn latest_in(
    executor: impl PgExecutor<'_>,
    user: UserId,
    program: ProgramId,
) -> Result<Option<ProgramVersion>, RepoError> {
    sqlx::query_as!(
        VersionRow,
        "SELECT id, program_id, version, document, created_at FROM program_versions
         WHERE program_id = $1 AND user_id = $2 ORDER BY version DESC LIMIT 1",
        program.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(executor)
    .await?
    .map(ProgramVersion::try_from)
    .transpose()
}

/// Every version of one of the user's programs, oldest first.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id.
pub async fn list_versions(
    pool: &PgPool,
    user: UserId,
    program: ProgramId,
) -> Result<Vec<ProgramVersion>, RepoError> {
    let versions = sqlx::query_as!(
        VersionRow,
        "SELECT id, program_id, version, document, created_at FROM program_versions
         WHERE program_id = $1 AND user_id = $2 ORDER BY version",
        program.as_uuid(),
        user.as_uuid(),
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(ProgramVersion::try_from)
    .collect::<Result<Vec<_>, RepoError>>()?;
    // A program always has at least one version (created together), so none means not the user's.
    if versions.is_empty() {
        return Err(RepoError::NotFound);
    }
    Ok(versions)
}

/// One version of one of the user's programs.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program version with that id.
pub async fn get_version(
    pool: &PgPool,
    user: UserId,
    id: ProgramVersionId,
) -> Result<ProgramVersion, RepoError> {
    let row = sqlx::query_as!(
        VersionRow,
        "SELECT id, program_id, version, document, created_at FROM program_versions
         WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(pool)
    .await?
    .ok_or(RepoError::NotFound)?;
    ProgramVersion::try_from(row)
}

/// The latest version of one of the user's programs.
///
/// # Errors
/// [`RepoError::NotFound`] when the user has no program with that id.
pub async fn latest_version(
    pool: &PgPool,
    user: UserId,
    program: ProgramId,
) -> Result<ProgramVersion, RepoError> {
    latest_in(pool, user, program)
        .await?
        .ok_or(RepoError::NotFound)
}

fn found(rows_affected: u64) -> Result<(), RepoError> {
    if rows_affected == 0 {
        Err(RepoError::NotFound)
    } else {
        Ok(())
    }
}

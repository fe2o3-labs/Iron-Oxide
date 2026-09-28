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

use iron_oxide_domain::program::BuiltinProgram;
use sqlx::{
    PgExecutor, PgPool, Postgres, Transaction,
    types::{JsonValue, Uuid, time::OffsetDateTime},
};

use super::{
    error::{Change, RepoError, narrow},
    ids::{CreationId, ProgramId, ProgramVersionId, UserId},
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
pub struct BuiltinSeed<'a> {
    /// The stable slug, e.g. `full-body-3day`.
    pub builtin_id: &'a str,
    pub name: &'a str,
    /// The program document, as written in `programs/*.json`.
    pub json: &'a str,
}

/// The seeds of the domain crate's built-in programs (already parsed and validated by
/// [`builtin_programs`](iron_oxide_domain::program::builtin_programs)), in display order.
pub fn builtin_seeds(builtins: &[BuiltinProgram]) -> Vec<BuiltinSeed<'_>> {
    builtins
        .iter()
        .map(|builtin| BuiltinSeed {
            builtin_id: builtin.id().as_str(),
            name: &builtin.program().name,
            json: builtin.json(),
        })
        .collect()
}

/// Advisory lock key serialising concurrent seeds (two instances starting at once).
const SEED_LOCK: i64 = 0x6972_6f6e_7365_6564; // "ironseed"

/// Upserts the built-in programs: creates the missing ones, adds a version to those whose
/// document changed, un-archives the listed ones and archives built-ins no longer listed. Safe to
/// run on every start.
///
/// # Errors
/// [`RepoError::Invalid`] when a seed's JSON does not parse or breaks a constraint.
pub async fn seed_builtins(pool: &PgPool, seeds: &[BuiltinSeed<'_>]) -> Result<(), RepoError> {
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
/// Idempotent on `creation`: when the user already created a program with it, that program and its
/// first version are returned with [`Change::Unchanged`] (even if the built-in changed or was
/// archived since), provided it was copied from the same built-in.
///
/// # Errors
/// - [`RepoError::NotFound`] when no built-in with that id is available.
/// - [`RepoError::Conflict`] when `creation` was already used for another request.
pub async fn copy_builtin(
    pool: &PgPool,
    user: UserId,
    creation: CreationId,
    builtin_id: &str,
) -> Result<(Change, Program, ProgramVersion), RepoError> {
    let same = |program: &Program, _same_document: bool| {
        program.source_builtin_id.as_deref() == Some(builtin_id)
    };
    let mut tx = pool.begin().await?;
    if let Some(existing) = find_creation(&mut tx, user, creation, None).await? {
        return replayed(existing, same);
    }
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
    let new = NewProgram {
        name: &source.name,
        source_builtin_id: Some(builtin_id),
        document: &source.document,
        compare_document: false,
    };
    let result = insert_program(&mut tx, user, creation, &new, same).await?;
    tx.commit().await?;
    Ok(result)
}

/// Creates a program owned by the user, with `document` as version 1.
///
/// Idempotent on `creation`: when the user already created a program with it, that program and its
/// first version are returned with [`Change::Unchanged`], provided the first version has the same
/// document and it was not copied from a built-in (the name may have been changed since).
///
/// # Errors
/// - [`RepoError::Conflict`] when `creation` was already used for another request.
/// - [`RepoError::Invalid`] for an empty or too long name (1 to 100 characters) or a document that
///   is not a JSON object with a numeric `schema_version`.
pub async fn create(
    pool: &PgPool,
    user: UserId,
    creation: CreationId,
    name: &str,
    document: &JsonValue,
) -> Result<(Change, Program, ProgramVersion), RepoError> {
    let same = |program: &Program, same_document: bool| {
        program.source_builtin_id.is_none() && same_document
    };
    let mut tx = pool.begin().await?;
    if let Some(existing) = find_creation(&mut tx, user, creation, Some(document)).await? {
        return replayed(existing, same);
    }
    let new = NewProgram {
        name,
        source_builtin_id: None,
        document,
        compare_document: true,
    };
    let result = insert_program(&mut tx, user, creation, &new, same).await?;
    tx.commit().await?;
    Ok(result)
}

/// A program found by its creation id, with its first version.
struct Existing {
    program: Program,
    first: ProgramVersion,
    /// Whether the first version's document equals the one compared with, as jsonb (so that
    /// `1e16` and `10000000000000000` are equal, as Postgres stores them). `false` when none was.
    same_document: bool,
}

/// The user's program created with `creation`, and its first version, compared with `compare`.
async fn find_creation(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    creation: CreationId,
    compare: Option<&JsonValue>,
) -> Result<Option<Existing>, RepoError> {
    let program = sqlx::query_as!(
        ProgramRow,
        "SELECT id, name, source_builtin_id, archived, created_at FROM programs
         WHERE user_id = $1 AND creation_id = $2",
        user.as_uuid(),
        creation.as_uuid(),
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(program) = program.map(Program::from) else {
        return Ok(None);
    };
    let first = sqlx::query!(
        r#"SELECT id, program_id, version, document, created_at,
                  COALESCE(document = $3::jsonb, false) AS "same_document!"
           FROM program_versions
           WHERE program_id = $1 AND user_id = $2 AND version = 1"#,
        program.id.as_uuid(),
        user.as_uuid(),
        compare,
    )
    .fetch_optional(&mut **tx)
    .await?
    // Created together with the program in one transaction, and never deleted on its own.
    .ok_or(RepoError::Corrupt("program_versions: first version missing"))?;
    let same_document = first.same_document;
    let first = ProgramVersion::try_from(VersionRow {
        id: first.id,
        program_id: first.program_id,
        version: first.version,
        document: first.document,
        created_at: first.created_at,
    })?;
    Ok(Some(Existing {
        program,
        first,
        same_document,
    }))
}

/// The answer to a retried create: the same request gets its program back, another one conflicts.
fn replayed(
    existing: Existing,
    same: impl Fn(&Program, bool) -> bool,
) -> Result<(Change, Program, ProgramVersion), RepoError> {
    if same(&existing.program, existing.same_document) {
        Ok((Change::Unchanged, existing.program, existing.first))
    } else {
        Err(RepoError::Conflict)
    }
}

/// What [`insert_program`] creates.
struct NewProgram<'a> {
    name: &'a str,
    source_builtin_id: Option<&'a str>,
    /// Version 1.
    document: &'a JsonValue,
    /// Whether a retry must have the same document (a create) or not (a copy).
    compare_document: bool,
}

async fn insert_program(
    tx: &mut Transaction<'_, Postgres>,
    user: UserId,
    creation: CreationId,
    new: &NewProgram<'_>,
    same: impl Fn(&Program, bool) -> bool,
) -> Result<(Change, Program, ProgramVersion), RepoError> {
    let row = sqlx::query_as!(
        ProgramRow,
        "INSERT INTO programs (user_id, creation_id, source_builtin_id, name)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, creation_id) DO NOTHING
         RETURNING id, name, source_builtin_id, archived, created_at",
        user.as_uuid(),
        creation.as_uuid(),
        new.source_builtin_id,
        new.name,
    )
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else {
        // A concurrent request with the same creation id committed first: answer as a retry.
        let compare = new.compare_document.then_some(new.document);
        return match find_creation(tx, user, creation, compare).await? {
            Some(existing) => replayed(existing, same),
            None => Err(RepoError::Transient),
        };
    };
    let program = Program::from(row);
    let version = insert_next_version(tx, program.id, new.document).await?;
    Ok((Change::Applied, program, version))
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
/// The program's row is locked while it is checked and changed, like `active_program::set` does,
/// so a concurrent activation of the same program either happens first (and archiving is refused)
/// or waits and then sees the program is archived. A trigger enforces the same rule in the
/// database.
///
/// # Errors
/// - [`RepoError::NotFound`] when the user has no program with that id.
/// - [`RepoError::ProgramActive`] when archiving the user's active program.
pub async fn set_archived(
    pool: &PgPool,
    user: UserId,
    id: ProgramId,
    archived: bool,
) -> Result<(), RepoError> {
    let mut tx = pool.begin().await?;
    let was_archived = sqlx::query_scalar!(
        "SELECT archived FROM programs WHERE id = $1 AND user_id = $2 FOR UPDATE",
        id.as_uuid(),
        user.as_uuid(),
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(RepoError::NotFound)?;
    if archived == was_archived {
        return Ok(());
    }
    if archived {
        let active = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM active_program WHERE user_id = $1 AND program_id = $2
               ) AS "active!""#,
            user.as_uuid(),
            id.as_uuid(),
        )
        .fetch_one(&mut *tx)
        .await?;
        if active {
            return Err(RepoError::ProgramActive);
        }
    }
    sqlx::query!(
        "UPDATE programs SET archived = $3 WHERE id = $1 AND user_id = $2",
        id.as_uuid(),
        user.as_uuid(),
        archived,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
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
    if let Some(latest) = latest_in(&mut *tx, user, program).await? {
        // Compared as jsonb, like the stored document (see `Existing::same_document`).
        let same = sqlx::query_scalar!(
            r#"SELECT document = $2::jsonb AS "same!" FROM program_versions WHERE id = $1"#,
            latest.id.as_uuid(),
            document,
        )
        .fetch_one(&mut *tx)
        .await?;
        if same {
            return Ok((Change::Unchanged, latest));
        }
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

#[cfg(test)]
mod tests {
    use iron_oxide_domain::program::builtin_programs;

    use super::*;
    use crate::server::db::{
        MIGRATOR,
        testing::{self, creation, document, random_uuid},
    };

    const STARTER: BuiltinSeed<'static> = BuiltinSeed {
        builtin_id: "starter",
        name: "Starter",
        json: r#"{"schema_version": 1, "name": "Starter"}"#,
    };

    fn not_found<T: std::fmt::Debug>(result: Result<T, RepoError>) {
        assert!(matches!(result, Err(RepoError::NotFound)), "{result:?}");
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn seeding_is_idempotent_and_versions_changed_documents(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let builtins = list_builtins(&pool).await.unwrap();
        assert_eq!(builtins.len(), 1);
        let first = &builtins[0];
        assert_eq!(first.program.name, "Starter");
        assert_eq!(first.program.source_builtin_id.as_deref(), Some("starter"));
        assert_eq!(first.latest.version, 1);

        let renamed = BuiltinSeed {
            name: "Starter v2",
            json: r#"{"schema_version": 1, "name": "Starter v2"}"#,
            ..STARTER
        };
        seed_builtins(&pool, &[renamed]).await.unwrap();
        let builtins = list_builtins(&pool).await.unwrap();
        assert_eq!(builtins[0].program.id, first.program.id);
        assert_eq!(builtins[0].program.name, "Starter v2");
        assert_eq!(builtins[0].latest.version, 2);

        // A built-in no longer shipped is archived, and comes back when it is shipped again.
        seed_builtins(&pool, &[]).await.unwrap();
        assert!(list_builtins(&pool).await.unwrap().is_empty());
        seed_builtins(&pool, &[renamed]).await.unwrap();
        assert_eq!(list_builtins(&pool).await.unwrap()[0].latest.version, 2);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn seeding_rejects_invalid_documents(pool: PgPool) {
        for json in ["not json", "[1]", r#"{"name": "no schema_version"}"#] {
            let seed = BuiltinSeed { json, ..STARTER };
            let result = seed_builtins(&pool, &[seed]).await;
            assert!(
                matches!(result, Err(RepoError::Invalid { .. })),
                "{json}: {result:?}"
            );
        }
        assert!(list_builtins(&pool).await.unwrap().is_empty());
    }

    #[test]
    fn builtin_seeds_come_from_the_domain_builtins() {
        let builtins = builtin_programs().unwrap();
        let seeds = builtin_seeds(&builtins);
        assert_eq!(seeds.len(), builtins.len());
        let full_body = seeds
            .iter()
            .find(|seed| seed.builtin_id == "full-body-3day")
            .unwrap();
        assert_eq!(full_body.name, "Full-body 3-day barbell");
        assert!(full_body.json.contains(r#""schema_version": 1"#));
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn the_domain_builtins_are_seeded(pool: PgPool) {
        let builtins = builtin_programs().unwrap();
        let seeds = builtin_seeds(&builtins);
        seed_builtins(&pool, &seeds).await.unwrap();
        // Seeding again at the next start changes nothing.
        seed_builtins(&pool, &seeds).await.unwrap();
        let stored = list_builtins(&pool).await.unwrap();
        assert_eq!(stored.len(), builtins.len());
        for builtin in &builtins {
            let row = stored
                .iter()
                .find(|row| row.program.source_builtin_id.as_deref() == Some(builtin.id().as_str()))
                .unwrap();
            assert_eq!(row.program.name, builtin.program().name);
            assert_eq!(row.latest.version, 1);
            // The stored document is the program itself.
            let program =
                iron_oxide_domain::program::Program::from_json(&row.latest.document.to_string())
                    .unwrap();
            assert_eq!(&program, builtin.program());
        }
        // A user can copy it.
        let user = testing::user(&pool).await;
        let (_, copy, version) = copy_builtin(&pool, user, creation(), "full-body-3day")
            .await
            .unwrap();
        assert_eq!(copy.name, "Full-body 3-day barbell");
        assert_eq!(version.version, 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn copying_a_builtin_creates_an_owned_program(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let user = testing::user(&pool).await;
        let (_, program, version) = copy_builtin(&pool, user, creation(), "starter")
            .await
            .unwrap();
        assert_eq!(program.name, "Starter");
        assert_eq!(program.source_builtin_id.as_deref(), Some("starter"));
        assert!(!program.archived);
        assert_eq!(version.version, 1);
        assert_eq!(version.program_id, program.id);
        assert_eq!(
            version.document,
            serde_json::from_str::<JsonValue>(STARTER.json).unwrap()
        );
        assert_ne!(
            program.id,
            list_builtins(&pool).await.unwrap()[0].program.id
        );
        assert_eq!(list(&pool, user, false).await.unwrap(), vec![program]);
        not_found(copy_builtin(&pool, user, creation(), "missing").await);
        // An archived built-in cannot be copied any more.
        seed_builtins(&pool, &[]).await.unwrap();
        not_found(copy_builtin(&pool, user, creation(), "starter").await);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn create_rename_archive_and_list(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, first, version) = create(&pool, user, creation(), "First", &document("First"))
            .await
            .unwrap();
        assert_eq!(first.source_builtin_id, None);
        assert_eq!(version.version, 1);
        // Database-generated ids are UUIDv7 (#65).
        assert_eq!(first.id.as_uuid().get_version_num(), 7);
        assert_eq!(version.id.as_uuid().get_version_num(), 7);
        let (_, second, _) = create(&pool, user, creation(), "Second", &document("Second"))
            .await
            .unwrap();
        rename(&pool, user, first.id, "Renamed").await.unwrap();
        assert_eq!(get(&pool, user, first.id).await.unwrap().name, "Renamed");
        set_archived(&pool, user, second.id, true).await.unwrap();
        let active: Vec<ProgramId> = list(&pool, user, false)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(active, vec![first.id]);
        assert_eq!(list(&pool, user, true).await.unwrap().len(), 2);
        set_archived(&pool, user, second.id, false).await.unwrap();
        assert_eq!(list(&pool, user, false).await.unwrap().len(), 2);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn invalid_names_and_documents_are_rejected(pool: PgPool) {
        let user = testing::user(&pool).await;
        let long = "x".repeat(101);
        for (name, document) in [
            ("", document("x")),
            (long.as_str(), document("x")),
            ("Ok", serde_json::json!([1])),
            ("Ok", serde_json::json!({"schema_version": "1"})),
            (
                "Ok",
                serde_json::json!({"schema_version": 1, "pad": "x".repeat(1_048_576)}),
            ),
        ] {
            let result = create(&pool, user, creation(), name, &document).await;
            assert!(
                matches!(result, Err(RepoError::Invalid { .. })),
                "{result:?}"
            );
        }
        assert!(list(&pool, user, true).await.unwrap().is_empty());
        let (_, program, _) = create(&pool, user, creation(), &"x".repeat(100), &document("x"))
            .await
            .unwrap();
        let result = rename(&pool, user, program.id, "").await;
        assert!(
            matches!(result, Err(RepoError::Invalid { .. })),
            "{result:?}"
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn versions_are_numbered_and_identical_uploads_are_no_ops(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, program, v1) = create(&pool, user, creation(), "P", &document("one"))
            .await
            .unwrap();
        let (change, v2) = add_version(&pool, user, program.id, &document("two"))
            .await
            .unwrap();
        assert_eq!((change, v2.version), (Change::Applied, 2));
        let (change, again) = add_version(&pool, user, program.id, &document("two"))
            .await
            .unwrap();
        assert_eq!((change, &again), (Change::Unchanged, &v2));
        // Going back to an older document is a new version.
        let (change, v3) = add_version(&pool, user, program.id, &document("one"))
            .await
            .unwrap();
        assert_eq!((change, v3.version), (Change::Applied, 3));
        assert_eq!(
            list_versions(&pool, user, program.id).await.unwrap(),
            vec![v1.clone(), v2, v3.clone()]
        );
        assert_eq!(latest_version(&pool, user, program.id).await.unwrap(), v3);
        assert_eq!(get_version(&pool, user, v1.id).await.unwrap(), v1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn concurrent_uploads_get_consecutive_versions(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (_, program, _) = create(&pool, user, creation(), "P", &document("0"))
            .await
            .unwrap();
        let uploads = (1..=8).map(|n| {
            let pool = pool.clone();
            tokio::spawn(async move {
                add_version(&pool, user, program.id, &document(&n.to_string())).await
            })
        });
        for upload in uploads.collect::<Vec<_>>() {
            upload.await.unwrap().unwrap();
        }
        let numbers: Vec<u32> = list_versions(&pool, user, program.id)
            .await
            .unwrap()
            .iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(numbers, (1..=9).collect::<Vec<_>>());
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn another_users_programs_are_invisible_and_untouchable(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let (_, program, version) = create(&pool, a, creation(), "A's", &document("A"))
            .await
            .unwrap();
        let guessed = ProgramId::from_uuid(random_uuid());
        let guessed_version = ProgramVersionId::from_uuid(random_uuid());

        // Every call with A's real id fails exactly like one with an id that does not exist.
        for id in [program.id, guessed] {
            not_found(get(&pool, b, id).await);
            not_found(rename(&pool, b, id, "Mine now").await);
            not_found(set_archived(&pool, b, id, true).await);
            not_found(add_version(&pool, b, id, &document("B")).await);
            not_found(list_versions(&pool, b, id).await);
            not_found(latest_version(&pool, b, id).await);
        }
        for id in [version.id, guessed_version] {
            not_found(get_version(&pool, b, id).await);
        }
        assert!(list(&pool, b, true).await.unwrap().is_empty());

        // A's program is exactly as it was.
        assert_eq!(get(&pool, a, program.id).await.unwrap(), program);
        assert_eq!(
            list_versions(&pool, a, program.id).await.unwrap(),
            vec![version]
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn nobody_can_change_a_builtin_through_the_repository(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let user = testing::user(&pool).await;
        let builtin = list_builtins(&pool).await.unwrap().remove(0);
        let id = builtin.program.id;
        not_found(get(&pool, user, id).await);
        not_found(rename(&pool, user, id, "Mine").await);
        not_found(set_archived(&pool, user, id, true).await);
        not_found(add_version(&pool, user, id, &document("B")).await);
        not_found(list_versions(&pool, user, id).await);
        not_found(get_version(&pool, user, builtin.latest.id).await);
        assert_eq!(list_builtins(&pool).await.unwrap(), vec![builtin]);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_retried_create_returns_the_same_program(pool: PgPool) {
        let (a, b) = testing::users_a_and_b(&pool).await;
        let key = creation();
        let (change, program, first) = create(&pool, a, key, "P", &document("P")).await.unwrap();
        assert_eq!(change, Change::Applied);
        let (change, again, again_first) =
            create(&pool, a, key, "P", &document("P")).await.unwrap();
        assert_eq!(
            (change, &again, &again_first),
            (Change::Unchanged, &program, &first)
        );
        // Still a retry after a rename (the name is not part of the comparison).
        rename(&pool, a, program.id, "Renamed").await.unwrap();
        let (change, again, _) = create(&pool, a, key, "P", &document("P")).await.unwrap();
        assert_eq!((change, again.id), (Change::Unchanged, program.id));
        // The same key for other content is not a retry.
        let result = create(&pool, a, key, "P", &document("other")).await;
        assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
        assert_eq!(list(&pool, a, true).await.unwrap().len(), 1);
        // Keys are per user: B's request with A's key is B's own.
        let (change, program_b, _) = create(&pool, b, key, "B", &document("B")).await.unwrap();
        assert_eq!(change, Change::Applied);
        assert_ne!(program_b.id, program.id);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn concurrent_creates_with_one_key_make_one_program(pool: PgPool) {
        let user = testing::user(&pool).await;
        let key = creation();
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { create(&pool, user, key, "P", &document("P")).await })
            })
            .collect();
        let mut applied = 0;
        for task in tasks {
            if task.await.unwrap().unwrap().0 == Change::Applied {
                applied += 1;
            }
        }
        assert_eq!(applied, 1);
        assert_eq!(list(&pool, user, true).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_retried_copy_returns_the_same_program(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let user = testing::user(&pool).await;
        let key = creation();
        let (change, program, first) = copy_builtin(&pool, user, key, "starter").await.unwrap();
        assert_eq!(change, Change::Applied);
        // The built-in changes and is then archived: a retry still returns the copy.
        let changed = BuiltinSeed {
            json: r#"{"schema_version": 1, "name": "Starter v2"}"#,
            ..STARTER
        };
        seed_builtins(&pool, &[changed]).await.unwrap();
        seed_builtins(&pool, &[]).await.unwrap();
        let (change, again, again_first) = copy_builtin(&pool, user, key, "starter").await.unwrap();
        assert_eq!(
            (change, &again, &again_first),
            (Change::Unchanged, &program, &first)
        );
        // A key used for a copy cannot be replayed as a create, and the other way round.
        let result = create(&pool, user, key, "P", &first.document).await;
        assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let (_, created, _) = create(&pool, user, creation(), "P", &document("P"))
            .await
            .unwrap();
        assert!(created.source_builtin_id.is_none());
        assert_eq!(list(&pool, user, true).await.unwrap().len(), 2);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn a_create_key_cannot_be_replayed_as_a_copy(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let user = testing::user(&pool).await;
        let key = creation();
        create(&pool, user, key, "P", &document("P")).await.unwrap();
        let result = copy_builtin(&pool, user, key, "starter").await;
        assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
        assert_eq!(list(&pool, user, true).await.unwrap().len(), 1);
    }

    /// jsonb normalises numbers (`1e16` is stored as `10000000000000000`), so a retried request
    /// must be compared as jsonb, not as the `serde_json` value read back.
    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn retries_compare_documents_as_jsonb(pool: PgPool) {
        let user = testing::user(&pool).await;
        let big: JsonValue =
            serde_json::from_str(r#"{"schema_version": 1, "big": 1e16, "small": 1.50}"#).unwrap();
        let key = creation();
        let (change, program, first) = create(&pool, user, key, "P", &big).await.unwrap();
        assert_eq!(change, Change::Applied);
        // What comes back differs from what was sent, as serde_json values.
        assert_ne!(first.document, big);
        let (change, again, _) = create(&pool, user, key, "P", &big).await.unwrap();
        assert_eq!((change, again.id), (Change::Unchanged, program.id));

        let (change, v2) = add_version(&pool, user, program.id, &big).await.unwrap();
        assert_eq!((change, v2.version), (Change::Unchanged, 1));
        let other: JsonValue =
            serde_json::from_str(r#"{"schema_version": 1, "big": 2e16}"#).unwrap();
        let (change, v2) = add_version(&pool, user, program.id, &other).await.unwrap();
        assert_eq!((change, v2.version), (Change::Applied, 2));
        let (change, again) = add_version(&pool, user, program.id, &other).await.unwrap();
        assert_eq!((change, again.id), (Change::Unchanged, v2.id));
        let result = create(&pool, user, key, "P", &other).await;
        assert!(matches!(result, Err(RepoError::Conflict)), "{result:?}");
    }
}

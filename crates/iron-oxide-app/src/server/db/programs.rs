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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::db::{
        MIGRATOR,
        testing::{self, document, random_uuid},
    };

    const STARTER: BuiltinSeed = BuiltinSeed {
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

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn the_seed_hook_is_valid(pool: PgPool) {
        seed_builtins(&pool, BUILTIN_PROGRAMS).await.unwrap();
        assert_eq!(
            list_builtins(&pool).await.unwrap().len(),
            BUILTIN_PROGRAMS.len()
        );
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn copying_a_builtin_creates_an_owned_program(pool: PgPool) {
        seed_builtins(&pool, &[STARTER]).await.unwrap();
        let user = testing::user(&pool).await;
        let (program, version) = copy_builtin(&pool, user, "starter").await.unwrap();
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
        not_found(copy_builtin(&pool, user, "missing").await);
        // An archived built-in cannot be copied any more.
        seed_builtins(&pool, &[]).await.unwrap();
        not_found(copy_builtin(&pool, user, "starter").await);
    }

    #[sqlx::test(migrator = "MIGRATOR")]
    #[ignore = "needs Postgres"]
    async fn create_rename_archive_and_list(pool: PgPool) {
        let user = testing::user(&pool).await;
        let (first, version) = create(&pool, user, "First", &document("First"))
            .await
            .unwrap();
        assert_eq!(first.source_builtin_id, None);
        assert_eq!(version.version, 1);
        let (second, _) = create(&pool, user, "Second", &document("Second"))
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
            let result = create(&pool, user, name, &document).await;
            assert!(
                matches!(result, Err(RepoError::Invalid { .. })),
                "{result:?}"
            );
        }
        assert!(list(&pool, user, true).await.unwrap().is_empty());
        let (program, _) = create(&pool, user, &"x".repeat(100), &document("x"))
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
        let (program, v1) = create(&pool, user, "P", &document("one")).await.unwrap();
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
        let (program, _) = create(&pool, user, "P", &document("0")).await.unwrap();
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
        let (program, version) = create(&pool, a, "A's", &document("A")).await.unwrap();
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
}

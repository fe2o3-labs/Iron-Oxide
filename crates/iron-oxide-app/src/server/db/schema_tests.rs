//! Tests of the schema itself, with raw SQL that bypasses the repository: the database must keep
//! users apart and reject invalid values even if application code gets a query wrong.

use sqlx::{PgPool, types::Uuid};

use super::{
    MIGRATOR,
    ids::UserId,
    testing::{self, at, random_uuid},
};

/// Tables that may exist in `public` without a `user_id` column. Anything else that holds data
/// must be owned by a user (or be listed here with a reason).
const TABLES_WITHOUT_OWNER: &[&str] = &[
    // sqlx's migration bookkeeping.
    "_sqlx_migrations",
    // The accounts themselves.
    "users",
];

/// Every table with a `user_id` column, from the catalog.
async fn user_owned_tables(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar!(
        r#"SELECT c.table_name AS "table_name!" FROM information_schema.columns c
           JOIN information_schema.tables t USING (table_schema, table_name)
           WHERE c.table_schema = 'public' AND c.column_name = 'user_id'
             AND t.table_type = 'BASE TABLE'
           ORDER BY c.table_name"#
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn count_owned_by(pool: &PgPool, table: &str, user: UserId) -> i64 {
    // The table name comes from the catalog, not from input; quoted as an identifier anyway.
    let sql = format!(
        "SELECT count(*) FROM \"{}\" WHERE user_id = $1",
        table.replace('"', "\"\"")
    );
    sqlx::query_scalar(&sql)
        .bind(user.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn the_catalog_lists_every_training_table(pool: PgPool) {
    // Guards the catalog query itself: if it found nothing, the tests below would pass vacuously.
    let tables = user_owned_tables(&pool).await;
    for expected in [
        "active_program",
        "program_versions",
        "programs",
        "training_maxes",
        "user_settings",
        "workout_sessions",
        "workout_sets",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "{expected}: {tables:?}"
        );
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_public_table_is_user_owned_or_allowlisted(pool: PgPool) {
    let unowned = sqlx::query_scalar!(
        r#"SELECT t.table_name AS "table_name!" FROM information_schema.tables t
           WHERE t.table_schema = 'public' AND t.table_type = 'BASE TABLE'
             AND NOT EXISTS (
                 SELECT 1 FROM information_schema.columns c
                 WHERE c.table_schema = t.table_schema AND c.table_name = t.table_name
                   AND c.column_name = 'user_id')
           ORDER BY 1"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let unexpected: Vec<&String> = unowned
        .iter()
        .filter(|table| !TABLES_WITHOUT_OWNER.contains(&table.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "tables without a user_id column: {unexpected:?}; give them an owner or allowlist them"
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_user_id_cascades_from_users_and_is_indexed(pool: PgPool) {
    // For each `user_id` column in `public`: a single-column foreign key to `users` with
    // ON DELETE CASCADE, and an index whose first column is `user_id`.
    let offenders = sqlx::query!(
        r#"SELECT cls.relname AS "table!",
                  EXISTS (
                      SELECT 1 FROM pg_constraint con
                      WHERE con.conrelid = cls.oid AND con.contype = 'f'
                        AND con.conkey = ARRAY[att.attnum]
                        AND con.confrelid = 'users'::regclass AND con.confdeltype = 'c'
                  ) AS "cascades!",
                  EXISTS (
                      SELECT 1 FROM pg_index idx
                      WHERE idx.indrelid = cls.oid AND idx.indkey[0] = att.attnum
                  ) AS "indexed!"
           FROM pg_class cls
           JOIN pg_namespace ns ON ns.oid = cls.relnamespace
           JOIN pg_attribute att ON att.attrelid = cls.oid
           WHERE ns.nspname = 'public' AND cls.relkind = 'r'
             AND att.attname = 'user_id' AND NOT att.attisdropped
           ORDER BY 1"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(offenders.len() >= 7, "{}", offenders.len());
    let bad: Vec<String> = offenders
        .iter()
        .filter(|row| !row.cascades || !row.indexed)
        .map(|row| {
            format!(
                "{} (cascades: {}, indexed: {})",
                row.table, row.cascades, row.indexed
            )
        })
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

/// Unique keys of user-owned tables that may leave out `user_id`. Only keys over values the
/// server generates or controls belong here: a key over a client-chosen value without `user_id`
/// would tell one user that another already uses that value.
const UNIQUE_KEYS_WITHOUT_OWNER: &[&str] = &[
    // Program and version ids come from gen_random_uuid(), never from a client.
    "programs_pkey",
    "program_versions_pkey",
    // One row per built-in id, among built-ins only (user_id IS NULL).
    "programs_builtin_key",
    // Version numbers are assigned by the server, within one program (checked as the caller's).
    "program_versions_program_id_version_key",
];

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_unique_key_of_a_user_owned_table_includes_user_id(pool: PgPool) {
    let keys = sqlx::query!(
        r#"SELECT idx_cls.relname AS "index!", tbl.relname AS "table!",
                  EXISTS (
                      SELECT 1 FROM pg_attribute att
                      WHERE att.attrelid = tbl.oid AND att.attname = 'user_id'
                        AND att.attnum = ANY (idx.indkey::smallint[])
                  ) AS "has_user_id!"
           FROM pg_index idx
           JOIN pg_class idx_cls ON idx_cls.oid = idx.indexrelid
           JOIN pg_class tbl ON tbl.oid = idx.indrelid
           JOIN pg_namespace ns ON ns.oid = tbl.relnamespace
           WHERE ns.nspname = 'public' AND idx.indisunique
             AND EXISTS (
                 SELECT 1 FROM pg_attribute att
                 WHERE att.attrelid = tbl.oid AND att.attname = 'user_id' AND NOT att.attisdropped
             )
           ORDER BY 2, 1"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    // Guards the query: the client-id tables' primary keys are among the keys it sees.
    for expected in ["workout_sessions_pkey", "workout_sets_pkey"] {
        assert!(
            keys.iter()
                .any(|key| key.index == expected && key.has_user_id),
            "{expected}"
        );
    }
    let bad: Vec<String> = keys
        .iter()
        .filter(|key| !key.has_user_id && !UNIQUE_KEYS_WITHOUT_OWNER.contains(&key.index.as_str()))
        .map(|key| format!("{}.{}", key.table, key.index))
        .collect();
    assert!(
        bad.is_empty(),
        "unique keys without user_id: {bad:?}; add user_id or allowlist them with a reason"
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn deleting_a_user_removes_their_rows_in_every_table_and_keeps_others(pool: PgPool) {
    let (a, b) = testing::users_a_and_b(&pool).await;
    testing::populate(&pool, a).await;
    testing::populate(&pool, b).await;
    let tables = user_owned_tables(&pool).await;
    // `populate` must reach every user-owned table, so a new table fails here until it does.
    for table in &tables {
        assert!(
            count_owned_by(&pool, table, a).await > 0,
            "populate() writes nothing to {table}"
        );
    }

    sqlx::query!("DELETE FROM users WHERE id = $1", a.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    for table in &tables {
        assert_eq!(count_owned_by(&pool, table, a).await, 0, "{table}");
        assert!(count_owned_by(&pool, table, b).await > 0, "{table}");
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn no_row_can_change_owner(pool: PgPool) {
    let a = testing::user(&pool).await;
    testing::populate(&pool, a).await;
    // C has no rows at all, so moving A's rows to C breaks no key or foreign key in the
    // single-owner tables: only the owner trigger stands in the way.
    let c = testing::user(&pool).await;
    for table in user_owned_tables(&pool).await {
        let before = count_owned_by(&pool, &table, a).await;
        let sql = format!("UPDATE \"{table}\" SET user_id = $2 WHERE user_id = $1");
        let error = sqlx::query(&sql)
            .bind(a.as_uuid())
            .bind(c.as_uuid())
            .execute(&pool)
            .await
            .unwrap_err();
        let db_error = error.as_database_error().unwrap();
        // Postgres checks row triggers before keys: the error must be the trigger's.
        assert_eq!(
            db_error.code().as_deref(),
            Some("23000"),
            "{table}: {error}"
        );
        assert!(
            db_error.message().contains("cannot change")
                || db_error.message().contains("immutable"),
            "{table}: {error}"
        );
        assert_eq!(count_owned_by(&pool, &table, a).await, before, "{table}");
        assert_eq!(count_owned_by(&pool, &table, c).await, 0, "{table}");
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_user_owned_table_has_an_owner_trigger(pool: PgPool) {
    let guarded = sqlx::query_scalar!(
        r#"SELECT DISTINCT cls.relname AS "table!" FROM pg_trigger trg
           JOIN pg_class cls ON cls.oid = trg.tgrelid
           JOIN pg_namespace ns ON ns.oid = cls.relnamespace
           JOIN pg_proc proc ON proc.oid = trg.tgfoid
           WHERE ns.nspname = 'public' AND NOT trg.tgisinternal
             AND proc.proname IN ('forbid_owner_change', 'forbid_update')
             -- A row-level (bit 0) BEFORE (bit 1) UPDATE (bit 4) trigger.
             AND trg.tgtype & 1 = 1 AND trg.tgtype & 2 = 2 AND trg.tgtype & 16 = 16"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let missing: Vec<String> = user_owned_tables(&pool)
        .await
        .into_iter()
        .filter(|table| !guarded.contains(table))
        .collect();
    assert!(missing.is_empty(), "no owner trigger on {missing:?}");
}

/// Runs raw SQL and returns whether Postgres accepted it.
async fn accepts(pool: &PgPool, sql: &str, binds: &[Uuid]) -> bool {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query.execute(pool).await.is_ok()
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_set_can_only_point_at_its_owners_session(pool: PgPool) {
    let (a, b) = testing::users_a_and_b(&pool).await;
    let session_a = testing::session(&pool, a).await;
    let insert = "INSERT INTO workout_sets (id, session_id, user_id, exercise_id, set_index, reps, \
                  warmup, completed_at) VALUES ($1, $2, $3, 'squat', 0, 5, false, now())";
    // B's set in A's session.
    assert!(
        !accepts(
            &pool,
            insert,
            &[random_uuid(), session_a.as_uuid(), b.as_uuid()]
        )
        .await
    );
    // A's own set in A's session is fine.
    assert!(
        accepts(
            &pool,
            insert,
            &[random_uuid(), session_a.as_uuid(), a.as_uuid()]
        )
        .await
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_session_can_only_use_its_owners_program_versions(pool: PgPool) {
    let (a, b) = testing::users_a_and_b(&pool).await;
    let (_, version_a) = testing::program(&pool, a).await;
    super::programs::seed_builtins(
        &pool,
        &[super::programs::BuiltinSeed {
            builtin_id: "starter",
            name: "Starter",
            json: r#"{"schema_version": 1}"#,
        }],
    )
    .await
    .unwrap();
    let builtin_version: Uuid =
        sqlx::query_scalar("SELECT id FROM program_versions WHERE user_id IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    let insert = "INSERT INTO workout_sessions (id, user_id, program_version_id, day_id, status, \
                  started_at) VALUES ($1, $2, $3, 'a', 'in_progress', now())";
    for (user, version) in [(b, version_a.as_uuid()), (a, builtin_version)] {
        assert!(
            !accepts(&pool, insert, &[random_uuid(), user.as_uuid(), version]).await,
            "{user:?} {version}"
        );
    }
    assert!(
        accepts(
            &pool,
            insert,
            &[random_uuid(), a.as_uuid(), version_a.as_uuid()]
        )
        .await
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn only_an_owned_program_can_be_active(pool: PgPool) {
    let (a, b) = testing::users_a_and_b(&pool).await;
    let (program_a, _) = testing::program(&pool, a).await;
    let builtin: Uuid = sqlx::query_scalar(
        "INSERT INTO programs (source_builtin_id, name) VALUES ('starter', 'S') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let insert = "INSERT INTO active_program (user_id, program_id) VALUES ($1, $2)";
    assert!(!accepts(&pool, insert, &[b.as_uuid(), program_a.as_uuid()]).await);
    assert!(!accepts(&pool, insert, &[a.as_uuid(), builtin]).await);
    assert!(accepts(&pool, insert, &[a.as_uuid(), program_a.as_uuid()]).await);
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_version_takes_its_programs_owner_and_is_immutable(pool: PgPool) {
    let (a, b) = testing::users_a_and_b(&pool).await;
    let (program_a, version_a) = testing::program(&pool, a).await;
    // The caller's user_id is ignored: the trigger copies the program's owner.
    let owner: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO program_versions (program_id, user_id, version, document) \
         VALUES ($1, $2, 2, '{\"schema_version\": 1}') RETURNING user_id",
    )
    .bind(program_a.as_uuid())
    .bind(b.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner, Some(a.as_uuid()));

    for update in [
        "UPDATE program_versions SET document = '{\"schema_version\": 2}' WHERE id = $1",
        "UPDATE program_versions SET version = 9 WHERE id = $1",
        "UPDATE program_versions SET created_at = now() WHERE id = $1",
    ] {
        assert!(
            !accepts(&pool, update, &[version_a.as_uuid()]).await,
            "{update}"
        );
    }
    // A version number is unique per program.
    assert!(
        !accepts(
            &pool,
            "INSERT INTO program_versions (program_id, version, document) \
             VALUES ($1, 1, '{\"schema_version\": 1}')",
            &[program_a.as_uuid()]
        )
        .await
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn only_built_ins_have_no_owner(pool: PgPool) {
    assert!(
        !accepts(
            &pool,
            "INSERT INTO programs (name) VALUES ('Nobody''s')",
            &[]
        )
        .await
    );
    let builtin = "INSERT INTO programs (source_builtin_id, name) VALUES ('starter', 'S')";
    assert!(accepts(&pool, builtin, &[]).await);
    // One row per built-in id.
    assert!(!accepts(&pool, builtin, &[]).await);
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_session_is_finished_exactly_when_it_has_ended(pool: PgPool) {
    let user = testing::user(&pool).await;
    let (_, version) = testing::program(&pool, user).await;
    let insert = |status: &'static str, finished: Option<i64>| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "INSERT INTO workout_sessions (id, user_id, program_version_id, day_id, status, \
                 started_at, finished_at) VALUES ($1, $2, $3, 'a', $4, $5, $6)",
            )
            .bind(random_uuid())
            .bind(user.as_uuid())
            .bind(version.as_uuid())
            .bind(status)
            .bind(at(0))
            .bind(finished.map(at))
            .execute(&pool)
            .await
            .is_ok()
        }
    };
    assert!(insert("in_progress", None).await);
    assert!(!insert("in_progress", Some(10)).await);
    for status in ["completed", "skipped", "abandoned"] {
        assert!(insert(status, Some(10)).await, "{status}");
        assert!(insert(status, Some(0)).await, "{status} at the start");
        assert!(!insert(status, None).await, "{status} without an end");
        assert!(!insert(status, Some(-1)).await, "{status} before the start");
    }
    assert!(!insert("paused", None).await);
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn set_values_are_range_checked(pool: PgPool) {
    let user = testing::user(&pool).await;
    let session = testing::session(&pool, user).await;
    // weight_ng and duration_s are bigint and nullable: insert a set with each value.
    let insert = |column: &'static str, value: i64| {
        let pool = pool.clone();
        async move {
            let sql = format!(
                "INSERT INTO workout_sets (id, session_id, user_id, exercise_id, set_index, reps, \
                 warmup, completed_at, {column}) \
                 VALUES ($1, $2, $3, 'squat', 0, 0, false, now(), $4)"
            );
            sqlx::query(&sql)
                .bind(random_uuid())
                .bind(session.as_uuid())
                .bind(user.as_uuid())
                .bind(value)
                .execute(&pool)
                .await
                .is_ok()
        }
    };
    for (column, max) in [
        ("weight_ng", 2_000_000_000_000_000_i64),
        ("duration_s", 4_294_967_295),
    ] {
        assert!(insert(column, 0).await, "{column} 0");
        assert!(insert(column, max).await, "{column} max");
        assert!(!insert(column, max + 1).await, "{column} max + 1");
        assert!(!insert(column, -1).await, "{column} -1");
    }
    // reps and set_index are integer (u16 range): update an existing set.
    for column in ["reps", "set_index"] {
        let sql = format!(
            "UPDATE workout_sets SET {column} = $1 WHERE id = (SELECT id FROM workout_sets LIMIT 1)"
        );
        for (value, ok) in [(0, true), (65_535, true), (65_536, false), (-1, false)] {
            let result = sqlx::query(&sql).bind(value).execute(&pool).await;
            assert_eq!(result.is_ok(), ok, "{column} = {value}: {result:?}");
        }
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn slugs_are_checked(pool: PgPool) {
    let user = testing::user(&pool).await;
    let insert = "INSERT INTO training_maxes (user_id, exercise_id, weight_ng, set_at) \
                  VALUES ($1, $2, 1, now())";
    let long_ok = "a".repeat(64);
    let too_long = "a".repeat(65);
    for (slug, ok) in [
        ("squat", true),
        ("back-squat-2", true),
        (long_ok.as_str(), true),
        (too_long.as_str(), false),
        ("", false),
        ("Back", false),
        ("-a", false),
        ("a-", false),
        ("a--b", false),
        ("a b", false),
        ("é", false),
    ] {
        let result = sqlx::query(insert)
            .bind(user.as_uuid())
            .bind(slug)
            .execute(&pool)
            .await;
        assert_eq!(result.is_ok(), ok, "{slug:?}: {result:?}");
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn programs_and_versions_are_only_deleted_with_their_user(pool: PgPool) {
    let user = testing::user(&pool).await;
    let (program, version) = testing::program(&pool, user).await;
    super::programs::seed_builtins(
        &pool,
        &[super::programs::BuiltinSeed {
            builtin_id: "starter",
            name: "Starter",
            json: r#"{"schema_version": 1}"#,
        }],
    )
    .await
    .unwrap();
    for (sql, id) in [
        (
            "DELETE FROM program_versions WHERE id = $1",
            version.as_uuid(),
        ),
        ("DELETE FROM programs WHERE id = $1", program.as_uuid()),
    ] {
        let error = sqlx::query(sql).bind(id).execute(&pool).await.unwrap_err();
        let db_error = error.as_database_error().unwrap();
        assert_eq!(db_error.code().as_deref(), Some("23000"), "{sql}: {error}");
        assert!(
            db_error.message().contains("only deleted by cascade"),
            "{error}"
        );
    }
    // Built-ins neither, even in bulk.
    for sql in [
        "DELETE FROM program_versions WHERE user_id IS NULL",
        "DELETE FROM programs WHERE user_id IS NULL",
    ] {
        assert!(!accepts(&pool, sql, &[]).await, "{sql}");
    }
    // Deleting the user still cascades to both.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM programs WHERE user_id = $1) \
              + (SELECT count(*) FROM program_versions WHERE user_id = $1)",
    )
    .bind(user.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert_eq!(
        super::programs::list_builtins(&pool).await.unwrap().len(),
        1
    );
}

/// Foreign keys between user-owned tables that may leave `user_id` out, with the reason.
const FOREIGN_KEYS_WITHOUT_OWNER: &[&str] = &[
    // Paired with program_versions_program_owner_fkey, which includes user_id; this one alone
    // covers built-ins, whose NULL user_id the composite key does not check.
    "program_versions_program_id_fkey",
];

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_foreign_key_between_user_owned_tables_includes_user_id(pool: PgPool) {
    // For each foreign key whose source and target both have a user_id column: user_id must be
    // among the source columns and map to the target's user_id, so the two rows share an owner.
    let keys = sqlx::query!(
        r#"SELECT con.conname AS "name!", src.relname AS "table!",
                  EXISTS (
                      SELECT 1 FROM unnest(con.conkey, con.confkey) AS k(src_col, dst_col)
                      JOIN pg_attribute sa ON sa.attrelid = con.conrelid AND sa.attnum = k.src_col
                      JOIN pg_attribute da ON da.attrelid = con.confrelid AND da.attnum = k.dst_col
                      WHERE sa.attname = 'user_id' AND da.attname = 'user_id'
                  ) AS "owned!"
           FROM pg_constraint con
           JOIN pg_class src ON src.oid = con.conrelid
           WHERE con.contype = 'f'
             AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = con.conrelid
                         AND a.attname = 'user_id' AND NOT a.attisdropped)
             AND EXISTS (SELECT 1 FROM pg_attribute a WHERE a.attrelid = con.confrelid
                         AND a.attname = 'user_id' AND NOT a.attisdropped)
           ORDER BY 2, 1"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    // Guards the query: the composite keys are among the ones it sees.
    for expected in [
        "workout_sets_session_owner_fkey",
        "workout_sessions_program_version_owner_fkey",
        "active_program_program_owner_fkey",
    ] {
        assert!(
            keys.iter().any(|key| key.name == expected && key.owned),
            "{expected}"
        );
    }
    let bad: Vec<String> = keys
        .iter()
        .filter(|key| !key.owned && !FOREIGN_KEYS_WITHOUT_OWNER.contains(&key.name.as_str()))
        .map(|key| format!("{}.{}", key.table, key.name))
        .collect();
    assert!(
        bad.is_empty(),
        "foreign keys between user-owned tables without user_id: {bad:?}"
    );
}

#[sqlx::test(migrator = "MIGRATOR")]
#[ignore = "needs Postgres"]
async fn no_tables_outside_public(pool: PgPool) {
    // The catalog checks above look at `public`: a table elsewhere would escape them. Extend them
    // before allowing another schema.
    let elsewhere = sqlx::query_scalar!(
        r#"SELECT (table_schema || '.' || table_name) AS "table!" FROM information_schema.tables
           WHERE table_schema NOT IN ('public', 'pg_catalog', 'information_schema')
             AND table_schema NOT LIKE 'pg\_%'
           ORDER BY 1"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(elsewhere.is_empty(), "{elsewhere:?}");
}

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
    let (a, b) = testing::users_a_and_b(&pool).await;
    testing::populate(&pool, a).await;
    testing::populate(&pool, b).await;
    for table in user_owned_tables(&pool).await {
        let before = count_owned_by(&pool, &table, a).await;
        let sql = format!("UPDATE \"{table}\" SET user_id = $2 WHERE user_id = $1");
        let result = sqlx::query(&sql)
            .bind(a.as_uuid())
            .bind(b.as_uuid())
            .execute(&pool)
            .await;
        assert!(result.is_err(), "{table}: {result:?}");
        assert_eq!(count_owned_by(&pool, &table, a).await, before, "{table}");
    }
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
    let insert = "INSERT INTO training_maxes (user_id, exercise_id, weight_ng) VALUES ($1, $2, 1)";
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

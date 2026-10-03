//! Export, import and account deletion (#22), through the real router.

use dioxus::server::axum::{
    body::Body,
    http::{StatusCode, header},
};
use iron_oxide_domain::{
    CreationId, ExerciseId, LoggedSet, ProgramId, Reps, SessionId, SetId, Weight,
    entitlements::FREE_CUSTOM_PROGRAMS, time::Timestamp,
};
use serde_json::{Value, json};
use sqlx::PgPool;

use super::*;
use crate::api::{account::DELETE_REAUTH_WINDOW_SECS, programs::UploadOutcome};
use crate::server::{
    api::{
        error::UNAUTHORIZED,
        testing::{self, CallError, TestApi, TestUser},
    },
    db::{ids::UserId, testing as db_testing},
};

const EXPORT: &str = "/api/account/export";
const IMPORT: &str = "/api/account/import";
const DELETE: &str = "/api/account/delete";
const ME: &str = "/api/auth/me";

/// A point in time, `minutes` after a fixed start.
fn t(minutes: i64) -> Timestamp {
    Timestamp::from_epoch_millis(1_790_000_000_000 + minutes * 60_000)
}

/// A valid program: day `a`, a back squat of `reps` reps and a plank; rotation `a`.
fn program_json(name: &str, reps: u16) -> Value {
    json!({
        "schema_version": 1,
        "name": name,
        "days": [{
            "id": "a",
            "name": "Day A",
            "exercises": [
                {
                    "id": "back-squat",
                    "name": "Back squat",
                    "work": { "reps": { "sets": 3, "reps": reps } },
                    "load": { "kg": 100 },
                    "rest": 180
                },
                {
                    "id": "plank",
                    "name": "Plank",
                    "work": { "hold": { "sets": 1, "seconds": 30 } },
                    "rest": 60
                }
            ]
        }],
        "rotation": ["a"]
    })
}

fn set(index: u16, at: Timestamp) -> LoggedSet<Timestamp> {
    LoggedSet {
        id: SetId::new_v7(),
        exercise: ExerciseId::new("back-squat").unwrap(),
        set_index: index,
        reps: Reps::new(5),
        weight: Some(Weight::from_kg(102.5).unwrap()),
        duration: None,
        warm_up: false,
        completed_at: at,
    }
}

async fn upload(user: &mut TestUser, target: Value, document: &Value) -> UploadOutcome {
    user.call(
        "/api/programs/upload",
        json!({ "target": target, "document": document.to_string() }),
    )
    .await
    .unwrap()
}

/// Gives `user` some of everything, through the API: settings, a training max, an active program
/// with two versions, an archived program, a completed session and a session in progress, with
/// sets.
async fn seed(user: &mut TestUser) {
    let call = async |user: &mut TestUser, path: &str, body: Value| {
        user.call::<Value>(path, body)
            .await
            .unwrap_or_else(|e| panic!("{path}: {e:?}"))
    };
    call(
        user,
        "/api/settings/update",
        json!({ "settings": {
            "unit": "lb", "bar_weight": 15, "default_rest": 90, "sound_enabled": false,
            "plate_inventory": [{ "plate": 20, "pairs": 2 }, { "plate": 1.25, "pairs": 1 }]
        } }),
    )
    .await;
    call(
        user,
        "/api/settings/training-max/set",
        json!({ "exercise_id": "bench-press", "weight": 82.5 }),
    )
    .await;
    let main = upload(
        user,
        json!({ "kind": "new_program", "creation_id": CreationId::new_v7() }),
        &program_json("Main", 5),
    )
    .await;
    let program_id = main.program.id;
    upload(
        user,
        json!({ "kind": "new_version", "program_id": program_id }),
        &program_json("Main", 6),
    )
    .await;
    let old = upload(
        user,
        json!({ "kind": "new_program", "creation_id": CreationId::new_v7() }),
        &program_json("Old", 8),
    )
    .await;
    call(
        user,
        "/api/programs/archive",
        json!({ "program_id": old.program.id, "archived": true }),
    )
    .await;
    call(
        user,
        "/api/programs/active/set",
        json!({ "program_id": program_id }),
    )
    .await;
    let done = SessionId::new_v7();
    call(
        user,
        "/api/sessions/start",
        json!({ "session_id": done, "started_at": t(0) }),
    )
    .await;
    for index in 0..3 {
        call(
            user,
            "/api/sessions/save-set",
            json!({ "session_id": done, "set": set(index, t(1 + i64::from(index))) }),
        )
        .await;
    }
    call(
        user,
        "/api/sessions/finish",
        json!({ "session_id": done, "outcome": "completed", "finished_at": t(10) }),
    )
    .await;
    let current = SessionId::new_v7();
    call(
        user,
        "/api/sessions/start",
        json!({ "session_id": current, "started_at": t(60) }),
    )
    .await;
    call(
        user,
        "/api/sessions/save-set",
        json!({ "session_id": current, "set": set(0, t(61)) }),
    )
    .await;
}

async fn export(user: &mut TestUser) -> ExportDocument {
    user.call(EXPORT, json!({})).await.unwrap()
}

async fn import(
    user: &mut TestUser,
    document: &ExportDocument,
) -> Result<ImportSummary, CallError> {
    user.call(
        IMPORT,
        json!({ "document": serde_json::to_string(document).unwrap() }),
    )
    .await
}

/// The training data of an export: what an import restores (not the account or its sign-in
/// methods, which belong to the account importing it).
fn training(document: &ExportDocument) -> Value {
    json!({
        "settings": document.settings,
        "training_maxes": document.training_maxes,
        "programs": document.programs,
        "active_program": document.active_program,
        "sessions": document.sessions,
    })
}

/// Every table with a `user_id` column, from the catalog: a table added later is covered too.
async fn user_tables(db: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.columns
         WHERE table_schema = 'public' AND column_name = 'user_id' ORDER BY table_name",
    )
    .fetch_all(db)
    .await
    .unwrap()
}

/// How many rows `user` has in each user-owned table, and in `users`.
async fn row_counts(db: &PgPool, user: UserId) -> Vec<(String, i64)> {
    let mut counts = Vec::new();
    for table in user_tables(db).await {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE user_id = $1"))
                .bind(user.as_uuid())
                .fetch_one(db)
                .await
                .unwrap();
        counts.push((table, count));
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE id = $1")
        .bind(user.as_uuid())
        .fetch_one(db)
        .await
        .unwrap();
    counts.push(("users".to_owned(), users));
    counts
}

/// Moves the sign-in of every session of `user` `seconds` into the past.
async fn age_sign_in(db: &PgPool, user: UserId, seconds: i64) {
    sqlx::query(
        "UPDATE sessions
         SET data = jsonb_set(data, '{auth.signed_in_at}',
                              to_jsonb(floor(extract(epoch FROM now()))::bigint - $2))
         WHERE user_id = $1",
    )
    .bind(user.as_uuid())
    .bind(seconds)
    .execute(db)
    .await
    .unwrap();
}

// --- Unit tests --------------------------------------------------------------------------------

#[test]
fn files_that_are_not_a_current_export_are_refused_before_parsing() {
    let message = |text: &str| Import::parse(text).unwrap_err().public().1.to_owned();
    assert!(message("not json").starts_with("This file is not an Iron Oxide export:"));
    assert_eq!(
        message(r#"{"format": "something-else", "format_version": 1}"#),
        "This file is not an Iron Oxide export."
    );
    assert_eq!(
        message(r#"{"format": "iron-oxide-export", "format_version": 2}"#),
        "This export has format version 2, which this version of Iron Oxide cannot read (it \
         reads version 1)."
    );
    assert_eq!(
        message(r#"{"format": "iron-oxide-export", "format_version": "1"}"#),
        "This export has no valid format version."
    );
    let error = message(r#"{"format": "iron-oxide-export", "format_version": 1}"#);
    assert!(
        error.starts_with("This export is not valid: missing field"),
        "{error}"
    );
}

#[test]
fn parser_messages_are_shortened() {
    let long = "x".repeat(10_000);
    let error = invalid_with("Prefix", &long);
    let message = error.public().1;
    assert!(
        message.chars().count() < MAX_MESSAGE_CHARS + 20,
        "{message}"
    );
    assert!(message.ends_with("…."));
}

#[test]
fn program_problems_get_paths_from_the_export_root() {
    let problems = ProgramProblems {
        errors: ["", "name", "[0]"]
            .into_iter()
            .map(|path| ProgramProblem {
                path: path.to_owned(),
                message: "m".to_owned(),
                line: Some(1),
                column: Some(2),
            })
            .collect(),
        omitted: 3,
    };
    let ApiError::InvalidProgramIn(message, problems) =
        invalid_program("programs[0].versions[1].document", "P", 2, problems)
    else {
        panic!("not an invalid program");
    };
    let paths: Vec<&str> = problems.errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "programs[0].versions[1].document",
            "programs[0].versions[1].document.name",
            "programs[0].versions[1].document[0]",
        ]
    );
    assert!(
        problems
            .errors
            .iter()
            .all(|e| e.line.is_none() && e.column.is_none())
    );
    assert_eq!(problems.omitted, 3);
    assert_eq!(
        message,
        "Program \"P\", version 2 (programs[0].versions[1].document), is not valid under this \
         version of Iron Oxide's rules: the document: m."
    );
}

// --- Export ------------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn the_export_has_everything_the_user_owns_and_no_secret(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut a = api.user("A").await;
    seed(&mut a).await;
    let (_, bytes) = a.call_raw(EXPORT, json!({})).await;
    let text = String::from_utf8(bytes).unwrap();
    let document: ExportDocument = serde_json::from_str(&text).unwrap();

    assert_eq!(document.format, EXPORT_FORMAT);
    assert_eq!(document.format_version, EXPORT_FORMAT_VERSION);
    assert_eq!(document.account.user_id.as_uuid(), a.id.as_uuid());
    assert_eq!(document.account.display_name.as_deref(), Some("A"));
    assert_eq!(document.account.plan, Plan::Free);
    assert_eq!(document.sign_in.passkeys.len(), 1);
    assert!(document.sign_in.linked_accounts.is_empty());
    let settings = document.settings.as_ref().unwrap();
    assert_eq!(settings.settings.unit, iron_oxide_domain::Unit::Lb);
    assert_eq!(document.training_maxes.len(), 1);
    assert_eq!(document.programs.len(), 2);
    let main = &document.programs[0];
    assert_eq!(
        (main.name.as_str(), main.versions.len(), main.archived),
        ("Main", 2, false)
    );
    assert_eq!(main.versions[1].document, program_json("Main", 6));
    assert!(document.programs[1].archived);
    assert_eq!(document.active_program, Some(main.creation_id));
    assert_eq!(document.sessions.len(), 2);
    assert_eq!(document.sessions[0].status, SessionStatus::Completed);
    assert_eq!(document.sessions[0].version, 2);
    assert_eq!(document.sessions[0].sets.len(), 3);
    let order: Vec<u16> = document.sessions[0]
        .sets
        .iter()
        .map(|s| s.set_index)
        .collect();
    assert_eq!(order, [0, 1, 2], "sets in the order they were completed");
    assert_eq!(document.sessions[1].status, SessionStatus::InProgress);
    assert_eq!(document.sessions[1].sets.len(), 1);

    // No key material, credential id, user handle or session id.
    let passkey: (Value, Vec<u8>) =
        sqlx::query_as("SELECT passkey, credential_id FROM passkeys WHERE user_id = $1")
            .bind(a.id.as_uuid())
            .fetch_one(&db)
            .await
            .unwrap();
    let handle: Uuid =
        sqlx::query_scalar("SELECT user_handle FROM webauthn_user_handles WHERE user_id = $1")
            .bind(a.id.as_uuid())
            .fetch_one(&db)
            .await
            .unwrap();
    let key = public_key(&passkey.0)
        .unwrap_or_else(|| panic!("no public key in {}", passkey.0))
        .to_string();
    for secret in [
        key,
        hex(&passkey.1),
        base64url(&passkey.1),
        handle.to_string(),
        "id_hash".to_owned(),
    ] {
        assert!(!text.contains(&secret), "the export contains {secret}");
    }
}

/// The public key's `x` coordinate in a stored `webauthn_rs` passkey.
fn public_key(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(map) => map.get("x").or_else(|| map.values().find_map(public_key)),
        Value::Array(items) => items.iter().find_map(public_key),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn base64url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_data_never_appears_in_an_export(db: PgPool) {
    let api = TestApi::new(db).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    seed(&mut a).await;
    seed(&mut b).await;
    let mine = export(&mut a).await;
    let theirs = export(&mut b).await;
    let text = serde_json::to_string(&mine).unwrap();
    let mut their_ids = vec![theirs.account.user_id.to_string()];
    their_ids.extend(theirs.programs.iter().map(|p| p.creation_id.to_string()));
    for session in &theirs.sessions {
        their_ids.push(session.id.to_string());
        their_ids.extend(session.sets.iter().map(|s| s.id.to_string()));
    }
    assert_eq!(their_ids.len(), 1 + 2 + 2 + 4);
    for id in their_ids {
        assert!(!text.contains(&id), "A's export contains B's {id}");
    }
    assert_eq!(mine.programs.len(), 2);
    assert_eq!(mine.sessions.len(), 2);
}

/// Every table with user data, and where the export puts it. A new table fails
/// [`every_user_table_is_exported_or_deliberately_left_out`] until it is listed here, with the
/// export, the import and `docs/export-format.md` updated.
const USER_TABLES: &[(&str, &str)] = &[
    ("active_program", "active_program"),
    (
        "auth_ceremonies",
        "not exported: in-flight sign-in state, minutes long",
    ),
    (
        "oauth_identities",
        "sign_in.linked_accounts: provider and dates, not the subject",
    ),
    (
        "passkeys",
        "sign_in.passkeys: metadata, no key material and no credential id",
    ),
    ("program_versions", "programs[].versions"),
    ("programs", "programs"),
    ("sessions", "not exported: sign-in sessions (secrets)"),
    ("training_maxes", "training_maxes"),
    ("user_settings", "settings"),
    (
        "webauthn_user_handles",
        "not exported: the random handle given to authenticators",
    ),
    ("workout_sessions", "sessions"),
    ("workout_sets", "sessions[].sets"),
];

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_user_table_is_exported_or_deliberately_left_out(db: PgPool) {
    let listed: Vec<&str> = USER_TABLES.iter().map(|(table, _)| *table).collect();
    assert_eq!(user_tables(&db).await, listed);
}

// --- Import ------------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn export_delete_import_restores_the_same_data(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut a = api.user("A").await;
    seed(&mut a).await;
    let before = export(&mut a).await;
    a.call::<()>(DELETE, json!({})).await.unwrap();

    let mut again = api.user("A again").await;
    let summary = import(&mut again, &before).await.unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            settings: true,
            training_maxes: 1,
            programs: 2,
            versions: 3,
            active_program: true,
            sessions: 2,
            sets: 4,
        }
    );
    let after = export(&mut again).await;
    assert_eq!(training(&after), training(&before));
    // The restored data works: the session in progress can be finished.
    let current = before.sessions[1].id;
    again
        .call::<Value>(
            "/api/sessions/finish",
            json!({ "session_id": current, "outcome": "completed", "finished_at": t(70) }),
        )
        .await
        .unwrap();
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn importing_the_same_export_twice_changes_nothing_the_second_time(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut c) = (api.user("A").await, api.user("C").await);
    seed(&mut a).await;
    let document = export(&mut a).await;

    // Into the account it came from: everything is already there.
    assert_eq!(
        import(&mut a, &document).await.unwrap(),
        ImportSummary::default()
    );
    assert_eq!(training(&export(&mut a).await), training(&document));

    // Into another account: once, then nothing.
    assert_ne!(
        import(&mut c, &document).await.unwrap(),
        ImportSummary::default()
    );
    let once = export(&mut c).await;
    let counts = row_counts(&db, c.id).await;
    assert_eq!(
        import(&mut c, &document).await.unwrap(),
        ImportSummary::default()
    );
    assert_eq!(training(&export(&mut c).await), training(&once));
    assert_eq!(row_counts(&db, c.id).await, counts, "no duplicate rows");
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn what_the_account_already_has_wins(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut a = api.user("A").await;
    seed(&mut a).await;
    let mut document = export(&mut a).await;
    let original = document.clone();

    // Every row of the export changed, plus one new session with a new set.
    let settings = document.settings.as_mut().unwrap();
    settings.settings.sound_enabled = true;
    document.training_maxes[0].weight = Weight::from_kg(100.0).unwrap();
    document.programs[0].name = "Renamed".to_owned();
    document.programs[0].versions[0].document = program_json("Changed", 3);
    document.sessions[0].sets[0].reps = Reps::new(1);
    let mut extra = document.sessions[0].clone();
    extra.id = SessionId::new_v7();
    extra.sets = vec![set(0, t(200))];
    extra.started_at = t(199);
    extra.finished_at = Some(t(201));
    document.sessions.push(extra.clone());
    // A second session in progress: the account's own one wins.
    let mut second_current = document.sessions[1].clone();
    second_current.id = SessionId::new_v7();
    second_current.sets = vec![set(0, t(300))];
    document.sessions[1].status = SessionStatus::Abandoned;
    document.sessions[1].finished_at = Some(t(65));
    document.sessions.push(second_current);

    let summary = import(&mut a, &document).await.unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            programs: 1,
            versions: 1,
            sessions: 1,
            sets: 1,
            ..ImportSummary::default()
        }
    );
    let after = export(&mut a).await;
    let mut expected = original.sessions.clone();
    expected.insert(2, extra);
    assert_eq!(after.sessions.len(), 3);
    assert_eq!(after.sessions, expected);
    // The program keeps its name and versions; the export's other version 1 goes to the
    // program's archived companion (versions are matched by content, never by number alone).
    assert_eq!(after.programs[..2], original.programs[..]);
    let companion = &after.programs[2];
    assert_eq!(
        companion.name, "Renamed (imported)",
        "named after the export's program"
    );
    assert!(companion.archived);
    assert_eq!(companion.versions.len(), 1);
    assert_eq!(companion.versions[0].version, 1);
    assert_eq!(companion.versions[0].document, program_json("Changed", 3));
    assert_eq!(after.settings, original.settings);
    assert_eq!(after.training_maxes, original.training_maxes);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn an_import_over_the_plan_limit_is_refused_before_writing(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut full, mut c) = (api.user("Full").await, api.user("C").await);
    for i in 0..FREE_CUSTOM_PROGRAMS {
        let name = format!("Program {i}");
        crate::server::db::programs::create(
            &db,
            full.id,
            db_testing::creation(),
            &name,
            &program_json(&name, 5),
            db_testing::unlimited,
        )
        .await
        .unwrap();
    }
    let document = export(&mut full).await;
    assert_eq!(document.programs.len() as u32, FREE_CUSTOM_PROGRAMS);
    // At the limit, importing its own export again adds nothing, so it is not refused.
    assert_eq!(
        import(&mut full, &document).await.unwrap(),
        ImportSummary::default()
    );

    // C already has one program: ten more would make eleven.
    let own = upload(
        &mut c,
        json!({ "kind": "new_program", "creation_id": CreationId::new_v7() }),
        &program_json("Own", 5),
    )
    .await;
    let counts = row_counts(&db, c.id).await;
    let error = import(&mut c, &document).await.unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(
        error.message,
        format!(
            "Your plan keeps up to {FREE_CUSTOM_PROGRAMS} programs. Archive one, or upgrade to Pro."
        )
    );
    assert_eq!(row_counts(&db, c.id).await, counts, "nothing written");

    // Archived, the same programs take no slot.
    let mut archived = document.clone();
    for program in &mut archived.programs {
        program.archived = true;
    }
    let summary = import(&mut c, &archived).await.unwrap();
    assert_eq!(summary.programs, FREE_CUSTOM_PROGRAMS);
    let listed: Vec<Value> = c
        .call("/api/programs/list", json!({ "include_archived": false }))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], json!(own.program.id));
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_export_imported_never_touches_the_owner(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    seed(&mut a).await;
    let document = export(&mut a).await;
    let a_rows = row_counts(&db, a.id).await;

    // B imports A's file: B gets copies of the data (same session and set ids, which are per
    // user; new program ids), A's rows are neither read nor changed.
    let summary = import(&mut b, &document).await.unwrap();
    assert_eq!(
        (summary.programs, summary.sessions, summary.sets),
        (2, 2, 4)
    );
    assert_eq!(training(&export(&mut b).await), training(&document));
    assert_eq!(training(&export(&mut a).await), training(&document));
    assert_eq!(row_counts(&db, a.id).await, a_rows);
    let a_programs: Vec<ProgramId> =
        sqlx::query_scalar("SELECT id FROM programs WHERE user_id = $1")
            .bind(a.id.as_uuid())
            .fetch_all(&db)
            .await
            .unwrap()
            .into_iter()
            .map(ProgramId::from_uuid)
            .collect();
    for id in a_programs {
        testing::assert_not_found_for_other_user(
            &mut b,
            "/api/programs/get",
            id.as_uuid(),
            |id| json!({ "program_id": id }),
        )
        .await;
    }

    // B deleting their account leaves A's data alone, shared session ids included.
    b.call::<()>(DELETE, json!({})).await.unwrap();
    assert_eq!(row_counts(&db, a.id).await, a_rows);
    assert_eq!(training(&export(&mut a).await), training(&document));
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn invalid_imports_are_422_and_write_nothing(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut c, mut d) = (
        api.user("A").await,
        api.user("C").await,
        api.user("D").await,
    );
    seed(&mut a).await;
    let valid = export(&mut a).await;
    let ids = (c.id, d.id);
    let counts = (row_counts(&db, ids.0).await, row_counts(&db, ids.1).await);
    let refused = async |c: &mut TestUser, document: Value| {
        let error = c
            .call_err(IMPORT, json!({ "document": document.to_string() }))
            .await;
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY, "{error:?}");
        error.message
    };

    let mut version_2 = serde_json::to_value(&valid).unwrap();
    version_2["format_version"] = json!(2);
    assert!(
        refused(&mut c, version_2)
            .await
            .contains("format version 2")
    );
    assert_eq!(
        refused(&mut c, json!({ "hello": "world" })).await,
        "This file is not an Iron Oxide export."
    );
    let raw = c
        .call_err(IMPORT, json!({ "document": "{ not json" }))
        .await;
    assert_eq!(raw.status, StatusCode::UNPROCESSABLE_ENTITY);

    let mutate = |change: &dyn Fn(&mut ExportDocument)| {
        let mut document = valid.clone();
        change(&mut document);
        serde_json::to_value(&document).unwrap()
    };
    let bad_program =
        mutate(&|d| d.programs[0].versions[0].document = json!({ "schema_version": 1 }));
    let (status, body) = c
        .send(
            c.post(IMPORT)
                .body(Body::from(
                    json!({ "document": bad_program.to_string() }).to_string(),
                ))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let path = &body["data"]["ServerError"]["details"]["errors"][0]["path"];
    assert!(
        path.as_str()
            .unwrap()
            .starts_with("programs[0].versions[0].document"),
        "{body}"
    );
    // Another user, for the per-user rate limit of imports.
    let c = &mut d;
    let unknown_version = mutate(&|d| d.sessions[0].version = 9);
    assert!(
        refused(c, unknown_version)
            .await
            .contains("sessions[0].version")
    );
    let unknown_day = mutate(&|d| d.sessions[0].day = iron_oxide_domain::DayId::new("z").unwrap());
    assert!(refused(c, unknown_day).await.contains("sessions[0].day"));
    let duplicate_set = mutate(&|d| {
        let first = d.sessions[0].sets[0].clone();
        d.sessions[1].sets.push(first);
    });
    assert!(
        refused(c, duplicate_set)
            .await
            .contains("sessions[1].sets[1].id")
    );
    let archived_active = mutate(&|d| d.programs[0].archived = true);
    assert!(refused(c, archived_active).await.contains("active_program"));
    let unended = mutate(&|d| d.sessions[0].finished_at = None);
    assert!(
        refused(c, unended)
            .await
            .contains("sessions[0].finished_at")
    );
    let mut bad_weight = serde_json::to_value(&valid).unwrap();
    bad_weight["training_maxes"][0]["weight"] = json!(-5);
    assert!(
        refused(c, bad_weight)
            .await
            .starts_with("This export is not valid")
    );
    let mut bad_settings = serde_json::to_value(&valid).unwrap();
    bad_settings["settings"]["default_rest"] = json!(100_000);
    assert!(refused(c, bad_settings).await.contains("default rest"));

    let after = (row_counts(&db, ids.0).await, row_counts(&db, ids.1).await);
    assert_eq!(after, counts, "nothing written");
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn oversize_imports_are_413(db: PgPool) {
    let api = TestApi::new(db).await;
    let mut c = api.user("C").await;
    let too_large = format!(
        "The export file is too large (the limit is {} MiB).",
        MAX_EXPORT_BYTES >> 20
    );

    // Announced: refused before the body is read.
    let announced = c
        .post(IMPORT)
        .header(header::CONTENT_LENGTH, IMPORT_BODY_LIMIT + 1)
        .body(Body::from("{}"))
        .unwrap();
    let (status, body) = c.send(announced).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["data"]["ServerError"]["message"], json!(too_large));

    // Sent without announcing it.
    let body = Body::from(vec![b' '; IMPORT_BODY_LIMIT + 1]);
    let (status, _) = c.send(c.post(IMPORT).body(body).unwrap()).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // A body within the transport limit holding a document over the export limit.
    let document = " ".repeat(MAX_EXPORT_BYTES + 1);
    let error = c.call_err(IMPORT, json!({ "document": document })).await;
    assert_eq!(
        error,
        CallError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: too_large
        }
    );
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_large_export_imports_past_the_default_body_limit(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut c) = (api.user("A").await, api.user("C").await);
    seed(&mut a).await;
    let mut document = export(&mut a).await;
    // Enough sets for a body well over axum's 2 MiB default.
    let session = &mut document.sessions[0];
    session.sets = (0..25_000_u16).map(|i| set(i % 100, t(1))).collect();
    let body = json!({ "document": serde_json::to_string(&document).unwrap() }).to_string();
    assert!(body.len() > 3 * 1024 * 1024, "{}", body.len());
    let summary = import(&mut c, &document).await.unwrap();
    assert_eq!(summary.sets, 25_000 + 1);
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM workout_sets WHERE user_id = $1")
        .bind(c.id.as_uuid())
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, 25_001);
}

// --- Deletion ----------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_rows_survive_while_a_deleted_account_leaves_none(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    // One row in every user-owned table, for both.
    db_testing::populate(&db, a.id).await;
    db_testing::populate(&db, b.id).await;
    let tables = user_tables(&db).await;
    assert!(tables.len() >= 12, "{tables:?}");
    for (table, count) in row_counts(&db, a.id).await {
        assert!(
            count > 0,
            "populate wrote nothing to {table}: the test would prove nothing"
        );
    }
    let b_rows = row_counts(&db, b.id).await;
    let b_data = export(&mut b).await;

    a.call::<()>(DELETE, json!({})).await.unwrap();

    for (table, count) in row_counts(&db, a.id).await {
        assert_eq!(count, 0, "{table} still has rows of the deleted user");
    }
    assert_eq!(row_counts(&db, b.id).await, b_rows);
    assert_eq!(training(&export(&mut b).await), training(&b_data));
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn deleting_the_account_signs_out_every_session(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut phone, mut passkey, credential) = api.user_with_passkey("A").await;
    let mut laptop = api.sign_in(&mut passkey, &credential).await;
    assert_eq!(laptop.id, phone.id);
    seed(&mut phone).await;

    laptop.call::<()>(DELETE, json!({})).await.unwrap();
    assert!(!laptop.has_cookie(), "the cookie is cleared");
    for browser in [&mut laptop, &mut phone] {
        let error = browser.call_err(ME, json!({})).await;
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    }
    // A replayed deletion has no session left.
    let error = phone.call_err(DELETE, json!({})).await;
    assert_eq!(
        error,
        CallError {
            status: StatusCode::UNAUTHORIZED,
            message: UNAUTHORIZED.to_owned()
        }
    );
    // The passkey no longer signs anyone in: the account is gone.
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(phone.id.as_uuid())
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn deleting_the_account_needs_a_recent_sign_in(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut passkey, credential) = api.user_with_passkey("A").await;
    seed(&mut a).await;
    let counts = row_counts(&db, a.id).await;
    age_sign_in(
        &db,
        a.id,
        i64::try_from(DELETE_REAUTH_WINDOW_SECS).unwrap() + 1,
    )
    .await;

    let error = a.call_err(DELETE, json!({})).await;
    assert_eq!(
        error,
        CallError {
            status: StatusCode::FORBIDDEN,
            message: REAUTHENTICATE.to_owned()
        }
    );
    assert_eq!(row_counts(&db, a.id).await, counts, "nothing deleted");
    assert!(
        a.call::<Value>(ME, json!({})).await.is_ok(),
        "still signed in"
    );

    // Signing in again (here in a new session) allows it.
    let mut again = api.sign_in(&mut passkey, &credential).await;
    again.call::<()>(DELETE, json!({})).await.unwrap();
    assert!(
        row_counts(&db, a.id)
            .await
            .iter()
            .all(|(_, count)| *count == 0)
    );
}

// --- Access ------------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_endpoint_needs_a_signed_in_same_origin_request(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut a = api.user("A").await;
    seed(&mut a).await;
    let document = serde_json::to_string(&export(&mut a).await).unwrap();
    let counts = row_counts(&db, a.id).await;
    let bodies = [
        (EXPORT, json!({})),
        (IMPORT, json!({ "document": document })),
        (DELETE, json!({})),
    ];
    for (path, body) in &bodies {
        testing::assert_unauthorized_when_signed_out(&api, path, body.clone()).await;
        let error = a.cross_site().call_err(path, body.clone()).await;
        assert_eq!(error.status, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(row_counts(&db, a.id).await, counts, "nothing changed");
    assert!(a.call::<Value>(ME, json!({})).await.is_ok());
}

// --- Review fixes ------------------------------------------------------------------------------

/// The review's scenario: a stale session (a stolen cookie) tries to add its own passkey, to sign
/// in afresh with it and delete the account. Adding the passkey needs the same recent sign-in.
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_stale_session_cannot_add_a_passkey_to_delete_the_account(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut victim = api.user("A").await;
    seed(&mut victim).await;
    age_sign_in(&db, victim.id, 24 * 60 * 60).await;
    let counts = row_counts(&db, victim.id).await;
    let mut thief = victim.clone();
    assert_eq!(
        thief.call_err(DELETE, json!({})).await.status,
        StatusCode::FORBIDDEN
    );

    // The thief's own passkey cannot be added: the ceremony is refused before it starts.
    let error = thief
        .call_err("/api/auth/passkey/add/begin", json!({}))
        .await;
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    // Nor Google linked.
    let error = thief
        .call_err(
            "/api/auth/google/begin",
            json!({ "intent": "Link", "popup": true }),
        )
        .await;
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(
        row_counts(&db, victim.id).await,
        counts,
        "nothing added or deleted"
    );
    let passkeys: i64 = sqlx::query_scalar("SELECT count(*) FROM passkeys WHERE user_id = $1")
        .bind(victim.id.as_uuid())
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(passkeys, 1);
}

/// The review's case: the export's version 1 of a program differs from the account's version 1.
/// Its session must land on a version that has its day, so that its plan loads, and the
/// account's program must keep its current version and stay the active program.
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn other_versions_of_an_existing_program_go_to_an_archived_companion(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let mut a = api.user("A").await;
    let creation = CreationId::new_v7();
    let main = upload(
        &mut a,
        json!({ "kind": "new_program", "creation_id": creation }),
        &program_json("Main", 5),
    )
    .await;
    a.call::<Value>(
        "/api/programs/active/set",
        json!({ "program_id": main.program.id }),
    )
    .await
    .unwrap();
    let mut document = export(&mut a).await;
    let mine = document.programs[0].clone();
    // The export's version 1 differs (day `b`), and it has a version 2 the account lacks too.
    let mut other = program_json("Main", 5);
    other["days"][0]["id"] = json!("b");
    other["rotation"] = json!(["b"]);
    document.programs[0].versions[0].document = other.clone();
    let mut newer = document.programs[0].versions[0].clone();
    newer.version = 2;
    newer.document = program_json("Main", 9);
    document.programs[0].versions.push(newer.clone());
    let session = SessionId::new_v7();
    document.sessions.push(ExportSession {
        id: session,
        program: creation,
        version: 1,
        day: iron_oxide_domain::DayId::new("b").unwrap(),
        status: SessionStatus::Completed,
        started_at: t(0),
        finished_at: Some(t(10)),
        sets: vec![set(0, t(1))],
    });

    let summary = import(&mut a, &document).await.unwrap();
    assert_eq!(
        (
            summary.programs,
            summary.versions,
            summary.sessions,
            summary.sets
        ),
        (1, 2, 1, 1)
    );
    // The session's plan loads, on its own day.
    let plan: Value = a
        .call("/api/sessions/plan", json!({ "session_id": session }))
        .await
        .unwrap();
    assert_eq!(plan["day_name"], json!("Day A"), "{plan}");
    // The account's program is untouched: same versions, same current version, still active.
    let after = export(&mut a).await;
    assert_eq!(after.programs[0], mine);
    let active: Value = a.call("/api/programs/active", json!({})).await.unwrap();
    assert_eq!(active["program"]["id"], json!(main.program.id));
    assert_eq!(active["version"]["version"], json!(1));
    assert_eq!(active["document"]["days"][0]["id"], json!("a"));
    assert_eq!(
        active["document"]["days"][0]["exercises"][0]["work"]["reps"]["reps"],
        json!(5)
    );
    // The export's versions are in the archived companion, under their own numbers.
    let companion = &after.programs[1];
    assert_eq!(companion.name, "Main (imported)");
    assert!(companion.archived);
    let versions: Vec<(u32, &Value)> = companion
        .versions
        .iter()
        .map(|v| (v.version, &v.document))
        .collect();
    assert_eq!(versions, [(1, &other), (2, &newer.document)]);
    assert_eq!(after.sessions[0].program, companion.creation_id);
    assert_eq!(after.sessions[0].version, 1);

    // Idempotent: the second time, the same companion and versions are found.
    assert_eq!(
        import(&mut a, &document).await.unwrap(),
        ImportSummary::default()
    );
    assert_eq!(training(&export(&mut a).await), training(&after));
}

#[test]
fn companions_have_a_stable_id_and_a_name_that_fits() {
    let id = Uuid::from_u128(42);
    assert_eq!(companion_creation_id(id), companion_creation_id(id));
    assert_ne!(companion_creation_id(id), id);
    assert_ne!(
        companion_creation_id(id),
        companion_creation_id(Uuid::from_u128(43))
    );
    assert_eq!(companion_name("Main"), "Main (imported)");
    let long = companion_name(&"é".repeat(100));
    assert_eq!(long.chars().count(), 100);
    assert!(long.ends_with(" (imported)"));
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_trickled_import_times_out_and_frees_its_slot(db: PgPool) {
    let api = TestApi::with_config(db.clone(), |config| {
        config.request_limits.import_body_read_timeout = std::time::Duration::from_millis(300);
    })
    .await;
    let (mut a, mut c) = (api.user("A").await, api.user("C").await);
    seed(&mut a).await;
    let document = export(&mut a).await;
    // A body that sends a little, then nothing more.
    let request = c
        .post(IMPORT)
        .body(crate::server::limits::tests::chunked(16 * 1024, true))
        .unwrap();
    let (status, body) = c.send(request).await;
    assert_eq!(status, StatusCode::REQUEST_TIMEOUT, "{body}");
    let slots = api.import_slots();
    assert_eq!(
        slots.available_permits(),
        MAX_CONCURRENT_IMPORTS,
        "the slot is freed"
    );
    // Twice as many trickled imports as slots, one after the other: none is left holding one.
    for _ in 0..2 * MAX_CONCURRENT_IMPORTS {
        let request = c
            .post(IMPORT)
            .body(crate::server::limits::tests::chunked(8 * 1024, true))
            .unwrap();
        assert_eq!(c.send(request).await.0, StatusCode::REQUEST_TIMEOUT);
    }
    assert_eq!(slots.available_permits(), MAX_CONCURRENT_IMPORTS);
    assert_ne!(
        import(&mut c, &document).await.unwrap(),
        ImportSummary::default()
    );
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_third_concurrent_import_is_a_retryable_503(db: PgPool) {
    let api = TestApi::new(db.clone()).await;
    let (mut a, mut c) = (api.user("A").await, api.user("C").await);
    seed(&mut a).await;
    let document = export(&mut a).await;
    let body = json!({ "document": serde_json::to_string(&document).unwrap() }).to_string();
    let counts = row_counts(&db, c.id).await;

    // Two imports running: every slot is taken.
    let held = api
        .import_slots()
        .try_acquire_many_owned(u32::try_from(MAX_CONCURRENT_IMPORTS).unwrap())
        .unwrap();
    let response = c
        .send_response(c.post(IMPORT).body(Body::from(body.clone())).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()[header::RETRY_AFTER],
        BUSY_RETRY_AFTER_SECS.to_string().as_str()
    );
    assert_eq!(row_counts(&db, c.id).await, counts, "nothing written");
    let error = c.call_err(IMPORT, json!({ "document": "{}" })).await;
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    let failure =
        crate::api::error::ApiFailure::classify(&dioxus::prelude::ServerFnError::ServerError {
            message: error.message.clone(),
            code: 503,
            details: None,
        });
    assert!(failure.kind.is_retryable(), "{failure:?}");

    // One finishes: the retry goes through.
    drop(held);
    assert_ne!(
        import(&mut c, &document).await.unwrap(),
        ImportSummary::default()
    );
}

//! Endpoint tests for the program functions: the real router over Postgres, as users A and B.

use dioxus::prelude::ServerFnError;
use dioxus::server::axum::{
    body::Body,
    http::{StatusCode, header},
};
use iron_oxide_domain::program::{builtin_programs, limits::MAX_REPORTED_ERRORS};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::types::Uuid;

use super::*;
use crate::api::programs::ProgramProblems;
use crate::server::api::{
    error::{INVALID_PROGRAM, NOT_FOUND},
    testing::{self, TestApi, TestUser},
};
use crate::server::db::testing as db_testing;

const BUILTINS: &str = "/api/programs/builtins";
const COPY: &str = "/api/programs/copy-builtin";
const LIST: &str = "/api/programs/list";
const GET: &str = "/api/programs/get";
const ACTIVE: &str = "/api/programs/active";
const SET_ACTIVE: &str = "/api/programs/active/set";
const UPLOAD: &str = "/api/programs/upload";
const VERSIONS: &str = "/api/programs/versions";
const ARCHIVE: &str = "/api/programs/archive";

const FULL_BODY: &str = "full-body-3day";

/// The app, with the built-in programs seeded as at startup.
async fn api(db: PgPool) -> TestApi {
    let builtins = builtin_programs().unwrap();
    programs::seed_builtins(&db, &programs::builtin_seeds(&builtins))
        .await
        .unwrap();
    TestApi::new(db).await
}

async fn ok<T: DeserializeOwned>(user: &mut TestUser, path: &str, body: Value) -> T {
    user.call(path, body)
        .await
        .unwrap_or_else(|error| panic!("{path}: {error:?}"))
}

/// The status and the body of a call that must fail.
async fn fails(user: &mut TestUser, path: &str, body: Value) -> (StatusCode, Value) {
    let request = user.post(path).body(Body::from(body.to_string())).unwrap();
    let (status, body) = user.send(request).await;
    assert!(!status.is_success(), "{path} succeeded: {body}");
    (status, body)
}

/// The public message of a failed call's body, as the server function wrote it.
fn message(body: &Value) -> &str {
    body["data"]["ServerError"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message in {body}"))
}

async fn status(user: &mut TestUser, path: &str, body: Value) -> StatusCode {
    fails(user, path, body).await.0
}

async fn copy_of(user: &mut TestUser, creation: CreationId) -> ProgramDetail {
    ok(
        user,
        COPY,
        json!({ "builtin_id": FULL_BODY, "creation_id": creation }),
    )
    .await
}

fn upload_body(target: UploadTarget, document: &str) -> Value {
    json!({ "target": target, "document": document })
}

fn new_program() -> UploadTarget {
    UploadTarget::NewProgram {
        creation_id: CreationId::new_v7(),
    }
}

async fn upload(user: &mut TestUser, target: UploadTarget, document: &str) -> UploadOutcome {
    ok(user, UPLOAD, upload_body(target, document)).await
}

async fn upload_version(user: &mut TestUser, id: ProgramId, document: &str) -> UploadOutcome {
    upload(user, UploadTarget::NewVersion { program_id: id }, document).await
}

async fn programs_of(user: &mut TestUser, include_archived: bool) -> Vec<ProgramView> {
    ok(user, LIST, json!({ "include_archived": include_archived })).await
}

async fn active_of(user: &mut TestUser) -> Option<ProgramDetail> {
    ok(user, ACTIVE, json!({})).await
}

async fn versions_of(user: &mut TestUser, id: ProgramId) -> Vec<VersionView> {
    ok(user, VERSIONS, json!({ "program_id": id })).await
}

/// The built-in program's JSON, renamed.
fn document(name: &str) -> String {
    let mut value: Value = serde_json::from_str(builtin_programs().unwrap()[0].json()).unwrap();
    value["name"] = json!(name);
    serde_json::to_string_pretty(&value).unwrap()
}

/// The id of the built-in program's own row (not a user's copy).
async fn builtin_row(db: &PgPool) -> ProgramId {
    programs::list_builtins(db).await.unwrap()[0]
        .program
        .id
        .into()
}

/// The problems of a refused upload, read the way the client reads them.
async fn problems(user: &mut TestUser, document: &str) -> ProgramProblems {
    let (status, body) = fails(user, UPLOAD, upload_body(new_program(), document)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(message(&body), INVALID_PROGRAM, "{body}");
    // What the Dioxus client turns this response into.
    let error = ServerFnError::ServerError {
        message: message(&body).to_owned(),
        code: 422,
        details: body.get("data").cloned(),
    };
    ProgramProblems::from_error(&error).unwrap_or_else(|| panic!("no problems in {body}"))
}

// --- Behaviour ---------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn builtins_are_listed_and_copied_idempotently(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;

    let builtins: Vec<BuiltinProgramView> = ok(&mut a, BUILTINS, json!({})).await;
    let expected = builtin_programs().unwrap();
    assert_eq!(builtins.len(), expected.len());
    let full_body = &builtins[0];
    assert_eq!(full_body.builtin_id.as_str(), FULL_BODY);
    assert_eq!(&full_body.document, expected[0].program());
    assert_eq!(full_body.name, expected[0].program().name);
    assert_eq!(full_body.version, 1);

    let key = CreationId::new_v7();
    let copy = copy_of(&mut a, key).await;
    assert_eq!(copy.program.name, full_body.name);
    assert_eq!(
        copy.program
            .source_builtin_id
            .as_ref()
            .map(BuiltinProgramId::as_str),
        Some(FULL_BODY)
    );
    assert!(!copy.program.archived);
    assert_eq!(copy.version.version, 1);
    assert_eq!(copy.document, full_body.document);
    // A retry returns the same copy; another key makes another copy.
    assert_eq!(copy_of(&mut a, key).await, copy);
    assert_eq!(programs_of(&mut a, false).await, vec![copy.program.clone()]);
    let other = copy_of(&mut a, CreationId::new_v7()).await;
    assert_ne!(other.program.id, copy.program.id);
    assert_eq!(programs_of(&mut a, false).await.len(), 2);

    // Unknown and malformed built-in ids are the same 404.
    for builtin_id in ["nope", "Not A Slug", ""] {
        let body = json!({ "builtin_id": builtin_id, "creation_id": CreationId::new_v7() });
        let (status, body) = fails(&mut a, COPY, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{builtin_id}");
        assert_eq!(message(&body), NOT_FOUND);
    }
    // A key already used for an upload is a conflict, not a copy.
    let target = new_program();
    upload(&mut a, target, &document("Keyed")).await;
    let UploadTarget::NewProgram { creation_id } = target else {
        unreachable!()
    };
    let body = json!({ "builtin_id": FULL_BODY, "creation_id": creation_id });
    assert_eq!(status(&mut a, COPY, body).await, StatusCode::CONFLICT);
    assert_eq!(programs_of(&mut a, false).await.len(), 3);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn get_program_returns_the_latest_version(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    let copy = copy_of(&mut a, CreationId::new_v7()).await;
    let detail: ProgramDetail = ok(&mut a, GET, json!({ "program_id": copy.program.id })).await;
    assert_eq!(detail, copy);

    let v2 = upload_version(&mut a, copy.program.id, &document("Changed")).await;
    let detail: ProgramDetail = ok(&mut a, GET, json!({ "program_id": copy.program.id })).await;
    assert_eq!(detail.version, v2.version);
    assert_eq!(detail.document.name, "Changed");
    // A new version does not rename the program.
    assert_eq!(detail.program, copy.program);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn the_active_program(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    assert_eq!(active_of(&mut a).await, None);

    let first = copy_of(&mut a, CreationId::new_v7()).await;
    let second = upload(&mut a, new_program(), &document("Second")).await;
    let set: ProgramDetail = ok(
        &mut a,
        SET_ACTIVE,
        json!({ "program_id": first.program.id }),
    )
    .await;
    assert_eq!(set, first);
    assert_eq!(active_of(&mut a).await, Some(first.clone()));
    // Again is a no-op; another program replaces it.
    let _: ProgramDetail = ok(
        &mut a,
        SET_ACTIVE,
        json!({ "program_id": first.program.id }),
    )
    .await;
    let _: ProgramDetail = ok(
        &mut a,
        SET_ACTIVE,
        json!({ "program_id": second.program.id }),
    )
    .await;
    let active = active_of(&mut a).await.unwrap();
    assert_eq!(active.program, second.program);
    // It follows the latest version.
    let v2 = upload_version(&mut a, second.program.id, &document("Second v2")).await;
    let active = active_of(&mut a).await.unwrap();
    assert_eq!(active.version, v2.version);
    assert_eq!(active.document.name, "Second v2");

    // The active program cannot be archived, and an archived one cannot be made active.
    let body = json!({ "program_id": second.program.id, "archived": true });
    assert_eq!(status(&mut a, ARCHIVE, body).await, StatusCode::CONFLICT);
    let () = ok(
        &mut a,
        ARCHIVE,
        json!({ "program_id": first.program.id, "archived": true }),
    )
    .await;
    let body = json!({ "program_id": first.program.id });
    assert_eq!(status(&mut a, SET_ACTIVE, body).await, StatusCode::CONFLICT);
    assert_eq!(
        active_of(&mut a).await.map(|detail| detail.program.id),
        Some(second.program.id)
    );
    assert!(!programs_of(&mut a, true).await[1].archived);

    // A built-in is never active: it must be copied first.
    let body = json!({ "program_id": builtin_row(&api.db).await });
    assert_eq!(
        status(&mut a, SET_ACTIVE, body).await,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn archive_and_restore(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    let kept = copy_of(&mut a, CreationId::new_v7()).await;
    let old = upload(&mut a, new_program(), &document("Old")).await;
    let archive = |archived: bool| json!({ "program_id": old.program.id, "archived": archived });

    let () = ok(&mut a, ARCHIVE, archive(true)).await;
    let visible: Vec<ProgramId> = programs_of(&mut a, false)
        .await
        .iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(visible, vec![kept.program.id]);
    let all = programs_of(&mut a, true).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|p| p.id == old.program.id && p.archived));
    // Nothing is deleted: it can still be read and get versions.
    assert_eq!(versions_of(&mut a, old.program.id).await, vec![old.version]);
    let v2 = upload_version(&mut a, old.program.id, &document("Old v2")).await;
    assert!(v2.saved);
    assert!(v2.program.archived);
    // Archiving twice is fine; restoring brings it back.
    let () = ok(&mut a, ARCHIVE, archive(true)).await;
    let () = ok(&mut a, ARCHIVE, archive(false)).await;
    assert_eq!(programs_of(&mut a, false).await.len(), 2);
}

// --- Uploads -----------------------------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn uploads_create_programs_and_versions(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;

    let created = upload(&mut a, new_program(), &document("Mine")).await;
    assert!(created.saved);
    assert_eq!(created.program.name, "Mine");
    assert_eq!(created.program.source_builtin_id, None);
    assert_eq!(created.version.version, 1);
    let id = created.program.id;

    // The same file again, or reformatted: no new version.
    let again = upload_version(&mut a, id, &document("Mine")).await;
    assert!(!again.saved);
    assert_eq!(again.version, created.version);
    let compact: Value = serde_json::from_str(&document("Mine")).unwrap();
    let again = upload_version(&mut a, id, &compact.to_string()).await;
    assert!(!again.saved);
    assert_eq!(again.version, created.version);

    // A change is a new version, and so is going back to the first document.
    let v2 = upload_version(&mut a, id, &document("Mine v2")).await;
    assert!(v2.saved);
    assert_eq!((v2.program.id, v2.version.version), (id, 2));
    let v3 = upload_version(&mut a, id, &document("Mine")).await;
    assert_eq!(v3.version.version, 3);
    assert_eq!(
        versions_of(&mut a, id).await,
        vec![created.version, v2.version, v3.version]
    );

    // Uploads are stored as written: the built-in's own file equals a fresh copy of it.
    let copy = copy_of(&mut a, CreationId::new_v7()).await;
    let same = upload_version(
        &mut a,
        copy.program.id,
        builtin_programs().unwrap()[0].json(),
    )
    .await;
    assert!(!same.saved);
    assert_eq!(same.version, copy.version);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn a_retried_new_program_upload_is_not_a_second_program(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    let target = new_program();
    let first = upload(&mut a, target, &document("P")).await;
    assert!(first.saved);
    let retry = upload(&mut a, target, &document("P")).await;
    assert!(!retry.saved);
    assert_eq!(
        (retry.program, retry.version),
        (first.program, first.version)
    );
    // The same key with another document is not a retry.
    let body = upload_body(target, &document("Q"));
    assert_eq!(status(&mut a, UPLOAD, body).await, StatusCode::CONFLICT);
    assert_eq!(programs_of(&mut a, true).await.len(), 1);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn invalid_uploads_get_the_path_aware_errors(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;

    // Not JSON: one problem, with its line and column.
    let found = problems(&mut a, "{\n  \"name\": ").await;
    assert_eq!((found.errors.len(), found.omitted), (1, 0));
    assert_eq!(found.errors[0].line, Some(2));
    assert!(found.errors[0].column.is_some());

    // The wrong shape: the path of the problem.
    let mut value: Value = serde_json::from_str(&document("P")).unwrap();
    value["days"][0]["exercises"][0]["rest"] = json!("long");
    let found = problems(&mut a, &value.to_string()).await;
    assert_eq!(
        found.errors[0].path, "days[0].exercises[0].rest",
        "{found:?}"
    );

    // Broken rules: every one, with its path.
    let mut value: Value = serde_json::from_str(&document("P")).unwrap();
    value["name"] = json!(" ");
    value["days"][0]["exercises"][0]["rest"] = json!(9000);
    let found = problems(&mut a, &value.to_string()).await;
    let paths: Vec<&str> = found.errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["name", "days[0].exercises[0].rest"], "{found:?}");
    assert!(found.errors.iter().all(|e| e.line.is_none()));

    // An unsupported schema version, and a document that is not an object.
    let found = problems(&mut a, r#"{"schema_version": 2, "name": "P"}"#).await;
    assert_eq!(found.errors[0].path, "schema_version");
    assert_eq!(problems(&mut a, "[1, 2]").await.errors.len(), 1);

    // The same checks for a new version, which is not saved either.
    let created = upload(&mut a, new_program(), &document("P")).await;
    let target = UploadTarget::NewVersion {
        program_id: created.program.id,
    };
    let (status, _) = fails(&mut a, UPLOAD, upload_body(target, "{")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        versions_of(&mut a, created.program.id).await,
        vec![created.version]
    );
    assert_eq!(programs_of(&mut a, true).await.len(), 1);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn the_problem_list_is_bounded(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    // 14 days (the limit) of 30 exercises (the limit) with a blank name each: 420 problems.
    let mut value: Value = serde_json::from_str(&document("P")).unwrap();
    let mut exercise = value["days"][0]["exercises"][0].clone();
    // Small exercises, so the document stays under the size limit.
    exercise.as_object_mut().unwrap().remove("notes");
    exercise.as_object_mut().unwrap().remove("warmup");
    let days: Vec<Value> = (0..14)
        .map(|day| {
            let exercises: Vec<Value> = (0..30)
                .map(|n| {
                    let mut exercise = exercise.clone();
                    exercise["id"] = json!(format!("e-{n}"));
                    exercise["name"] = json!(" ");
                    exercise
                })
                .collect();
            json!({ "id": format!("d-{day}"), "name": "Day", "exercises": exercises })
        })
        .collect();
    value["days"] = json!(days);
    value["rotation"] = json!((0..14).map(|day| format!("d-{day}")).collect::<Vec<_>>());
    let found = problems(&mut a, &value.to_string()).await;
    assert_eq!(found.errors.len(), MAX_REPORTED_ERRORS);
    assert_eq!(found.omitted, 420 - MAX_REPORTED_ERRORS);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn oversized_uploads_are_refused_before_parsing(db: PgPool) {
    let api = api(db).await;
    let mut a = api.user("A").await;
    let upload_status = |document: String| upload_body(new_program(), &document);

    // A document just over the limit, in a body under the transport limit: refused by its size
    // (it is not even JSON, so it was not parsed).
    let body = upload_status("x".repeat(MAX_DOCUMENT_BYTES + 1));
    let (status, body) = fails(&mut a, UPLOAD, body).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(
        message(&body),
        "The program file is too large (the limit is 256 KiB)."
    );
    // At the limit, it is parsed (and refused as invalid JSON).
    let body = upload_status("x".repeat(MAX_DOCUMENT_BYTES));
    assert_eq!(
        status_of(&mut a, body).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // A document of quotes doubles once escaped in the body, and still reaches the parser.
    let body = upload_status("\"".repeat(MAX_DOCUMENT_BYTES));
    assert_eq!(
        status_of(&mut a, body).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    let raw = |size: usize| upload_body(new_program(), &"x".repeat(size)).to_string();
    // A body over the transport limit, announced by its Content-Length…
    let big = raw(UPLOAD_BODY_LIMIT);
    let request = a
        .post(UPLOAD)
        .header(header::CONTENT_LENGTH, big.len())
        .body(Body::from(big.clone()))
        .unwrap();
    assert_eq!(a.send(request).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    // …or not (as a chunked body arrives)…
    let request = a.post(UPLOAD).body(Body::from(big)).unwrap();
    assert_eq!(a.send(request).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    // …or with a Content-Length that lies.
    let request = a
        .post(UPLOAD)
        .header(header::CONTENT_LENGTH, 10)
        .body(Body::from(raw(UPLOAD_BODY_LIMIT)))
        .unwrap();
    assert_eq!(a.send(request).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    // Far over axum's 2 MiB default, where Dioxus alone would panic: still a clean 413.
    let request = a.post(UPLOAD).body(Body::from(raw(3 << 20))).unwrap();
    let (status, body) = a.send(request).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    // In the same shape as the server function's own errors.
    assert_eq!(
        message(&body),
        "The program file is too large (the limit is 256 KiB)."
    );
    assert_eq!(body["code"], 413);

    // Nothing was saved, and the upload still works.
    assert!(programs_of(&mut a, true).await.is_empty());
    assert!(upload(&mut a, new_program(), &document("P")).await.saved);
}

async fn status_of(user: &mut TestUser, body: Value) -> StatusCode {
    status(user, UPLOAD, body).await
}

// --- Authentication and isolation --------------------------------------------------------------

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn every_function_needs_a_signed_in_user(db: PgPool) {
    let api = api(db).await;
    let id = ProgramId::new_v7();
    for (path, body) in [
        (BUILTINS, json!({})),
        (
            COPY,
            json!({ "builtin_id": FULL_BODY, "creation_id": CreationId::new_v7() }),
        ),
        (LIST, json!({ "include_archived": true })),
        (GET, json!({ "program_id": id })),
        (ACTIVE, json!({})),
        (SET_ACTIVE, json!({ "program_id": id })),
        (UPLOAD, upload_body(new_program(), &document("P"))),
        (VERSIONS, json!({ "program_id": id })),
        (ARCHIVE, json!({ "program_id": id, "archived": true })),
    ] {
        testing::assert_unauthorized_when_signed_out(&api, path, body).await;
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM programs WHERE user_id IS NOT NULL")
        .fetch_one(&api.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// A's data for the isolation tests: a copy of the built-in (the active program) and an uploaded
/// program with two versions.
struct Owned {
    copy: ProgramDetail,
    uploaded: UploadOutcome,
    programs: Vec<ProgramView>,
    versions: Vec<VersionView>,
    active: Option<ProgramDetail>,
}

impl Owned {
    async fn create(a: &mut TestUser) -> Self {
        let copy = copy_of(a, CreationId::new_v7()).await;
        let uploaded = upload(a, new_program(), &document("A's")).await;
        upload_version(a, uploaded.program.id, &document("A's v2")).await;
        let _: ProgramDetail = ok(a, SET_ACTIVE, json!({ "program_id": copy.program.id })).await;
        Self {
            programs: programs_of(a, true).await,
            versions: versions_of(a, uploaded.program.id).await,
            active: active_of(a).await,
            copy,
            uploaded,
        }
    }

    /// A's programs, their versions and the active program are exactly as they were.
    async fn assert_unchanged(&self, a: &mut TestUser) {
        assert_eq!(programs_of(a, true).await, self.programs);
        assert_eq!(
            versions_of(a, self.uploaded.program.id).await,
            self.versions
        );
        assert_eq!(active_of(a).await, self.active);
    }

    fn ids(&self) -> [Uuid; 2] {
        [
            self.copy.program.id.as_uuid(),
            self.uploaded.program.id.as_uuid(),
        ]
    }
}

/// B gets `404 Not found.` for each of A's program ids, for the built-in's own row, and for an id
/// of nobody's, and A's data does not change.
async fn assert_isolated(db: PgPool, path: &str, body: impl Fn(Uuid) -> Value) {
    let api = api(db).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    let owned = Owned::create(&mut a).await;
    for id in owned.ids() {
        testing::assert_not_found_for_other_user(&mut b, path, id, &body).await;
    }
    let builtin = builtin_row(&api.db).await.as_uuid();
    testing::assert_not_found_for_other_user(&mut b, path, builtin, &body).await;
    owned.assert_unchanged(&mut a).await;
    // Nor did B get anything.
    assert!(programs_of(&mut b, true).await.is_empty());
    assert_eq!(active_of(&mut b).await, None);
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_program_cannot_be_read(db: PgPool) {
    assert_isolated(db, GET, |id| json!({ "program_id": id })).await;
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_program_cannot_be_made_active(db: PgPool) {
    assert_isolated(db, SET_ACTIVE, |id| json!({ "program_id": id })).await;
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_program_versions_cannot_be_listed(db: PgPool) {
    assert_isolated(db, VERSIONS, |id| json!({ "program_id": id })).await;
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_program_cannot_get_a_version(db: PgPool) {
    assert_isolated(db, UPLOAD, |id| {
        let target = UploadTarget::NewVersion {
            program_id: ProgramId::from_uuid(id),
        };
        upload_body(target, &document("B's"))
    })
    .await;
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_program_cannot_be_archived_or_restored(db: PgPool) {
    assert_isolated(
        db,
        ARCHIVE,
        |id| json!({ "program_id": id, "archived": true }),
    )
    .await;
}

#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_archived_program_cannot_be_restored(db: PgPool) {
    let api = api(db).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    let archived = upload(&mut a, new_program(), &document("A's")).await;
    let archive = |id: Uuid| json!({ "program_id": id, "archived": false });
    let () = ok(
        &mut a,
        ARCHIVE,
        json!({ "program_id": archived.program.id, "archived": true }),
    )
    .await;
    testing::assert_not_found_for_other_user(
        &mut b,
        ARCHIVE,
        archived.program.id.as_uuid(),
        archive,
    )
    .await;
    assert!(programs_of(&mut a, true).await[0].archived);
}

/// Lists and the active program are per user, and creation ids are too: B's copy or upload with
/// A's key is B's own new program.
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn another_users_programs_are_not_listed_or_copied_into(db: PgPool) {
    let api = api(db).await;
    let (mut a, mut b) = api.users_a_and_b().await;
    let copy_key = CreationId::new_v7();
    let a_copy = copy_of(&mut a, copy_key).await;
    let upload_key = CreationId::new_v7();
    let a_upload = upload(
        &mut a,
        UploadTarget::NewProgram {
            creation_id: upload_key,
        },
        &document("A's"),
    )
    .await;
    let _: ProgramDetail = ok(
        &mut a,
        SET_ACTIVE,
        json!({ "program_id": a_copy.program.id }),
    )
    .await;
    let a_programs = programs_of(&mut a, true).await;
    let a_active = active_of(&mut a).await;

    assert!(programs_of(&mut b, true).await.is_empty());
    assert_eq!(active_of(&mut b).await, None);

    // The same keys (and the same document) make B's own programs.
    let b_copy = copy_of(&mut b, copy_key).await;
    assert_ne!(b_copy.program.id, a_copy.program.id);
    let b_upload = upload(
        &mut b,
        UploadTarget::NewProgram {
            creation_id: upload_key,
        },
        &document("A's"),
    )
    .await;
    assert!(b_upload.saved);
    assert_ne!(b_upload.program.id, a_upload.program.id);
    let _: ProgramDetail = ok(
        &mut b,
        SET_ACTIVE,
        json!({ "program_id": b_upload.program.id }),
    )
    .await;

    // Each sees only their own.
    let b_ids: Vec<ProgramId> = programs_of(&mut b, true)
        .await
        .iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(b_ids, vec![b_copy.program.id, b_upload.program.id]);
    assert_eq!(
        active_of(&mut b).await.map(|detail| detail.program.id),
        Some(b_upload.program.id)
    );
    assert_eq!(programs_of(&mut a, true).await, a_programs);
    assert_eq!(active_of(&mut a).await, a_active);
}

// --- Activating and archiving concurrently ------------------------------------------------------

/// A new program of `owner`'s with a valid document (`set_active` returns it).
async fn valid_program(db: &PgPool, owner: UserId) -> ProgramId {
    let document: JsonValue = serde_json::from_str(&document("Race")).unwrap();
    let creation = crate::server::db::ids::CreationId::from(CreationId::new_v7());
    let (_, program, _) = programs::create(db, owner, creation, "Race", &document)
        .await
        .unwrap();
    program.id.into()
}

/// `owner`'s active program and whether `program` is archived, as stored.
async fn stored_state(db: &PgPool, owner: UserId, program: ProgramId) -> (Option<ProgramId>, bool) {
    let active = active_program::get(db, owner)
        .await
        .unwrap()
        .map(Into::into);
    let archived = programs::get(db, owner, program.into())
        .await
        .unwrap()
        .archived;
    (active, archived)
}

/// Waits until `waiters` statements of this test's database wait for a lock.
async fn wait_for_lock_waiters(db: &PgPool, waiters: i64) {
    for _ in 0..1_000 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(db)
        .await
        .unwrap();
        if waiting >= waiters {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("fewer than {waiters} statements ever waited for a lock");
}

/// Making a program active and archiving it at the same time: exactly one of them wins, the other
/// gets `409`, and an archived program is never the active one (it was in 299 of 300 runs before
/// both took the program's row lock).
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn activating_and_archiving_at_once_never_leave_an_archived_active_program(db: PgPool) {
    let owner = db_testing::user(&db).await;
    let other = valid_program(&db, owner).await;
    for _ in 0..100 {
        active_program::set(&db, owner, other.into()).await.unwrap();
        let program = valid_program(&db, owner).await;
        let (activated, archived) = tokio::join!(
            set_active(&db, owner, program),
            set_archived(&db, owner, program, true)
        );
        let state = stored_state(&db, owner, program).await;
        match (&activated, &archived) {
            (Ok(_), Err(ApiError::Conflict(_))) => assert_eq!(state, (Some(program), false)),
            (Err(ApiError::Conflict(_)), Ok(())) => assert_eq!(state, (Some(other), true)),
            _ => panic!(
                "activate: {:?}, archive: {archived:?}, stored: {state:?}",
                activated.as_ref().map(|detail| detail.program.id)
            ),
        }
    }
}

/// The reviewer's interleaving, forced: an activation that waits (here on another transaction's
/// lock) after its checks makes a concurrent archive wait too, which then sees the new active
/// program and refuses.
#[sqlx::test(migrator = "crate::server::db::MIGRATOR")]
#[ignore = "needs Postgres"]
async fn an_archive_waits_for_a_pending_activation(db: PgPool) {
    let owner = db_testing::user(&db).await;
    let other = valid_program(&db, owner).await;
    active_program::set(&db, owner, other.into()).await.unwrap();
    let program = valid_program(&db, owner).await;

    let mut blocker = db.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM active_program WHERE user_id = $1 FOR UPDATE")
        .bind(owner.as_uuid())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let activate = tokio::spawn({
        let db = db.clone();
        async move { set_active(&db, owner, program).await }
    });
    wait_for_lock_waiters(&db, 1).await;
    let archive = tokio::spawn({
        let db = db.clone();
        async move { set_archived(&db, owner, program, true).await }
    });
    // The archive waits for the activation's lock on the program instead of going ahead.
    wait_for_lock_waiters(&db, 2).await;
    assert!(!archive.is_finished());
    blocker.rollback().await.unwrap();

    let activated = activate.await.unwrap();
    assert!(activated.is_ok(), "{activated:?}");
    let archived = archive.await.unwrap();
    assert!(
        matches!(archived, Err(ApiError::Conflict(_))),
        "{archived:?}"
    );
    assert_eq!(
        stored_state(&db, owner, program).await,
        (Some(program), false)
    );
}

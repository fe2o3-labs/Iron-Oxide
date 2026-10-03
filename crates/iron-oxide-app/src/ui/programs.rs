//! The Programs screen (#35): the built-in programs and the user's own, the active one, a
//! program's details and versions, copying a built-in, activating, archiving, and uploading a
//! `program.json`.
//!
//! Everything shown from a program document, and every problem the server reports about an
//! uploaded one, is rendered as text nodes: those texts are the user's own and may contain markup.
//!
//! Copies and new-program uploads carry a client `creation_id` (UUIDv7). It is kept until the
//! request succeeds, so retrying after a lost answer returns the program the first attempt
//! created instead of a second one (which would also take a second slot of the plan's quota).

use dioxus::prelude::*;
use iron_oxide_domain::entitlements::{Entitlements, Feature, Limit, Quota};
use iron_oxide_domain::program::{
    BuiltinProgramId, Load, PROGRAM_SCHEMA_URL, Program, ProgressionRule, RepTarget, UnitWeight,
    Work, limits::MAX_DOCUMENT_BYTES,
};
use iron_oxide_domain::time::Timestamp;
use iron_oxide_domain::{CreationId, ProgramId, Unit};

use super::components::{Button, ButtonVariant, Card, Chip, EmptyState, LoadingState};
use super::errors::{BannerKind, Errors, use_errors};
use super::shell::Route;
use super::weight::{use_unit, weight_text};
use crate::api::billing::my_entitlements;
use crate::api::error::{ApiFailure, FailureKind};
use crate::api::programs::{
    BuiltinProgramView, ProgramDetail, ProgramProblem, ProgramProblems, ProgramView, UploadTarget,
    VersionView, copy_builtin_program, get_active_program, get_program, list_builtin_programs,
    list_program_versions, list_programs, set_active_program, set_program_archived, upload_program,
};

// --- View models ---------------------------------------------------------------------------------

/// `3 × 5`, `3 × 8–12`, `3 × 45 s hold`, `8 rounds: 30 s on, 90 s off`.
#[must_use]
pub fn work_text(work: Work) -> String {
    match work {
        Work::Reps { sets, reps } => match reps {
            RepTarget::Fixed(reps) => format!("{sets} × {reps}"),
            RepTarget::Range(range) => format!("{sets} × {}–{}", range.min, range.max),
        },
        Work::Hold { sets, seconds } => format!("{sets} × {} s hold", seconds.get()),
        Work::Intervals { work, rest, rounds } => {
            format!("{rounds} rounds: {} s on, {} s off", work.get(), rest.get())
        }
    }
}

/// A program weight in the user's unit, with the value as written when the program uses the other
/// unit: `60 kg`, `132.28 lb (60 kg)`.
#[must_use]
pub fn program_weight_text(weight: UnitWeight, unit: Unit) -> String {
    let shown = weight_text(weight.weight(), unit);
    if weight.unit() == unit {
        shown
    } else {
        format!("{shown} ({weight})")
    }
}

/// `60 kg` (in the user's unit), `75% of training max`, or nothing.
#[must_use]
pub fn load_text(load: Option<Load>, unit: Unit) -> Option<String> {
    match load? {
        Load::Weight(weight) => Some(program_weight_text(weight, unit)),
        Load::PercentOfTrainingMax(percent) => Some(format!("{percent} of training max")),
    }
}

/// How an exercise progresses, in a few words.
#[must_use]
pub fn progression_text(rule: &ProgressionRule, unit: Unit) -> String {
    let main = match rule {
        ProgressionRule::None => return "No automatic progression".to_owned(),
        ProgressionRule::AddWhenTopOfRange { increment, .. } => format!(
            "+{} when every set hits its reps",
            program_weight_text(*increment, unit)
        ),
        ProgressionRule::DoubleProgression { increment, .. } => format!(
            "Double progression: add reps, then +{}",
            program_weight_text(*increment, unit)
        ),
        ProgressionRule::TrainingMax { increment, .. } => format!(
            "Training max +{} when every set hits its reps",
            program_weight_text(*increment, unit)
        ),
    };
    match rule.deload() {
        Some(deload) => format!(
            "{main}; −{} after {} failed sessions",
            deload.percent, deload.failures
        ),
        None => main,
    }
}

/// `2026-10-03`: the UTC date of a timestamp.
#[must_use]
pub fn date_text(at: Timestamp) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let days = at.epoch_millis().div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Where a problem is: its JSON path, its line and column, or the document itself.
#[must_use]
pub fn problem_place(problem: &ProgramProblem) -> String {
    let position = match (problem.line, problem.column) {
        (Some(line), Some(column)) => Some(format!("line {line}, column {column}")),
        (Some(line), None) => Some(format!("line {line}")),
        _ => None,
    };
    match (problem.path.is_empty(), position) {
        (false, Some(position)) => format!("{} ({position})", problem.path),
        (false, None) => problem.path.clone(),
        (true, Some(position)) => position,
        (true, None) => "Document".to_owned(),
    }
}

/// `256 KiB`.
#[must_use]
pub fn size_text(bytes: usize) -> String {
    format!("{} KiB", bytes / 1024)
}

/// The message for a file over [`MAX_DOCUMENT_BYTES`].
#[must_use]
pub fn too_large_message() -> String {
    format!(
        "This file is too large: a program.json can be at most {}.",
        size_text(MAX_DOCUMENT_BYTES)
    )
}

/// Why an action failed, as the screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionFailure {
    /// The document breaks rules: listed one by one.
    Problems(ProgramProblems),
    /// A plan limit (403): its message, with a link to the plan.
    Plan(String),
    /// Anything else: the shared banner.
    Other(String),
}

/// Classifies a failed copy, upload, activation or archive.
#[must_use]
pub fn action_failure(error: &ServerFnError) -> ActionFailure {
    if let Some(problems) = ProgramProblems::from_error(error) {
        return ActionFailure::Problems(problems);
    }
    let failure = ApiFailure::classify(error);
    let code = match error {
        ServerFnError::ServerError { code, .. }
        | ServerFnError::Request(dioxus::fullstack::RequestError::Status(_, code)) => Some(*code),
        _ => None,
    };
    match (code, failure.kind) {
        // A 413 may come from the transport without our message.
        (Some(413), _) => ActionFailure::Other(too_large_message()),
        (_, FailureKind::Forbidden) => ActionFailure::Plan(failure.message),
        _ => ActionFailure::Other(failure.message),
    }
}

/// The id to send for a create keyed by `key` (a built-in id, an uploaded document): the one of
/// the attempt still pending for the same key, else a new one.
#[must_use]
pub fn creation_id_for<K: PartialEq>(pending: Option<&(K, CreationId)>, key: &K) -> CreationId {
    match pending {
        Some((pending_key, id)) if pending_key == key => *id,
        _ => CreationId::new_v7(),
    }
}

/// What the upload card says about the plan: whether uploading is included, and the slots left.
#[must_use]
pub fn upload_allowance(entitlements: &Entitlements, unarchived: usize) -> (bool, Option<String>) {
    let allowed = entitlements
        .features
        .iter()
        .any(|access| access.feature == Feature::UploadPrograms && access.allowed);
    let slots = entitlements
        .limits
        .iter()
        .find(|limit| limit.quota == Quota::CustomPrograms)
        .and_then(|limit| match limit.limit {
            Limit::AtMost { max } => Some(format!(
                "{unarchived} of {max} active programs used (archived ones don't count)."
            )),
            Limit::Unlimited => None,
        });
    (allowed, slots)
}

/// The user's programs, unarchived ones first, each group oldest first (the server's order).
#[must_use]
pub fn split_programs(programs: &[ProgramView]) -> (Vec<ProgramView>, Vec<ProgramView>) {
    programs
        .iter()
        .cloned()
        .partition(|program| !program.archived)
}

// --- State ---------------------------------------------------------------------------------------

/// What the screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Screen {
    List,
    Builtin(BuiltinProgramId),
    Mine(ProgramId),
}

/// The screen's shared state. `Copy`, so every handler can take one.
#[derive(Clone, Copy, PartialEq)]
struct Programs {
    screen: Signal<Screen>,
    /// Bumped after every change, so the lists and details reload.
    generation: Signal<u64>,
    /// A plan limit (403) to show with a link to the plan.
    plan_notice: Signal<Option<String>>,
    /// The problems of the last refused upload.
    problems: Signal<Option<ProgramProblems>>,
    /// A copy whose answer was lost: retried with the same id.
    pending_copy: Signal<Option<(BuiltinProgramId, CreationId)>>,
    /// A new-program upload whose answer was lost: retried with the same id.
    pending_upload: Signal<Option<(String, CreationId)>>,
    busy: Signal<bool>,
    errors: Errors,
}

impl Programs {
    fn open(mut self, screen: Screen) {
        self.plan_notice.set(None);
        self.problems.set(None);
        self.screen.set(screen);
        scroll_to_top();
    }

    fn changed(mut self) {
        let next = *self.generation.peek() + 1;
        self.generation.set(next);
    }

    /// Starts an action, unless one is running.
    fn start(mut self) -> bool {
        if *self.busy.peek() {
            return false;
        }
        self.busy.set(true);
        self.plan_notice.set(None);
        self.problems.set(None);
        true
    }

    fn done(mut self) {
        self.busy.set(false);
    }

    /// Shows why an action failed.
    fn fail(mut self, error: &ServerFnError) {
        let errors_ = self.errors;
        match action_failure(error) {
            ActionFailure::Problems(problems) => self.problems.set(Some(problems)),
            ActionFailure::Plan(message) => self.plan_notice.set(Some(message)),
            ActionFailure::Other(message) if error_is_413(error) => {
                errors_.show(BannerKind::Error, message);
            }
            ActionFailure::Other(_) => errors_.report(error),
        }
    }
}

fn error_is_413(error: &ServerFnError) -> bool {
    matches!(
        error,
        ServerFnError::ServerError { code: 413, .. }
            | ServerFnError::Request(dioxus::fullstack::RequestError::Status(_, 413))
    )
}

fn scroll_to_top() {
    #[cfg(feature = "web")]
    if let Some(window) = web_sys::window() {
        window.scroll_to_with_x_and_y(0.0, 0.0);
    }
}

// --- Screen --------------------------------------------------------------------------------------

/// The Programs page.
#[component]
pub fn ProgramsPage() -> Element {
    let state = Programs {
        screen: use_signal(|| Screen::List),
        generation: use_signal(|| 0),
        plan_notice: use_signal(|| None),
        problems: use_signal(|| None),
        pending_copy: use_signal(|| None),
        pending_upload: use_signal(|| None),
        busy: use_signal(|| false),
        errors: use_errors(),
    };
    let screen = state.screen.read().clone();
    match screen {
        Screen::List => rsx! { ProgramList { state } },
        Screen::Builtin(id) => rsx! { BuiltinDetail { state, id } },
        Screen::Mine(id) => rsx! { MineDetail { state, id } },
    }
}

/// A 403 from a plan limit, with a link to the plan.
#[component]
fn PlanNotice(state: Programs) -> Element {
    let Some(message) = state.plan_notice.read().clone() else {
        return rsx! {};
    };
    rsx! {
        div { class: "io-notice io-notice-error io-plan-notice", role: "alert",
            p { "{message}" }
            Link { to: Route::Settings {}, "See your plan in Settings" }
        }
    }
}

/// The problems of a refused upload, one per line, as plain text.
#[component]
fn ProblemList(state: Programs) -> Element {
    let Some(problems) = state.problems.read().clone() else {
        return rsx! {};
    };
    rsx! {
        div { class: "io-notice io-notice-error io-problems", role: "alert",
            p { "This program is not valid:" }
            ul {
                for (index, problem) in problems.errors.iter().enumerate() {
                    li { key: "{index}",
                        span { class: "io-problem-place", "{problem_place(problem)}" }
                        " "
                        span { "{problem.message}" }
                    }
                }
            }
            if problems.omitted > 0 {
                p { "…and {problems.omitted} more." }
            }
        }
    }
}

#[component]
fn ProgramList(state: Programs) -> Element {
    let errors = use_errors();
    let mut show_archived = use_signal(|| false);
    let data = use_resource(move || async move {
        let _ = state.generation.read();
        let loaded = async {
            let mine = list_programs(true).await?;
            let active = get_active_program().await?;
            let builtins = list_builtin_programs().await?;
            Ok::<_, ServerFnError>((mine, active, builtins))
        }
        .await;
        if let Err(error) = &loaded {
            errors.report(error);
        }
        loaded.ok()
    });
    let entitlements = use_resource(move || async move {
        let _ = state.generation.read();
        my_entitlements().await.ok()
    });

    let header = rsx! {
        div { class: "io-page-header",
            h1 { class: "io-title", "Programs" }
        }
    };
    let loaded = data.read().clone();
    let Some(loaded) = loaded else {
        return rsx! { {header} LoadingState { message: "Loading your programs…" } };
    };
    let Some((mine, active, builtins)) = loaded else {
        let mut data = data;
        return rsx! {
            {header}
            EmptyState { title: "Not loaded", message: "Your programs could not be loaded.",
                Button { variant: ButtonVariant::Secondary, onclick: move |_| data.restart(), "Try again" }
            }
        };
    };
    let active_id = active.as_ref().map(|detail| detail.program.id);
    let (current, archived) = split_programs(&mine);
    let allowance = entitlements
        .read()
        .clone()
        .flatten()
        .map(|entitlements| upload_allowance(&entitlements, current.len()));

    rsx! {
        {header}
        PlanNotice { state }
        match &active {
            Some(detail) => rsx! {
                section { class: "io-card io-active", aria_labelledby: "active-title",
                    span { class: "io-label", "Training with" }
                    h2 { id: "active-title", "{detail.program.name}" }
                    p { class: "io-muted",
                        "{detail.document.days.len()} days · version {detail.version.version}"
                    }
                    Button {
                        variant: ButtonVariant::Secondary,
                        onclick: {
                            let id = detail.program.id;
                            move |_| state.open(Screen::Mine(id))
                        },
                        "Details"
                    }
                }
            },
            None => rsx! {
                Card { title: "No active program",
                    p { class: "io-muted",
                        "Copy a built-in program below, or upload your own, then make it active."
                    }
                }
            },
        }

        Card { title: "Your programs",
            if current.is_empty() {
                p { class: "io-muted", "None yet." }
            }
            ul { class: "io-list",
                for program in current {
                    ProgramRow { key: "{program.id}", state, program: program.clone(), active: Some(program.id) == active_id }
                }
            }
            if !archived.is_empty() {
                Chip {
                    selected: *show_archived.read(),
                    onclick: move |_| {
                        let shown = *show_archived.peek();
                        show_archived.set(!shown);
                    },
                    "Archived ({archived.len()})"
                }
                if *show_archived.read() {
                    ul { class: "io-list",
                        for program in archived {
                            ProgramRow { key: "{program.id}", state, program: program.clone(), active: false }
                        }
                    }
                }
            }
        }

        UploadCard { state, allowance, target: None }

        Card { title: "Built-in programs",
            ul { class: "io-list",
                for builtin in builtins {
                    BuiltinRow { key: "{builtin.builtin_id}", state, builtin: builtin.clone() }
                }
            }
        }
    }
}

#[component]
fn ProgramRow(state: Programs, program: ProgramView, active: bool) -> Element {
    let id = program.id;
    let origin = if program.source_builtin_id.is_some() {
        "Copied from a built-in"
    } else {
        "Uploaded"
    };
    rsx! {
        li { class: "io-row",
            div { class: "io-row-main",
                span { class: "io-row-title",
                    "{program.name}"
                    if active {
                        span { class: "io-badge", "active" }
                    }
                    if program.archived {
                        span { class: "io-badge io-badge-muted", "archived" }
                    }
                }
                span { class: "io-muted io-row-meta", "{origin} · {date_text(program.created_at)}" }
            }
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| state.open(Screen::Mine(id)),
                "Open"
            }
        }
    }
}

#[component]
fn BuiltinRow(state: Programs, builtin: BuiltinProgramView) -> Element {
    let id = builtin.builtin_id.clone();
    rsx! {
        li { class: "io-row",
            div { class: "io-row-main",
                span { class: "io-row-title", "{builtin.name}" }
                span { class: "io-muted io-row-meta",
                    "{builtin.document.days.len()} days · version {builtin.version}"
                }
            }
            Button {
                variant: ButtonVariant::Secondary,
                onclick: move |_| state.open(Screen::Builtin(id.clone())),
                "Open"
            }
        }
    }
}

/// Uploading a `program.json`: as a new program (`target: None`) or as a new version of one.
#[component]
fn UploadCard(
    state: Programs,
    allowance: Option<(bool, Option<String>)>,
    target: Option<ProgramId>,
) -> Element {
    // Re-keys the file input after each pick, so picking the same file again works.
    let mut picks = use_signal(|| 0_u32);
    let allowed = allowance.as_ref().is_none_or(|(allowed, _)| *allowed);
    let slots = allowance.and_then(|(_, slots)| slots);
    let busy = *state.busy.read();
    let input_id = if target.is_some() {
        "upload-version"
    } else {
        "upload-program"
    };

    let on_pick = move |event: FormEvent| {
        let Some(file) = event.files().into_iter().next() else {
            return;
        };
        let next = *picks.peek() + 1;
        picks.set(next);
        if usize::try_from(file.size()).map_or(true, |size| size > MAX_DOCUMENT_BYTES) {
            state.errors.show(BannerKind::Error, too_large_message());
            return;
        }
        if !state.start() {
            return;
        }
        spawn(async move {
            let document = match file.read_string().await {
                Ok(document) => document,
                Err(_) => {
                    state.errors.show(
                        BannerKind::Error,
                        "This file could not be read. Is it a text file?",
                    );
                    state.done();
                    return;
                }
            };
            upload(state, target, document).await;
            state.done();
        });
    };

    let (title, intro) = match target {
        None => (
            "Upload a program",
            "Write your own program as a program.json file and upload it.",
        ),
        Some(_) => (
            "Upload a new version",
            "Upload a changed program.json: it becomes this program's next version.",
        ),
    };
    rsx! {
        Card { title,
            p { class: "io-muted", "{intro}" }
            p { class: "io-muted io-hint",
                "The format: "
                a { href: PROGRAM_SCHEMA_URL, target: "_blank", rel: "noopener noreferrer", "program.schema.json" }
                " (at most {size_text(MAX_DOCUMENT_BYTES)})."
            }
            if allowed {
                FilePicker {
                    key: "{picks}",
                    id: input_id,
                    busy,
                    on_pick,
                }
            } else {
                p { class: "io-notice io-notice-info",
                    "Uploading programs is part of Iron Oxide Pro. "
                    Link { to: Route::Settings {}, "See your plan" }
                }
            }
            if let Some(slots) = slots {
                if target.is_none() {
                    p { class: "io-muted io-hint", "{slots}" }
                }
            }
            ProblemList { state }
        }
    }
}

/// The button that opens the file picker: a label for a visually hidden file input, so it looks
/// like every other button. Re-created (by its key) after each pick.
#[component]
fn FilePicker(id: &'static str, busy: bool, on_pick: EventHandler<FormEvent>) -> Element {
    let class = if busy {
        "io-button io-button-primary io-file-button io-file-button-busy"
    } else {
        "io-button io-button-primary io-file-button"
    };
    rsx! {
        label { class, r#for: id, "aria-busy": busy,
            if busy { "Uploading…" } else { "Choose a program.json" }
        }
        input {
            id,
            class: "io-sr-only",
            r#type: "file",
            accept: ".json,application/json",
            disabled: busy,
            onchange: move |event| on_pick.call(event),
        }
    }
}

async fn upload(state: Programs, target: Option<ProgramId>, document: String) {
    let upload_target = match target {
        Some(program_id) => UploadTarget::NewVersion { program_id },
        None => {
            let creation_id = creation_id_for(state.pending_upload.peek().as_ref(), &document);
            let mut pending = state.pending_upload;
            pending.set(Some((document.clone(), creation_id)));
            UploadTarget::NewProgram { creation_id }
        }
    };
    match upload_program(upload_target, document).await {
        Ok(outcome) => {
            let mut pending = state.pending_upload;
            pending.set(None);
            let message = match (outcome.saved, target.is_some()) {
                (false, true) => "No change: this is the same as the latest version.".to_owned(),
                (false, false) => format!(
                    "\u{201c}{}\u{201d} is already uploaded.",
                    outcome.program.name
                ),
                (true, true) => format!("Version {} uploaded.", outcome.version.version),
                (true, false) => format!("\u{201c}{}\u{201d} uploaded.", outcome.program.name),
            };
            state.errors.show(BannerKind::Info, message);
            state.changed();
            if target.is_none() {
                state.open(Screen::Mine(outcome.program.id));
            }
        }
        Err(error) => {
            // Refused for good: the next attempt is a new upload.
            if !ApiFailure::classify(&error).is_retryable() {
                let mut pending = state.pending_upload;
                pending.set(None);
            }
            state.fail(&error);
        }
    }
}

/// The days of a program: each day's exercises, their work, load and progression.
#[component]
fn ProgramDays(document: Program) -> Element {
    let unit = use_unit();
    let rotation: Vec<String> = document
        .rotation
        .iter()
        .filter_map(|id| document.days.iter().find(|day| &day.id == id))
        .map(|day| day.name.clone())
        .collect();
    rsx! {
        if let Some(description) = &document.description {
            p { class: "io-program-description", "{description}" }
        }
        p { class: "io-muted io-hint", "Rotation: {rotation.join(\" → \")}" }
        for day in document.days.iter() {
            section { key: "{day.id}", class: "io-card io-day",
                h2 { "{day.name}" }
                ul { class: "io-list",
                    for (index, exercise) in day.exercises.iter().enumerate() {
                        li { key: "{index}", class: "io-row io-exercise",
                            div { class: "io-row-main",
                                span { class: "io-row-title", "{exercise.name}" }
                                span { class: "io-exercise-work",
                                    "{work_text(exercise.work)}"
                                    if let Some(load) = load_text(exercise.load, unit) {
                                        " · {load}"
                                    }
                                    " · rest {exercise.rest.get()} s"
                                }
                                span { class: "io-muted io-row-meta", "{progression_text(&exercise.progression, unit)}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A back button to the list.
#[component]
fn BackButton(state: Programs) -> Element {
    rsx! {
        div { class: "io-back",
            Button { variant: ButtonVariant::Ghost, onclick: move |_| state.open(Screen::List), "← Programs" }
        }
    }
}

#[component]
fn BuiltinDetail(state: Programs, id: BuiltinProgramId) -> Element {
    let errors = use_errors();
    let builtins = use_resource(move || async move {
        let result = list_builtin_programs().await;
        if let Err(error) = &result {
            errors.report(error);
        }
        result.ok()
    });
    let found = builtins
        .read()
        .clone()
        .map(|list| list.and_then(|list| list.into_iter().find(|b| b.builtin_id == id)));
    let busy = *state.busy.read();
    let builtin = match found {
        None => return rsx! { BackButton { state } LoadingState {} },
        Some(None) => {
            return rsx! {
                BackButton { state }
                EmptyState { title: "Not found", message: "This built-in program could not be loaded." }
            };
        }
        Some(Some(builtin)) => builtin,
    };
    let copy_id = builtin.builtin_id.clone();
    let copy = move |_| {
        if !state.start() {
            return;
        }
        let builtin_id = copy_id.clone();
        spawn(async move {
            let creation_id = creation_id_for(state.pending_copy.peek().as_ref(), &builtin_id);
            let mut pending = state.pending_copy;
            pending.set(Some((builtin_id.clone(), creation_id)));
            match copy_builtin_program(builtin_id.as_str().to_owned(), creation_id).await {
                Ok(detail) => {
                    pending.set(None);
                    state.errors.show(
                        BannerKind::Info,
                        format!(
                            "\u{201c}{}\u{201d} is now one of your programs.",
                            detail.program.name
                        ),
                    );
                    state.changed();
                    state.done();
                    state.open(Screen::Mine(detail.program.id));
                    return;
                }
                Err(error) => {
                    if !ApiFailure::classify(&error).is_retryable() {
                        pending.set(None);
                    }
                    state.fail(&error);
                }
            }
            state.done();
        });
    };
    rsx! {
        BackButton { state }
        div { class: "io-page-header",
            span { class: "io-label", "Built-in · version {builtin.version}" }
            h1 { class: "io-title", "{builtin.name}" }
        }
        PlanNotice { state }
        div { class: "io-actions",
            Button { busy, block: true, onclick: copy, if busy { "Copying…" } else { "Copy to my programs" } }
            p { class: "io-muted io-hint",
                "You train with your own copy, which you can make active and change."
            }
        }
        ProgramDays { document: builtin.document.clone() }
    }
}

/// What the details of one of the user's programs need.
#[derive(Debug, Clone, PartialEq)]
struct MineData {
    detail: ProgramDetail,
    versions: Vec<VersionView>,
    active: bool,
}

#[component]
fn MineDetail(state: Programs, id: ProgramId) -> Element {
    let errors = use_errors();
    let data = use_resource(move || async move {
        let _ = state.generation.read();
        let loaded = async {
            let detail = get_program(id).await?;
            let versions = list_program_versions(id).await?;
            let active = get_active_program()
                .await?
                .is_some_and(|active| active.program.id == id);
            Ok::<_, ServerFnError>(MineData {
                detail,
                versions,
                active,
            })
        }
        .await;
        if let Err(error) = &loaded {
            errors.report(error);
        }
        loaded.ok()
    });
    let busy = *state.busy.read();
    let loaded = data.read().clone();
    let data_ = match loaded {
        None => return rsx! { BackButton { state } LoadingState {} },
        Some(None) => {
            return rsx! {
                BackButton { state }
                EmptyState { title: "Not found", message: "This program could not be loaded." }
            };
        }
        Some(Some(data_)) => data_,
    };
    let program = data_.detail.program.clone();
    let archived = program.archived;
    let active = data_.active;

    let activate = move |_| {
        if !state.start() {
            return;
        }
        spawn(async move {
            match set_active_program(id).await {
                Ok(detail) => {
                    state.errors.show(
                        BannerKind::Info,
                        format!(
                            "You now train with \u{201c}{}\u{201d}.",
                            detail.program.name
                        ),
                    );
                    state.changed();
                }
                Err(error) => state.fail(&error),
            }
            state.done();
        });
    };
    let archive = move |_| {
        if !state.start() {
            return;
        }
        spawn(async move {
            match set_program_archived(id, !archived).await {
                Ok(()) => {
                    let message = if archived {
                        "Program restored."
                    } else {
                        "Program archived. Its history is kept."
                    };
                    state.errors.show(BannerKind::Info, message);
                    state.changed();
                }
                Err(error) => state.fail(&error),
            }
            state.done();
        });
    };

    let label = if active {
        "Active"
    } else if archived {
        "Archived"
    } else {
        "Your program"
    };
    rsx! {
        BackButton { state }
        div { class: "io-page-header",
            span { class: "io-label", "{label} · version {data_.detail.version.version}" }
            h1 { class: "io-title", "{program.name}" }
        }
        PlanNotice { state }
        div { class: "io-actions",
            if !active {
                Button {
                    block: true,
                    busy,
                    disabled: archived,
                    onclick: activate,
                    "Make active"
                }
                if archived {
                    p { class: "io-muted io-hint", "Restore this program to make it active." }
                }
            }
            Button {
                variant: ButtonVariant::Ghost,
                block: true,
                disabled: busy || active,
                onclick: archive,
                if archived { "Restore" } else { "Archive" }
            }
            if active {
                p { class: "io-muted io-hint",
                    "This is the program you train with: make another one active to archive it."
                }
            }
        }
        ProgramDays { document: data_.detail.document.clone() }
        Card { title: "Versions",
            ul { class: "io-list",
                for version in data_.versions.iter().rev() {
                    li { key: "{version.id}", class: "io-row",
                        div { class: "io-row-main",
                            span { class: "io-row-title",
                                "Version {version.version}"
                                if version.id == data_.detail.version.id {
                                    span { class: "io-badge", "latest" }
                                }
                            }
                            span { class: "io-muted io-row-meta", "{date_text(version.created_at)}" }
                        }
                    }
                }
            }
        }
        UploadCard { state, allowance: None, target: Some(id) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iron_oxide_domain::entitlements::Plan;
    use serde_json::json;

    fn program(json: &str) -> Program {
        Program::from_json(json).unwrap()
    }

    fn sample() -> Program {
        program(
            r#"{
              "schema_version": 1,
              "name": "Sample",
              "days": [
                { "id": "a", "name": "A", "exercises": [
                  { "id": "squat", "name": "Squat", "work": { "reps": { "sets": 3, "reps": 5 } },
                    "load": { "kg": 100 }, "rest": 180,
                    "progression": { "add_when_top_of_range": { "increment": { "kg": 2.5 },
                      "deload_after_failures": { "failures": 3, "percent": 10 } } } },
                  { "id": "curl", "name": "Curl", "work": { "reps": { "sets": 3, "reps": { "min": 8, "max": 12 } } },
                    "load": { "lb": 30 }, "rest": 60,
                    "progression": { "double_progression": { "increment": { "lb": 5 } } } },
                  { "id": "bench", "name": "Bench", "work": { "reps": { "sets": 3, "reps": 5 } },
                    "load": { "percent_of_training_max": 72.5 }, "rest": 120,
                    "progression": { "training_max": { "increment": { "kg": 2.5 } } } },
                  { "id": "plank", "name": "Plank", "work": { "hold": { "sets": 3, "seconds": 45 } }, "rest": 60 },
                  { "id": "sprint", "name": "Sprint", "work": { "intervals": { "work": 30, "rest": 90, "rounds": 8 } }, "rest": 120 }
                ] }
              ],
              "rotation": ["a"]
            }"#,
        )
    }

    #[test]
    fn work_reads_at_a_glance() {
        let document = sample();
        let texts: Vec<String> = document.days[0]
            .exercises
            .iter()
            .map(|exercise| work_text(exercise.work))
            .collect();
        assert_eq!(
            texts,
            [
                "3 × 5",
                "3 × 8–12",
                "3 × 5",
                "3 × 45 s hold",
                "8 rounds: 30 s on, 90 s off"
            ]
        );
    }

    #[test]
    fn loads_follow_the_users_unit() {
        let document = sample();
        let loads: Vec<Option<String>> = document.days[0]
            .exercises
            .iter()
            .map(|exercise| load_text(exercise.load, Unit::Kg))
            .collect();
        assert_eq!(
            loads,
            [
                Some("100 kg".to_owned()),
                Some("13.61 kg (30 lb)".to_owned()),
                Some("72.5% of training max".to_owned()),
                None,
                None
            ]
        );
        assert_eq!(
            load_text(document.days[0].exercises[0].load, Unit::Lb),
            Some("220.46 lb (100 kg)".to_owned())
        );
    }

    #[test]
    fn progressions_are_summarised() {
        let document = sample();
        let rules: Vec<String> = document.days[0]
            .exercises
            .iter()
            .map(|exercise| progression_text(&exercise.progression, Unit::Kg))
            .collect();
        assert_eq!(
            rules,
            [
                "+2.5 kg when every set hits its reps; −10% after 3 failed sessions",
                "Double progression: add reps, then +2.27 kg (5 lb)",
                "Training max +2.5 kg when every set hits its reps",
                "No automatic progression",
                "No automatic progression",
            ]
        );
    }

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(date_text(Timestamp::from_epoch_millis(0)), "1970-01-01");
        // 2026-10-03T02:00:00Z.
        assert_eq!(
            date_text(Timestamp::from_epoch_millis(1_791_000_000_000)),
            "2026-10-03"
        );
        // 2024-02-29 (leap day), 23:59:59.999.
        assert_eq!(
            date_text(Timestamp::from_epoch_millis(1_709_251_199_999)),
            "2024-02-29"
        );
        assert_eq!(date_text(Timestamp::from_epoch_millis(-1)), "1969-12-31");
    }

    fn problem(path: &str, line: Option<usize>, column: Option<usize>) -> ProgramProblem {
        ProgramProblem {
            path: path.to_owned(),
            message: "m".to_owned(),
            line,
            column,
        }
    }

    #[test]
    fn problems_say_where() {
        assert_eq!(
            problem_place(&problem("days[1].exercises[2].reps", None, None)),
            "days[1].exercises[2].reps"
        );
        assert_eq!(
            problem_place(&problem("", Some(2), Some(5))),
            "line 2, column 5"
        );
        assert_eq!(
            problem_place(&problem("days[0]", Some(4), Some(1))),
            "days[0] (line 4, column 1)"
        );
        assert_eq!(problem_place(&problem("", None, None)), "Document");
    }

    fn server(code: u16, message: &str, details: Option<serde_json::Value>) -> ServerFnError {
        ServerFnError::ServerError {
            message: message.to_owned(),
            code,
            details,
        }
    }

    #[test]
    fn failures_are_classified_for_the_screen() {
        let problems = ProgramProblems {
            errors: vec![problem("name", None, None)],
            omitted: 0,
        };
        let details = serde_json::to_value(&problems).unwrap();
        assert_eq!(
            action_failure(&server(422, "This program is not valid.", Some(details))),
            ActionFailure::Problems(problems)
        );
        assert_eq!(
            action_failure(&server(
                403,
                "Your plan keeps up to 10 programs. Archive one, or upgrade to Pro.",
                None
            )),
            ActionFailure::Plan(
                "Your plan keeps up to 10 programs. Archive one, or upgrade to Pro.".to_owned()
            )
        );
        // A 413, with or without our body, says how large a file may be.
        for error in [
            server(413, "Too large.", None),
            ServerFnError::Request(dioxus::fullstack::RequestError::Status(
                "Payload Too Large".to_owned(),
                413,
            )),
        ] {
            assert_eq!(
                action_failure(&error),
                ActionFailure::Other(too_large_message())
            );
            assert!(error_is_413(&error));
        }
        assert_eq!(
            action_failure(&server(409, "This program is active.", None)),
            ActionFailure::Other("This program is active.".to_owned())
        );
        // A 422 without problems is a plain message.
        assert_eq!(
            action_failure(&server(422, "Bad.", Some(json!("x")))),
            ActionFailure::Other("Bad.".to_owned())
        );
    }

    #[test]
    fn a_retry_reuses_the_pending_creation_id() {
        let first = CreationId::new_v7();
        let pending = ("doc".to_owned(), first);
        assert_eq!(creation_id_for(Some(&pending), &"doc".to_owned()), first);
        // Another document, or nothing pending: a new id.
        assert_ne!(creation_id_for(Some(&pending), &"other".to_owned()), first);
        assert_ne!(creation_id_for::<String>(None, &"doc".to_owned()), first);
    }

    #[test]
    fn the_upload_card_follows_the_plan() {
        let (allowed, slots) = upload_allowance(&Entitlements::of(Plan::Free), 3);
        assert!(allowed);
        assert_eq!(
            slots.as_deref(),
            Some("3 of 10 active programs used (archived ones don't count).")
        );
        let (allowed, slots) = upload_allowance(&Entitlements::of(Plan::Pro), 30);
        assert!(allowed);
        assert_eq!(slots, None);
        // A plan without the feature locks the upload.
        let mut locked = Entitlements::of(Plan::Free);
        for access in &mut locked.features {
            access.allowed = false;
        }
        assert!(!upload_allowance(&locked, 0).0);
    }

    #[test]
    fn archived_programs_are_listed_apart() {
        let view = |n: u128, archived: bool| ProgramView {
            id: ProgramId::from_uuid(uuid::Uuid::from_u128(n)),
            name: format!("P{n}"),
            source_builtin_id: None,
            archived,
            created_at: Timestamp::from_epoch_millis(0),
        };
        let (current, archived) = split_programs(&[view(1, false), view(2, true), view(3, false)]);
        let names = |list: &[ProgramView]| list.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&current), ["P1", "P3"]);
        assert_eq!(names(&archived), ["P2"]);
    }

    #[test]
    fn the_size_limit_is_said_in_kib() {
        assert_eq!(size_text(MAX_DOCUMENT_BYTES), "256 KiB");
        assert!(too_large_message().contains("256 KiB"));
    }
}

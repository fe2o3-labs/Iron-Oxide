//! The home screen (#27): the active program, the next day of its rotation with a preview of its
//! exercises, a big Start (or Resume) button, and the last session.
//!
//! The next day comes from the server (`get_next_session_plan`, which applies the domain's
//! rotation); starting a session goes through [`super::session::writes`].

use dioxus::prelude::*;
use iron_oxide_domain::progression::{NextTargets, SetGoal};
use iron_oxide_domain::{SessionId, SessionStatus, Unit, Weight, time::Timestamp};

use super::components::{Button, Card, EmptyState, LoadingState};
use super::errors::use_errors;
use super::session::writes;
use super::shell::Route;
use super::weight::{use_unit, weight_number, weight_text};
use crate::api::error::{ApiFailure, FailureKind};
use crate::api::history::{SessionSummary, history_page};
use crate::api::programs::{ProgramDetail, get_active_program};
use crate::api::sessions::{
    NextSessionPlan, PlannedExercise, SessionView, get_in_progress_session, get_next_session_plan,
};

/// What the home screen shows once loaded.
#[derive(Debug, Clone, PartialEq)]
enum HomeData {
    /// No active program: point to Programs.
    NoProgram,
    Ready(Box<Today>),
}

#[derive(Debug, Clone, PartialEq)]
struct Today {
    program: ProgramDetail,
    next: NextSessionPlan,
    /// The session in progress, if any: Resume instead of Start.
    in_progress: Option<SessionView>,
    /// The most recently ended session.
    last: Option<SessionSummary>,
}

async fn load() -> Result<HomeData, ServerFnError> {
    // Without an active program the next plan is a 409: ask for it only with one.
    let Some(program) = get_active_program().await? else {
        return Ok(HomeData::NoProgram);
    };
    let in_progress = get_in_progress_session()
        .await?
        .map(|session| session.session);
    let next = get_next_session_plan().await?;
    let last = history_page(None, Some(1))
        .await?
        .sessions
        .into_iter()
        .next();
    Ok(HomeData::Ready(Box::new(Today {
        program,
        next,
        in_progress,
        last,
    })))
}

/// One exercise of the preview: `"5 × 5 · 100 kg"`, `"3 × 45 s"`, `"Training max needed"`.
/// Empty when there is nothing to say (no working sets).
#[must_use]
pub fn exercise_summary(targets: &NextTargets, unit: Unit) -> String {
    let Some(targets) = targets.ready() else {
        return "Training max needed".to_owned();
    };
    let sets = &targets.working;
    let Some(first) = sets.first() else {
        return String::new();
    };
    let count = sets.len();
    let same_goal = sets.iter().all(|set| set.goal == first.goal);
    let work = match first.goal {
        _ if !same_goal => format!("{count} sets"),
        SetGoal::Reps { reps, .. } => format!("{count} × {reps}"),
        SetGoal::Hold { seconds } => format!("{count} × {} s", seconds.get()),
        SetGoal::Intervals { work, rest, rounds } => {
            format!("{rounds} × {}/{} s", work.get(), rest.get())
        }
    };
    let weights: Vec<Weight> = sets.iter().filter_map(|set| set.weight).collect();
    let load = match (weights.iter().min(), weights.iter().max()) {
        (Some(&low), Some(&high)) if low == high => Some(weight_text(high, unit)),
        (Some(&low), Some(&high)) => Some(format!(
            "{}–{}",
            weight_number(low, unit),
            weight_text(high, unit)
        )),
        _ => None,
    };
    match load {
        Some(load) => format!("{work} · {load}"),
        None => work,
    }
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The local day number of `at` (days since 1970-01-01), `offset_minutes` east of UTC.
const fn local_day(at: Timestamp, offset_minutes: i32) -> i64 {
    (at.epoch_millis() + offset_minutes as i64 * 60_000).div_euclid(DAY_MS)
}

/// The (year, month 1-12, day 1-31) of a day number (Howard Hinnant's `civil_from_days`).
const fn civil_from_days(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "month is 1 to 12"
    )]
    (year, month as usize, day)
}

/// When `then` was, seen from `now`, in the user's local time: `"today"`, `"yesterday"`,
/// `"3 days ago"` within a week, else the date: `"12 Sep"`, with the year if it is not this one.
#[must_use]
pub fn relative_day(then: Timestamp, now: Timestamp, offset_minutes: i32) -> String {
    let (then_day, today) = (
        local_day(then, offset_minutes),
        local_day(now, offset_minutes),
    );
    match today - then_day {
        0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        days @ 2..=6 => format!("{days} days ago"),
        _ => {
            let (year, month, day) = civil_from_days(then_day);
            let (this_year, _, _) = civil_from_days(today);
            let month = MONTHS[month.saturating_sub(1).min(11)];
            if year == this_year {
                format!("{day} {month}")
            } else {
                format!("{day} {month} {year}")
            }
        }
    }
}

/// The last-session line: `"Last session: yesterday · Day A · 15 sets"`.
#[must_use]
pub fn last_session_text(
    last: &SessionSummary,
    day_name: Option<&str>,
    now: Timestamp,
    offset_minutes: i32,
) -> String {
    let when = relative_day(
        last.finished_at.unwrap_or(last.started_at),
        now,
        offset_minutes,
    );
    let day = day_name.map_or_else(|| last.day_id.to_string(), str::to_owned);
    let what = match last.status {
        SessionStatus::Skipped => "skipped".to_owned(),
        SessionStatus::Abandoned => "abandoned".to_owned(),
        SessionStatus::Completed | SessionStatus::InProgress => match last.working_sets {
            1 => "1 set".to_owned(),
            sets => format!("{sets} sets"),
        },
    };
    format!("Last session: {when} · {day} · {what}")
}

/// The user's offset from UTC in minutes, east positive (the browser's time zone).
fn local_offset_minutes() -> i32 {
    #[cfg(feature = "web")]
    {
        // getTimezoneOffset() is UTC minus local time, in whole minutes.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a time zone offset is at most a few hundred minutes"
        )]
        let west = js_sys::Date::new_0().get_timezone_offset() as i32;
        -west
    }
    #[cfg(not(feature = "web"))]
    {
        0
    }
}

/// The name of the day `id` in the active program, if it still has one.
fn day_name(program: &ProgramDetail, id: &iron_oxide_domain::DayId) -> Option<String> {
    program
        .document
        .days
        .iter()
        .find(|day| &day.id == id)
        .map(|day| day.name.clone())
}

#[component]
pub fn Home() -> Element {
    let errors = use_errors();
    let mut data = use_resource(move || async move {
        let loaded = load().await;
        if let Err(error) = &loaded {
            errors.report(error);
        }
        loaded
    });

    let state = data.read().clone();
    match state {
        None => rsx! {
            LoadingState { message: "Loading your program…" }
        },
        // The error is in the banner; this offers a way to try again.
        Some(Err(_)) => rsx! {
            EmptyState { title: "Not loaded", message: "Your program could not be loaded.",
                Button { onclick: move |_| data.restart(), "Try again" }
            }
        },
        Some(Ok(HomeData::NoProgram)) => rsx! {
            EmptyState {
                title: "No program yet",
                message: "Choose a program to train with. Your next workout will show up here.",
                Link { class: "io-button io-button-primary", to: Route::Programs {}, "Choose a program" }
            }
        },
        Some(Ok(HomeData::Ready(today))) => rsx! {
            TodayView { today: *today, on_stale: move |()| data.restart() }
        },
    }
}

/// The loaded home screen. `on_stale` reloads it (another session turned out to be in progress).
#[component]
fn TodayView(today: Today, on_stale: EventHandler<()>) -> Element {
    let unit = use_unit();
    let errors = use_errors();
    let navigator = use_navigator();
    let mut busy = use_signal(|| false);

    let in_progress = today.in_progress.clone();
    let (day, status) = match &in_progress {
        Some(session) => (
            day_name(&today.program, &session.day).unwrap_or_else(|| session.day.to_string()),
            "In progress",
        ),
        None => (today.next.day_name.clone(), "Next up"),
    };
    // The preview is the next plan's, so only when it is the day shown.
    let preview: Vec<PlannedExercise> = match &in_progress {
        Some(session) if session.day != today.next.day => Vec::new(),
        _ => today.next.exercises.clone(),
    };
    let last = today.last.as_ref().map(|last| {
        let name = day_name(&today.program, &last.day_id);
        last_session_text(last, name.as_deref(), writes::now(), local_offset_minutes())
    });
    let program_name = today.program.program.name.clone();

    let start = move |_| {
        if *busy.peek() {
            return;
        }
        if in_progress.is_some() {
            navigator.push(Route::Session {});
            return;
        }
        busy.set(true);
        // A new id for each attempt; the server recognises a retry of the same attempt.
        let session_id = SessionId::new_v7();
        spawn(async move {
            match writes::start_session(session_id, writes::now()).await {
                Ok(_) => {
                    navigator.push(Route::Session {});
                }
                Err(error) => {
                    errors.report(&error);
                    // A 409: a session is already in progress (or the program changed): reload,
                    // which shows Resume.
                    if ApiFailure::classify(&error).kind == FailureKind::Conflict {
                        on_stale.call(());
                    }
                }
            }
            busy.set(false);
        });
    };

    rsx! {
        div { class: "io-page-header",
            span { class: "io-label", "{program_name}" }
            h1 { class: "io-title io-home-day", "{day}" }
            p { class: "io-muted", "{status}" }
        }
        if !preview.is_empty() {
            Card {
                ul { class: "io-list io-home-exercises", aria_label: "Exercises",
                    for planned in preview {
                        li { key: "{planned.exercise.id}", class: "io-row",
                            span { class: "io-row-title", "{planned.exercise.name}" }
                            span { class: "io-muted io-row-meta",
                                "{exercise_summary(&planned.targets, unit)}"
                            }
                        }
                    }
                }
            }
        }
        div { class: "io-actions",
            Button { xl: true, block: true, busy: busy(), onclick: start,
                if today.in_progress.is_some() { "Resume" } else { "Start" }
            }
            p { class: "io-muted io-hint io-home-last",
                {last.unwrap_or_else(|| "No session yet.".to_owned())}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use iron_oxide_domain::progression::{ExerciseTargets, SetTarget, TargetSource};
    use iron_oxide_domain::{
        DayId, ExerciseId, ProgramId, ProgramVersionId, Reps, Seconds, SessionId,
    };

    use super::*;

    fn kg(value: f64) -> Weight {
        Weight::from_kg(value).unwrap()
    }

    fn targets(working: Vec<SetTarget>) -> NextTargets {
        NextTargets::Ready(ExerciseTargets {
            exercise: "back-squat".parse::<ExerciseId>().unwrap(),
            source: TargetSource::ProgramDefault,
            warmup: Vec::new(),
            working,
            training_max: None,
            failed_sessions: 0,
            last_verdict: None,
            change: None,
        })
    }

    fn reps(count: u16, weight: Option<Weight>) -> SetTarget {
        SetTarget {
            weight,
            goal: SetGoal::Reps {
                reps: Reps::new(count),
                range: None,
            },
        }
    }

    #[test]
    fn the_preview_sums_up_the_working_sets() {
        let five = targets(vec![reps(5, Some(kg(100.0))); 5]);
        assert_eq!(exercise_summary(&five, Unit::Kg), "5 × 5 · 100 kg");
        assert_eq!(exercise_summary(&five, Unit::Lb), "5 × 5 · 220.46 lb");

        let ramp = targets(vec![reps(5, Some(kg(80.0))), reps(5, Some(kg(90.0)))]);
        assert_eq!(exercise_summary(&ramp, Unit::Kg), "2 × 5 · 80–90 kg");

        let mixed = targets(vec![reps(5, None), reps(3, None)]);
        assert_eq!(exercise_summary(&mixed, Unit::Kg), "2 sets");

        let plank = targets(vec![
            SetTarget {
                weight: None,
                goal: SetGoal::Hold {
                    seconds: Seconds::new(45)
                },
            };
            3
        ]);
        assert_eq!(exercise_summary(&plank, Unit::Kg), "3 × 45 s");

        let sprints = targets(vec![SetTarget {
            weight: None,
            goal: SetGoal::Intervals {
                work: Seconds::new(30),
                rest: Seconds::new(90),
                rounds: 8,
            },
        }]);
        assert_eq!(exercise_summary(&sprints, Unit::Kg), "8 × 30/90 s");

        assert_eq!(exercise_summary(&targets(Vec::new()), Unit::Kg), "");
        let needs = NextTargets::NeedsTrainingMax {
            exercise: "bench-press".parse::<ExerciseId>().unwrap(),
        };
        assert_eq!(exercise_summary(&needs, Unit::Kg), "Training max needed");
    }

    /// 2026-10-03 12:00 UTC.
    const NOW: Timestamp = Timestamp::from_epoch_millis(1_791_028_800_000);

    fn hours_before(hours: i64) -> Timestamp {
        Timestamp::from_epoch_millis(NOW.epoch_millis() - hours * 3_600_000)
    }

    #[test]
    fn days_are_relative_within_a_week() {
        assert_eq!(relative_day(hours_before(1), NOW, 0), "today");
        assert_eq!(relative_day(hours_before(13), NOW, 0), "yesterday");
        assert_eq!(relative_day(hours_before(24 * 3), NOW, 0), "3 days ago");
        assert_eq!(relative_day(hours_before(24 * 6), NOW, 0), "6 days ago");
        assert_eq!(relative_day(hours_before(24 * 7), NOW, 0), "26 Sep");
        assert_eq!(relative_day(hours_before(24 * 365), NOW, 0), "3 Oct 2025");
        assert_eq!(
            relative_day(Timestamp::from_epoch_millis(0), NOW, 0),
            "1 Jan 1970"
        );
    }

    #[test]
    fn days_follow_the_local_time_zone() {
        // 13 hours before noon UTC is 23:00 UTC yesterday, but 01:00 today at UTC+2.
        assert_eq!(relative_day(hours_before(13), NOW, 120), "today");
        // 11 hours before is 01:00 UTC today, but 20:00 yesterday at UTC−5.
        assert_eq!(relative_day(hours_before(11), NOW, -300), "yesterday");
        assert_eq!(relative_day(hours_before(11), NOW, 0), "today");
    }

    #[test]
    fn leap_days_and_month_ends_are_dated() {
        // 2024-02-29 12:00 UTC.
        let leap = Timestamp::from_epoch_millis(1_709_208_000_000);
        assert_eq!(relative_day(leap, NOW, 0), "29 Feb 2024");
        // 2026-03-31 12:00 UTC.
        let march = Timestamp::from_epoch_millis(1_774_958_400_000);
        assert_eq!(relative_day(march, NOW, 0), "31 Mar");
    }

    fn summary(status: SessionStatus, working_sets: u32) -> SessionSummary {
        SessionSummary {
            id: SessionId::new_v7(),
            program_id: ProgramId::new_v7(),
            program_name: "Full body".to_owned(),
            program_version_id: ProgramVersionId::new_v7(),
            program_version: 1,
            day_id: "a".parse::<DayId>().unwrap(),
            status,
            started_at: hours_before(26),
            finished_at: Some(hours_before(25)),
            working_sets,
        }
    }

    #[test]
    fn the_last_session_line_says_when_which_day_and_how_much() {
        let done = summary(SessionStatus::Completed, 15);
        assert_eq!(
            last_session_text(&done, Some("Day A"), NOW, 0),
            "Last session: yesterday · Day A · 15 sets"
        );
        assert_eq!(
            last_session_text(&summary(SessionStatus::Completed, 1), None, NOW, 0),
            "Last session: yesterday · a · 1 set"
        );
        assert_eq!(
            last_session_text(&summary(SessionStatus::Skipped, 0), Some("Day A"), NOW, 0),
            "Last session: yesterday · Day A · skipped"
        );
        assert_eq!(
            last_session_text(&summary(SessionStatus::Abandoned, 3), Some("Day A"), NOW, 0),
            "Last session: yesterday · Day A · abandoned"
        );
    }
}

//! Limits on a program document. Uploaded programs are untrusted input: these bound what the
//! app parses, stores and shows, and catch typos such as `"rest": 9000`.
//!
//! [`Program::from_json`](super::Program::from_json) and
//! [`Program::validate`](super::Program::validate) enforce them, and `program.schema.json` is
//! generated from them, so the editor and the app agree.

/// Largest document [`Program::from_json`](super::Program::from_json) reads, in bytes (256 KiB),
/// checked before parsing. A server can use it to cap the request body too. The built-in program
/// is about 9 KiB. It binds before the other limits for extreme programs (420 exercises with
/// 2 000-character notes would not fit), which the JSON Schema cannot express.
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
/// Most validation errors reported; the rest are counted in
/// [`ValidationErrors::omitted`](super::ValidationErrors::omitted).
pub const MAX_REPORTED_ERRORS: usize = 100;
/// Longest user value repeated in an error message, in characters. Longer ones end with `…`.
pub const MAX_ECHOED_CHARS: usize = 64;

/// Longest program, day or exercise name, in characters.
pub const MAX_NAME_CHARS: usize = 100;
/// Longest program description or exercise notes, in characters.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// Most days in a program.
pub const MAX_DAYS: usize = 14;
/// Longest rotation: each day appears at most once, so no more entries than days.
pub const MAX_ROTATION: usize = MAX_DAYS;
/// Most exercises in a day.
pub const MAX_EXERCISES_PER_DAY: usize = 30;
/// Most working sets (or holds) of an exercise.
pub const MAX_SETS: u16 = 20;
/// Most reps in a set (working or warm-up).
pub const MAX_REPS: u16 = 100;
/// Longest rest, hold, or interval phase, in seconds (one hour).
pub const MAX_SECONDS: u32 = 3_600;
/// Most interval rounds.
pub const MAX_ROUNDS: u16 = 100;
/// Most lines in a warm-up.
pub const MAX_WARMUP_LINES: usize = 10;
/// Most sets in one warm-up line.
pub const MAX_WARMUP_SETS: u16 = 10;
/// Most failed sessions a deload can wait for.
pub const MAX_DELOAD_FAILURES: u16 = 10;

/// Largest load as a percentage of the training max (exclusive of 0).
pub const MAX_PERCENT_OF_TRAINING_MAX: u32 = 150;
/// A warm-up in percent of the working weight is above 0 % and below this.
pub const WARMUP_PERCENT_BELOW: u32 = 100;
/// Largest deload, in percent (exclusive of 0).
pub const MAX_DELOAD_PERCENT: u32 = 50;
/// Largest progression increment in kilograms.
pub const MAX_INCREMENT_KG: u32 = 20;
/// Largest progression increment in pounds.
pub const MAX_INCREMENT_LB: u32 = 45;
/// Heaviest weight in kilograms: [`Weight::MAX`](crate::Weight::MAX).
pub const MAX_WEIGHT_KG: u32 = 2_000;

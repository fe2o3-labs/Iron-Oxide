//! Errors for program documents, each pointing at the JSON path it is about.

use std::borrow::Cow;
use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::limits::{MAX_ECHOED_CHARS, MAX_REPORTED_ERRORS};
use super::values::UnitWeight;
use crate::{Percent, Reps, Unit};

/// Longest parse error message, in characters. serde's messages repeat the offending key or
/// value, which an upload controls.
const MAX_MESSAGE_CHARS: usize = 512;

/// `text` cut to `max` characters, with `…` when something was cut.
fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or_default()),
    }
}

/// A user value as repeated in an error message: at most
/// [`MAX_ECHOED_CHARS`](super::limits::MAX_ECHOED_CHARS) characters.
pub(crate) fn echo(value: &str) -> String {
    shorten(value, MAX_ECHOED_CHARS)
}

/// One step of a [`JsonPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathSegment {
    /// An object key.
    Key(Cow<'static, str>),
    /// An array index.
    Index(usize),
}

/// Where a value sits in a program document, displayed as `days[1].exercises[2].reps`.
///
/// Parse errors and validation errors use the same format, so the UI can show both the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct JsonPath(Vec<PathSegment>);

impl JsonPath {
    /// The document itself.
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// This path followed by an object key.
    #[must_use]
    pub fn key(&self, key: impl Into<Cow<'static, str>>) -> Self {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Key(key.into()));
        Self(segments)
    }

    /// This path followed by an array index.
    #[must_use]
    pub fn index(&self, index: usize) -> Self {
        let mut segments = self.0.clone();
        segments.push(PathSegment::Index(index));
        Self(segments)
    }

    /// The steps from the root.
    #[must_use]
    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    /// Whether this is the document itself.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&serde_path_to_error::Path> for JsonPath {
    fn from(path: &serde_path_to_error::Path) -> Self {
        use serde_path_to_error::Segment;
        Self(
            path.iter()
                .filter_map(|segment| match segment {
                    Segment::Seq { index } => Some(PathSegment::Index(*index)),
                    Segment::Map { key } => Some(PathSegment::Key(Cow::Owned(echo(key)))),
                    Segment::Enum { variant } => Some(PathSegment::Key(Cow::Owned(echo(variant)))),
                    Segment::Unknown => None,
                })
                .collect(),
        )
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, segment) in self.0.iter().enumerate() {
            match segment {
                PathSegment::Key(key) if position == 0 => f.write_str(key)?,
                PathSegment::Key(key) => write!(f, ".{key}")?,
                PathSegment::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}

impl Serialize for JsonPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// What is wrong with a value that parsed but breaks a program rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ValidationErrorKind {
    /// The document format version is not one this app reads.
    #[error("unsupported schema_version {found} (this app reads version {supported})")]
    UnsupportedSchemaVersion {
        /// The version in the document.
        found: u64,
        /// The version this app reads.
        supported: u32,
    },
    /// A text is empty or only whitespace.
    #[error("must not be blank")]
    Blank,
    /// A text is too long.
    #[error("must be at most {max} characters (got {len})")]
    TooLong {
        /// The limit.
        max: usize,
        /// The actual length, in characters.
        len: usize,
    },
    /// A list is empty.
    #[error("must contain at least one {item}")]
    Empty {
        /// What the list holds (`day`, `exercise`).
        item: &'static str,
    },
    /// A list is too long.
    #[error("must contain at most {max} {items} (got {len})")]
    TooMany {
        /// What the list holds, plural (`days`).
        items: &'static str,
        /// The limit.
        max: usize,
        /// The actual length.
        len: usize,
    },
    /// A count or duration is outside its allowed range.
    #[error("must be between {min} and {max} (got {value})")]
    OutOfRange {
        /// The rejected value.
        value: u64,
        /// The smallest accepted value.
        min: u64,
        /// The largest accepted value.
        max: u64,
    },
    /// A percentage is outside its allowed range.
    #[error("must be {range} (got {value})")]
    PercentOutOfRange {
        /// The rejected value.
        value: Percent,
        /// The accepted range in words, e.g. `above 0% and at most 150%`.
        range: &'static str,
    },
    /// A weight is zero where it must not be.
    #[error("must be greater than zero")]
    ZeroWeight,
    /// Two days share an id.
    #[error("duplicate day id `{id}` (already used at {first})")]
    DuplicateDayId {
        /// The repeated id.
        id: String,
        /// Where it was first used.
        first: JsonPath,
    },
    /// The rotation names a day that does not exist.
    #[error("unknown day `{id}`")]
    UnknownDay {
        /// The missing day id.
        id: String,
    },
    /// A day appears twice in the rotation.
    #[error("day `{id}` appears more than once in the rotation (first at {first})")]
    DuplicateRotationDay {
        /// The repeated day id.
        id: String,
        /// Its first occurrence.
        first: JsonPath,
    },
    /// A day never appears in the rotation, so it would never be trained.
    #[error("day `{id}` is not in the rotation")]
    DayNotInRotation {
        /// The unused day.
        id: String,
    },
    /// The same exercise appears twice in a day.
    #[error("exercise `{id}` already appears in this day at {first}")]
    DuplicateExercise {
        /// The repeated exercise id.
        id: String,
        /// Its first occurrence.
        first: JsonPath,
    },
    /// The same exercise id is described differently on two days.
    #[error("exercise `{id}` must have the same {field} everywhere (see {first})")]
    InconsistentExercise {
        /// The exercise id.
        id: String,
        /// What differs: `name`, `progression` or `kind of load`.
        field: &'static str,
        /// The first occurrence, which the others must match.
        first: JsonPath,
    },
    /// The bottom of a rep range is above its top.
    #[error("min {min} is greater than max {max}")]
    RepRangeInverted {
        /// The bottom of the range.
        min: Reps,
        /// The top of the range.
        max: Reps,
    },
    /// A fixed warm-up weight is not lighter than the working weight.
    #[error("warm-up load {warmup} must be lighter than the working load {working}")]
    WarmupNotLighter {
        /// The warm-up weight.
        warmup: UnitWeight,
        /// The working weight.
        working: UnitWeight,
    },
    /// Warm-ups on a hold or on intervals.
    #[error("warm-up sets need sets of reps, not timed work")]
    WarmupOnTimedWork,
    /// A warm-up in percent of the working weight, on an exercise without a load.
    #[error("percent_of_working_weight needs a working load on the exercise")]
    WarmupNeedsWorkingLoad,
    /// A rule that adds weight, on an exercise without a fixed weight.
    #[error("`{rule}` needs a kg or lb load")]
    ProgressionNeedsWeightLoad {
        /// The rule's JSON name.
        rule: &'static str,
    },
    /// `training_max` on an exercise whose load is not a percentage of the training max.
    #[error("`training_max` needs a percent_of_training_max load")]
    ProgressionNeedsTrainingMaxLoad,
    /// A rule that climbs a rep range, on a fixed rep count.
    #[error(r#"`{rule}` needs a rep range such as {{"min": 8, "max": 12}}"#)]
    ProgressionNeedsRepRange {
        /// The rule's JSON name.
        rule: &'static str,
    },
    /// A progression rule on a hold or on intervals.
    #[error("timed work cannot use `{rule}`; use \"none\"")]
    ProgressionOnTimedWork {
        /// The rule's JSON name.
        rule: &'static str,
    },
    /// The increment and the load are written in different units.
    #[error("the increment is in {increment} but the load is in {load}")]
    IncrementUnitMismatch {
        /// The increment's unit.
        increment: Unit,
        /// The load's unit.
        load: Unit,
    },
    /// Members of a superset are not next to each other.
    #[error("superset `{id}` must be next to the other exercises of the group")]
    SupersetNotContiguous {
        /// The superset label.
        id: String,
    },
    /// A superset with a single exercise.
    #[error("superset `{id}` needs at least two exercises")]
    SupersetTooSmall {
        /// The superset label.
        id: String,
    },
    /// Members of a superset have different set counts, so they cannot alternate.
    #[error(
        "superset `{id}` needs the same number of sets for every exercise ({sets} here, {expected} at {first})"
    )]
    SupersetSetsMismatch {
        /// The superset label.
        id: String,
        /// This member's sets.
        sets: u16,
        /// The first member's sets.
        expected: u16,
        /// The first member.
        first: JsonPath,
    },
    /// Intervals inside a superset.
    #[error("intervals cannot be part of a superset")]
    SupersetWithIntervals,
    /// A percent-of-training-max load on a hold or on intervals.
    #[error("timed work cannot use a percent_of_training_max load")]
    TrainingMaxOnTimedWork,
    /// A progression increment above its limit.
    #[error("must be at most {max} {unit}")]
    IncrementTooLarge {
        /// The limit, in `unit`.
        max: u32,
        /// The increment's unit.
        unit: Unit,
    },
}

/// A rule broken by a program that parsed, with the JSON path of the offending value.
///
/// Serializes as `{"path": "days[1].exercises[2].work.reps.reps", "message": "min 12 is greater than max 8"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub struct ValidationError {
    /// Where the problem is.
    pub path: JsonPath,
    /// What the problem is.
    pub kind: ValidationErrorKind,
}

impl ValidationError {
    /// Builds an error.
    #[must_use]
    pub const fn new(path: JsonPath, kind: ValidationErrorKind) -> Self {
        Self { path, kind }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_root() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.path, self.kind)
        }
    }
}

impl Serialize for ValidationError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("ValidationError", 2)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("message", &self.kind.to_string())?;
        state.end()
    }
}

/// The document is not valid JSON, or does not have the shape of a program.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, thiserror::Error)]
pub struct ParseError {
    /// Where parsing stopped, as far as it is known.
    pub path: JsonPath,
    /// What went wrong, without the position, at most 512 characters.
    pub message: String,
    /// 1-based line, or 0 when unknown.
    pub line: usize,
    /// 1-based column, or 0 when unknown.
    pub column: usize,
}

impl ParseError {
    pub(super) fn from_serde(path: JsonPath, error: &serde_json::Error) -> Self {
        let (line, column) = (error.line(), error.column());
        let full = error.to_string();
        let suffix = format!(" at line {line} column {column}");
        let message = shorten(
            full.strip_suffix(&suffix).unwrap_or(&full),
            MAX_MESSAGE_CHARS,
        );
        Self {
            path,
            message,
            line,
            column,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.path.is_root() {
            write!(f, "{}: ", self.path)?;
        }
        write!(f, "{}", self.message)?;
        if self.line > 0 {
            write!(f, " (line {}, column {})", self.line, self.column)?;
        }
        Ok(())
    }
}

/// Every rule a program breaks, up to
/// [`MAX_REPORTED_ERRORS`](super::limits::MAX_REPORTED_ERRORS); the rest are only counted. Never
/// empty when returned as an error.
///
/// Serializes as `{"errors": [{"path", "message"}, …], "omitted": 0}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, thiserror::Error)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
    omitted: usize,
}

impl ValidationErrors {
    /// Records an error, or only counts it past the limit.
    pub(super) fn push(&mut self, error: ValidationError) {
        if self.errors.len() < MAX_REPORTED_ERRORS {
            self.errors.push(error);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    /// Whether no error was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// The reported errors, in document order.
    #[must_use]
    pub fn as_slice(&self) -> &[ValidationError] {
        &self.errors
    }

    /// The reported errors, in document order.
    #[must_use]
    pub fn into_vec(self) -> Vec<ValidationError> {
        self.errors
    }

    /// How many more errors were found but not reported.
    #[must_use]
    pub const fn omitted(&self) -> usize {
        self.omitted
    }
}

impl<'a> IntoIterator for &'a ValidationErrors {
    type Item = &'a ValidationError;
    type IntoIter = std::slice::Iter<'a, ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.iter()
    }
}

impl fmt::Display for ValidationErrors {
    /// One error per line, then `… and N more errors` if some were omitted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, error) in self.errors.iter().enumerate() {
            if position > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{error}")?;
        }
        match self.omitted {
            0 => Ok(()),
            1 => f.write_str("\n… and 1 more error"),
            omitted => write!(f, "\n… and {omitted} more errors"),
        }
    }
}

/// Why a program document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
pub enum ProgramError {
    /// Not JSON, or not shaped like a program. Parsing stops at the first such error.
    #[error("{0}")]
    Parse(ParseError),
    /// A well-formed program that breaks one or more rules. All of them are listed.
    #[error("{0}")]
    Invalid(ValidationErrors),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::limits::MAX_ECHOED_CHARS;

    #[test]
    fn path_display() {
        let path = JsonPath::root()
            .key("days")
            .index(1)
            .key("exercises")
            .index(2)
            .key("reps");
        assert_eq!(path.to_string(), "days[1].exercises[2].reps");
        assert_eq!(path.segments().len(), 5);
        assert!(!path.is_root());
        assert!(JsonPath::root().is_root());
        assert_eq!(JsonPath::root().to_string(), "");
        assert_eq!(JsonPath::root().index(0).key("a").to_string(), "[0].a");
        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            r#""days[1].exercises[2].reps""#
        );
    }

    #[test]
    fn validation_error_display_and_serde() {
        let error = ValidationError::new(
            JsonPath::root().key("days").index(1),
            ValidationErrorKind::RepRangeInverted {
                min: Reps::new(12),
                max: Reps::new(8),
            },
        );
        assert_eq!(error.to_string(), "days[1]: min 12 is greater than max 8");
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"path":"days[1]","message":"min 12 is greater than max 8"}"#
        );
        let root = ValidationError::new(JsonPath::root(), ValidationErrorKind::Blank);
        assert_eq!(root.to_string(), "must not be blank");

        let mut errors = ValidationErrors::default();
        assert!(errors.is_empty());
        errors.push(error.clone());
        errors.push(root.clone());
        assert!(!errors.is_empty());
        assert_eq!(
            errors.to_string(),
            "days[1]: min 12 is greater than max 8\nmust not be blank"
        );
        assert_eq!(errors.as_slice().len(), 2);
        assert_eq!(errors.omitted(), 0);
        assert_eq!((&errors).into_iter().count(), 2);
        assert_eq!(
            ProgramError::Invalid(errors.clone()).to_string(),
            errors.to_string()
        );
        assert_eq!(
            serde_json::to_value(&errors).unwrap(),
            serde_json::json!({
                "errors": [
                    {"path": "days[1]", "message": "min 12 is greater than max 8"},
                    {"path": "", "message": "must not be blank"},
                ],
                "omitted": 0,
            })
        );
        assert_eq!(errors.into_vec(), vec![error, root]);
    }

    #[test]
    fn errors_past_the_limit_are_only_counted() {
        let blank = ValidationError::new(JsonPath::root(), ValidationErrorKind::Blank);
        let mut errors = ValidationErrors::default();
        for _ in 0..MAX_REPORTED_ERRORS {
            errors.push(blank.clone());
        }
        assert_eq!(
            (errors.as_slice().len(), errors.omitted()),
            (MAX_REPORTED_ERRORS, 0)
        );
        assert!(!errors.to_string().contains("more"));
        errors.push(blank.clone());
        assert_eq!(
            (errors.as_slice().len(), errors.omitted()),
            (MAX_REPORTED_ERRORS, 1)
        );
        assert!(
            errors
                .to_string()
                .ends_with("must not be blank\n… and 1 more error")
        );
        errors.push(blank);
        assert!(errors.to_string().ends_with("\n… and 2 more errors"));
        assert_eq!(serde_json::to_value(&errors).unwrap()["omitted"], 2);
    }

    #[test]
    fn echoed_values_and_messages_are_shortened() {
        assert_eq!(echo("short"), "short");
        let exact = "é".repeat(MAX_ECHOED_CHARS);
        assert_eq!(echo(&exact), exact);
        let long = "é".repeat(MAX_ECHOED_CHARS + 1);
        assert_eq!(echo(&long), format!("{exact}…"));
        assert_eq!(shorten("abc", 0), "…");
        assert_eq!(shorten("", 0), "");

        // A huge unknown key: the path and the message stay short.
        let key = "k".repeat(10_000);
        let json = format!(r#"{{"{key}": 1}}"#);
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Strict {}
        let deserializer = &mut serde_json::Deserializer::from_str(&json);
        let error = serde_path_to_error::deserialize::<_, Strict>(deserializer).unwrap_err();
        let parse = ParseError::from_serde(error.path().into(), error.inner());
        assert_eq!(parse.path.to_string().chars().count(), MAX_ECHOED_CHARS + 1);
        assert_eq!(parse.message.chars().count(), MAX_MESSAGE_CHARS + 1);
        assert!(parse.message.ends_with('…'));
    }

    #[test]
    fn parse_error_strips_the_position_from_the_message() {
        let err = serde_json::from_str::<u8>("\n  x").unwrap_err();
        let parse = ParseError::from_serde(JsonPath::root(), &err);
        assert_eq!(parse.message, "expected value");
        assert_eq!((parse.line, parse.column), (2, 3));
        assert_eq!(parse.to_string(), "expected value (line 2, column 3)");
        let at = ParseError::from_serde(JsonPath::root().key("name"), &err);
        assert_eq!(at.to_string(), "name: expected value (line 2, column 3)");
        let unknown = ParseError {
            path: JsonPath::root(),
            message: "boom".to_owned(),
            line: 0,
            column: 0,
        };
        assert_eq!(unknown.to_string(), "boom");
        assert_eq!(
            serde_json::to_value(&parse).unwrap(),
            serde_json::json!({"path": "", "message": "expected value", "line": 2, "column": 3})
        );
        assert_eq!(
            ProgramError::Parse(parse.clone()).to_string(),
            parse.to_string()
        );
    }
}

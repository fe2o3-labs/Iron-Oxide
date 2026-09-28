//! Values written in a program document: weights with their unit, loads, rep targets, tempo and
//! demo links. Each one checks its own format while it is parsed; rules that span several fields
//! live in [`Program::validate`](super::Program::validate).

use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::PROGRAM_SCHEMA_URL;
use super::error::echo;
use crate::{Percent, Reps, Unit, ValueError, Weight};

/// A weight together with the unit it was written in: `{"kg": 60}` or `{"lb": 135}`.
///
/// The unit is kept so the document serializes back the way it was written, and so validation
/// can tell when an increment and a load use different units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "UnitWeightRepr", into = "UnitWeightRepr")]
pub struct UnitWeight {
    weight: Weight,
    unit: Unit,
}

impl UnitWeight {
    /// Builds a weight from a number in `unit`.
    ///
    /// # Errors
    /// Whatever [`Weight::new`] rejects: NaN, infinities, negative or too large values.
    pub fn new(value: f64, unit: Unit) -> Result<Self, ValueError> {
        Ok(Self {
            weight: Weight::new(value, unit)?,
            unit,
        })
    }

    /// Pairs an exact weight with the unit it should be shown and written in.
    #[must_use]
    pub const fn from_weight(weight: Weight, unit: Unit) -> Self {
        Self { weight, unit }
    }

    /// The exact weight.
    #[must_use]
    pub const fn weight(self) -> Weight {
        self.weight
    }

    /// The unit it was written in.
    #[must_use]
    pub const fn unit(self) -> Unit {
        self.unit
    }
}

impl fmt::Display for UnitWeight {
    /// `60 kg`, `2.5 lb`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.weight.display_in(self.unit), f)
    }
}

/// The JSON shape of a [`UnitWeight`]: the unit is the key.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum UnitWeightRepr {
    /// Kilograms.
    #[cfg_attr(test, schemars(schema_with = "super::schema::increment_kg"))]
    Kg(f64),
    /// Pounds.
    #[cfg_attr(test, schemars(schema_with = "super::schema::increment_lb"))]
    Lb(f64),
}

impl TryFrom<UnitWeightRepr> for UnitWeight {
    type Error = ValueError;

    fn try_from(repr: UnitWeightRepr) -> Result<Self, Self::Error> {
        match repr {
            UnitWeightRepr::Kg(kg) => Self::new(kg, Unit::Kg),
            UnitWeightRepr::Lb(lb) => Self::new(lb, Unit::Lb),
        }
    }
}

impl From<UnitWeight> for UnitWeightRepr {
    fn from(weight: UnitWeight) -> Self {
        let value = weight.weight.value_in(weight.unit);
        match weight.unit {
            Unit::Kg => Self::Kg(value),
            Unit::Lb => Self::Lb(value),
        }
    }
}

/// The working load of an exercise.
///
/// JSON: `{"kg": 60}`, `{"lb": 135}` or `{"percent_of_training_max": 75}`. A percentage refers to
/// the training max of the exercise itself, which the lifter enters in the app: training maxes
/// are personal numbers that progress every few weeks, so they are not part of the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "LoadRepr", into = "LoadRepr")]
pub enum Load {
    /// A fixed weight.
    Weight(UnitWeight),
    /// A percentage of the exercise's training max.
    PercentOfTrainingMax(Percent),
}

impl Load {
    /// The fixed weight, if the load is one.
    #[must_use]
    pub const fn weight(self) -> Option<UnitWeight> {
        match self {
            Self::Weight(weight) => Some(weight),
            Self::PercentOfTrainingMax(_) => None,
        }
    }

    /// Whether the load is a percentage of the training max.
    #[must_use]
    pub const fn is_percent_of_training_max(self) -> bool {
        matches!(self, Self::PercentOfTrainingMax(_))
    }
}

/// The JSON shape of a [`Load`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum LoadRepr {
    /// A fixed weight in kilograms.
    #[cfg_attr(test, schemars(schema_with = "super::schema::load_kg"))]
    Kg(f64),
    /// A fixed weight in pounds.
    #[cfg_attr(test, schemars(schema_with = "super::schema::load_lb"))]
    Lb(f64),
    /// A percentage of this exercise's training max, which the lifter enters in the app.
    #[serde(deserialize_with = "super::whole::percent")]
    #[cfg_attr(test, schemars(schema_with = "super::schema::training_max_percent"))]
    PercentOfTrainingMax(Percent),
}

impl TryFrom<LoadRepr> for Load {
    type Error = ValueError;

    fn try_from(repr: LoadRepr) -> Result<Self, Self::Error> {
        match repr {
            LoadRepr::Kg(kg) => UnitWeight::new(kg, Unit::Kg).map(Self::Weight),
            LoadRepr::Lb(lb) => UnitWeight::new(lb, Unit::Lb).map(Self::Weight),
            LoadRepr::PercentOfTrainingMax(percent) => Ok(Self::PercentOfTrainingMax(percent)),
        }
    }
}

impl From<Load> for LoadRepr {
    fn from(load: Load) -> Self {
        match load {
            Load::Weight(weight) => match UnitWeightRepr::from(weight) {
                UnitWeightRepr::Kg(kg) => Self::Kg(kg),
                UnitWeightRepr::Lb(lb) => Self::Lb(lb),
            },
            Load::PercentOfTrainingMax(percent) => Self::PercentOfTrainingMax(percent),
        }
    }
}

/// The load of a warm-up set.
///
/// JSON: `{"kg": 20}` (e.g. the empty bar), `{"lb": 45}` or `{"percent_of_working_weight": 50}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "WarmupLoadRepr", into = "WarmupLoadRepr")]
pub enum WarmupLoad {
    /// A fixed weight.
    Weight(UnitWeight),
    /// A percentage of the working weight of the exercise.
    PercentOfWorkingWeight(Percent),
}

/// The JSON shape of a [`WarmupLoad`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum WarmupLoadRepr {
    /// A fixed weight in kilograms (20 for the empty bar).
    #[cfg_attr(test, schemars(schema_with = "super::schema::load_kg"))]
    Kg(f64),
    /// A fixed weight in pounds (45 for the empty bar).
    #[cfg_attr(test, schemars(schema_with = "super::schema::load_lb"))]
    Lb(f64),
    /// A percentage of the exercise's working weight, below 100.
    #[serde(deserialize_with = "super::whole::percent")]
    #[cfg_attr(test, schemars(schema_with = "super::schema::warmup_percent"))]
    PercentOfWorkingWeight(Percent),
}

impl TryFrom<WarmupLoadRepr> for WarmupLoad {
    type Error = ValueError;

    fn try_from(repr: WarmupLoadRepr) -> Result<Self, Self::Error> {
        match repr {
            WarmupLoadRepr::Kg(kg) => UnitWeight::new(kg, Unit::Kg).map(Self::Weight),
            WarmupLoadRepr::Lb(lb) => UnitWeight::new(lb, Unit::Lb).map(Self::Weight),
            WarmupLoadRepr::PercentOfWorkingWeight(percent) => {
                Ok(Self::PercentOfWorkingWeight(percent))
            }
        }
    }
}

impl From<WarmupLoad> for WarmupLoadRepr {
    fn from(load: WarmupLoad) -> Self {
        match load {
            WarmupLoad::Weight(weight) => match UnitWeightRepr::from(weight) {
                UnitWeightRepr::Kg(kg) => Self::Kg(kg),
                UnitWeightRepr::Lb(lb) => Self::Lb(lb),
            },
            WarmupLoad::PercentOfWorkingWeight(percent) => Self::PercentOfWorkingWeight(percent),
        }
    }
}

/// An inclusive rep range such as 8 to 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RepRange {
    /// The fewest reps that count as a successful set.
    #[serde(deserialize_with = "super::whole::reps")]
    #[cfg_attr(test, schemars(schema_with = "super::schema::rep_count"))]
    pub min: Reps,
    /// The top of the range.
    #[serde(deserialize_with = "super::whole::reps")]
    #[cfg_attr(test, schemars(schema_with = "super::schema::rep_count"))]
    pub max: Reps,
}

/// The reps to aim for in each working set: a fixed count (`5`) or a range
/// (`{"min": 8, "max": 12}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepTarget {
    /// Exactly this many reps.
    Fixed(Reps),
    /// Anywhere in the range.
    Range(RepRange),
}

impl RepTarget {
    /// The fewest reps that count as a successful set.
    #[must_use]
    pub const fn min(self) -> Reps {
        match self {
            Self::Fixed(reps) => reps,
            Self::Range(range) => range.min,
        }
    }

    /// The most reps asked for: the fixed count, or the top of the range.
    #[must_use]
    pub const fn max(self) -> Reps {
        match self {
            Self::Fixed(reps) => reps,
            Self::Range(range) => range.max,
        }
    }
}

impl Serialize for RepTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(reps) => reps.serialize(serializer),
            Self::Range(range) => range.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RepTarget {
    /// Accepts an integer or a `{min, max}` object. Written by hand rather than with
    /// `#[serde(untagged)]`, whose only error is "data did not match any variant".
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RepTargetVisitor;

        impl<'de> Visitor<'de> for RepTargetVisitor {
            type Value = RepTarget;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"a rep count such as 5, or a range such as {"min": 8, "max": 12}"#)
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<RepTarget, E> {
                u16::try_from(value)
                    .map(|count| RepTarget::Fixed(Reps::new(count)))
                    .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
            }

            /// An integral float such as `5.0` counts, as in JSON Schema.
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<RepTarget, E> {
                super::whole::integral(value)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Float(value), &self))
                    .and_then(|value| self.visit_u64(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<RepTarget, E> {
                u64::try_from(value)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))
                    .and_then(|value| self.visit_u64(value))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<RepTarget, A::Error> {
                RepRange::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(RepTarget::Range)
            }
        }

        deserializer.deserialize_any(RepTargetVisitor)
    }
}

/// One phase of a [`Tempo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TempoPhase {
    /// This many seconds (0 to 99).
    Seconds(u8),
    /// As fast as possible, written `X`.
    Explosive,
}

impl fmt::Display for TempoPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seconds(seconds) => write!(f, "{seconds}"),
            Self::Explosive => f.write_str("X"),
        }
    }
}

/// A tempo was not written as four phases like `3-1-X-0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
#[error(
    "invalid tempo `{value}`: expected four phases such as `3-1-X-0` \
     (each 0 to 99 seconds, or X for explosive)"
)]
pub struct InvalidTempo {
    /// The rejected input, shortened to
    /// [`MAX_ECHOED_CHARS`](super::limits::MAX_ECHOED_CHARS) characters.
    pub value: String,
}

/// A lifting tempo: eccentric, pause at the bottom, concentric, pause at the top. Written
/// `3-1-X-0`, where `X` means explosive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Tempo([TempoPhase; 4]);

impl Tempo {
    /// The lowering phase.
    #[must_use]
    pub const fn eccentric(self) -> TempoPhase {
        self.0[0]
    }

    /// The pause at the bottom.
    #[must_use]
    pub const fn bottom_pause(self) -> TempoPhase {
        self.0[1]
    }

    /// The lifting phase.
    #[must_use]
    pub const fn concentric(self) -> TempoPhase {
        self.0[2]
    }

    /// The pause at the top.
    #[must_use]
    pub const fn top_pause(self) -> TempoPhase {
        self.0[3]
    }
}

fn parse_tempo_phase(text: &str) -> Option<TempoPhase> {
    if text == "X" {
        return Some(TempoPhase::Explosive);
    }
    let digits_only =
        !text.is_empty() && text.len() <= 2 && text.bytes().all(|b| b.is_ascii_digit());
    if digits_only {
        text.parse().ok().map(TempoPhase::Seconds)
    } else {
        None
    }
}

impl FromStr for Tempo {
    type Err = InvalidTempo;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidTempo { value: echo(s) };
        let mut phases = [TempoPhase::Explosive; 4];
        let mut parts = s.split('-');
        for phase in &mut phases {
            *phase = parts
                .next()
                .and_then(parse_tempo_phase)
                .ok_or_else(invalid)?;
        }
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(Self(phases))
    }
}

impl TryFrom<String> for Tempo {
    type Error = InvalidTempo;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Tempo> for String {
    fn from(tempo: Tempo) -> Self {
        tempo.to_string()
    }
}

impl fmt::Display for Tempo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}-{b}-{c}-{d}")
    }
}

/// A demo link was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, thiserror::Error)]
#[error("invalid demo URL `{value}`: {reason}")]
pub struct InvalidDemoUrl {
    /// The rejected input, shortened to
    /// [`MAX_ECHOED_CHARS`](super::limits::MAX_ECHOED_CHARS) characters.
    pub value: String,
    /// Why it was rejected.
    pub reason: &'static str,
}

/// A link to a demonstration of the exercise (a video, an article).
///
/// Uploaded programs are untrusted and the app renders this as a link, so the check is an
/// allow-list rather than a URL parser (which would also pull IDNA tables into the wasm client):
///
/// - `https://` only. Every mainstream video and article host serves https, and a plain-http
///   link from an https app is a downgrade the lifter cannot see.
/// - Printable ASCII only, without `\`, `"`, `<`, `>`, `^`, `` ` ``, `{`, `|` or `}`. This rules
///   out whitespace, control characters and every invisible or bidirectional Unicode character
///   (U+200B, U+202E…), so the text shown is the address visited. Browsers treat `\` as `/`,
///   which would let `https://evil.example\.youtube.com` read like a YouTube link. Internationalised
///   hosts and paths are written in their ASCII form (punycode, percent-encoding), as browsers
///   copy them.
/// - A host made of letters, digits and hyphens in dot-separated labels (IP literals in brackets
///   are not accepted), with an optional port from 1 to 65535. No user name or password.
/// - At most [`DemoUrl::MAX_LEN`] characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DemoUrl(String);

impl DemoUrl {
    /// Longest accepted URL, in characters (all ASCII, so also in bytes).
    pub const MAX_LEN: usize = 2_048;

    /// Validates a URL.
    ///
    /// # Errors
    /// [`InvalidDemoUrl`] when it breaks one of the rules above.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDemoUrl> {
        let value = value.into();
        match demo_url_problem(&value) {
            None => Ok(Self(value)),
            Some(reason) => Err(InvalidDemoUrl {
                value: echo(&value),
                reason,
            }),
        }
    }

    /// The URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The characters allowed anywhere in a [`DemoUrl`]. `schema.rs` mirrors this set.
pub(super) const fn is_url_char(byte: u8) -> bool {
    matches!(byte, b'!'..=b'~')
        && !matches!(
            byte,
            b'"' | b'<' | b'>' | b'\\' | b'^' | b'`' | b'{' | b'|' | b'}'
        )
}

/// The characters allowed in a host name label. `schema.rs` builds its pattern from this.
pub(super) const fn is_host_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

/// Dot-separated labels of [`is_host_char`] characters, e.g. `www.example.com` or `1.2.3.4`.
fn is_host(host: &str) -> bool {
    host.split('.')
        .all(|label| !label.is_empty() && label.bytes().all(is_host_char))
}

/// A port a browser can connect to: 1 to 65535, without leading zeros.
fn is_port(port: &str) -> bool {
    !port.starts_with('0')
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

fn demo_url_problem(value: &str) -> Option<&'static str> {
    if !value.bytes().all(is_url_char) {
        return Some(
            "may only contain printable ASCII characters, without spaces or \\ \" < > ^ ` { | }",
        );
    }
    if value.len() > DemoUrl::MAX_LEN {
        return Some("must be at most 2048 characters");
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return Some("must start with https://");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Some("must not contain a user name or password");
    }
    let (host, port) = authority
        .rfind(':')
        .map_or((authority, ""), |colon| authority.split_at(colon));
    if !is_host(host) {
        return Some("must have a host name such as www.example.com");
    }
    if let Some(port) = port.strip_prefix(':') {
        if !is_port(port) {
            return Some("the port must be a number from 1 to 65535");
        }
    } else if !port.is_empty() {
        return Some("must have a host name such as www.example.com");
    }
    None
}

impl TryFrom<String> for DemoUrl {
    type Error = InvalidDemoUrl;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DemoUrl> for String {
    fn from(url: DemoUrl) -> Self {
        url.0
    }
}

impl AsRef<str> for DemoUrl {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DemoUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `$schema` field of a program document: only [`PROGRAM_SCHEMA_URL`] is accepted, so the
/// app never stores and serves back an arbitrary link.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SchemaUrl;

impl SchemaUrl {
    /// The URL, [`PROGRAM_SCHEMA_URL`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        PROGRAM_SCHEMA_URL
    }
}

impl Serialize for SchemaUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(PROGRAM_SCHEMA_URL)
    }
}

impl<'de> Deserialize<'de> for SchemaUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let url = String::deserialize(deserializer)?;
        if url == PROGRAM_SCHEMA_URL {
            Ok(Self)
        } else {
            Err(de::Error::custom(format_args!(
                "`$schema` must be \"{PROGRAM_SCHEMA_URL}\" or be left out (got \"{}\")",
                echo(&url)
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn kg(value: f64) -> UnitWeight {
        UnitWeight::new(value, Unit::Kg).unwrap()
    }

    fn lb(value: f64) -> UnitWeight {
        UnitWeight::new(value, Unit::Lb).unwrap()
    }

    #[test]
    fn unit_weight_reads_lb_as_pounds() {
        let weight: UnitWeight = serde_json::from_str(r#"{"lb": 135}"#).unwrap();
        assert_eq!(weight.weight(), Weight::from_lb(135.0).unwrap());
        assert_eq!(weight.unit(), Unit::Lb);
        assert_eq!(weight.to_string(), "135 lb");
        assert_eq!(serde_json::to_string(&weight).unwrap(), r#"{"lb":135.0}"#);

        let weight: UnitWeight = serde_json::from_str(r#"{"kg": 2.5}"#).unwrap();
        assert_eq!(weight, kg(2.5));
        assert_eq!(weight.to_string(), "2.5 kg");
        assert_eq!(
            UnitWeight::from_weight(Weight::from_kg(20.0).unwrap(), Unit::Lb).to_string(),
            "44.09 lb"
        );
    }

    #[test]
    fn unit_weight_rejects_bad_numbers_and_shapes() {
        let err = serde_json::from_str::<UnitWeight>(r#"{"kg": -5}"#).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("weight must not be negative (got -5)"),
            "{err}"
        );
        assert!(serde_json::from_str::<UnitWeight>(r#"{"kg": 2001}"#).is_err());
        assert!(serde_json::from_str::<UnitWeight>(r#"{"stone": 10}"#).is_err());
        assert!(serde_json::from_str::<UnitWeight>("60").is_err());
        assert!(serde_json::from_str::<UnitWeight>(r#"{"kg": 60, "lb": 5}"#).is_err());
    }

    #[test]
    fn load_shapes() {
        assert_eq!(
            serde_json::from_str::<Load>(r#"{"kg": 60}"#).unwrap(),
            Load::Weight(kg(60.0))
        );
        assert_eq!(
            serde_json::from_str::<Load>(r#"{"lb": 135}"#).unwrap(),
            Load::Weight(lb(135.0))
        );
        let tm: Load = serde_json::from_str(r#"{"percent_of_training_max": 72.5}"#).unwrap();
        assert_eq!(tm, Load::PercentOfTrainingMax(Percent::new(72.5).unwrap()));
        assert!(tm.is_percent_of_training_max());
        assert_eq!(tm.weight(), None);
        assert_eq!(Load::Weight(kg(60.0)).weight(), Some(kg(60.0)));
        assert!(!Load::Weight(kg(60.0)).is_percent_of_training_max());
        assert_eq!(
            serde_json::to_string(&tm).unwrap(),
            r#"{"percent_of_training_max":72.5}"#
        );
        assert!(serde_json::from_str::<Load>(r#"{"percent_of_training_max": -1}"#).is_err());
        assert!(serde_json::from_str::<Load>(r#"{"kg": -1}"#).is_err());
    }

    #[test]
    fn warmup_load_shapes() {
        assert_eq!(
            serde_json::from_str::<WarmupLoad>(r#"{"kg": 20}"#).unwrap(),
            WarmupLoad::Weight(kg(20.0))
        );
        assert_eq!(
            serde_json::from_str::<WarmupLoad>(r#"{"lb": 45}"#).unwrap(),
            WarmupLoad::Weight(lb(45.0))
        );
        let pct: WarmupLoad = serde_json::from_str(r#"{"percent_of_working_weight": 50}"#).unwrap();
        assert_eq!(
            pct,
            WarmupLoad::PercentOfWorkingWeight(Percent::new(50.0).unwrap())
        );
        assert_eq!(
            serde_json::to_string(&pct).unwrap(),
            r#"{"percent_of_working_weight":50.0}"#
        );
        assert!(serde_json::from_str::<WarmupLoad>(r#"{"lb": -45}"#).is_err());
        assert!(serde_json::from_str::<WarmupLoad>(r#"{"percent_of_training_max": 50}"#).is_err());
    }

    #[test]
    fn rep_target_accepts_a_count_or_a_range() {
        assert_eq!(
            serde_json::from_str::<RepTarget>("5").unwrap(),
            RepTarget::Fixed(Reps::new(5))
        );
        let range = serde_json::from_str::<RepTarget>(r#"{"min": 8, "max": 12}"#).unwrap();
        assert_eq!(
            range,
            RepTarget::Range(RepRange {
                min: Reps::new(8),
                max: Reps::new(12)
            })
        );
        assert_eq!(range.min(), Reps::new(8));
        assert_eq!(range.max(), Reps::new(12));
        assert_eq!(RepTarget::Fixed(Reps::new(5)).min(), Reps::new(5));
        assert_eq!(RepTarget::Fixed(Reps::new(5)).max(), Reps::new(5));
        assert_eq!(
            serde_json::to_string(&range).unwrap(),
            r#"{"min":8,"max":12}"#
        );
        assert_eq!(
            serde_json::to_string(&RepTarget::Fixed(Reps::new(5))).unwrap(),
            "5"
        );
    }

    #[test]
    fn rep_target_errors_are_readable() {
        let expected = r#"a rep count such as 5, or a range such as {"min": 8, "max": 12}"#;
        for input in ["-1", "65536", "2.5", "\"5\"", "[5]"] {
            let err = serde_json::from_str::<RepTarget>(input).unwrap_err();
            assert!(err.to_string().contains(expected), "{input}: {err}");
        }
        let err = serde_json::from_str::<RepTarget>(r#"{"min": 8}"#).unwrap_err();
        assert!(err.to_string().starts_with("missing field `max`"), "{err}");
        let err =
            serde_json::from_str::<RepTarget>(r#"{"min": 8, "max": 12, "avg": 10}"#).unwrap_err();
        assert!(err.to_string().starts_with("unknown field `avg`"), "{err}");
    }

    #[test]
    fn tempo_parses_and_displays() {
        let tempo: Tempo = "3-1-X-0".parse().unwrap();
        assert_eq!(tempo.eccentric(), TempoPhase::Seconds(3));
        assert_eq!(tempo.bottom_pause(), TempoPhase::Seconds(1));
        assert_eq!(tempo.concentric(), TempoPhase::Explosive);
        assert_eq!(tempo.top_pause(), TempoPhase::Seconds(0));
        assert_eq!(tempo.to_string(), "3-1-X-0");
        assert_eq!(String::from(tempo), "3-1-X-0");
        assert_eq!(
            "10-0-1-99".parse::<Tempo>().unwrap().to_string(),
            "10-0-1-99"
        );
        assert_eq!(serde_json::to_string(&tempo).unwrap(), "\"3-1-X-0\"");
        assert_eq!(serde_json::from_str::<Tempo>("\"3-1-X-0\"").unwrap(), tempo);
    }

    #[test]
    fn tempo_rejects_other_formats() {
        for bad in [
            "",
            "3-1-1",
            "3-1-1-0-0",
            "3110",
            "3-1-x-0",
            "3-1--0",
            "100-1-1-0",
            "3-1-1-0 ",
            "+3-1-1-0",
            "3.5-1-1-0",
            "a-b-c-d",
        ] {
            let err = bad.parse::<Tempo>().unwrap_err();
            assert_eq!(err.value, bad);
            assert!(
                err.to_string()
                    .starts_with(&format!("invalid tempo `{bad}`"))
            );
        }
        assert!(Tempo::try_from("3-1".to_owned()).is_err());
        assert!(serde_json::from_str::<Tempo>("\"3-1\"").is_err());
    }

    #[test]
    fn demo_url_accepts_well_formed_https() {
        for good in [
            "https://www.youtube.com/watch?v=abc&t=10s",
            "https://example.com",
            "https://example.com:8443/x#t=1",
            "https://1.2.3.4:65535/demo",
            "https://example.com:1",
            "https://xn--bcher-kva.example/%C3%A9t%C3%A9?q=[1]~_!$&'()*+,;=:@",
        ] {
            let url = DemoUrl::new(good).unwrap();
            assert_eq!(url.as_str(), good);
            assert_eq!(url.as_ref(), good);
            assert_eq!(url.to_string(), good);
            assert_eq!(String::from(url.clone()), good);
            assert_eq!(DemoUrl::try_from(good.to_owned()).unwrap(), url);
        }
        let json = serde_json::to_string(&DemoUrl::new("https://a.b").unwrap()).unwrap();
        assert_eq!(json, "\"https://a.b\"");
    }

    #[test]
    fn demo_url_rejects_everything_else() {
        const CHARS: &str =
            "may only contain printable ASCII characters, without spaces or \\ \" < > ^ ` { | }";
        const HOST: &str = "must have a host name such as www.example.com";
        const PORT: &str = "the port must be a number from 1 to 65535";
        let cases = [
            ("http://example.com", "must start with https://"),
            ("ftp://example.com", "must start with https://"),
            ("javascript:alert(1)", "must start with https://"),
            ("HTTPS://example.com", "must start with https://"),
            ("example.com", "must start with https://"),
            ("https://", HOST),
            ("https:///path", HOST),
            ("https://:80/x", HOST),
            ("https://a..b/", HOST),
            ("https://a.b./", HOST),
            ("https://a_b.c/", HOST),
            ("https://[]/", HOST),
            ("https://[::1]/", HOST),
            ("https://[1.2.3.4]/", HOST),
            ("https://[::1]:443/", HOST),
            ("https://a]b/", HOST),
            ("https://example.com:/x", PORT),
            ("https://example.com:0/x", PORT),
            ("https://example.com:080/x", PORT),
            ("https://example.com:65536/x", PORT),
            ("https://example.com:99999/x", PORT),
            ("https://example.com:123456/x", PORT),
            ("https://example.com:8a/x", PORT),
            ("https://example.com:+80/x", PORT),
            ("https://evil.example\\.youtube.com/", CHARS),
            ("https://exa mple.com", CHARS),
            ("https://example.com/\n", CHARS),
            ("https://example.com/\u{1}", CHARS),
            ("https://evil.example/\u{202e}moc.elgoog", CHARS),
            ("https://evil.example/\u{200b}", CHARS),
            ("https://ex\u{e4}mple.com/", CHARS),
            ("https://example.com/<script>", CHARS),
            ("https://example.com/\"", CHARS),
            ("https://example.com/{x}|^`", CHARS),
            (
                "https://user:pw@example.com",
                "must not contain a user name or password",
            ),
            (
                "https://youtube.com@evil.example",
                "must not contain a user name or password",
            ),
        ];
        for (bad, reason) in cases {
            let err = DemoUrl::new(bad).unwrap_err();
            assert_eq!(err.reason, reason, "{bad}");
            assert_eq!(
                err.to_string(),
                format!("invalid demo URL `{bad}`: {reason}")
            );
        }
        let long = format!("https://example.com/{}", "a".repeat(DemoUrl::MAX_LEN));
        let err = DemoUrl::new(long).unwrap_err();
        assert_eq!(err.reason, "must be at most 2048 characters");
        // The rejected value is echoed shortened.
        assert_eq!(
            err.value.chars().count(),
            crate::program::limits::MAX_ECHOED_CHARS + 1
        );
        let max = format!("https://e.com/{}", "a".repeat(DemoUrl::MAX_LEN - 14));
        assert_eq!(max.len(), DemoUrl::MAX_LEN);
        assert!(DemoUrl::new(max).is_ok());
        // Non-ASCII is rejected by its characters before its byte length is counted.
        let accents = format!("https://a.b/{}", "é".repeat(1_100));
        assert_eq!(DemoUrl::new(accents).unwrap_err().reason, CHARS);
        assert!(serde_json::from_str::<DemoUrl>("\"ftp://x\"").is_err());
    }

    #[test]
    fn host_characters() {
        for byte in 0_u8..=255 {
            let expected = byte.is_ascii_alphanumeric() || byte == b'-';
            assert_eq!(is_host_char(byte), expected, "{byte:#x}");
        }
    }

    #[test]
    fn url_characters() {
        for byte in 0_u8..=255 {
            let expected = (0x21..=0x7e).contains(&byte) && !b"\"<>\\^`{|}".contains(&byte);
            assert_eq!(is_url_char(byte), expected, "{byte:#x}");
        }
    }

    #[test]
    fn schema_url_accepts_only_the_published_url() {
        let json = format!("\"{PROGRAM_SCHEMA_URL}\"");
        assert_eq!(serde_json::from_str::<SchemaUrl>(&json).unwrap(), SchemaUrl);
        assert_eq!(serde_json::to_string(&SchemaUrl).unwrap(), json);
        assert_eq!(SchemaUrl.as_str(), PROGRAM_SCHEMA_URL);
        let err = serde_json::from_str::<SchemaUrl>("\"javascript:alert(1)\"").unwrap_err();
        assert!(
            err.to_string().starts_with(&format!(
                "`$schema` must be \"{PROGRAM_SCHEMA_URL}\" or be left out (got \"javascript:alert(1)\")"
            )),
            "{err}"
        );
        let long = format!("\"{}\"", "x".repeat(100_000));
        let err = serde_json::from_str::<SchemaUrl>(&long).unwrap_err();
        assert!(err.to_string().len() < 300, "{err}");
        assert!(serde_json::from_str::<SchemaUrl>("1").is_err());
    }

    #[test]
    fn tempo_and_slug_errors_echo_a_shortened_value() {
        let long = "9-".repeat(1_000);
        let err = long.parse::<Tempo>().unwrap_err();
        assert_eq!(
            err.value.chars().count(),
            crate::program::limits::MAX_ECHOED_CHARS + 1
        );
        assert!(err.value.ends_with('…'));
    }

    fn any_unit_weight() -> impl Strategy<Value = UnitWeight> {
        // Up to 4 decimals in lb and 6 in kg are stored exactly (see `Weight`).
        prop_oneof![
            (0_u64..=2_000_000_000).prop_map(|micro| kg(micro as f64 / 1e6)),
            (0_u64..=44_092_452).prop_map(|tenth_milli| lb(tenth_milli as f64 / 1e4)),
        ]
    }

    proptest! {
        #[test]
        fn unit_weight_round_trips(weight in any_unit_weight()) {
            let json = serde_json::to_string(&weight).unwrap();
            prop_assert_eq!(serde_json::from_str::<UnitWeight>(&json).unwrap(), weight);
            let load = Load::Weight(weight);
            let json = serde_json::to_string(&load).unwrap();
            prop_assert_eq!(serde_json::from_str::<Load>(&json).unwrap(), load);
            let warmup = WarmupLoad::Weight(weight);
            let json = serde_json::to_string(&warmup).unwrap();
            prop_assert_eq!(serde_json::from_str::<WarmupLoad>(&json).unwrap(), warmup);
        }

        #[test]
        fn tempo_round_trips(phases in proptest::array::uniform4(
            prop_oneof![Just(TempoPhase::Explosive), (0_u8..=99).prop_map(TempoPhase::Seconds)]
        )) {
            let tempo = Tempo(phases);
            prop_assert_eq!(tempo.to_string().parse::<Tempo>().unwrap(), tempo);
        }

        #[test]
        fn rep_target_round_trips(min in 0_u16.., max in 0_u16.., fixed in any::<bool>()) {
            let target = if fixed {
                RepTarget::Fixed(Reps::new(min))
            } else {
                RepTarget::Range(RepRange { min: Reps::new(min), max: Reps::new(max) })
            };
            let json = serde_json::to_string(&target).unwrap();
            prop_assert_eq!(serde_json::from_str::<RepTarget>(&json).unwrap(), target);
        }
    }
}

//! Values written in a program document: weights with their unit, loads, rep targets, tempo and
//! demo links. Each one checks its own format while it is parsed; rules that span several fields
//! live in [`Program::validate`](super::Program::validate).

use std::fmt;
use std::str::FromStr;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
    Kg(f64),
    /// Pounds.
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
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
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
    Kg(f64),
    /// A fixed weight in pounds.
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
    Lb(f64),
    /// A percentage of this exercise's training max, which the lifter enters in the app.
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
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
    Kg(f64),
    /// A fixed weight in pounds (45 for the empty bar).
    #[cfg_attr(test, schemars(schema_with = "super::schema::weight_number"))]
    Lb(f64),
    /// A percentage of the exercise's working weight, below 100.
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
    pub min: Reps,
    /// The top of the range.
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
    /// The rejected input.
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
        let invalid = || InvalidTempo {
            value: s.to_owned(),
        };
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
    /// The rejected input.
    pub value: String,
    /// Why it was rejected.
    pub reason: &'static str,
}

/// A link to a demonstration of the exercise (a video, an article): an absolute `http://` or
/// `https://` URL.
///
/// The check is deliberately strict and small instead of pulling a full URL parser (with its
/// IDNA tables) into the wasm client: lowercase `http`/`https` scheme, a non-empty host, no
/// credentials, no whitespace or control characters, at most 2 048 characters. The app only
/// ever renders it as a link.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DemoUrl(String);

impl DemoUrl {
    /// Longest accepted URL, in bytes.
    pub const MAX_LEN: usize = 2_048;

    /// Validates a URL.
    ///
    /// # Errors
    /// [`InvalidDemoUrl`] when it breaks one of the rules above.
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDemoUrl> {
        let value = value.into();
        match demo_url_problem(&value) {
            None => Ok(Self(value)),
            Some(reason) => Err(InvalidDemoUrl { value, reason }),
        }
    }

    /// The URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn demo_url_problem(value: &str) -> Option<&'static str> {
    if value.len() > DemoUrl::MAX_LEN {
        return Some("must be at most 2048 characters");
    }
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return Some("must start with https:// or http://");
    };
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("must not contain spaces or control characters");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Some("must not contain a user name or password");
    }
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _port)| host);
    if host.is_empty() {
        return Some("must have a host");
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
    fn demo_url_accepts_http_and_https() {
        for good in [
            "https://www.youtube.com/watch?v=abc",
            "http://example.com",
            "https://example.com:8443/x#t=1",
            "https://[::1]/demo",
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
        let cases = [
            ("ftp://example.com", "must start with https:// or http://"),
            ("javascript:alert(1)", "must start with https:// or http://"),
            ("HTTPS://example.com", "must start with https:// or http://"),
            ("example.com", "must start with https:// or http://"),
            ("https://", "must have a host"),
            ("https:///path", "must have a host"),
            ("https://:80/", "must have a host"),
            (
                "https://exa mple.com",
                "must not contain spaces or control characters",
            ),
            (
                "https://example.com/\n",
                "must not contain spaces or control characters",
            ),
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
        assert_eq!(
            DemoUrl::new(long).unwrap_err().reason,
            "must be at most 2048 characters"
        );
        let max = format!("https://e.com/{}", "a".repeat(DemoUrl::MAX_LEN - 14));
        assert_eq!(max.len(), DemoUrl::MAX_LEN);
        assert!(DemoUrl::new(max).is_ok());
        assert!(serde_json::from_str::<DemoUrl>("\"ftp://x\"").is_err());
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

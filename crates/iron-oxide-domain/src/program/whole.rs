//! Whole-number fields that also accept an integral float such as `3.0`.
//!
//! JSON Schema's `"integer"` type treats `3` and `3.0` as the same number, so an editor using
//! `program.schema.json` accepts `"sets": 3.0`. These deserializers accept it too, so the schema
//! and the parser agree. Fractions, negatives and out-of-range values are still rejected.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserializer;
use serde::de::{self, Visitor};

use crate::{Percent, Reps, Seconds};

/// 2^64 as an `f64`: every integral float in `[0, 2^64)` converts to `u64` exactly.
const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;

/// `value` as a `u64` if it is integral and in range.
pub(super) fn integral(value: f64) -> Option<u64> {
    if value.is_finite() && value.fract() == 0.0 && (0.0..TWO_POW_64).contains(&value) {
        // Integral and in range, so the cast is exact (and -0.0 becomes 0).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(value as u64)
    } else {
        None
    }
}

struct Whole<T> {
    expecting: &'static str,
    target: PhantomData<T>,
}

impl<T: TryFrom<u64>> Visitor<'_> for Whole<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.expecting)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<T, E> {
        T::try_from(value).map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<T, E> {
        match u64::try_from(value) {
            Ok(value) => self.visit_u64(value),
            Err(_) => Err(E::invalid_value(de::Unexpected::Signed(value), &self)),
        }
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<T, E> {
        match integral(value) {
            Some(value) => self.visit_u64(value),
            None => Err(E::invalid_value(de::Unexpected::Float(value), &self)),
        }
    }
}

fn whole<'de, D: Deserializer<'de>, T: TryFrom<u64>>(
    deserializer: D,
    expecting: &'static str,
) -> Result<T, D::Error> {
    deserializer.deserialize_any(Whole {
        expecting,
        target: PhantomData,
    })
}

const U16: &str = "a whole number from 0 to 65535";
const U32: &str = "a whole number from 0 to 4294967295";

pub(super) fn u16<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    whole(deserializer, U16)
}

pub(super) fn u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    whole(deserializer, U32)
}

pub(super) fn reps<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Reps, D::Error> {
    whole::<D, u16>(deserializer, U16).map(Reps::new)
}

pub(super) fn seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Seconds, D::Error> {
    whole::<D, u32>(deserializer, U32).map(Seconds::new)
}

/// A percentage with at most two decimals (`72.5`, `33.33`), as `Percent` stores it. More
/// decimals are rejected rather than rounded, so a bound such as "above 0 %" means the same in
/// the app and in the schema (`multipleOf: 0.01`).
pub(super) fn percent<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Percent, D::Error> {
    let raw = <f64 as serde::Deserialize>::deserialize(deserializer)?;
    let percent = Percent::new(raw).map_err(de::Error::custom)?;
    if percent.as_percent() == raw {
        Ok(percent)
    } else {
        Err(de::Error::invalid_value(
            de::Unexpected::Float(raw),
            &"a percentage with at most two decimals",
        ))
    }
}

/// The same rule for a value already parsed: an integral, non-negative number.
pub(super) fn as_whole(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_f64().and_then(integral))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Probe {
        #[serde(deserialize_with = "super::u16")]
        sets: u16,
        #[serde(deserialize_with = "super::seconds")]
        rest: crate::Seconds,
    }

    fn parse(sets: &str, rest: &str) -> Result<(u16, u32), String> {
        serde_json::from_str::<Probe>(&format!(r#"{{"sets": {sets}, "rest": {rest}}}"#))
            .map(|probe| (probe.sets, probe.rest.get()))
            .map_err(|error| error.to_string())
    }

    #[test]
    fn accepts_integers_and_integral_floats() {
        assert_eq!(parse("3", "90"), Ok((3, 90)));
        assert_eq!(parse("3.0", "90.0"), Ok((3, 90)));
        assert_eq!(parse("-0.0", "0"), Ok((0, 0)));
        assert_eq!(parse("1e2", "4294967295"), Ok((100, u32::MAX)));
        assert_eq!(parse("65535.0", "4294967295.0"), Ok((u16::MAX, u32::MAX)));
    }

    #[test]
    fn rejects_everything_else() {
        let cases = [
            ("3.5", "invalid value: floating point"),
            ("-1", "invalid value: integer `-1`"),
            ("-1.0", "invalid value: floating point"),
            ("65536", "invalid value: integer `65536`"),
            ("65536.0", "invalid value: integer `65536`"),
            ("1e300", "invalid value: floating point"),
            ("\"3\"", "invalid type: string \"3\""),
            ("null", "invalid type: null"),
            ("[3]", "invalid type: sequence"),
        ];
        for (sets, start) in cases {
            let error = parse(sets, "0").unwrap_err();
            assert!(error.starts_with(start), "{sets}: {error}");
            assert!(
                error.contains(", expected a whole number from 0 to 65535"),
                "{sets}: {error}"
            );
        }
        let error = parse("1", "4294967296").unwrap_err();
        assert!(
            error.contains("expected a whole number from 0 to 4294967295"),
            "{error}"
        );
        assert!(parse("1", "18446744073709551616.0").is_err());
        assert!(parse("1", "18446744073709550000.0").is_err());
    }

    #[test]
    fn percent_keeps_two_decimals() {
        #[derive(Debug, serde::Deserialize)]
        struct P(#[serde(deserialize_with = "super::percent")] crate::Percent);
        let parse = |text: &str| serde_json::from_str::<P>(text).map(|p| p.0.basis_points());
        assert_eq!(parse("72.5").unwrap(), 7_250);
        assert_eq!(parse("33.33").unwrap(), 3_333);
        assert_eq!(parse("0.01").unwrap(), 1);
        assert_eq!(parse("100").unwrap(), 10_000);
        assert_eq!(parse("-0.0").unwrap(), 0);
        for bad in ["0.001", "99.999", "33.333"] {
            let err = parse(bad).unwrap_err().to_string();
            assert!(
                err.contains("a percentage with at most two decimals"),
                "{bad}: {err}"
            );
        }
        assert!(
            parse("-1")
                .unwrap_err()
                .to_string()
                .starts_with("percent must not be negative")
        );
        assert!(parse("\"5\"").is_err());
    }

    #[test]
    fn as_whole_matches_the_deserializers() {
        use serde_json::json;
        assert_eq!(super::as_whole(&json!(2)), Some(2));
        assert_eq!(super::as_whole(&json!(2.0)), Some(2));
        assert_eq!(super::as_whole(&json!(-0.0)), Some(0));
        assert_eq!(super::as_whole(&json!(2.5)), None);
        assert_eq!(super::as_whole(&json!(-1)), None);
        assert_eq!(super::as_whole(&json!(-1.0)), None);
        assert_eq!(super::as_whole(&json!(1e300)), None);
        assert_eq!(super::as_whole(&json!("2")), None);
    }
}

//! Generation of `schemas/program.schema.json` from the program types.
//!
//! schemars is a dev-dependency and the derives are `cfg_attr(test, ...)`, so none of this is
//! compiled into the app. The committed file is the product: editors use it through `$schema`,
//! and the server can serve [`PROGRAM_SCHEMA_JSON`](super::PROGRAM_SCHEMA_JSON). CI runs plain
//! `cargo test`, which runs [`committed_schema_is_up_to_date`], so a stale file fails the build.
//!
//! Regenerate with `UPDATE_SCHEMA=1 cargo test -p iron-oxide-domain program::schema`.
//!
//! The types with hand-written serde (the core newtypes and the program values) get hand-written
//! schemas here that mirror what their `Deserialize` accepts. The integration tests validate the
//! built-in programs and the fixtures against the committed schema, which catches drift.

use std::borrow::Cow;
use std::path::PathBuf;

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use super::ids::SupersetId;
use super::values::{
    DemoUrl, Load, LoadRepr, RepRange, RepTarget, Tempo, UnitWeight, UnitWeightRepr, WarmupLoad,
    WarmupLoadRepr,
};
use super::{PROGRAM_SCHEMA_URL, Program};
use crate::{DayId, ExerciseId, Percent, Reps, Seconds};

/// Matches the slug rules of `ExerciseId`, `DayId` and `SupersetId`.
const SLUG_PATTERN: &str = "^[a-z0-9]+(-[a-z0-9]+)*$";

/// A weight number in its unit; the upper bound (2 000 kg) depends on the unit, so only the lower
/// bound is in the schema.
pub(super) fn weight_number(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "number", "minimum": 0 })
}

fn slug_schema(description: &str) -> Schema {
    json_schema!({
        "type": "string",
        "pattern": SLUG_PATTERN,
        "minLength": 1,
        "maxLength": crate::SLUG_MAX_LEN,
        "description": description,
    })
}

/// `inline`: primitives are written in place; the program values get a named `$defs` entry.
macro_rules! manual_schema {
    ($type:ty, $name:literal, inline = $inline:literal, |$generator:ident| $body:expr) => {
        impl JsonSchema for $type {
            fn inline_schema() -> bool {
                $inline
            }

            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed($name)
            }

            fn json_schema($generator: &mut SchemaGenerator) -> Schema {
                $body
            }
        }
    };
}

manual_schema!(Percent, "Percent", inline = true, |_g| json_schema!({
    "type": "number",
    "minimum": 0,
    "maximum": 1000,
}));
manual_schema!(Reps, "Reps", inline = true, |_g| json_schema!({
    "type": "integer",
    "minimum": 0,
    "maximum": u16::MAX,
}));
manual_schema!(Seconds, "Seconds", inline = true, |_g| json_schema!({
    "type": "integer",
    "minimum": 0,
    "maximum": u32::MAX,
}));
manual_schema!(ExerciseId, "ExerciseId", inline = true, |_g| slug_schema(
    "Lowercase letters and digits in words separated by single hyphens, e.g. `back-squat`."
));
manual_schema!(DayId, "DayId", inline = true, |_g| slug_schema(
    "Lowercase letters and digits in words separated by single hyphens, e.g. `a` or `upper-1`."
));
manual_schema!(SupersetId, "SupersetId", inline = true, |_g| slug_schema(
    "Lowercase letters and digits in words separated by single hyphens, e.g. `a`."
));
manual_schema!(Tempo, "Tempo", inline = false, |_g| json_schema!({
    "type": "string",
    "pattern": "^([0-9]{1,2}|X)-([0-9]{1,2}|X)-([0-9]{1,2}|X)-([0-9]{1,2}|X)$",
    "examples": ["3-1-X-0"],
}));
manual_schema!(DemoUrl, "DemoUrl", inline = false, |_g| json_schema!({
    "type": "string",
    "pattern": "^https?://[^\\s@/?#]+([/?#]\\S*)?$",
    "maxLength": DemoUrl::MAX_LEN,
}));
manual_schema!(UnitWeight, "UnitWeight", inline = false, |g| {
    UnitWeightRepr::json_schema(g)
});
manual_schema!(Load, "Load", inline = false, |g| LoadRepr::json_schema(g));
manual_schema!(WarmupLoad, "WarmupLoad", inline = false, |g| {
    WarmupLoadRepr::json_schema(g)
});
manual_schema!(RepTarget, "RepTarget", inline = false, |g| json_schema!({
    "oneOf": [
        { "type": "integer", "minimum": 0, "maximum": u16::MAX, "description": "Exactly this many reps." },
        g.subschema_for::<RepRange>(),
    ],
}));

/// The schema document, as committed.
fn generate() -> String {
    let mut schema = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<Program>();
    schema.insert("$id".to_owned(), PROGRAM_SCHEMA_URL.into());
    schema.insert("title".to_owned(), "Iron Oxide training program".into());
    let mut json = serde_json::to_string_pretty(&schema).unwrap();
    json.push('\n');
    json
}

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/program.schema.json")
}

#[test]
fn committed_schema_is_up_to_date() {
    let generated = generate();
    let path = committed_path();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "{} is stale: regenerate it with \
         `UPDATE_SCHEMA=1 cargo test -p iron-oxide-domain program::schema` and commit it",
        path.display()
    );
    assert_eq!(super::PROGRAM_SCHEMA_JSON, committed);
}

#[test]
fn schema_describes_the_document() {
    let schema: serde_json::Value = serde_json::from_str(&generate()).unwrap();
    assert_eq!(schema["$id"], PROGRAM_SCHEMA_URL);
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["additionalProperties"], false);
    let required = schema["required"].as_array().unwrap();
    for field in ["schema_version", "name", "days", "rotation"] {
        assert!(required.contains(&field.into()), "{field}");
    }
    assert!(schema["properties"]["$schema"].is_object());
}

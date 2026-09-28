//! Shape rules serde's derives do not enforce.
//!
//! A derived struct also deserializes from a JSON array of its fields in order, so
//! `{"intervals": [8, 30, 90]}` would silently mean work 8 s, rest 30 s, 90 rounds, and error
//! paths would name keys that are not in the document. A unit variant also deserializes from
//! `{"none": null}`. The schema rejects both, so this walk rejects them too, on the document that
//! serde has already accepted.

use serde_json::{Map, Value};

use super::error::{JsonPath, ParseError};

fn error(path: JsonPath, message: &str) -> ParseError {
    ParseError {
        path,
        message: message.to_owned(),
        line: 0,
        column: 0,
    }
}

/// Checks a document that deserialized into a `Program`.
pub(super) fn check(document: &Value) -> Result<(), ParseError> {
    let Value::Object(root) = document else {
        return Ok(());
    };
    for_each_field(root, &JsonPath::root(), |key, value, path| match key {
        "days" => list_of_objects(value, path, day),
        "rotation" => Ok(()),
        _ => no_arrays(value, path),
    })
}

fn day(day: &Map<String, Value>, path: &JsonPath) -> Result<(), ParseError> {
    for_each_field(day, path, |key, value, path| match key {
        "exercises" => list_of_objects(value, path, exercise),
        _ => no_arrays(value, path),
    })
}

fn exercise(exercise: &Map<String, Value>, path: &JsonPath) -> Result<(), ParseError> {
    for_each_field(exercise, path, |key, value, path| match key {
        "warmup" => list_of_objects(value, path, |line, path| {
            for_each_field(line, path, |_, value, path| no_arrays(value, path))
        }),
        "progression" => progression(value, path),
        _ => no_arrays(value, path),
    })
}

fn progression(value: &Value, path: &JsonPath) -> Result<(), ParseError> {
    if let Value::Object(rule) = value
        && rule.contains_key("none")
    {
        return Err(error(
            path.key("none"),
            r#"write the rule `none` as the string "none""#,
        ));
    }
    no_arrays(value, path)
}

fn for_each_field(
    object: &Map<String, Value>,
    path: &JsonPath,
    mut check: impl FnMut(&str, &Value, &JsonPath) -> Result<(), ParseError>,
) -> Result<(), ParseError> {
    object
        .iter()
        .try_for_each(|(key, value)| check(key, value, &path.key(key.clone())))
}

/// An array whose items must all be objects (days, exercises, warm-up lines).
fn list_of_objects(
    value: &Value,
    path: &JsonPath,
    item: impl Fn(&Map<String, Value>, &JsonPath) -> Result<(), ParseError>,
) -> Result<(), ParseError> {
    let Value::Array(items) = value else {
        return no_arrays(value, path);
    };
    items.iter().enumerate().try_for_each(|(index, value)| {
        let path = path.index(index);
        match value {
            Value::Object(object) => item(object, &path),
            _ => Err(error(path, "expected an object")),
        }
    })
}

/// Anything other than the lists above: arrays are never allowed.
fn no_arrays(value: &Value, path: &JsonPath) -> Result<(), ParseError> {
    match value {
        Value::Array(_) => Err(error(path.clone(), "expected an object, not an array")),
        Value::Object(object) => {
            for_each_field(object, path, |_, value, path| no_arrays(value, path))
        }
        _ => Ok(()),
    }
}

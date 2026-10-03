//! Finding the program in an AI assistant's answer (#108).
//!
//! A user asks their own AI assistant for a program and pastes its answer. Even when asked for
//! JSON only, assistants wrap it: Markdown code fences, a sentence before, a tip after.
//! [`extract_json`] finds the one JSON object in such a text and returns it as written. It only
//! cuts the surrounding text away: what it returns still goes through
//! [`Program::from_json`](super::Program::from_json), so it never makes a document valid that the
//! validator would refuse.
//!
//! The scan is a single pass with a depth counter (no recursion), so it is linear in the input and
//! safe on hostile text. Quotes are tracked only inside braces: prose quotes and apostrophes
//! around the JSON don't matter, while braces inside JSON strings are not mistaken for structure.

use std::fmt;
use std::ops::Range;

use super::limits::MAX_DOCUMENT_BYTES;

/// Largest pasted text [`extract_json`] reads, in bytes: room for a document of
/// [`MAX_DOCUMENT_BYTES`] and a lot of prose around it.
pub const MAX_PASTE_BYTES: usize = 4 * MAX_DOCUMENT_BYTES;

/// Why no single JSON object could be taken from a pasted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtractError {
    /// Nothing but whitespace.
    Empty,
    /// Longer than [`MAX_PASTE_BYTES`].
    TooLong {
        /// The text's length, in bytes.
        bytes: usize,
    },
    /// No `{` at all.
    NoJson,
    /// An object starts but never closes: the answer was cut off.
    CutOff,
    /// Several separate objects, and no way to tell which is the program.
    Several {
        /// How many top-level `{…}` blocks the text holds.
        count: usize,
    },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Paste your AI's answer first."),
            Self::TooLong { .. } => write!(
                f,
                "This text is too long: paste at most {} KiB.",
                MAX_PASTE_BYTES / 1024
            ),
            Self::NoJson => f.write_str(
                "There is no JSON program in this text. Ask your AI to answer with the program as \
                 JSON.",
            ),
            Self::CutOff => f.write_str(
                "The program looks cut off: its JSON never closes. Ask your AI to send the whole \
                 program again.",
            ),
            Self::Several { count } => write!(
                f,
                "This text holds {count} separate JSON blocks, so it is not clear which one is the \
                 program. Keep only the program, or ask your AI to answer with a single JSON \
                 document."
            ),
        }
    }
}

impl std::error::Error for ExtractError {}

/// The JSON object in a pasted text: the text itself when it is one, else the one object among
/// fences and prose.
///
/// - Exactly one top-level `{…}` block: that block, even if it is not valid JSON, so the
///   validator reports its line and column.
/// - Several blocks (prose such as "use {kg} or {lb}" makes blocks too): the one that is a valid
///   JSON object, if exactly one is; otherwise [`ExtractError::Several`].
/// - A block that never closes and nothing else usable: [`ExtractError::CutOff`].
///
/// # Errors
/// [`ExtractError`], with a message for the user.
pub fn extract_json(text: &str) -> Result<&str, ExtractError> {
    if text.len() > MAX_PASTE_BYTES {
        return Err(ExtractError::TooLong { bytes: text.len() });
    }
    // A byte-order mark is not part of the JSON.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.trim().is_empty() {
        return Err(ExtractError::Empty);
    }
    let (blocks, unclosed) = top_level_blocks(text);
    let block = |range: &Range<usize>| text.get(range.clone()).unwrap_or_default();
    if let [only] = blocks.as_slice()
        && !unclosed
    {
        return Ok(block(only));
    }
    let objects: Vec<&str> = blocks
        .iter()
        .map(block)
        .filter(|candidate| is_json_object(candidate))
        .collect();
    match (objects.as_slice(), unclosed) {
        ([object], _) => Ok(object),
        ([], true) => Err(ExtractError::CutOff),
        ([], false) if blocks.is_empty() => Err(ExtractError::NoJson),
        // Two valid objects or more: count those; else every block, none being the program.
        (objects, _) => Err(ExtractError::Several {
            count: if objects.len() > 1 {
                objects.len()
            } else {
                blocks.len()
            },
        }),
    }
}

/// The byte ranges of the top-level `{…}` blocks, and whether a last one never closes.
fn top_level_blocks(text: &str) -> (Vec<Range<usize>>, bool) {
    let mut blocks = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    let mut in_string = false;
    let mut escaped = false;
    // Every byte that matters is ASCII, and UTF-8 never uses ASCII bytes inside a multi-byte
    // character, so a byte scan is exact and the ranges fall on character boundaries.
    for (index, byte) in text.bytes().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'{' => {
                if depth == 0 {
                    start = index;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    blocks.push(start..index + 1);
                }
            }
            b'"' if depth > 0 => in_string = true,
            _ => {}
        }
    }
    (blocks, depth > 0)
}

/// Whether `candidate` is one complete JSON object (nothing is kept from it).
fn is_json_object(candidate: &str) -> bool {
    candidate.starts_with('{') && serde_json::from_str::<serde::de::IgnoredAny>(candidate).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = r#"{
  "schema_version": 1,
  "name": "Mine",
  "days": [{ "id": "a", "name": "A", "exercises": [
    { "id": "squat", "name": "Squat", "work": { "reps": { "sets": 3, "reps": 5 } }, "rest": 120 }
  ] }],
  "rotation": ["a"]
}"#;

    #[test]
    fn a_bare_document_is_taken_as_it_is() {
        assert_eq!(extract_json(PROGRAM), Ok(PROGRAM));
        let padded = format!("\n\n  {PROGRAM}\n ");
        assert_eq!(extract_json(&padded), Ok(PROGRAM));
        assert_eq!(extract_json(&format!("\u{feff}{PROGRAM}")), Ok(PROGRAM));
    }

    #[test]
    fn fences_and_prose_are_cut_away() {
        for wrapped in [
            format!("```json\n{PROGRAM}\n```"),
            format!("```\n{PROGRAM}\n```"),
            format!("~~~json\n{PROGRAM}\n~~~"),
            format!(
                "Here is your program:\n\n```json\n{PROGRAM}\n```\n\nGood luck, and don't skip the warm-ups!"
            ),
            format!(
                "Sure! It's a 3-day \"full body\" plan.\n{PROGRAM}\nLet me know if you'd like changes."
            ),
            // Four-backtick fences around three-backtick ones.
            format!("````\n```json\n{PROGRAM}\n```\n````"),
        ] {
            assert_eq!(extract_json(&wrapped), Ok(PROGRAM), "{wrapped}");
        }
    }

    #[test]
    fn braces_quotes_and_fences_inside_strings_are_text() {
        let tricky =
            r#"{"name": "A } tricky { \"name\" ```json``` \\", "notes": "{}{}", "x": ["}"]}"#;
        assert_eq!(extract_json(tricky), Ok(tricky));
        let wrapped = format!("```json\n{tricky}\n```\nThat's it }}");
        assert_eq!(extract_json(&wrapped), Ok(tricky));
        // Non-ASCII text around and inside.
        let accented = r#"{"name": "Séance — jambes 💪"}"#;
        assert_eq!(
            extract_json(&format!("Voilà « ton » programme : {accented} 🙂")),
            Ok(accented)
        );
    }

    #[test]
    fn prose_braces_do_not_hide_the_program() {
        let text = format!(
            "Loads are written as {{kg}} or {{lb}}, e.g. {{like this}}.\n```json\n{PROGRAM}\n```"
        );
        assert_eq!(extract_json(&text), Ok(PROGRAM));
    }

    #[test]
    fn a_single_broken_block_is_returned_for_the_validator_to_report() {
        let broken = r#"{"schema_version": 1, "name": "Mine",}"#;
        assert_eq!(extract_json(&format!("```json\n{broken}\n```")), Ok(broken));
    }

    #[test]
    fn several_objects_are_ambiguous() {
        // The program twice.
        assert_eq!(
            extract_json(&format!("{PROGRAM}\n\nAnd again:\n{PROGRAM}")),
            Err(ExtractError::Several { count: 2 })
        );
        // Two different objects, one in prose.
        assert_eq!(
            extract_json(&format!("An increment is {{\"kg\": 2.5}}.\n{PROGRAM}")),
            Err(ExtractError::Several { count: 2 })
        );
        // An array of two programs holds two objects at the top level of braces.
        assert_eq!(
            extract_json(&format!("[{PROGRAM}, {PROGRAM}]")),
            Err(ExtractError::Several { count: 2 })
        );
        // Several blocks and none is JSON.
        assert_eq!(
            extract_json("{a} and {b}"),
            Err(ExtractError::Several { count: 2 })
        );
    }

    #[test]
    fn a_cut_off_answer_says_so() {
        let cut = PROGRAM.get(..PROGRAM.len() / 2).unwrap();
        assert_eq!(
            extract_json(&format!("```json\n{cut}")),
            Err(ExtractError::CutOff)
        );
        // An unclosed string swallows the rest.
        assert_eq!(
            extract_json(r#"{"name": "never closed }"#),
            Err(ExtractError::CutOff)
        );
        // A closed block in the prose before it doesn't help when it is not JSON.
        assert_eq!(
            extract_json(&format!("Use {{kg}}.\n{cut}")),
            Err(ExtractError::CutOff)
        );
    }

    #[test]
    fn text_without_json_is_refused() {
        assert_eq!(extract_json(""), Err(ExtractError::Empty));
        assert_eq!(extract_json(" \n\t "), Err(ExtractError::Empty));
        assert_eq!(extract_json("\u{feff}"), Err(ExtractError::Empty));
        assert_eq!(
            extract_json("I can't help with that. What are your goals?"),
            Err(ExtractError::NoJson)
        );
        // A stray closing brace or an array is not an object.
        assert_eq!(extract_json("} oops ]"), Err(ExtractError::NoJson));
        assert_eq!(extract_json("[1, 2, 3]"), Err(ExtractError::NoJson));
    }

    #[test]
    fn huge_or_deep_input_is_bounded() {
        // Over the cap: refused before scanning.
        let huge = "a".repeat(MAX_PASTE_BYTES + 1);
        assert_eq!(
            extract_json(&huge),
            Err(ExtractError::TooLong {
                bytes: MAX_PASTE_BYTES + 1
            })
        );
        // At the cap, prose around the program: one linear scan.
        let prose = "lorem ipsum \"quoted\" it's ".repeat(MAX_PASTE_BYTES / 64);
        let text = format!("{prose}{PROGRAM}{prose}");
        assert!(text.len() <= MAX_PASTE_BYTES);
        assert_eq!(extract_json(&text), Ok(PROGRAM));
        // Deep nesting: no recursion here; serde's own depth limit stops the parse check.
        let open = "{".repeat(100_000);
        assert_eq!(extract_json(&open), Err(ExtractError::CutOff));
        let deep = format!("{open}{}", "}".repeat(100_000));
        assert_eq!(extract_json(&deep), Ok(deep.as_str()));
        assert!(super::super::Program::from_json(&deep).is_err());
        // Many tiny blocks.
        let many = "{} ".repeat(100_000);
        assert_eq!(
            extract_json(&many),
            Err(ExtractError::Several { count: 100_000 })
        );
    }

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(
            ExtractError::TooLong { bytes: 1 }.to_string(),
            "This text is too long: paste at most 1024 KiB."
        );
        assert!(
            ExtractError::Several { count: 3 }
                .to_string()
                .starts_with("This text holds 3 separate JSON blocks")
        );
    }
}

//! Loads an optional local `.env` file for development.
//!
//! Only `.env` in the working directory is read (no search in parent directories), real
//! environment variables win over it, and a bad file is reported by line number only: the
//! offending content is never printed, since it may hold a secret.

use std::{io, path::Path};

/// Why the `.env` file could not be loaded. Never contains any of the file's content.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DotenvError {
    #[error("cannot read {path}: {kind}")]
    Read { path: String, kind: io::ErrorKind },
    #[error("{path} has a syntax error on line {line} (content not shown: it may hold a secret)")]
    Syntax { path: String, line: usize },
    #[error("{path} is not valid UTF-8")]
    NotUtf8 { path: String },
    #[error("{path} could not be loaded")]
    Other { path: String },
}

/// Loads `path` into the process environment if it exists. Variables already set are kept.
/// Nothing is loaded unless the whole file parses.
pub fn load(path: &Path) -> Result<(), DotenvError> {
    let shown = path.display().to_string();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(DotenvError::Read {
                path: shown,
                kind: error.kind(),
            });
        }
    };
    let content = String::from_utf8(bytes).map_err(|_| DotenvError::NotUtf8 {
        path: shown.clone(),
    })?;

    // Parse everything first, so a bad line leaves the environment untouched.
    for item in dotenvy::from_read_iter(content.as_bytes()) {
        if let Err(error) = item {
            return Err(describe(error, &content, shown));
        }
    }
    dotenvy::from_read(content.as_bytes()).map_err(|error| describe(error, &content, shown))
}

/// Turns a dotenvy error into one that never carries file content (dotenvy's `Display` for a
/// parse error quotes the offending line).
fn describe(error: dotenvy::Error, content: &str, path: String) -> DotenvError {
    match error {
        dotenvy::Error::LineParse(rest, _) => DotenvError::Syntax {
            line: line_of(content, &rest),
            path,
        },
        dotenvy::Error::Io(error) => DotenvError::Read {
            path,
            kind: error.kind(),
        },
        _ => DotenvError::Other { path },
    }
}

/// 1-based line number of the entry dotenvy failed on. `rest` is the unparsed text it reports,
/// which can start mid-line (at the value).
///
/// The same text can appear earlier (e.g. in a comment), so this takes the last match whose line
/// is preceded only by entries that parse: any later match has the bad entry before it.
fn line_of(content: &str, rest: &str) -> usize {
    let parses = |prefix: &str| dotenvy::from_read_iter(prefix.as_bytes()).all(|item| item.is_ok());
    let line_start = content
        .rmatch_indices(rest)
        .map(|(index, _)| {
            content[..index]
                .rfind('\n')
                .map_or(0, |newline| newline + 1)
        })
        .find(|&start| parses(&content[..start]))
        .unwrap_or(0);
    content[..line_start].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory for one test.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("iron-oxide-dotenv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_is_fine() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir.join(".env")), Ok(()));
    }

    #[test]
    fn a_syntax_error_names_the_line_but_never_the_content() {
        let dir = temp_dir("syntax");
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "# comment\nAPP_BASE_URL=http://localhost\nDATABASE_URL=postgres://u:SECRETxyz@h/d x\n",
        )
        .unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            matches!(error, DotenvError::Syntax { line: 3, .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("on line 3"), "{error}");
        assert!(
            !format!("{error} {error:?}").contains("SECRETxyz"),
            "{error:?}"
        );
    }

    #[test]
    fn an_unterminated_quote_never_shows_the_content() {
        let dir = temp_dir("quote");
        let path = dir.join(".env");
        std::fs::write(&path, "GOOGLE_CLIENT_SECRET=\"SECRETxyz\nSESSION_KEY=abc\n").unwrap();
        let error = load(&path).unwrap_err();
        assert!(
            matches!(error, DotenvError::Syntax { line: 1, .. }),
            "{error:?}"
        );
        assert!(!format!("{error} {error:?}").contains("SECRETxyz"));
    }

    #[test]
    fn invalid_utf8_is_reported_without_content() {
        let dir = temp_dir("utf8");
        let path = dir.join(".env");
        std::fs::write(&path, b"SECRET=\xff\xfeSECRETxyz\n").unwrap();
        let error = load(&path).unwrap_err();
        assert!(matches!(error, DotenvError::NotUtf8 { .. }), "{error:?}");
        assert!(!format!("{error} {error:?}").contains("SECRETxyz"));
    }

    #[test]
    fn a_directory_instead_of_a_file_is_a_read_error() {
        let dir = temp_dir("dir");
        let path = dir.join(".env");
        std::fs::create_dir_all(&path).unwrap();
        assert!(matches!(load(&path), Err(DotenvError::Read { .. })));
    }

    #[test]
    fn a_valid_file_is_loaded() {
        let dir = temp_dir("valid");
        let path = dir.join(".env");
        let name = format!("IRON_OXIDE_DOTENV_TEST_{}", std::process::id());
        std::fs::write(&path, format!("# comment\n{name}=\"loaded value\"\n")).unwrap();
        assert_eq!(load(&path), Ok(()));
        assert_eq!(std::env::var(&name).as_deref(), Ok("loaded value"));
    }

    #[test]
    fn line_of_counts_from_one() {
        assert_eq!(line_of("A=1\nB=2\nC\n", "C\n"), 3);
        assert_eq!(line_of("C\n", "C\n"), 1);
        assert_eq!(line_of("A=1\n", "not found"), 1);
        // The same text in an earlier comment is not the bad line.
        assert_eq!(line_of("A=ok\n# C=x y\nC=x y\n", "C=x y\n"), 3);
    }
}

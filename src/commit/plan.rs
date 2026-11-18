//! What the commit engine is given: a plan of whole-file changes, plus the line
//! ending and path rules it enforces.

use std::path::{Component, Path};

/// A validated proposal. Creating a plan never writes to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub id: String,
    pub digest: String,
    pub files: Vec<PlannedFile>,
}

/// Original and proposed contents for one file. An absent original (an added
/// file) or an absent proposal (a deleted one) is marked by its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedFile {
    pub path: String,
    pub original: String,
    pub original_digest: String,
    pub proposed: String,
    pub proposed_digest: String,
}

/// The path is not a safe relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::Crlf => "\r\n",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewlineError {
    LoneCr,
    Mixed,
}

pub(crate) fn detect_newline(contents: &str) -> Result<Newline, NewlineError> {
    let bytes = contents.as_bytes();
    let mut has_lf = false;
    let mut has_crlf = false;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    has_crlf = true;
                    index += 2;
                } else {
                    return Err(NewlineError::LoneCr);
                }
            }
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    if has_lf && has_crlf {
        return Err(NewlineError::Mixed);
    }

    if has_crlf {
        Ok(Newline::Crlf)
    } else {
        Ok(Newline::Lf)
    }
}


/// Validates relative paths: rejects empty, backslashes, absolute paths,
/// non-normal components (`.` / `..`), and `.git` components (case-insensitive).
pub(crate) fn validate_relative_path(path: &str) -> Result<(), InvalidPath> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.ends_with('/') {
        return Err(InvalidPath);
    }

    let p = Path::new(path);
    if p.is_absolute() {
        return Err(InvalidPath);
    }

    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(InvalidPath);
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(InvalidPath);
        }
    }

    for component in p.components() {
        match component {
            Component::Normal(c) => {
                if c.to_str().is_some_and(|s| s.eq_ignore_ascii_case(".git")) {
                    return Err(InvalidPath);
                }
            }
            _ => return Err(InvalidPath),
        }
    }

    Ok(())
}

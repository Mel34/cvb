//! Parses the structure of an `apply_patch` envelope.
//!
//! CVB does not interpret hunks: the installed `apply_patch` does that, in a
//! scratch copy. What CVB needs before it runs anything is the list of files the
//! patch touches and how, so it can check the paths, copy the right files and
//! commit the result. The parser is pure and strict: anything it does not
//! recognise is rejected, never guessed at.
//!
//! Format (one-character prefixes make `***` at column 1 unambiguous):
//!
//! ```text
//! *** Begin Patch
//! *** Update File: PATH        optionally followed by  *** Move to: PATH
//! @@ context                   hunks: lines starting with ' ', '+' or '-'
//! *** Add File: PATH           body: lines starting with '+'
//! *** Delete File: PATH        no body
//! *** End Patch
//! ```

use std::collections::{HashMap, HashSet};

use crate::commit::plan::validate_relative_path;

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const UPDATE: &str = "*** Update File: ";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";
const MAX_PATH_BYTES: usize = 4096;

/// What a patch does to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    Add,
    Update,
    Delete,
    /// Update, then rename to `to`.
    Move { to: String },
}

/// One file section of a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileOp {
    pub path: String,
    pub change: Change,
    /// 1-based line of the header, for messages.
    pub line: usize,
}

/// Which syntax a patch was written in. It decides the program that applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// `*** Begin Patch` ... `*** End Patch`, applied by `apply_patch`.
    ApplyPatch,
    /// A unified diff as `git diff` prints it, applied by `git apply`.
    GitDiff,
}

/// A structurally valid patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedPatch {
    pub ops: Vec<FileOp>,
    pub format: Format,
    /// Exact permission bits for added files whose patch states them
    /// (`new file mode 100755`), keyed by path.
    pub modes: HashMap<String, u32>,
    /// For a diff whose hunk header counts were wrong: the diff with the counts
    /// corrected, which is what gets applied. `None` means "apply the pasted text".
    pub normalized: Option<String>,
    /// Remarks for the report (what CVB corrected).
    pub notes: Vec<String>,
}

impl ParsedPatch {
    /// Every path the patch names, sources and destinations, in patch order.
    pub(crate) fn paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        for op in &self.ops {
            paths.push(op.path.as_str());
            if let Change::Move { to } = &op.change {
                paths.push(to.as_str());
            }
        }
        paths
    }
}

/// Why a patch was refused. Every variant carries the 1-based line it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseError {
    MissingBegin,
    MissingEnd,
    NoFiles,
    UnknownHeader { line: usize, text: String },
    UnexpectedLine { line: usize, text: String },
    MisplacedMove { line: usize },
    EndOfFileOutsideUpdate { line: usize },
    UnsafePath { line: usize, path: String },
    DuplicatePath { line: usize, path: String },
    MoveToSelf { line: usize, path: String },
    EmptyUpdate { line: usize, path: String },
    /// A diff that stops inside a hunk (the clipboard probably cut it short).
    Truncated { line: usize },
    /// Valid syntax that CVB does not apply (binary patches, mode changes, ...).
    Unsupported { line: usize, what: String },
    /// A diff that is malformed in the way the message says.
    BadDiff { line: usize, message: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::MissingBegin => write!(f, "the patch does not start with '{BEGIN}'"),
            ParseError::MissingEnd => write!(f, "the patch does not end with '{END}'"),
            ParseError::NoFiles => write!(f, "the patch names no files"),
            ParseError::UnknownHeader { line, text } => {
                write!(f, "line {line}: unknown header: {}", show(text))
            }
            ParseError::UnexpectedLine { line, text } => {
                write!(f, "line {line}: unexpected line: {}", show(text))
            }
            ParseError::MisplacedMove { line } => write!(
                f,
                "line {line}: '*** Move to:' must directly follow '*** Update File:'"
            ),
            ParseError::EndOfFileOutsideUpdate { line } => {
                write!(f, "line {line}: '{END_OF_FILE}' is only valid inside an update")
            }
            ParseError::UnsafePath { line, path } => {
                write!(f, "line {line}: unsafe path: {}", show(path))
            }
            ParseError::DuplicatePath { line, path } => write!(
                f,
                "line {line}: path named more than once (a patch may touch each path once): {}",
                show(path)
            ),
            ParseError::MoveToSelf { line, path } => {
                write!(f, "line {line}: file is moved onto itself: {}", show(path))
            }
            ParseError::EmptyUpdate { line, path } => {
                write!(f, "line {line}: update of {} has no changes", show(path))
            }
            ParseError::Truncated { line } => write!(
                f,
                "incomplete paste: the diff ends inside a hunk at line {line}. The clipboard may \
                 have truncated it (4 MB limit); split the patch into smaller ones"
            ),
            ParseError::Unsupported { line, what } => {
                write!(f, "line {line}: not supported: {what}")
            }
            ParseError::BadDiff { line, message } => write!(f, "line {line}: {message}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Quotes text for a message, with control characters made visible and long
/// text cut short, so a hostile line cannot drive the terminal.
pub(crate) fn show(text: &str) -> String {
    let mut out = String::from("'");
    for (count, ch) in text.chars().enumerate() {
        if count >= 80 {
            out.push('…');
            break;
        }
        if ch.is_control() {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

enum Open {
    None,
    Add,
    Delete,
    Update { index: usize, body: bool, moved: bool },
}

/// Parses a pasted patch in either syntax: `*** Begin Patch` selects the
/// `apply_patch` format, a first line of `diff --git ` or `--- ` selects a diff.
pub(crate) fn parse_any(text: &str) -> Result<ParsedPatch, ParseError> {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if first.starts_with("diff --git ") || first.starts_with("--- ") {
        crate::patch::gitdiff::parse_git_diff(text)
    } else {
        parse_patch(text)
    }
}

/// Parses `text`, which must be one complete patch.
pub(crate) fn parse_patch(text: &str) -> Result<ParsedPatch, ParseError> {
    // Line numbers count every line of `text`, blank ones included.
    let lines: Vec<(usize, &str)> = text
        .split('\n')
        .enumerate()
        .map(|(i, l)| (i + 1, l.strip_suffix('\r').unwrap_or(l)))
        .collect();

    let first = lines.iter().position(|(_, l)| !l.trim().is_empty());
    let last = lines.iter().rposition(|(_, l)| !l.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return Err(ParseError::MissingBegin);
    };
    if lines[first].1 != BEGIN {
        return Err(ParseError::MissingBegin);
    }
    if lines[last].1 != END || last == first {
        return Err(ParseError::MissingEnd);
    }

    let mut ops: Vec<FileOp> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut open = Open::None;

    for &(number, line) in &lines[first + 1..last] {
        if let Some(rest) = line.strip_prefix(UPDATE) {
            finish(&open, &ops)?;
            let path = checked_path(rest, number, &mut seen)?;
            ops.push(FileOp { path, change: Change::Update, line: number });
            open = Open::Update { index: ops.len() - 1, body: false, moved: false };
        } else if let Some(rest) = line.strip_prefix(ADD) {
            finish(&open, &ops)?;
            let path = checked_path(rest, number, &mut seen)?;
            ops.push(FileOp { path, change: Change::Add, line: number });
            open = Open::Add;
        } else if let Some(rest) = line.strip_prefix(DELETE) {
            finish(&open, &ops)?;
            let path = checked_path(rest, number, &mut seen)?;
            ops.push(FileOp { path, change: Change::Delete, line: number });
            open = Open::Delete;
        } else if let Some(rest) = line.strip_prefix(MOVE_TO) {
            match &mut open {
                Open::Update { index, body: false, moved } if !*moved => {
                    if rest == ops[*index].path {
                        return Err(ParseError::MoveToSelf {
                            line: number,
                            path: rest.to_string(),
                        });
                    }
                    let to = checked_path(rest, number, &mut seen)?;
                    ops[*index].change = Change::Move { to };
                    *moved = true;
                }
                _ => return Err(ParseError::MisplacedMove { line: number }),
            }
        } else if line == END_OF_FILE {
            match &mut open {
                Open::Update { body, .. } => *body = true,
                _ => return Err(ParseError::EndOfFileOutsideUpdate { line: number }),
            }
        } else if line.starts_with("***") {
            return Err(ParseError::UnknownHeader { line: number, text: line.to_string() });
        } else {
            match &mut open {
                Open::None => {
                    if !line.trim().is_empty() {
                        return Err(unexpected(number, line));
                    }
                }
                Open::Add => {
                    if !line.starts_with('+') && !line.is_empty() {
                        return Err(unexpected(number, line));
                    }
                }
                Open::Delete => {
                    if !line.trim().is_empty() {
                        return Err(unexpected(number, line));
                    }
                }
                Open::Update { body, .. } => match line.chars().next() {
                    None => {}
                    Some(' ' | '+' | '-' | '@') => *body = true,
                    Some(_) => return Err(unexpected(number, line)),
                },
            }
        }
    }
    finish(&open, &ops)?;

    if ops.is_empty() {
        return Err(ParseError::NoFiles);
    }
    Ok(ParsedPatch {
        ops,
        format: Format::ApplyPatch,
        modes: HashMap::new(),
        normalized: None,
        notes: Vec::new(),
    })
}

fn unexpected(line: usize, text: &str) -> ParseError {
    ParseError::UnexpectedLine { line, text: text.to_string() }
}

/// Checks the section that is being closed.
fn finish(open: &Open, ops: &[FileOp]) -> Result<(), ParseError> {
    if let Open::Update { index, body: false, moved: false } = open {
        let op = &ops[*index];
        return Err(ParseError::EmptyUpdate { line: op.line, path: op.path.clone() });
    }
    Ok(())
}

/// Syntax rules for a header path, and uniqueness across the whole patch.
pub(crate) fn checked_path(
    raw: &str,
    line: usize,
    seen: &mut HashSet<String>,
) -> Result<String, ParseError> {
    let unsafe_path = || ParseError::UnsafePath { line, path: raw.to_string() };
    if raw.len() > MAX_PATH_BYTES
        || raw.trim() != raw
        || raw.chars().any(char::is_control)
        || validate_relative_path(raw).is_err()
    {
        return Err(unsafe_path());
    }
    if !seen.insert(raw.to_string()) {
        return Err(ParseError::DuplicatePath { line, path: raw.to_string() });
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch\n")
    }

    fn ok(body: &str) -> ParsedPatch {
        parse_patch(&patch(body)).unwrap_or_else(|e| panic!("{e}: {body:?}"))
    }

    fn err(body: &str) -> ParseError {
        parse_patch(&patch(body)).expect_err(body)
    }

    fn op(path: &str, change: Change, line: usize) -> FileOp {
        FileOp { path: path.to_string(), change, line }
    }

    #[test]
    fn parses_add_update_delete_and_move() {
        let parsed = ok("*** Add File: new/a.txt\n+one\n+two\n\
             *** Update File: src/b.rs\n@@ fn main\n ctx\n-old\n+new\n\
             *** Delete File: old.txt\n\
             *** Update File: c.txt\n*** Move to: d/e.txt\n@@\n-x\n+y\n");
        assert_eq!(
            parsed.ops,
            vec![
                op("new/a.txt", Change::Add, 2),
                op("src/b.rs", Change::Update, 5),
                op("old.txt", Change::Delete, 10),
                op("c.txt", Change::Move { to: "d/e.txt".into() }, 11),
            ]
        );
        assert_eq!(
            parsed.paths(),
            vec!["new/a.txt", "src/b.rs", "old.txt", "c.txt", "d/e.txt"]
        );
    }

    #[test]
    fn accepts_the_shape_foot_pastes() {
        // No trailing newline after the end line, and CRLF anywhere is tolerated.
        let text = "*** Begin Patch\n*** Delete File: x\n*** End Patch";
        assert!(parse_patch(text).is_ok());
        let crlf = "*** Begin Patch\r\n*** Delete File: x\r\n*** End Patch\r\n";
        assert!(parse_patch(crlf).is_ok());
    }

    #[test]
    fn accepts_blank_lines_around_and_inside_sections() {
        let text = "\n*** Begin Patch\n\n*** Update File: a\n@@\n a\n\n-b\n+c\n\n*** End Patch\n\n";
        assert!(parse_patch(text).is_ok());
    }

    #[test]
    fn a_pure_rename_is_accepted() {
        let parsed = ok("*** Update File: a\n*** Move to: b\n");
        assert_eq!(parsed.ops[0].change, Change::Move { to: "b".into() });
    }

    #[test]
    fn end_of_file_marker_is_part_of_an_update() {
        ok("*** Update File: a\n@@\n-x\n+y\n*** End of File\n");
        assert!(matches!(
            err("*** Add File: a\n+x\n*** End of File\n"),
            ParseError::EndOfFileOutsideUpdate { line: 4 }
        ));
    }

    #[test]
    fn missing_envelope_lines_are_rejected() {
        assert_eq!(parse_patch(""), Err(ParseError::MissingBegin));
        assert_eq!(parse_patch("\n \n"), Err(ParseError::MissingBegin));
        assert_eq!(
            parse_patch("*** Update File: a\n*** End Patch\n"),
            Err(ParseError::MissingBegin)
        );
        assert_eq!(
            parse_patch("*** Begin Patch\n*** Delete File: a\n"),
            Err(ParseError::MissingEnd)
        );
        assert_eq!(parse_patch("*** Begin Patch\n"), Err(ParseError::MissingEnd));
        assert_eq!(
            parse_patch("*** Begin Patch\n*** End Patch\n"),
            Err(ParseError::NoFiles)
        );
    }

    #[test]
    fn every_truncation_of_a_valid_patch_is_rejected() {
        let full = patch(
            "*** Add File: a.txt\n+one\n*** Update File: b.txt\n@@\n-x\n+y\n*** Delete File: c.txt\n",
        );
        let end = full.rfind(END).unwrap() + END.len();
        for cut in 0..end {
            if !full.is_char_boundary(cut) {
                continue;
            }
            assert!(
                parse_patch(&full[..cut]).is_err(),
                "a cut at {cut} was accepted: {:?}",
                &full[..cut]
            );
        }
        assert!(parse_patch(&full[..end]).is_ok());
    }

    #[test]
    fn unknown_headers_and_stray_text_are_rejected() {
        assert!(matches!(
            err("*** Rename File: a\n"),
            ParseError::UnknownHeader { line: 2, .. }
        ));
        assert!(matches!(
            err("*** Update File:a\n"),
            ParseError::UnknownHeader { .. }
        ));
        assert!(matches!(
            err("*** Begin Patch\n*** Delete File: a\n"),
            ParseError::UnknownHeader { .. }
        ));
        assert!(matches!(err("hello\n*** Delete File: a\n"), ParseError::UnexpectedLine { line: 2, .. }));
        assert!(matches!(
            err("*** Delete File: a\nstray\n"),
            ParseError::UnexpectedLine { line: 3, .. }
        ));
        assert!(matches!(
            err("*** Add File: a\n+ok\nnot a plus line\n"),
            ParseError::UnexpectedLine { line: 4, .. }
        ));
        assert!(matches!(
            err("*** Update File: a\n@@\n-x\nstray\n"),
            ParseError::UnexpectedLine { line: 5, .. }
        ));
    }

    #[test]
    fn move_must_directly_follow_its_update() {
        for body in [
            "*** Move to: b\n",
            "*** Add File: a\n*** Move to: b\n",
            "*** Delete File: a\n*** Move to: b\n",
            "*** Update File: a\n@@\n-x\n+y\n*** Move to: b\n",
            "*** Update File: a\n*** Move to: b\n*** Move to: c\n",
        ] {
            assert!(
                matches!(err(body), ParseError::MisplacedMove { .. }),
                "{body:?}"
            );
        }
    }

    #[test]
    fn empty_updates_are_rejected() {
        assert!(matches!(
            err("*** Update File: a\n*** Delete File: b\n"),
            ParseError::EmptyUpdate { line: 2, .. }
        ));
        assert!(matches!(
            err("*** Update File: a\n"),
            ParseError::EmptyUpdate { line: 2, .. }
        ));
    }

    #[test]
    fn unsafe_paths_are_rejected_wherever_they_appear() {
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            ".git/config",
            "src/.GIT/hooks",
            "a\\b",
            "",
            " a",
            "a ",
            "a\tb",
            "a\u{1b}[31m",
            "a\0b",
        ] {
            for body in [
                format!("*** Add File: {bad}\n+x\n"),
                format!("*** Delete File: {bad}\n"),
                format!("*** Update File: {bad}\n@@\n-x\n"),
                format!("*** Update File: ok\n*** Move to: {bad}\n"),
            ] {
                assert!(
                    matches!(err(&body), ParseError::UnsafePath { .. }),
                    "{bad:?} in {body:?}"
                );
            }
        }
        let long = "a".repeat(MAX_PATH_BYTES + 1);
        assert!(matches!(
            err(&format!("*** Delete File: {long}\n")),
            ParseError::UnsafePath { .. }
        ));
    }

    #[test]
    fn a_path_may_be_named_only_once() {
        for body in [
            "*** Update File: a\n@@\n-x\n*** Update File: a\n@@\n-y\n",
            "*** Delete File: a\n*** Add File: a\n+x\n",
            "*** Add File: a\n+x\n*** Delete File: a\n",
            // A move onto a path another section names.
            "*** Update File: a\n*** Move to: b\n*** Add File: b\n+x\n",
            "*** Add File: b\n+x\n*** Update File: a\n*** Move to: b\n",
            // Chained and swapped moves.
            "*** Update File: a\n*** Move to: b\n*** Update File: b\n*** Move to: c\n",
            "*** Update File: a\n*** Move to: b\n*** Update File: b\n*** Move to: a\n",
            // A file's source reused as another move's destination.
            "*** Update File: a\n*** Move to: b\n*** Update File: c\n*** Move to: a\n",
        ] {
            assert!(
                matches!(err(body), ParseError::DuplicatePath { .. }),
                "{body:?}"
            );
        }
    }

    #[test]
    fn a_move_onto_itself_is_rejected() {
        assert!(matches!(
            err("*** Update File: a\n*** Move to: a\n"),
            ParseError::MoveToSelf { line: 3, .. }
        ));
    }

    #[test]
    fn messages_never_carry_raw_control_characters() {
        let hostile = "*** Update File: a\n@@\n-x\n\u{1b}]0;pwned\u{7}\n";
        let message = err(hostile).to_string();
        assert!(!message.chars().any(char::is_control), "{message:?}");
        assert!(message.contains("\\u{1b}"), "{message}");

        let long = format!("*** Bogus: {}\n", "x".repeat(500));
        assert!(err(&long).to_string().len() < 200);
    }

    #[test]
    fn body_text_that_looks_like_a_header_is_still_body() {
        // A line inside a hunk that merely mentions a marker carries a prefix.
        ok("*** Add File: a\n+*** End Patch\n+*** Delete File: b\n");
        ok("*** Update File: a\n@@\n-*** Begin Patch\n+*** Update File: z\n");
    }
}

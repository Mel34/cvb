//! Parses a unified diff as `git diff` prints it.
//!
//! Like the `apply_patch` parser this reads structure only: which files change
//! and how, so the paths can be checked and the result committed. `git apply`
//! interprets the hunks, in a scratch copy.
//!
//! The line counts in the hunk headers (`@@ -a,b +c,d @@`) are not trusted. LLMs
//! cannot count lines and get them wrong in both directions, and a strict reader
//! would refuse most of what they write. A hunk therefore ends where the
//! structure says it ends (the next `@@`, `diff --git`, or a `--- a/...` /
//! `+++ ...` pair), its counts are recomputed from its lines, and only a header
//! whose counts differ is rewritten; `git apply` then receives the corrected
//! text and the report says so. The header counts serve only to decide whether
//! blank lines at the very end of a hunk are context or separators.
//!
//! Consequence: a diff has no end marker and the counts no longer vouch for
//! completeness, so a paste cut inside a hunk is noticed only when the hunk has
//! no lines at all. The optional review gate is the safety net for that.
//!
//! Supported: modified files, new files (with their mode), deleted files, renames
//! (with or without changes), and `\ No newline at end of file`. Refused with a
//! reason: binary patches, copies, symlinks and submodules, mode changes, merge
//! (combined) diffs, quoted file names, and paths without the `a/` and `b/`
//! prefixes.

use std::collections::{HashMap, HashSet};

use crate::patch::parse::{
    Change, FileOp, Format, ParseError, ParsedPatch, checked_path, show,
};

const DEV_NULL: &str = "/dev/null";

#[derive(Default)]
struct FileDiff {
    /// 1-based line where the file's section starts.
    line: usize,
    /// The path from the `diff --git` line, when both halves are the same.
    header_path: Option<String>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    new_file_mode: Option<String>,
    deleted_file_mode: Option<String>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    copy: bool,
    quoted: bool,
    /// Paths from the `---` and `+++` lines (prefix removed), `None` for /dev/null.
    old_path: Option<String>,
    new_path: Option<String>,
    old_null: bool,
    new_null: bool,
    /// A `---` / `+++` pair was seen.
    has_files: bool,
    hunks: usize,
}

fn bad(line: usize, message: &str) -> ParseError {
    ParseError::BadDiff { line, message: message.to_string() }
}

fn unsupported(line: usize, what: &str) -> ParseError {
    ParseError::Unsupported { line, what: what.to_string() }
}

/// `a/P b/P` (the rest of a `diff --git` line) to `P`, when both halves agree.
fn same_path(rest: &str) -> Option<String> {
    let body = rest.strip_prefix("a/")?;
    let len = body.len();
    if len < 4 || (len - 3) % 2 != 0 {
        return None;
    }
    let half = (len - 3) / 2;
    let first = body.get(..half)?;
    let tail = body.get(half..)?.strip_prefix(" b/")?;
    (tail == first).then(|| first.to_string())
}

/// `@@ -a,b +c,d @@tail`.
struct HunkHeader<'a> {
    old_start: &'a str,
    new_start: &'a str,
    old_count: usize,
    new_count: usize,
    /// Everything after the closing `@@` (the function context git adds).
    tail: &'a str,
}

/// `98` or `98,9` to the start line and the line count (1 when omitted).
fn range(text: &str) -> Option<(&str, usize)> {
    match text.split_once(',') {
        Some((start, count)) => {
            start.parse::<usize>().ok()?;
            Some((start, count.parse().ok()?))
        }
        None => {
            text.parse::<usize>().ok()?;
            Some((text, 1))
        }
    }
}

fn parse_hunk_header(header: &str) -> Option<HunkHeader<'_>> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, tail) = rest.split_once(" @@")?;
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some(HunkHeader { old_start, new_start, old_count, new_count, tail })
}

/// A `---` or `+++` operand: path without the timestamp git tools may append.
fn operand(text: &str) -> &str {
    text.split('\t').next().unwrap_or(text)
}

fn strip_prefix_ab(path: &str, prefix: &str, line: usize) -> Result<(Option<String>, bool), ParseError> {
    let path = operand(path);
    if path == DEV_NULL {
        return Ok((None, true));
    }
    if path.starts_with('"') {
        return Err(unsupported(line, "quoted file names (use a path without special characters)"));
    }
    match path.strip_prefix(prefix) {
        Some(rest) if !rest.is_empty() => Ok((Some(rest.to_string()), false)),
        _ => Err(bad(
            line,
            &format!(
                "path {} lacks the '{prefix}' prefix; use the default prefixes of git diff",
                show(path)
            ),
        )),
    }
}

/// Does a file header (`--- a/...` or `--- /dev/null`, then `+++ `) start at `at`?
fn starts_file_header(lines: &[(usize, &str)], at: usize) -> bool {
    let line = lines[at].1;
    (line.starts_with("--- a/") || line.starts_with("--- /dev/null"))
        && lines.get(at + 1).is_some_and(|next| next.1.starts_with("+++ "))
}

/// One hunk, read by structure, with its counts recomputed.
struct Hunk {
    /// Index of the first line after the hunk.
    end: usize,
    /// The lines to hand to `git apply`: the header (rewritten when its counts
    /// were wrong), then the body.
    emit: Vec<String>,
    corrected: bool,
}

/// Reads the hunk whose `@@` header is at `lines[at]` (`raws` holds the same
/// lines with their original line endings).
fn read_hunk(lines: &[(usize, &str)], raws: &[&str], at: usize) -> Result<Hunk, ParseError> {
    let (number, header_text) = lines[at];
    let Some(header) = parse_hunk_header(header_text) else {
        return Err(bad(number, &format!("malformed hunk header: {}", show(header_text))));
    };

    let mut body: Vec<usize> = Vec::new();
    let mut i = at + 1;
    while let Some(&(n, text)) = lines.get(i) {
        if text.starts_with("@@") || text.starts_with("diff --git ") || starts_file_header(lines, i) {
            break;
        }
        match text.chars().next() {
            // An empty line is a context line whose blank was stripped in transit.
            Some(' ' | '-' | '+' | '\\') | None => body.push(i),
            Some(_) => {
                return Err(bad(n, &format!("line does not belong to the hunk: {}", show(text))));
            }
        }
        i += 1;
    }

    let tally = |indices: &[usize]| {
        indices.iter().fold((0usize, 0usize), |(old, new), &k| {
            match lines[k].1.chars().next() {
                Some('-') => (old + 1, new),
                Some('+') => (old, new + 1),
                Some('\\') => (old, new),
                _ => (old + 1, new + 1),
            }
        })
    };
    // Blank lines at the very end are context only when the header has room for
    // them; otherwise they separate this hunk from what follows.
    let blanks = body.iter().rev().take_while(|&&k| lines[k].1.is_empty()).count();
    let core = body.len() - blanks;
    let (core_old, core_new) = tally(&body[..core]);
    let room = header.old_count.saturating_sub(core_old).min(header.new_count.saturating_sub(core_new));
    body.truncate(core + blanks.min(room));
    if body.is_empty() {
        return Err(ParseError::Truncated { line: number });
    }

    let (old, new) = tally(&body);
    let corrected = (old, new) != (header.old_count, header.new_count);
    let mut emit = Vec::with_capacity(body.len() + 1);
    emit.push(if corrected {
        let cr = if raws[at].ends_with('\r') { "\r" } else { "" };
        format!(
            "@@ -{},{old} +{},{new} @@{}{cr}",
            header.old_start, header.new_start, header.tail
        )
    } else {
        raws[at].to_string()
    });
    for &k in &body {
        let raw = raws[k];
        // Git reads an empty line as blank context; say so explicitly.
        emit.push(if lines[k].1.is_empty() {
            if raw.ends_with('\r') { " \r".to_string() } else { " ".to_string() }
        } else {
            raw.to_string()
        });
    }
    Ok(Hunk { end: i, emit, corrected })
}

/// Parses `text`, which must be one complete diff.
pub(crate) fn parse_git_diff(text: &str) -> Result<ParsedPatch, ParseError> {
    let mut raws: Vec<&str> = text.split('\n').collect();
    // `split` leaves an empty last element when the text ends with a line feed.
    if raws.last().is_some_and(|l| l.is_empty()) {
        raws.pop();
    }
    let lines: Vec<(usize, &str)> = raws
        .iter()
        .enumerate()
        .map(|(i, l)| (i + 1, l.strip_suffix('\r').unwrap_or(l)))
        .collect();
    // The text handed to `git apply`: the pasted lines, hunk headers corrected.
    let mut out: Vec<String> = Vec::with_capacity(raws.len());
    let mut corrected_headers = 0usize;

    let mut files: Vec<FileDiff> = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut i = 0;
    while i < lines.len() {
        let (n, line) = lines[i];
        if line.trim().is_empty() {
            i += 1;
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take());
            current = Some(FileDiff {
                line: n,
                header_path: same_path(rest),
                quoted: rest.contains('"'),
                ..FileDiff::default()
            });
            out.push(raws[i].to_string());
            i += 1;
        } else if let Some(old) = line.strip_prefix("--- ") {
            let Some(&(m, next)) = lines.get(i + 1) else {
                return Err(ParseError::Truncated { line: n });
            };
            let Some(new) = next.strip_prefix("+++ ") else {
                return Err(bad(m, "expected a '+++ ' line after the '--- ' line"));
            };
            if current.as_ref().is_none_or(|f| f.has_files) {
                files.extend(current.take());
                current = Some(FileDiff { line: n, ..FileDiff::default() });
            }
            let file = current.as_mut().expect("just ensured");
            (file.old_path, file.old_null) = strip_prefix_ab(old, "a/", n)?;
            (file.new_path, file.new_null) = strip_prefix_ab(new, "b/", m)?;
            file.has_files = true;
            out.push(raws[i].to_string());
            out.push(raws[i + 1].to_string());
            i += 2;
            while lines.get(i).is_some_and(|l| l.1.starts_with("@@")) {
                if !lines[i].1.starts_with("@@ ") {
                    return Err(unsupported(lines[i].0, "combined (merge) diffs"));
                }
                let hunk = read_hunk(&lines, &raws, i)?;
                i = hunk.end;
                file.hunks += 1;
                corrected_headers += usize::from(hunk.corrected);
                out.extend(hunk.emit);
            }
        } else if let Some(file) = current.as_mut() {
            let value = |key: &str| line.strip_prefix(key).map(|v| v.trim().to_string());
            if let Some(v) = value("old mode ") {
                file.old_mode = Some(v);
            } else if let Some(v) = value("new mode ") {
                file.new_mode = Some(v);
            } else if let Some(v) = value("new file mode ") {
                file.new_file_mode = Some(v);
            } else if let Some(v) = value("deleted file mode ") {
                file.deleted_file_mode = Some(v);
            } else if let Some(v) = value("rename from ") {
                file.rename_from = Some(v);
            } else if let Some(v) = value("rename to ") {
                file.rename_to = Some(v);
            } else if line.starts_with("copy from ") || line.starts_with("copy to ") {
                file.copy = true;
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                // Its data lines would only confuse the reader: refuse right here.
                return Err(unsupported(n, "binary patches"));
            } else if line.starts_with("similarity index ")
                || line.starts_with("dissimilarity index ")
                || line.starts_with("index ")
            {
                // Informational.
            } else {
                return Err(bad(n, &format!("unexpected line: {}", show(line))));
            }
            out.push(raws[i].to_string());
            i += 1;
        } else {
            return Err(bad(n, &format!("unexpected line: {}", show(line))));
        }
    }
    files.extend(current);
    if files.is_empty() {
        return Err(ParseError::NoFiles);
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut ops = Vec::new();
    let mut modes = HashMap::new();
    for file in &files {
        convert(file, &mut seen, &mut ops, &mut modes)?;
    }
    let mut notes = Vec::new();
    let mut normalized = None;
    if corrected_headers > 0 {
        let s = if corrected_headers == 1 { "" } else { "s" };
        notes.push(format!(
            "Note: corrected the line counts of {corrected_headers} hunk header{s} \
             (the counts in the diff did not match its lines)."
        ));
        normalized = Some(format!("{}\n", out.join("\n")));
    }
    Ok(ParsedPatch { ops, format: Format::GitDiff, modes, normalized, notes })
}

/// `100755` to the permission bits; only regular-file modes are accepted.
fn file_mode(text: &str, line: usize) -> Result<u32, ParseError> {
    let value = u32::from_str_radix(text, 8)
        .map_err(|_| bad(line, &format!("malformed file mode: {}", show(text))))?;
    match value >> 9 {
        0o100 => Ok(value & 0o777),
        0o120 => Err(unsupported(line, "symbolic links")),
        0o160 => Err(unsupported(line, "submodules")),
        _ => Err(unsupported(line, &format!("file mode {text}"))),
    }
}

fn convert(
    file: &FileDiff,
    seen: &mut HashSet<String>,
    ops: &mut Vec<FileOp>,
    modes: &mut HashMap<String, u32>,
) -> Result<(), ParseError> {
    let line = file.line;
    if file.copy {
        return Err(unsupported(line, "copies (copy from / copy to)"));
    }
    if file.quoted {
        return Err(unsupported(line, "quoted file names (use a path without special characters)"));
    }
    let any_path = |first: &Option<String>| {
        first.clone().or_else(|| file.header_path.clone())
    };

    let is_add = file.new_file_mode.is_some() || file.old_null;
    let is_delete = file.deleted_file_mode.is_some() || file.new_null;
    if is_add && is_delete {
        return Err(bad(line, "a file cannot be both created and deleted"));
    }
    let renamed = file.rename_from.is_some() || file.rename_to.is_some();
    if (is_add || is_delete) && renamed {
        return Err(bad(line, "a rename cannot also create or delete a file"));
    }

    let no_path = || bad(line, "cannot tell which file this section changes");
    if is_add {
        let path = any_path(&file.new_path).ok_or_else(no_path)?;
        let path = checked_path(&path, line, seen)?;
        if let Some(mode) = &file.new_file_mode {
            modes.insert(path.clone(), file_mode(mode, line)?);
        }
        ops.push(FileOp { path, change: Change::Add, line });
    } else if is_delete {
        let path = any_path(&file.old_path).ok_or_else(no_path)?;
        let path = checked_path(&path, line, seen)?;
        if let Some(mode) = &file.deleted_file_mode {
            file_mode(mode, line)?;
        }
        ops.push(FileOp { path, change: Change::Delete, line });
    } else if renamed {
        let (Some(from), Some(to)) = (&file.rename_from, &file.rename_to) else {
            return Err(bad(line, "a rename needs both 'rename from' and 'rename to'"));
        };
        if file.old_mode.is_some() || file.new_mode.is_some() {
            return Err(unsupported(line, "a mode change (chmod) together with a rename"));
        }
        if file.has_files
            && (file.old_path.as_deref() != Some(from.as_str())
                || file.new_path.as_deref() != Some(to.as_str()))
        {
            return Err(bad(line, "the rename lines and the '---' / '+++' lines name different files"));
        }
        let from = checked_path(from, line, seen)?;
        let to = checked_path(to, line, seen)?;
        ops.push(FileOp { path: from, change: Change::Move { to }, line });
    } else {
        if file.old_mode.is_some() || file.new_mode.is_some() {
            return Err(unsupported(line, "a mode change (chmod); apply it with chmod"));
        }
        let path = any_path(&file.new_path).ok_or_else(no_path)?;
        if file.has_files && file.old_path.as_deref() != file.new_path.as_deref() {
            return Err(bad(line, "the '---' and '+++' lines name different files (not a rename?)"));
        }
        let path = checked_path(&path, line, seen)?;
        if file.hunks == 0 {
            return Err(ParseError::EmptyUpdate { line, path });
        }
        ops.push(FileOp { path, change: Change::Update, line });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> ParsedPatch {
        parse_git_diff(text).unwrap_or_else(|e| panic!("{e}\n{text}"))
    }

    fn err(text: &str) -> ParseError {
        parse_git_diff(text).expect_err(text)
    }

    fn op(path: &str, change: Change, line: usize) -> FileOp {
        FileOp { path: path.to_string(), change, line }
    }

    const MODIFY: &str = "diff --git a/src/a.rs b/src/a.rs\nindex 111..222 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n";

    #[test]
    fn a_modified_file_is_an_update() {
        let parsed = ok(MODIFY);
        assert_eq!(parsed.format, Format::GitDiff);
        assert_eq!(parsed.ops, vec![op("src/a.rs", Change::Update, 1)]);
        assert!(parsed.modes.is_empty());
    }

    #[test]
    fn a_new_file_is_an_add_and_keeps_its_mode() {
        let parsed = ok(
            "diff --git a/bin/run.sh b/bin/run.sh\nnew file mode 100755\nindex 000..111\n--- /dev/null\n+++ b/bin/run.sh\n@@ -0,0 +1,2 @@\n+#!/bin/sh\n+echo hi\n",
        );
        assert_eq!(parsed.ops, vec![op("bin/run.sh", Change::Add, 1)]);
        assert_eq!(parsed.modes.get("bin/run.sh"), Some(&0o755));
    }

    #[test]
    fn an_empty_new_file_has_no_hunks_and_is_still_an_add() {
        let parsed = ok("diff --git a/empty b/empty\nnew file mode 100644\nindex 0000000..e69de29\n");
        assert_eq!(parsed.ops, vec![op("empty", Change::Add, 1)]);
        assert_eq!(parsed.modes.get("empty"), Some(&0o644));
    }

    #[test]
    fn a_deleted_file_is_a_delete() {
        let parsed = ok(
            "diff --git a/old.txt b/old.txt\ndeleted file mode 100644\nindex 111..000\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
        );
        assert_eq!(parsed.ops, vec![op("old.txt", Change::Delete, 1)]);
    }

    #[test]
    fn a_rename_with_and_without_changes_is_a_move() {
        let pure = ok("diff --git a/a.txt b/b.txt\nsimilarity index 100%\nrename from a.txt\nrename to b.txt\n");
        assert_eq!(pure.ops, vec![op("a.txt", Change::Move { to: "b.txt".into() }, 1)]);
        let edited = ok(
            "diff --git a/a.txt b/d/b.txt\nsimilarity index 80%\nrename from a.txt\nrename to d/b.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/d/b.txt\n@@ -1 +1 @@\n-x\n+y\n",
        );
        assert_eq!(edited.ops, vec![op("a.txt", Change::Move { to: "d/b.txt".into() }, 1)]);
    }

    #[test]
    fn several_files_keep_patch_order() {
        let text = format!(
            "{MODIFY}diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+n\ndiff --git a/g.txt b/g.txt\ndeleted file mode 100644\n--- a/g.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-g\n"
        );
        let parsed = ok(&text);
        let paths: Vec<&str> = parsed.ops.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "n.txt", "g.txt"]);
    }

    #[test]
    fn a_plain_unified_diff_without_git_headers_is_accepted() {
        let parsed = ok("--- a/x.txt\t2026-01-01 00:00:00\n+++ b/x.txt\t2026-01-02 00:00:00\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(parsed.ops, vec![op("x.txt", Change::Update, 1)]);
        let new = ok("--- /dev/null\n+++ b/y.txt\n@@ -0,0 +1 @@\n+y\n");
        assert_eq!(new.ops, vec![op("y.txt", Change::Add, 1)]);
        assert!(new.modes.is_empty());
    }

    #[test]
    fn two_plain_diffs_in_a_row_are_two_files() {
        let parsed = ok("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n");
        assert_eq!(parsed.ops.len(), 2);
    }

    #[test]
    fn dashes_in_content_are_content_and_only_a_real_file_header_ends_a_hunk() {
        // The removed line "-- note" prints as "--- note"; it is not a file header.
        let parsed = ok("diff --git a/s.sql b/s.sql\n--- a/s.sql\n+++ b/s.sql\n@@ -1,2 +1,1 @@\n--- note\n keep\n");
        assert_eq!(parsed.ops, vec![op("s.sql", Change::Update, 1)]);
        assert_eq!(parsed.normalized, None, "the counts were right");
        // A removed line that looks like "+++ " is content as well.
        ok("diff --git a/s b/s\n--- a/s\n+++ b/s\n@@ -1,2 +1,1 @@\n-x\n-+++ y\n+z\n");
        // A `--- a/...` line followed by `+++ ` does start the next file, even when the
        // hunk above claims more lines than it has.
        let two = ok("--- a/x\n+++ b/x\n@@ -1,9 +1,9 @@\n-a\n+b\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n");
        let paths: Vec<&str> = two.ops.iter().map(|o| o.path.as_str()).collect();
        assert_eq!(paths, ["x", "y"]);
    }

    #[test]
    fn omitted_counts_mean_one_line() {
        ok("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -3 +3 @@\n-a\n+b\n");
    }

    #[test]
    fn no_newline_markers_do_not_count_as_lines() {
        ok("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n");
    }

    #[test]
    fn an_empty_line_inside_a_hunk_is_a_blank_context_line() {
        ok("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@\n a\n\n-c\n+d\n");
    }

    #[test]
    fn several_hunks_and_trailing_blank_lines_are_accepted() {
        ok("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-c\n+d\n\n\n");
    }

    #[test]
    fn crlf_pastes_parse_like_lf_pastes() {
        let crlf = MODIFY.replace('\n', "\r\n");
        assert_eq!(ok(&crlf).ops, ok(MODIFY).ops);
    }

    #[test]
    fn a_hunk_with_no_lines_is_truncated() {
        let full = MODIFY;
        let header_end = full.find("@@ -1,3 +1,3 @@").unwrap() + "@@ -1,3 +1,3 @@\n".len();
        // Everything up to and including the header line (a cut inside the header
        // is a malformed header; after it, no lines) is refused.
        for cut in full.find("@@").unwrap()..header_end {
            let result = parse_git_diff(&full[..cut]);
            assert!(result.is_err(), "accepted a diff cut inside or right after a hunk header: {:?}", &full[..cut]);
        }
        let ParseError::Truncated { line } = err(&full[..header_end]) else { panic!() };
        assert_eq!(line, 5);
        let message = err(&full[..header_end]).to_string();
        assert!(message.starts_with("incomplete paste") && message.contains("4 MB"), "{message}");
        assert!(parse_git_diff(full).is_ok());
        assert!(parse_git_diff(&full[..full.len() - 1]).is_ok(), "no final line feed is normal");
    }

    /// The price of not trusting counts, pinned: once a hunk has a line, a cut
    /// later in it cannot be noticed. The review gate is the safety net.
    #[test]
    fn a_cut_after_the_first_line_of_a_hunk_is_the_known_blind_spot() {
        let cut = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,4 +1,4 @@\n a\n-b\n+c = compute(1, 2";
        assert!(parse_git_diff(cut).is_ok());
    }

    #[test]
    fn a_diff_that_ends_after_the_file_header_is_truncated() {
        assert!(matches!(err("--- a/x\n"), ParseError::Truncated { .. }));
    }

    #[test]
    fn right_counts_leave_the_diff_untouched() {
        let parsed = ok(MODIFY);
        assert_eq!(parsed.normalized, None);
        assert!(parsed.notes.is_empty());
    }

    #[test]
    fn counts_that_are_too_small_are_corrected() {
        // Reported case: the header says 7 and 7, the hunk has 8 and 8 lines.
        let text = "diff --git a/f.zig b/f.zig\n--- a/f.zig\n+++ b/f.zig\n@@ -15,7 +15,7 @@ pub fn Pool(comptime T: type) type {\n a\n b\n-old\n+new\n c\n\n d\n e\n f\n";
        let parsed = ok(text);
        let fixed = parsed.normalized.as_deref().expect("corrected");
        assert!(!fixed.contains("@@ -15,7 +15,7 @@"), "{fixed}");
        assert!(fixed.contains("\n@@ -15,8 +15,8 @@ pub fn Pool(comptime T: type) type {\n"), "{fixed}");
        assert_eq!(parsed.notes.len(), 1);
        assert!(parsed.notes[0].contains("corrected the line counts of 1 hunk header "), "{:?}", parsed.notes);
    }

    #[test]
    fn counts_that_are_too_large_are_corrected() {
        // Reported case: the header says 9 old lines, there are 7 (new: 11, right).
        let text = "diff --git a/t.zig b/t.zig\n--- a/t.zig\n+++ b/t.zig\n@@ -98,9 +98,11 @@ test \"x\" {\n     try a();\n     try b();\n\n+    // c\n+    try d();\n     var results: [2]u32 = undefined;\n-    try collect();\n+    for (&results) |*slot| {\n+        slot.* = 1;\n+    }\n     try e();\n }\n";
        let parsed = ok(text);
        let fixed = parsed.normalized.as_deref().expect("corrected");
        assert!(fixed.contains("\n@@ -98,7 +98,11 @@ test \"x\" {\n"), "{fixed}");
        assert_eq!(parsed.ops, vec![op("t.zig", Change::Update, 1)]);
    }

    #[test]
    fn only_wrong_headers_are_rewritten_and_everything_else_is_kept_byte_for_byte() {
        let text = "diff --git a/a b/a\nindex 1..2 100644\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n x\n-y\n+z\n@@ -10,5 +10,5 @@ ctx\n-p\n+q\n";
        let fixed = ok(text).normalized.expect("second header is wrong");
        let expected = "diff --git a/a b/a\nindex 1..2 100644\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n x\n-y\n+z\n@@ -10,1 +10,1 @@ ctx\n-p\n+q\n";
        assert_eq!(fixed, expected);
    }

    #[test]
    fn corrections_keep_crlf_line_endings() {
        let text = "diff --git a/a b/a\r\n--- a/a\r\n+++ b/a\r\n@@ -1,5 +1,5 @@\r\n-y\r\n+z\r\n";
        let fixed = ok(text).normalized.expect("corrected");
        assert_eq!(fixed, "diff --git a/a b/a\r\n--- a/a\r\n+++ b/a\r\n@@ -1,1 +1,1 @@\r\n-y\r\n+z\r\n");
    }

    #[test]
    fn blank_lines_at_the_end_of_a_hunk_are_context_only_when_the_header_has_room() {
        // Header says 3 and 3: the stripped blank last line is a context line.
        let kept = ok("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n x\n-y\n+z\n\n");
        assert_eq!(kept.normalized, None, "counts were right, the blank is context");
        // Blank lines between two files are separators, not context.
        let split = ok("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-y\n+z\n\n\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-p\n+q\n");
        assert_eq!(split.ops.len(), 2);
        assert_eq!(split.normalized, None, "the separators were dropped without changing a header");
    }

    #[test]
    fn a_trailing_blank_needs_room_on_both_sides_of_the_header_to_count_as_context() {
        // Old side has room (header 2, lines 1) but the new side has none (3 of 3):
        // the blank is a separator, so the header becomes -1,1 +1,3, not -1,2 +1,4.
        let fixed = ok("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,3 @@\n x\n+z\n+w\n\n")
            .normalized
            .expect("old count was wrong");
        assert!(fixed.contains("@@ -1,1 +1,3 @@"), "{fixed}");
        assert!(!fixed.ends_with(" \n"), "{fixed:?}");
    }

    #[test]
    fn a_blank_context_line_in_the_middle_is_written_as_a_space() {
        let fixed = ok("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,9 +1,9 @@\n x\n\n-y\n+z\n").normalized.unwrap();
        assert!(fixed.contains("\n x\n \n-y\n"), "{fixed:?}");
    }

    #[test]
    fn a_foreign_line_inside_a_hunk_is_still_rejected() {
        assert!(matches!(
            err("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n a\nhello\n"),
            ParseError::BadDiff { .. }
        ));
    }

    #[test]
    fn unsupported_things_say_what_they_are() {
        let cases = [
            ("diff --git a/x b/x\nindex 1..2\nBinary files a/x and b/x differ\n", "binary"),
            ("diff --git a/x b/x\nGIT binary patch\nliteral 1\n", "binary"),
            ("diff --git a/x b/y\ncopy from x\ncopy to y\n", "copies"),
            ("diff --git a/x b/x\nold mode 100644\nnew mode 100755\n", "mode change"),
            ("diff --git a/l b/l\nnew file mode 120000\n--- /dev/null\n+++ b/l\n@@ -0,0 +1 @@\n+target\n", "symbolic"),
            ("diff --git a/m b/m\nnew file mode 160000\n", "submodule"),
            ("diff --git \"a/x y\" \"b/x y\"\n--- \"a/x y\"\n+++ \"b/x y\"\n@@ -1 +1 @@\n-a\n+b\n", "quoted"),
            ("diff --cc x\n", "unexpected"),
            ("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@@ -1,1 -1,1 +1,1 @@@\n", "merge"),
        ];
        for (text, word) in cases {
            let message = err(text).to_string();
            assert!(message.contains(word), "{word}: {message}");
        }
    }

    #[test]
    fn paths_need_the_default_prefixes() {
        let message = err("diff --git x y\n--- x\n+++ y\n@@ -1 +1 @@\n-a\n+b\n").to_string();
        assert!(message.contains("prefix"), "{message}");
    }

    #[test]
    fn unsafe_paths_are_rejected_like_in_the_other_format() {
        for path in ["../x", "/etc/passwd", ".git/config", "a/../../b"] {
            let text = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n");
            let result = parse_git_diff(&text);
            assert!(matches!(result, Err(ParseError::UnsafePath { .. })), "{path}: {result:?}");
        }
    }

    #[test]
    fn a_path_may_be_named_once() {
        let text = format!("{MODIFY}{MODIFY}");
        assert!(matches!(err(&text), ParseError::DuplicatePath { .. }));
    }

    #[test]
    fn a_section_without_hunks_or_changes_is_an_empty_update() {
        assert!(matches!(err("diff --git a/x b/x\nindex 1..1 100644\n"), ParseError::EmptyUpdate { .. }));
    }

    #[test]
    fn mismatched_names_are_rejected() {
        assert!(matches!(
            err("diff --git a/x b/y\n--- a/x\n+++ b/y\n@@ -1 +1 @@\n-a\n+b\n"),
            ParseError::BadDiff { .. }
        ));
    }

    #[test]
    fn prose_before_or_after_the_diff_is_rejected() {
        assert!(matches!(err("Here is the diff:\ndiff --git a/x b/x\n"), ParseError::BadDiff { .. }));
        let trailing = format!("{MODIFY}That should do it.\n");
        assert!(matches!(err(&trailing), ParseError::BadDiff { .. }));
    }

    #[test]
    fn parse_any_picks_the_format_by_the_first_line() {
        use crate::patch::parse::parse_any;
        assert_eq!(parse_any(MODIFY).unwrap().format, Format::GitDiff);
        let patch = "*** Begin Patch\n*** Delete File: x\n*** End Patch\n";
        assert_eq!(parse_any(patch).unwrap().format, Format::ApplyPatch);
        assert_eq!(parse_any("\n\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").unwrap().format, Format::GitDiff);
        assert!(matches!(parse_any("hello"), Err(ParseError::MissingBegin)));
    }

    #[test]
    fn same_path_needs_both_halves_to_agree() {
        assert_eq!(same_path("a/x b/x"), Some("x".into()));
        assert_eq!(same_path("a/dir/f.rs b/dir/f.rs"), Some("dir/f.rs".into()));
        assert_eq!(same_path("a/x y b/x y"), Some("x y".into()));
        assert_eq!(same_path("a/x b/y"), None);
        assert_eq!(same_path("x y"), None);
    }
}

//! Parses a unified diff as `git diff` prints it.
//!
//! Like the `apply_patch` parser this reads structure only: which files change
//! and how, so the paths can be checked and the result committed. `git apply`
//! interprets the hunks, in a scratch copy. One thing is read deeper than in the
//! other format: the line counts of every hunk header (`@@ -a,b +c,d @@`). A diff
//! has no end marker, so those counts are the only way to see that a paste was cut
//! short, and they decide where a hunk ends (a removed line may itself start with
//! `--`).
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

fn range_count(range: &str) -> Option<usize> {
    match range.split_once(',') {
        Some((start, count)) => {
            start.parse::<usize>().ok()?;
            count.parse().ok()
        }
        None => {
            range.parse::<usize>().ok()?;
            Some(1)
        }
    }
}

/// `@@ -a,b +c,d @@ ...` to `(b, d)`.
fn hunk_counts(header: &str) -> Option<(usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    Some((range_count(old)?, range_count(new)?))
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

/// Reads one hunk body whose header is at `lines[at]`; returns the index after it.
fn read_hunk(lines: &[(usize, &str)], at: usize) -> Result<usize, ParseError> {
    let (number, header) = lines[at];
    let Some((mut old_left, mut new_left)) = hunk_counts(header) else {
        return Err(bad(number, &format!("malformed hunk header: {}", show(header))));
    };
    let mut i = at + 1;
    while old_left > 0 || new_left > 0 {
        let Some(&(n, text)) = lines.get(i) else {
            return Err(ParseError::Truncated { line: lines.last().map_or(number, |l| l.0) });
        };
        let counts_off = || {
            bad(n, "the hunk has more lines than its '@@' header says (a line count is wrong)")
        };
        match text.chars().next() {
            // An empty line is a context line whose blank was stripped in transit.
            Some(' ') | None => {
                old_left = old_left.checked_sub(1).ok_or_else(counts_off)?;
                new_left = new_left.checked_sub(1).ok_or_else(counts_off)?;
            }
            Some('-') => old_left = old_left.checked_sub(1).ok_or_else(counts_off)?,
            Some('+') => new_left = new_left.checked_sub(1).ok_or_else(counts_off)?,
            Some('\\') => {}
            Some(_) => {
                return Err(bad(
                    n,
                    &format!("line does not belong to the hunk: {}", show(text)),
                ));
            }
        }
        i += 1;
    }
    while lines.get(i).is_some_and(|l| l.1.starts_with('\\')) {
        i += 1;
    }
    Ok(i)
}

/// Parses `text`, which must be one complete diff.
pub(crate) fn parse_git_diff(text: &str) -> Result<ParsedPatch, ParseError> {
    let mut lines: Vec<(usize, &str)> = text
        .split('\n')
        .enumerate()
        .map(|(i, l)| (i + 1, l.strip_suffix('\r').unwrap_or(l)))
        .collect();
    // `split` leaves an empty last element when the text ends with a line feed.
    if lines.last().is_some_and(|l| l.1.is_empty()) {
        lines.pop();
    }

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
            i += 2;
            while lines.get(i).is_some_and(|l| l.1.starts_with("@@")) {
                if !lines[i].1.starts_with("@@ ") {
                    return Err(unsupported(lines[i].0, "combined (merge) diffs"));
                }
                i = read_hunk(&lines, i)?;
                file.hunks += 1;
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
    Ok(ParsedPatch { ops, format: Format::GitDiff, modes })
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
    fn counts_decide_where_a_hunk_ends_so_dashes_in_content_are_content() {
        // The removed line "-- note" prints as "--- note"; it is not a file header.
        let text = "diff --git a/s.sql b/s.sql\n--- a/s.sql\n+++ b/s.sql\n@@ -1,2 +1,1 @@\n--- note\n-- keep\n";
        // old 2 lines ("-- note", "- keep"), new 1 line is wrong: make it right.
        let _ = text;
        let text = "diff --git a/s.sql b/s.sql\n--- a/s.sql\n+++ b/s.sql\n@@ -1,2 +1,1 @@\n--- note\n keep\n";
        let parsed = ok(text);
        assert_eq!(parsed.ops, vec![op("s.sql", Change::Update, 1)]);
        // A removed line that looks like "+++ " is content as well.
        ok("diff --git a/s b/s\n--- a/s\n+++ b/s\n@@ -1,2 +1,1 @@\n-x\n-+++ y\n+z\n");
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
    fn a_paste_cut_before_the_last_line_of_a_hunk_is_never_accepted() {
        let full = MODIFY;
        let body_start = full.find("@@").unwrap();
        let last_line = full[..full.len() - 1].rfind('\n').unwrap() + 1;
        // A diff has no end marker and a terminal paste ends without a line
        // feed, so a cut inside the very last line (after its first character)
        // cannot be told from a complete paste. Every earlier cut can.
        for cut in body_start..=last_line {
            let prefix = &full[..cut];
            let result = parse_git_diff(prefix);
            assert!(result.is_err(), "accepted a truncated diff: {prefix:?}");
        }
        assert!(parse_git_diff(full).is_ok());
        assert!(parse_git_diff(&full[..full.len() - 1]).is_ok(), "no final line feed is normal");
    }

    /// The limit above, pinned: this is why the review gate matters for diffs.
    #[test]
    fn a_cut_inside_the_last_line_is_the_known_blind_spot() {
        let cut = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b = compute(1, 2";
        assert!(parse_git_diff(cut).is_ok());
    }

    #[test]
    fn truncation_is_reported_as_an_incomplete_paste() {
        let cut = &MODIFY[..MODIFY.len() - "three\n".len() - 1];
        let ParseError::Truncated { line } = err(cut) else { panic!("{cut:?}") };
        assert!(line >= 6, "{line}");
        let message = err(cut).to_string();
        assert!(message.starts_with("incomplete paste") && message.contains("4 MB"), "{message}");
    }

    #[test]
    fn a_diff_that_ends_after_the_file_header_is_truncated() {
        assert!(matches!(err("--- a/x\n"), ParseError::Truncated { .. }));
    }

    #[test]
    fn wrong_counts_are_rejected() {
        // More lines than the header says.
        let ParseError::BadDiff { message, .. } =
            err("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n+c\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-d\n+e\n")
        else {
            panic!()
        };
        assert!(message.contains("unexpected") || message.contains("lines"), "{message}");
        // A foreign line inside the hunk.
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

//! Reads the hunks of a patch, in either format, with their position in the text.
//!
//! The failed-hunk picker needs to know which hunks of a patch do not match the
//! files, what each one expects, and where its lines sit in the pasted text so a
//! corrected hunk can be spliced back in. This reader is structural only: it never
//! decides whether a patch is acceptable (`parse` does that).

use std::ops::Range;

use crate::patch::already::found;
use crate::patch::parse::Format;

const BEGIN: &str = "*** Begin Patch";
const UPDATE: &str = "*** Update File: ";

/// What a hunk line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    Context,
    Remove,
    Add,
}

impl LineKind {
    pub(crate) fn prefix(self) -> char {
        match self {
            LineKind::Context => ' ',
            LineKind::Remove => '-',
            LineKind::Add => '+',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HunkLine {
    pub kind: LineKind,
    pub text: String,
}

/// One hunk of an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hunk {
    pub lines: Vec<HunkLine>,
    /// Index (0-based, in the lines of the patch text) of the `@@` line, if the
    /// hunk has one. The first hunk of an `apply_patch` update may have none.
    pub header: Option<usize>,
    /// Indexes of the hunk's body lines.
    pub body: Range<usize>,
    /// `apply_patch`: the text after `@@`, used by the tool to locate the hunk.
    pub hint: Option<String>,
    /// Diff: the 1-based first line of the old side stated in the header.
    pub old_start: Option<usize>,
    /// The header's text after the closing `@@` of a diff (function context).
    pub tail: String,
}

impl Hunk {
    /// The lines the hunk expects to find: context and removed lines, in order.
    pub(crate) fn old_side(&self) -> Vec<(LineKind, &str)> {
        self.lines
            .iter()
            .filter(|l| l.kind != LineKind::Add)
            .map(|l| (l.kind, l.text.as_str()))
            .collect()
    }

    pub(crate) fn old_texts(&self) -> Vec<String> {
        self.old_side().into_iter().map(|(_, t)| t.to_string()).collect()
    }
}

/// The hunks of one updated file, under the path the patch reads it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilePatch {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

/// All hunks of `text`. A patch with carriage returns is not read (an empty list):
/// the picker does not handle CRLF patches.
pub(crate) fn read(text: &str, format: Format) -> Vec<FilePatch> {
    if text.contains('\r') {
        return Vec::new();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    match format {
        Format::ApplyPatch => read_envelope(&lines),
        Format::GitDiff => read_diff(&lines),
    }
}

fn read_envelope(lines: &[&str]) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut open = false;
    let mut started = false;
    for (index, line) in lines.iter().enumerate() {
        let line = *line;
        if !started {
            started = line == BEGIN;
            continue;
        }
        if let Some(path) = line.strip_prefix(UPDATE) {
            files.push(FilePatch { path: path.to_string(), hunks: Vec::new() });
            open = true;
        } else if line.starts_with("*** ") && line != "*** End of File" {
            open = line.starts_with("*** Move to: ") && open;
        } else if open {
            let hunks = &mut files.last_mut().expect("open implies a file").hunks;
            if let Some(after) = line.strip_prefix("@@") {
                let hint = after.trim();
                hunks.push(Hunk {
                    lines: Vec::new(),
                    header: Some(index),
                    body: index + 1..index + 1,
                    hint: (!hint.is_empty()).then(|| hint.to_string()),
                    old_start: None,
                    tail: String::new(),
                });
                continue;
            }
            if line == "*** End of File" {
                continue;
            }
            let (kind, text) = if let Some(rest) = line.strip_prefix('+') {
                (LineKind::Add, rest)
            } else if let Some(rest) = line.strip_prefix('-') {
                (LineKind::Remove, rest)
            } else if let Some(rest) = line.strip_prefix(' ') {
                (LineKind::Context, rest)
            } else if line.is_empty() {
                (LineKind::Context, "")
            } else {
                continue;
            };
            if hunks.is_empty() {
                // A body line with no `@@` before it: the first chunk of an update
                // may omit the header line.
                hunks.push(Hunk {
                    lines: Vec::new(),
                    header: None,
                    body: index..index,
                    hint: None,
                    old_start: None,
                    tail: String::new(),
                });
            }
            let hunk = hunks.last_mut().expect("a hunk is open");
            hunk.lines.push(HunkLine { kind, text: text.to_string() });
            hunk.body.end = index + 1;
        }
    }
    for file in &mut files {
        file.hunks.retain(|h| !h.lines.is_empty());
    }
    files.retain(|f| !f.hunks.is_empty());
    files
}

/// `@@ -a,b +c,d @@ tail` into (a, b, c, d, tail).
fn diff_header(line: &str) -> Option<(usize, usize, usize, usize, String)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, tail) = rest.split_once(" @@")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (a, b) = range(old)?;
    let (c, d) = range(new)?;
    Some((a, b, c, d, tail.to_string()))
}

fn read_diff(lines: &[&str]) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut path: Option<String> = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if line.starts_with("diff --git ") {
            path = None;
        } else if let Some(name) = line.strip_prefix("--- ") {
            path = name.strip_prefix("a/").map(str::to_string);
        } else if let Some((old_start, old_count, _, new_count, tail)) = diff_header(line) {
            let Some(path) = path.clone() else {
                index += 1;
                continue;
            };
            let header = index;
            let (mut old_left, mut new_left) = (old_count, new_count);
            let mut hunk_lines = Vec::new();
            index += 1;
            let first = index;
            while index < lines.len() && (old_left > 0 || new_left > 0) {
                let body = lines[index];
                let (kind, text) = if let Some(rest) = body.strip_prefix('+') {
                    new_left = new_left.saturating_sub(1);
                    (LineKind::Add, rest)
                } else if let Some(rest) = body.strip_prefix('-') {
                    old_left = old_left.saturating_sub(1);
                    (LineKind::Remove, rest)
                } else if let Some(rest) = body.strip_prefix(' ') {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    (LineKind::Context, rest)
                } else if body.is_empty() {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    (LineKind::Context, "")
                } else {
                    break;
                };
                hunk_lines.push(HunkLine { kind, text: text.to_string() });
                index += 1;
            }
            let hunk = Hunk {
                lines: hunk_lines,
                header: Some(header),
                body: first..index,
                hint: None,
                old_start: Some(old_start),
                tail,
            };
            // `\ No newline at end of file` belongs to the line before it; a hunk
            // that carries one is left to the tool (the picker does not offer it).
            let meta = lines.get(index).is_some_and(|l| l.starts_with('\\'));
            if !meta {
                match files.last_mut() {
                    Some(file) if file.path == path => file.hunks.push(hunk),
                    _ => files.push(FilePatch { path, hunks: vec![hunk] }),
                }
            }
            continue;
        }
        index += 1;
    }
    files
}

/// Does the hunk's old side occur in `file` the way the tools compare (exactly,
/// then ignoring trailing white space, then ignoring all of it)?
pub(crate) fn matches(hunk: &Hunk, file: &[&str]) -> bool {
    let old = hunk.old_texts();
    old.is_empty() || found(file, &old)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch\n")
    }

    fn kinds(hunk: &Hunk) -> String {
        hunk.lines.iter().map(|l| l.kind.prefix()).collect()
    }

    #[test]
    fn an_envelope_update_is_read_hunk_by_hunk() {
        let text = envelope(
            "*** Update File: a.txt\n@@ fn one\n x\n-y\n+z\n@@\n p\n+q\n*** Add File: b\n+n\n*** Update File: c\n@@\n-k\n",
        );
        let files = read(&text, Format::ApplyPatch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].hunks.len(), 2);
        let first = &files[0].hunks[0];
        assert_eq!(kinds(first), " -+");
        assert_eq!(first.hint.as_deref(), Some("fn one"));
        assert_eq!(first.header, Some(2));
        assert_eq!(first.body, 3..6);
        assert_eq!(files[0].hunks[1].hint, None);
        assert_eq!(files[0].hunks[1].body, 7..9);
        assert_eq!(files[1].path, "c");
        assert_eq!(files[1].hunks[0].lines, vec![HunkLine { kind: LineKind::Remove, text: "k".into() }]);
    }

    #[test]
    fn a_first_chunk_without_a_header_line_is_a_hunk() {
        let text = envelope("*** Update File: a\n x\n-y\n+z\n");
        let files = read(&text, Format::ApplyPatch);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.header, None);
        assert_eq!(hunk.body, 2..5);
        assert_eq!(kinds(hunk), " -+");
    }

    #[test]
    fn end_of_file_marker_keeps_the_next_section_out_of_the_hunk() {
        let text = envelope("*** Update File: a\n@@\n x\n-y\n*** End of File\n*** Update File: b\n@@\n k\n-m\n");
        let files = read(&text, Format::ApplyPatch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].hunks[0].lines.len(), 2);
        assert_eq!(files[1].path, "b");
    }

    #[test]
    fn a_hunk_without_an_old_side_always_matches() {
        let text = envelope("*** Update File: a\n@@\n+new\n");
        let hunk = &read(&text, Format::ApplyPatch)[0].hunks[0];
        assert!(matches(hunk, &["q"]));
    }

    #[test]
    fn matches_is_false_when_the_old_side_is_absent() {
        let text = envelope("*** Update File: a\n@@\n x\n-y\n");
        let hunk = &read(&text, Format::ApplyPatch)[0].hunks[0];
        assert!(matches(hunk, &["x", "y"]));
        assert!(!matches(hunk, &["x", "z"]));
    }

    #[test]
    fn a_blank_line_is_empty_context_and_end_of_file_is_a_hint_only() {
        let text = envelope("*** Update File: a\n@@\n x\n\n-y\n*** End of File\n");
        let hunk = &read(&text, Format::ApplyPatch)[0].hunks[0];
        assert_eq!(kinds(hunk), "  -");
        assert_eq!(hunk.lines[0].text, "x");
        assert_eq!(hunk.lines[1].text, "");
        assert_eq!(hunk.old_side().len(), 3);
    }

    #[test]
    fn move_to_keeps_the_hunks_and_other_operations_have_none() {
        let text = envelope(
            "*** Update File: a\n*** Move to: b\n@@\n-x\n+y\n*** Delete File: d\n*** Add File: e\n+1\n",
        );
        let files = read(&text, Format::ApplyPatch);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a");
    }

    #[test]
    fn a_patch_with_carriage_returns_is_not_read() {
        let text = "*** Begin Patch\r\n*** Update File: a\r\n@@\r\n-x\r\n+y\r\n*** End Patch\r\n";
        assert!(read(text, Format::ApplyPatch).is_empty());
    }

    const DIFF: &str = "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -2,3 +2,3 @@ fn main\n a\n-b\n+B\n c\n@@ -9,2 +9,3 @@\n x\n+y\n z\ndiff --git a/g b/g\nnew file mode 100644\n--- /dev/null\n+++ b/g\n@@ -0,0 +1,1 @@\n+n\n";

    #[test]
    fn a_diff_is_read_by_the_header_counts() {
        let files = read(DIFF, Format::GitDiff);
        assert_eq!(files.len(), 1, "a new file has no old side to match: {files:?}");
        assert_eq!(files[0].path, "f.txt");
        let hunks = &files[0].hunks;
        assert_eq!(hunks.len(), 2);
        assert_eq!(kinds(&hunks[0]), " -+ ");
        assert_eq!(hunks[0].old_start, Some(2));
        assert_eq!(hunks[0].tail, " fn main");
        assert_eq!(hunks[0].header, Some(3));
        assert_eq!(hunks[0].body, 4..8);
        assert_eq!(kinds(&hunks[1]), " + ");
        assert_eq!(hunks[1].old_start, Some(9));
        assert_eq!(hunks[1].tail, "");
    }

    #[test]
    fn a_hunk_with_a_no_newline_marker_is_left_out() {
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n";
        // The marker follows the removed line, inside the counts' reach: the read
        // stops at it and the hunk is not offered.
        assert!(read(diff, Format::GitDiff).is_empty());
    }

    #[test]
    fn matching_uses_the_tolerance_of_the_tools() {
        let hunk = &read(&envelope("*** Update File: a\n@@\n x\n-y\n"), Format::ApplyPatch)[0].hunks[0];
        assert!(matches(hunk, &["p", "x", "y", "q"]));
        assert!(matches(hunk, &["p", "x  ", "  y", "q"]));
        assert!(!matches(hunk, &["p", "x", "yy", "q"]));
        assert!(!matches(hunk, &["x"]));
    }

    #[test]
    fn old_side_leaves_out_added_lines() {
        let hunk = &read(&envelope("*** Update File: a\n@@\n x\n+n\n-y\n"), Format::ApplyPatch)[0].hunks[0];
        assert_eq!(hunk.old_texts(), vec!["x".to_string(), "y".to_string()]);
        assert_eq!(
            hunk.old_side().iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![LineKind::Context, LineKind::Remove]
        );
    }
}

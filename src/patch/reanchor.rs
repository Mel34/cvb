//! Turns a chosen candidate into a corrected patch.
//!
//! A hunk that matched nothing is rewritten so that its old side (context and
//! removed lines) is exactly the text of the chosen window of the file. Added
//! lines are kept as they were. The rest of the patch is untouched, except that a
//! diff's hunk headers are renumbered where the rewrite changed them.
//!
//! This changes what the patch says. A removed line the LLM misquoted is replaced
//! by the real line, so the real line is the one deleted. A removed line that is
//! not in the file at all is dropped: nothing is deleted for it, and its added
//! lines stay. The picker shows the difference before anything is applied.

use std::collections::HashMap;

use crate::patch::hunks::{FilePatch, Hunk, LineKind};
use crate::patch::near::{Candidate, Pair};
use crate::patch::parse::Format;

/// A candidate chosen for one hunk: `files[file].hunks[hunk]`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Choice<'a> {
    pub file: usize,
    pub hunk: usize,
    pub candidate: &'a Candidate,
}

type Body = Vec<(LineKind, String)>;

/// The hunk's lines with the old side replaced by the window's real text.
fn rebuild(hunk: &Hunk, candidate: &Candidate, file: &[&str]) -> Body {
    // File lines to keep as context, keyed by the old line they precede.
    let mut extras: HashMap<usize, Vec<&str>> = HashMap::new();
    let mut counterpart: HashMap<usize, Option<&str>> = HashMap::new();
    let mut pending: Vec<&str> = Vec::new();
    for pair in &candidate.pairs {
        match *pair {
            Pair::Extra { line } => pending.push(file[line]),
            Pair::Both { old, line } => {
                if !pending.is_empty() {
                    extras.insert(old, std::mem::take(&mut pending));
                }
                counterpart.insert(old, Some(file[line]));
            }
            Pair::Missing { old } => {
                if !pending.is_empty() {
                    extras.insert(old, std::mem::take(&mut pending));
                }
                counterpart.insert(old, None);
            }
        }
    }
    let mut body: Body = Vec::new();
    let mut old = 0usize;
    for line in &hunk.lines {
        if line.kind == LineKind::Add {
            body.push((LineKind::Add, line.text.clone()));
            continue;
        }
        for extra in extras.remove(&old).unwrap_or_default() {
            body.push((LineKind::Context, extra.to_string()));
        }
        if let Some(Some(text)) = counterpart.get(&old) {
            body.push((line.kind, (*text).to_string()));
        }
        old += 1;
    }
    for extra in pending {
        body.push((LineKind::Context, extra.to_string()));
    }
    body
}

/// The hunk as it would be applied at `candidate`: the lines of the corrected
/// patch for this hunk, marked ` `, `-` and `+`. It is produced by the same
/// `rebuild` that `reanchor` uses, so what is shown is what would be applied.
pub(crate) fn corrected_hunk(
    hunk: &Hunk,
    candidate: &Candidate,
    file: &[&str],
) -> Vec<(LineKind, String)> {
    rebuild(hunk, candidate, file)
}

fn render(body: &Body) -> Vec<String> {
    body.iter().map(|(kind, text)| format!("{}{text}", kind.prefix())).collect()
}

fn counts(body: &Body) -> (usize, usize) {
    let old = body.iter().filter(|(k, _)| *k != LineKind::Add).count();
    let new = body.iter().filter(|(k, _)| *k != LineKind::Remove).count();
    (old, new)
}

/// `patch` with the chosen hunks corrected. `files` is what `hunks::read` made of
/// `patch`; `originals` holds the current text of each file by path.
pub(crate) fn reanchor(
    patch: &str,
    format: Format,
    files: &[FilePatch],
    choices: &[Choice],
    originals: &HashMap<String, String>,
) -> String {
    let lines: Vec<&str> = patch.split('\n').collect();
    // Replacements: (first line, one past the last, new lines).
    let mut edits: Vec<(usize, usize, Vec<String>)> = Vec::new();

    for (file_index, file) in files.iter().enumerate() {
        let mine: Vec<&Choice> = choices.iter().filter(|c| c.file == file_index).collect();
        if mine.is_empty() {
            continue;
        }
        let text = originals.get(&file.path).map(String::as_str).unwrap_or("");
        let file_lines: Vec<&str> = text.lines().collect();

        // Final shape of every hunk of this file, in order.
        let mut delta: isize = 0;
        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let choice = mine.iter().find(|c| c.hunk == hunk_index);
            let body: Body = match choice {
                Some(c) => rebuild(hunk, c.candidate, &file_lines),
                None => hunk.lines.iter().map(|l| (l.kind, l.text.clone())).collect(),
            };
            let (old_count, new_count) = counts(&body);
            match format {
                Format::GitDiff => {
                    let old_start = match choice {
                        Some(c) => c
                            .candidate
                            .pairs
                            .iter()
                            .find_map(|p| match *p {
                                Pair::Both { line, .. } | Pair::Extra { line } => Some(line + 1),
                                Pair::Missing { .. } => None,
                            })
                            .unwrap_or(1),
                        None => hunk.old_start.unwrap_or(1),
                    };
                    let new_start = if old_count == 0 {
                        (old_start as isize + delta + 1).max(0) as usize
                    } else if new_count == 0 {
                        (old_start as isize + delta - 1).max(0) as usize
                    } else {
                        (old_start as isize + delta).max(0) as usize
                    };
                    let header = format!(
                        "@@ -{old_start},{old_count} +{new_start},{new_count} @@{}",
                        hunk.tail
                    );
                    let header_at = hunk.header.expect("a diff hunk has a header line");
                    if choice.is_some() {
                        let mut new_lines = vec![header];
                        new_lines.extend(render(&body));
                        edits.push((header_at, hunk.body.end, new_lines));
                    } else {
                        edits.push((header_at, header_at + 1, vec![header]));
                    }
                }
                Format::ApplyPatch => {
                    let Some(_) = choice else { continue };
                    // A `@@` locator the file does not contain would make the
                    // tool fail again; the corrected hunk is placed by its lines.
                    let keep_hint = hunk
                        .hint
                        .as_ref()
                        .is_some_and(|h| file_lines.iter().any(|l| l.trim() == h.trim()));
                    let mut new_lines = Vec::new();
                    let start = match hunk.header {
                        Some(at) => {
                            new_lines.push(if keep_hint {
                                lines[at].to_string()
                            } else {
                                "@@".to_string()
                            });
                            at
                        }
                        None => hunk.body.start,
                    };
                    new_lines.extend(render(&body));
                    edits.push((start, hunk.body.end, new_lines));
                }
            }
            delta += new_count as isize - old_count as isize;
        }
    }

    edits.sort_by_key(|(start, _, _)| *start);
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut at = 0usize;
    for (start, end, new_lines) in edits {
        out.extend(lines[at..start].iter().map(|l| l.to_string()));
        out.extend(new_lines);
        at = end;
    }
    out.extend(lines[at..].iter().map(|l| l.to_string()));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::hunks::read;
    use crate::patch::near::candidates;

    fn envelope(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch\n")
    }

    fn originals(path: &str, text: &str) -> HashMap<String, String> {
        HashMap::from([(path.to_string(), text.to_string())])
    }

    /// Re-anchors every failing hunk to its best candidate.
    fn fix(patch: &str, format: Format, path: &str, file: &str) -> String {
        let files = read(patch, format);
        let originals = originals(path, file);
        let lines: Vec<&str> = file.lines().collect();
        let mut found = Vec::new();
        for (fi, f) in files.iter().enumerate() {
            for (hi, h) in f.hunks.iter().enumerate() {
                if crate::patch::hunks::matches(h, &lines) {
                    continue;
                }
                let c = candidates(h, &lines).unwrap().remove(0);
                found.push((fi, hi, c));
            }
        }
        let choices: Vec<Choice> = found
            .iter()
            .map(|(file, hunk, candidate)| Choice { file: *file, hunk: *hunk, candidate })
            .collect();
        reanchor(patch, format, &files, &choices, &originals)
    }

    const FILE: &str = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    run(a, b, c);\n}\n";

    #[test]
    fn a_misquoted_context_line_is_replaced_by_the_real_one() {
        let patch = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 11;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n")
        );
    }

    #[test]
    fn a_misquoted_removed_line_becomes_the_real_line() {
        let patch = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let b = 22;\n+    let b = 5;\n     let c = 3;\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert!(fixed.contains("-    let b = 2;\n+    let b = 5;\n"), "{fixed}");
    }

    #[test]
    fn a_missing_removed_line_is_dropped_not_replaced() {
        // `let zz = 9;` is not in the file: nothing is removed in its place, the
        // added line stays, and no real line is turned into a removal.
        let patch = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let zz = 9;\n+    let b = 5;\n     let b = 2;\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n+    let b = 5;\n     let b = 2;\n")
        );

        // The same in a diff: the corrected diff applies with git and deletes nothing.
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n fn main() {\n     let a = 1;\n-    let zz = 9;\n+    let b = 5;\n     let b = 2;\n";
        let fixed = fix(diff, Format::GitDiff, "f", FILE);
        assert_eq!(
            git_applies(FILE, &fixed).as_deref(),
            Some("fn main() {\n    let a = 1;\n    let b = 5;\n    let b = 2;\n    let c = 3;\n    run(a, b, c);\n}\n")
        );
    }

    #[test]
    fn a_left_out_context_line_is_added_and_an_invented_one_dropped() {
        let patch = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let c = 3;\n+    let c = 4;\n     run(a, b, c);\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n     let b = 2;\n-    let c = 3;\n+    let c = 4;\n     run(a, b, c);\n")
        );

        let patch = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n     let zz = 9;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n")
        );
    }

    #[test]
    fn a_locator_the_file_lacks_is_dropped_and_one_it_has_is_kept() {
        let body = |hint: &str| {
            envelope(&format!("*** Update File: f\n@@ {hint}\n fn main() {{\n     let a = 11;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n"))
        };
        let fixed = fix(&body("fn nowhere()"), Format::ApplyPatch, "f", FILE);
        assert!(fixed.contains("\n@@\n fn main() {\n"), "{fixed}");
        let fixed = fix(&body("fn main() {"), Format::ApplyPatch, "f", FILE);
        assert!(fixed.contains("\n@@ fn main() {\n fn main() {\n"), "{fixed}");
    }

    #[test]
    fn a_hunk_without_a_header_line_keeps_none() {
        let patch = envelope("*** Update File: f\n fn main() {\n     let a = 11;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", FILE);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n")
        );
    }

    #[test]
    fn only_the_chosen_hunks_change_and_other_files_are_untouched() {
        let file = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let patch = envelope("*** Update File: f\n@@\n a\n-b\n+B\n c\n@@\n e\n-f9\n+F\n g\n*** Add File: n\n+new\n*** Delete File: z\n");
        let fixed = fix(&patch, Format::ApplyPatch, "f", file);
        assert_eq!(
            fixed,
            envelope("*** Update File: f\n@@\n a\n-b\n+B\n c\n@@\n e\n-f\n+F\n g\n*** Add File: n\n+new\n*** Delete File: z\n")
        );
    }

    #[test]
    fn choosing_an_exact_window_changes_nothing() {
        let patch = envelope("*** Update File: f\n@@ fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n");
        let files = read(&patch, Format::ApplyPatch);
        let candidate = Candidate {
            start: 1,
            len: 3,
            pairs: (0..3).map(|old| Pair::Both { old, line: 1 + old }).collect(),
            score: 1.0,
            loose: false,
            changed: 0,
        };
        let out = reanchor(
            &patch,
            Format::ApplyPatch,
            &files,
            &[Choice { file: 0, hunk: 0, candidate: &candidate }],
            &originals("f", FILE),
        );
        assert_eq!(out, patch);
        // No choices: the text is returned as it came.
        assert_eq!(reanchor(&patch, Format::ApplyPatch, &files, &[], &originals("f", FILE)), patch);
    }

    #[test]
    fn a_diff_hunk_gets_the_real_lines_and_a_renumbered_header() {
        let file = "one\ntwo\nthree\nfour\nfive\nsix\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -9,3 +9,3 @@ ctx\n two\n-thre\n+3\n four\n";
        let fixed = fix(diff, Format::GitDiff, "f", file);
        assert_eq!(
            fixed,
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,3 +2,3 @@ ctx\n two\n-three\n+3\n four\n"
        );
    }

    #[test]
    fn later_diff_hunks_follow_a_changed_line_count() {
        // Hunk 1 asks to remove a line that is not in the file; dropping that
        // removal changes the net line count. Hunk 2 matches and is renumbered.
        let file = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n a\n-zz\n b\n+B\n c\n@@ -8,3 +8,3 @@\n h\n-i\n+I\n j\n";
        let fixed = fix(diff, Format::GitDiff, "f", file);
        assert_eq!(
            fixed,
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,4 @@\n a\n b\n+B\n c\n@@ -8,3 +9,3 @@\n h\n-i\n+I\n j\n"
        );
    }

    #[test]
    fn a_delete_only_diff_hunk_is_renumbered_for_the_following_hunk() {
        let file = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,2 @@\n a\n-zz\n-b\n-c\n d\n@@ -8,3 +6,3 @@\n h\n-i\n+I\n j\n";
        let fixed = fix(diff, Format::GitDiff, "f", file);
        assert!(git_applies(file, &fixed).is_some(), "{fixed}");
    }

    fn git_applies(file: &str, diff: &str) -> Option<String> {
        use std::process::Command;
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cvb-reanchor-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), file).unwrap();
        let mut child = Command::new("git")
            .args(["apply", "--whitespace=nowarn", "-"])
            .current_dir(&dir)
            .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        use std::io::Write;
        child.stdin.take().unwrap().write_all(diff.as_bytes()).unwrap();
        let ok = child.wait().unwrap().success();
        let out = ok.then(|| std::fs::read_to_string(dir.join("f")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn a_corrected_diff_applies_with_git_and_the_original_does_not() {
        let file = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n a\n zz\n b\n+B\n c\n@@ -8,3 +9,3 @@\n h\n-i\n+I\n j\n";
        assert_eq!(git_applies(file, diff), None, "the pasted diff must fail");
        let fixed = fix(diff, Format::GitDiff, "f", file);
        assert_eq!(
            git_applies(file, &fixed).as_deref(),
            Some("a\nb\nB\nc\nd\ne\nf\ng\nh\nI\nj\n")
        );

        // A misquoted removed line deletes the real one.
        let file = "one\ntwo\nthree\nfour\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n one\n-tw0\n+TWO\n three\n four\n";
        assert_eq!(git_applies(file, diff), None);
        let fixed = fix(diff, Format::GitDiff, "f", file);
        assert_eq!(git_applies(file, &fixed).as_deref(), Some("one\nTWO\nthree\nfour\n"));
    }
}

//! Recognises a failed patch whose changes are already in the files.
//!
//! When an LLM patch is pasted twice (or the file was edited by hand to the same
//! result) `apply_patch` fails with "Failed to find expected lines": the context
//! it needs no longer sits next to the old text. The failure is correct, but it
//! reads like a broken patch. This module only *annotates* that failure; it never
//! changes the outcome and is never consulted when the patch applied.
//!
//! The parser deliberately ignores hunks, so this has its own small reader. A
//! hunk counts as already present when its new side (context plus `+` lines) is
//! found, contiguously, in the file and its old side (context plus `-` lines) is
//! not. Hunks without any `+` line are not judged: their new side is only context,
//! which proves nothing about a deletion.

use std::collections::HashMap;
use std::path::Path;

const BEGIN: &str = "*** Begin Patch";
const UPDATE: &str = "*** Update File: ";

/// One `@@` chunk of an update.
#[derive(Debug, Default, PartialEq, Eq)]
struct Hunk {
    old: Vec<String>,
    new: Vec<String>,
    added: Vec<String>,
}

/// The hunks of every update, keyed by the path in the `Update File` header.
fn read_hunks(text: &str) -> Vec<(String, Vec<Hunk>)> {
    let mut files: Vec<(String, Vec<Hunk>)> = Vec::new();
    let mut open = false;
    let mut started = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if !started {
            started = line == BEGIN;
            continue;
        }
        if let Some(path) = line.strip_prefix(UPDATE) {
            files.push((path.to_string(), vec![Hunk::default()]));
            open = true;
        } else if line.starts_with("*** ") && line != "*** End of File" {
            // Move to, Add File, Delete File, End Patch: not a hunk line.
            open = line.starts_with("*** Move to: ") && open;
        } else if open {
            let hunks = &mut files.last_mut().expect("open implies a file").1;
            let hunk = hunks.last_mut().expect("a file always has a hunk");
            if line.starts_with("@@") {
                if *hunk != Hunk::default() {
                    hunks.push(Hunk::default());
                }
            } else if line == "*** End of File" {
                // Position hint only.
            } else if let Some(rest) = line.strip_prefix('+') {
                hunk.new.push(rest.to_string());
                hunk.added.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.old.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.old.push(rest.to_string());
                hunk.new.push(rest.to_string());
            } else if line.is_empty() {
                hunk.old.push(String::new());
                hunk.new.push(String::new());
            }
        }
    }
    for (_, hunks) in &mut files {
        hunks.retain(|h| *h != Hunk::default());
    }
    files
}

/// Is `needle` a contiguous run of `file`, the way `apply_patch` compares:
/// exactly, then ignoring trailing white space, then ignoring all of it?
fn found(file: &[&str], needle: &[String]) -> bool {
    if needle.is_empty() {
        return false;
    }
    let tiers: [fn(&str) -> &str; 3] = [|s| s, str::trim_end, str::trim];
    tiers.iter().any(|norm| {
        file.windows(needle.len())
            .any(|window| window.iter().zip(needle).all(|(a, b)| norm(a) == norm(b)))
    })
}

fn already_present(hunk: &Hunk, file: &[&str]) -> bool {
    hunk.added.iter().any(|l| !l.trim().is_empty())
        && found(file, &hunk.new)
        && !found(file, &hunk.old)
}

/// The note to add to a failure report, if the patch looks already applied.
/// `originals` holds the current text of the files the patch reads.
pub(crate) fn note(
    patch: &str,
    originals: &HashMap<String, String>,
    root: &Path,
) -> Option<String> {
    let mut lines = Vec::new();
    let (mut total, mut present) = (0usize, 0usize);
    for (path, hunks) in read_hunks(patch) {
        let Some(text) = originals.get(&path) else { continue };
        let file: Vec<&str> = text.lines().collect();
        let here = hunks.iter().filter(|h| already_present(h, &file)).count();
        total += hunks.len();
        present += here;
        if here > 0 {
            let full = root.join(&path).display().to_string();
            lines.push(if here == hunks.len() {
                format!("  {full} ({here} of {} hunks)", hunks.len())
            } else {
                format!("  {full}: {here} of {} hunks", hunks.len())
            });
        }
    }
    if present == 0 {
        return None;
    }
    let head = if present == total {
        "Note: every hunk of this patch appears to be already present in the files \
         (it was probably applied before):"
    } else {
        "Note: some hunks of this patch appear to be already present in the files:"
    };
    Some(format!("{head}\n{}\n", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect()
    }

    fn patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch\n")
    }

    fn note_for(body: &str, pairs: &[(&str, &str)]) -> Option<String> {
        note(&patch(body), &files(pairs), Path::new("/r"))
    }

    /// The case that was reported: both added lines are already between the
    /// context lines, so the old three-line sequence is gone.
    const REDIWM: &str = "*** Update File: ops.zig\n@@\n         self.state = .idle;\n         self.clearManifestLookup();\n+        self.clearDirectoryProgress();\n+        self.directory_progress_valid = true;\n         self.manifest.clearRetainingCapacity();\n";

    #[test]
    fn the_reported_case_is_recognised() {
        let text = note_for(
            REDIWM,
            &[(
                "ops.zig",
                "fn f() {\n        self.state = .idle;\n        self.clearManifestLookup();\n        self.clearDirectoryProgress();\n        self.directory_progress_valid = true;\n        self.manifest.clearRetainingCapacity();\n}\n",
            )],
        )
        .expect("note");
        assert!(text.starts_with("Note: every hunk of this patch appears to be already present"), "{text}");
        assert!(text.contains("  /r/ops.zig (1 of 1 hunks)"), "{text}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_patch_that_still_applies_gets_no_note() {
        // Old side present (and the new side is not): the failure is something else.
        let original = "        self.state = .idle;\n        self.clearManifestLookup();\n        self.manifest.clearRetainingCapacity();\n";
        assert_eq!(note_for(REDIWM, &[("ops.zig", original)]), None);
    }

    #[test]
    fn a_file_holding_both_the_old_and_the_new_text_gets_no_note() {
        // One copy is patched, one is not: apply_patch can still apply this hunk,
        // so a failure elsewhere must not be explained as "already applied".
        let both = "k\nold\nm\n---\nk\nnew\nm\n";
        let body = "*** Update File: a.txt\n@@\n k\n-old\n+new\n m\n";
        assert_eq!(note_for(body, &[("a.txt", both)]), None);
    }

    #[test]
    fn a_file_that_has_neither_side_gets_no_note() {
        assert_eq!(note_for(REDIWM, &[("ops.zig", "something else entirely\n")]), None);
    }

    #[test]
    fn partial_presence_is_reported_per_file_without_claiming_all() {
        let body = "*** Update File: a.txt\n@@\n ctx1\n+added one\n ctx2\n@@\n ctx3\n-gone\n+added two\n ctx4\n";
        let a = "ctx1\nadded one\nctx2\nctx3\ngone\nctx4\n";
        let text = note_for(body, &[("a.txt", a)]).expect("note");
        assert!(text.starts_with("Note: some hunks of this patch appear to be already present"), "{text}");
        assert!(text.contains("  /r/a.txt: 1 of 2 hunks"), "{text}");
    }

    #[test]
    fn several_files_are_listed_and_unrelated_ones_left_out() {
        let body = "*** Update File: a.txt\n@@\n k\n+new a\n m\n*** Update File: b.txt\n@@\n k\n+new b\n m\n";
        let text = note_for(body, &[("a.txt", "k\nnew a\nm\n"), ("b.txt", "k\nm\n")]).expect("note");
        assert!(text.starts_with("Note: some hunks"), "{text}");
        assert!(text.contains("/r/a.txt (1 of 1 hunks)"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
    }

    #[test]
    fn a_hunk_without_added_lines_is_never_judged() {
        // After a deletion the context lines are adjacent, but so they may have
        // been before: nothing is proven, so nothing is said.
        let body = "*** Update File: a.txt\n@@\n k1\n-gone\n k2\n";
        assert_eq!(note_for(body, &[("a.txt", "k1\nk2\n")]), None);
    }

    #[test]
    fn blank_added_lines_alone_prove_nothing() {
        let body = "*** Update File: a.txt\n@@\n k\n+\n";
        assert_eq!(note_for(body, &[("a.txt", "k\n\n")]), None);
    }

    #[test]
    fn matching_tolerates_trailing_space_and_indentation_like_apply_patch() {
        let body = "*** Update File: a.txt\n@@\n k\n+  new\n m\n";
        assert!(note_for(body, &[("a.txt", "k  \n  new\nm\n")]).is_some());
        assert!(note_for(body, &[("a.txt", "k\n\tnew\nm\n")]).is_some());
    }

    #[test]
    fn crlf_files_and_crlf_patches_match() {
        let body = "*** Update File: a.txt\r\n@@\r\n k\r\n+new\r\n m\r\n";
        assert!(note(&patch(body), &files(&[("a.txt", "k\r\nnew\r\nm\r\n")]), Path::new("/r")).is_some());
    }

    #[test]
    fn a_moved_file_is_judged_by_its_source() {
        let body = "*** Update File: a.txt\n*** Move to: b.txt\n@@\n k\n+new\n m\n";
        assert!(note_for(body, &[("a.txt", "k\nnew\nm\n")]).is_some());
    }

    #[test]
    fn add_and_delete_sections_are_ignored() {
        let body = "*** Add File: n.txt\n+k\n+new\n*** Delete File: d.txt\n*** Update File: a.txt\n@@\n k\n+new\n m\n";
        let text = note_for(body, &[("a.txt", "k\nnew\nm\n"), ("n.txt", "k\nnew\n"), ("d.txt", "k\nnew\n")])
            .expect("note");
        assert!(!text.contains("n.txt") && !text.contains("d.txt"), "{text}");
    }

    #[test]
    fn text_before_begin_and_after_end_is_not_read() {
        let text = "+ignored\n*** Begin Patch\n*** Update File: a.txt\n@@\n k\n+new\n m\n*** End Patch\n+tail\n";
        let got = note(text, &files(&[("a.txt", "k\nnew\nm\n")]), Path::new("/r")).expect("note");
        assert!(got.contains("(1 of 1 hunks)"), "{got}");
    }

    #[test]
    fn end_of_file_marker_does_not_split_or_break_a_hunk() {
        let body = "*** Update File: a.txt\n@@\n k\n+new\n m\n*** End of File\n";
        assert!(note_for(body, &[("a.txt", "k\nnew\nm\n")]).is_some());
    }
}

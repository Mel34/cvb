//! The optional review gate (`CVB_PATCH_REVIEW=on`).
//!
//! With the gate on, a recognized patch is shown instead of applied. The proxy
//! then holds a [`Review`]: Ctrl+B twice applies the patch, any other key
//! cancels it and goes on to the shell as usual. The state machine here is pure
//! (bytes in, decision out); `pty.rs` does the printing and the applying.

use std::ffi::OsStr;

use crate::patch::parse::{Change, Format, ParsedPatch};

/// The key that confirms a review: Ctrl+B (STX).
pub(crate) const CONFIRM_KEY: u8 = 0x02;

/// Longest patch shown in full. A longer one is shown as its head and tail, the
/// tail being where a clipboard that cut the paste short shows.
const MAX_SHOWN_LINES: usize = 300;
const SHOWN_HEAD_LINES: usize = 150;
const SHOWN_TAIL_LINES: usize = 150;
/// Files named in the summary line before it says "and N more".
const MAX_SUMMARY_FILES: usize = 12;

/// `CVB_PATCH_REVIEW=on` (or `1`, `true`, `yes`) turns the gate on. It is off
/// unless asked for.
pub(crate) fn enabled_by(value: Option<&OsStr>) -> bool {
    match value.and_then(OsStr::to_str) {
        Some(text) => matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "on" | "1" | "true" | "yes"
        ),
        None => false,
    }
}

pub(crate) fn enabled() -> bool {
    enabled_by(std::env::var_os("CVB_PATCH_REVIEW").as_deref())
}

/// A patch waiting for confirmation.
#[derive(Debug)]
pub(crate) struct Review {
    text: String,
    armed: bool,
}

/// What a keystroke did to a review.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Still waiting. `just_armed` is true when this input was the first Ctrl+B.
    Waiting { just_armed: bool },
    /// Confirmed. Apply the patch, then treat `rest` as ordinary input.
    Apply { text: String, rest: Vec<u8> },
    /// Cancelled by the key that starts `rest`; `rest` is ordinary input.
    Cancel { rest: Vec<u8> },
}

impl Review {
    pub(crate) fn new(text: String) -> Self {
        Self { text, armed: false }
    }

    /// Feeds input read from the terminal. Bytes are taken in order: the first
    /// Ctrl+B arms, the second applies, anything else cancels.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Step {
        let mut just_armed = false;
        for (at, &byte) in bytes.iter().enumerate() {
            if byte != CONFIRM_KEY {
                return Step::Cancel { rest: bytes[at..].to_vec() };
            }
            if self.armed {
                let text = std::mem::take(&mut self.text);
                return Step::Apply { text, rest: bytes[at + 1..].to_vec() };
            }
            self.armed = true;
            just_armed = true;
        }
        Step::Waiting { just_armed }
    }
}

/// What the screen shows while the gate waits: the patch (its head and tail when
/// it is long), what it will do, and how to go on. The pasted text is used as
/// the user pasted it; `pty.rs` makes control characters visible.
pub(crate) fn display(text: &str, parsed: &ParsedPatch) -> String {
    let mut out = String::new();
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > MAX_SHOWN_LINES {
        for line in &lines[..SHOWN_HEAD_LINES] {
            out.push_str(line);
            out.push('\n');
        }
        let hidden = lines.len() - SHOWN_HEAD_LINES - SHOWN_TAIL_LINES;
        out.push_str(&format!("... {hidden} lines not shown ...\n"));
        for line in &lines[lines.len() - SHOWN_TAIL_LINES..] {
            out.push_str(line);
            out.push('\n');
        }
    } else {
        for line in &lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str(&summary(parsed, lines.len()));
    out.push('\n');
    for note in &parsed.notes {
        out.push_str(note);
        out.push('\n');
    }
    out.push_str("Not applied yet. Press Ctrl+B twice to apply; any other key cancels.\n");
    out
}

/// One line: format, size and the files with what happens to each.
fn summary(parsed: &ParsedPatch, line_count: usize) -> String {
    let format = match parsed.format {
        Format::ApplyPatch => "apply_patch",
        Format::GitDiff => "git diff",
    };
    let count = parsed.ops.len();
    let mut files: Vec<String> = parsed
        .ops
        .iter()
        .take(MAX_SUMMARY_FILES)
        .map(|op| match &op.change {
            Change::Add => format!("add {}", op.path),
            Change::Update => format!("update {}", op.path),
            Change::Delete => format!("delete {}", op.path),
            Change::Move { to } => format!("move {} -> {to}", op.path),
        })
        .collect();
    if count > MAX_SUMMARY_FILES {
        files.push(format!("and {} more", count - MAX_SUMMARY_FILES));
    }
    let plural = if count == 1 { "" } else { "s" };
    format!(
        "CVB patch REVIEW ({format}, {line_count} lines, {count} file{plural}): {}",
        files.join(", ")
    )
}

/// Printed when the first Ctrl+B arrives.
pub(crate) const ARMED_NOTE: &str = "CVB patch: press Ctrl+B once more to apply; any other key cancels.";

/// Printed when a review is dropped.
pub(crate) const CANCELLED_NOTE: &str = "CVB patch: cancelled, nothing was applied.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::parse::parse_any;

    fn review() -> Review {
        Review::new("PATCH".to_string())
    }

    #[test]
    fn the_gate_is_off_unless_switched_on() {
        assert!(!enabled_by(None));
        assert!(!enabled_by(Some(OsStr::new(""))));
        assert!(!enabled_by(Some(OsStr::new("off"))));
        assert!(!enabled_by(Some(OsStr::new("maybe"))));
        for on in ["on", "ON", " 1 ", "true", "Yes"] {
            assert!(enabled_by(Some(OsStr::new(on))), "{on:?}");
        }
    }

    #[test]
    fn two_presses_apply_the_patch() {
        let mut r = review();
        assert_eq!(r.feed(&[CONFIRM_KEY]), Step::Waiting { just_armed: true });
        assert_eq!(
            r.feed(&[CONFIRM_KEY]),
            Step::Apply { text: "PATCH".into(), rest: vec![] }
        );
    }

    #[test]
    fn both_presses_in_one_read_apply_and_keep_what_follows() {
        let mut r = review();
        assert_eq!(
            r.feed(b"\x02\x02ls\n"),
            Step::Apply { text: "PATCH".into(), rest: b"ls\n".to_vec() }
        );
    }

    #[test]
    fn another_key_cancels_and_is_handed_back() {
        let mut r = review();
        assert_eq!(r.feed(b"x"), Step::Cancel { rest: b"x".to_vec() });
        let mut r = review();
        assert_eq!(r.feed(b"\r"), Step::Cancel { rest: b"\r".to_vec() });
        // An arrow key starts with Escape: it cancels and arrives whole.
        let mut r = review();
        assert_eq!(r.feed(b"\x1b[A"), Step::Cancel { rest: b"\x1b[A".to_vec() });
    }

    #[test]
    fn a_key_between_the_presses_cancels() {
        let mut r = review();
        assert_eq!(r.feed(&[CONFIRM_KEY]), Step::Waiting { just_armed: true });
        assert_eq!(r.feed(b"x"), Step::Cancel { rest: b"x".to_vec() });
        // In one read too: the second press never counts.
        let mut r = review();
        assert_eq!(r.feed(b"\x02x\x02"), Step::Cancel { rest: b"x\x02".to_vec() });
    }

    #[test]
    fn a_paste_cancels_and_arrives_whole_for_the_recognizer() {
        let mut r = review();
        let paste = b"\x1b[200~*** Begin Patch\n*** End Patch\x1b[201~";
        assert_eq!(r.feed(paste), Step::Cancel { rest: paste.to_vec() });
    }

    #[test]
    fn an_empty_read_changes_nothing() {
        let mut r = review();
        assert_eq!(r.feed(b""), Step::Waiting { just_armed: false });
        assert_eq!(r.feed(&[CONFIRM_KEY]), Step::Waiting { just_armed: true });
        assert_eq!(r.feed(b""), Step::Waiting { just_armed: false });
        assert!(matches!(r.feed(&[CONFIRM_KEY]), Step::Apply { .. }));
    }

    #[test]
    fn a_review_that_was_armed_keeps_its_progress_across_reads() {
        let mut r = review();
        r.feed(&[CONFIRM_KEY]);
        // The second press, in a later read, still applies.
        assert!(matches!(r.feed(b"\x02abc"), Step::Apply { rest, .. } if rest == b"abc"));
    }

    const SMALL: &str = "*** Begin Patch\n*** Update File: a.txt\n@@\n-x\n+y\n*** End Patch";

    #[test]
    fn the_display_shows_the_patch_a_summary_and_how_to_go_on() {
        let parsed = parse_any(SMALL).unwrap();
        let text = display(SMALL, &parsed);
        assert!(text.starts_with("*** Begin Patch\n*** Update File: a.txt\n"), "{text}");
        assert!(
            text.contains(
                "CVB patch REVIEW (apply_patch, 6 lines, 1 file): update a.txt\n"
            ),
            "{text}"
        );
        assert!(
            text.ends_with("Not applied yet. Press Ctrl+B twice to apply; any other key cancels.\n"),
            "{text}"
        );
    }

    #[test]
    fn the_summary_names_each_kind_of_change() {
        let patch = "*** Begin Patch\n*** Update File: a\n@@\n-x\n+y\n*** Add File: b\n+n\n\
                     *** Delete File: c\n*** Update File: d\n*** Move to: e\n@@\n-p\n+q\n*** End Patch";
        let text = display(patch, &parse_any(patch).unwrap());
        assert!(
            text.contains("4 files): update a, add b, delete c, move d -> e\n"),
            "{text}"
        );
    }

    #[test]
    fn the_summary_says_git_diff_for_a_diff() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let text = display(diff, &parse_any(diff).unwrap());
        assert!(text.contains("CVB patch REVIEW (git diff, 6 lines, 1 file): update x\n"), "{text}");
    }

    #[test]
    fn corrections_made_by_the_parser_are_listed() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,5 +1,5 @@\n-a\n+b\n";
        let parsed = parse_any(diff).unwrap();
        assert!(!parsed.notes.is_empty());
        let text = display(diff, &parsed);
        assert!(text.contains(&format!("{}\n", parsed.notes[0])), "{text}");
    }

    #[test]
    fn a_long_patch_is_shown_as_head_and_tail() {
        let mut patch = String::from("*** Begin Patch\n*** Add File: big\n");
        for n in 0..400 {
            patch.push_str(&format!("+line {n}\n"));
        }
        patch.push_str("*** End Patch");
        let parsed = parse_any(&patch).unwrap();
        let text = display(&patch, &parsed);
        // 403 patch lines: 150 shown at each end, 103 hidden.
        assert!(text.contains("... 103 lines not shown ...\n"), "{text}");
        assert!(text.starts_with("*** Begin Patch\n*** Add File: big\n+line 0\n"));
        assert!(text.contains("+line 399\n*** End Patch\n"), "the tail is shown: {text}");
        assert!(!text.contains("+line 200\n"), "the middle is hidden");
        assert!(text.contains("(apply_patch, 403 lines, 1 file)"), "{text}");
        // Exactly at the limit nothing is hidden.
        let mut exact = String::from("*** Begin Patch\n*** Add File: f\n");
        for n in 0..(MAX_SHOWN_LINES - 3) {
            exact.push_str(&format!("+l{n}\n"));
        }
        exact.push_str("*** End Patch");
        assert_eq!(exact.lines().count(), MAX_SHOWN_LINES);
        let shown = display(&exact, &parse_any(&exact).unwrap());
        assert!(!shown.contains("not shown"), "{shown}");
    }

    #[test]
    fn a_long_file_list_is_cut_with_a_count() {
        let mut patch = String::from("*** Begin Patch\n");
        for n in 0..15 {
            patch.push_str(&format!("*** Delete File: f{n}\n"));
        }
        patch.push_str("*** End Patch");
        let text = display(&patch, &parse_any(&patch).unwrap());
        assert!(text.contains("15 files): delete f0, "), "{text}");
        assert!(text.contains("delete f11, and 3 more\n"), "{text}");
        assert!(!text.contains("delete f12,"), "{text}");
    }
}

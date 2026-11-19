//! The failed-hunk picker.
//!
//! When a pasted patch fails because some hunks do not match the files, the
//! picker offers, for each such hunk, the nearest places in the file where it
//! could belong. The user chooses one place per hunk; the patch is then
//! corrected (see `reanchor`), previewed, and applied only after Ctrl+B twice.
//!
//! The rules (decided 2026-11-18): all or nothing, so skipping a hunk cancels
//! the whole patch; there is no overall inactivity timeout. A partial Escape
//! sequence is held briefly so split arrow-key sequences can be recognized;
//! an unresolved Escape cancels the picker. Any other unknown input cancels
//! and is handed on to the shell. The state machine is pure (bytes in,
//! decision out); `pty.rs` does the printing, previewing and applying.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::patch::hunks::{FilePatch, Hunk, LineKind, matches, read};
use crate::patch::near::{Candidate, Pair, candidates};
use crate::patch::parse::Format;
use crate::patch::reanchor::{Choice, reanchor};
use crate::patch::review::{CONFIRM_KEY, clip};

/// Lines of the patch's own text shown for a hunk, and of each candidate.
const MAX_EXPECTED_LINES: usize = 14;
const MAX_CANDIDATE_LINES: usize = 24;
/// Longest line shown whole.
const MAX_LINE_CHARS: usize = 110;
const ESCAPE_SEQUENCE_WAIT: Duration = Duration::from_millis(30);

/// One hunk that does not match, with the places it could belong.
#[derive(Debug)]
struct Item {
    file: usize,
    hunk: usize,
    candidates: Vec<Candidate>,
}

/// A picker in progress.
#[derive(Debug)]
pub(crate) struct Picker {
    text: String,
    format: Format,
    files: Vec<FilePatch>,
    originals: HashMap<String, String>,
    items: Vec<Item>,
    /// Candidate chosen for `items[i]`, for each `i` already decided.
    picks: Vec<usize>,
    /// Candidate currently being inspected for the next undecided hunk.
    selected: usize,
    /// An Escape sequence split across terminal reads.
    pending_escape: Vec<u8>,
    escape_deadline: Option<Instant>,
    armed: bool,
}

/// What input did to a picker.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing to show; `just_armed` is true when this input was the first
    /// Ctrl+B of the confirmation.
    Waiting { just_armed: bool },
    /// Show this text (the next hunk to decide).
    Show(String),
    /// Every hunk has a choice. Show `view`, run a preview of `patch`, show
    /// its outcome, and wait for Ctrl+B twice.
    Confirm { view: String, patch: String },
    /// Confirmed. Apply `text`, then treat `rest` as ordinary input.
    Apply { text: String, rest: Vec<u8> },
    /// Cancelled. `rest` is input to hand on (empty when the user cancelled
    /// with a key the picker owns, `s` or `q`).
    Cancel { rest: Vec<u8> },
}

impl Picker {
    /// A picker for `text` (a complete patch) over the current `originals`, or
    /// the reason there is nothing to offer.
    pub(crate) fn new(
        text: &str,
        format: Format,
        originals: HashMap<String, String>,
    ) -> Result<Self, String> {
        let files = read(text, format);
        let mut items = Vec::new();
        for (file_index, file) in files.iter().enumerate() {
            let Some(original) = originals.get(&file.path) else { continue };
            let lines: Vec<&str> = original.lines().collect();
            for (hunk_index, hunk) in file.hunks.iter().enumerate() {
                if matches(hunk, &lines) {
                    continue;
                }
                let found = candidates(hunk, &lines).map_err(|refusal| {
                    format!("no suggestions for {}: {}", file.path, refusal.describe())
                })?;
                if found.is_empty() {
                    return Err(format!(
                        "no suggestions: hunk {} of {} is not similar to any text in the file",
                        hunk_index + 1,
                        file.path
                    ));
                }
                items.push(Item { file: file_index, hunk: hunk_index, candidates: found });
            }
        }
        if items.is_empty() {
            return Err("no suggestions: no hunk of this patch can be placed".to_string());
        }
        Ok(Self {
            text: text.to_string(),
            format,
            files,
            originals,
            items,
            picks: Vec::new(),
            selected: 0,
            pending_escape: Vec::new(),
            escape_deadline: None,
            armed: false,
        })
    }

    /// How many hunks need a decision.
    pub(crate) fn hunk_count(&self) -> usize {
        self.items.len()
    }

    /// The first screen.
    pub(crate) fn first_view(&self) -> String {
        format!(
            "CVB patch: {} hunk{} did not match the files. Nothing was modified.\n{}",
            self.items.len(),
            if self.items.len() == 1 { "" } else { "s" },
            self.view_item(0)
        )
    }

    /// All alternatives, with untruncated source lines, for external review.
    /// This is copied once when the picker opens; navigation does not replace it.
    pub(crate) fn review_bundle(&self) -> String {
        let mut out = String::from(
            "CVB FAILED-HUNK REVIEW\n\
             The patch failed to match the current files. No changes have been made.\n\
             Candidates are suggestions, not automatic decisions.\n",
        );

        for (index, item) in self.items.iter().enumerate() {
            let file = &self.files[item.file];
            let hunk = &file.hunks[item.hunk];
            let lines: Vec<&str> = self.originals[&file.path].lines().collect();

            out.push_str(&format!(
                "\n========== HUNK {} OF {} ==========\n\
                 File: {}\n\
                 Patch hunk: {} of {}\n",
                index + 1,
                self.items.len(),
                file.path,
                item.hunk + 1,
                file.hunks.len(),
            ));
            if let Some(start) = hunk.old_start {
                out.push_str(&format!("Original line hint: {start}\n"));
            }

            out.push_str("Patch as supplied:\n");
            for line in &hunk.lines {
                let mark = match line.kind {
                    LineKind::Context => ' ',
                    LineKind::Remove => '-',
                    LineKind::Add => '+',
                };
                out.push_str(&format!("{mark}{}\n", line.text));
            }

            out.push_str("\nCandidate comparisons:\n");
            for (number, candidate) in item.candidates.iter().enumerate() {
                out.push_str(&candidate_review_view(
                    number + 1,
                    candidate,
                    hunk,
                    &lines,
                ));
            }
        }

        out.push_str(
            "\nPlease compare the alternatives semantically. \
             A higher textual score does not prove that a location is correct.\n",
        );
        out
    }

    fn confirming(&self) -> bool {
        self.picks.len() == self.items.len()
    }

    /// Feeds input read from the terminal, byte by byte.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Step {
        self.feed_at(bytes, Instant::now())
    }

    fn feed_at(&mut self, bytes: &[u8], now: Instant) -> Step {
        let mut input = std::mem::take(&mut self.pending_escape);
        input.extend_from_slice(bytes);
        self.escape_deadline = None;

        let mut changed = false;
        let mut just_armed = false;
        let started_confirming = self.confirming();
        let mut at = 0;

        while at < input.len() {
            if self.confirming() {
                let byte = input[at];
                if byte != CONFIRM_KEY {
                    return Step::Cancel { rest: input[at..].to_vec() };
                }
                if self.armed {
                    return Step::Apply {
                        text: self.corrected(),
                        rest: input[at + 1..].to_vec(),
                    };
                }
                self.armed = true;
                just_armed = true;
                at += 1;
                continue;
            }

            let current = self.picks.len();
            let byte = input[at];

            // CSI and SS3 cursor sequences. Retain incomplete sequences so
            // an arrow key split across PTY reads is still recognized.
            if byte == 0x1b {
                let rest = &input[at..];
                if rest.len() == 1 || (rest.len() == 2 && matches!(rest[1], b'[' | b'O')) {
                    self.pending_escape.extend_from_slice(rest);
                    self.escape_deadline = Some(now + ESCAPE_SEQUENCE_WAIT);
                    break;
                }

                let direction = match (rest.get(1), rest.get(2)) {
                    (Some(b'['), Some(b'D')) | (Some(b'O'), Some(b'D')) => Some(false),
                    (Some(b'['), Some(b'C')) | (Some(b'O'), Some(b'C')) => Some(true),
                    (Some(b'['), Some(b'A')) => Some(false),
                    (Some(b'['), Some(b'B')) => Some(true),
                    _ => None,
                };

                if let Some(forward) = direction {
                    let previous = self.selected;
                    let last = self.items[current].candidates.len() - 1;
                    self.selected = if forward {
                        (self.selected + 1).min(last)
                    } else {
                        self.selected.saturating_sub(1)
                    };
                    changed |= self.selected != previous;
                    at += 3;
                    continue;
                }

                // Unknown sequences are returned to the shell unchanged.
                return Step::Cancel { rest: rest.to_vec() };
            }

            match byte {
                b's' | b'q' => return Step::Cancel { rest: Vec::new() },
                b'\r' | b'\n' => {
                    self.picks.push(self.selected);
                    self.selected = 0;
                    at += 1;
                    changed = true;
                }
                b'1'..=b'9' => {
                    let choice = usize::from(byte - b'1');
                    if choice >= self.items[current].candidates.len() {
                        at += 1;
                        continue;
                    }
                    self.picks.push(choice);
                    self.selected = 0;
                    at += 1;
                    changed = true;
                }
                _ => return Step::Cancel { rest: input[at..].to_vec() },
            }
        }

        if self.confirming() && !started_confirming {
            self.confirm_step()
        } else if changed {
            Step::Show(self.view_item(self.picks.len()))
        } else {
            Step::Waiting { just_armed }
        }
    }

    /// Milliseconds until a partial Escape sequence must be resolved.
    pub(crate) fn poll_timeout_ms(&self, now: Instant) -> Option<u16> {
        let deadline = self.escape_deadline?;
        let left = deadline.saturating_duration_since(now);
        let millis = left.as_millis()
            + u128::from(left.subsec_nanos() % 1_000_000 != 0);
        Some(millis.min(u128::from(u16::MAX)) as u16)
    }

    /// Resolves a partial Escape sequence after the terminal-read timeout.
    pub(crate) fn flush_if_due(&mut self, now: Instant) -> Option<Step> {
        let deadline = self.escape_deadline?;
        if now < deadline {
            return None;
        }

        self.escape_deadline = None;
        let pending = std::mem::take(&mut self.pending_escape);
        if pending == b"\x1b" {
            Some(Step::Cancel { rest: Vec::new() })
        } else {
            Some(Step::Cancel { rest: pending })
        }
    }

    fn confirm_step(&self) -> Step {
        Step::Confirm { view: self.confirm_view(), patch: self.corrected() }
    }

    /// The patch with every chosen hunk moved to its chosen place.
    pub(crate) fn corrected(&self) -> String {
        let choices: Vec<Choice> = self
            .picks
            .iter()
            .enumerate()
            .map(|(i, &pick)| Choice {
                file: self.items[i].file,
                hunk: self.items[i].hunk,
                candidate: &self.items[i].candidates[pick],
            })
            .collect();
        reanchor(&self.text, self.format, &self.files, &choices, &self.originals)
    }

    fn any_loose(&self) -> bool {
        self.picks
            .iter()
            .enumerate()
            .any(|(i, &pick)| self.items[i].candidates[pick].loose)
    }

    fn confirm_view(&self) -> String {
        let mut out = String::from("CVB patch: corrected patch (hunks moved to the chosen places):\n");
        out.push_str(&clip(&self.corrected()));
        if self.any_loose() {
            out.push_str(
                "WARNING: for at least one hunk the line to be removed differs from what the patch named;\n\
                 the real line in the file is the one that will be removed. Check the lines above.\n",
            );
        }
        out
    }

    /// The decision screen for `items[index]`.
    fn view_item(&self, index: usize) -> String {
        let item = &self.items[index];
        let file = &self.files[item.file];
        let hunk = &file.hunks[item.hunk];
        let lines: Vec<&str> = self.originals[&file.path].lines().collect();
        let mut out = format!(
            "Hunk {} of {} to decide: {}, hunk {} of {}",
            index + 1,
            self.items.len(),
            file.path,
            item.hunk + 1,
            file.hunks.len()
        );
        if let Some(start) = hunk.old_start {
            out.push_str(&format!(" (the patch says line {start})"));
        }
        out.push_str(".\nThe patch expects:\n");
        out.push_str(&expected_view(hunk));
        out.push_str(&format!(
            "\nCandidate {} of {}:\n",
            self.selected + 1,
            item.candidates.len()
        ));
        out.push_str(&candidate_view(
            self.selected + 1,
            &item.candidates[self.selected],
            hunk,
            &lines,
        ));
        out.push_str(&format!(
            "Left/Right browse candidates; Enter selects; 1-9 selects directly; \
             s skips (cancels the whole patch); q or Esc cancels.\n"
        ));
        out
    }
}

/// The hunk's old side, one line each, marked as in a patch.
fn expected_view(hunk: &Hunk) -> String {
    let old = hunk.old_side();
    let mut out = String::new();
    for (kind, text) in old.iter().take(MAX_EXPECTED_LINES) {
        let mark = if *kind == LineKind::Remove { '-' } else { ' ' };
        out.push_str(&format!("  {mark} {}\n", cut(text)));
    }
    if old.len() > MAX_EXPECTED_LINES {
        out.push_str(&format!("    ... {} more lines\n", old.len() - MAX_EXPECTED_LINES));
    }
    out
}

/// One candidate: where it is, how it differs, and its lines side by side.
fn candidate_view(number: usize, candidate: &Candidate, hunk: &Hunk, file: &[&str]) -> String {
    let old = hunk.old_side();
    let first = candidate.start + 1;
    let last = candidate.start + candidate.len;
    let how = if candidate.changed == 0 {
        "identical apart from white space".to_string()
    } else {
        let plural = if candidate.changed == 1 { "" } else { "s" };
        format!("{} line{plural} differ", candidate.changed)
    };
    let loose = if candidate.loose { "; a line to remove differs" } else { "" };
    let mut out = format!("[{number}] lines {first}-{last}: {how}{loose}\n");
    for (shown, pair) in candidate.pairs.iter().enumerate() {
        if shown == MAX_CANDIDATE_LINES {
            out.push_str("      ...\n");
            break;
        }
        match *pair {
            Pair::Both { old: o, line } => {
                let expected = old[o].1;
                if expected.trim() == file[line].trim() {
                    out.push_str(&format!("      {}\n", cut(file[line])));
                } else {
                    out.push_str(&format!("    ! expected: {}\n", cut(expected)));
                    out.push_str(&format!("    !   actual: {}\n", cut(file[line])));
                }
            }
            Pair::Extra { line } => {
                out.push_str(&format!("    + only in file: {}\n", cut(file[line])));
            }
            Pair::Missing { old: o } => {
                out.push_str(&format!("    - not in file: {}\n", cut(old[o].1)));
            }
        }
    }
    out
}

/// Complete candidate data for the clipboard review bundle. Unlike the
/// terminal view, this deliberately preserves long lines and every pair.
fn candidate_review_view(
    number: usize,
    candidate: &Candidate,
    hunk: &Hunk,
    file: &[&str],
) -> String {
    let old = hunk.old_side();
    let first = candidate.start + 1;
    let last = candidate.start + candidate.len;
    let confidence = if candidate.loose { "LOOSE" } else { "STRICT" };
    let mut out = format!(
        "\n[{number}] lines {first}-{last}; {confidence}; score {:.3}; {} changed line(s)\n",
        candidate.score,
        candidate.changed,
    );

    for pair in &candidate.pairs {
        match *pair {
            Pair::Both { old: o, line } => {
                let expected = old[o].1;
                let actual = file[line];
                if expected.trim() == actual.trim() {
                    out.push_str(&format!("  = file L{}: {actual}\n", line + 1));
                } else {
                    out.push_str(&format!("  ! expected: {expected}\n"));
                    out.push_str(&format!("  ! actual L{}: {actual}\n", line + 1));
                }
            }
            Pair::Extra { line } => {
                out.push_str(&format!("  + file-only L{}: {}\n", line + 1, file[line]));
            }
            Pair::Missing { old: o } => {
                out.push_str(&format!("  - patch-only: {}\n", old[o].1));
            }
        }
    }
    out
}

fn cut(text: &str) -> String {
    if text.chars().count() <= MAX_LINE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_LINE_CHARS).collect();
    format!("{head}...")
}

/// Printed when the first Ctrl+B of the confirmation arrives.
pub(crate) const ARMED_NOTE: &str =
    "CVB patch: press Ctrl+B once more to apply the corrected patch; any other key cancels.";

/// Printed when a picker is dropped.
pub(crate) const CANCELLED_NOTE: &str = "CVB patch: cancelled, nothing was applied.";

/// Printed under the corrected patch's preview, before the confirmation.
pub(crate) const CONFIRM_NOTE: &str =
    "Not applied yet. Press Ctrl+B twice to apply the corrected patch; any other key cancels.";

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "fn main() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    run(a, b, c);\n}\n";

    fn originals() -> HashMap<String, String> {
        HashMap::from([("f".to_string(), FILE.to_string())])
    }

    fn envelope(body: &str) -> String {
        format!("*** Begin Patch\n{body}*** End Patch\n")
    }

    /// Misquotes a context line: one candidate, strict.
    fn one_hunk() -> String {
        envelope("*** Update File: f\n@@\n fn main() {\n     let a = 11;\n-    let b = 2;\n+    let b = 5;\n     let c = 3;\n")
    }

    fn picker(text: &str) -> Picker {
        Picker::new(text, Format::ApplyPatch, originals()).expect("picker")
    }

    #[test]
    fn nothing_is_offered_when_every_hunk_matches() {
        let text = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 5;\n");
        let error = Picker::new(&text, Format::ApplyPatch, originals()).unwrap_err();
        assert!(error.starts_with("no suggestions"), "{error}");
    }

    #[test]
    fn nothing_is_offered_when_a_hunk_resembles_nothing() {
        let text = envelope("*** Update File: f\n@@\n qqqqqqq\n-wwwwwww\n+eeeeeee\n");
        let error = Picker::new(&text, Format::ApplyPatch, originals()).unwrap_err();
        assert!(error.contains("not similar"), "{error}");
    }

    #[test]
    fn a_hunk_too_short_to_place_is_refused_with_the_reason() {
        let text = envelope("*** Update File: f\n@@\n-zz\n+yy\n");
        let error = Picker::new(&text, Format::ApplyPatch, originals()).unwrap_err();
        assert!(error.contains("too few lines"), "{error}");
    }

    #[test]
    fn the_first_view_shows_the_expectation_and_the_difference() {
        let p = picker(&one_hunk());
        assert_eq!(p.hunk_count(), 1);
        let view = p.first_view();
        assert!(view.contains("1 hunk did not match"), "{view}");
        assert!(view.contains("The patch expects:"), "{view}");
        assert!(view.contains("expected:      let a = 11;") || view.contains("expected: "), "{view}");
        assert!(view.contains("let a = 1;"), "{view}");
        assert!(view.contains("[1] lines 1-4"), "{view}");
        assert!(view.contains("Left/Right browse candidates; Enter selects"), "{view}");
    }

    #[test]
    fn choosing_the_only_candidate_asks_for_confirmation() {
        let mut p = picker(&one_hunk());
        match p.feed(b"1") {
            Step::Confirm { view, patch } => {
                assert!(view.contains("corrected patch"), "{view}");
                assert!(!view.contains("WARNING"), "{view}");
                assert!(patch.contains(" let a = 1;"), "{patch}");
                assert!(!patch.contains("let a = 11;"), "{patch}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn two_presses_apply_the_corrected_patch() {
        let mut p = picker(&one_hunk());
        let Step::Confirm { patch, .. } = p.feed(b"1") else { panic!() };
        assert_eq!(p.feed(&[CONFIRM_KEY]), Step::Waiting { just_armed: true });
        assert_eq!(p.feed(&[CONFIRM_KEY]), Step::Apply { text: patch, rest: vec![] });
    }

    #[test]
    fn both_presses_in_one_read_apply_and_keep_what_follows() {
        let mut p = picker(&one_hunk());
        let Step::Confirm { patch, .. } = p.feed(b"1") else { panic!() };
        assert_eq!(p.feed(b"\x02\x02ls\n"), Step::Apply { text: patch, rest: b"ls\n".to_vec() });
    }

    #[test]
    fn the_choice_and_the_confirmation_may_arrive_in_one_read() {
        let mut p = picker(&one_hunk());
        // "1", then Ctrl+B: the confirmation is shown and already armed.
        assert!(matches!(p.feed(b"1\x02"), Step::Confirm { .. }));
        assert!(matches!(p.feed(&[CONFIRM_KEY]), Step::Apply { .. }));
        let mut p = picker(&one_hunk());
        assert!(matches!(p.feed(b"1\x02\x02x"), Step::Apply { rest, .. } if rest == b"x"));
    }

    #[test]
    fn another_key_while_confirming_cancels_and_is_handed_on() {
        let mut p = picker(&one_hunk());
        let _ = p.feed(b"1");
        assert_eq!(p.feed(b"x"), Step::Cancel { rest: b"x".to_vec() });
        let mut p = picker(&one_hunk());
        let _ = p.feed(b"1");
        let _ = p.feed(&[CONFIRM_KEY]);
        assert_eq!(p.feed(b"\r"), Step::Cancel { rest: b"\r".to_vec() });
    }

    #[test]
    fn skip_and_quit_cancel_the_whole_patch_and_consume_the_key() {
        for key in [b"s", b"q"] {
            let mut p = picker(&one_hunk());
            assert_eq!(p.feed(key), Step::Cancel { rest: vec![] });
        }
    }

    #[test]
    fn an_unknown_key_cancels_and_is_handed_on_whole() {
        let mut p = picker(&one_hunk());
        assert_eq!(p.feed(b"z"), Step::Cancel { rest: b"z".to_vec() });
        let mut p = picker(&one_hunk());
        assert_eq!(p.feed(&[CONFIRM_KEY]), Step::Cancel { rest: vec![CONFIRM_KEY] });
    }

    fn two_candidate_picker() -> Picker {
        let file = concat!(
            "fn first_here() {\n",
            "    let a = 1;\n",
            "    let b = 2;\n",
            "}\n",
            "\n",
            "fn second_here() {\n",
            "    let a = 1;\n",
            "    let b = 2;\n",
            "}\n",
        );
        let text = envelope(
            "*** Update File: f\n@@\n fn missing() {\n     let a = 1;\n-    let b = 22;\n+    let b = 5;\n }\n",
        );
        let originals = HashMap::from([("f".to_string(), file.to_string())]);
        Picker::new(&text, Format::ApplyPatch, originals).expect("two candidates")
    }

    #[test]
    fn arrow_keys_browse_candidates_and_enter_selects_the_highlighted_one() {
        let mut p = two_candidate_picker();
        assert!(p.first_view().contains("Candidate 1 of 2"));

        match p.feed(b"\x1b[C") {
            Step::Show(view) => assert!(view.contains("Candidate 2 of 2"), "{view}"),
            other => panic!("{other:?}"),
        }

        match p.feed(b"\r") {
            Step::Confirm { patch, .. } => {
                assert!(patch.contains("fn second_here()"), "{patch}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arrow_sequences_split_across_reads_are_recognized() {
        let mut p = two_candidate_picker();
        let now = Instant::now();

        assert_eq!(
            p.feed_at(b"\x1b", now),
            Step::Waiting { just_armed: false }
        );
        assert_eq!(
            p.feed_at(b"[", now + Duration::from_millis(5)),
            Step::Waiting { just_armed: false }
        );

        match p.feed_at(b"C", now + Duration::from_millis(10)) {
            Step::Show(view) => assert!(view.contains("Candidate 2 of 2"), "{view}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(p.poll_timeout_ms(now + Duration::from_millis(10)), None);
    }

    #[test]
    fn a_lone_escape_cancels_only_after_its_deadline() {
        let mut p = two_candidate_picker();
        let now = Instant::now();

        assert_eq!(
            p.feed_at(b"\x1b", now),
            Step::Waiting { just_armed: false }
        );
        assert_eq!(p.poll_timeout_ms(now), Some(30));
        assert_eq!(p.flush_if_due(now + Duration::from_millis(29)), None);
        assert_eq!(
            p.flush_if_due(now + Duration::from_millis(30)),
            Some(Step::Cancel { rest: Vec::new() })
        );
    }

    #[test]
    fn an_incomplete_non_escape_sequence_is_returned_to_the_shell() {
        let mut p = two_candidate_picker();
        let now = Instant::now();

        assert_eq!(
            p.feed_at(b"\x1b[", now),
            Step::Waiting { just_armed: false }
        );
        assert_eq!(
            p.flush_if_due(now + ESCAPE_SEQUENCE_WAIT),
            Some(Step::Cancel { rest: b"\x1b[".to_vec() })
        );
    }

    #[test]
    fn the_review_bundle_contains_all_candidates_and_untruncated_differences() {
        let p = two_candidate_picker();
        let bundle = p.review_bundle();
        assert!(bundle.contains("CVB FAILED-HUNK REVIEW"), "{bundle}");
        assert!(bundle.contains("fn missing()"), "{bundle}");
    assert!(bundle.contains("actual L1: fn first_here()"), "{bundle}");
    assert!(bundle.contains("actual L6: fn second_here()"), "{bundle}");
        assert!(bundle.contains("LOOSE"), "{bundle}");
    }

    #[test]
    fn a_digit_without_a_candidate_is_ignored() {
        let mut p = picker(&one_hunk());
        assert_eq!(p.feed(b"2"), Step::Waiting { just_armed: false });
        assert_eq!(p.feed(b"0"), Step::Cancel { rest: b"0".to_vec() });
        let mut p = picker(&one_hunk());
        assert_eq!(p.feed(b"9"), Step::Waiting { just_armed: false });
        assert!(matches!(p.feed(b"1"), Step::Confirm { .. }));
    }

    const TWO_FILE: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\no\n";

    fn two_hunks() -> (String, HashMap<String, String>) {
        let text = envelope("*** Update File: f\n@@\n a\n bb\n-c\n+C\n d\n@@\n l\n mm\n-n\n+N\n o\n");
        (text, HashMap::from([("f".to_string(), TWO_FILE.to_string())]))
    }

    #[test]
    fn each_failing_hunk_gets_its_turn_in_order() {
        let (text, originals) = two_hunks();
        let mut p = Picker::new(&text, Format::ApplyPatch, originals).unwrap();
        assert_eq!(p.hunk_count(), 2);
        assert!(p.first_view().contains("Hunk 1 of 2 to decide: f, hunk 1 of 2"));
        match p.feed(b"1") {
            Step::Show(view) => assert!(view.contains("Hunk 2 of 2 to decide: f, hunk 2 of 2"), "{view}"),
            other => panic!("{other:?}"),
        }
        match p.feed(b"1") {
            Step::Confirm { patch, .. } => {
                assert!(patch.contains(" b\n-c\n+C\n"), "{patch}");
                assert!(patch.contains(" m\n-n\n+N\n"), "{patch}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn skipping_the_second_hunk_cancels_everything() {
        let (text, originals) = two_hunks();
        let mut p = Picker::new(&text, Format::ApplyPatch, originals).unwrap();
        let _ = p.feed(b"1");
        assert_eq!(p.feed(b"s"), Step::Cancel { rest: vec![] });
    }

    #[test]
    fn matching_hunks_are_left_alone_and_not_asked_about() {
        let text = envelope("*** Update File: f\n@@\n a\n-b\n+B\n c\n@@\n l\n mm\n-n\n+N\n o\n");
        let originals = HashMap::from([("f".to_string(), TWO_FILE.to_string())]);
        let mut p = Picker::new(&text, Format::ApplyPatch, originals).unwrap();
        assert_eq!(p.hunk_count(), 1);
        let Step::Confirm { patch, .. } = p.feed(b"1") else { panic!() };
        assert!(patch.contains(" a\n-b\n+B\n c\n"), "{patch}");
    }

    #[test]
    fn a_loose_choice_is_warned_about_on_the_confirmation_screen() {
        // The line to remove is misquoted: the real line would be removed.
        let text = envelope("*** Update File: f\n@@\n fn main() {\n     let a = 1;\n-    let b = 20;\n+    let b = 5;\n     let c = 3;\n");
        let mut p = picker(&text);
        let Step::Confirm { view, patch } = p.feed(b"1") else { panic!() };
        assert!(view.contains("WARNING"), "{view}");
        assert!(patch.contains("-    let b = 2;"), "{patch}");
    }

    #[test]
    fn a_diff_is_corrected_with_renumbered_headers() {
        let file = "one\ntwo\nthree\nfour\nfive\nsix\n";
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -9,3 +9,3 @@ ctx\n two\n-thre\n+3\n four\n";
        let originals = HashMap::from([("f".to_string(), file.to_string())]);
        let mut p = Picker::new(diff, Format::GitDiff, originals).unwrap();
        assert!(p.first_view().contains("(the patch says line 9)"));
        let Step::Confirm { patch, .. } = p.feed(b"1") else { panic!() };
        assert!(patch.contains("@@ -2,3 +2,3 @@ ctx"), "{patch}");
        assert!(patch.contains("-three"), "{patch}");
    }

    #[test]
    fn the_view_marks_removed_lines_and_says_how_a_candidate_differs() {
        let view = picker(&one_hunk()).first_view();
        assert!(view.contains("  -     let b = 2;"), "{view}");
        assert!(view.contains("1 line differ"), "{view}");
        assert!(view.contains("! expected:     let a = 11;"), "{view}");
        assert!(view.contains("!   actual:     let a = 1;"), "{view}");
    }

    #[test]
    fn the_last_choice_with_trailing_input_keeps_the_input() {
        let mut p = picker(&one_hunk());
        // A stray key after the choice cancels and is handed on.
        assert_eq!(p.feed(b"1x"), Step::Cancel { rest: b"x".to_vec() });
    }

    #[test]
    fn long_lines_and_long_candidates_are_cut() {
        let long = "x".repeat(300);
        assert_eq!(cut(&long).chars().count(), MAX_LINE_CHARS + 3);
        assert_eq!(cut("short"), "short");
    }
}

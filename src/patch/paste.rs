//! Recognizes a pasted `apply_patch` envelope in the bytes the user types.
//!
//! The terminal wraps a paste in the bracketed-paste markers `ESC[200~` and
//! `ESC[201~`. This state machine sits on the stdin path of the PTY proxy. It is
//! pure: it is fed bytes, told whether the shell is idle, and returns events. It
//! never reads a clock; the caller decides when to call [`PasteRecognizer::on_timeout`].
//!
//! Rules (see `design-apply-patch.md`, section 3):
//!
//! * Only a paste that *starts* while the shell is idle is examined.
//! * Anything that is not a patch is forwarded byte-identical, markers included.
//!   A paste is held back only while it can still turn out to be a patch: the
//!   first non-blank line must be exactly `*** Begin Patch`.
//! * A patch is buffered until the end marker, up to [`MAX_PATCH_BYTES`]. Over the
//!   cap, or on timeout, the held bytes are forwarded unchanged.
//! * A paste whose first non-blank line is `diff --git ...`, or a `--- a/...` line
//!   followed by `+++ `, is a unified diff. It has no end marker, so the whole
//!   paste is the patch and the parser judges whether it is complete.
//! * The whole paste must be exactly one patch: begin marker first, `*** End
//!   Patch` last, only blank lines outside them. A paste that starts like a patch
//!   but contains no `*** End Patch` line is [`PasteEvent::Reject`]ed (the
//!   clipboard probably truncated it) and is never forwarded to the shell. One
//!   that has an end line but also other text after it is forwarded unchanged.

/// Start of a bracketed paste.
pub(crate) const PASTE_START: &[u8] = b"\x1b[200~";
/// End of a bracketed paste.
pub(crate) const PASTE_END: &[u8] = b"\x1b[201~";
/// Largest paste that is held back (below the 4 MB Mutter clipboard truncation).
pub(crate) const MAX_PATCH_BYTES: usize = 3 * 1024 * 1024;

const BEGIN: &[u8] = b"*** Begin Patch";
const DIFF_GIT: &[u8] = b"diff --git ";
const DIFF_OLD: &[u8] = b"--- ";
const DIFF_NEW: &[u8] = b"+++ ";
const END_LINE: &[u8] = b"*** End Patch";

/// Why a patch-looking paste was not applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectReason {
    /// No `*** End Patch` line before the end of the paste.
    Incomplete,
    /// The patch is not valid UTF-8.
    NotUtf8,
}

/// Something the caller must do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PasteEvent {
    /// Send these bytes to the shell unchanged.
    Forward(Vec<u8>),
    /// A complete patch, normalized: starts with `*** Begin Patch`, ends with
    /// `*** End Patch` and one line feed. Nothing was sent to the shell.
    Patch(String),
    /// A patch that cannot be applied. Nothing was sent to the shell.
    Reject(RejectReason),
}

/// What the recognizer is holding back, so the caller can pick a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pending {
    /// Nothing is held.
    Nothing,
    /// A few bytes that might be the start of a paste marker (for example a lone
    /// Escape key). Flush after a very short wait.
    PartialMarker,
    /// A paste that may still be a patch. Flush after a longer wait.
    Paste,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between pastes: looking for the start marker.
    Normal,
    /// A paste started at an idle prompt; not yet known to be a patch.
    Sniff,
    /// Known to be a patch (or a diff); buffering until the end marker.
    Buffer,
    /// Inside a paste that is passed through; looking for its end marker.
    Foreign,
}

/// Stateful recognizer, kept across reads.
#[derive(Debug)]
pub(crate) struct PasteRecognizer {
    state: State,
    /// Unprocessed bytes. In `Sniff` and `Buffer` this is the paste body so far
    /// (without the start marker); in `Normal` and `Foreign` it is only a held
    /// partial marker.
    buf: Vec<u8>,
    /// Offset in `buf` from which the end marker still has to be searched.
    scan_from: usize,
    /// What the held paste turned out to be (valid from `Buffer` on).
    kind: Kind,
}

/// The syntax of a held paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `*** Begin Patch` ... `*** End Patch`.
    Envelope,
    /// A unified diff.
    Diff,
}

/// What the start of a paste says about a diff.
enum DiffSniff {
    Yes,
    No,
    /// Not enough bytes yet.
    More,
}

/// Does `rest` (the paste without leading blank space) start a unified diff?
/// `ended` says no more bytes will come.
fn sniff_diff(rest: &[u8], ended: bool) -> DiffSniff {
    let decide_more = |more: bool| if more && !ended { DiffSniff::More } else { DiffSniff::No };
    if rest.starts_with(DIFF_GIT) {
        return DiffSniff::Yes;
    }
    if DIFF_GIT.starts_with(rest) || DIFF_OLD.starts_with(rest) {
        return decide_more(true);
    }
    if !rest.starts_with(DIFF_OLD) {
        return DiffSniff::No;
    }
    // `--- ` line: it must name an `a/` path or /dev/null, then `+++ ` follows.
    let Some(newline) = rest.iter().position(|b| *b == b'\n') else {
        let line = trim_cr(rest);
        let fits = |operand: &[u8]| operand.starts_with(line) || line.starts_with(operand);
        return decide_more(fits(b"--- a/") || fits(b"--- /dev/null"));
    };
    let line = trim_cr(&rest[..newline]);
    if !(line.starts_with(b"--- a/") || line.starts_with(b"--- /dev/null")) {
        return DiffSniff::No;
    }
    let next = &rest[newline + 1..];
    if next.starts_with(DIFF_NEW) {
        DiffSniff::Yes
    } else {
        decide_more(DIFF_NEW.starts_with(next))
    }
}

impl Default for PasteRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl PasteRecognizer {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Normal,
            buf: Vec::new(),
            scan_from: 0,
            kind: Kind::Envelope,
        }
    }

    /// Reports what is held back.
    pub(crate) fn pending(&self) -> Pending {
        match self.state {
            State::Sniff | State::Buffer => Pending::Paste,
            State::Normal | State::Foreign if !self.buf.is_empty() => Pending::PartialMarker,
            _ => Pending::Nothing,
        }
    }

    /// Feeds bytes read from the terminal. `idle` says whether the shell is at a
    /// prompt right now; it only matters when a paste starts in `bytes`.
    pub(crate) fn feed(&mut self, bytes: &[u8], idle: bool) -> Vec<PasteEvent> {
        let mut events = Vec::new();
        self.buf.extend_from_slice(bytes);
        self.run(idle, &mut events);
        events
    }

    /// Gives up on whatever is held and forwards it unchanged. A paste cut short
    /// this way is then passed through until its end marker.
    pub(crate) fn on_timeout(&mut self) -> Vec<PasteEvent> {
        let mut events = Vec::new();
        match self.state {
            State::Normal | State::Foreign => {
                if !self.buf.is_empty() {
                    push_forward(&mut events, std::mem::take(&mut self.buf));
                }
            }
            State::Sniff | State::Buffer => {
                let mut bytes = PASTE_START.to_vec();
                bytes.append(&mut self.buf);
                push_forward(&mut events, bytes);
                self.state = State::Foreign;
            }
        }
        self.scan_from = 0;
        events
    }

    fn run(&mut self, idle: bool, events: &mut Vec<PasteEvent>) {
        loop {
            let progressed = match self.state {
                State::Normal => self.step_normal(idle, events),
                State::Foreign => self.step_foreign(events),
                State::Sniff => self.step_sniff(events),
                State::Buffer => self.step_buffer(events),
            };
            if !progressed {
                return;
            }
        }
    }

    /// Normal: forward everything up to a start marker; hold a partial marker.
    fn step_normal(&mut self, idle: bool, events: &mut Vec<PasteEvent>) -> bool {
        if let Some(at) = find(&self.buf, PASTE_START, 0) {
            let rest = self.buf.split_off(at + PASTE_START.len());
            self.buf.truncate(at);
            if !self.buf.is_empty() {
                push_forward(events, std::mem::take(&mut self.buf));
            }
            self.buf = rest;
            self.scan_from = 0;
            if idle {
                self.state = State::Sniff;
            } else {
                push_forward(events, PASTE_START.to_vec());
                self.state = State::Foreign;
            }
            return true;
        }
        self.forward_all_but_partial(PASTE_START, events);
        false
    }

    /// Foreign: forward up to and including the end marker; hold a partial one.
    fn step_foreign(&mut self, events: &mut Vec<PasteEvent>) -> bool {
        if let Some(at) = find(&self.buf, PASTE_END, 0) {
            let rest = self.buf.split_off(at + PASTE_END.len());
            push_forward(events, std::mem::take(&mut self.buf));
            self.buf = rest;
            self.state = State::Normal;
            return true;
        }
        self.forward_all_but_partial(PASTE_END, events);
        false
    }

    fn forward_all_but_partial(&mut self, marker: &[u8], events: &mut Vec<PasteEvent>) {
        let keep = partial_suffix_len(&self.buf, marker);
        let split = self.buf.len() - keep;
        let held = self.buf.split_off(split);
        if !self.buf.is_empty() {
            push_forward(events, std::mem::take(&mut self.buf));
        }
        self.buf = held;
    }

    /// Sniff: decide whether the paste can still be a patch.
    fn step_sniff(&mut self, events: &mut Vec<PasteEvent>) -> bool {
        if self.buf.len() > MAX_PATCH_BYTES {
            self.give_up(events);
            return true;
        }
        let end = self.find_end();
        let content_len = end.unwrap_or(self.buf.len());
        let content = &self.buf[..content_len];
        let skip = content
            .iter()
            .take_while(|b| matches!(**b, b' ' | b'\t' | b'\r' | b'\n'))
            .count();
        let rest = &content[skip..];

        if !(rest.starts_with(BEGIN) || BEGIN.starts_with(rest)) {
            // Not an envelope: maybe a unified diff.
            return match sniff_diff(rest, end.is_some()) {
                DiffSniff::Yes => {
                    self.kind = Kind::Diff;
                    self.state = State::Buffer;
                    true
                }
                DiffSniff::No => {
                    self.not_a_patch(end, events);
                    true
                }
                DiffSniff::More => false,
            };
        }
        self.kind = Kind::Envelope;

        if rest.len() < BEGIN.len() {
            if !BEGIN.starts_with(rest) || end.is_some() {
                // Not a patch (or an empty paste): pass through.
                self.not_a_patch(end, events);
                return true;
            }
            return false; // need more bytes
        }
        if &rest[..BEGIN.len()] != BEGIN {
            self.not_a_patch(end, events);
            return true;
        }
        match rest.get(BEGIN.len()) {
            Some(b'\n') | Some(b'\r') => {
                self.state = State::Buffer;
                true
            }
            // `*** Begin Patch` and then nothing more: a patch that ends right
            // here (rejected as incomplete) or one that needs more bytes.
            None if end.is_some() => {
                self.state = State::Buffer;
                true
            }
            None => false,
            // The start of the end marker may be split across reads.
            Some(_) if PASTE_END.starts_with(&rest[BEGIN.len()..]) => false,
            Some(_) => {
                self.not_a_patch(end, events);
                true
            }
        }
    }

    /// Buffer: wait for the end marker, then classify the body.
    fn step_buffer(&mut self, events: &mut Vec<PasteEvent>) -> bool {
        if self.buf.len() > MAX_PATCH_BYTES {
            self.give_up(events);
            return true;
        }
        let Some(at) = self.find_end() else {
            return false;
        };
        let rest = self.buf.split_off(at + PASTE_END.len());
        self.buf.truncate(at);
        let body = std::mem::take(&mut self.buf);
        self.buf = rest;
        self.scan_from = 0;
        self.state = State::Normal;
        events.push(match self.kind {
            Kind::Envelope => classify(body),
            Kind::Diff => classify_diff(body),
        });
        true
    }

    /// Finds the end marker, remembering how far the buffer has been searched.
    fn find_end(&mut self) -> Option<usize> {
        let found = find(&self.buf, PASTE_END, self.scan_from);
        if found.is_none() {
            self.scan_from = self.buf.len().saturating_sub(PASTE_END.len() - 1);
        }
        found
    }

    /// The paste is not a patch: forward the start marker and what was held, and
    /// continue in `Foreign` (or `Normal` if the end marker is already in hand).
    fn not_a_patch(&mut self, end: Option<usize>, events: &mut Vec<PasteEvent>) {
        let mut bytes = PASTE_START.to_vec();
        match end {
            Some(at) => {
                let rest = self.buf.split_off(at + PASTE_END.len());
                bytes.append(&mut self.buf);
                self.buf = rest;
                self.state = State::Normal;
            }
            None => {
                bytes.append(&mut self.buf);
                self.state = State::Foreign;
            }
        }
        self.scan_from = 0;
        push_forward(events, bytes);
    }

    /// Over the size cap: forward everything held, unchanged.
    fn give_up(&mut self, events: &mut Vec<PasteEvent>) {
        let mut bytes = PASTE_START.to_vec();
        bytes.append(&mut self.buf);
        push_forward(events, bytes);
        self.state = State::Foreign;
        self.scan_from = 0;
    }
}

/// A unified diff: the whole paste, without blank lines around it, ending in
/// one line feed. Only line feeds are trimmed at the end (a last line may be a
/// context line holding a single space).
fn classify_diff(body: Vec<u8>) -> PasteEvent {
    match std::str::from_utf8(&body) {
        Ok(text) => {
            let text = text.trim_start_matches([' ', '\t', '\r', '\n']);
            let text = text.trim_end_matches(['\r', '\n']);
            PasteEvent::Patch(format!("{text}\n"))
        }
        Err(_) => PasteEvent::Reject(RejectReason::NotUtf8),
    }
}

/// Decides what a complete paste body is.
fn classify(body: Vec<u8>) -> PasteEvent {
    let lines = line_spans(&body);
    let is_end = |&(s, e): &(usize, usize)| trim_cr(&body[s..e]) == END_LINE;
    let is_blank = |&(s, e): &(usize, usize)| body[s..e].iter().all(u8::is_ascii_whitespace);

    // Sniffing guaranteed the first non-blank line is `*** Begin Patch`.
    let first = lines.iter().find(|l| !is_blank(l));
    let last = lines.iter().rev().find(|l| !is_blank(l));
    let (Some(&(start, _)), Some(last)) = (first, last) else {
        return PasteEvent::Reject(RejectReason::Incomplete);
    };

    let begin_lines = lines
        .iter()
        .filter(|&&(s, e)| trim_cr(&body[s..e]) == BEGIN)
        .count();
    let end_lines = lines.iter().filter(|l| is_end(l)).count();
    if begin_lines > 1 || end_lines > 1 {
        // Several patches in one paste: not exactly one patch.
        let mut bytes = PASTE_START.to_vec();
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(PASTE_END);
        return PasteEvent::Forward(bytes);
    }

    if !is_end(last) {
        if end_lines > 0 {
            // An end line followed by other text: not exactly one patch.
            let mut bytes = PASTE_START.to_vec();
            bytes.extend_from_slice(&body);
            bytes.extend_from_slice(PASTE_END);
            return PasteEvent::Forward(bytes);
        }
        return PasteEvent::Reject(RejectReason::Incomplete);
    }

    let end_of_text = last.0 + trim_cr(&body[last.0..last.1]).len();
    match std::str::from_utf8(&body[start..end_of_text]) {
        Ok(text) => {
            let mut patch = String::with_capacity(text.len() + 1);
            patch.push_str(text);
            patch.push('\n');
            PasteEvent::Patch(patch)
        }
        Err(_) => PasteEvent::Reject(RejectReason::NotUtf8),
    }
}

/// `(start, end)` of each line, end excluding the line feed.
fn line_spans(body: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (i, byte) in body.iter().enumerate() {
        if *byte == b'\n' {
            spans.push((start, i));
            start = i + 1;
        }
    }
    if start < body.len() {
        spans.push((start, body.len()));
    }
    spans
}

fn trim_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if haystack.len() < needle.len() || from > haystack.len() - needle.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Length of the longest proper prefix of `marker` that ends `buf`.
fn partial_suffix_len(buf: &[u8], marker: &[u8]) -> usize {
    (1..marker.len())
        .rev()
        .find(|&n| buf.len() >= n && buf[buf.len() - n..] == marker[..n])
        .unwrap_or(0)
}

/// Appends a forward event, merging with a preceding one.
fn push_forward(events: &mut Vec<PasteEvent>, bytes: Vec<u8>) {
    if bytes.is_empty() {
        return;
    }
    if let Some(PasteEvent::Forward(last)) = events.last_mut() {
        last.extend_from_slice(&bytes);
    } else {
        events.push(PasteEvent::Forward(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End Patch";

    fn wrapped(body: &str) -> Vec<u8> {
        let mut v = PASTE_START.to_vec();
        v.extend_from_slice(body.as_bytes());
        v.extend_from_slice(PASTE_END);
        v
    }

    /// Feeds `input` in the given chunk sizes (cycled) and merges the events.
    fn run(input: &[u8], sizes: &[usize], idle: bool) -> (Vec<PasteEvent>, PasteRecognizer) {
        let mut r = PasteRecognizer::new();
        let mut events: Vec<PasteEvent> = Vec::new();
        let mut at = 0;
        let mut i = 0;
        while at < input.len() {
            let n = sizes[i % sizes.len()].max(1).min(input.len() - at);
            for ev in r.feed(&input[at..at + n], idle) {
                match ev {
                    PasteEvent::Forward(b) => push_forward(&mut events, b),
                    other => events.push(other),
                }
            }
            at += n;
            i += 1;
        }
        (events, r)
    }

    fn forwarded(events: &[PasteEvent]) -> Vec<u8> {
        events
            .iter()
            .filter_map(|e| match e {
                PasteEvent::Forward(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn expected_patch() -> Vec<PasteEvent> {
        vec![PasteEvent::Patch(format!("{PATCH}\n"))]
    }

    #[test]
    fn recognizes_a_patch_without_a_trailing_newline() {
        // This is what foot sends: the paste ends right after `*** End Patch`.
        let (events, r) = run(&wrapped(PATCH), &[usize::MAX], true);
        assert_eq!(events, expected_patch());
        assert_eq!(r.pending(), Pending::Nothing);
    }

    #[test]
    fn recognizes_a_patch_for_every_chunking() {
        let input = wrapped(PATCH);
        for size in 1..=input.len() {
            let (events, r) = run(&input, &[size], true);
            assert_eq!(events, expected_patch(), "chunk size {size}");
            assert_eq!(r.pending(), Pending::Nothing, "chunk size {size}");
        }
    }

    #[test]
    fn recognizes_a_patch_for_irregular_chunkings() {
        let input = wrapped(PATCH);
        for sizes in [[1, 7, 2], [5, 1, 1], [3, 3, 11], [2, 9, 1]] {
            let (events, _) = run(&input, &sizes, true);
            assert_eq!(events, expected_patch(), "sizes {sizes:?}");
        }
    }

    #[test]
    fn tolerates_blank_lines_and_crlf_around_the_patch() {
        let body = format!("\n  \n{}\r\n\n", PATCH.replace('\n', "\r\n"));
        let (events, _) = run(&wrapped(&body), &[4], true);
        let [PasteEvent::Patch(text)] = events.as_slice() else {
            panic!("expected one patch, got {events:?}");
        };
        // Structure lines lose their CR at the ends; body lines are untouched.
        assert!(text.starts_with("*** Begin Patch\r\n*** Update File: a.txt\r\n"));
        assert!(text.ends_with("\r\n*** End Patch\n"));
    }

    #[test]
    fn patch_bytes_inside_the_body_are_preserved() {
        let body = "*** Begin Patch\n*** Add File: x.txt\n+tab\there  \n+é\n*** End Patch";
        let (events, _) = run(&wrapped(body), &[3], true);
        assert_eq!(events, vec![PasteEvent::Patch(format!("{body}\n"))]);
    }

    #[test]
    fn plain_text_input_is_forwarded_byte_identical_for_every_chunking() {
        let mut input = b"ls -la\n".to_vec();
        input.extend_from_slice(&wrapped("echo hi\nsecond line"));
        input.extend_from_slice(b"\x1b[A\x1b\x1b[1;5C tail");
        for size in 1..=input.len() {
            let (events, r) = run(&input, &[size], true);
            assert_eq!(forwarded(&events), input, "chunk size {size}");
            assert!(
                events.iter().all(|e| matches!(e, PasteEvent::Forward(_))),
                "chunk size {size}"
            );
            assert_eq!(r.pending(), Pending::Nothing, "chunk size {size}");
        }
    }

    #[test]
    fn a_paste_that_only_mentions_the_marker_is_forwarded() {
        for body in [
            "echo '*** Begin Patch'",
            "see *** Begin Patch\n*** End Patch",
            "*** Begin Patchwork\nmore",
            "*** begin patch\n*** End Patch",
            "** Begin Patch",
            "",
            "   ",
        ] {
            let input = wrapped(body);
            for size in [1, 2, 5, usize::MAX] {
                let (events, r) = run(&input, &[size], true);
                assert_eq!(forwarded(&events), input, "{body:?} chunk {size}");
                assert!(
                    events.iter().all(|e| matches!(e, PasteEvent::Forward(_))),
                    "{body:?} chunk {size}"
                );
                assert_eq!(r.pending(), Pending::Nothing, "{body:?} chunk {size}");
            }
        }
    }

    #[test]
    fn a_patch_inside_other_text_is_forwarded() {
        let body = format!("run this:\n{PATCH}");
        let input = wrapped(&body);
        let (events, _) = run(&input, &[3], true);
        assert_eq!(forwarded(&events), input);
    }

    #[test]
    fn a_pasted_patch_followed_by_other_text_is_forwarded() {
        let body = format!("{PATCH}\nrm -rf x\n");
        let input = wrapped(&body);
        for size in [1, 4, usize::MAX] {
            let (events, _) = run(&input, &[size], true);
            assert_eq!(forwarded(&events), input, "chunk {size}");
            assert!(events.iter().all(|e| matches!(e, PasteEvent::Forward(_))));
        }
    }

    #[test]
    fn two_patches_in_one_paste_are_forwarded_not_applied() {
        let body = format!("{PATCH}\n{PATCH}");
        let input = wrapped(&body);
        let (events, _) = run(&input, &[usize::MAX], true);
        assert_eq!(forwarded(&events), input);
    }

    #[test]
    fn a_truncated_patch_is_rejected_and_never_forwarded() {
        let body = "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+ne";
        for size in [1, 3, usize::MAX] {
            let (events, r) = run(&wrapped(body), &[size], true);
            assert_eq!(
                events,
                vec![PasteEvent::Reject(RejectReason::Incomplete)],
                "chunk {size}"
            );
            assert_eq!(r.pending(), Pending::Nothing);
        }
    }

    #[test]
    fn a_begin_line_alone_is_rejected_as_incomplete() {
        for body in ["*** Begin Patch", "*** Begin Patch\n", "\n*** Begin Patch\r\n"] {
            let (events, _) = run(&wrapped(body), &[2], true);
            assert_eq!(
                events,
                vec![PasteEvent::Reject(RejectReason::Incomplete)],
                "{body:?}"
            );
        }
    }

    #[test]
    fn a_patch_that_is_not_utf8_is_rejected() {
        let mut input = PASTE_START.to_vec();
        input.extend_from_slice(b"*** Begin Patch\n*** Add File: x\n+\xff\xfe\n*** End Patch");
        input.extend_from_slice(PASTE_END);
        let (events, _) = run(&input, &[usize::MAX], true);
        assert_eq!(events, vec![PasteEvent::Reject(RejectReason::NotUtf8)]);
    }

    #[test]
    fn a_paste_is_not_examined_when_the_shell_is_not_idle() {
        let input = wrapped(PATCH);
        for size in [1, 6, usize::MAX] {
            let (events, r) = run(&input, &[size], false);
            assert_eq!(forwarded(&events), input, "chunk {size}");
            assert!(events.iter().all(|e| matches!(e, PasteEvent::Forward(_))));
            assert_eq!(r.pending(), Pending::Nothing, "chunk {size}");
        }
    }

    #[test]
    fn idleness_is_read_when_the_paste_starts_not_later() {
        // The start marker arrives while busy; the rest while idle: still passed.
        let input = wrapped(PATCH);
        let mut r = PasteRecognizer::new();
        let mut got = forwarded(&r.feed(&input[..10], false));
        got.extend(forwarded(&r.feed(&input[10..], true)));
        assert_eq!(got, input);
    }

    #[test]
    fn keys_typed_after_a_patch_are_processed_in_order() {
        let mut input = b"ls\n".to_vec();
        input.extend_from_slice(&wrapped(PATCH));
        input.extend_from_slice(b"pwd\n");
        for size in [1, 5, usize::MAX] {
            let (events, _) = run(&input, &[size], true);
            assert_eq!(
                events,
                vec![
                    PasteEvent::Forward(b"ls\n".to_vec()),
                    PasteEvent::Patch(format!("{PATCH}\n")),
                    PasteEvent::Forward(b"pwd\n".to_vec()),
                ],
                "chunk {size}"
            );
        }
    }

    #[test]
    fn two_patch_pastes_in_a_row_are_both_recognized() {
        let mut input = wrapped(PATCH);
        input.extend_from_slice(&wrapped(PATCH));
        let (events, _) = run(&input, &[7], true);
        assert_eq!(
            events,
            vec![
                PasteEvent::Patch(format!("{PATCH}\n")),
                PasteEvent::Patch(format!("{PATCH}\n")),
            ]
        );
    }

    #[test]
    fn a_lone_escape_is_held_then_flushed_by_timeout() {
        let mut r = PasteRecognizer::new();
        assert!(r.feed(b"\x1b", true).is_empty());
        assert_eq!(r.pending(), Pending::PartialMarker);
        assert_eq!(r.on_timeout(), vec![PasteEvent::Forward(b"\x1b".to_vec())]);
        assert_eq!(r.pending(), Pending::Nothing);
        // Normal service resumes.
        assert_eq!(
            r.feed(b"a", true),
            vec![PasteEvent::Forward(b"a".to_vec())]
        );
    }

    #[test]
    fn a_partial_start_marker_that_turns_out_to_be_something_else_is_released() {
        let mut r = PasteRecognizer::new();
        assert!(r.feed(b"\x1b[20", true).is_empty());
        assert_eq!(
            r.feed(b"1~x", true),
            vec![PasteEvent::Forward(b"\x1b[201~x".to_vec())]
        );
    }

    #[test]
    fn a_stalled_patch_paste_is_flushed_unchanged_and_the_rest_passes_through() {
        let mut r = PasteRecognizer::new();
        let head = b"\x1b[200~*** Begin Patch\n*** Update File: a.txt\n";
        assert!(r.feed(head, true).is_empty());
        assert_eq!(r.pending(), Pending::Paste);
        assert_eq!(r.on_timeout(), vec![PasteEvent::Forward(head.to_vec())]);
        assert_eq!(r.pending(), Pending::Nothing);
        // The remainder of that same paste, then ordinary typing.
        assert_eq!(
            r.feed(b"@@\n*** End Patch\x1b[201~ls", true),
            vec![PasteEvent::Forward(
                b"@@\n*** End Patch\x1b[201~ls".to_vec()
            )]
        );
        assert_eq!(r.feed(b"\n", true), vec![PasteEvent::Forward(b"\n".to_vec())]);
    }

    #[test]
    fn a_stalled_sniff_is_flushed_unchanged() {
        let mut r = PasteRecognizer::new();
        assert!(r.feed(b"\x1b[200~*** Beg", true).is_empty());
        assert_eq!(r.pending(), Pending::Paste);
        assert_eq!(
            r.on_timeout(),
            vec![PasteEvent::Forward(b"\x1b[200~*** Beg".to_vec())]
        );
    }

    #[test]
    fn an_oversized_patch_paste_is_forwarded_unchanged() {
        let mut body = String::from("*** Begin Patch\n*** Add File: big.txt\n");
        while body.len() <= MAX_PATCH_BYTES {
            body.push_str("+xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n");
        }
        body.push_str("*** End Patch");
        let input = wrapped(&body);
        let (events, r) = run(&input, &[8192], true);
        assert_eq!(forwarded(&events), input);
        assert!(events.iter().all(|e| matches!(e, PasteEvent::Forward(_))));
        assert_eq!(r.pending(), Pending::Nothing);
    }

    #[test]
    fn a_patch_just_under_the_cap_is_recognized() {
        let mut body = String::from("*** Begin Patch\n*** Add File: big.txt\n");
        let line = "+yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\n";
        while body.len() + line.len() + 20 < MAX_PATCH_BYTES {
            body.push_str(line);
        }
        body.push_str("*** End Patch");
        assert!(body.len() <= MAX_PATCH_BYTES);
        let (events, _) = run(&wrapped(&body), &[8192], true);
        assert!(matches!(events.as_slice(), [PasteEvent::Patch(_)]));
    }

    #[test]
    fn a_literal_end_marker_split_across_reads_inside_a_patch_is_found() {
        let input = wrapped(PATCH);
        let cut = input.len() - 3; // splits ESC[201~
        let mut r = PasteRecognizer::new();
        assert!(r.feed(&input[..cut], true).is_empty());
        assert_eq!(r.feed(&input[cut..], true), expected_patch());
    }

    /// Chunking never changes the result: random splits of mixed input agree with
    /// feeding everything at once.
    #[test]
    fn random_chunkings_agree_with_a_single_read() {
        let mut input = b"echo before\n".to_vec();
        input.extend_from_slice(&wrapped(PATCH));
        input.extend_from_slice(&wrapped("plain paste\nlines"));
        input.extend_from_slice(b"\x1b[200");
        input.extend_from_slice(&wrapped("*** Begin Patch\n+x"));
        input.extend_from_slice(b"tail\x1b");
        let (whole, whole_r) = run(&input, &[usize::MAX], true);

        let mut seed: u64 = 0x9e3779b97f4a7c15;
        for _ in 0..500 {
            let mut sizes = Vec::new();
            for _ in 0..8 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                sizes.push(1 + (seed >> 33) as usize % 13);
            }
            let (events, r) = run(&input, &sizes, true);
            assert_eq!(events, whole, "sizes {sizes:?}");
            assert_eq!(r.pending(), whole_r.pending(), "sizes {sizes:?}");
        }
    }

    const DIFF: &str = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new";
    const PLAIN_DIFF: &str = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new";

    fn diff_events(text: &str) -> Vec<PasteEvent> {
        vec![PasteEvent::Patch(format!("{}\n", text.trim_end_matches('\n')))]
    }

    #[test]
    fn recognizes_a_git_diff_for_every_chunking() {
        for text in [DIFF, PLAIN_DIFF] {
            let input = wrapped(text);
            for size in 1..=input.len() {
                let (events, r) = run(&input, &[size], true);
                assert_eq!(events, diff_events(text), "chunk size {size}: {text}");
                assert_eq!(r.pending(), Pending::Nothing, "chunk size {size}");
            }
        }
    }

    #[test]
    fn a_diff_is_trimmed_of_blank_lines_and_ends_in_one_line_feed() {
        let (events, _) = run(&wrapped(&format!("\n \n{DIFF}\n\n\n")), &[usize::MAX], true);
        assert_eq!(events, diff_events(DIFF));
        // A final context line that is a single space survives.
        let with_blank_context = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n+b\n ";
        let (events, _) = run(&wrapped(with_blank_context), &[usize::MAX], true);
        assert_eq!(events, vec![PasteEvent::Patch(format!("{with_blank_context}\n"))]);
    }

    #[test]
    fn a_diff_with_crlf_lines_is_kept_as_pasted() {
        let crlf = DIFF.replace('\n', "\r\n");
        let (events, _) = run(&wrapped(&crlf), &[usize::MAX], true);
        let [PasteEvent::Patch(text)] = events.as_slice() else { panic!("{events:?}") };
        assert!(text.starts_with("diff --git a/a.txt b/a.txt\r\n") && text.ends_with("+new\n"), "{text:?}");
    }

    #[test]
    fn a_diff_that_is_not_valid_utf8_is_rejected() {
        let mut input = PASTE_START.to_vec();
        input.extend_from_slice(b"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-\xff\n+b");
        input.extend_from_slice(PASTE_END);
        let (events, _) = run(&input, &[usize::MAX], true);
        assert_eq!(events, vec![PasteEvent::Reject(RejectReason::NotUtf8)]);
    }

    #[test]
    fn pastes_that_only_resemble_a_diff_reach_the_shell_unchanged() {
        let cases = [
            "---\ntitle: notes\n---\n",         // YAML front matter
            "--- some markdown rule\n",           // not an a/ path
            "--- a/x\nnot a plus line\n",         // no +++ after ---
            "--- notes\n+++ more notes\n",        // a ---/+++ pair, but not a/ paths
            "--- a/x",                             // cut right there
            "diff -u old new\n",                  // classic diff command line
            "diff --gi",                           // cut inside the marker
            "difference between the two\n",
            "- item\n- item\n",
            "+++ b/x\n",
        ];
        for text in cases {
            let input = wrapped(text);
            for size in [usize::MAX, 7, 1] {
                let (events, r) = run(&input, &[size], true);
                assert_eq!(forwarded(&events), input, "{text:?} chunk {size}");
                assert!(!events.iter().any(|e| !matches!(e, PasteEvent::Forward(_))), "{text:?}");
                assert_eq!(r.pending(), Pending::Nothing);
            }
        }
    }

    #[test]
    fn a_diff_pasted_while_a_command_runs_is_not_touched() {
        let input = wrapped(DIFF);
        let (events, _) = run(&input, &[usize::MAX], false);
        assert_eq!(forwarded(&events), input);
    }

    #[test]
    fn a_partial_diff_is_held_and_then_handed_over_on_timeout() {
        let mut r = PasteRecognizer::new();
        let mut input = PASTE_START.to_vec();
        input.extend_from_slice(b"diff --git a/x b/x\n--- a/x\n");
        assert!(r.feed(&input, true).is_empty());
        assert_eq!(r.pending(), Pending::Paste);
        let events = r.on_timeout();
        assert_eq!(forwarded(&events), input);
    }

    #[test]
    fn a_diff_followed_by_more_pastes_leaves_the_recognizer_ready() {
        let mut input = wrapped(DIFF);
        input.extend_from_slice(b"ls\r");
        input.extend_from_slice(&wrapped(PATCH));
        let (events, r) = run(&input, &[usize::MAX], true);
        assert_eq!(events.len(), 3, "{events:?}");
        assert!(matches!(&events[0], PasteEvent::Patch(t) if t.starts_with("diff --git")));
        assert_eq!(events[1], PasteEvent::Forward(b"ls\r".to_vec()));
        assert_eq!(events[2], expected_patch()[0]);
        assert_eq!(r.pending(), Pending::Nothing);
    }
}

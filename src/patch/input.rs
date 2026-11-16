//! The recognizer plus the clocks the proxy needs around it.
//!
//! [`PasteRecognizer`](crate::patch::paste::PasteRecognizer) is pure and never
//! looks at a clock. The proxy has to flush what the recognizer holds when the
//! terminal goes quiet: a few bytes that might start a paste marker (an Escape
//! key press) after a moment, a patch paste that stopped arriving after a few
//! seconds. This wrapper turns that into one deadline the poll loop can sleep on.

use std::time::{Duration, Instant};

use crate::patch::paste::{Pending, PasteEvent, PasteRecognizer};

/// How long a lone Escape (or other partial marker) is held.
pub(crate) const PARTIAL_MARKER_WAIT: Duration = Duration::from_millis(30);
/// How long a possible patch may stay silent before it is passed through.
pub(crate) const PASTE_STALL_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct PatchInput {
    enabled: bool,
    recognizer: PasteRecognizer,
    deadline: Option<Instant>,
}

impl PatchInput {
    /// A disabled input forwards every byte untouched.
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled, recognizer: PasteRecognizer::new(), deadline: None }
    }

    /// Feeds bytes from the terminal; `idle` says whether the shell is at a prompt.
    pub(crate) fn feed(&mut self, bytes: &[u8], idle: bool, now: Instant) -> Vec<PasteEvent> {
        if !self.enabled {
            return vec![PasteEvent::Forward(bytes.to_vec())];
        }
        let events = self.recognizer.feed(bytes, idle);
        self.set_deadline(now);
        events
    }

    /// Milliseconds the poll loop may sleep, or `None` to sleep until input.
    pub(crate) fn poll_timeout_ms(&self, now: Instant) -> Option<u16> {
        let deadline = self.deadline?;
        let left = deadline.saturating_duration_since(now);
        // Round up so the poll never wakes a hair early and spins.
        let millis = left.as_millis() + u128::from(left.subsec_nanos() % 1_000_000 != 0);
        Some(millis.min(u128::from(u16::MAX)) as u16)
    }

    /// Flushes whatever is held if its deadline has passed.
    pub(crate) fn flush_if_due(&mut self, now: Instant) -> Vec<PasteEvent> {
        match self.deadline {
            Some(deadline) if now >= deadline => {
                let events = self.recognizer.on_timeout();
                self.set_deadline(now);
                events
            }
            _ => Vec::new(),
        }
    }

    fn set_deadline(&mut self, now: Instant) {
        self.deadline = match self.recognizer.pending() {
            Pending::Nothing => None,
            Pending::PartialMarker => Some(now + PARTIAL_MARKER_WAIT),
            Pending::Paste => Some(now + PASTE_STALL_WAIT),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn nothing_held_means_no_timeout() {
        let mut input = PatchInput::new(true);
        let now = Instant::now();
        assert_eq!(input.poll_timeout_ms(now), None);
        assert_eq!(forwarded(&input.feed(b"ls\n", true, now)), b"ls\n");
        assert_eq!(input.poll_timeout_ms(now), None);
        assert!(input.flush_if_due(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn a_lone_escape_is_released_after_the_short_wait() {
        let mut input = PatchInput::new(true);
        let t0 = Instant::now();
        assert!(input.feed(b"\x1b", true, t0).is_empty());
        assert_eq!(input.poll_timeout_ms(t0), Some(30));
        assert_eq!(input.poll_timeout_ms(t0 + Duration::from_millis(10)), Some(20));
        // Too early: nothing happens.
        assert!(input.flush_if_due(t0 + Duration::from_millis(29)).is_empty());
        let events = input.flush_if_due(t0 + Duration::from_millis(30));
        assert_eq!(forwarded(&events), b"\x1b");
        assert_eq!(input.poll_timeout_ms(t0 + Duration::from_millis(30)), None);
    }

    #[test]
    fn a_stalled_patch_paste_is_released_after_the_long_wait() {
        let mut input = PatchInput::new(true);
        let t0 = Instant::now();
        let head = b"\x1b[200~*** Begin Patch\n*** Update File: a\n";
        assert!(input.feed(head, true, t0).is_empty());
        assert_eq!(input.poll_timeout_ms(t0), Some(2000));
        // More bytes arriving push the deadline out.
        let t1 = t0 + Duration::from_millis(1500);
        assert!(input.feed(b"@@\n", true, t1).is_empty());
        assert_eq!(input.poll_timeout_ms(t1), Some(2000));
        assert!(input.flush_if_due(t0 + Duration::from_millis(2100)).is_empty());
        let events = input.flush_if_due(t1 + PASTE_STALL_WAIT);
        let mut expected = head.to_vec();
        expected.extend_from_slice(b"@@\n");
        assert_eq!(forwarded(&events), expected);
        assert_eq!(input.poll_timeout_ms(t1 + PASTE_STALL_WAIT), None);
    }

    #[test]
    fn a_complete_patch_leaves_no_deadline() {
        let mut input = PatchInput::new(true);
        let now = Instant::now();
        let paste = b"\x1b[200~*** Begin Patch\n*** Delete File: a\n*** End Patch\x1b[201~";
        let events = input.feed(paste, true, now);
        assert!(matches!(events.as_slice(), [PasteEvent::Patch(_)]));
        assert_eq!(input.poll_timeout_ms(now), None);
    }

    #[test]
    fn a_disabled_input_forwards_everything_including_patches() {
        let mut input = PatchInput::new(false);
        let now = Instant::now();
        let paste = b"\x1b[200~*** Begin Patch\n*** Delete File: a\n*** End Patch\x1b[201~";
        assert_eq!(forwarded(&input.feed(paste, true, now)), paste.to_vec());
        assert_eq!(forwarded(&input.feed(b"\x1b", true, now)), b"\x1b");
        assert_eq!(input.poll_timeout_ms(now), None);
    }

    #[test]
    fn long_waits_are_clamped_to_what_poll_accepts() {
        let mut input = PatchInput::new(true);
        let now = Instant::now();
        input.deadline = Some(now + Duration::from_secs(3600));
        assert_eq!(input.poll_timeout_ms(now), Some(u16::MAX));
        // A fraction of a millisecond rounds up instead of becoming a busy loop.
        input.deadline = Some(now + Duration::from_micros(100));
        assert_eq!(input.poll_timeout_ms(now), Some(1));
    }
}

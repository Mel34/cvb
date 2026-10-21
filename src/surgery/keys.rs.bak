#![allow(dead_code)]

use std::io::Read;
use crate::surgery::parse::MAX_INPUT_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Paste(String),
    PasteKey,
    Build,
    Abort,
    Space,
    Left,
    Right,
    Other,
    PasteError(String),
    Eof,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}


use std::time::{Duration, Instant};

pub(crate) const ARM_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArmingState {
    Idle,
    Candidate,
    Armed(Instant),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArmingAction {
    NothingToBuild,
    Armed,
    FireBuild,
    DisarmedAndHandle(Event),
    Abort,
    Handle(Event),
}

impl ArmingState {
    pub(crate) fn is_armed(&self, now: Instant) -> bool {
        match self {
            ArmingState::Armed(armed_at) => now.saturating_duration_since(*armed_at) <= ARM_TIMEOUT,
            _ => false,
        }
    }

    pub(crate) fn step(&mut self, event: Event, now: Instant) -> ArmingAction {
        match *self {
            ArmingState::Idle => match event {
                Event::Build => ArmingAction::NothingToBuild,
                Event::Abort => ArmingAction::Abort,
                other => ArmingAction::Handle(other),
            },
            ArmingState::Candidate => match event {
                Event::Build => {
                    *self = ArmingState::Armed(now);
                    ArmingAction::Armed
                }
                Event::Abort => {
                    *self = ArmingState::Idle;
                    ArmingAction::Abort
                }
                other => ArmingAction::Handle(other),
            },
            ArmingState::Armed(armed_at) => {
                let timed_out = now.saturating_duration_since(armed_at) > ARM_TIMEOUT;
                if timed_out {
                    *self = ArmingState::Candidate;
                    match event {
                        Event::Build => {
                            *self = ArmingState::Armed(now);
                            ArmingAction::Armed
                        }
                        Event::Abort => {
                            *self = ArmingState::Idle;
                            ArmingAction::Abort
                        }
                        other => ArmingAction::DisarmedAndHandle(other),
                    }
                } else {
                    match event {
                        Event::Build => {
                            *self = ArmingState::Candidate;
                            ArmingAction::FireBuild
                        }
                        Event::Abort => {
                            *self = ArmingState::Idle;
                            ArmingAction::Abort
                        }
                        other => {
                            *self = ArmingState::Candidate;
                            ArmingAction::DisarmedAndHandle(other)
                        }
                    }
                }
            }
        }
    }
}

pub(crate) struct EventReader<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: Read> EventReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
        }
    }

    pub(crate) fn next_event(&mut self) -> Result<Event, std::io::Error> {
        let mut chunk = [0u8; 4096];
        loop {
            if self.buf.is_empty() {
                let n = self.reader.read(&mut chunk)?;
                if n == 0 {
                    return Ok(Event::Eof);
                }
                self.buf.extend_from_slice(&chunk[..n]);
            }

            let first = self.buf[0];
            match first {
                0x02 => {
                    self.buf.remove(0);
                    return Ok(Event::Build);
                }
                0x16 => {
                    self.buf.remove(0);
                    return Ok(Event::PasteKey);
                }
                0x03 | b'q' => {
                    self.buf.remove(0);
                    return Ok(Event::Abort);
                }
                b' ' => {
                    self.buf.remove(0);
                    return Ok(Event::Space);
                }
                0x1b => {
                    const START_MARKER: &[u8] = b"\x1b[200~";

                    while self.buf.len() < START_MARKER.len() {
                        let n = self.reader.read(&mut chunk)?;
                        if n == 0 {
                            break;
                        }
                        self.buf.extend_from_slice(&chunk[..n]);
                    }

                    if self.buf.starts_with(START_MARKER) {
                        self.buf.drain(..START_MARKER.len());
                        return self.read_bracketed_paste(&mut chunk);
                    }

                    if self.buf.starts_with(b"\x1b[D") || self.buf.starts_with(b"\x1bOD") {
                        self.buf.drain(..3);
                        return Ok(Event::Left);
                    }

                    if self.buf.starts_with(b"\x1b[C") || self.buf.starts_with(b"\x1bOC") {
                        self.buf.drain(..3);
                        return Ok(Event::Right);
                    }

                    if self.buf.len() >= 2 && (self.buf[1] == b'[' || self.buf[1] == b'O') {
                        if self.buf.len() >= 3 {
                            self.buf.drain(..3);
                        } else {
                            self.buf.drain(..self.buf.len());
                        }
                        return Ok(Event::Other);
                    }

                    self.buf.remove(0);
                    return Ok(Event::Other);
                }
                _ => {
                    self.buf.remove(0);
                    return Ok(Event::Other);
                }
            }
        }
    }

    fn read_bracketed_paste(&mut self, chunk: &mut [u8; 4096]) -> Result<Event, std::io::Error> {
        const END_MARKER: &[u8] = b"\x1b[201~";
        let mut payload = Vec::new();
        let mut exceeded = false;

        loop {
            if let Some(pos) = find_subslice(&self.buf, END_MARKER) {
                let to_take = pos;
                if payload.len().saturating_add(to_take) > MAX_INPUT_BYTES {
                    exceeded = true;
                } else {
                    payload.extend_from_slice(&self.buf[..to_take]);
                }
                self.buf.drain(..pos + END_MARKER.len());
                if exceeded {
                    return Ok(Event::PasteError(format!(
                        "bracketed paste payload exceeds 3 MiB cap ({MAX_INPUT_BYTES} bytes)"
                    )));
                }
                return match String::from_utf8(payload) {
                    Ok(text) => Ok(Event::Paste(text)),
                    Err(_) => Ok(Event::PasteError("bracketed paste is not valid UTF-8".to_string())),
                };
            }

            if self.buf.len() >= END_MARKER.len() {
                let safe_len = self.buf.len() - END_MARKER.len() + 1;
                if payload.len().saturating_add(safe_len) > MAX_INPUT_BYTES {
                    exceeded = true;
                } else {
                    payload.extend_from_slice(&self.buf[..safe_len]);
                }
                self.buf.drain(..safe_len);
            }

            let n = self.reader.read(chunk)?;
            if n == 0 {
                self.buf.clear();
                return Ok(Event::PasteError(
                    "unterminated bracketed paste: stream ended before closing marker".to_string(),
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn pasted_envelope_with_keys_never_produces_key_events() {
        let pasted_text = "q a x \x02 \x16 \x03 \t \n \u{1f600} \u{00e9}";
        let raw = format!("\x1b[200~{pasted_text}\x1b[201~");
        let mut reader = EventReader::new(Cursor::new(raw.into_bytes()));

        let event = reader.next_event().unwrap();
        assert_eq!(event, Event::Paste(pasted_text.to_string()));
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn bracketed_paste_3_mib_succeeds() {
        let payload = vec![b'x'; MAX_INPUT_BYTES];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"\x1b[200~");
        raw.extend_from_slice(&payload);
        raw.extend_from_slice(b"\x1b[201~");

        let mut reader = EventReader::new(Cursor::new(raw));
        let event = reader.next_event().unwrap();
        match event {
            Event::Paste(s) => assert_eq!(s.len(), MAX_INPUT_BYTES),
            other => panic!("expected Event::Paste, got {other:?}"),
        }
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn bracketed_paste_over_cap_returns_error_event_without_hang() {
        let payload = vec![b'x'; MAX_INPUT_BYTES + 1];
        let mut raw = Vec::new();
        raw.extend_from_slice(b"\x1b[200~");
        raw.extend_from_slice(&payload);
        raw.extend_from_slice(b"\x1b[201~");

        let mut reader = EventReader::new(Cursor::new(raw));
        let event = reader.next_event().unwrap();
        match event {
            Event::PasteError(msg) => assert!(msg.contains("exceeds 3 MiB cap")),
            other => panic!("expected Event::PasteError, got {other:?}"),
        }
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn paste_split_across_reads() {
        struct OneByteReader<'a>(&'a [u8]);
        impl<'a> Read for OneByteReader<'a> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.0[0];
                self.0 = &self.0[1..];
                Ok(1)
            }
        }

        let raw = b"\x1b[200~hello world\x1b[201~";
        let mut reader = EventReader::new(OneByteReader(raw));
        let event = reader.next_event().unwrap();
        assert_eq!(event, Event::Paste("hello world".to_string()));
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn unterminated_paste_returns_error_event() {
        let raw = b"\x1b[200~unterminated paste payload without end";
        let mut reader = EventReader::new(Cursor::new(raw.to_vec()));
        let event = reader.next_event().unwrap();
        match event {
            Event::PasteError(msg) => assert!(msg.contains("unterminated bracketed paste")),
            other => panic!("expected Event::PasteError, got {other:?}"),
        }
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn individual_key_events_decode_accurately() {
        let stream = vec![
            0x02,                 // Build (Ctrl+B)
            0x16,                 // PasteKey (Ctrl+V)
            0x03,                 // Abort (Ctrl+C)
            b'q',                 // Abort (q)
            b' ',                 // Space
            0x1b, b'[', b'D',     // Left
            0x1b, b'O', b'D',     // Left
            0x1b, b'[', b'C',     // Right
            0x1b, b'O', b'C',     // Right
            b'z',                 // Other
        ];

        let mut reader = EventReader::new(Cursor::new(stream));
        assert_eq!(reader.next_event().unwrap(), Event::Build);
        assert_eq!(reader.next_event().unwrap(), Event::PasteKey);
        assert_eq!(reader.next_event().unwrap(), Event::Abort);
        assert_eq!(reader.next_event().unwrap(), Event::Abort);
        assert_eq!(reader.next_event().unwrap(), Event::Space);
        assert_eq!(reader.next_event().unwrap(), Event::Left);
        assert_eq!(reader.next_event().unwrap(), Event::Left);
        assert_eq!(reader.next_event().unwrap(), Event::Right);
        assert_eq!(reader.next_event().unwrap(), Event::Right);
        assert_eq!(reader.next_event().unwrap(), Event::Other);
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn ctrl_b_while_idle_reports_nothing_to_build() {
        let mut state = ArmingState::Idle;
        let now = Instant::now();
        assert_eq!(state.step(Event::Build, now), ArmingAction::NothingToBuild);
        assert_eq!(state, ArmingState::Idle);
    }

    #[test]
    fn first_build_in_candidate_arms_second_within_3s_fires() {
        let mut state = ArmingState::Candidate;
        let t0 = Instant::now();

        // First press arms
        assert_eq!(state.step(Event::Build, t0), ArmingAction::Armed);
        assert_eq!(state, ArmingState::Armed(t0));
        assert!(state.is_armed(t0 + Duration::from_secs(1)));

        // Second press at 2s fires
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(state.step(Event::Build, t1), ArmingAction::FireBuild);
        assert_eq!(state, ArmingState::Candidate);
    }

    #[test]
    fn arm_then_timeout_disarms() {
        let mut state = ArmingState::Candidate;
        let t0 = Instant::now();

        assert_eq!(state.step(Event::Build, t0), ArmingAction::Armed);

        // At 3.01s, is_armed is false
        let t_expired = t0 + Duration::from_millis(3001);
        assert!(!state.is_armed(t_expired));

        // Build after timeout acts as first press (re-arms)
        assert_eq!(state.step(Event::Build, t_expired), ArmingAction::Armed);
        assert_eq!(state, ArmingState::Armed(t_expired));
    }

    #[test]
    fn arm_then_paste_disarms_and_handles_paste() {
        let mut state = ArmingState::Candidate;
        let t0 = Instant::now();

        assert_eq!(state.step(Event::Build, t0), ArmingAction::Armed);

        let paste_event = Event::Paste("new-envelope".into());
        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(
            state.step(paste_event.clone(), t1),
            ArmingAction::DisarmedAndHandle(paste_event)
        );
        assert_eq!(state, ArmingState::Candidate);
    }

    #[test]
    fn arm_then_other_key_disarms_and_handles_key() {
        let mut state = ArmingState::Candidate;
        let t0 = Instant::now();

        assert_eq!(state.step(Event::Build, t0), ArmingAction::Armed);

        let t1 = t0 + Duration::from_secs(1);
        assert_eq!(
            state.step(Event::Space, t1),
            ArmingAction::DisarmedAndHandle(Event::Space)
        );
        assert_eq!(state, ArmingState::Candidate);
    }

    #[test]
    fn abort_resets_each_state_to_idle() {
        let now = Instant::now();

        let mut idle = ArmingState::Idle;
        assert_eq!(idle.step(Event::Abort, now), ArmingAction::Abort);
        assert_eq!(idle, ArmingState::Idle);

        let mut cand = ArmingState::Candidate;
        assert_eq!(cand.step(Event::Abort, now), ArmingAction::Abort);
        assert_eq!(cand, ArmingState::Idle);

        let mut armed = ArmingState::Armed(now);
        assert_eq!(armed.step(Event::Abort, now), ArmingAction::Abort);
        assert_eq!(armed, ArmingState::Idle);
    }
}

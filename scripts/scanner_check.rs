#![allow(dead_code)]
// Scratch check: baseline MarkerScanner (verbatim logic from pty.rs) vs a
// three-marker scanner, plus the proposed capture semantics (PATHOP => capture.end()).

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerEvent {
    Data(Vec<u8>),
    Start,
    PathOp,
    End,
}

// ---------- baseline: current scanner logic, two markers ----------
struct OldScanner {
    start: Vec<u8>,
    end: Vec<u8>,
    pending: Vec<u8>,
}

impl OldScanner {
    fn new() -> Self {
        Self {
            start: b"\x1b]777;CVB;START\x07".to_vec(),
            end: b"\x1b]777;CVB;END\x07".to_vec(),
            pending: Vec::new(),
        }
    }

    fn process(&mut self, data: &[u8]) -> Vec<MarkerEvent> {
        self.pending.extend_from_slice(data);
        let mut events = Vec::new();
        loop {
            let start_pos = find_subslice(&self.pending, &self.start);
            let end_pos = find_subslice(&self.pending, &self.end);
            let marker = match (start_pos, end_pos) {
                (Some(start), Some(end)) if start <= end => Some((start, self.start.len(), true)),
                (Some(_), Some(end)) => Some((end, self.end.len(), false)),
                (Some(start), None) => Some((start, self.start.len(), true)),
                (None, Some(end)) => Some((end, self.end.len(), false)),
                (None, None) => break,
            };
            let Some((pos, len, is_start)) = marker else { break };
            if pos > 0 {
                events.push(MarkerEvent::Data(self.pending.drain(..pos).collect()));
            }
            self.pending.drain(..len);
            events.push(if is_start { MarkerEvent::Start } else { MarkerEvent::End });
        }
        if !self.pending.is_empty() {
            let keep = self.start.len().max(self.end.len()).saturating_sub(1);
            if self.pending.len() > keep {
                let split_at = self.pending.len() - keep;
                events.push(MarkerEvent::Data(self.pending.drain(..split_at).collect()));
            }
        }
        events
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

// ---------- proposed: table-driven scanner with PATHOP ----------
struct Scanner {
    markers: Vec<(Vec<u8>, MarkerEvent)>,
    pending: Vec<u8>,
    keep: usize,
}

impl Scanner {
    fn new() -> Self {
        let markers = vec![
            (b"\x1b]777;CVB;START\x07".to_vec(), MarkerEvent::Start),
            (b"\x1b]777;CVB;END\x07".to_vec(), MarkerEvent::End),
            (b"\x1b]777;CVB;PATHOP\x07".to_vec(), MarkerEvent::PathOp),
        ];
        let keep = markers.iter().map(|(m, _)| m.len()).max().unwrap() - 1;
        Self { markers, pending: Vec::new(), keep }
    }

    fn process(&mut self, data: &[u8]) -> Vec<MarkerEvent> {
        self.pending.extend_from_slice(data);
        let mut events = Vec::new();
        loop {
            let mut best: Option<(usize, usize, MarkerEvent)> = None;
            for (bytes, event) in &self.markers {
                if let Some(pos) = find_subslice(&self.pending, bytes) {
                    if best.as_ref().map_or(true, |(p, _, _)| pos < *p) {
                        best = Some((pos, bytes.len(), event.clone()));
                    }
                }
            }
            let Some((pos, len, event)) = best else { break };
            if pos > 0 {
                events.push(MarkerEvent::Data(self.pending.drain(..pos).collect()));
            }
            self.pending.drain(..len);
            events.push(event);
        }
        if self.pending.len() > self.keep {
            let split_at = self.pending.len() - self.keep;
            events.push(MarkerEvent::Data(self.pending.drain(..split_at).collect()));
        }
        events
    }

    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

// ---------- CaptureState: verbatim from pty.rs ----------
#[derive(Default)]
struct CaptureState {
    capturing: bool,
    captured: Vec<u8>,
}

impl CaptureState {
    fn start(&mut self) {
        if !self.capturing {
            self.capturing = true;
            self.captured.clear();
        }
    }
    fn push(&mut self, data: &[u8]) {
        if self.capturing {
            self.captured.extend_from_slice(data);
        }
    }
    fn end(&mut self) -> Vec<u8> {
        self.capturing = false;
        std::mem::take(&mut self.captured)
    }
}

const S: &[u8] = b"\x1b]777;CVB;START\x07";
const E: &[u8] = b"\x1b]777;CVB;END\x07";
const P: &[u8] = b"\x1b]777;CVB;PATHOP\x07";

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

fn coalesce(events: Vec<MarkerEvent>) -> Vec<MarkerEvent> {
    let mut out: Vec<MarkerEvent> = Vec::new();
    for ev in events {
        match (out.last_mut(), ev) {
            (Some(MarkerEvent::Data(prev)), MarkerEvent::Data(next)) => prev.extend(next),
            (_, ev) => out.push(ev),
        }
    }
    out
}

fn scan(stream: &[u8], chunk: usize) -> Vec<MarkerEvent> {
    let mut s = Scanner::new();
    let mut events = Vec::new();
    for c in stream.chunks(chunk) {
        events.extend(s.process(c));
    }
    let tail = s.finish();
    if !tail.is_empty() {
        events.push(MarkerEvent::Data(tail));
    }
    coalesce(events)
}

// The proposed proxy semantics: PATHOP => capture.end(); END dispatches only if capturing.
fn dispatches(events: &[MarkerEvent]) -> Vec<Vec<u8>> {
    let mut cap = CaptureState::default();
    let mut out = Vec::new();
    for ev in events {
        match ev {
            MarkerEvent::Data(d) => cap.push(d),
            MarkerEvent::Start => cap.start(),
            MarkerEvent::PathOp => {
                cap.end();
            }
            MarkerEvent::End => {
                if !cap.capturing {
                    continue;
                }
                out.push(cap.end());
            }
        }
    }
    out
}

fn replay(stream: &[u8]) -> Vec<Vec<u8>> {
    let whole = dispatches(&scan(stream, stream.len().max(1)));
    // chunk invariance: 1-byte, 3-byte and 7-byte reads must give the same dispatches
    for chunk in [1usize, 3, 7, 16, 17] {
        assert_eq!(dispatches(&scan(stream, chunk)), whole, "chunk size {chunk}");
    }
    whole
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_lengths_and_holdback() {
        assert_eq!(S.len(), 16);
        assert_eq!(E.len(), 14);
        assert_eq!(P.len(), 17);
        assert_eq!(Scanner::new().keep, 16);

        // baseline keeps max(START, END) - 1 = 15 and would split a 17-byte PATHOP if it were added
        let mut old = OldScanner::new();
        old.process(&vec![b'x'; 100]);
        assert_eq!(old.pending.len(), 15);

        let mut new = Scanner::new();
        new.process(&vec![b'x'; 100]);
        assert_eq!(new.pending.len(), 16);
    }

    #[test]
    fn stale_holdback_would_miss_a_split_pathop() {
        // If PATHOP were added without raising the holdback from 15 to 16,
        // a marker split after its 16th byte would be cut in the middle.
        let mut s = Scanner::new();
        s.keep = 15;
        let mut events = s.process(&cat(&[b"abc", &P[..16]]));
        events.extend(s.process(&P[16..]));
        let events = coalesce(events);
        assert!(!events.contains(&MarkerEvent::PathOp), "expected the 15-byte holdback to lose it");
    }

    #[test]
    fn every_split_point_is_recognized() {
        let stream = cat(&[
            b"echo\r\n", S, b"\x1b[31mred\x1b[0m\x1b", b"data", P, b"client\r\n", E, b"$ ", S, b"hi\r\n", E,
        ]);
        let reference = scan(&stream, stream.len());
        for i in 0..=stream.len() {
            let mut s = Scanner::new();
            let mut events = s.process(&stream[..i]);
            events.extend(s.process(&stream[i..]));
            let tail = s.finish();
            if !tail.is_empty() {
                events.push(MarkerEvent::Data(tail));
            }
            assert_eq!(coalesce(events), reference, "split at {i}");
        }
        assert_eq!(scan(&stream, 1), reference);
    }

    #[test]
    fn pathop_discards_active_capture() {
        assert!(replay(&cat(&[S, b"abc", P, b"client msg\r\n", E])).is_empty());
    }

    #[test]
    fn pathop_without_capture_is_a_noop() {
        let out = replay(&cat(&[P, S, b"x\r\n", E]));
        assert_eq!(out, vec![b"x\r\n".to_vec()]);
    }

    #[test]
    fn bracketed_paste_block_dispatches_only_the_last_command() {
        // observed: <S><P>client-out:a<S><P>client-out:b<S>hi<E>
        let out = replay(&cat(&[
            b"cvb a\r\ncvb b\r\necho hi\r\n",
            S, P, b"client-out:a\r\n",
            S, P, b"client-out:b\r\n",
            S, b"hi\r\n",
            E,
        ]));
        assert_eq!(out, vec![b"hi\r\n".to_vec()]);
    }

    #[test]
    fn bracketed_paste_block_of_plain_commands_accumulates() {
        let out = replay(&cat(&[S, b"1\r\n", S, b"2\r\n", E]));
        assert_eq!(out, vec![b"1\r\n2\r\n".to_vec()]);
    }

    #[test]
    fn one_line_with_two_pathops_and_one_start() {
        // observed: <S><P>a<P>b hi <E>  -> clipboard keeps the file contents, nothing dispatched
        assert!(replay(&cat(&[S, P, b"client-out:a\r\n", P, b"client-out:b\r\n", b"hi\r\n", E])).is_empty());
    }

    #[test]
    fn paste_off_per_line_prompts() {
        let out = replay(&cat(&[
            S, P, b"client-out:a\r\n", E, b"$ ",
            S, P, b"client-out:b\r\n", E, b"$ ",
            S, b"hi\r\n", E,
        ]));
        assert_eq!(out, vec![b"hi\r\n".to_vec()]);
    }

    #[test]
    fn block_ending_in_a_path_capture_dispatches_nothing() {
        assert!(replay(&cat(&[S, b"hi\r\n", S, P, b"client-out:a\r\n", E])).is_empty());
    }

    #[test]
    fn background_pathop_after_end_is_harmless() {
        // observed: <S>[1] 310<E>$ <S>next<E>$ <P>
        let out = replay(&cat(&[
            S, b"[1] 310\r\n", E, b"$ ", S, b"next\r\n", E, b"$ ", P, b"client-out:a\r\n", S, b"later\r\n", E,
        ]));
        assert_eq!(out, vec![b"[1] 310\r\n".to_vec(), b"next\r\n".to_vec(), b"later\r\n".to_vec()]);
    }

    #[test]
    fn background_pathop_mid_command_loses_only_that_command() {
        let out = replay(&cat(&[S, b"[1] 310\r\n", E, S, b"next\r\n", P, b"more\r\n", E, S, b"after\r\n", E]));
        assert_eq!(out, vec![b"[1] 310\r\n".to_vec(), b"after\r\n".to_vec()]);
    }

    #[test]
    fn finish_tail_is_a_duplicate_of_bytes_already_written_to_stdout() {
        // proxy(): stdout.write_all(output) for every chunk, THEN scanner.process(output);
        // after the loop it writes scanner.finish() to stdout again.
        let chunks: Vec<&[u8]> = vec![b"$ ", b"exit\r\n"];
        let mut old = OldScanner::new();
        let mut stdout: Vec<u8> = Vec::new();
        for c in &chunks {
            stdout.extend_from_slice(c);
            old.process(c);
        }
        let tail = old.finish();
        assert!(!tail.is_empty());
        let before = stdout.clone();
        stdout.extend_from_slice(&tail);
        println!("stdout before finish(): {:?}", String::from_utf8_lossy(&before));
        println!("duplicated by finish():  {:?}", String::from_utf8_lossy(&tail));
        assert!(before.ends_with(&tail));
    }

    #[test]
    fn capture_end_is_idempotent_when_idle() {
        let mut c = CaptureState::default();
        assert_eq!(c.end(), b"");
        assert_eq!(c.end(), b"");
        assert!(!c.capturing);
    }
}

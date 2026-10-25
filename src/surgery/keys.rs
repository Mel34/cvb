#![allow(dead_code)]

use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nix::sys::signal::{signal, SigHandler, Signal};
use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg, SpecialCharacterIndices, Termios};

use crate::surgery::parse::MAX_INPUT_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    Paste(String),
    PasteKey,
    Build,
    Abort,
    Space,
    Enter,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    ChooseCandidate(usize),
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
    terminal_fd: Option<RawFd>,
}

impl<R: Read> EventReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            terminal_fd: None,
        }
    }

    pub(crate) fn new_terminal(reader: R, terminal_fd: RawFd) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            terminal_fd: Some(terminal_fd),
        }
    }

    fn read_escape_continuation(
        &mut self,
        chunk: &mut [u8],
    ) -> Result<usize, std::io::Error> {
        let Some(fd) = self.terminal_fd else {
            return self.reader.read(chunk);
        };

        loop {
            let mut descriptor = nix::libc::pollfd {
                fd,
                events: nix::libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { nix::libc::poll(&mut descriptor, 1, 50) };
            if result > 0 {
                return self.reader.read(chunk);
            }
            if result == 0 {
                return Ok(0);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
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
                        if self.buf.starts_with(b"\x1b[D")
                            || self.buf.starts_with(b"\x1bOD")
                            || self.buf.starts_with(b"\x1b[C")
                            || self.buf.starts_with(b"\x1bOC")
                        {
                            break;
                        }
                        if self.buf.len() >= 3
                            && (self.buf[1] == b'[' || self.buf[1] == b'O')
                            && !START_MARKER.starts_with(&self.buf)
                            && self.buf.last().is_some_and(|byte| (0x40..=0x7e).contains(byte))
                        {
                            break;
                        }

                        let n = self.read_escape_continuation(&mut chunk)?;
                        if n == 0 {
                            break;
                        }
                        self.buf.extend_from_slice(&chunk[..n]);
                    }

                    if self.buf.starts_with(START_MARKER) {
                        self.buf.drain(..START_MARKER.len());
                        return self.read_bracketed_paste(&mut chunk);
                    }

                    let escape_keys: &[(&[u8], Event)] = &[
                        (b"\x1b[D", Event::Left),
                        (b"\x1bOD", Event::Left),
                        (b"\x1b[C", Event::Right),
                        (b"\x1bOC", Event::Right),
                        (b"\x1b[A", Event::Up),
                        (b"\x1bOA", Event::Up),
                        (b"\x1b[B", Event::Down),
                        (b"\x1bOB", Event::Down),
                        (b"\x1b[H", Event::Home),
                        (b"\x1bOH", Event::Home),
                        (b"\x1b[1~", Event::Home),
                        (b"\x1b[7~", Event::Home),
                        (b"\x1b[F", Event::End),
                        (b"\x1bOF", Event::End),
                        (b"\x1b[4~", Event::End),
                        (b"\x1b[8~", Event::End),
                        (b"\x1b[5~", Event::PageUp),
                        (b"\x1b[6~", Event::PageDown),
                    ];

                    for (sequence, event) in escape_keys {
                        if self.buf.starts_with(sequence) {
                            self.buf.drain(..sequence.len());
                            return Ok(event.clone());
                        }
                    }

                    if self.buf.len() == 1 {
                        self.buf.remove(0);
                        return Ok(Event::Abort);
                    }

                    if START_MARKER.starts_with(&self.buf) {
                        self.buf.clear();
                        return Ok(Event::Other);
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
                    return Ok(Event::Abort);
                }
                0x0d | 0x0a => {
                    self.buf.remove(0);
                    return Ok(Event::Enter);
                }
                b'1'..=b'5' => {
                    self.buf.remove(0);
                    return Ok(Event::ChooseCandidate((first - b'1') as usize));
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

static GUARD_MUTEX: Mutex<()> = Mutex::new(());
static ACTIVE_GUARD: AtomicBool = AtomicBool::new(false);
static GUARD_FD: AtomicI32 = AtomicI32::new(-1);
static mut SAVED_TERMIOS: nix::libc::termios = unsafe { std::mem::zeroed() };

extern "C" fn surgery_signal_handler(sig: nix::libc::c_int) {
    if ACTIVE_GUARD.swap(false, Ordering::SeqCst) {
        let fd = GUARD_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            unsafe {
                let _ = nix::libc::write(
                    fd,
                    b"\x1b[?2004l".as_ptr() as *const nix::libc::c_void,
                    8,
                );
                let ptr = std::ptr::addr_of!(SAVED_TERMIOS);
                let _ = nix::libc::tcsetattr(
                    fd,
                    nix::libc::TCSADRAIN,
                    ptr,
                );
            }
        }
    }

    unsafe {
        nix::libc::signal(sig, nix::libc::SIG_DFL);
        nix::libc::raise(sig);
    }
}

pub(crate) struct SurgeryTerminalGuard {
    raw_fd: RawFd,
    orig_termios: Termios,
    prev_sigterm: SigHandler,
    prev_sighup: SigHandler,
    _file: Option<std::fs::File>,
    _lock: Option<std::sync::MutexGuard<'static, ()>>,
}

impl SurgeryTerminalGuard {
    pub(crate) fn enter() -> Result<Self, (String, i32)> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| {
                (
                    "E-NOTTY: interactive terminal confirmation requires /dev/tty".to_string(),
                    2,
                )
            })?;
        let mut guard = Self::enter_on(&file)?;
        guard._file = Some(file);
        Ok(guard)
    }

    pub(crate) fn enter_on<F: AsFd + AsRawFd>(fd: &F) -> Result<Self, (String, i32)> {
        let lock = match GUARD_MUTEX.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        let orig = tcgetattr(fd.as_fd()).map_err(|_| {
            (
                "E-NOTTY: interactive terminal confirmation requires /dev/tty".to_string(),
                2,
            )
        })?;

        let mut raw = orig.clone();
        raw.local_flags.remove(LocalFlags::ICANON);
        raw.local_flags.remove(LocalFlags::ECHO);
        raw.local_flags.remove(LocalFlags::ISIG);
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;

        tcsetattr(fd.as_fd(), SetArg::TCSADRAIN, &raw).map_err(|e| {
            (
                format!("CVB: failed to configure terminal raw mode: {e}"),
                2,
            )
        })?;

        let raw_fd = fd.as_raw_fd();
        let prev_term;
        let prev_hup;
        unsafe {
            let _ = nix::libc::write(
                raw_fd,
                b"\x1b[?2004h".as_ptr() as *const nix::libc::c_void,
                8,
            );
            let c_termios: nix::libc::termios = orig.clone().into();
            let ptr = std::ptr::addr_of_mut!(SAVED_TERMIOS);
            *ptr = c_termios;
            GUARD_FD.store(raw_fd, Ordering::SeqCst);
            ACTIVE_GUARD.store(true, Ordering::SeqCst);

            prev_term = signal(Signal::SIGTERM, SigHandler::Handler(surgery_signal_handler))
                .unwrap_or(SigHandler::SigDfl);
            prev_hup = signal(Signal::SIGHUP, SigHandler::Handler(surgery_signal_handler))
                .unwrap_or(SigHandler::SigDfl);
        }

        Ok(Self {
            raw_fd,
            orig_termios: orig,
            prev_sigterm: prev_term,
            prev_sighup: prev_hup,
            _file: None,
            _lock: Some(lock),
        })
    }

    pub(crate) fn file(&self) -> Option<&std::fs::File> {
        self._file.as_ref()
    }

    pub(crate) fn raw_fd(&self) -> RawFd {
        self.raw_fd
    }
}

impl Drop for SurgeryTerminalGuard {
    fn drop(&mut self) {
        if ACTIVE_GUARD.swap(false, Ordering::SeqCst) {
            let fd = self.raw_fd;
            unsafe {
                let _ = nix::libc::write(
                    fd,
                    b"\x1b[?2004l".as_ptr() as *const nix::libc::c_void,
                    8,
                );
                let borrowed = BorrowedFd::borrow_raw(fd);
                let _ = tcsetattr(borrowed, SetArg::TCSADRAIN, &self.orig_termios);
                let _ = signal(Signal::SIGTERM, self.prev_sigterm);
                let _ = signal(Signal::SIGHUP, self.prev_sighup);
            }
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
    fn page_navigation_sequences_split_across_reads() {
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

        let raw = b"\x1b[5~\x1b[6~";
        let mut reader = EventReader::new(OneByteReader(raw));
        assert_eq!(reader.next_event().unwrap(), Event::PageUp);
        assert_eq!(reader.next_event().unwrap(), Event::PageDown);
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
    fn lone_escape_aborts() {
        let mut reader = EventReader::new(Cursor::new(b"\x1b".to_vec()));
        assert_eq!(reader.next_event().unwrap(), Event::Abort);
        assert_eq!(reader.next_event().unwrap(), Event::Eof);
    }

    #[test]
    fn incomplete_escape_sequence_is_ignored() {
        let mut reader = EventReader::new(Cursor::new(b"\x1b[".to_vec()));
        assert_eq!(reader.next_event().unwrap(), Event::Other);
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
            b'\r',                // Enter (CR)
            b'\n',                // Enter (LF)
            0x1b, b'[', b'D',     // Left
            0x1b, b'O', b'D',     // Left
            0x1b, b'[', b'C',     // Right
            0x1b, b'O', b'C',     // Right
            0x1b, b'[', b'A',     // Up
            0x1b, b'O', b'A',     // Up
            0x1b, b'[', b'B',     // Down
            0x1b, b'O', b'B',     // Down
            0x1b, b'[', b'H',     // Home
            0x1b, b'O', b'H',     // Home
            0x1b, b'[', b'1', b'~', // Home
            0x1b, b'[', b'7', b'~', // Home
            0x1b, b'[', b'F',     // End
            0x1b, b'O', b'F',     // End
            0x1b, b'[', b'4', b'~', // End
            0x1b, b'[', b'8', b'~', // End
            0x1b, b'[', b'5', b'~', // PageUp
            0x1b, b'[', b'6', b'~', // PageDown
            b'1',                 // Candidate 1
            b'2',                 // Candidate 2
            b'3',                 // Candidate 3
            b'4',                 // Candidate 4
            b'5',                 // Candidate 5
            b'z',                 // Other
        ];

        let mut reader = EventReader::new(Cursor::new(stream));
        assert_eq!(reader.next_event().unwrap(), Event::Build);
        assert_eq!(reader.next_event().unwrap(), Event::PasteKey);
        assert_eq!(reader.next_event().unwrap(), Event::Abort);
        assert_eq!(reader.next_event().unwrap(), Event::Abort);
        assert_eq!(reader.next_event().unwrap(), Event::Space);
        assert_eq!(reader.next_event().unwrap(), Event::Enter);
        assert_eq!(reader.next_event().unwrap(), Event::Enter);
        assert_eq!(reader.next_event().unwrap(), Event::Left);
        assert_eq!(reader.next_event().unwrap(), Event::Left);
        assert_eq!(reader.next_event().unwrap(), Event::Right);
        assert_eq!(reader.next_event().unwrap(), Event::Right);
        assert_eq!(reader.next_event().unwrap(), Event::Up);
        assert_eq!(reader.next_event().unwrap(), Event::Up);
        assert_eq!(reader.next_event().unwrap(), Event::Down);
        assert_eq!(reader.next_event().unwrap(), Event::Down);
        assert_eq!(reader.next_event().unwrap(), Event::Home);
        assert_eq!(reader.next_event().unwrap(), Event::Home);
        assert_eq!(reader.next_event().unwrap(), Event::Home);
        assert_eq!(reader.next_event().unwrap(), Event::Home);
        assert_eq!(reader.next_event().unwrap(), Event::End);
        assert_eq!(reader.next_event().unwrap(), Event::End);
        assert_eq!(reader.next_event().unwrap(), Event::End);
        assert_eq!(reader.next_event().unwrap(), Event::End);
        assert_eq!(reader.next_event().unwrap(), Event::PageUp);
        assert_eq!(reader.next_event().unwrap(), Event::PageDown);
        assert_eq!(reader.next_event().unwrap(), Event::ChooseCandidate(0));
        assert_eq!(reader.next_event().unwrap(), Event::ChooseCandidate(1));
        assert_eq!(reader.next_event().unwrap(), Event::ChooseCandidate(2));
        assert_eq!(reader.next_event().unwrap(), Event::ChooseCandidate(3));
        assert_eq!(reader.next_event().unwrap(), Event::ChooseCandidate(4));
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

    #[test]
    fn terminal_guard_configures_raw_mode_and_restores_on_drop() {
        use nix::pty::openpty;
        use nix::sys::termios::LocalFlags;

        let pty = openpty(None, None).expect("openpty must succeed");
        let orig_slave = tcgetattr(&pty.slave).expect("tcgetattr on slave");

        let mut master_file = std::fs::File::from(pty.master);
        let mut buf = [0u8; 64];

        {
            let guard = SurgeryTerminalGuard::enter_on(&pty.slave).expect("guard enters");

            // Assert ICANON, ECHO, ISIG are cleared
            let current = tcgetattr(&pty.slave).expect("tcgetattr on slave");
            assert!(!current.local_flags.contains(LocalFlags::ICANON));
            assert!(!current.local_flags.contains(LocalFlags::ECHO));
            assert!(!current.local_flags.contains(LocalFlags::ISIG));
            assert_eq!(current.control_chars[SpecialCharacterIndices::VMIN as usize], 1);
            assert_eq!(current.control_chars[SpecialCharacterIndices::VTIME as usize], 0);

            // Master receives bracketed paste enable sequence "\x1b[?2004h"
            let n = master_file.read(&mut buf).expect("read from master");
            assert_eq!(&buf[..n], b"\x1b[?2004h");

            drop(guard);
        }

        // On drop, master receives bracketed paste disable sequence "\x1b[?2004l"
        let n = master_file.read(&mut buf).expect("read from master");
        assert_eq!(&buf[..n], b"\x1b[?2004l");

        // Slave termios is restored to exact original
        let restored = tcgetattr(&pty.slave).expect("tcgetattr on slave");
        assert_eq!(restored, orig_slave);
    }

    #[test]
    fn terminal_guard_returns_enotty_on_non_terminal() {
        let (reader, _writer) = nix::unistd::pipe().expect("pipe must succeed");
        let result = SurgeryTerminalGuard::enter_on(&reader);
        assert!(result.is_err());
        let (msg, code) = result.err().unwrap();
        assert!(msg.starts_with("E-NOTTY"));
        assert_eq!(code, 2);
    }
}

use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use nix::ioctl_read_bad;
use nix::ioctl_write_ptr_bad;
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{Pid, pipe, read, write};

use crate::control::{ControlChannel, ControlMessage};
use crate::keybind::{self, KeybindWatcher};
use crate::keyboard::HotkeyMonitor;
use crate::sound::SoundPlayer;

ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);
ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

const CONTROL_FD: i32 = 3;
const NO_OUTPUT: &str = "[NO_VISIBLE_OUTPUT]";

const DEFAULT_SKIP_LIST: &str = "\
# Commands listed here (one name per line) do not replace the clipboard when
# they produce no visible output. Output, including errors, is still captured.
# Blank lines and lines starting with # are ignored.
cd
";

static DEBUG_LOG_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static DEBUG_LOG_START: OnceLock<Instant> = OnceLock::new();

fn debug_log(event: &str, details: &str) {
    let Some(path) = std::env::var_os("CVB_DEBUG_LOG") else {
        return;
    };

    let sequence = DEBUG_LOG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mono_ns = DEBUG_LOG_START
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos();

    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };

    let _ = writeln!(
        file,
        "seq={sequence} mono_ns={mono_ns} event={event} {details}"
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MarkerEvent {
    Data(Vec<u8>),
    Start,
    Pathop,
    Skip,
    End(i32),
}

struct MarkerScanner {
    start: Vec<u8>,
    end: Vec<u8>,
    pathop: Vec<u8>,
    skip: Vec<u8>,
    pending: Vec<u8>,
}

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

#[cfg(test)]
mod capture_state_tests {
    use super::CaptureState;

    #[test]
    fn captures_data_between_start_and_end() {
        let mut capture = CaptureState::default();

        capture.start();
        capture.push(b"ONE\n");
        capture.push(b"TWO\n");

        assert_eq!(capture.end(), b"ONE\nTWO\n");
    }

    #[test]
    fn repeated_start_keeps_existing_capture() {
        let mut capture = CaptureState::default();

        capture.start();
        capture.push(b"ONE\n");
        capture.start();
        capture.push(b"TWO\n");

        assert_eq!(capture.end(), b"ONE\nTWO\n");
    }

    #[test]
    fn ignores_data_before_start() {
        let mut capture = CaptureState::default();

        capture.push(b"IGNORED\n");

        assert_eq!(capture.end(), b"");
    }

    #[test]
    fn end_returns_capture_and_resets_state() {
        let mut capture = CaptureState::default();

        capture.start();
        capture.push(b"ONE\n");

        assert_eq!(capture.end(), b"ONE\n");
        assert_eq!(capture.end(), b"");
    }

    #[test]
    fn new_capture_starts_clean_after_end() {
        let mut capture = CaptureState::default();

        capture.start();
        capture.push(b"ONE\n");
        assert_eq!(capture.end(), b"ONE\n");

        capture.start();
        capture.push(b"TWO\n");

        assert_eq!(capture.end(), b"TWO\n");
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum TranscriptSegment {
    Terminal(Vec<u8>),
    Literal(Vec<u8>),
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Transcript {
    segments: Vec<TranscriptSegment>,
}

#[allow(dead_code)]
impl Transcript {
    fn push_terminal(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if let Some(TranscriptSegment::Terminal(previous)) = self.segments.last_mut() {
            previous.extend_from_slice(data);
        } else {
            self.segments
                .push(TranscriptSegment::Terminal(data.to_vec()));
        }
    }

    fn push_literal(&mut self, data: &[u8]) {
        if !data.is_empty() {
            self.segments
                .push(TranscriptSegment::Literal(data.to_vec()));
        }
    }
}

fn render_terminal_segment(bytes: &[u8], rows: u16, cols: u16) -> String {
    let mut renderer = vt100::Parser::new(rows, cols, bytes.len());
    renderer.process(bytes);

    screen_to_text(&mut renderer)
}

/// True when rendering the transcript would yield only `[NO_VISIBLE_OUTPUT]`:
/// no literal (file bridge) content and no terminal segment with visible text.
fn transcript_is_visibly_empty(transcript: &Transcript, rows: u16, cols: u16) -> bool {
    transcript.segments.iter().all(|segment| match segment {
        TranscriptSegment::Terminal(bytes) => render_terminal_segment(bytes, rows, cols).is_empty(),
        TranscriptSegment::Literal(_) => false,
    })
}

fn render_transcript(transcript: &Transcript, rows: u16, cols: u16) -> Vec<u8> {
    if transcript.segments.is_empty() {
        return NO_OUTPUT.as_bytes().to_vec();
    }

    let mut output = Vec::new();

    for segment in &transcript.segments {
        match segment {
            TranscriptSegment::Terminal(bytes) => {
                output.extend_from_slice(b"[Terminal output]\n--------\n");

                let text = render_terminal_segment(bytes, rows, cols);
                let processed = if text.is_empty() {
                    NO_OUTPUT.to_string()
                } else {
                    text
                };

                output.extend_from_slice(processed.as_bytes());

                if !processed.ends_with('\n') {
                    output.push(b'\n');
                }
            }
            TranscriptSegment::Literal(bytes) => {
                output.extend_from_slice(bytes);
            }
        }
    }

    output
}

#[cfg(test)]
mod transcript_tests {
    use super::{Transcript, TranscriptSegment, render_transcript, transcript_is_visibly_empty};

    #[test]
    fn renders_terminal_and_preserves_literal_bytes_in_order() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"\x1b[31mRED\x1b[0m\r\n");
        transcript.push_literal(b"[file.txt]\n--------\n\x1b[31mRAW\x1b[0m\n");
        transcript.push_terminal(b"NEXT\r\n");

        let rendered = render_transcript(&transcript, 24, 80);

        assert_eq!(
            rendered,
            b"[Terminal output]\n--------\nRED\n[file.txt]\n--------\n\x1b[31mRAW\x1b[0m\n[Terminal output]\n--------\nNEXT\n"
        );
    }

    #[test]
    fn empty_transcript_is_visibly_empty() {
        assert!(transcript_is_visibly_empty(&Transcript::default(), 24, 80));
    }

    #[test]
    fn terminal_segment_without_text_is_visibly_empty() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"\x1b[0m\r\n\r\n");

        assert!(transcript_is_visibly_empty(&transcript, 24, 80));
    }

    #[test]
    fn terminal_text_is_not_visibly_empty() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"bash: cd: /nope: No such file or directory\r\n");

        assert!(!transcript_is_visibly_empty(&transcript, 24, 80));
    }

    #[test]
    fn literal_segment_is_not_visibly_empty() {
        let mut transcript = Transcript::default();

        transcript.push_literal(b"[file.txt]\n--------\n");

        assert!(!transcript_is_visibly_empty(&transcript, 24, 80));
    }

    #[test]
    fn renders_empty_transcript_as_no_output() {
        let transcript = Transcript::default();

        assert_eq!(
            render_transcript(&transcript, 24, 80),
            b"[NO_VISIBLE_OUTPUT]"
        );
    }

    #[test]
    fn preserves_terminal_and_literal_segments_in_order() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"command output\n");
        transcript.push_literal(b"\x1b[31mfile contents\x1b[0m");
        transcript.push_terminal(b"next command output\n");

        assert_eq!(
            transcript.segments,
            vec![
                TranscriptSegment::Terminal(b"command output\n".to_vec()),
                TranscriptSegment::Literal(b"\x1b[31mfile contents\x1b[0m".to_vec()),
                TranscriptSegment::Terminal(b"next command output\n".to_vec()),
            ]
        );
    }

    #[test]
    fn combines_adjacent_terminal_chunks() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"\x1b[");
        transcript.push_terminal(b"31mred\x1b[0m");

        assert_eq!(
            transcript.segments,
            vec![TranscriptSegment::Terminal(b"\x1b[31mred\x1b[0m".to_vec())]
        );
    }

    #[test]
    fn ignores_empty_segments() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"");
        transcript.push_literal(b"");

        assert!(transcript.segments.is_empty());
    }
}

impl MarkerScanner {
    fn new() -> Self {
        Self {
            start: b"\x1b]777;CVB;START\x07".to_vec(),
            end: b"\x1b]777;CVB;END\x07".to_vec(),
            pathop: b"\x1b]777;CVB;PATHOP\x07".to_vec(),
            skip: b"\x1b]777;CVB;SKIP\x07".to_vec(),
            pending: Vec::new(),
        }
    }

    fn process(&mut self, data: &[u8]) -> Vec<MarkerEvent> {
        self.pending.extend_from_slice(data);

        let mut events = Vec::new();

        loop {
            let marker = [
                (
                    find_subslice(&self.pending, &self.start),
                    self.start.len(),
                    MarkerEvent::Start,
                ),
                (
                    find_subslice(&self.pending, &self.end),
                    self.end.len(),
                    MarkerEvent::End(0),
                ),
                (
                    find_subslice(&self.pending, &self.pathop),
                    self.pathop.len(),
                    MarkerEvent::Pathop,
                ),
                (
                    find_subslice(&self.pending, &self.skip),
                    self.skip.len(),
                    MarkerEvent::Skip,
                ),
            ]
            .into_iter()
            .filter_map(|(position, length, event)| {
                position.map(|position| (position, length, event))
            })
            .min_by_key(|(position, _, _)| *position);

            let Some((position, length, event)) = marker else {
                break;
            };

            if position > 0 {
                events.push(MarkerEvent::Data(self.pending.drain(..position).collect()));
            }

            self.pending.drain(..length);
            events.push(event);
        }

        if !self.pending.is_empty() {
            let keep = self
                .start
                .len()
                .max(self.end.len())
                .max(self.pathop.len())
                .max(self.skip.len())
                .saturating_sub(1);

            if self.pending.len() > keep {
                let split_at = self.pending.len() - keep;
                events.push(MarkerEvent::Data(self.pending.drain(..split_at).collect()));
            }
        }

        events
    }
}

#[cfg(test)]
mod marker_scanner_tests {
    use super::{MarkerEvent, MarkerScanner};

    #[test]
    fn recognizes_pathop_split_at_sixteenth_byte() {
        let marker = b"\x1b]777;CVB;PATHOP\x07";
        assert_eq!(marker.len(), 17);

        let mut scanner = MarkerScanner::new();

        assert_eq!(scanner.process(&marker[..16]), Vec::<MarkerEvent>::new());

        assert_eq!(scanner.process(&marker[16..]), vec![MarkerEvent::Pathop]);
    }

    #[test]
    fn recognizes_bracketed_paste_marker_sequence() {
        let mut scanner = MarkerScanner::new();

        let events = scanner.process(
            b"\x1b]777;CVB;START\x07\x1b]777;CVB;PATHOP\x07\x1b]777;CVB;START\x07\x1b]777;CVB;PATHOP\x07\x1b]777;CVB;START\x07\x1b]777;CVB;END\x07",
        );

        assert_eq!(
            events,
            vec![
                MarkerEvent::Start,
                MarkerEvent::Pathop,
                MarkerEvent::Start,
                MarkerEvent::Pathop,
                MarkerEvent::Start,
                MarkerEvent::End(0),
            ]
        );
    }

    #[test]
    fn recognizes_skip_marker_before_end() {
        let mut scanner = MarkerScanner::new();

        let events =
            scanner.process(b"\x1b]777;CVB;START\x07\x1b]777;CVB;SKIP\x07\x1b]777;CVB;END\x07");

        assert_eq!(
            events,
            vec![MarkerEvent::Start, MarkerEvent::Skip, MarkerEvent::End(0)]
        );
    }

    #[test]
    fn recognizes_skip_marker_split_across_reads() {
        let marker = b"\x1b]777;CVB;SKIP\x07";
        let mut scanner = MarkerScanner::new();

        assert_eq!(scanner.process(&marker[..10]), Vec::<MarkerEvent>::new());
        assert_eq!(scanner.process(&marker[10..]), vec![MarkerEvent::Skip]);
    }

    #[test]
    fn recognizes_start_and_end_markers() {
        let mut scanner = MarkerScanner::new();

        let mut events =
            scanner.process(b"before\x1b]777;CVB;START\x07ONE\x1b]777;CVB;END\x07after");

        events.extend(scanner.process(b"0123456789012345"));

        assert_eq!(
            events,
            vec![
                MarkerEvent::Data(b"before".to_vec()),
                MarkerEvent::Start,
                MarkerEvent::Data(b"ONE".to_vec()),
                MarkerEvent::End(0),
                MarkerEvent::Data(b"after".to_vec()),
            ]
        );
    }

    #[test]
    fn preserves_data_between_markers() {
        let mut scanner = MarkerScanner::new();

        let events = scanner.process(b"\x1b]777;CVB;START\x07ONE\nTWO\n\x1b]777;CVB;END\x07");

        assert_eq!(
            events,
            vec![
                MarkerEvent::Start,
                MarkerEvent::Data(b"ONE\nTWO\n".to_vec()),
                MarkerEvent::End(0),
            ]
        );
    }

    #[test]
    fn recognizes_marker_split_across_reads() {
        let mut scanner = MarkerScanner::new();

        assert_eq!(
            scanner.process(b"\x1b]777;CVB;ST"),
            Vec::<MarkerEvent>::new()
        );

        let events = scanner.process(b"ART\x07ONE\x1b]777;CVB;END\x07");

        assert_eq!(
            events,
            vec![
                MarkerEvent::Start,
                MarkerEvent::Data(b"ONE".to_vec()),
                MarkerEvent::End(0),
            ]
        );
    }
}

#[cfg(test)]
mod skip_file_tests {
    use super::{DEFAULT_SKIP_LIST, ensure_skip_file, skip_file_path};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn prefers_xdg_config_home() {
        assert_eq!(
            skip_file_path(Some("/x/config".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/config/cvb/skip"))
        );
    }

    #[test]
    fn falls_back_to_home_dot_config() {
        assert_eq!(
            skip_file_path(None, Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/cvb/skip"))
        );
    }

    #[test]
    fn empty_xdg_config_home_counts_as_unset() {
        assert_eq!(
            skip_file_path(Some(OsString::new()), Some("/home/u".into())),
            Some(PathBuf::from("/home/u/.config/cvb/skip"))
        );
    }

    #[test]
    fn no_location_without_xdg_or_home() {
        assert_eq!(skip_file_path(None, None), None);
    }

    #[test]
    fn creates_default_file_once_and_keeps_user_edits() {
        let dir = std::env::temp_dir().join(format!("cvb-skip-test-{}", std::process::id()));
        let path = dir.join("cvb").join("skip");
        let _ = std::fs::remove_dir_all(&dir);

        ensure_skip_file(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_SKIP_LIST);

        std::fs::write(&path, "pushd\n").unwrap();
        ensure_skip_file(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "pushd\n");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

static mut SIGWINCH_PIPE: Option<i32> = None;

extern "C" fn handle_sigwinch(_: libc::c_int) {
    unsafe {
        if let Some(fd) = SIGWINCH_PIPE {
            let _ = libc::write(fd, &[1u8] as *const u8 as *const libc::c_void, 1);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum SurgeryProxyState {
    #[default]
    Ordinary,
    Active {
        token: String,
    },
    Completed {
        outcome: String,
    },
    Aborted,
}

fn capture_terminal_output(
    capture: &mut CaptureState,
    transcript: &mut Transcript,
    state: &SurgeryProxyState,
    output: &[u8],
) {
    if capture.capturing && *state == SurgeryProxyState::Ordinary {
        transcript.push_terminal(output);
        capture.push(output);
    }
}
pub fn run(rcfile: &Path) -> Result<i32, String> {
    let stdin = io::stdin();

    let original_termios = termios::tcgetattr(stdin.as_fd())
        .map_err(|error| format!("CVB: unable to read terminal settings: {error}"))?;

    let raw_termios = make_raw(&original_termios);

    termios::tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &raw_termios)
        .map_err(|error| format!("CVB: unable to enter raw terminal mode: {error}"))?;

    let result = run_pty(rcfile);

    if let Err(error) = termios::tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &original_termios) {
        eprintln!("CVB: unable to restore terminal settings: {error}");
    }

    result
}

fn run_pty(rcfile: &Path) -> Result<i32, String> {
    let (signal_read, signal_write) =
        pipe().map_err(|error| format!("CVB: unable to create signal pipe: {error}"))?;

    let (control_parent, control_child) = control_socketpair()?;

    set_nonblocking(&control_parent)?;

    unsafe {
        SIGWINCH_PIPE = Some(signal_write.as_raw_fd());

        signal::signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch))
            .map_err(|error| format!("CVB: unable to install SIGWINCH handler: {error}"))?;
    }

    let winsize = terminal_size()?;

    let skip_file = skip_file_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    );
    if let Some(path) = &skip_file {
        ensure_skip_file(path);
    }

    let result = unsafe { forkpty(Some(&winsize), None) }
        .map_err(|error| format!("CVB: unable to create PTY: {error}"))?;

    match result {
        ForkptyResult::Child => {
            unsafe {
                SIGWINCH_PIPE = None;

                signal::signal(Signal::SIGWINCH, SigHandler::SigDfl)
                    .map_err(|error| format!("CVB: unable to restore SIGWINCH handler: {error}"))?;
            }

            drop(signal_read);
            drop(signal_write);
            drop(control_parent);

            if let Err(error) = install_control_fd(control_child) {
                eprintln!("{error}");
                std::process::exit(127);
            }

            // Tell the Bash integration where the skip list lives. Left unset
            // when no config directory can be determined.
            match &skip_file {
                // Safe: the forked child is single-threaded here.
                Some(path) => unsafe { std::env::set_var("CVB_SKIP_FILE", path) },
                None => unsafe { std::env::remove_var("CVB_SKIP_FILE") },
            }

            unsafe {
                libc::setenv(
                    c"PAGER".as_ptr(),
                    c"cat".as_ptr(),
                    1,
                );
                libc::setenv(
                    c"GIT_PAGER".as_ptr(),
                    c"cat".as_ptr(),
                    1,
                );
                libc::setenv(
                    c"SYSTEMD_PAGER".as_ptr(),
                    c"cat".as_ptr(),
                    1,
                );
            }

            let bash = CString::new("bash").unwrap();

            let rcfile_arg = match CString::new(rcfile.to_string_lossy().as_bytes()) {
                Ok(value) => value,
                Err(_) => {
                    eprintln!("CVB: rcfile path contains a NUL byte");
                    std::process::exit(127);
                }
            };

            let rcfile_option = CString::new("--rcfile").unwrap();
            let interactive = CString::new("-i").unwrap();

            let args = [
                bash.as_c_str(),
                rcfile_option.as_c_str(),
                rcfile_arg.as_c_str(),
                interactive.as_c_str(),
            ];

            match nix::unistd::execvp(&bash, &args) {
                Ok(never) => match never {},
                Err(error) => {
                    eprintln!("CVB: unable to exec Bash: {error}");
                    std::process::exit(127);
                }
            }
        }

        ForkptyResult::Parent { master, child } => {
            drop(control_child);

            let mut control = ControlChannel::new(control_parent);
            let sound = SoundPlayer::new();

            let result = proxy(
                &master,
                child,
                &signal_read,
                &mut control,
                winsize.ws_row,
                winsize.ws_col,
                sound.as_ref(),
            );

            let restore_result = unsafe {
                SIGWINCH_PIPE = None;

                signal::signal(Signal::SIGWINCH, SigHandler::SigDfl)
                    .map_err(|error| format!("CVB: unable to restore SIGWINCH handler: {error}"))
            };

            let result = match (result, restore_result) {
                (Ok(()), Ok(_)) => Ok(()),
                (Err(proxy_error), Ok(_)) => Err(proxy_error),
                (Ok(()), Err(restore_error)) => Err(restore_error),
                (Err(proxy_error), Err(restore_error)) => {
                    Err(format!("{proxy_error}; additionally, {restore_error}"))
                }
            };

            if let Err(error) = result {
                return match terminate_and_reap_child(child) {
                    Ok(()) => Err(error),
                    Err(cleanup_error) => Err(format!("{error}; additionally, {cleanup_error}")),
                };
            }

            let status = waitpid(child, None)
                .map_err(|error| format!("CVB: unable to wait for child: {error}"))?;

            Ok(status_to_code(status))
        }
    }
}

/// Location of the skip list: `$XDG_CONFIG_HOME/cvb/skip`, falling back to
/// `~/.config/cvb/skip`. An empty `XDG_CONFIG_HOME` counts as unset.
fn skip_file_path(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let config_dir = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| home.map(|home| std::path::PathBuf::from(home).join(".config")))?;

    Some(config_dir.join("cvb").join("skip"))
}

/// Create the skip list with defaults on first use. Failure is not fatal:
/// without the file nothing is skipped.
fn ensure_skip_file(path: &Path) {
    if path.exists() {
        return;
    }

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "CVB: unable to create config directory {}: {error}",
            parent.display()
        );
        return;
    }

    if let Err(error) = std::fs::write(path, DEFAULT_SKIP_LIST) {
        eprintln!(
            "CVB: unable to create skip list {}: {error}",
            path.display()
        );
    }
}

fn control_socketpair() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [0; 2];

    let result = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };

    if result == -1 {
        return Err(format!(
            "CVB: unable to create control socketpair: {}",
            io::Error::last_os_error()
        ));
    }

    let parent = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let child = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    Ok((parent, child))
}

fn set_nonblocking(fd: &OwnedFd) -> Result<(), String> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };

    if flags == -1 {
        return Err(format!(
            "CVB: unable to read control socket flags: {}",
            io::Error::last_os_error()
        ));
    }

    let result = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };

    if result == -1 {
        return Err(format!(
            "CVB: unable to make control socket nonblocking: {}",
            io::Error::last_os_error()
        ));
    }

    Ok(())
}

fn install_control_fd(control: OwnedFd) -> Result<(), String> {
    let result = unsafe { libc::dup2(control.as_raw_fd(), CONTROL_FD) };

    if result == -1 {
        return Err(format!(
            "CVB: unable to install control channel on fd {CONTROL_FD}: {}",
            io::Error::last_os_error()
        ));
    }

    if control.as_raw_fd() != CONTROL_FD {
        drop(control);
    }

    Ok(())
}

fn proxy(
    master: &OwnedFd,
    child: Pid,
    signal_read: &OwnedFd,
    control: &mut ControlChannel,
    rows: u16,
    cols: u16,
    sound: Option<&SoundPlayer>,
) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();

    let mut buffer = [0u8; 8192];
    let mut signal_buffer = [0u8; 64];
    let mut rows = rows;
    let mut cols = cols;
    let mut screen_terminal = vt100::Parser::new(rows, cols, 1000);
    let bindings = keybind::load()?;
    let mut keyboard = HotkeyMonitor::new(bindings)?;
    let keybind_watcher = KeybindWatcher::new()?;
    let mut marker_scanner = MarkerScanner::new();
    let mut transcript = Transcript::default();
    let mut capture = CaptureState::default();
    // Set by the SKIP marker: the finished command line consisted only of
    // commands from the skip list. Consumed by the next END marker.
    let mut skip_requested = false;
    let mut surgery_state = SurgeryProxyState::Ordinary;
    debug_log("proxy_start", &format!("pid={}", std::process::id()));

    loop {
        let keyboard_fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(keyboard.fd()) };
        let keybind_fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(keybind_watcher.fd()) };

        let mut poll_fds = [
            nix::poll::PollFd::new(stdin.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(master.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(signal_read.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(control.fd().as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(keyboard_fd, nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(keybind_fd, nix::poll::PollFlags::POLLIN),
        ];

        match nix::poll::poll(&mut poll_fds, None::<u16>) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(format!("CVB: PTY poll failed: {error}")),
        }

        let stdin_events = poll_fds[0]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        let master_events = poll_fds[1]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        let signal_events = poll_fds[2]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        let control_events = poll_fds[3]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        let keyboard_events = poll_fds[4]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        let keybind_events = poll_fds[5]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        if keybind_events.contains(nix::poll::PollFlags::POLLIN)
            && keybind_watcher.changed()?
        {
            match keybind::load() {
                Ok(bindings) => keyboard.set_bindings(bindings),
                Err(error) => eprintln!("{error}"),
            }
        }

        if signal_events.contains(nix::poll::PollFlags::POLLIN) {
            let _ = read(signal_read.as_fd(), &mut signal_buffer);

            let winsize = resize_pty(master, child)?;
            rows = winsize.ws_row;
            cols = winsize.ws_col;
            screen_terminal
                .screen_mut()
                .set_size(winsize.ws_row, winsize.ws_col);
        }

        if keyboard_events.contains(nix::poll::PollFlags::POLLIN) && keyboard.poll()? {
            capture_screen(&mut screen_terminal)?;

            if let Some(sound) = sound {
                sound.play("screen-capture");
            }
        }

        if stdin_events.contains(nix::poll::PollFlags::POLLIN) {
            let count = read(stdin.as_fd(), &mut buffer)
                .map_err(|error| format!("CVB: stdin read failed: {error}"))?;

            if count == 0 {
                break;
            }

            write(master.as_fd(), &buffer[..count])
                .map_err(|error| format!("CVB: PTY write failed: {error}"))?;
        }

        if master_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            let count = match read(master.as_fd(), &mut buffer) {
                Ok(count) => count,
                Err(nix::errno::Errno::EIO) => break,
                Err(error) => return Err(format!("CVB: PTY read failed: {error}")),
            };

            if count == 0 {
                break;
            }

            let output = &buffer[..count];

            screen_terminal.process(output);

            let mut stdout = stdout.lock();

            stdout
                .write_all(output)
                .map_err(|error| format!("CVB: stdout write failed: {error}"))?;

            let events = marker_scanner.process(output);

            for event in events {
                match event {
                    MarkerEvent::Data(output) => {
                        capture_terminal_output(
                            &mut capture,
                            &mut transcript,
                            &surgery_state,
                            &output,
                        );
                    }

                    MarkerEvent::Pathop => {
                        let capturing_before = capture.capturing;

                        debug_log(
                            "marker_pathop",
                            &format!(
                                "capturing_before={capturing_before} \
                                 capture_preserved=true"
                            ),
                        );
                    }

                    MarkerEvent::Skip => {
                        skip_requested = capture.capturing;

                        debug_log("marker_skip", &format!("skip_requested={skip_requested}"));
                    }

                    MarkerEvent::Start => {
                        let capturing_before = capture.capturing;
                        skip_requested = false;
                        if !matches!(surgery_state, SurgeryProxyState::Active { .. }) {
                            surgery_state = SurgeryProxyState::Ordinary;
                        }
                        capture.start();

                        debug_log(
                            "marker_start",
                            &format!(
                                "capturing_before={capturing_before} capturing_after={}",
                                capture.capturing
                            ),
                        );
                    }

                    MarkerEvent::End(_status) => {
                        let capturing_before = capture.capturing;
                        let skip_listed = std::mem::take(&mut skip_requested);

                        if !capture.capturing {
                            debug_log(
                                "marker_end_no_capture",
                                &format!(
                                    "capturing_before={capturing_before} capturing_after={}",
                                    capture.capturing
                                ),
                            );
                            continue;
                        }

                        let _captured = capture.end();

                        match std::mem::replace(&mut surgery_state, SurgeryProxyState::Ordinary) {
                            SurgeryProxyState::Completed { outcome } => {
                                debug_log("surgery_dispatch_outcome", &outcome);
                                let buffer = crate::clipboard::ClipboardBuffer::new(outcome.into_bytes());
                                crate::clipboard::copy_to_clipboard(&buffer)?;
                                transcript = Transcript::default();
                                continue;
                            }
                            SurgeryProxyState::Active { .. } | SurgeryProxyState::Aborted => {
                                debug_log("surgery_aborted_or_abrupt_end", "suppressing clipboard update");
                                transcript = Transcript::default();
                                continue;
                            }
                            SurgeryProxyState::Ordinary => {}
                        }

                        // A skip-listed command line with no visible output
                        // leaves the clipboard untouched. Any output, such as
                        // an error message, is captured as usual.
                        if skip_listed && transcript_is_visibly_empty(&transcript, rows, cols) {
                            debug_log("capture_skipped", "reason=skip_listed_no_output");
                            transcript = Transcript::default();
                            continue;
                        }

                        let rendered = render_transcript(&transcript, rows, cols);
                        let captured_bytes = rendered.len();
                        let buffer = crate::clipboard::ClipboardBuffer::new(rendered);

                        debug_log(
                            "clipboard_dispatch_begin",
                            &format!(
                                "capturing_before={capturing_before} capturing_after={} captured_bytes={captured_bytes}",
                                capture.capturing
                            ),
                        );

                        crate::clipboard::process(&buffer)?;

                        debug_log(
                            "clipboard_dispatch_ok",
                            &format!(
                                "capturing_before={capturing_before} capturing_after={} captured_bytes={captured_bytes}",
                                capture.capturing
                            ),
                        );

                        transcript = Transcript::default();
                    }
                }
            }

            stdout
                .flush()
                .map_err(|error| format!("CVB: stdout flush failed: {error}"))?;
        }

        if control_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            for message in control.receive()? {
                handle_control_message(message, control, &mut transcript, &mut surgery_state)?;
            }

            if control_events.contains(nix::poll::PollFlags::POLLHUP) {
                break;
            }
        }
    }

    Ok(())
}

fn handle_control_message(
    message: ControlMessage,
    control: &ControlChannel,
    transcript: &mut Transcript,
    surgery_state: &mut SurgeryProxyState,
) -> Result<(), String> {
    let expected_token = std::env::var("CVB_TOKEN").ok();
    handle_control_message_with(
        message,
        control,
        transcript,
        surgery_state,
        expected_token.as_deref(),
        crate::clipboard::copy_to_clipboard,
    )
}

fn handle_control_message_with<F>(
    message: ControlMessage,
    control: &ControlChannel,
    transcript: &mut Transcript,
    surgery_state: &mut SurgeryProxyState,
    expected_token: Option<&str>,
    mut copy: F,
) -> Result<(), String>
where
    F: FnMut(&crate::clipboard::ClipboardBuffer) -> Result<(), String>,
{
    match message {
        ControlMessage::Exit => {}

        ControlMessage::TargetRequest { id, cwd, args } => {
            let arg_count = args.len();

            debug_log("target_request", &format!("id={id} arg_count={arg_count}"));

            let paths = args
                .into_iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>();

            let base_dir = std::path::PathBuf::from(cwd);
            let leaves = crate::build_path_leaves(&paths, &base_dir);
            let status = if leaves.iter().all(|(_, _, succeeded)| *succeeded) {
                0
            } else {
                1
            };

            for (label, contents, _) in leaves {
                transcript.push_literal(label.as_bytes());
                transcript.push_literal(b"\n--------\n");
                transcript.push_literal(&contents);

                if !contents.ends_with(b"\n") {
                    transcript.push_literal(b"\n");
                }
            }

            control.send(&ControlMessage::TargetResponse {
                id,
                status,
                message: String::new(),
            })?;
        }

        ControlMessage::TargetResponse { .. } => {}

        ControlMessage::SurgeryBegin { token } => {
            if matches!(surgery_state, SurgeryProxyState::Ordinary) {
                if let Some(expected) = expected_token {
                    if !expected.trim().is_empty() && token == expected {
                        *surgery_state = SurgeryProxyState::Active { token };
                        *transcript = Transcript::default();
                    }
                }
            }
        }

        ControlMessage::ClipboardWrite { text } => {
            if matches!(surgery_state, SurgeryProxyState::Active { .. }) {
                let buffer = crate::clipboard::ClipboardBuffer::new(text.into_bytes());
                copy(&buffer)?;
            }
        }

        ControlMessage::SurgeryEnd { status, outcome } => {
            if matches!(surgery_state, SurgeryProxyState::Active { .. }) {
                if status == 0 {
                    *surgery_state = SurgeryProxyState::Completed { outcome };
                } else {
                    *surgery_state = SurgeryProxyState::Aborted;
                }
                *transcript = Transcript::default();
            }
        }
    }

    Ok(())
}

fn capture_screen(terminal: &mut vt100::Parser) -> Result<(), String> {
    terminal.screen_mut().set_scrollback(usize::MAX);

    terminal.screen_mut().set_scrollback(0);

    let text = screen_viewport_to_text(terminal);

    let processed = if text.is_empty() {
        NO_OUTPUT.to_string()
    } else {
        text
    };

    let buffer = crate::clipboard::ClipboardBuffer::new(processed.into_bytes());
    crate::clipboard::process(&buffer)?;

    Ok(())
}

fn screen_viewport_to_text(terminal: &mut vt100::Parser) -> String {
    let (_, cols) = terminal.screen().size();

    terminal.screen_mut().set_scrollback(0);

    terminal
        .screen()
        .rows(0, cols)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

fn screen_to_text(terminal: &mut vt100::Parser) -> String {
    let (_, cols) = terminal.screen().size();

    terminal.screen_mut().set_scrollback(usize::MAX);
    let max_scrollback = terminal.screen().scrollback();

    let mut output = Vec::new();

    for offset in (1..=max_scrollback).rev() {
        terminal.screen_mut().set_scrollback(offset);

        if let Some(row) = terminal.screen().rows(0, cols).next() {
            output.push(row);
        }
    }

    terminal.screen_mut().set_scrollback(0);

    for row in terminal.screen().rows(0, cols) {
        output.push(row);
    }

    output.join("\n").trim_end_matches('\n').to_string()
}

fn make_raw(original: &Termios) -> Termios {
    let mut raw = original.clone();

    termios::cfmakeraw(&mut raw);

    raw
}

fn terminal_size() -> Result<Winsize, String> {
    let stdin = io::stdin();

    let mut winsize = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    unsafe {
        tiocgwinsz(stdin.as_raw_fd(), &mut winsize)
            .map_err(|error| format!("CVB: unable to determine terminal size: {error}"))?;
    }

    if winsize.ws_row == 0 || winsize.ws_col == 0 {
        return Err("CVB: terminal reported an invalid size".to_string());
    }

    Ok(winsize)
}

fn resize_pty(master: &OwnedFd, child: Pid) -> Result<Winsize, String> {
    let winsize = terminal_size()?;

    unsafe {
        tiocswinsz(master.as_raw_fd(), &winsize)
            .map_err(|error| format!("CVB: unable to resize PTY: {error}"))?;
    }

    signal::kill(child, Signal::SIGWINCH)
        .map_err(|error| format!("CVB: unable to forward SIGWINCH: {error}"))?;

    Ok(winsize)
}

fn status_to_code(status: WaitStatus) -> i32 {
    match status {
        WaitStatus::Exited(_, code) => code,
        WaitStatus::Signaled(_, signal, _) => 128 + signal as i32,
        _ => 1,
    }
}

fn terminate_and_reap_child(child: Pid) -> Result<(), String> {
    match signal::kill(child, Signal::SIGKILL) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(error) => {
            return Err(format!(
                "CVB: unable to terminate child after proxy failure: {error}"
            ));
        }
    }

    loop {
        match waitpid(child, None) {
            Ok(_) | Err(nix::errno::Errno::ECHILD) => return Ok(()),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => {
                return Err(format!(
                    "CVB: unable to reap child after proxy failure: {error}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{screen_to_text, screen_viewport_to_text};

    #[test]
    fn proxy_failure_cleanup_terminates_and_reaps_child() {
        use nix::errno::Errno;
        use nix::sys::wait::{waitpid, WaitPidFlag};
        use nix::unistd::{fork, ForkResult};

        let child = match unsafe { fork() }.expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => loop {
                unsafe { libc::pause(); }
            },
        };

        super::terminate_and_reap_child(child).expect("cleanup");

        assert!(matches!(
            waitpid(child, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        ));
    }

    #[test]
    fn renders_plain_multiline_output() {
        let mut terminal = vt100::Parser::new(24, 80, 64);

        terminal.process(b"ONE\r\nTWO\r\nTHREE\r\n");

        assert_eq!(screen_to_text(&mut terminal), "ONE\nTWO\nTHREE");
    }

    #[test]
    fn strips_ansi_escape_sequences_from_text() {
        let mut terminal = vt100::Parser::new(24, 80, 64);

        terminal.process(b"\x1b[31mRED\x1b[0m\r\n");

        assert_eq!(screen_to_text(&mut terminal), "RED");
    }

    #[test]
    fn renders_carriage_return_rewrites() {
        let mut terminal = vt100::Parser::new(24, 80, 64);

        terminal.process(b"progress 10%\rprogress 100%\r\n");

        assert_eq!(screen_to_text(&mut terminal), "progress 100%");
    }

    #[test]
    fn renders_scrolled_back_output() {
        let mut terminal = vt100::Parser::new(3, 80, 64);

        terminal.process(b"ONE\r\nTWO\r\nTHREE\r\nFOUR\r\nFIVE\r\n");

        assert_eq!(screen_to_text(&mut terminal), "ONE\nTWO\nTHREE\nFOUR\nFIVE");
    }

    #[test]
    fn screenshot_rendering_excludes_scrollback() {
        let mut terminal = vt100::Parser::new(3, 80, 64);

        terminal.process(b"ONE\r\nTWO\r\nTHREE\r\nFOUR\r\nFIVE");

        assert_eq!(screen_viewport_to_text(&mut terminal), "THREE\nFOUR\nFIVE");
    }

    #[test]
    fn surgery_begin_sets_active_state_and_clears_transcript() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        transcript.push_terminal(b"dirty modal terminal output");
        let mut state = super::SurgeryProxyState::Ordinary;

        super::handle_control_message_with(
            super::ControlMessage::SurgeryBegin {
                token: "token123".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("token123"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(
            state,
            super::SurgeryProxyState::Active {
                token: "token123".to_string()
            }
        );
        assert!(transcript.segments.is_empty());
    }

    #[test]
    fn clipboard_write_in_active_state_copies_payload() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Active {
            token: "tok".to_string(),
        };
        let mut copied = Vec::new();

        super::handle_control_message_with(
            super::ControlMessage::ClipboardWrite {
                text: "diff candidate".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(copied, vec![b"diff candidate".to_vec()]);
    }

    #[test]
    fn surgery_end_completed_sets_completed_state() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Active {
            token: "tok".to_string(),
        };

        super::handle_control_message_with(
            super::ControlMessage::SurgeryEnd {
                status: 0,
                outcome: "CVB surgery APPLIED id=xyz files=2\n".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(
            state,
            super::SurgeryProxyState::Completed {
                outcome: "CVB surgery APPLIED id=xyz files=2\n".to_string()
            }
        );
        assert!(transcript.segments.is_empty());
    }

    #[test]
    fn surgery_end_aborted_sets_aborted_state() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Active {
            token: "tok".to_string(),
        };

        super::handle_control_message_with(
            super::ControlMessage::SurgeryEnd {
                status: 1,
                outcome: String::new(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(state, super::SurgeryProxyState::Aborted);
        assert!(transcript.segments.is_empty());
    }

    #[test]
    fn fatal_error_clipboard_write_then_surgery_end_aborts() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Active {
            token: "tok".to_string(),
        };
        let mut copied = Vec::new();

        super::handle_control_message_with(
            super::ControlMessage::ClipboardWrite {
                text: "CVB: fatal error report\n".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(copied, vec![b"CVB: fatal error report\n".to_vec()]);

        super::handle_control_message_with(
            super::ControlMessage::SurgeryEnd {
                status: 1,
                outcome: String::new(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(state, super::SurgeryProxyState::Aborted);
        assert!(transcript.segments.is_empty());
    }

    #[test]
    fn clipboard_write_failure_is_propagated_without_changing_surgery_state() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        transcript.push_terminal(b"existing transcript");
        let transcript_before = transcript.clone();
        let mut state = super::SurgeryProxyState::Active {
            token: "tok".to_string(),
        };

        let result = super::handle_control_message_with(
            super::ControlMessage::ClipboardWrite {
                text: "diff candidate".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("tok"),
            |_| Err("test clipboard failure".to_string()),
        );

        assert_eq!(result.unwrap_err(), "test clipboard failure");
        assert_eq!(
            state,
            super::SurgeryProxyState::Active {
                token: "tok".to_string(),
            }
        );
        assert_eq!(transcript.segments, transcript_before.segments);
    }

    #[test]
    fn surgery_begin_rejects_mismatched_token() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        transcript.push_terminal(b"dirty modal terminal output");
        let mut state = super::SurgeryProxyState::Ordinary;

        super::handle_control_message_with(
            super::ControlMessage::SurgeryBegin {
                token: "wrong-token".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("valid-token"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(state, super::SurgeryProxyState::Ordinary);
        assert!(!transcript.segments.is_empty());
    }

    #[test]
    fn surgery_begin_rejects_when_already_active() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Active {
            token: "first-token".to_string(),
        };

        super::handle_control_message_with(
            super::ControlMessage::SurgeryBegin {
                token: "second-token".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("second-token"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(
            state,
            super::SurgeryProxyState::Active {
                token: "first-token".to_string()
            }
        );
    }

    #[test]
    fn surgery_end_ignored_when_ordinary() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        transcript.push_terminal(b"ordinary command output");
        let mut state = super::SurgeryProxyState::Ordinary;

        super::handle_control_message_with(
            super::ControlMessage::SurgeryEnd {
                status: 0,
                outcome: "stray outcome".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("token"),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(state, super::SurgeryProxyState::Ordinary);
        assert!(!transcript.segments.is_empty());
    }

    #[test]
    fn terminal_capture_and_transcript_follow_surgery_state() {
        let sample_data = b"some modal surgery output";
        let ordinary = super::SurgeryProxyState::Ordinary;

        // Ordinary + enabled: both capture and transcript record output.
        let mut capture = super::CaptureState::default();
        capture.start();
        let mut transcript = super::Transcript::default();

        super::capture_terminal_output(
            &mut capture,
            &mut transcript,
            &ordinary,
            sample_data,
        );

        assert_eq!(capture.end().as_slice(), sample_data);
        assert_eq!(
            transcript.segments,
            vec![super::TranscriptSegment::Terminal(sample_data.to_vec())]
        );

        // Ordinary + disabled: neither records output.
        let mut capture = super::CaptureState::default();
        let mut transcript = super::Transcript::default();

        super::capture_terminal_output(
            &mut capture,
            &mut transcript,
            &ordinary,
            sample_data,
        );

        assert!(capture.end().is_empty());
        assert!(transcript.segments.is_empty());

        // Interactive surgery states suppress both outputs.
        for state in [
            super::SurgeryProxyState::Active {
                token: "test-token".to_string(),
            },
            super::SurgeryProxyState::Completed {
                outcome: "test-outcome".to_string(),
            },
            super::SurgeryProxyState::Aborted,
        ] {
            let mut capture = super::CaptureState::default();
            capture.start();
            let mut transcript = super::Transcript::default();

            super::capture_terminal_output(
                &mut capture,
                &mut transcript,
                &state,
                sample_data,
            );

            assert!(
                capture.end().is_empty(),
                "capture must not accumulate data while in state {state:?}"
            );
            assert!(
                transcript.segments.is_empty(),
                "transcript must not accumulate data while in state {state:?}"
            );
        }
    }

    #[test]
    fn clipboard_write_ignored_when_not_active() {
        let (parent, _child) = super::control_socketpair().unwrap();
        let control = super::ControlChannel::new(parent);
        let mut transcript = super::Transcript::default();
        let mut state = super::SurgeryProxyState::Ordinary;
        let mut copied = false;

        super::handle_control_message_with(
            super::ControlMessage::ClipboardWrite {
                text: "stray payload".to_string(),
            },
            &control,
            &mut transcript,
            &mut state,
            Some("token"),
            |_| {
                copied = true;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(state, super::SurgeryProxyState::Ordinary);
        assert!(!copied);
    }
}

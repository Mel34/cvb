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
    Command(String),
    End(i32),
}

struct MarkerScanner {
    start: Vec<u8>,
    end_prefix: Vec<u8>,
    pathop: Vec<u8>,
    skip: Vec<u8>,
    command_prefix: Vec<u8>,
    pending: Vec<u8>,
}

#[derive(Default)]
struct CaptureState {
    capturing: bool,
    captured: Vec<u8>,
}

struct CommandBlockCapture {
    commands: Vec<String>,
    results: Vec<(i32, Vec<u8>)>,
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

/// True when there is no literal content and no terminal segment with visible text.
fn transcript_is_visibly_empty(transcript: &Transcript, rows: u16, cols: u16) -> bool {
    transcript.segments.iter().all(|segment| match segment {
        TranscriptSegment::Terminal(bytes) => render_terminal_segment(bytes, rows, cols).is_empty(),
        TranscriptSegment::Literal(_) => false,
    })
}

fn render_transcript(
    transcript: &Transcript,
    rows: u16,
    cols: u16,
    status: i32,
    commands: &[String],
) -> Vec<u8> {
    let mut output = Vec::new();

    for segment in &transcript.segments {
        match segment {
            TranscriptSegment::Terminal(bytes) => {
                let text = render_terminal_segment(bytes, rows, cols);
                if !text.is_empty() {
                    output.extend_from_slice(b"Terminal output:\n");
                    output.extend_from_slice(text.as_bytes());

                    if !text.ends_with('\n') {
                        output.push(b'\n');
                    }
                }
            }
            TranscriptSegment::Literal(bytes) => {
                output.extend_from_slice(bytes);
            }
        }
    }

    if output.is_empty() {
        let message = if commands.len() == 1 {
            let command = &commands[0];
            if status == 0 {
                format!(
                    "The command `{command}` completed successfully (exit 0) and produced no visible output."
                )
            } else {
                format!(
                    "The command `{command}` failed (exit {status}) and produced no visible output."
                )
            }
        } else if commands.len() > 1 {
            let mut message = format!(
                "The command block finished with exit {status} and produced no visible output.\nCommands observed:\n"
            );
            for command in commands {
                message.push_str("- ");
                message.push_str(command);
                message.push('\n');
            }
            message.trim_end().to_string()
        } else if status == 0 {
            "The command completed successfully (exit 0) and produced no visible output."
                .to_string()
        } else {
            format!("The command failed (exit {status}) and produced no visible output.")
        };
        output.extend_from_slice(message.as_bytes());
    }

    output
}

#[cfg(test)]
mod transcript_tests {
    use super::{
        Transcript, TranscriptSegment, render_transcript, transcript_is_visibly_empty,
    };

    #[test]
    fn renders_terminal_and_preserves_literal_bytes_in_order() {
        let mut transcript = Transcript::default();

        transcript.push_terminal(b"\x1b[31mRED\x1b[0m\r\n");
        transcript.push_literal(b"[file.txt]\n--------\n\x1b[31mRAW\x1b[0m\n");
        transcript.push_terminal(b"NEXT\r\n");

        let rendered = render_transcript(&transcript, 24, 80, 0, &[]);

        assert_eq!(
            rendered,
            b"Terminal output:\nRED\n[file.txt]\n--------\n\x1b[31mRAW\x1b[0m\nTerminal output:\nNEXT\n"
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
    fn renders_empty_transcript_as_success_status() {
        let transcript = Transcript::default();

        assert_eq!(
            render_transcript(&transcript, 24, 80, 0, &[]),
            b"The command completed successfully (exit 0) and produced no visible output."
        );
    }

    #[test]
    fn renders_empty_transcript_with_single_command() {
        let transcript = Transcript::default();
        let commands = vec!["git diff --check".to_string()];

        assert_eq!(
            render_transcript(&transcript, 24, 80, 0, &commands),
            b"The command `git diff --check` completed successfully (exit 0) and produced no visible output."
        );
    }

    #[test]
    fn renders_empty_transcript_with_multiple_commands_and_final_status() {
        let transcript = Transcript::default();
        let commands = vec![
            "zig build test".to_string(),
            "zig build".to_string(),
            "git diff --check".to_string(),
        ];

        assert_eq!(
            render_transcript(&transcript, 24, 80, 0, &commands),
            b"The command block finished with exit 0 and produced no visible output.\nCommands observed:\n- zig build test\n- zig build\n- git diff --check"
        );
    }

    #[test]
    fn renders_empty_transcript_as_failure_status() {
        let transcript = Transcript::default();

        assert_eq!(
            render_transcript(&transcript, 24, 80, 7, &[]),
            b"The command failed (exit 7) and produced no visible output."
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
            end_prefix: b"\x1b]777;CVB;END;".to_vec(),
            pathop: b"\x1b]777;CVB;PATHOP\x07".to_vec(),
            skip: b"\x1b]777;CVB;SKIP\x07".to_vec(),
            command_prefix: b"\x1b]777;CVB;COMMAND;".to_vec(),
            pending: Vec::new(),
        }
    }

    fn process(&mut self, data: &[u8]) -> Vec<MarkerEvent> {
        self.pending.extend_from_slice(data);

        let mut events = Vec::new();

        loop {
            let command_position = find_subslice(&self.pending, &self.command_prefix);
            let marker = [
                (
                    find_subslice(&self.pending, &self.start),
                    self.start.len(),
                    MarkerEvent::Start,
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

            let end_position = find_subslice(&self.pending, &self.end_prefix);

            if let Some(command_position) = command_position
                && marker
                    .as_ref()
                    .is_none_or(|(position, _, _)| command_position <= *position)
                && end_position.is_none_or(|position| command_position <= position)
            {
                if command_position > 0 {
                    events.push(MarkerEvent::Data(
                        self.pending.drain(..command_position).collect(),
                    ));
                    continue;
                }

                let payload_start = self.command_prefix.len();
                let Some(payload_end) = self.pending[payload_start..]
                    .iter()
                    .position(|byte| *byte == b'\x07')
                    .map(|position| payload_start + position)
                else {
                    break;
                };

                let decoded = decode_hex(&self.pending[payload_start..payload_end]);
                self.pending.drain(..=payload_end);

                if let Some(command) = decoded {
                    events.push(MarkerEvent::Command(
                        String::from_utf8_lossy(&command).into_owned(),
                    ));
                }
                continue;
            }

            if let Some(end_position) = end_position
                && marker
                    .as_ref()
                    .is_none_or(|(position, _, _)| end_position <= *position)
            {
                if end_position > 0 {
                    events.push(MarkerEvent::Data(
                        self.pending.drain(..end_position).collect(),
                    ));
                    continue;
                }

                let marker_length = self.end_prefix.len() + 4;
                if self.pending.len() < marker_length {
                    break;
                }

                let status_bytes = &self.pending[self.end_prefix.len()..self.end_prefix.len() + 3];
                let terminator = self.pending[self.end_prefix.len() + 3];

                if status_bytes.iter().all(u8::is_ascii_digit) && terminator == b'\x07' {
                    let status = status_bytes
                        .iter()
                        .fold(0, |value, digit| value * 10 + i32::from(digit - b'0'));
                    self.pending.drain(..marker_length);
                    events.push(MarkerEvent::End(status));
                } else {
                    events.push(MarkerEvent::Data(self.pending.drain(..1).collect()));
                }
                continue;
            }

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
            // An unfinished command or end marker is held back whole.
            let command_unfinished = self.pending.starts_with(&self.command_prefix)
                && !self.pending[self.command_prefix.len()..].contains(&b'\x07');
            let end_unfinished = self.pending.starts_with(&self.end_prefix)
                && self.pending.len() < self.end_prefix.len() + 4;
            let keep = if command_unfinished || end_unfinished {
                self.pending.len()
            } else {
                [
                    self.start.as_slice(),
                    self.end_prefix.as_slice(),
                    self.pathop.as_slice(),
                    self.skip.as_slice(),
                    self.command_prefix.as_slice(),
                ]
                .iter()
                .map(|marker| {
                    (1..=self.pending.len().min(marker.len()))
                        .rev()
                        .find(|length| self.pending.ends_with(&marker[..*length]))
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0)
            };

            if self.pending.len() > keep {
                let split_at = self.pending.len() - keep;
                events.push(MarkerEvent::Data(self.pending.drain(..split_at).collect()));
            }
        }

        events
    }
}

fn decode_hex(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }

    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        decoded.push(((high << 4) | low) as u8);
    }
    Some(decoded)
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
            b"\x1b]777;CVB;START\x07\x1b]777;CVB;PATHOP\x07\x1b]777;CVB;START\x07\x1b]777;CVB;PATHOP\x07\x1b]777;CVB;START\x07\x1b]777;CVB;END;000\x07",
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
            scanner.process(b"\x1b]777;CVB;START\x07\x1b]777;CVB;SKIP\x07\x1b]777;CVB;END;000\x07");

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
    fn recognizes_command_metadata() {
        let mut scanner = MarkerScanner::new();

        assert_eq!(
            scanner.process(
                b"\x1b]777;CVB;COMMAND;6769742064696666202d2d636865636b\x07"
            ),
            vec![MarkerEvent::Command("git diff --check".to_string())]
        );
    }

    #[test]
    fn recognizes_command_metadata_split_across_reads() {
        let marker = b"\x1b]777;CVB;COMMAND;7072696e7466202768656c6c6f2727\x07";
        let mut scanner = MarkerScanner::new();

        assert!(scanner.process(&marker[..23]).is_empty());
        assert_eq!(
            scanner.process(&marker[23..]),
            vec![MarkerEvent::Command("printf 'hello''".to_string())]
        );
    }

    #[test]
    fn ignores_malformed_command_metadata() {
        let mut scanner = MarkerScanner::new();

        assert!(scanner
            .process(b"\x1b]777;CVB;COMMAND;xyz\x07")
            .is_empty());
    }

    #[test]
    fn recognizes_start_and_end_markers() {
        let mut scanner = MarkerScanner::new();

        let mut events =
            scanner.process(b"before\x1b]777;CVB;START\x07ONE\x1b]777;CVB;END;000\x07after");

        events.extend(scanner.process(b"0123456789012345"));

        assert_eq!(
            events,
            vec![
                MarkerEvent::Data(b"before".to_vec()),
                MarkerEvent::Start,
                MarkerEvent::Data(b"ONE".to_vec()),
                MarkerEvent::End(0),
                MarkerEvent::Data(b"after".to_vec()),
                MarkerEvent::Data(b"0123456789012345".to_vec()),
            ]
        );
    }

    #[test]
    fn preserves_data_between_markers() {
        let mut scanner = MarkerScanner::new();

        let events = scanner.process(b"\x1b]777;CVB;START\x07ONE\nTWO\n\x1b]777;CVB;END;000\x07");

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

        let events = scanner.process(b"ART\x07ONE\x1b]777;CVB;END;000\x07");

        assert_eq!(
            events,
            vec![
                MarkerEvent::Start,
                MarkerEvent::Data(b"ONE".to_vec()),
                MarkerEvent::End(0),
            ]
        );
    }

    #[test]
    fn recognizes_nonzero_exit_status() {
        let mut scanner = MarkerScanner::new();

        assert_eq!(
            scanner.process(b"\x1b]777;CVB;END;127\x07"),
            vec![MarkerEvent::End(127)]
        );
    }

    #[test]
    fn recognizes_exit_status_marker_split_across_reads() {
        let marker = b"\x1b]777;CVB;END;017\x07";
        let mut scanner = MarkerScanner::new();

        assert_eq!(scanner.process(&marker[..17]), Vec::<MarkerEvent>::new());
        assert_eq!(scanner.process(&marker[17..]), vec![MarkerEvent::End(17)]);
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

fn capture_terminal_output(
    capture: &mut CaptureState,
    transcript: &mut Transcript,
    output: &[u8],
) {
    if capture.capturing {
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

fn write_to_master(master: &OwnedFd, data: &[u8]) -> Result<(), String> {
    let mut written = 0;
    while written < data.len() {
        match write(master.as_fd(), &data[written..]) {
            Ok(0) => {
                return Err("CVB: PTY write returned zero bytes".to_string());
            }
            Ok(count) => written += count,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => {
                return Err(format!("CVB: PTY write failed: {error}"));
            }
        }
    }
    Ok(())
}

/// What the proxy holds about a pasted patch between keystrokes.
struct PatchState {
    /// `CVB_PATCH_REVIEW=on`: a recognized patch waits for Ctrl+B twice.
    review_on: bool,
    review: Option<crate::patch::review::Review>,
    /// A failed patch whose non-matching hunks the user is placing.
    picker: Option<crate::patch::picker::Picker>,
}

impl PatchState {
    fn new() -> Self {
        Self { review_on: crate::patch::review::enabled(), review: None, picker: None }
    }
}

/// Carries out what the paste recognizer decided, in order. With the review
/// gate on, a patch is shown and held instead of applied.
fn dispatch_paste_events(
    events: Vec<crate::patch::paste::PasteEvent>,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
    state: &mut PatchState,
    command_block: &mut Option<CommandBlockCapture>,
) -> Result<(), String> {
    use crate::patch::deliver;
    use crate::patch::paste::PasteEvent;

    for event in events {
        match event {
            PasteEvent::Forward(bytes) => {
                // Typed after a paste in the same read: it answers the review
                // or the picker.
                let bytes = gate_input(state, bytes, master, child, stdout, screen_terminal)?;
                if !bytes.is_empty() {
                    write_to_master(master, &bytes)?;
                }
            }
            PasteEvent::CommandBlock { body, commands } => {
                let mut bytes =
                    gate_input(state, body, master, child, stdout, screen_terminal)?;
                if bytes.is_empty() {
                    continue;
                }

                if !bytes.ends_with(b"\n") && !bytes.ends_with(b"\r") {
                    bytes.push(b'\n');
                }
                bytes.extend_from_slice(b"__cvb_batch_end\r");
                write_to_master(master, &bytes)?;
                *command_block = Some(CommandBlockCapture {
                    commands,
                    results: Vec::new(),
                });
            }
            PasteEvent::Patch(text) => {
                debug_log("patch_paste", &format!("bytes={} review={}", text.len(), state.review_on));
                // A new patch replaces whatever was waiting.
                state.picker = None;
                if state.review_on {
                    match deliver::review_screen(&text) {
                        Ok(shown) => {
                            show_note(&shown, master, stdout, screen_terminal)?;
                            state.review = Some(crate::patch::review::Review::new(text));
                        }
                        Err(report) => {
                            present_patch_report(&report, master, stdout, screen_terminal)?;
                        }
                    }
                } else {
                    apply_and_present(&text, state, master, child, stdout, screen_terminal)?;
                }
            }
            PasteEvent::Reject(reason) => {
                debug_log("patch_paste_rejected", &format!("reason={reason:?}"));
                present_patch_report(
                    &deliver::rejection_text(reason),
                    master,
                    stdout,
                    screen_terminal,
                )?;
            }
        }
    }
    Ok(())
}

/// Applies a patch and shows the result. A failure caused by hunks that do not
/// match opens the picker, whose first screen follows the report.
fn apply_and_present(
    text: &str,
    state: &mut PatchState,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<(), String> {
    let (report, picker) = crate::patch::deliver::apply_offering_picker(text, child.as_raw());
    present_patch_report(&report, master, stdout, screen_terminal)?;
    if let Some(picker) = picker {
        debug_log("patch_picker_open", &format!("hunks={}", picker.hunk_count()));
        let first_view = picker.first_view();
        let review_bundle = picker.review_bundle();
        state.picker = Some(picker);
        show_note(&first_view, master, stdout, screen_terminal)?;

        let buffer = crate::clipboard::ClipboardBuffer::new(review_bundle.into_bytes());
        match crate::clipboard::process_with_outcome(&buffer) {
            Ok(crate::clipboard::ChunkSequenceOutcome::Completed) => {
                show_note(
                    "CVB patch: all candidate comparisons copied to the clipboard. Paste them here for review.\n",
                    master,
                    stdout,
                    screen_terminal,
                )?;
            }
            Ok(crate::clipboard::ChunkSequenceOutcome::Cancelled) => {
                show_note(
                    "CVB patch: clipboard transfer stopped before all review parts were copied.\n",
                    master,
                    stdout,
                    screen_terminal,
                )?;
            }
            Err(error) => {
                show_note(
                    &format!("CVB patch: could not copy candidate comparisons: {error}\n"),
                    master,
                    stdout,
                    screen_terminal,
                )?;
            }
        }
    }
    Ok(())
}

/// Lets a patch waiting for review, or a picker, see the input first. Returns
/// what is left for the shell: everything when nothing is waiting, what follows
/// the key that answered otherwise, nothing while still waiting.
fn gate_input(
    state: &mut PatchState,
    bytes: Vec<u8>,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<Vec<u8>, String> {
    let bytes = review_input(state, bytes, master, child, stdout, screen_terminal)?;
    if bytes.is_empty() {
        return Ok(bytes);
    }
    picker_input(state, bytes, master, child, stdout, screen_terminal)
}

fn review_input(
    state: &mut PatchState,
    bytes: Vec<u8>,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<Vec<u8>, String> {
    use crate::patch::review::{ARMED_NOTE, CANCELLED_NOTE, Step};

    let Some(active) = state.review.as_mut() else {
        return Ok(bytes);
    };
    match active.feed(&bytes) {
        Step::Waiting { just_armed } => {
            if just_armed {
                show_note(ARMED_NOTE, master, stdout, screen_terminal)?;
            }
            Ok(Vec::new())
        }
        Step::Apply { text, rest } => {
            state.review = None;
            debug_log("patch_review_apply", &format!("bytes={}", text.len()));
            apply_and_present(&text, state, master, child, stdout, screen_terminal)?;
            Ok(rest)
        }
        Step::Cancel { rest } => {
            state.review = None;
            debug_log("patch_review_cancel", "");
            show_note(CANCELLED_NOTE, master, stdout, screen_terminal)?;
            Ok(rest)
        }
    }
}

fn picker_input(
    state: &mut PatchState,
    bytes: Vec<u8>,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<Vec<u8>, String> {
    let Some(active) = state.picker.as_mut() else {
        return Ok(bytes);
    };
    let step = active.feed(&bytes);
    picker_step(state, step, master, child, stdout, screen_terminal)
}

fn picker_step(
    state: &mut PatchState,
    step: crate::patch::picker::Step,
    master: &OwnedFd,
    child: Pid,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<Vec<u8>, String> {
    use crate::patch::deliver;
    use crate::patch::picker::{ARMED_NOTE, CANCELLED_NOTE, CONFIRM_NOTE, Step};

    match step {
        Step::Waiting { just_armed } => {
            if just_armed {
                show_note(ARMED_NOTE, master, stdout, screen_terminal)?;
            }
            Ok(Vec::new())
        }
        Step::Show(view) => {
            show_note(&view, master, stdout, screen_terminal)?;
            Ok(Vec::new())
        }
        Step::Confirm { view, patch } => {
            // Show what the corrected patch is, then what applying it would do.
            // The files must still be the ones the picker saw.
            let Some(expected) = state.picker.as_ref().map(|picker| picker.digests()) else {
                return Ok(Vec::new());
            };
            match deliver::preview_for_shell(&patch, child.as_raw(), &expected) {
                Ok(outcome) => {
                    let shown = format!("{view}{outcome}{CONFIRM_NOTE}\n");
                    show_note(&shown, master, stdout, screen_terminal)?;
                }
                Err(report) => {
                    // The corrected patch does not apply either: drop the picker.
                    state.picker = None;
                    debug_log("patch_picker_preview_failed", "");
                    let shown = format!(
                        "{view}CVB patch: the corrected patch does not apply either; nothing was changed.\n{report}"
                    );
                    present_patch_report(&shown, master, stdout, screen_terminal)?;
                }
            }
            Ok(Vec::new())
        }
        Step::Apply { text, rest } => {
            let Some(expected) = state.picker.take().map(|picker| picker.digests()) else {
                return Ok(rest);
            };
            debug_log("patch_picker_apply", &format!("bytes={}", text.len()));
            // A correction that fails again does not reopen the picker: the
            // report is shown as it is. Files changed since the picker opened
            // are refused.
            let report = deliver::apply_for_shell(&text, child.as_raw(), &expected);
            present_patch_report(&report, master, stdout, screen_terminal)?;
            Ok(rest)
        }
        Step::Cancel { rest } => {
            state.picker = None;
            debug_log("patch_picker_cancel", "");
            show_note(CANCELLED_NOTE, master, stdout, screen_terminal)?;
            Ok(rest)
        }
    }
}

/// Prints text on the terminal (and in CVB's screen model) without copying it.
fn print_screen_text(
    text: &str,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<(), String> {
    let screen = crate::patch::deliver::screen_bytes(text);
    screen_terminal.process(&screen);
    let mut stdout = stdout.lock();
    stdout
        .write_all(&screen)
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("CVB: stdout write failed: {error}"))
}

/// Prints text for the user only, then has the shell redraw its prompt.
fn show_note(
    text: &str,
    master: &OwnedFd,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<(), String> {
    print_screen_text(text, stdout, screen_terminal)?;
    write_to_master(master, crate::patch::deliver::REDRAW_SEQUENCE)
}

/// Shows the result of a pasted patch, copies it, and has the shell redraw its
/// prompt. The text never passes through the shell, so it is not part of any
/// command's transcript and leaves no history entry.
fn present_patch_report(
    text: &str,
    master: &OwnedFd,
    stdout: &io::Stdout,
    screen_terminal: &mut vt100::Parser,
) -> Result<(), String> {
    print_screen_text(text, stdout, screen_terminal)?;

    // A clipboard problem must not end the session; say so and carry on.
    let buffer = crate::clipboard::ClipboardBuffer::new(text.as_bytes().to_vec());
    if let Err(error) = crate::clipboard::process(&buffer) {
        let note = crate::patch::deliver::screen_bytes(&format!(
            "CVB: the result could not be copied to the clipboard: {error}"
        ));
        screen_terminal.process(&note);
        let mut stdout = stdout.lock();
        let _ = stdout.write_all(&note).and_then(|()| stdout.flush());
    }

    write_to_master(master, crate::patch::deliver::REDRAW_SEQUENCE)
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
    let mut commands = Vec::new();
    // Set by the SKIP marker: the finished command line consisted only of
    // commands from the skip list. Consumed by the next END marker.
    let mut skip_requested = false;
    let mut command_block: Option<CommandBlockCapture> = None;
    // Recognizes a patch pasted at an idle prompt (see src/patch/). Everything
    // else typed or pasted goes to the shell unchanged.
    let mut patch_input = crate::patch::input::PatchInput::new(crate::patch::deliver::enabled());
    // The review gate (CVB_PATCH_REVIEW=on) and the failed-hunk picker.
    let mut patch_state = PatchState::new();
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

        let now = Instant::now();
        let picker_timeout = patch_state
            .picker
            .as_ref()
            .and_then(|picker| picker.poll_timeout_ms(now));
        let poll_timeout = match (patch_input.poll_timeout_ms(now), picker_timeout) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(timeout), None) | (None, Some(timeout)) => Some(timeout),
            (None, None) => None,
        };

        match nix::poll::poll(&mut poll_fds, poll_timeout) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(format!("CVB: PTY poll failed: {error}")),
        }

        // A held partial marker (an Escape key) or a stalled patch paste goes to
        // the shell once its deadline has passed.
        let now = Instant::now();
        let due = patch_input.flush_if_due(now);
        if !due.is_empty() {
            dispatch_paste_events(
                due,
                master,
                child,
                &stdout,
                &mut screen_terminal,
                &mut patch_state,
                &mut command_block,
            )?;
        }

        let picker_due = patch_state
            .picker
            .as_mut()
            .and_then(|picker| picker.flush_if_due(now));
        if let Some(step) = picker_due {
            let incoming = picker_step(
                &mut patch_state,
                step,
                master,
                child,
                &stdout,
                &mut screen_terminal,
            )?;
            if !incoming.is_empty() {
                let events = patch_input.feed(&incoming, !capture.capturing, now);
                dispatch_paste_events(
                    events,
                    master,
                    child,
                    &stdout,
                    &mut screen_terminal,
                    &mut patch_state,
                    &mut command_block,
                )?;
            }
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

        for (name, events) in [
            ("stdin", stdin_events),
            ("PTY master", master_events),
            ("signal pipe", signal_events),
            ("control socket", control_events),
            ("keyboard monitor", keyboard_events),
            ("keybind watcher", keybind_events),
        ] {
            if events.contains(nix::poll::PollFlags::POLLNVAL) {
                return Err(format!("CVB: {name} descriptor became invalid (POLLNVAL)"));
            }
        }

        if keybind_events.contains(nix::poll::PollFlags::POLLIN)
            && keybind_watcher.changed()?
        {
            match keybind::load() {
                Ok(bindings) => keyboard.set_bindings(bindings),
                Err(error) => eprintln!("{error}"),
            }
        }

        if keybind_events.intersects(
            nix::poll::PollFlags::POLLERR | nix::poll::PollFlags::POLLHUP,
        ) {
            return Err(format!(
                "CVB: keybind watcher failed: {:?}",
                keybind_events
            ));
        }

        if signal_events.contains(nix::poll::PollFlags::POLLIN) {
            let count = loop {
                match read(signal_read.as_fd(), &mut signal_buffer) {
                    Ok(count) => break count,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(error) => {
                        return Err(format!("CVB: signal pipe read failed: {error}"));
                    }
                }
            };

            if count == 0 {
                return Err("CVB: signal pipe closed unexpectedly".to_string());
            }

            let winsize = resize_pty(master, child)?;
            rows = winsize.ws_row;
            cols = winsize.ws_col;
            screen_terminal
                .screen_mut()
                .set_size(winsize.ws_row, winsize.ws_col);
        }
        if signal_events.intersects(
            nix::poll::PollFlags::POLLERR | nix::poll::PollFlags::POLLHUP,
        ) {
            return Err(format!("CVB: signal pipe failed: {:?}", signal_events));
        }


        if keyboard_events.contains(nix::poll::PollFlags::POLLIN) && keyboard.poll()? {
            capture_screen(&mut screen_terminal)?;

            if let Some(sound) = sound {
                sound.play("screen-capture");
            }
        }
        if keyboard_events.intersects(
            nix::poll::PollFlags::POLLERR | nix::poll::PollFlags::POLLHUP,
        ) {
            return Err(format!(
                "CVB: keyboard monitor failed: {:?}",
                keyboard_events
            ));
        }


        if stdin_events.contains(nix::poll::PollFlags::POLLIN) {
            let count = loop {
                match read(stdin.as_fd(), &mut buffer) {
                    Ok(count) => break count,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(error) => {
                        return Err(format!("CVB: stdin read failed: {error}"));
                    }
                }
            };

            if count == 0 {
                break;
            }

            // A paste only counts as a patch when the shell is at a prompt: not
            // while a command (an editor, `cat`, ssh) is running.
            let idle = !capture.capturing;
            // A patch waiting for review, or a picker, sees the input first.
            let incoming = gate_input(
                &mut patch_state,
                buffer[..count].to_vec(),
                master,
                child,
                &stdout,
                &mut screen_terminal,
            )?;
            if !incoming.is_empty() {
                let events = patch_input.feed(&incoming, idle, Instant::now());
                dispatch_paste_events(
                    events,
                    master,
                    child,
                    &stdout,
                    &mut screen_terminal,
                    &mut patch_state,
                    &mut command_block,
                )?;
            }
        } else if stdin_events.intersects(
            nix::poll::PollFlags::POLLHUP | nix::poll::PollFlags::POLLERR,
        ) {
            break;
        }

        if master_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            let count = loop {
                match read(master.as_fd(), &mut buffer) {
                    Ok(count) => break count,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(nix::errno::Errno::EIO) => break 0,
                    Err(error) => {
                        return Err(format!("CVB: PTY read failed: {error}"));
                    }
                }
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
                        capture_terminal_output(&mut capture, &mut transcript, &output);
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

                    MarkerEvent::Command(command) => {
                        if capture.capturing {
                            commands.push(command);
                        }
                    }

                    MarkerEvent::Start => {
                        let capturing_before = capture.capturing;
                        skip_requested = false;
                        capture.start();
                        commands.clear();

                        debug_log(
                            "marker_start",
                            &format!(
                                "capturing_before={capturing_before} capturing_after={}",
                                capture.capturing
                            ),
                        );
                    }

                    MarkerEvent::End(status) => {
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

                        if let Some(block) = command_block.as_mut() {
                            let sentinel = commands
                                .iter()
                                .any(|command| command == "__cvb_batch_end");

                            if sentinel {
                                let mut report = String::from("Command block results:\n\n");
                                let mut passed = 0usize;
                                let mut failed = 0usize;

                                for (index, (command, (exit_status, rendered))) in block
                                    .commands
                                    .iter()
                                    .zip(block.results.iter())
                                    .enumerate()
                                {
                                    report.push_str(&format!(
                                        "Command {}/{}: {}\n",
                                        index + 1,
                                        block.commands.len(),
                                        command
                                    ));
                                    report.push_str(
                                        &String::from_utf8_lossy(rendered),
                                    );
                                    if !rendered.ends_with(b"\n") {
                                        report.push('\n');
                                    }
                                    if *exit_status == 0 {
                                        passed += 1;
                                        report.push_str("Exit status: 0 (PASS)\n\n");
                                    } else {
                                        failed += 1;
                                        report.push_str(&format!(
                                            "Exit status: {} (FAIL)\n\n",
                                            exit_status
                                        ));
                                    }
                                }

                                if block.results.len() != block.commands.len() {
                                    report.push_str(&format!(
                                        "Accounting mismatch: expected {} command result(s), received {}.\n",
                                        block.commands.len(),
                                        block.results.len()
                                    ));
                                }

                                report.push_str(&format!(
                                    "Command block: {} passed, {} failed.\n",
                                    passed, failed
                                ));

                                let rendered = report.into_bytes();
                                let captured_bytes = rendered.len();
                                let buffer =
                                    crate::clipboard::ClipboardBuffer::new(rendered);
                                debug_log(
                                    "command_block_dispatch",
                                    &format!(
                                        "expected={} completed={} passed={} failed={} captured_bytes={captured_bytes}",
                                        block.commands.len(),
                                        block.results.len(),
                                        passed,
                                        failed
                                    ),
                                );
                                crate::clipboard::process(&buffer)?;
                                command_block = None;
                            } else {
                                let index = block.results.len();
                                let command = block
                                    .commands
                                    .get(index)
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        format!("<unexpected command {}>", index + 1)
                                    });
                                let rendered =
                                    render_transcript(&transcript, rows, cols, status, &commands);
                                block.results.push((status, rendered));
                                debug_log(
                                    "command_block_result",
                                    &format!(
                                        "index={} expected={} status={status} skip_listed={skip_listed}",
                                        index + 1,
                                        block.commands.len()
                                    ),
                                );
                                let _ = command;
                            }

                            transcript = Transcript::default();
                            commands.clear();
                            continue;
                        }

                        // A skip-listed command line with no visible output
                        // leaves the clipboard untouched. Any output, such as
                        // an error message, is captured as usual.
                        if skip_listed && transcript_is_visibly_empty(&transcript, rows, cols) {
                            debug_log("capture_skipped", "reason=skip_listed_no_output");
                            transcript = Transcript::default();
                            commands.clear();
                            continue;
                        }

                        let rendered =
                            render_transcript(&transcript, rows, cols, status, &commands);
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
                        commands.clear();
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
                handle_control_message(message, control, &mut transcript)?;
            }

            if control_events.contains(nix::poll::PollFlags::POLLERR) {
                return Err(format!(
                    "CVB: control socket failed: {:?}",
                    control_events
                ));
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
) -> Result<(), String> {
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
    }

    Ok(())
}

fn capture_screen(terminal: &mut vt100::Parser) -> Result<(), String> {
    terminal.screen_mut().set_scrollback(usize::MAX);

    terminal.screen_mut().set_scrollback(0);

    let text = screen_viewport_to_text(terminal);

    let processed = if text.is_empty() {
        "The captured terminal screen contains no visible text.".to_string()
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
    fn terminal_capture_and_transcript_follow_capture_state() {
        let sample_data = b"some command output";

        // Capturing: both capture and transcript record output.
        let mut capture = super::CaptureState::default();
        capture.start();
        let mut transcript = super::Transcript::default();

        super::capture_terminal_output(
            &mut capture,
            &mut transcript,
            sample_data,
        );

        assert_eq!(capture.end().as_slice(), sample_data);
        assert_eq!(
            transcript.segments,
            vec![super::TranscriptSegment::Terminal(sample_data.to_vec())]
        );

        // Not capturing: neither records output.
        let mut capture = super::CaptureState::default();
        let mut transcript = super::Transcript::default();

        super::capture_terminal_output(
            &mut capture,
            &mut transcript,
            sample_data,
        );

        assert!(capture.end().is_empty());
        assert!(transcript.segments.is_empty());
    }
}

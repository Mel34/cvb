use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DEFAULT_COPY_LIMIT: usize = 9_750;

fn configured_copy_limit() -> usize {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        });

    let Some(config_dir) = config_dir else {
        eprintln!(
            "CVB: unable to determine config directory; using default limit {DEFAULT_COPY_LIMIT}"
        );
        return DEFAULT_COPY_LIMIT;
    };

    load_copy_limit(&config_dir.join("cvb").join("limit"))
}

fn load_copy_limit(path: &Path) -> usize {
    if !path.exists() {
        let Some(parent) = path.parent() else {
            eprintln!("CVB: invalid limit config path; using default limit {DEFAULT_COPY_LIMIT}");
            return DEFAULT_COPY_LIMIT;
        };

        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!(
                "CVB: unable to create limit config directory: {error}; using default limit {DEFAULT_COPY_LIMIT}"
            );
            return DEFAULT_COPY_LIMIT;
        }

        let initial_contents = format!("{}{}", DEFAULT_COPY_LIMIT, char::from(10));
        if let Err(error) = fs::write(path, initial_contents) {
            eprintln!(
                "CVB: unable to initialize limit config: {error}; using default limit {DEFAULT_COPY_LIMIT}"
            );
            return DEFAULT_COPY_LIMIT;
        }
    }

    match fs::read_to_string(path) {
        Ok(contents) => match contents.trim().parse::<usize>() {
            Ok(limit) if limit > 0 => limit,
            _ => {
                eprintln!(
                    "CVB: invalid limit in {}; using default limit {DEFAULT_COPY_LIMIT}",
                    path.display()
                );
                DEFAULT_COPY_LIMIT
            }
        },
        Err(error) => {
            eprintln!(
                "CVB: unable to read {}: {error}; using default limit {DEFAULT_COPY_LIMIT}",
                path.display()
            );
            DEFAULT_COPY_LIMIT
        }
    }
}
fn chunk_prefix(number: usize, total: usize, final_part: bool) -> String {
    if final_part {
        format!(
            "[Part {number}/{total} - FINAL PART]\nThis is the final part ({number}/{total}). All parts have now been transmitted. Please assemble all parts and process the entire request now.\n---\n"
        )
    } else {
        format!(
            "[Part {number}/{total} - DO NOT PROCESS YET]\nThis is part {number} of {total}. Do not analyze, answer, or summarize yet. Simply reply: \"Part {number}/{total} received, awaiting next part.\" and wait for subsequent parts.\n---\n"
        )
    }
}

fn split_chunk_payloads(
    text: &str,
    limit: usize,
    total_guess: usize,
) -> Result<Vec<String>, String> {
    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let number = chunks.len() + 1;
        let intermediate = chunk_prefix(number, total_guess, false);
        let final_prefix = chunk_prefix(number, total_guess, true);
        let overhead = intermediate
            .chars()
            .count()
            .max(final_prefix.chars().count());

        let available = limit
            .checked_sub(overhead)
            .ok_or_else(|| "CVB: chunk framing exceeds the configured limit".to_string())?;

        if available == 0 {
            return Err("CVB: chunk framing leaves no room for content".to_string());
        }

        let mut end = remaining
            .char_indices()
            .nth(available)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());

        if end == 0 {
            return Err("CVB: unable to make progress while splitting capture".to_string());
        }

        if end < remaining.len() {
            let candidate = &remaining[..end];

            if let Some(index) = candidate.rfind('\n') {
                end = index + 1;
            } else if let Some(index) = candidate.rfind(' ') {
                end = index + 1;
            }
        }

        if end == 0 {
            return Err("CVB: unable to make progress while splitting capture".to_string());
        }

        chunks.push(remaining[..end].to_string());
        remaining = &remaining[end..];
    }

    Ok(chunks)
}

fn chunk_messages(text: &str, limit: usize) -> Result<Vec<String>, String> {
    if text.chars().count() <= limit {
        return Ok(vec![text.to_string()]);
    }

    let mut total_guess = 2;

    loop {
        let chunks = split_chunk_payloads(text, limit, total_guess)?;
        let actual_total = chunks.len();

        if actual_total == total_guess {
            return Ok(chunks
                .into_iter()
                .enumerate()
                .map(|(index, chunk)| {
                    let number = index + 1;
                    let prefix = chunk_prefix(number, actual_total, number == actual_total);
                    format!("{prefix}{chunk}")
                })
                .collect());
        }

        total_guess = actual_total;
    }
}

pub(crate) fn process(buffer: &ClipboardBuffer) -> Result<(), String> {
    process_with(
        buffer,
        configured_copy_limit(),
        copy_to_clipboard,
        copy_chunk_sequence,
    )
}

fn process_with<C, S>(
    buffer: &ClipboardBuffer,
    limit: usize,
    mut copy: C,
    mut copy_sequence: S,
) -> Result<(), String>
where
    C: FnMut(&ClipboardBuffer) -> Result<(), String>,
    S: FnMut(&[String]) -> Result<(), String>,
{
    let text = match std::str::from_utf8(buffer.as_bytes()) {
        Ok(text) => text,
        Err(_) if buffer.len() < limit => return copy(buffer),
        Err(_) => {
            return Err(
                "CVB: oversized capture is not valid UTF-8; cannot split it safely".to_string(),
            );
        }
    };

    let chunks = chunk_messages(text, limit)?;

    if chunks.len() == 1 {
        return copy(buffer);
    }

    copy_sequence(&chunks)
}

const ESCAPE_SEQUENCE_TIMEOUT_MS: u16 = 50;
const PROGRESS_BAR_MAX_CELLS: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkKey {
    /// Space: next part; finishes after the last part.
    Space,
    Left,
    Right,
    Cancel,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkMove {
    To(usize),
    Done,
    Cancel,
    Ignore,
}

fn navigate(index: usize, total: usize, key: ChunkKey) -> ChunkMove {
    let has_next = index + 1 < total;

    match key {
        ChunkKey::Space if has_next => ChunkMove::To(index + 1),
        ChunkKey::Space => ChunkMove::Done,
        ChunkKey::Right if has_next => ChunkMove::To(index + 1),
        ChunkKey::Right => ChunkMove::Ignore,
        ChunkKey::Left => ChunkMove::To(index.saturating_sub(1)),
        ChunkKey::Cancel => ChunkMove::Cancel,
        ChunkKey::Other => ChunkMove::Ignore,
    }
}

/// Decodes one key press. `first` is the byte already read; `next` supplies
/// follow-up bytes of an escape sequence and returns `None` when none arrive
/// in time (so a lone Escape is distinguishable from an arrow key).
fn decode_chunk_key(first: u8, mut next: impl FnMut() -> Option<u8>) -> ChunkKey {
    match first {
        b' ' => ChunkKey::Space,
        b'q' | b'Q' | 0x03 => ChunkKey::Cancel,
        0x1b => match next() {
            Some(b'[') => decode_csi(&mut next),
            Some(b'O') => match next() {
                Some(b'C') => ChunkKey::Right,
                Some(b'D') => ChunkKey::Left,
                _ => ChunkKey::Other,
            },
            _ => ChunkKey::Other,
        },
        _ => ChunkKey::Other,
    }
}

fn decode_csi(next: &mut impl FnMut() -> Option<u8>) -> ChunkKey {
    // Parameter and intermediate bytes (0x20..=0x3f) precede the final byte
    // (0x40..=0x7e). Consume the whole sequence so modified or unrelated keys
    // (Ctrl+Right, Up, Delete, ...) never leak their tail bytes as key presses.
    let mut parameters = 0;

    loop {
        match next() {
            Some(b'C') if parameters == 0 => return ChunkKey::Right,
            Some(b'D') if parameters == 0 => return ChunkKey::Left,
            Some(0x20..=0x3f) if parameters < 16 => parameters += 1,
            _ => return ChunkKey::Other,
        }
    }
}

fn progress_bar(index: usize, total: usize) -> String {
    let cells = total.clamp(1, PROGRESS_BAR_MAX_CELLS);
    let current = if total <= PROGRESS_BAR_MAX_CELLS {
        index
    } else {
        index * cells / total
    };

    (0..cells)
        .map(|cell| match cell.cmp(&current) {
            std::cmp::Ordering::Less => '=',
            std::cmp::Ordering::Equal => 'O',
            std::cmp::Ordering::Greater => '-',
        })
        .collect()
}

fn chunk_status_line(index: usize, total: usize) -> String {
    let bar = progress_bar(index, total);
    let number = index + 1;

    if number == total {
        format!("[{bar}]({number}/{total}) \u{2190} back, Space/q done")
    } else {
        format!("[{bar}]({number}/{total}) \u{2190}/\u{2192} Space next, q cancel")
    }
}

fn read_stdin_byte() -> Result<u8, String> {
    use std::os::fd::{FromRawFd, RawFd};

    // Read fd 0 directly: std's buffered Stdin could swallow the tail of an
    // escape sequence, which would then be invisible to the poll() timeout.
    let mut stdin = std::mem::ManuallyDrop::new(unsafe { fs::File::from_raw_fd(0 as RawFd) });
    let mut byte = [0u8; 1];

    loop {
        match stdin.read(&mut byte) {
            Ok(1) => return Ok(byte[0]),
            Ok(_) => {
                return Err("CVB: unable to read chunk control key: end of input".to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(format!("CVB: unable to read chunk control key: {error}"));
            }
        }
    }
}

fn read_stdin_byte_within(timeout_ms: u16) -> Option<u8> {
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let mut fds = [nix::poll::PollFd::new(
        stdin.as_fd(),
        nix::poll::PollFlags::POLLIN,
    )];

    match nix::poll::poll(&mut fds, timeout_ms) {
        Ok(ready) if ready > 0 => read_stdin_byte().ok(),
        _ => None,
    }
}

/// Switches a cooked terminal to unbuffered, no-echo input for the duration
/// of the chunk sequence, and restores it on drop. When CVB runs inside its
/// own raw-mode pty wrapper the terminal is already non-canonical, so nothing
/// is changed. ISIG is cleared so Ctrl+C arrives as a key and the terminal is
/// always restored by `Drop` rather than left half-configured by a signal.
struct ChunkKeyTerminal {
    fd: std::os::fd::OwnedFd,
    original: nix::sys::termios::Termios,
}

impl ChunkKeyTerminal {
    fn enter_on(fd: std::os::fd::BorrowedFd<'_>) -> Option<Self> {
        use nix::sys::termios::{self, LocalFlags, SetArg, SpecialCharacterIndices};

        let original = termios::tcgetattr(fd).ok()?;

        if !original.local_flags.contains(LocalFlags::ICANON) {
            return None;
        }

        let mut unbuffered = original.clone();
        unbuffered
            .local_flags
            .remove(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        unbuffered.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        unbuffered.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;

        let owned = fd.try_clone_to_owned().ok()?;
        // TCSAFLUSH drops anything typed before the prompt appeared, such as
        // the Enter that launched the command.
        termios::tcsetattr(fd, SetArg::TCSAFLUSH, &unbuffered).ok()?;

        Some(Self {
            fd: owned,
            original,
        })
    }
}

impl Drop for ChunkKeyTerminal {
    fn drop(&mut self) {
        use std::os::fd::AsFd;

        let _ = nix::sys::termios::tcsetattr(
            self.fd.as_fd(),
            nix::sys::termios::SetArg::TCSANOW,
            &self.original,
        );
    }
}

fn copy_chunk_sequence(chunks: &[String]) -> Result<(), String> {
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let _terminal = ChunkKeyTerminal::enter_on(stdin.as_fd());

    copy_chunk_sequence_with(chunks, copy_to_clipboard, || {
        let first = read_stdin_byte()?;
        Ok(decode_chunk_key(first, || {
            read_stdin_byte_within(ESCAPE_SEQUENCE_TIMEOUT_MS)
        }))
    })
}

fn clear_chunk_status() -> Result<(), String> {
    eprint!("\r\x1b[2K");
    std::io::stderr()
        .flush()
        .map_err(|error| format!("CVB: unable to clear chunk status: {error}"))
}

fn copy_chunk_sequence_with<C, R>(
    chunks: &[String],
    mut copy: C,
    mut read_key: R,
) -> Result<(), String>
where
    C: FnMut(&ClipboardBuffer) -> Result<(), String>,
    R: FnMut() -> Result<ChunkKey, String>,
{
    let total = chunks.len();
    let mut index = 0;

    if total == 0 {
        return Ok(());
    }

    loop {
        copy(&ClipboardBuffer::new(chunks[index].as_bytes().to_vec()))?;

        eprint!("\r\x1b[2K{}", chunk_status_line(index, total));
        std::io::stderr()
            .flush()
            .map_err(|error| format!("CVB: unable to update chunk status: {error}"))?;

        let movement = loop {
            match navigate(index, total, read_key()?) {
                ChunkMove::Ignore => {}
                other => break other,
            }
        };

        match movement {
            ChunkMove::To(target) => index = target,
            ChunkMove::Done | ChunkMove::Cancel => return clear_chunk_status(),
            ChunkMove::Ignore => unreachable!("ignored keys are consumed above"),
        }
    }
}

impl ClipboardBuffer {
    pub(crate) fn new(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.contents.len()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.contents
    }
}

pub(crate) const CLIPBOARD_READ_CAP: usize = 5 * 1024 * 1024;
pub(crate) const CLIPBOARD_STDERR_CAP: usize = 4 * 1024;

pub(crate) fn read_from_clipboard() -> Result<Vec<u8>, String> {
    let child = std::process::Command::new("wl-paste")
        .arg("--no-newline")
        .arg("--type")
        .arg("text/plain")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-paste: {error}"))?;

    drain_child_pipes(child, "wl-paste")
}

fn set_pipe_nonblocking(fd: std::os::fd::RawFd) -> Result<(), String> {
    // SAFETY: fcntl operates on the valid file descriptor owned by a live child pipe.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(format!(
            "CVB: unable to inspect clipboard pipe flags: {}",
            std::io::Error::last_os_error()
        ));
    }

    // SAFETY: F_SETFL updates the flags on the same live file descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(format!(
            "CVB: unable to set clipboard pipe nonblocking: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(())
}

fn drain_child_pipes(
    mut child: std::process::Child,
    process_name: &str,
) -> Result<Vec<u8>, String> {
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd;

    let mut stdout_pipe = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("CVB: {process_name} stdout pipe was not created"));
        }
    };
    let mut stderr_pipe = match child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("CVB: {process_name} stderr pipe was not created"));
        }
    };

    let result = (|| -> Result<(Vec<u8>, Vec<u8>, bool), String> {
        set_pipe_nonblocking(stdout_pipe.as_raw_fd())?;
        set_pipe_nonblocking(stderr_pipe.as_raw_fd())?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut scratch = [0_u8; 8192];

        while !stdout_eof || !stderr_eof {
            let mut fds = [
                libc::pollfd {
                    fd: if stdout_eof {
                        -1
                    } else {
                        stdout_pipe.as_raw_fd()
                    },
                    events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                    revents: 0,
                },
                libc::pollfd {
                    fd: if stderr_eof {
                        -1
                    } else {
                        stderr_pipe.as_raw_fd()
                    },
                    events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                    revents: 0,
                },
            ];

            // SAFETY: fds points to two initialized pollfd entries for live pipes.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
            if ready == -1 {
                let error = std::io::Error::last_os_error();
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("CVB: polling {process_name} pipes failed: {error}"));
            }

            if fds.iter().any(|fd| fd.revents & libc::POLLNVAL != 0) {
                return Err(format!(
                    "CVB: invalid pipe descriptor while reading {process_name}"
                ));
            }

            if !stdout_eof && fds[0].revents != 0 {
                loop {
                    match stdout_pipe.read(&mut scratch) {
                        Ok(0) => {
                            stdout_eof = true;
                            break;
                        }
                        Ok(n) => {
                            let remaining = (CLIPBOARD_READ_CAP + 1).saturating_sub(stdout.len());
                            stdout.extend_from_slice(&scratch[..n.min(remaining)]);
                            if stdout.len() > CLIPBOARD_READ_CAP {
                                return Ok((stdout, stderr, true));
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        Err(error) => {
                            return Err(format!(
                                "CVB: reading {process_name} stdout failed: {error}"
                            ));
                        }
                    }
                }
            }

            if !stderr_eof && fds[1].revents != 0 {
                loop {
                    match stderr_pipe.read(&mut scratch) {
                        Ok(0) => {
                            stderr_eof = true;
                            break;
                        }
                        Ok(n) => {
                            let remaining = (CLIPBOARD_STDERR_CAP + 1).saturating_sub(stderr.len());
                            stderr.extend_from_slice(&scratch[..n.min(remaining)]);
                            // Keep draining stderr even after its diagnostic buffer is full.
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                        Err(error) => {
                            return Err(format!(
                                "CVB: reading {process_name} stderr failed: {error}"
                            ));
                        }
                    }
                }
            }
        }

        Ok((stdout, stderr, false))
    })();

    let (stdout, stderr, oversized) = match result {
        Ok(result) => result,
        Err(error) => {
            drop(stdout_pipe);
            drop(stderr_pipe);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };

    drop(stdout_pipe);
    drop(stderr_pipe);

    if oversized {
        // Stop the producer, then always attempt to reap it. If kill fails
        // because the process already exited, wait() still resolves its status.
        let _ = child.kill();
        child
            .wait()
            .map_err(|error| format!("CVB: unable to reap oversized {process_name}: {error}"))?;

        return Ok(stdout);
    }

    let status = child
        .wait()
        .map_err(|error| format!("CVB: unable to wait for {process_name}: {error}"))?;

    if !status.success() {
        let code = status
            .code()
            .map_or("signal".to_string(), |c| c.to_string());
        let stderr = String::from_utf8_lossy(&stderr);
        let detail = if stderr.trim().is_empty() {
            format!("{process_name} exited with status {code}")
        } else {
            format!(
                "{process_name} exited with status {code}: {}",
                stderr.trim()
            )
        };
        return Err(format!("CVB: unable to read clipboard: {detail}"));
    }

    Ok(stdout)
}

#[cfg(test)]
pub(crate) fn read_from_clipboard_with<F>(mut run_cmd: F) -> Result<Vec<u8>, String>
where
    F: FnMut() -> Result<std::process::Output, String>,
{
    let output = run_cmd()?;

    // Keep the test seam bounded too. The extra byte signals the caller that
    // the clipboard exceeded the capture cap.
    if output.stdout.len() > CLIPBOARD_READ_CAP {
        return Ok(output.stdout[..=CLIPBOARD_READ_CAP].to_vec());
    }

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map_or("signal".to_string(), |c| c.to_string());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            format!("wl-paste exited with status {code}")
        } else {
            format!("wl-paste exited with status {code}: {}", stderr.trim())
        };
        return Err(format!("CVB: unable to read clipboard: {detail}"));
    }

    Ok(output.stdout)
}

pub(crate) fn copy_to_clipboard(buffer: &ClipboardBuffer) -> Result<(), String> {
    let mut child = std::process::Command::new("wl-copy")
        .arg("--type")
        .arg("text/plain")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(buffer.as_bytes())
            .map_err(|error| format!("CVB: unable to write to wl-copy: {error}"))?;
    }

    let status = child
        .wait()
        .map_err(|error| format!("CVB: unable to wait for wl-copy: {error}"))?;

    if !status.success() {
        return Err(format!("CVB: wl-copy exited with status {status}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{read_from_clipboard_with, ClipboardBuffer};

    #[test]
    fn read_from_clipboard_success() {
        let bytes = read_from_clipboard_with(|| {
            Ok(std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: b"clipboard content\n".to_vec(),
                stderr: Vec::new(),
            })
        })
        .unwrap();
        assert_eq!(bytes, b"clipboard content\n");
    }

    #[test]
    fn read_from_clipboard_propagates_failure() {
        let err = read_from_clipboard_with(|| {
            Ok(std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
                stdout: Vec::new(),
                stderr: b"Nothing copied".to_vec(),
            })
        })
        .unwrap_err();
        assert!(err.contains("Nothing copied"));
    }

    #[test]
    fn read_from_clipboard_bounds_oversized_stdout() {
        let cap = super::CLIPBOARD_READ_CAP;
        let bytes = read_from_clipboard_with(|| {
            Ok(std::process::Output {
                status: std::os::unix::process::ExitStatusExt::from_raw(0),
                stdout: vec![b'x'; cap + 100],
                stderr: Vec::new(),
            })
        })
        .unwrap();

        assert_eq!(bytes.len(), cap + 1);
        assert!(bytes.iter().all(|&byte| byte == b'x'));
    }

    #[test]
    fn drain_child_pipes_drains_large_stderr_without_deadlock() {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("head -c 1048576 /dev/zero >&2; printf 'clipboard-ok'")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::drain_child_pipes(child, "test-child").unwrap();
        assert_eq!(output, b"clipboard-ok");
    }

    #[test]
    fn drain_child_pipes_stops_oversized_stdout() {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exec yes x")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let output = super::drain_child_pipes(child, "test-child").unwrap();

        assert_eq!(output.len(), super::CLIPBOARD_READ_CAP + 1);
        assert!(output.iter().all(|&byte| byte == b'x' || byte == b'\n'));
    }

    #[test]
    fn drain_child_pipes_reports_nonzero_exit_and_stderr() {
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("printf 'clipboard unavailable' >&2; exit 7")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let error = super::drain_child_pipes(child, "test-child").unwrap_err();

        assert!(error.contains("unable to read clipboard"), "{error}");
        assert!(error.contains("status 7"), "{error}");
        assert!(error.contains("clipboard unavailable"), "{error}");
    }

    fn temporary_limit_path(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        std::env::temp_dir()
            .join(format!("cvb-limit-{label}-{}-{nonce}", std::process::id()))
            .join("cvb")
            .join("limit")
    }

    #[test]
    fn limit_config_initializes_default_with_newline() {
        let path = temporary_limit_path("init");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        assert_eq!(super::load_copy_limit(&path), 9_750);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "9750\n");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limit_config_reads_custom_value() {
        let path = temporary_limit_path("custom");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "4321\n").unwrap();

        assert_eq!(super::load_copy_limit(&path), 4_321);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limit_config_invalid_value_uses_default() {
        let path = temporary_limit_path("invalid");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-a-number\n").unwrap();

        assert_eq!(super::load_copy_limit(&path), 9_750);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn chunk_messages_leave_small_input_unchanged() {
        let input = "hello 🦀\nworld";
        let chunks = super::chunk_messages(input, 100).unwrap();

        assert_eq!(chunks, vec![input]);
    }

    #[test]
    fn chunk_messages_reconstruct_original_text_exactly() {
        let input = "line one with spaces\né 🧑‍💻\n".repeat(200);
        let chunks = super::chunk_messages(&input, 300).unwrap();
        let total = chunks.len();

        assert!(total > 1);

        let mut reconstructed = String::new();

        for (index, chunk) in chunks.iter().enumerate() {
            let number = index + 1;
            let final_part = number == total;

            assert!(
                chunk.chars().count() <= 300,
                "part {number}/{total} exceeds the configured character limit"
            );

            let (header, payload) = chunk
                .split_once("---\n")
                .unwrap_or_else(|| panic!("part {number}/{total} has no framing separator"));

            assert!(
                header.starts_with(&format!("[Part {number}/{total} - ")),
                "part {number}/{total} has incorrect numbering"
            );

            if final_part {
                assert!(header.contains("FINAL PART"));
                assert!(!header.contains("DO NOT PROCESS YET"));
            } else {
                assert!(header.contains("DO NOT PROCESS YET"));
                assert!(!header.contains("FINAL PART"));
            }

            reconstructed.push_str(payload);
        }

        assert_eq!(
            reconstructed.as_bytes(),
            input.as_bytes(),
            "reassembling the payloads must reproduce the original bytes exactly"
        );
    }

    #[test]
    fn chunk_messages_preserve_edge_case_payloads_exactly() {
        let inputs = [
            "é🧑‍💻漢字".repeat(500),
            "\n\n\nline\n\n".repeat(100),
            "   leading and trailing spaces   \n".repeat(100),
            "x".repeat(2_000),
            "line one\nline two\n".repeat(200),
        ];

        for input in inputs {
            let chunks = super::chunk_messages(&input, 300).unwrap();
            assert!(chunks.len() > 1);

            let reconstructed = chunks
                .iter()
                .map(|chunk| {
                    chunk
                        .split_once("---\n")
                        .expect("chunk must contain framing separator")
                        .1
                })
                .collect::<String>();

            assert_eq!(
                reconstructed.as_bytes(),
                input.as_bytes(),
                "chunk reconstruction changed the original payload"
            );

            assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 300));
        }
    }

    #[test]
    fn chunk_messages_prefer_newline_boundaries() {
        let input = format!("{}\n{}", "a".repeat(40), "b".repeat(600));
        let chunks = super::chunk_messages(&input, 300).unwrap();

        assert!(chunks.len() > 1);
        let first_payload = chunks[0].split_once("---\n").unwrap().1;
        assert!(first_payload.ends_with("\n"));
    }

    #[test]
    fn chunk_messages_prefer_spaces_when_no_newline_fits() {
        let input = format!("{} {}", "a".repeat(40), "b".repeat(600));
        let chunks = super::chunk_messages(&input, 300).unwrap();

        assert!(chunks.len() > 1);
        let first_payload = chunks[0].split_once("---\n").unwrap().1;
        assert!(first_payload.ends_with(' '));
    }

    #[test]
    fn chunk_messages_reject_limits_too_small_for_framing() {
        let error = super::chunk_messages(&"x".repeat(100), 10).unwrap_err();

        assert!(error.contains("framing"));
    }

    #[test]
    fn buffer_owns_exact_bytes() {
        let contents = vec![0x00, 0x41, 0xFF];
        let buffer = ClipboardBuffer::new(contents.clone());

        assert_eq!(buffer.as_bytes(), contents.as_slice());
        assert_eq!(buffer.len(), contents.len());
    }

    #[test]
    fn buffer_length_counts_utf8_bytes() {
        let buffer = ClipboardBuffer::new("é".as_bytes().to_vec());

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.as_bytes(), "é".as_bytes());
    }

    #[test]
    fn chunk_sequence_copies_every_part_and_reconstructs_original() {
        let input = "Unicode 🦀 and whitespace\n\n".repeat(100);
        let chunks = super::chunk_messages(&input, 300).unwrap();
        let expected_chunks = chunks.clone();
        let mut copied = Vec::<Vec<u8>>::new();
        let mut keys = vec![super::ChunkKey::Space; chunks.len()].into_iter();

        super::copy_chunk_sequence_with(
            &chunks,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            || {
                Ok(keys
                    .next()
                    .expect("a key should advance each part and finish the last"))
            },
        )
        .unwrap();

        assert_eq!(
            copied,
            expected_chunks
                .iter()
                .map(|chunk| chunk.as_bytes().to_vec())
                .collect::<Vec<_>>(),
            "the clipboard must receive every framed chunk in order"
        );

        let reconstructed = copied
            .iter()
            .map(|chunk| {
                std::str::from_utf8(chunk)
                    .unwrap()
                    .split_once("---\n")
                    .expect("each copied chunk must have a framing separator")
                    .1
            })
            .collect::<String>();

        assert_eq!(reconstructed.as_bytes(), input.as_bytes());
        assert_eq!(keys.next(), None);
    }

    #[test]
    fn chunk_sequence_cancellation_stops_after_current_part() {
        for raw_key in [b'q', b'Q', 0x03] {
            let cancel_key = super::decode_chunk_key(raw_key, || None);
            assert_eq!(cancel_key, super::ChunkKey::Cancel);
            let chunks = vec![
                "first framed chunk".to_string(),
                "second framed chunk".to_string(),
                "third framed chunk".to_string(),
            ];
            let mut copied = Vec::<Vec<u8>>::new();

            super::copy_chunk_sequence_with(
                &chunks,
                |buffer| {
                    copied.push(buffer.as_bytes().to_vec());
                    Ok(())
                },
                || Ok(cancel_key),
            )
            .unwrap();

            assert_eq!(
                copied,
                vec![chunks[0].as_bytes().to_vec()],
                "cancel key {cancel_key:?} must prevent later chunks from being copied"
            );
        }
    }

    #[test]
    fn chunk_sequence_ignores_unrecognized_keys() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
            "third framed chunk".to_string(),
        ];
        let mut copied = Vec::<Vec<u8>>::new();
        let mut keys = (*b"x\n !  ")
            .into_iter()
            .map(|key| super::decode_chunk_key(key, || None));

        super::copy_chunk_sequence_with(
            &chunks,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            || Ok(keys.next().expect("expected scripted control key")),
        )
        .unwrap();

        assert_eq!(
            copied,
            chunks
                .iter()
                .map(|chunk| chunk.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn chunk_sequence_propagates_key_read_failure_without_advancing() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
        ];
        let mut copy_attempts = 0;

        let result = super::copy_chunk_sequence_with(
            &chunks,
            |_| {
                copy_attempts += 1;
                Ok(())
            },
            || Err("simulated key read failure".to_string()),
        );

        assert_eq!(result.unwrap_err(), "simulated key read failure");
        assert_eq!(copy_attempts, 1);
    }

    fn key_from(bytes: &[u8]) -> super::ChunkKey {
        let mut rest = bytes[1..].iter().copied();
        super::decode_chunk_key(bytes[0], || rest.next())
    }

    #[test]
    fn decoder_recognizes_arrow_keys_in_both_cursor_modes() {
        assert_eq!(key_from(b"\x1b[C"), super::ChunkKey::Right);
        assert_eq!(key_from(b"\x1b[D"), super::ChunkKey::Left);
        assert_eq!(key_from(b"\x1bOC"), super::ChunkKey::Right);
        assert_eq!(key_from(b"\x1bOD"), super::ChunkKey::Left);
        assert_eq!(key_from(b" "), super::ChunkKey::Space);
    }

    #[test]
    fn decoder_treats_lone_escape_and_other_sequences_as_other() {
        assert_eq!(key_from(b"\x1b"), super::ChunkKey::Other);
        assert_eq!(key_from(b"\x1b[A"), super::ChunkKey::Other);
        assert_eq!(key_from(b"\x1b[B"), super::ChunkKey::Other);
        assert_eq!(key_from(b"\x1b[1;5C"), super::ChunkKey::Other);
        assert_eq!(key_from(b"\x1b[3~"), super::ChunkKey::Other);
    }

    #[test]
    fn decoder_consumes_whole_unrecognized_sequences() {
        // Ctrl+Right is ESC [ 1 ; 5 C; the trailing bytes must not remain.
        let bytes = b"\x1b[1;5C q";
        let mut rest = bytes[1..].iter().copied();
        let key = super::decode_chunk_key(bytes[0], || rest.next());
        assert_eq!(key, super::ChunkKey::Other);
        assert_eq!(rest.collect::<Vec<_>>(), b" q");
    }

    #[test]
    fn navigate_moves_within_bounds() {
        use super::{ChunkKey as K, ChunkMove as M, navigate};

        assert_eq!(navigate(0, 4, K::Space), M::To(1));
        assert_eq!(navigate(0, 4, K::Right), M::To(1));
        assert_eq!(navigate(2, 4, K::Left), M::To(1));
        assert_eq!(
            navigate(0, 4, K::Left),
            M::To(0),
            "left on first part re-copies it"
        );
        assert_eq!(
            navigate(3, 4, K::Right),
            M::Ignore,
            "right clamps on the last part"
        );
        assert_eq!(
            navigate(3, 4, K::Space),
            M::Done,
            "space finishes on the last part"
        );
        assert_eq!(navigate(1, 4, K::Cancel), M::Cancel);
        assert_eq!(navigate(1, 4, K::Other), M::Ignore);
    }

    #[test]
    fn chunk_sequence_left_recopies_previous_part_after_accidental_space() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
            "third framed chunk".to_string(),
        ];
        let mut copied = Vec::<Vec<u8>>::new();
        let mut keys = [
            super::ChunkKey::Space, // -> part 2
            super::ChunkKey::Space, // -> part 3 (user did not paste part 2)
            super::ChunkKey::Left,  // -> part 2 again
            super::ChunkKey::Right, // -> part 3 again
            super::ChunkKey::Right, // clamped, ignored
            super::ChunkKey::Space, // finish
        ]
        .into_iter();

        super::copy_chunk_sequence_with(
            &chunks,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            || Ok(keys.next().expect("expected scripted key")),
        )
        .unwrap();

        let order: Vec<&str> = copied
            .iter()
            .map(|bytes| std::str::from_utf8(bytes).unwrap())
            .collect();
        assert_eq!(
            order,
            [
                "first framed chunk",
                "second framed chunk",
                "third framed chunk",
                "second framed chunk",
                "third framed chunk",
            ]
        );
        assert_eq!(keys.next(), None);
    }

    #[test]
    fn progress_bar_marks_done_current_and_pending_parts() {
        assert_eq!(super::progress_bar(0, 8), "O-------");
        assert_eq!(super::progress_bar(2, 8), "==O-----");
        assert_eq!(super::progress_bar(7, 8), "=======O");
    }

    #[test]
    fn progress_bar_scales_to_a_bounded_width() {
        let bar = super::progress_bar(50, 100);
        assert_eq!(bar.chars().count(), super::PROGRESS_BAR_MAX_CELLS);
        assert_eq!(bar.chars().filter(|c| *c == 'O').count(), 1);
        assert!(bar.starts_with('='));
        assert!(bar.ends_with('-'));
        assert!(super::progress_bar(99, 100).ends_with('O'));
    }

    #[test]
    fn status_line_shows_position_and_hints() {
        let middle = super::chunk_status_line(2, 8);
        assert!(middle.starts_with("[==O-----](3/8)"));
        assert!(middle.contains("Space next"));

        let last = super::chunk_status_line(7, 8);
        assert!(last.starts_with("[=======O](8/8)"));
        assert!(last.contains("back"));
    }

    #[test]
    fn chunk_key_terminal_disables_line_buffering_and_restores_it() {
        use nix::sys::termios::{self, LocalFlags};
        use std::os::fd::AsFd;

        let pty = nix::pty::openpty(None, None).unwrap();
        let slave = pty.slave;

        let before = termios::tcgetattr(slave.as_fd()).unwrap();
        assert!(before.local_flags.contains(LocalFlags::ICANON));
        assert!(before.local_flags.contains(LocalFlags::ECHO));

        {
            let guard = super::ChunkKeyTerminal::enter_on(slave.as_fd())
                .expect("a cooked terminal must be switched");
            let during = termios::tcgetattr(slave.as_fd()).unwrap();
            assert!(!during.local_flags.contains(LocalFlags::ICANON));
            assert!(!during.local_flags.contains(LocalFlags::ECHO));
            assert!(!during.local_flags.contains(LocalFlags::ISIG));

            // An already non-canonical terminal (CVB's own raw pty wrapper)
            // must be left alone.
            assert!(super::ChunkKeyTerminal::enter_on(slave.as_fd()).is_none());
            drop(guard);
        }

        let after = termios::tcgetattr(slave.as_fd()).unwrap();
        assert_eq!(after.local_flags, before.local_flags);
    }

    #[test]
    fn process_copies_small_valid_text_unchanged() {
        let buffer = ClipboardBuffer::new(b"hello".to_vec());
        let mut copied = Vec::new();
        let mut sequence_calls = 0;

        super::process_with(
            &buffer,
            100,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            |_| {
                sequence_calls += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(copied, vec![b"hello".to_vec()]);
        assert_eq!(sequence_calls, 0);
    }

    #[test]
    fn process_sends_oversized_valid_text_to_chunk_sequence() {
        let input = "x".repeat(1_000);
        let buffer = ClipboardBuffer::new(input.as_bytes().to_vec());
        let mut copied = Vec::new();
        let mut received_chunks = Vec::new();

        super::process_with(
            &buffer,
            300,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            |chunks| {
                received_chunks = chunks.to_vec();
                Ok(())
            },
        )
        .unwrap();

        assert!(copied.is_empty());
        assert!(received_chunks.len() > 1);

        let reconstructed = received_chunks
            .iter()
            .map(|chunk| {
                chunk
                    .split_once("---\n")
                    .expect("each chunk must contain its framing separator")
                    .1
            })
            .collect::<String>();

        assert_eq!(reconstructed.as_bytes(), input.as_bytes());
    }

    #[test]
    fn process_copies_small_invalid_utf8_unchanged() {
        let bytes = vec![0xff, 0x00, 0x41];
        let buffer = ClipboardBuffer::new(bytes.clone());
        let mut copied = Vec::new();
        let mut sequence_calls = 0;

        super::process_with(
            &buffer,
            10,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            |_| {
                sequence_calls += 1;
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(copied, vec![bytes]);
        assert_eq!(sequence_calls, 0);
    }

    #[test]
    fn process_rejects_oversized_invalid_utf8_without_copying() {
        let buffer = ClipboardBuffer::new(vec![0xff; 10]);
        let mut copy_calls = 0;
        let mut sequence_calls = 0;

        let result = super::process_with(
            &buffer,
            10,
            |_| {
                copy_calls += 1;
                Ok(())
            },
            |_| {
                sequence_calls += 1;
                Ok(())
            },
        );

        assert_eq!(
            result.unwrap_err(),
            "CVB: oversized capture is not valid UTF-8; cannot split it safely"
        );
        assert_eq!(copy_calls, 0);
        assert_eq!(sequence_calls, 0);
    }

    #[test]
    fn process_propagates_direct_copy_failure() {
        let buffer = ClipboardBuffer::new(b"hello".to_vec());
        let mut sequence_calls = 0;

        let result = super::process_with(
            &buffer,
            100,
            |_| Err("simulated direct-copy failure".to_string()),
            |_| {
                sequence_calls += 1;
                Ok(())
            },
        );

        assert_eq!(result.unwrap_err(), "simulated direct-copy failure");
        assert_eq!(sequence_calls, 0);
    }

    #[test]
    fn process_propagates_chunk_sequence_failure() {
        let buffer = ClipboardBuffer::new("x".repeat(1_000).into_bytes());
        let mut copy_calls = 0;
        let mut sequence_calls = 0;

        let result = super::process_with(
            &buffer,
            300,
            |_| {
                copy_calls += 1;
                Ok(())
            },
            |_| {
                sequence_calls += 1;
                Err("simulated chunk-sequence failure".to_string())
            },
        );

        assert_eq!(result.unwrap_err(), "simulated chunk-sequence failure");
        assert_eq!(copy_calls, 0);
        assert_eq!(sequence_calls, 1);
    }

    #[test]
    fn chunk_sequence_propagates_copy_failure_without_advancing() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
        ];
        let mut copy_attempts = 0;
        let mut key_reads = 0;

        let result = super::copy_chunk_sequence_with(
            &chunks,
            |_| {
                copy_attempts += 1;
                Err("simulated clipboard failure".to_string())
            },
            || {
                key_reads += 1;
                Ok(super::ChunkKey::Space)
            },
        );

        assert_eq!(result.unwrap_err(), "simulated clipboard failure");
        assert_eq!(copy_attempts, 1);
        assert_eq!(key_reads, 0);
    }
}

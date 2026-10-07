use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::num::NonZeroU16;

use nix::ioctl_read_bad;
use nix::ioctl_write_ptr_bad;
use nix::pty::{forkpty, ForkptyResult, Winsize};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{pipe, read, write, Pid};

use crate::control::{ControlChannel, ControlMessage};
use crate::keyboard::HotkeyMonitor;
use crate::keybind::{self, KeybindWatcher};
use crate::sound::SoundPlayer;

ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);
ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

const CONTROL_FD: i32 = 3;
const NO_OUTPUT: &str = "[NO_VISIBLE_OUTPUT]";

static mut SIGWINCH_PIPE: Option<i32> = None;

extern "C" fn handle_sigwinch(_: libc::c_int) {
    unsafe {
        if let Some(fd) = SIGWINCH_PIPE {
            let _ = libc::write(fd, &[1u8] as *const u8 as *const libc::c_void, 1);
        }
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

            unsafe {
                libc::setenv(
                    b"PAGER\0".as_ptr() as *const libc::c_char,
                    b"cat\0".as_ptr() as *const libc::c_char,
                    1,
                );
                libc::setenv(
                    b"GIT_PAGER\0".as_ptr() as *const libc::c_char,
                    b"cat\0".as_ptr() as *const libc::c_char,
                    1,
                );
                libc::setenv(
                    b"SYSTEMD_PAGER\0".as_ptr() as *const libc::c_char,
                    b"cat\0".as_ptr() as *const libc::c_char,
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

            unsafe {
                SIGWINCH_PIPE = None;

                signal::signal(Signal::SIGWINCH, SigHandler::SigDfl)
                    .map_err(|error| format!("CVB: unable to restore SIGWINCH handler: {error}"))?;
            }

            result?;

            let status = waitpid(child, None)
                .map_err(|error| format!("CVB: unable to wait for child: {error}"))?;

            Ok(status_to_code(status))
        }
    }
}

fn control_socketpair() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [0; 2];

    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM,
            0,
            fds.as_mut_ptr(),
        )
    };

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

    let result = unsafe {
        libc::fcntl(
            fd.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        )
    };

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

struct CaptureState {
    cursor_row: u16,
    cursor_col: u16,
    prompt: String,
    command: String,
    copy: bool,
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
    let mut terminal = vt100::Parser::new(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), 1000);
    let mut screen_terminal = vt100::Parser::new(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), 1000);
    let bindings = keybind::load()?;
    let mut keyboard = HotkeyMonitor::new(bindings)?;
    let keybind_watcher = KeybindWatcher::new()?;
    let mut capture_state: Option<CaptureState> = None;
    let mut end_seen = false;
    let mut marker_seen_pending = false;
    let mut loop_iteration = 0u64;
    let mut sync_marker_pending: Vec<u8> = Vec::new();

    loop {
        loop_iteration += 1;

        let keyboard_fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(keyboard.fd()) };
        let keybind_fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(keybind_watcher.fd()) };

        let mut poll_fds = [
            nix::poll::PollFd::new(stdin.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(master.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(signal_read.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(
                control.fd().as_fd(),
                nix::poll::PollFlags::POLLIN,
            ),
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

        if keybind_events.contains(nix::poll::PollFlags::POLLIN) {
            if keybind_watcher.changed()? {
                match keybind::load() {
                    Ok(bindings) => keyboard.set_bindings(bindings),
                    Err(error) => eprintln!("{error}"),
                }
            }
        }

        if signal_events.contains(nix::poll::PollFlags::POLLIN) {
            let _ = read(signal_read.as_fd(), &mut signal_buffer);

            let winsize = resize_pty(master, child)?;
            terminal.screen_mut().set_size(NonZeroU16::new(winsize.ws_row).unwrap(), NonZeroU16::new(winsize.ws_col).unwrap());
            screen_terminal
                .screen_mut()
                .set_size(NonZeroU16::new(winsize.ws_row).unwrap(), NonZeroU16::new(winsize.ws_col).unwrap());
        }

        if keyboard_events.contains(nix::poll::PollFlags::POLLIN) {
            if keyboard.poll()? {
                capture_screen(&mut screen_terminal)?;

                if let Some(sound) = sound {
                    sound.play("screen-capture");
                }
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

        if master_events.intersects(nix::poll::PollFlags::POLLIN | nix::poll::PollFlags::POLLHUP | nix::poll::PollFlags::POLLERR) {
            let count = match read(master.as_fd(), &mut buffer) {
                Ok(count) => count,
                Err(nix::errno::Errno::EIO) => break,
                Err(error) => return Err(format!("CVB: PTY read failed: {error}")),
            };

            if count == 0 {
                break;
            }

            let (output, marker_seen) =
                consume_sync_marker_stream(&mut sync_marker_pending, &buffer[..count]);

            if marker_seen {
                marker_seen_pending = true;
            }

            terminal.process(&output);
            screen_terminal.process(&output);

            let mut stdout = stdout.lock();

            stdout
                .write_all(&output)
                .map_err(|error| format!("CVB: stdout write failed: {error}"))?;

            stdout
                .flush()
                .map_err(|error| format!("CVB: stdout flush failed: {error}"))?;

            {
                use std::fs::OpenOptions;

                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/cvb-capture-debug.log")
                    .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

                let cursor = terminal.screen().cursor_position();

                writeln!(
                    file,
                    "MASTER iteration={} bytes={} cursor=({}, {})",
                    loop_iteration,
                    count,
                    cursor.0,
                    cursor.1,
                        )
                .map_err(|error| format!("CVB: unable to write capture debug log: {error}"))?;
            }
        }

        if control_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            for message in control.receive()? {
                handle_control_message(
                    message,
                    &mut capture_state,
                    &mut end_seen,
                    &terminal,
                )?;
            }
        }

        if end_seen && marker_seen_pending {
            if let Some(state) = capture_state.take() {
                capture_command_output(
                    &mut terminal,
                    &mut screen_terminal,
                    &state,
                )?;
            }

            end_seen = false;
            marker_seen_pending = false;
        }
    }

    Ok(())
}

fn handle_control_message(
    message: ControlMessage,
    capture_state: &mut Option<CaptureState>,
    end_seen: &mut bool,
    terminal: &vt100::Parser,
) -> Result<(), String> {
    match message {
        ControlMessage::Start {
            command,
            copy,
            ..
        } => {
            let screen = terminal.screen();
            let (cursor_row, cursor_col) = screen.cursor_position();

            let prompt = screen.contents_between(
                cursor_row,
                0,
                cursor_row,
                cursor_col,
            );

            *capture_state = Some(CaptureState {
                cursor_row,
                cursor_col,
                prompt,
                command,
                copy,
            });

            *end_seen = false;
        }

        ControlMessage::End { .. } => {
            if capture_state.is_some() {
                *end_seen = true;
            }
        }

        ControlMessage::Exit => {}

        ControlMessage::TargetRequest { .. } | ControlMessage::TargetResponse { .. } => {}
    }

    Ok(())
}

const CVB_SYNC_MARKER: &[u8] = b"\x1b]777;CVB;END\x07";

fn consume_sync_marker_stream(
    pending: &mut Vec<u8>,
    input: &[u8],
) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut marker_seen = false;

    for &byte in input {
        pending.push(byte);

        while !pending.is_empty() {
            if pending.len() <= CVB_SYNC_MARKER.len()
                && CVB_SYNC_MARKER.starts_with(pending)
            {
                break;
            }

            if pending.starts_with(CVB_SYNC_MARKER) {
                pending.drain(..CVB_SYNC_MARKER.len());
                marker_seen = true;
            } else {
                output.push(pending.remove(0));
            }
        }
    }

    (output, marker_seen)
}

fn capture_command_output(
    terminal: &mut vt100::Parser,
    screen_terminal: &mut vt100::Parser,
    state: &CaptureState,
) -> Result<(), String> {
    let alternate_screen = screen_terminal.screen().alternate_screen();

    if alternate_screen {
        let text = screen_terminal.screen().contents();
        let processed = if text.is_empty() {
            NO_OUTPUT.to_string()
        } else {
            text
        };

        if state.copy {
            copy_to_clipboard(&processed)?;
        }

        return Ok(());
    }

    use vt100::capture::{
        basic_formatted_to_plain,
        BasicFormattedCaptureRange,
        BasicFormattedCaptureState,
    };

    let mut captured = String::new();
    let mut capture_state = BasicFormattedCaptureState::default();

    terminal
        .screen()
        .write_contents_formatted_basic(
            &mut captured,
            BasicFormattedCaptureRange::Full(&mut capture_state),
        )
        .map_err(|error| format!("CVB: unable to capture terminal history: {error}"))?;

    let plain: String = basic_formatted_to_plain(&captured).collect();

    let extracted = if let Some(command_position) = plain.find(&state.command) {
        let after_command = &plain[command_position + state.command.len()..];

        after_command
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        String::new()
    };

    let processed = if extracted.is_empty() {
        NO_OUTPUT.to_string()
    } else {
        extracted
    };

    if state.copy {
        copy_to_clipboard(&processed)?;
    }

    Ok(())
}
fn capture_screen(terminal: &mut vt100::Parser) -> Result<(), String> {
    terminal.screen_mut().set_scrollback(usize::MAX);

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cvb-capture-debug.log")
        .ok()
        .and_then(|mut file| {
            writeln!(
                file,
                "SCREEN DEBUG: scrollback={} contents_len={}",
                terminal.screen().scrollback(),
                terminal.screen().contents().len()
            )
            .ok()
        });

    terminal.screen_mut().set_scrollback(0);

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/cvb-capture-debug.log")
        .ok()
        .and_then(|mut file| writeln!(file, "SCREEN CAPTURE ENTERED").ok());

    let text = screen_to_text(terminal);

    let processed = if text.is_empty() {
        NO_OUTPUT.to_string()
    } else {
        text
    };

    copy_to_clipboard(&processed)?;

    Ok(())
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    clipboard
        .stdin
        .take()
        .ok_or_else(|| "CVB: unable to open wl-copy stdin".to_string())?
        .write_all(text.as_bytes())
        .map_err(|error| format!("CVB: unable to write to wl-copy: {error}"))?;

    let status = clipboard
        .wait()
        .map_err(|error| format!("CVB: unable to wait for wl-copy: {error}"))?;

    if !status.success() {
        return Err("CVB: wl-copy failed".to_string());
    }

    Ok(())
}

fn screen_to_text(terminal: &mut vt100::Parser) -> String {
    let (_, cols) = terminal.screen().size();

    terminal.screen_mut().set_scrollback(usize::MAX);
    let max_scrollback = terminal.screen().scrollback();

    let mut output = Vec::new();

    for offset in (1..=max_scrollback).rev() {
        terminal.screen_mut().set_scrollback(offset);

        if let Some(row) = terminal.screen().rows(0, cols.get()).next() {
            output.push(row);
        }
    }

    terminal.screen_mut().set_scrollback(0);

    for row in terminal.screen().rows(0, cols.get()) {
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

#[cfg(test)]
mod tests {

    #[test]
    fn sync_marker_stream_is_detected_when_split_across_reads() {
        let marker = super::CVB_SYNC_MARKER;
        let split = marker.len() / 2;
        let mut pending = Vec::new();

        let (output, seen) =
            super::consume_sync_marker_stream(&mut pending, b"BEFORE");

        assert_eq!(output, b"BEFORE");
        assert!(!seen);
        assert!(pending.is_empty());

        let (output, seen) =
            super::consume_sync_marker_stream(&mut pending, &marker[..split]);

        assert!(output.is_empty());
        assert!(!seen);
        assert_eq!(pending, &marker[..split]);

        let mut second = marker[split..].to_vec();
        second.extend_from_slice(b"AFTER");

        let (output, seen) =
            super::consume_sync_marker_stream(&mut pending, &second);

        assert_eq!(output, b"AFTER");
        assert!(seen);
        assert!(pending.is_empty());
    }

}

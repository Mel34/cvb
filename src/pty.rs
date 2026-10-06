use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use nix::ioctl_read_bad;
use nix::ioctl_write_ptr_bad;
use nix::pty::{ForkptyResult, Winsize, forkpty};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::termios::{self, SetArg, Termios};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{Pid, pipe, read, write};

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
    let mut terminal = vt100::Parser::new(rows, cols, 0);
    let mut screen_terminal = vt100::Parser::new(rows, cols, 0);
    let bindings = keybind::load()?;
    let mut keyboard = HotkeyMonitor::new(bindings)?;
    let keybind_watcher = KeybindWatcher::new()?;
    let mut capture_state: Option<CaptureState> = None;
    let mut pending_end = false;
    let mut loop_iteration = 0u64;

    loop {
        loop_iteration += 1;
        let capture_ready = pending_end;
                let keyboard_fd =
            unsafe { std::os::fd::BorrowedFd::borrow_raw(keyboard.fd()) };
        let keybind_fd =
            unsafe { std::os::fd::BorrowedFd::borrow_raw(keybind_watcher.fd()) };

        let mut poll_fds = [
            nix::poll::PollFd::new(stdin.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(master.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(signal_read.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(
                control.fd().as_fd(),
                nix::poll::PollFlags::POLLIN,
            ),
            nix::poll::PollFd::new(
                keyboard_fd,
                nix::poll::PollFlags::POLLIN,
            ),
            nix::poll::PollFd::new(
                keybind_fd,
                nix::poll::PollFlags::POLLIN,
            ),
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
            match keybind_watcher.changed()? {
                true => match keybind::load() {
                    Ok(bindings) => {
                        keyboard.set_bindings(bindings);
                    }
                    Err(error) => {
                        eprintln!("{error}");
                    }
                },
                false => {}
            }
        }

        if signal_events.contains(nix::poll::PollFlags::POLLIN) {
            let _ = read(signal_read.as_fd(), &mut signal_buffer);

            let winsize = resize_pty(master, child)?;
            let rows = winsize.ws_row;
            let cols = winsize.ws_col;

            terminal.screen_mut().set_size(rows, cols);
            screen_terminal.screen_mut().set_size(rows, cols);
        }

        if keyboard_events.contains(nix::poll::PollFlags::POLLIN) {
            if keyboard.poll()? {
                capture_screen(&mut screen_terminal)?;

                if let Some(sound) = sound {
                    sound.play("screen-capture");
                }
            }
        }

        if master_events.intersects(
            nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR
                | nix::poll::PollFlags::POLLNVAL,
        ) {
            break;
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

        if master_events.contains(nix::poll::PollFlags::POLLIN) {
            let count = read(master.as_fd(), &mut buffer)
                .map_err(|error| format!("CVB: PTY read failed: {error}"))?;

            if count == 0 {
                break;
            }

            terminal.process(&buffer[..count]);
            screen_terminal.process(&buffer[..count]);

            let mut stdout = stdout.lock();

            stdout
                .write_all(&buffer[..count])
                .map_err(|error| format!("CVB: stdout write failed: {error}"))?;

            stdout
                .flush()
                .map_err(|error| format!("CVB: stdout flush failed: {error}"))?;

            {
                use std::fs::OpenOptions;
                use std::io::Write;

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
                    &terminal,
                    &mut capture_state,
                    &mut pending_end,
                )?;
            }
        }

        if capture_ready {
            if let Some(state) = capture_state.take() {
                capture_command_output(&mut terminal, &mut screen_terminal, rows, cols, &state)?;
            }

            pending_end = false;
        }
    }

    Ok(())
}

fn handle_control_message(
    message: ControlMessage,
    terminal: &vt100::Parser,
    capture_state: &mut Option<CaptureState>,
    pending_end: &mut bool,
) -> Result<(), String> {
    match message {
        ControlMessage::Start {
            command,
            copy,
            ..
        } => {
            let screen = terminal.screen();
            let (cursor_row, cursor_col) = screen.cursor_position();

            {
                use std::fs::OpenOptions;
                use std::io::Write;

                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/cvb-capture-debug.log")
                    .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

                writeln!(
                    file,
                    "CONTROL START command={:?} cursor=({}, {}) screen={:?}",
                    command,
                    cursor_row,
                    cursor_col,
                    screen.contents(),
                )
                .map_err(|error| format!("CVB: unable to write capture debug log: {error}"))?;
            }

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

            *pending_end = false;
        }

        ControlMessage::End { .. } => {
            {
                use std::fs::OpenOptions;
                use std::io::Write;

                let screen = terminal.screen();
                let (cursor_row, cursor_col) = screen.cursor_position();

                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/cvb-capture-debug.log")
                    .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

                writeln!(
                    file,
                    "CONTROL END cursor=({}, {})",
                    cursor_row,
                    cursor_col,
                )
                .map_err(|error| format!("CVB: unable to write capture debug log: {error}"))?;
            }

            *pending_end = true;
        }

        ControlMessage::Exit => {}

        ControlMessage::TargetRequest { .. } | ControlMessage::TargetResponse { .. } => {}
    }

    Ok(())
}

fn capture_command_output(
    terminal: &mut vt100::Parser,
    screen_terminal: &mut vt100::Parser,
    rows: u16,
    cols: u16,
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

        copy_to_clipboard(&processed)?;

        return Ok(());
    }

    let end_cursor = terminal.screen().cursor_position();

    let region = terminal.screen().contents_between(
        state.cursor_row,
        state.cursor_col,
        end_cursor.0,
        end_cursor.1,
    );

    let region = region.strip_prefix(&state.command).unwrap_or(&region);

    let output = region
        .strip_suffix(&state.prompt)
        .unwrap_or(region)
        .trim_matches('\n');

    {
        use std::fs::OpenOptions;
        use std::io::Write;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/cvb-capture-debug.log")
            .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

        writeln!(
            file,
            "start=({}, {}) end=({}, {}) command_len={} region_len={} output_len={} prefix={} prompt_len={}",
            state.cursor_row,
            state.cursor_col,
            end_cursor.0,
            end_cursor.1,
            state.command.len(),
            region.len(),
            output.len(),
            region.starts_with(&state.command),
            state.prompt.len(),
        )
        .map_err(|error| format!("CVB: unable to write capture debug log: {error}"))?;
    }

    let processed = if output.is_empty() {
        NO_OUTPUT.to_string()
    } else {
        output.to_string()
    };

    if state.copy {
        copy_to_clipboard(&processed)?;
    }

    *terminal = vt100::Parser::new(rows, cols, 1000);

    Ok(())
}

fn capture_screen(
    terminal: &mut vt100::Parser,
) -> Result<(), String> {
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

fn screen_to_text(terminal: &vt100::Parser) -> String {
    terminal.screen().contents()
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
    use super::screen_to_text;

    #[test]
    fn osc_sync_marker_is_not_screen_content() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"BEFORE");
        terminal.process(b"\x1b]1337;CVB-END\x07");
        terminal.process(b"AFTER");

        assert_eq!(terminal.screen().contents(), "BEFOREAFTER");
    }

    #[test]
    fn screen_to_text_extracts_visible_content() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"hello\r\nworld");

        assert_eq!(screen_to_text(&terminal), "hello\nworld");
    }

    #[test]
    fn reset_starts_next_capture_with_empty_screen() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"echo one\r\none\r\n");
        assert_eq!(screen_to_text(&terminal), "echo one\none");

        terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"echo two\r\ntwo\r\n");
        assert_eq!(screen_to_text(&terminal), "echo two\ntwo");
    }

    #[test]
    fn reset_removes_previous_capture_content() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"first command\r\nfirst output\r\n");
        assert!(screen_to_text(&terminal).contains("first command"));
        assert!(screen_to_text(&terminal).contains("first output"));

        terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"second command\r\nsecond output\r\n");

        let captured = screen_to_text(&terminal);

        assert!(captured.contains("second command"));
        assert!(captured.contains("second output"));
        assert!(!captured.contains("first command"));
        assert!(!captured.contains("first output"));
    }

    #[test]
    fn screen_to_text_handles_carriage_return_overwrite() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"hello\rworld");

        assert_eq!(screen_to_text(&terminal), "world");
    }

    #[test]
    fn screen_to_text_ignores_ansi_color_sequences() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"\x1b[31mred\x1b[0m");

        assert_eq!(screen_to_text(&terminal), "red");
    }

    #[test]
    fn screen_to_text_handles_cursor_movement() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"hello\x1b[2DXX");

        assert_eq!(screen_to_text(&terminal), "helXX");
    }

    #[test]
    fn screen_to_text_handles_screen_clearing() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"hello\x1b[2K");

        assert_eq!(screen_to_text(&terminal), "");
    }

    #[test]
    fn screen_to_text_preserves_multiple_lines() {
        let mut terminal = vt100::Parser::new(4, 20, 0);

        terminal.process(b"one\r\ntwo\r\nthree");

        assert_eq!(screen_to_text(&terminal), "one\ntwo\nthree");
    }

    #[test]
    fn scrollback_can_be_viewed() {
        let mut terminal = vt100::Parser::new(4, 20, 20);

        terminal.process(
            b"line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7\r\nline8",
        );

        assert_eq!(terminal.screen().contents(), "line5\nline6\nline7\nline8");

        terminal.screen_mut().set_scrollback(4);

        assert_eq!(terminal.screen().scrollback(), 4);
        assert!(terminal.screen().contents().contains("line1"));
    }
}

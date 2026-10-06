use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

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
const PROMPT_MARKER: &[u8] = b"\x1b[31m\xe2\x97\x8f\x1b[0m ";

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
    prompt_serial_at_start: u64,
}

struct InputTracker {
    active: bool,
    waiting_for_submit: bool,
    command_exact: bool,
    text: Vec<u8>,
    escape: Vec<u8>,
}

impl InputTracker {
    fn new() -> Self {
        Self {
            active: false,
            waiting_for_submit: false,
            command_exact: true,
            text: Vec::new(),
            escape: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.active = false;
        self.waiting_for_submit = false;
        self.command_exact = true;
        self.text.clear();
        self.escape.clear();
    }

    fn command_text(&self) -> String {
        if !self.command_exact {
            return String::new();
        }

        String::from_utf8_lossy(&self.text)
            .trim_matches(|character| character == '\r' || character == '\n')
            .to_string()
    }

    fn feed(
        &mut self,
        bytes: &[u8],
        terminal: &vt100::Parser,
    ) -> Option<(u16, u16, String)> {
        let mut start = None;

        for &byte in bytes {
            if !self.escape.is_empty() {
                self.escape.push(byte);

                if (0x40..=0x7e).contains(&byte) {
                    if byte == b'~' {
                        let params = &self.escape[2..self.escape.len() - 1];

                        if params == b"200" || params == b"201" {
                            self.escape.clear();
                            continue;
                        }
                    }

                    if self.escape.len() >= 3
                        && self.escape[1] == b'['
                        && matches!(byte, b'A' | b'B')
                        && !self.active
                    {
                        self.waiting_for_submit = true;
                    }

                    let bracketed_paste = byte == b'~'
                        && self.escape.len() >= 3
                        && (&self.escape[2..self.escape.len() - 1] == b"200"
                            || &self.escape[2..self.escape.len() - 1] == b"201");

                    if !bracketed_paste {
                        self.command_exact = false;
                    }

                    self.escape.clear();
                }

                continue;
            }

            if byte == 0x1b {
                self.escape.push(byte);
                continue;
            }

            if byte == b'\r' || byte == b'\n' {
                if !self.active && self.waiting_for_submit {
                    let (row, col) = terminal.screen().cursor_position();

                    self.active = true;
                    self.waiting_for_submit = false;
                    self.command_exact = false;
                    self.text.clear();

                    if start.is_none() {
                        start = Some((row, col, String::new()));
                    }
                } else if self.active && self.command_exact {
                    self.text.push(b'\n');
                }

                continue;
            }

            if !self.active {
                if byte == 0x12 {
                    self.waiting_for_submit = true;
                    continue;
                }

                if !is_command_input_byte(byte) || self.waiting_for_submit {
                    continue;
                }

                let (row, col) = terminal.screen().cursor_position();
                let prompt = terminal.screen().contents_between(row, 0, row, col);

                self.active = true;
                self.command_exact = true;

                if start.is_none() {
                    start = Some((row, col, prompt));
                }
            }

            if self.command_exact {
                match byte {
                    0x7f | 0x08 => {
                        let mut cursor = self.text.len();

                        if cursor > 0 {
                            cursor -= 1;

                            while cursor > 0 && (self.text[cursor] & 0xc0) == 0x80 {
                                cursor -= 1;
                            }

                            self.text.truncate(cursor);
                        }
                    }
                    0x17 => {
                        while self.text.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
                            self.text.pop();
                        }

                        while self.text.last().is_some_and(|byte| !byte.is_ascii_whitespace()) {
                            self.text.pop();
                        }
                    }
                    0x15 => self.text.clear(),
                    b'\t' | 0x20..=0x7e | 0x80..=0xff => self.text.push(byte),
                    _ => {}
                }
            }
        }

        start
    }
}

fn is_command_input_byte(byte: u8) -> bool {
    byte == b'\t' || (byte >= 0x20 && byte != 0x7f)
}

fn update_prompt_marker(matcher: &mut usize, serial: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        if byte == PROMPT_MARKER[*matcher] {
            *matcher += 1;

            if *matcher == PROMPT_MARKER.len() {
                *matcher = 0;
                *serial += 1;
            }
        } else {
            *matcher = usize::from(byte == PROMPT_MARKER[0]);
        }
    }
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
    let mut terminal = vt100::Parser::new(rows, cols, 1000);
    let mut screen_terminal = vt100::Parser::new(rows, cols, 1000);
    let bindings = keybind::load()?;
    let mut keyboard = HotkeyMonitor::new(bindings)?;
    let keybind_watcher = KeybindWatcher::new()?;
    let mut input_tracker = InputTracker::new();
    let mut capture_state: Option<CaptureState> = None;
    let mut pending_end = false;
    let mut prompt_matcher = 0usize;
    let mut prompt_serial = 0u64;
    let mut loop_iteration = 0u64;

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
            terminal.screen_mut().set_size(winsize.ws_row, winsize.ws_col);
            screen_terminal
                .screen_mut()
                .set_size(winsize.ws_row, winsize.ws_col);
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

            if capture_state.is_none() {
                if let Some((cursor_row, cursor_col, prompt)) =
                    input_tracker.feed(&buffer[..count], &terminal)
                {
                    capture_state = Some(CaptureState {
                        cursor_row,
                        cursor_col,
                        prompt,
                        command: input_tracker.command_text(),
                        copy: true,
                        prompt_serial_at_start: prompt_serial,
                    });
                }
            } else {
                input_tracker.feed(&buffer[..count], &terminal);
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

            update_prompt_marker(&mut prompt_matcher, &mut prompt_serial, &buffer[..count]);

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

                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/cvb-capture-debug.log")
                    .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

                let cursor = terminal.screen().cursor_position();

                writeln!(
                    file,
                    "MASTER iteration={} bytes={} cursor=({}, {}) prompt_serial={}",
                    loop_iteration,
                    count,
                    cursor.0,
                    cursor.1,
                    prompt_serial,
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
                    &mut pending_end,
                )?;
            }
        }

        if pending_end {
            let ready = capture_state
                .as_ref()
                .is_some_and(|state| prompt_serial > state.prompt_serial_at_start);

            if ready {
                if let Some(mut state) = capture_state.take() {
                    state.command = input_tracker.command_text();
                    state.copy = !is_cvb_command(&state.command);

                    capture_command_output(
                        &mut terminal,
                        &mut screen_terminal,
                        rows,
                        cols,
                        &state,
                    )?;

                    input_tracker.reset();
                }

                pending_end = false;
            }
        }
    }

    Ok(())
}

fn handle_control_message(
    message: ControlMessage,
    capture_state: &mut Option<CaptureState>,
    pending_end: &mut bool,
) -> Result<(), String> {
    match message {
        ControlMessage::Start { .. } => {}

        ControlMessage::End { .. } => {
            if capture_state.is_some() {
                *pending_end = true;
            }
        }

        ControlMessage::Exit => {}

        ControlMessage::TargetRequest { .. } | ControlMessage::TargetResponse { .. } => {}
    }

    Ok(())
}

fn is_cvb_command(command: &str) -> bool {
    let command = command.trim_start();
    command == "cvb" || command.starts_with("cvb ")
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

        if state.copy {
            copy_to_clipboard(&processed)?;
        }

        return Ok(());
    }

    let end_cursor = terminal.screen().cursor_position();

    let original_region = terminal.screen().contents_between(
        state.cursor_row,
        state.cursor_col,
        end_cursor.0,
        end_cursor.1,
    );

    let region = original_region
        .strip_suffix(&state.prompt)
        .unwrap_or(&original_region);

    let region = if !state.command.is_empty() {
        region
            .strip_prefix(&state.command)
            .unwrap_or(region)
    } else {
        region
    };

    let output = region.trim_matches('\n');

    {
        use std::fs::OpenOptions;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/cvb-capture-debug.log")
            .map_err(|error| format!("CVB: unable to open capture debug log: {error}"))?;

        let tail = original_region
            .chars()
            .rev()
            .take(100)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();

        writeln!(
            file,
            "START cursor=({}, {}) prompt_serial_start={} end_cursor=({}, {}) command_len={} region_len={} output_len={} prefix={} prompt_len={} prompt_serial={} tail={:?}",
            state.cursor_row,
            state.cursor_col,
            state.prompt_serial_at_start,
            end_cursor.0,
            end_cursor.1,
            state.command.len(),
            region.len(),
            output.len(),
            original_region.starts_with(&state.command),
            state.prompt.len(),
            0u64,
            tail,
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

#[cfg(test)]
mod tests {
    use super::{is_cvb_command, update_prompt_marker, InputTracker, PROMPT_MARKER};

    #[test]
    fn prompt_marker_is_detected_across_reads() {
        let mut matcher = 0usize;
        let mut serial = 0u64;

        update_prompt_marker(&mut matcher, &mut serial, &PROMPT_MARKER[..5]);
        update_prompt_marker(&mut matcher, &mut serial, &PROMPT_MARKER[5..]);

        assert_eq!(serial, 1);
    }

    #[test]
    fn input_tracker_starts_on_command_text() {
        let mut tracker = InputTracker::new();
        let terminal = vt100::Parser::new(4, 20, 0);

        let start = tracker.feed(b"echo hello\r", &terminal);

        assert_eq!(start, Some((0, 0, String::new())));
        assert_eq!(tracker.command_text(), "echo hello");
    }

    #[test]
    fn input_tracker_waits_for_history_recall_submission() {
        let mut tracker = InputTracker::new();
        let terminal = vt100::Parser::new(4, 20, 0);

        let start = tracker.feed(b"\x1b[A\r", &terminal);

        assert_eq!(start, Some((0, 0, String::new())));
        assert_eq!(tracker.command_text(), "");
    }

    #[test]
    fn recognizes_cvb_commands() {
        assert!(is_cvb_command("cvb"));
        assert!(is_cvb_command("  cvb file"));
        assert!(!is_cvb_command("cvbench"));
    }
}

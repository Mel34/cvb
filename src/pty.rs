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

use crate::clipboard::ClipboardWatcher;
use crate::control::{ControlChannel, ControlMessage};

ioctl_read_bad!(tiocgwinsz, libc::TIOCGWINSZ, Winsize);
ioctl_write_ptr_bad!(tiocswinsz, libc::TIOCSWINSZ, Winsize);

const CONTROL_FD: i32 = 3;

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

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("CVB: unable to create input runtime: {error}"))?;

    let input = runtime
        .block_on(crate::input::Input::new())
        .map_err(|error| format!("CVB: unable to initialize input: {error}"))?;

    let mut hotkey_monitor = crate::keyboard::HotkeyMonitor::new()
        .map_err(|error| format!("CVB: unable to initialize keyboard monitor: {error}"))?;

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

            let mut clipboard = ClipboardWatcher::new()?;

            let result = proxy(
                &master,
                child,
                &signal_read,
                &mut control,
                &input,
                &mut hotkey_monitor,
                &mut clipboard,
                winsize.ws_row,
                winsize.ws_col,
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

    let result =
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };

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

    let result =
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };

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
    input: &crate::input::Input,
    hotkey_monitor: &mut crate::keyboard::HotkeyMonitor,
    clipboard: &mut ClipboardWatcher,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();

    let mut buffer = [0u8; 8192];
    let mut signal_buffer = [0u8; 64];
    let mut command_output = Vec::new();
    let mut terminal = vt100::Parser::new(rows, cols, 0);
    let mut capturing_output = false;
    let mut copy_output = false;

    loop {
        if hotkey_monitor.poll()? {
            capturing_output = false;
            command_output.clear();

            std::thread::sleep(std::time::Duration::from_millis(100));

            input
                .inject_ctrl_c()
                .map_err(|error| format!("CVB: unable to inject Ctrl+C: {error}"))?;

            std::thread::sleep(std::time::Duration::from_millis(100));

            input
                .inject_escape()
                .map_err(|error| format!("CVB: unable to inject Escape: {error}"))?;

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        clipboard.dispatch_pending()?;

        let clipboard_guard = loop {
            if let Some(guard) = clipboard.prepare_read() {
                break guard;
            }

            clipboard.dispatch_pending()?;
        };

        let mut poll_fds = [
            nix::poll::PollFd::new(stdin.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(master.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(signal_read.as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(control.fd().as_fd(), nix::poll::PollFlags::POLLIN),
            nix::poll::PollFd::new(
                clipboard.wayland_fd(),
                nix::poll::PollFlags::POLLIN,
            ),
        ];

        match nix::poll::poll(&mut poll_fds, None::<u16>) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => {
                drop(clipboard_guard);
                continue;
            }
            Err(error) => {
                drop(clipboard_guard);
                return Err(format!("CVB: PTY poll failed: {error}"));
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

        let clipboard_events = poll_fds[4]
            .revents()
            .unwrap_or(nix::poll::PollFlags::empty());

        if clipboard_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            clipboard.read_events(clipboard_guard)?;
        } else {
            drop(clipboard_guard);
        }

        if signal_events.contains(nix::poll::PollFlags::POLLIN) {
            let _ = read(signal_read.as_fd(), &mut signal_buffer);

            resize_pty(master, child)?;
        }

        if control_events.intersects(
            nix::poll::PollFlags::POLLIN
                | nix::poll::PollFlags::POLLHUP
                | nix::poll::PollFlags::POLLERR,
        ) {
            for message in control.receive()? {
                handle_control_message(
                    message,
                    &mut command_output,
                    &mut capturing_output,
                    &mut copy_output,
                )?;
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

            if capturing_output {
                command_output.extend_from_slice(&buffer[..count]);
            }

            let mut stdout = stdout.lock();

            stdout
                .write_all(&buffer[..count])
                .map_err(|error| format!("CVB: stdout write failed: {error}"))?;

            stdout
                .flush()
                .map_err(|error| format!("CVB: stdout flush failed: {error}"))?;
        }
    }

    Ok(())
}

fn handle_control_message(
    message: ControlMessage,
    command_output: &mut Vec<u8>,
    capturing_output: &mut bool,
    copy_output: &mut bool,
) -> Result<(), String> {
    match message {
        ControlMessage::Start {
            id: _,
            command: _,
            copy,
        } => {
            command_output.clear();
            *copy_output = copy;
            *capturing_output = copy;
        }

        ControlMessage::End { id: _, status: _ } => {
            *capturing_output = false;

            if !*copy_output {
                return Ok(());
            }

            let processed = postprocess_output(command_output);

            let mut child = std::process::Command::new("wl-copy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(&processed)
                    .map_err(|error| format!("CVB: unable to write to wl-copy: {error}"))?;
            }

            let status = child
                .wait()
                .map_err(|error| format!("CVB: unable to wait for wl-copy: {error}"))?;

            if !status.success() {
                return Err(format!("CVB: wl-copy exited with status {status}"));
            }
        }

        ControlMessage::Exit => {}
    }

    Ok(())
}

fn postprocess_output(output: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(output.len());
    let mut index = 0;

    while index < output.len() {
        if output[index] == 0x1b && index + 1 < output.len() {
            match output[index + 1] {
                b']' => {
                    index += 2;

                    while index < output.len() && output[index] != 0x07 {
                        index += 1;
                    }

                    if index < output.len() {
                        index += 1;
                    }

                    continue;
                }

                b'[' => {
                    index += 2;

                    while index < output.len() {
                        let byte = output[index];
                        index += 1;

                        if (0x40..=0x7e).contains(&byte) {
                            break;
                        }
                    }

                    continue;
                }

                _ => {}
            }
        }

        result.push(output[index]);
        index += 1;
    }

    if result.is_empty() {
        b"[CVB: no output]\n".to_vec()
    } else {
        result
    }
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

fn resize_pty(master: &OwnedFd, child: Pid) -> Result<(), String> {
    let winsize = terminal_size()?;

    unsafe {
        tiocswinsz(master.as_raw_fd(), &winsize)
            .map_err(|error| format!("CVB: unable to resize PTY: {error}"))?;
    }

    signal::kill(child, Signal::SIGWINCH)
        .map_err(|error| format!("CVB: unable to forward SIGWINCH: {error}"))?;

    Ok(())
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
    use super::postprocess_output;

    #[test]
    fn preserves_plain_output() {
        assert_eq!(postprocess_output(b"hello\r\n"), b"hello\r\n");
    }

    #[test]
    fn strips_osc_sequence() {
        assert_eq!(
            postprocess_output(b"hello\x1b]0;title\x07world\r\n"),
            b"helloworld\r\n"
        );
    }

    #[test]
    fn strips_csi_sequences() {
        assert_eq!(postprocess_output(b"\x1b[31mred\x1b[0m\r\n"), b"red\r\n");
    }

    #[test]
    fn preserves_carriage_returns() {
        assert_eq!(postprocess_output(b"hello\rworld\r\n"), b"hello\rworld\r\n");
    }

    #[test]
    fn preserves_backspaces() {
        assert_eq!(postprocess_output(b"hellp\x08\r\n"), b"hellp\x08\r\n");
    }

    #[test]
    fn returns_marker_for_empty_output() {
        assert_eq!(postprocess_output(b""), b"[CVB: no output]\n");
    }

    #[test]
    fn processes_mixed_output() {
        assert_eq!(
            postprocess_output(b"\x1b]0;title\x07\x1b[32mhello\x1b[0m\r\n"),
            b"hello\r\n"
        );
    }
}
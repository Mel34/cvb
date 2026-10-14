#!/usr/bin/env python3
"""Exercise CVB's real interactive PTY, marker, transcript, and clipboard path."""

import fcntl
import hashlib
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
START = b"\x1b]777;CVB;START\x07"
END = b"\x1b]777;CVB;END\x07"

passed = 0
failed = 0
buffer = bytearray()
master = None
child_pid = None


def check(name, condition, detail=""):
    global passed, failed
    if condition:
        passed += 1
        print(f"ok   {name}", flush=True)
    else:
        failed += 1
        print(f"FAIL {name}", flush=True)
        if detail:
            if isinstance(detail, bytes):
                print(
                    f"     payload length={len(detail)} bytes; "
                    f"preview={detail[:160]!r}",
                    flush=True,
                )
            else:
                print(f"     {str(detail)[:300]!r}", flush=True)


def read_until(marker, timeout=10):
    deadline = time.monotonic() + timeout
    while True:
        position = buffer.find(marker)
        if position >= 0:
            result = bytes(buffer[:position])
            del buffer[:position + len(marker)]
            return result

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for {marker!r}")

        ready, _, _ = select.select([master], [], [], min(remaining, 0.2))
        if not ready:
            continue

        try:
            data = os.read(master, 65536)
        except OSError as error:
            raise RuntimeError(f"PTY closed: {error}") from error

        if not data:
            raise RuntimeError("PTY closed before expected marker")
        buffer.extend(data)


def clipboard_files(directory):
    return sorted(directory.glob("capture-*.bin"))


def wait_for_payload(directory, count, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        files = clipboard_files(directory)
        if len(files) >= count:
            return [path.read_bytes() for path in files]

        ready, _, _ = select.select([master], [], [], 0.1)
        if ready:
            try:
                data = os.read(master, 65536)
                if data:
                    buffer.extend(data)
            except OSError:
                pass

        result = os.waitpid(child_pid, os.WNOHANG)
        if result[0]:
            raise RuntimeError(
                "CVB exited before clipboard capture; terminal output: "
                + bytes(buffer).decode("utf-8", "replace")
            )

    raise TimeoutError(f"timed out waiting for clipboard payload {count}")


def run_command(name, command, clipboard_dir, expected, forbidden=()):
    before = len(clipboard_files(clipboard_dir))
    os.write(master, command.encode("utf-8") + b"\n")
    payloads = wait_for_payload(clipboard_dir, before + 1)
    payload = payloads[before]

    check(
        name,
        all(item in payload for item in expected)
        and all(item not in payload for item in forbidden),
        payload,
    )
    return payload


def main():
    global master, child_pid

    build = subprocess.run(
        ["cargo", "build", "--bin", "cvb"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if build.returncode:
        print(build.stdout, file=sys.stderr)
        print(build.stderr, file=sys.stderr)
        return 1

    binary = ROOT / "target" / "debug" / "cvb"

    with tempfile.TemporaryDirectory(prefix="cvb-pty-test-") as temporary:
        work = Path(temporary)
        home = work / "home"
        config = work / "config"
        runtime = work / "runtime"
        fake_bin = work / "bin"
        clipboard_dir = work / "clipboard"

        for directory in (home, config, runtime, fake_bin, clipboard_dir):
            directory.mkdir(parents=True)

        (home / ".bashrc").write_text("")
        cvb_config = config / "cvb"
        cvb_config.mkdir(parents=True, exist_ok=True)
        (cvb_config / "limit").write_text("400000\n")
        init_file = work / "parent-init"
        history_file = work / "history"
        parent_history_file = work / "parent-history"
        for path in (init_file, history_file, parent_history_file):
            path.write_text("")

        fake_copy = fake_bin / "wl-copy"
        fake_copy.write_text(
            f"""#!{sys.executable}
import os
import sys
from pathlib import Path

directory = Path(os.environ["CVB_TEST_CLIPBOARD_DIR"])
files = sorted(directory.glob("capture-*.bin"))
number = len(files) + 1
(directory / f"capture-{{number:03}}.bin").write_bytes(
    sys.stdin.buffer.read()
)
"""
        )
        fake_copy.chmod(0o755)

        control_source = ROOT / "data" / "cvb.bash"
        env = os.environ.copy()
        env.update({
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config),
            "XDG_RUNTIME_DIR": str(runtime),
            "CVB_INIT_FILE": str(init_file),
            "CVB_HISTORY_FILE": str(history_file),
            "CVB_PARENT_HISTORY_FILE": str(parent_history_file),
            "CVB_BASH_SOURCE": str(control_source),
            "CVB_BASH_SHA256": hashlib.sha256(
                control_source.read_bytes()
            ).hexdigest(),
            "CVB_TEST_CLIPBOARD_DIR": str(clipboard_dir),
            "TERM": "xterm",
            "HISTFILE": str(home / ".bash_history"),
            "PATH": str(fake_bin) + os.pathsep + env.get("PATH", ""),
        })

        pid, master_fd = pty.fork()
        if pid == 0:
            os.execve(str(binary), [str(binary)], env)

        child_pid = pid
        master = master_fd
        fcntl.ioctl(master, 0x5414, struct.pack("HHHH", 24, 80, 0, 0))

        try:
            read_until(END)
            check("interactive CVB session starts", True)

            run_command(
                "captures ordinary command output",
                "printf 'PTY_SINGLE_OK\\n'",
                clipboard_dir,
                (b"[Terminal output]", b"PTY_SINGLE_OK"),
            )

            run_command(
                "captures multiline conditional",
                """if true; then
    printf 'PTY_IF_OK\\n'
else
    printf 'PTY_WRONG_BRANCH\\n'
fi""",
                clipboard_dir,
                (b"PTY_IF_OK",),
                (b"PTY_WRONG_BRANCH",),
            )

            run_command(
                "captures stderr written to terminal",
                "printf 'PTY_STDERR_OK\\n' >&2",
                clipboard_dir,
                (b"PTY_STDERR_OK",),
            )

            # ANSI formatting should preserve visible text without leaking escapes.
            run_command(
                "handles ANSI color control sequences",
                "printf 'PTY_ANSI_BEGIN\\n'; "
                "printf '\\033[31mPTY_ANSI_TEXT\\033[0m\\n'; "
                "printf 'PTY_ANSI_END\\n'",
                clipboard_dir,
                (b"PTY_ANSI_BEGIN", b"PTY_ANSI_TEXT", b"PTY_ANSI_END"),
                (b"\x1b",),
            )

            # Carriage return should replace the existing line contents.
            run_command(
                "handles carriage-return line rewrites",
                "printf 'PTY_CR_BEGIN\\n'; "
                "printf 'PTY_CR_OLD\\rPTY_CR_NEW\\n'; "
                "printf 'PTY_CR_END\\n'",
                clipboard_dir,
                (b"PTY_CR_BEGIN", b"PTY_CR_NEW", b"PTY_CR_END"),
                (b"PTY_CR_OLD",),
            )

            # Backspace moves left; subsequent characters overwrite existing text.
            run_command(
                "handles backspace cursor movement",
                "printf 'PTY_BS_BEGIN\\n'; "
                "printf 'ABCDE\\b\\bXY\\n'; "
                "printf 'PTY_BS_END\\n'",
                clipboard_dir,
                (b"PTY_BS_BEGIN", b"ABCXY", b"PTY_BS_END"),
                (b"ABCDE",),
            )

            # Horizontal tabs should expand to terminal tab stops.
            run_command(
                "handles horizontal tab expansion",
                "printf 'PTY_TAB_BEGIN\\n'; "
                "printf 'A\\tB\\n'; "
                "printf 'PTY_TAB_END\\n'",
                clipboard_dir,
                (b"PTY_TAB_BEGIN", b"A       B", b"PTY_TAB_END"),
                (b"A\tB",),
            )

            # Cursor-up and erase-line sequences should capture the final visible text.
            run_command(
                "handles ANSI cursor movement and line erasure",
                "printf 'PTY_CURSOR_BEGIN\\nPTY_CURSOR_OLD\\n'; "
                "printf '\\033[1A\\033[2KPTY_CURSOR_NEW\\nPTY_CURSOR_END\\n'",
                clipboard_dir,
                (
                    b"PTY_CURSOR_BEGIN",
                    b"PTY_CURSOR_NEW",
                    b"PTY_CURSOR_END",
                ),
                (b"PTY_CURSOR_OLD",),
            )

            # UTF-8 output should survive capture without character corruption.
            run_command(
                "preserves UTF-8 terminal output",
                "printf 'PTY_UTF8_BEGIN\\n'; "
                "printf 'Zażółć 世界\\n'; "
                "printf 'PTY_UTF8_END\\n'",
                clipboard_dir,
                (
                    b"PTY_UTF8_BEGIN",
                    "Zażółć 世界".encode("utf-8"),
                    b"PTY_UTF8_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Wide Unicode characters occupy two terminal cells.
            run_command(
                "handles wide Unicode terminal characters",
                "printf 'PTY_WIDE_BEGIN\\n'; "
                "printf '界X\\n'; "
                "printf 'PTY_WIDE_END\\n'",
                clipboard_dir,
                (
                    b"PTY_WIDE_BEGIN",
                    "界X".encode("utf-8"),
                    b"PTY_WIDE_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Combining marks have zero width and must stay attached to the base character.
            run_command(
                "preserves combining Unicode characters",
                "printf 'PTY_COMBINING_BEGIN\\n'; "
                "printf 'éX\\n'; "
                "printf 'PTY_COMBINING_END\\n'",
                clipboard_dir,
                (
                    b"PTY_COMBINING_BEGIN",
                    "e\u0301X".encode("utf-8"),
                    b"PTY_COMBINING_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Text beyond the right margin should wrap to the next terminal row.
            run_command(
                "handles automatic right-margin wrapping",
                "printf 'PTY_WRAP_BEGIN\\n'; "
                "printf '%080d' 0; "
                "printf 'X\\nPTY_WRAP_END\\n'",
                clipboard_dir,
                (
                    b"PTY_WRAP_BEGIN",
                    b"0" * 80 + b"\nX",
                    b"PTY_WRAP_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Wrapped lines should survive scrollback reconstruction in order.
            scroll_payload = run_command(
                "captures wrapped output across scrollback",
                "printf 'PTY_SCROLL_BEGIN\\n'; "
                "for i in $(seq 1 30); do "
                "printf 'PTY_SCROLL_%02d:' \"$i\"; "
                "printf '%070d' \"$i\"; "
                "printf '\\n'; "
                "done; "
                "printf 'PTY_SCROLL_END\\n'",
                clipboard_dir,
                (
                    b"PTY_SCROLL_BEGIN",
                    b"PTY_SCROLL_01:",
                    b"PTY_SCROLL_15:",
                    b"PTY_SCROLL_30:",
                    b"PTY_SCROLL_END",
                ),
                (b"\xef\xbf\xbd",),
            )
            scroll_markers = (
                b"PTY_SCROLL_BEGIN",
                b"PTY_SCROLL_01:",
                b"PTY_SCROLL_15:",
                b"PTY_SCROLL_30:",
                b"PTY_SCROLL_END",
            )
            check(
                "preserves wrapped scrollback marker order",
                [scroll_payload.find(marker) for marker in scroll_markers]
                == sorted(
                    scroll_payload.find(marker) for marker in scroll_markers
                )
                and all(marker in scroll_payload for marker in scroll_markers),
                scroll_payload,
            )

            # Output without a final newline must still be captured.
            run_command(
                "captures output without a trailing newline",
                "printf 'PTY_PARTIAL_BEGIN'; "
                "printf 'PTY_PARTIAL_BODY'; "
                "printf 'PTY_PARTIAL_END'",
                clipboard_dir,
                (
                    b"PTY_PARTIAL_BEGIN",
                    b"PTY_PARTIAL_BODY",
                    b"PTY_PARTIAL_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Empty lines between output must remain present.
            run_command(
                "preserves intentional blank lines",
                "printf 'PTY_BLANK_BEGIN\\n\\n"
                "PTY_BLANK_MIDDLE\\n\\nPTY_BLANK_END\\n'",
                clipboard_dir,
                (
                    b"PTY_BLANK_BEGIN\n\nPTY_BLANK_MIDDLE",
                    b"PTY_BLANK_MIDDLE\n\nPTY_BLANK_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # CSI insert-character should shift existing characters right.
            run_command(
                "handles ANSI character insertion",
                "printf 'PTY_INSERT_BEGINABCDE\\033[2D\\033[2@\\n"
                "PTY_INSERT_END\\n'",
                clipboard_dir,
                (
                    b"PTY_INSERT_BEGINABC  DE",
                    b"PTY_INSERT_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # CSI delete-character should shift remaining characters left.
            run_command(
                "handles ANSI character deletion",
                "printf 'PTY_DELETE_BEGINABCDE\\033[3D\\033[2P\\n"
                "PTY_DELETE_END\\n'",
                clipboard_dir,
                (
                    b"PTY_DELETE_BEGINABE",
                    b"PTY_DELETE_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # CSI 0 K should erase text from the cursor to the line end.
            run_command(
                "handles erase-to-end-of-line",
                "printf 'PTY_ERASE_BEGINDELETE_THIS\\033[11D"
                "\\033[0KPTY_ERASE_END\\n'",
                clipboard_dir,
                (b"PTY_ERASE_BEGINPTY_ERASE_END",),
                (b"DELETE_THIS", b"\xef\xbf\xbd"),
            )

            # OSC terminal-title sequences must not leak into captured text.
            run_command(
                "strips OSC terminal-title sequences",
                "printf 'PTY_OSC_BEGIN\\033]0;"
                "PTY_OSC_TITLE_SHOULD_NOT_LEAK\\007"
                "PTY_OSC_END\\n'",
                clipboard_dir,
                (b"PTY_OSC_BEGIN", b"PTY_OSC_END"),
                (
                    b"PTY_OSC_TITLE_SHOULD_NOT_LEAK",
                    b"\x1b",
                    b"\xef\xbf\xbd",
                ),
            )

            # Send several commands without waiting between them.
            burst_before = len(clipboard_files(clipboard_dir))
            burst_commands = [
                "printf 'PTY_BURST_1\\n'",
                "printf 'PTY_BURST_2\\n'",
                "printf 'PTY_BURST_3\\n'",
            ]
            os.write(
                master,
                ("\n".join(burst_commands) + "\n").encode("utf-8"),
            )
            burst_payloads = wait_for_payload(
                clipboard_dir, burst_before + len(burst_commands)
            )
            burst_payloads = burst_payloads[
                burst_before:burst_before + len(burst_commands)
            ]

            for index, payload in enumerate(burst_payloads, start=1):
                expected = f"PTY_BURST_{index}".encode()
                forbidden = [
                    f"PTY_BURST_{other}".encode()
                    for other in range(1, len(burst_commands) + 1)
                    if other != index
                ]
                check(
                    f"rapid command {index} captured separately and in order",
                    expected in payload
                    and all(marker not in payload for marker in forbidden),
                    payload,
                )

            # Stress capture with output much larger than a terminal screen.
            large_payload = run_command(
                "captures large terminal output markers",
                "printf 'PTY_LARGE_BEGIN\\n'; "
                "head -c 262144 /dev/zero | tr '\\000' 'X'; "
                "printf '\\nPTY_LARGE_END\\n'",
                clipboard_dir,
                (b"PTY_LARGE_BEGIN", b"PTY_LARGE_END"),
            )
            x_count = large_payload.count(b"X")
            check(
                "captures all large terminal output characters",
                x_count == 262144,
                f"expected 262144 X bytes, found {x_count}; "
                f"payload length={len(large_payload)} bytes",
            )

            # Interrupt a foreground command and require the shell to recover.
            before = len(clipboard_files(clipboard_dir))
            buffer.clear()
            os.write(
                master,
                b"printf 'PTY_INTERRUPT_READY\\n'; sleep 30\n",
            )
            read_until(START)
            read_until(b"PTY_INTERRUPT_READY")
            time.sleep(0.2)
            os.write(master, b"\x03")
            payloads = wait_for_payload(
                clipboard_dir, before + 1, timeout=10
            )
            interrupted_payload = payloads[before]
            check(
                "Ctrl+C interrupts foreground command",
                b"[Terminal output]" in interrupted_payload,
                interrupted_payload,
            )

            run_command(
                "capture recovers after Ctrl+C",
                "printf 'PTY_AFTER_CTRL_C_OK\\n'",
                clipboard_dir,
                (b"PTY_AFTER_CTRL_C_OK",),
            )

            run_command(
                "silent failure produces no-visible-output marker",
                "false",
                clipboard_dir,
                (b"[NO_VISIBLE_OUTPUT]",),
            )

            run_command(
                "capture recovers after failed command",
                "printf 'PTY_AFTER_FAILURE_OK\\n'",
                clipboard_dir,
                (b"PTY_AFTER_FAILURE_OK",),
            )

            fixture = work / "bridge-fixture.txt"
            fixture.write_bytes(b"PTY_FILE_BRIDGE_OK\n")
            run_command(
                "file bridge crosses the control channel",
                f"cvb {fixture}",
                clipboard_dir,
                (b"PTY_FILE_BRIDGE_OK",),
            )

            os.write(master, b"exit\n")
            deadline = time.monotonic() + 10
            status = None
            while time.monotonic() < deadline:
                result = os.waitpid(child_pid, os.WNOHANG)
                if result[0]:
                    status = os.waitstatus_to_exitcode(result[1])
                    child_pid = None
                    break
                time.sleep(0.05)

            check("interactive session exits cleanly", status == 0, status)

        except Exception as error:
            diagnostic = bytes(buffer).decode("utf-8", "replace")
            details = f"{error}\n{diagnostic}"
            prerequisite_markers = (
                "libinput",
                "udev context",
                "seat0",
            )

            if passed == 0 and any(
                marker in details.lower()
                for marker in prerequisite_markers
            ):
                print(
                    "SKIP: interactive PTY unavailable because libinput "
                    "could not initialize."
                )
                print(details[-2000:])
                return 0

            check("PTY harness completed", False, details[-2000:])

        finally:
            if child_pid is not None:
                try:
                    os.kill(child_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    os.waitpid(child_pid, 0)
                except ChildProcessError:
                    pass
            if master is not None:
                os.close(master)

    print(f"\n{passed} passed, {failed} failed", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

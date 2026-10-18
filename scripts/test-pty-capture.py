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


def resize_terminal(rows, cols):
    fcntl.ioctl(master, 0x5414, struct.pack("HHHH", rows, cols, 0, 0))


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


def drain_terminal(quiet=0.15):
    """Consume terminal output until the PTY has been quiet for `quiet` seconds."""
    while True:
        ready, _, _ = select.select([master], [], [], quiet)
        if not ready:
            return
        try:
            data = os.read(master, 65536)
        except OSError:
            return
        if not data:
            return
        buffer.extend(data)


def run_skipped(name, command, clipboard_dir, settle=0.5):
    """Run a command that must leave the clipboard untouched.

    Waiting for the END marker proves the command line has finished, so a
    typed-ahead line can never be echoed inside this command's capture
    window. The settle delay then gives any wrongly dispatched capture time
    to land before the capture count is compared.
    """
    drain_terminal()
    buffer.clear()
    before = len(clipboard_files(clipboard_dir))
    os.write(master, command.encode("utf-8") + b"\n")
    read_until(END)
    time.sleep(settle)
    after = len(clipboard_files(clipboard_dir))
    check(name, after == before, f"{after - before} new capture(s)")


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

            # Clearing the display must remove old text from the rendered screen.
            run_command(
                "handles full-screen erase",
                "printf 'PTY_CLEAR_OLD_BEGIN\\nPTY_CLEAR_OLD_LINE\\n'; "
                "printf '\\033[H\\033[2J'; "
                "printf 'PTY_CLEAR_NEW_SCREEN\\n'",
                clipboard_dir,
                (b"PTY_CLEAR_NEW_SCREEN",),
                (b"PTY_CLEAR_OLD_BEGIN", b"PTY_CLEAR_OLD_LINE"),
            )

            # CSI 2K should remove the selected line without losing its neighbors.
            run_command(
                "erases a line without retaining erased text",
                "printf '\\033[2J\\033[H'; "
                "printf 'PTY_DLINE_BEGIN\\nPTY_DLINE_REMOVE\\n"
                "PTY_DLINE_END\\n'; "
                "printf '\\033[2A\\033[2K'",
                clipboard_dir,
                (b"PTY_DLINE_BEGIN", b"PTY_DLINE_END"),
                (b"PTY_DLINE_REMOVE",),
            )

            # CSI L inserts a blank row and shifts the following rows down.
            run_command(
                "handles ANSI line insertion",
                "printf '\\033[2J\\033[H'; "
                "printf 'PTY_IL_TOP\\nPTY_IL_MIDDLE\\nPTY_IL_BOTTOM'; "
                "printf '\\033[2;1H\\033[1L'; "
                "printf '\\033[5;1HPTY_IL_END\\n'",
                clipboard_dir,
                (
                    b"PTY_IL_TOP\n\nPTY_IL_MIDDLE\n"
                    b"PTY_IL_BOTTOM\nPTY_IL_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Scrolling inside a restricted region must not move the rows outside it.
            run_command(
                "respects ANSI scrolling-region boundaries",
                "printf '\\033[2J\\033[HPTY_REGION_TOP\\n"
                "PTY_REGION_ONE\\nPTY_REGION_TWO\\nPTY_REGION_BOTTOM'; "
                "printf '\\033[2;3r\\033[3;1H\\n\\033[r'",
                clipboard_dir,
                (
                    b"PTY_REGION_TOP",
                    b"PTY_REGION_TWO",
                    b"PTY_REGION_BOTTOM",
                ),
                (b"PTY_REGION_ONE",),
            )

            # A CSI sequence split across writes must still be parsed as one sequence.
            run_command(
                "handles CSI sequences split across writes",
                "printf 'PTY_SPLIT_CSI_BEGIN\\n'; "
                "printf '\\033'; sleep 0.05; "
                "printf '[31mPTY_SPLIT_CSI_TEXT\\033'; sleep 0.05; "
                "printf '[0m\\nPTY_SPLIT_CSI_END\\n'",
                clipboard_dir,
                (
                    b"PTY_SPLIT_CSI_BEGIN",
                    b"PTY_SPLIT_CSI_TEXT",
                    b"PTY_SPLIT_CSI_END",
                ),
                (b"\x1b", b"\xef\xbf\xbd"),
            )

            # An OSC sequence split across writes must neither leak nor corrupt text.
            run_command(
                "handles OSC sequences split across writes",
                "printf 'PTY_SPLIT_OSC_BEGIN\\n'; "
                "printf '\\033]0;PTY_SPLIT_OSC_TITLE'; sleep 0.05; "
                "printf '\\007'; "
                "printf 'PTY_SPLIT_OSC_END\\n'",
                clipboard_dir,
                (b"PTY_SPLIT_OSC_BEGIN", b"PTY_SPLIT_OSC_END"),
                (
                    b"PTY_SPLIT_OSC_TITLE",
                    b"\x1b",
                    b"\xef\xbf\xbd",
                ),
            )

            # The renderer must use the terminal width after a resize.
            resize_terminal(24, 40)
            time.sleep(0.1)
            try:
                run_command(
                    "renders output at the resized terminal width",
                    "sleep 0.1; "
                    "printf '\\033[2J\\033[HPTY_RESIZE_BEFORE_BEGIN\\n'; "
                    "printf '%050d' 0; "
                    "printf '\\nPTY_RESIZE_BEFORE_END\\n'",
                    clipboard_dir,
                    (
                        b"PTY_RESIZE_BEFORE_BEGIN",
                        b"0" * 40 + b"\n" + b"0" * 10,
                        b"PTY_RESIZE_BEFORE_END",
                    ),
                    (b"\xef\xbf\xbd",),
                )
            finally:
                resize_terminal(24, 80)
                time.sleep(0.1)

            # Resizing while a command is running must also update transcript rendering.
            resize_before = len(clipboard_files(clipboard_dir))
            buffer.clear()
            os.write(
                master,
                b"printf 'PTY_RESIZE_MID_PHASE\\n'; "
                b"sleep 0.4; "
                b"printf '%050d' 0; "
                b"printf '\\nPTY_RESIZE_MID_END\\n'\n",
            )
            try:
                read_until(START)
                read_until(b"PTY_RESIZE_MID_PHASE")
                resize_terminal(24, 40)
                resize_payloads = wait_for_payload(
                    clipboard_dir, resize_before + 1, timeout=10
                )
                resize_payload = resize_payloads[resize_before]
                check(
                    "tracks terminal resizing during active capture",
                    b"PTY_RESIZE_MID_PHASE" in resize_payload
                    and b"0" * 40 + b"\n" + b"0" * 10 in resize_payload
                    and b"PTY_RESIZE_MID_END" in resize_payload
                    and b"\xef\xbf\xbd" not in resize_payload,
                    resize_payload,
                )
            finally:
                resize_terminal(24, 80)
                time.sleep(0.1)

            # Output sent to the alternate screen must not contaminate the main screen.
            run_command(
                "isolates alternate-screen contents",
                "printf 'PTY_ALT_MAIN_BEGIN\\n'; "
                "printf '\\033[?1049hPTY_ALT_SCREEN_SHOULD_NOT_LEAK'; "
                "sleep 0.05; "
                "printf '\\033[?1049l'; "
                "printf 'PTY_ALT_MAIN_END\\n'",
                clipboard_dir,
                (b"PTY_ALT_MAIN_BEGIN", b"PTY_ALT_MAIN_END"),
                (b"PTY_ALT_SCREEN_SHOULD_NOT_LEAK", b"\xef\xbf\xbd"),
            )

            # Variation selectors should remain attached to their base character.
            run_command(
                "preserves emoji variation selectors",
                "printf 'PTY_VS16_BEGIN\\n❤️\\nPTY_VS16_END\\n'",
                clipboard_dir,
                (
                    b"PTY_VS16_BEGIN",
                    "❤️".encode("utf-8"),
                    b"PTY_VS16_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # Supplementary-plane UTF-8 must survive terminal rendering unchanged.
            run_command(
                "preserves supplementary-plane Unicode",
                "printf 'PTY_SUPPLEMENTARY_BEGIN\\n𐍈\\n"
                "PTY_SUPPLEMENTARY_END\\n'",
                clipboard_dir,
                (
                    b"PTY_SUPPLEMENTARY_BEGIN",
                    "𐍈".encode("utf-8"),
                    b"PTY_SUPPLEMENTARY_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # A double-width glyph at the right edge must wrap as a whole glyph.
            run_command(
                "wraps wide Unicode at the right margin",
                "printf '\\033[2J\\033[H'; "
                "printf '%079d' 0 | tr '0' 'A'; "
                "printf '界X\\nPTY_WIDE_MARGIN_END\\n'",
                clipboard_dir,
                (
                    b"A" * 79 + b"\n" + "界X".encode("utf-8"),
                    b"PTY_WIDE_MARGIN_END",
                ),
                (b"\xef\xbf\xbd",),
            )

            # A genuinely empty capture must not reuse a preceding command's output.
            run_command(
                "isolation setup leaves a unique prior marker",
                "printf 'PTY_ISOLATION_PREVIOUS_ONLY\\n'",
                clipboard_dir,
                (b"PTY_ISOLATION_PREVIOUS_ONLY",),
            )
            run_command(
                "empty capture does not retain previous output",
                ":",
                clipboard_dir,
                (b"[NO_VISIBLE_OUTPUT]",),
                (b"PTY_ISOLATION_PREVIOUS_ONLY",),
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

            run_command(
                "capture state resets after a large payload",
                "printf 'PTY_AFTER_LARGE_OK\\n'",
                clipboard_dir,
                (b"PTY_AFTER_LARGE_OK",),
                (b"PTY_LARGE_BEGIN", b"X" * 128),
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

            # Skip list: CVB creates it on first start with `cd` listed.
            skip_file = cvb_config / "skip"
            check(
                "skip list is created with cd listed",
                skip_file.is_file()
                and "cd" in skip_file.read_text().splitlines(),
                skip_file.read_text() if skip_file.is_file() else "missing",
            )

            run_command(
                "skip setup leaves a unique prior capture",
                "printf 'PTY_SKIP_PREVIOUS\\n'",
                clipboard_dir,
                (b"PTY_SKIP_PREVIOUS",),
            )
            skip_before = len(clipboard_files(clipboard_dir))
            run_skipped(
                "listed silent command does not touch the clipboard",
                "cd /",
                clipboard_dir,
            )
            # The sentinel's command text is `PTY_SKIP_%s`, so the expected
            # `PTY_SKIP_NEXT` can only come from real command output, never
            # from an echoed command line.
            run_command(
                "skip sentinel is the next capture",
                "printf 'PTY_SKIP_%s\\n' NEXT",
                clipboard_dir,
                (b"PTY_SKIP_NEXT",),
                (b"[NO_VISIBLE_OUTPUT]", b"%s"),
            )
            check(
                "listed silent command adds no capture",
                len(clipboard_files(clipboard_dir)) == skip_before + 1,
                len(clipboard_files(clipboard_dir)) - skip_before,
            )

            run_command(
                "listed command that fails captures its error",
                "cd /cvb-no-such-directory",
                clipboard_dir,
                (b"No such file or directory",),
                (b"[NO_VISIBLE_OUTPUT]",),
            )

            run_command(
                "listed command followed by unlisted command is captured",
                "cd / && printf 'PTY_SKIP_MIXED\\n'",
                clipboard_dir,
                (b"PTY_SKIP_MIXED",),
            )

            run_command(
                "unlisted silent command still captures the marker",
                "true",
                clipboard_dir,
                (b"[NO_VISIBLE_OUTPUT]",),
            )

            # Edits to the skip list apply to the running session.
            skip_file.write_text("# nothing skipped\n")
            run_command(
                "removed entry is captured again",
                "cd /",
                clipboard_dir,
                (b"[NO_VISIBLE_OUTPUT]",),
            )
            skip_file.write_text("cd\n")

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

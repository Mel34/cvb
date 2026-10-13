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
            print(f"     {detail!r}", flush=True)


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

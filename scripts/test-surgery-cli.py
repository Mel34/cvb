#!/usr/bin/env python3
"""Integration tests for the CVB surgery CLI confirmation workflow."""

import errno
import os
import pathlib
import pty
import select
import subprocess
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[1]
BINARY = REPO / "target" / "debug" / "cvb"

ENVELOPE = b"""CVB::testtoken123::BEGIN v=1 id=test-id
CVB::testtoken123::FILE path=example.rs
CVB::testtoken123::OP replace lines=2-2
CVB::testtoken123::OLD n=1
    println!("hello");
CVB::testtoken123::NEW n=1
    println!("goodbye");
CVB::testtoken123::ENDOP
CVB::testtoken123::END ops=1
"""

ORIGINAL = 'fn greet() {\n    println!("hello");\n}\n'
PROMPT = b"Apply these changes? [y/N]: "
NOTTY = "E-NOTTY: interactive terminal confirmation requires /dev/tty"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def make_project(parent):
    root = pathlib.Path(parent) / "project"
    root.mkdir()
    target = root / "example.rs"
    target.write_text(ORIGINAL)
    return root, target


def require_unchanged(target):
    actual = target.read_text()
    require(
        actual == ORIGINAL,
        f"target file changed unexpectedly:\n{actual}",
    )
    require(
        'println!("goodbye");' not in actual,
        "proposed content was written despite the test not being confirmed",
    )


def test_spec():
    result = subprocess.run(
        [str(BINARY), "surgery", "--spec"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
        timeout=10,
    )

    require(result.returncode == 0, f"expected exit 0, got {result.returncode}")
    require(
        b"CVB Surgery Envelope (CSL), version 1" in result.stdout,
        f"specification header missing: {result.stdout!r}",
    )
    require(
        b"--dry-run" in result.stdout and b"--stdin" in result.stdout,
        f"specification usage options missing: {result.stdout!r}",
    )
    require(not result.stderr, f"unexpected stderr: {result.stderr!r}")


def test_dry_run():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-dry-run-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        result = subprocess.run(
            [
                str(BINARY), "surgery", "--dry-run", "--stdin",
                "--root", str(root),
            ],
            input=ENVELOPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, "XDG_STATE_HOME": str(state_home)},
            start_new_session=True,
            timeout=10,
        )

        require(result.returncode == 0, f"expected exit 0, got {result.returncode}")
        require(
            b'println!("goodbye");' in result.stdout,
            "dry-run output did not contain the proposed change",
        )
        require(
            PROMPT not in result.stdout and PROMPT not in result.stderr,
            "dry-run unexpectedly asked for interactive confirmation",
        )
        require_unchanged(target)
        require(
            not (state_home / "cvb" / "surgery").exists(),
            "surgery state was created during dry-run",
        )
        require(
            sorted(path.name for path in pathlib.Path(tmp).iterdir()) == ["project"],
            f"dry-run created unexpected workspace artifacts: "
            f"{sorted(path.name for path in pathlib.Path(tmp).iterdir())!r}",
        )
        require(
            sorted(path.name for path in root.iterdir()) == ["example.rs"],
            f"dry-run created unexpected project artifacts: "
            f"{sorted(path.name for path in root.iterdir())!r}",
        )


def test_missing_tty():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-no-tty-") as tmp:
        root, target = make_project(tmp)
        result = subprocess.run(
            [
                str(BINARY), "surgery", "--stdin",
                "--root", str(root),
            ],
            input=ENVELOPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            timeout=10,
        )

        require(result.returncode == 2, f"expected exit 2, got {result.returncode}")
        require(
            NOTTY.encode() in result.stderr,
            f"required E-NOTTY message missing: {result.stderr!r}",
        )
        require_unchanged(target)


def test_decline():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-decline-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        read_fd, write_fd = os.pipe()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(read_fd, 0)
                os.close(read_fd)
                os.close(write_fd)
                os.environ["XDG_STATE_HOME"] = str(state_home)
                os.execv(
                    str(BINARY),
                    [
                        str(BINARY), "surgery", "--stdin",
                        "--root", str(root),
                    ],
                )
            except BaseException:
                os._exit(127)

        os.close(read_fd)
        try:
            os.write(write_fd, ENVELOPE)
        finally:
            os.close(write_fd)

        output = bytearray()
        response_sent = False
        child_status = None
        deadline = time.monotonic() + 10

        try:
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master_fd], [], [], 0.1)
                if ready:
                    try:
                        data = os.read(master_fd, 4096)
                    except OSError as exc:
                        if exc.errno == errno.EIO:
                            data = b""
                        else:
                            raise
                    if data:
                        output.extend(data)

                if not response_sent and PROMPT in output:
                    os.write(master_fd, b"n\n")
                    response_sent = True

                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    child_status = status
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI did not exit within 10 seconds")

            # Drain output remaining after the child exits.
            while True:
                ready, _, _ = select.select([master_fd], [], [], 0)
                if not ready:
                    break
                try:
                    data = os.read(master_fd, 4096)
                except OSError as exc:
                    if exc.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
        finally:
            os.close(master_fd)

        require(response_sent, f"confirmation prompt not seen:\n{output.decode(errors='replace')}")
        require(os.WIFEXITED(child_status), f"child did not exit normally: {child_status}")
        exit_code = os.WEXITSTATUS(child_status)
        require(exit_code == 3, f"expected decline exit 3, got {exit_code}")
        require(
            PROMPT in output,
            "confirmation prompt was not present on the controlling terminal",
        )
        require_unchanged(target)
        require(
            not (state_home / "cvb" / "surgery").exists(),
            "surgery state was created despite declined confirmation",
        )


def test_confirmed_apply():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-confirmed-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        read_fd, write_fd = os.pipe()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(read_fd, 0)
                os.close(read_fd)
                os.close(write_fd)
                os.environ["XDG_STATE_HOME"] = str(state_home)
                os.execv(
                    str(BINARY),
                    [
                        str(BINARY), "surgery", "--stdin",
                        "--root", str(root),
                    ],
                )
            except BaseException:
                os._exit(127)

        os.close(read_fd)
        try:
            os.write(write_fd, ENVELOPE)
        finally:
            os.close(write_fd)

        output = bytearray()
        response_sent = False
        child_status = None
        deadline = time.monotonic() + 10

        try:
            while time.monotonic() < deadline:
                ready, _, _ = select.select([master_fd], [], [], 0.1)
                if ready:
                    try:
                        data = os.read(master_fd, 4096)
                    except OSError as exc:
                        if exc.errno == errno.EIO:
                            data = b""
                        else:
                            raise
                    if data:
                        output.extend(data)

                if not response_sent and PROMPT in output:
                    os.write(master_fd, b"y\n")
                    response_sent = True

                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    child_status = status
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI did not exit within 10 seconds")

            # Drain output remaining after the child exits.
            while True:
                ready, _, _ = select.select([master_fd], [], [], 0)
                if not ready:
                    break
                try:
                    data = os.read(master_fd, 4096)
                except OSError as exc:
                    if exc.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
        finally:
            os.close(master_fd)

        require(response_sent, f"confirmation prompt not seen:\n{output.decode(errors='replace')}")
        require(os.WIFEXITED(child_status), f"child did not exit normally: {child_status}")
        exit_code = os.WEXITSTATUS(child_status)
        require(exit_code == 0, f"expected successful exit 0, got {exit_code}")
        require(
            PROMPT in output,
            "confirmation prompt was not present on the controlling terminal",
        )
        expected = ORIGINAL.replace('println!("hello");', 'println!("goodbye");')
        require(
            target.read_text() == expected,
            f"confirmed changes were not applied correctly: {target.read_text()!r}",
        )
        require(
            (state_home / "cvb" / "surgery").exists(),
            "surgery state was not created after confirmed application",
        )


def main():
    build = subprocess.run(
        ["cargo", "build", "--quiet", "--bin", "cvb"],
        cwd=REPO,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if build.returncode != 0:
        raise SystemExit(
            "FAIL: cargo build --bin cvb\n"
            + build.stdout
            + build.stderr
        )

    tests = [
        ("spec output", test_spec),
        ("dry-run", test_dry_run),
        ("missing TTY", test_missing_tty),
        ("declined confirmation", test_decline),
        ("confirmed apply", test_confirmed_apply),
    ]
    failures = []

    for name, test in tests:
        try:
            test()
        except Exception as exc:
            failures.append((name, exc))
            print(f"FAIL: {name}: {exc}")
        else:
            print(f"PASS: {name}")

    print(f"Surgery CLI: {len(tests) - len(failures)} passed, {len(failures)} failed")
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()

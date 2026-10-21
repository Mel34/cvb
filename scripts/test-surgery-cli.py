#!/usr/bin/env python3
"""Integration tests for the CVB surgery CLI confirmation workflow."""

import atexit
import errno
import os
import pathlib
import pty
import re
import select
import socket
import subprocess
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[1]

def cvb_env():
    env = {**os.environ}
    if "CVB_BASH_SOURCE" in env:
        del env["CVB_BASH_SOURCE"]
    return env

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


def get_stty(fd):
    return subprocess.check_output(["stty", "-g"], stdin=fd, text=True).strip()


def test_spec():
    test_token = "SpecToken1234567"
    env = {**os.environ, "CVB_TOKEN": test_token}
    env.pop("CVB_BASH_SOURCE", None)
    result = subprocess.run(
        [str(BINARY), "surgery", "--spec"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
        timeout=10,
    )

    require(result.returncode == 0, f"expected exit 0, got {result.returncode}")
    require(
        b"CVB Surgery Language (CSL), protocol version 1" in result.stdout,
        f"specification header missing: {result.stdout!r}",
    )

    token_match = re.search(
        rb"^CVB::([A-Za-z0-9]{16})::BEGIN v=1 id=example-replace$",
        result.stdout,
        re.MULTILINE,
    )
    require(token_match is not None, "example-replace BEGIN or token missing")
    token = token_match.group(1)
    require(token == test_token.encode("ascii"), f"expected token {test_token!r}, got {token!r}")

    for example in (b"example-replace", b"example-delete", b"example-insert"):
        marker = b"CVB::" + token + b"::BEGIN v=1 id=" + example
        require(marker in result.stdout, f"example missing or token inconsistent: {example!r}")

    for fragment in (
        b"3 MiB",
        b"8 MiB",
        b"32 MiB",
        b"500 operations",
        b"E-LIMIT",
        b"E-LOCK",
        b"E-NOTTY",
        b"E-REPLAY",
        b"E-RECOVER",
        b"E-STALE",
        b"E-IO",
        b"E-NOSESSION",
        b"E-TOKEN",
    ):
        require(fragment in result.stdout, f"specification content missing: {fragment!r}")

    require(b"{TOKEN}" not in result.stdout, "unresolved token placeholder in specification")
    require(b"  CVB::" not in result.stdout, "protocol control lines are unexpectedly indented")
    require(not result.stderr, f"unexpected stderr: {result.stderr!r}")

    # Verify cvb spec is byte-identical to cvb surgery --spec
    spec_cmd_result = subprocess.run(
        [str(BINARY), "spec"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        start_new_session=True,
        timeout=10,
    )
    require(spec_cmd_result.returncode == 0, f"expected cvb spec exit 0, got {spec_cmd_result.returncode}")
    require(spec_cmd_result.stdout == result.stdout, "cvb spec output must be byte-identical to cvb surgery --spec")


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
            env={**cvb_env(), "XDG_STATE_HOME": str(state_home), "CVB_TOKEN": "testtoken123"},
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


def test_missing_session_token():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-no-session-") as tmp:
        root, target = make_project(tmp)
        env = os.environ.copy()
        env.pop("CVB_TOKEN", None)

        result = subprocess.run(
            [
                str(BINARY), "surgery", "--stdin",
                "--root", str(root),
            ],
            input=ENVELOPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            start_new_session=True,
            timeout=10,
        )

        require(result.returncode == 2, f"expected exit 2, got {result.returncode}")
        require(
            b"E-NOSESSION" in result.stderr,
            f"expected E-NOSESSION before input parsing, got: {result.stderr!r}",
        )
        require_unchanged(target)


def test_mismatched_session_token():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-bad-token-") as tmp:
        root, target = make_project(tmp)
        envelope = ENVELOPE.replace(
            b"testtoken123", b"differenttoken123"
        )
        result = subprocess.run(
            [
                str(BINARY), "surgery", "--stdin", "--dry-run",
                "--root", str(root),
            ],
            input=envelope,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**cvb_env(), "CVB_TOKEN": "testtoken123"},
            start_new_session=True,
            timeout=10,
        )

        require(result.returncode == 2, f"expected exit 2, got {result.returncode}")
        require(
            b"E-TOKEN" in result.stderr,
            f"expected E-TOKEN for a mismatched session token, got: {result.stderr!r}",
        )
        require_unchanged(target)


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
            env={**cvb_env(), "CVB_TOKEN": "testtoken123"},
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
                os.environ["CVB_TOKEN"] = "testtoken123"
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

        stty_before = get_stty(master_fd)
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
            stty_after = get_stty(master_fd)
            os.close(master_fd)
        require(stty_after == stty_before, f"terminal state was not restored: before={stty_before} after={stty_after}")

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
                os.environ["CVB_TOKEN"] = "testtoken123"
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

        stty_before = get_stty(master_fd)
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
            stty_after = get_stty(master_fd)
            os.close(master_fd)
        require(stty_after == stty_before, f"terminal state was not restored: before={stty_before} after={stty_after}")

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


def test_precondition_failure_reports_clean_e_precond():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-precond-") as tmp:
        root, target = make_project(tmp)
        # target contains ORIGINAL with `println!("hello");` on line 2.
        # Send an envelope expecting `println!("mismatched");` to trigger E-PRECOND.
        envelope = b"""CVB::testtoken123::BEGIN v=1 id=test-precond
CVB::testtoken123::FILE path=example.rs
CVB::testtoken123::OP replace lines=2-2
CVB::testtoken123::OLD n=1
    println!("mismatched");
CVB::testtoken123::NEW n=1
    println!("goodbye");
CVB::testtoken123::ENDOP
CVB::testtoken123::END ops=1
"""
        result = subprocess.run(
            [
                str(BINARY), "surgery", "--stdin",
                "--root", str(root),
            ],
            input=envelope,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**cvb_env(), "CVB_TOKEN": "testtoken123"},
            start_new_session=True,
            timeout=10,
        )

        require(result.returncode == 2, f"expected exit 2, got {result.returncode}")
        stderr = result.stderr.decode(errors="replace")
        require(
            "Plan(PlanFailure" not in stderr,
            f"debug structure found in stderr: {stderr}",
        )
        require(
            "CVB: E-PRECOND:" in stderr,
            f"expected 'CVB: E-PRECOND:' in stderr, got: {stderr}",
        )
        require_unchanged(target)


def test_terminal_restoration_on_sigterm():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-sigterm-") as tmp:
        root, target = make_project(tmp)
        read_fd, write_fd = os.pipe()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(read_fd, 0)
                os.close(read_fd)
                os.close(write_fd)
                os.environ["CVB_TOKEN"] = "testtoken123"
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

        stty_before = get_stty(master_fd)
        output = bytearray()
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

                if PROMPT in output:
                    # Terminal guard has entered raw mode; send SIGTERM
                    os.kill(pid, 15)
                    _, child_status = os.waitpid(pid, 0)
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI did not enter prompt or exit within 10 seconds")
        finally:
            stty_after = get_stty(master_fd)
            os.close(master_fd)

        require(os.WIFSIGNALED(child_status), f"child expected to be signaled: {child_status}")
        require(os.WTERMSIG(child_status) == 15, f"expected SIGTERM (15), got {os.WTERMSIG(child_status)}")
        require(stty_after == stty_before, f"terminal state was not restored after SIGTERM: before={stty_before} after={stty_after}")
        require_unchanged(target)


def test_mode_bracketed_paste_and_abort():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-mode-abort-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        ctrl_parent, ctrl_child = socket.socketpair()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(ctrl_child.fileno(), 3)
                ctrl_child.close()
                ctrl_parent.close()
                os.environ["XDG_STATE_HOME"] = str(state_home)
                os.environ["CVB_TOKEN"] = "testtoken123"
                os.execv(
                    str(BINARY),
                    [
                        str(BINARY), "surgery",
                        "--root", str(root),
                    ],
                )
            except BaseException:
                os._exit(127)

        ctrl_child.close()

        stty_before = get_stty(master_fd)
        output = bytearray()
        paste_sent = False
        abort_sent = False
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

                if not paste_sent and b"[CVB surgery]" in output:
                    os.write(master_fd, b"\x1b[200~" + ENVELOPE + b"\x1b[201~")
                    paste_sent = True

                if paste_sent and not abort_sent and b"println!(\"goodbye\");" in output:
                    time.sleep(0.05)
                    os.write(master_fd, b"q")
                    abort_sent = True

                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    child_status = status
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI mode did not exit within 10 seconds")
        finally:
            ctrl_parent.close()
            stty_after = get_stty(master_fd)
            os.close(master_fd)

        require(stty_after == stty_before, f"terminal state not restored: before={stty_before} after={stty_after}")
        require(os.WIFEXITED(child_status), f"child did not exit normally: {child_status}")
        exit_code = os.WEXITSTATUS(child_status)
        require(exit_code == 3, f"expected abort exit 3, got {exit_code}")
        require_unchanged(target)


def test_mode_two_press_apply():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-mode-apply-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        ctrl_parent, ctrl_child = socket.socketpair()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(ctrl_child.fileno(), 3)
                ctrl_child.close()
                ctrl_parent.close()
                os.environ["XDG_STATE_HOME"] = str(state_home)
                os.environ["CVB_TOKEN"] = "testtoken123"
                os.execv(
                    str(BINARY),
                    [
                        str(BINARY), "surgery",
                        "--root", str(root),
                    ],
                )
            except BaseException:
                os._exit(127)

        ctrl_child.close()

        stty_before = get_stty(master_fd)
        output = bytearray()
        paste_sent = False
        first_b_sent = False
        second_b_sent = False
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

                if not paste_sent and b"[CVB surgery]" in output:
                    os.write(master_fd, b"\x1b[200~" + ENVELOPE + b"\x1b[201~")
                    paste_sent = True

                if paste_sent and not first_b_sent and b"println!(\"goodbye\");" in output:
                    time.sleep(0.05)
                    os.write(master_fd, b"\x02") # Ctrl+B
                    first_b_sent = True

                if first_b_sent and not second_b_sent and b"again to build" in output:
                    require_unchanged(target) # target must NOT be modified after single Ctrl+B
                    time.sleep(0.05)
                    os.write(master_fd, b"\x02") # 2nd Ctrl+B fires build
                    second_b_sent = True

                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    child_status = status
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI mode did not exit within 10 seconds")
        finally:
            ctrl_parent.close()
            stty_after = get_stty(master_fd)
            os.close(master_fd)

        require(stty_after == stty_before, f"terminal state not restored: before={stty_before} after={stty_after}")
        require(os.WIFEXITED(child_status), f"child did not exit normally: {child_status}")
        exit_code = os.WEXITSTATUS(child_status)
        require(exit_code == 0, f"expected apply exit 0, got {exit_code}")
        expected = ORIGINAL.replace('println!("hello");', 'println!("goodbye");')
        require(target.read_text() == expected, f"target content mismatch: {target.read_text()!r}")


def test_mode_stale_file_stays_open_and_recovers():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-mode-stale-") as tmp:
        root, target = make_project(tmp)
        state_home = pathlib.Path(tmp) / "state-home"
        ctrl_parent, ctrl_child = socket.socketpair()
        pid, master_fd = pty.fork()

        if pid == 0:
            try:
                os.dup2(ctrl_child.fileno(), 3)
                ctrl_child.close()
                ctrl_parent.close()
                os.environ["XDG_STATE_HOME"] = str(state_home)
                os.environ["CVB_TOKEN"] = "testtoken123"
                os.execv(
                    str(BINARY),
                    [
                        str(BINARY), "surgery",
                        "--root", str(root),
                    ],
                )
            except BaseException:
                os._exit(127)

        ctrl_child.close()

        stty_before = get_stty(master_fd)
        output = bytearray()
        paste_sent = False
        target_modified = False
        arm_sent = False
        fire_sent = False
        abort_sent = False
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

                if not paste_sent and b"[CVB surgery]" in output:
                    os.write(master_fd, b"\x1b[200~" + ENVELOPE + b"\x1b[201~")
                    paste_sent = True

                if paste_sent and not target_modified and b"println!(\"goodbye\");" in output:
                    target.write_text("interfering concurrent edit\n")
                    target_modified = True

                if target_modified and not arm_sent:
                    time.sleep(0.05)
                    os.write(master_fd, b"\x02") # Ctrl+B (arm)
                    arm_sent = True

                if arm_sent and not fire_sent and b"again to build" in output:
                    time.sleep(0.05)
                    os.write(master_fd, b"\x02") # Ctrl+B (fire build -> encounters E-STALE)
                    fire_sent = True

                if fire_sent and not abort_sent and b"E-STALE" in output:
                    # Mode must still be alive!
                    time.sleep(0.05)
                    os.write(master_fd, b"q") # Abort
                    abort_sent = True

                waited_pid, status = os.waitpid(pid, os.WNOHANG)
                if waited_pid == pid:
                    child_status = status
                    break

            if child_status is None:
                os.kill(pid, 9)
                _, child_status = os.waitpid(pid, 0)
                raise AssertionError("CLI mode did not exit within 10 seconds")
        finally:
            ctrl_parent.close()
            stty_after = get_stty(master_fd)
            os.close(master_fd)

        require(stty_after == stty_before, f"terminal state not restored: before={stty_before} after={stty_after}")
        require(os.WIFEXITED(child_status), f"child did not exit normally: {child_status}")
        exit_code = os.WEXITSTATUS(child_status)
        require(exit_code == 3, f"expected abort exit 3 after stale recovery, got {exit_code}")
        require(target.read_text() == "interfering concurrent edit\n", "concurrent edit was overwritten")


def test_missing_control_socket_fails():
    with tempfile.TemporaryDirectory(prefix="cvb-surgery-missing-control-") as tmp:
        root, _ = make_project(tmp)
        proc = subprocess.run(
            [str(BINARY), "surgery", "--root", str(root)],
            env=dict(cvb_env(), CVB_TOKEN="testtoken123"),
            capture_output=True,
            text=True,
            close_fds=True,
        )
        require(proc.returncode == 2, f"expected exit code 2, got {proc.returncode}")
        require(
            "E-NOSESSION: interactive surgery requires an active CVB session" in proc.stderr,
            f"expected E-NOSESSION in stderr, got: {proc.stderr}",
        )


def main():
    # Keep surgery integration tests from overwriting the user's clipboard.
    fake_bin = tempfile.TemporaryDirectory(prefix="cvb-test-bin-")
    atexit.register(fake_bin.cleanup)

    fake_wl_copy = pathlib.Path(fake_bin.name) / "wl-copy"
    fake_wl_copy.write_text("#!/bin/sh\nexit 0\n")
    fake_wl_copy.chmod(0o755)

    os.environ["PATH"] = (
        f"{fake_bin.name}{os.pathsep}{os.environ.get('PATH', '')}"
    )
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
        ("missing session token", test_missing_session_token),
        ("mismatched session token", test_mismatched_session_token),
        ("missing TTY", test_missing_tty),
        ("missing control socket fails", test_missing_control_socket_fails),
        ("declined confirmation", test_decline),
        ("confirmed apply", test_confirmed_apply),
        ("precondition clean error", test_precondition_failure_reports_clean_e_precond),
        ("terminal restoration on SIGTERM", test_terminal_restoration_on_sigterm),
        ("interactive mode paste and abort", test_mode_bracketed_paste_and_abort),
        ("interactive mode two-press apply", test_mode_two_press_apply),
        ("interactive mode stale file recovery", test_mode_stale_file_stays_open_and_recovers),
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

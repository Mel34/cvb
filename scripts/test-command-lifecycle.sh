#!/usr/bin/env bash
# End-to-end tests for CVB's Bash command/capture lifecycle.
#
# Uses the real data/cvb-control.bash in interactive Bash under a PTY.
# Does not require Wayland, a clipboard, or the CVB executable.
#
# Run: scripts/test-command-lifecycle.sh

set -u

root=$(cd -- "$(dirname -- "$0")/.." && pwd)

for tool in bash python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'FAIL: required command not found: %s\n' "$tool" >&2
        exit 1
    fi
done

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 - "$root/data/cvb-control.bash" "$work" <<'PY'
import os
import pty
import select
import sys
import time

control_script, work = sys.argv[1:]
start_marker = b"\x1b]777;CVB;START\x07"
end_marker = b"\x1b]777;CVB;END\x07"
skip_marker = b"\x1b]777;CVB;SKIP\x07"

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


def read_until(marker, timeout=5):
    """Return bytes before marker, retaining any bytes after it."""
    deadline = time.monotonic() + timeout

    while True:
        position = buffer.find(marker)
        if position >= 0:
            result = bytes(buffer[:position])
            del buffer[:position + len(marker)]
            return result

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for marker {marker!r}")

        ready, _, _ = select.select([master], [], [], remaining)
        if not ready:
            continue

        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            raise RuntimeError(f"PTY read failed: {error}") from error

        if not chunk:
            raise RuntimeError("Bash exited before emitting the expected marker")

        buffer.extend(chunk)


def run_command(name, command, expected=None, expect_empty=False):
    """Run one complete Bash command and inspect its captured output."""
    try:
        os.write(master, command.encode() + b"\n")
        read_until(start_marker)
        output = read_until(end_marker)

        if expect_empty:
            check(name, output == b"", output)
        else:
            check(
                name,
                expected is not None and expected in output,
                output,
            )
        return output
    except Exception as error:
        check(name, False, str(error))
        raise


try:
    home = os.path.join(work, "home")
    os.makedirs(home, exist_ok=True)

    history_file = os.path.join(work, "history")
    parent_history = os.path.join(work, "parent-history")
    open(history_file, "wb").close()
    open(parent_history, "wb").close()

    skip_file = os.path.join(work, "skip")
    with open(skip_file, "w", encoding="utf-8") as skip:
        skip.write("# Commands that may be skipped\n\ncd\n  pushd   # trailing comment\n")

    rcfile = os.path.join(work, "test.rc")
    with open(rcfile, "w", encoding="utf-8") as rc:
        rc.write("set +o history\n")
        rc.write("PS1='PROMPT> '\n")
        rc.write(f"source {control_script!r}\n")
        rc.write("cvb_control_history_start\n")

    pid, master = pty.fork()

    if pid == 0:
        os.environ["HOME"] = home
        os.environ["HISTFILE"] = history_file
        os.environ["CVB_HISTORY_FILE"] = history_file
        os.environ["CVB_PARENT_HISTORY_FILE"] = parent_history
        os.environ["CVB_SKIP_FILE"] = skip_file
        os.environ["TERM"] = "xterm"
        os.environ["PS1"] = "PROMPT> "
        os.execvp(
            "bash",
            ["bash", "--noprofile", "--rcfile", rcfile, "-i"],
        )

    child_pid = pid

    # The initial prompt hook emits END before the first command.
    read_until(end_marker)

    # 1. Ordinary single-line command.
    run_command(
        "single command",
        "printf 'SINGLE_COMMAND_OK\\n'",
        b"SINGLE_COMMAND_OK",
    )

    # 2. A multiline conditional is one complete command.
    run_command(
        "multiline if block",
        """if true; then
    printf 'IF_BLOCK_OK\\n'
else
    printf 'WRONG_BRANCH\\n'
fi""",
        b"IF_BLOCK_OK",
    )

    # 3. Nested control flow executes and captures all iterations.
    output = run_command(
        "nested loop block",
        """for outer in A B; do
    for inner in 1 2; do
        printf 'LOOP_%s_%s\\n' "$outer" "$inner"
    done
done""",
        b"LOOP_A_1",
    )
    for item in (b"LOOP_A_2", b"LOOP_B_1", b"LOOP_B_2"):
        check(f"nested loop includes {item.decode()}", item in output, output)

    # 4. Define the function as one complete multiline command.
    run_command(
        "multiline function definition",
        """cvb_test_function() {
    printf 'FUNCTION_BLOCK_OK\\n'
}""",
        expect_empty=True,
    )

    # Invoke it separately: Bash treats this as a second command.
    run_command(
        "function invocation",
        "cvb_test_function",
        b"FUNCTION_BLOCK_OK",
    )

    # 5. A heredoc preserves content and blank lines.
    output = run_command(
        "heredoc content",
        """cat <<'CVB_TEST_EOF'
HEREDOC_LINE_ONE

HEREDOC_LINE_TWO
CVB_TEST_EOF""",
        b"HEREDOC_LINE_ONE",
    )
    check("heredoc preserves blank line", b"ONE\r\n\r\nHEREDOC_LINE_TWO" in output, output)
    check("heredoc preserves final line", b"HEREDOC_LINE_TWO" in output, output)

    # 6. A silent, failing command still reaches its end marker.
    run_command(
        "silent failing command",
        "false",
        expect_empty=True,
    )

    # 7. The next command starts cleanly after the failure.
    run_command(
        "command after failure",
        "printf 'AFTER_FAILURE_OK\\n'",
        b"AFTER_FAILURE_OK",
    )

    # 8. Adjacent commands each produce their own capture.
    run_command(
        "first adjacent command",
        "printf 'ADJACENT_FIRST\\n'",
        b"ADJACENT_FIRST",
    )
    run_command(
        "second adjacent command",
        "printf 'ADJACENT_SECOND\\n'",
        b"ADJACENT_SECOND",
    )

    # 9. Skip list: the SKIP marker is emitted only when every top-level
    #    command of the line is listed. Whether the capture is dropped is
    #    decided by CVB from the output, so errors still carry the marker here.
    def run_skip(name, command, expect_skip, expected=None):
        try:
            os.write(master, command.encode() + b"\n")
            read_until(start_marker)
            output = read_until(end_marker)
            has_skip = skip_marker in output
            ok = has_skip == expect_skip and (expected is None or expected in output)
            check(name, ok, output)
        except Exception as error:
            check(name, False, str(error))
            raise

    run_skip("skip: listed command", "cd /", True)
    run_skip("skip: listed command with argument and quotes", "cd '/'", True)
    run_skip("skip: error output still reaches the capture", "cd /cvb-no-such-dir", True, b"No such file")
    run_skip("skip: unlisted command", "printf 'UNLISTED\\n'", False, b"UNLISTED")
    run_skip("skip: empty unlisted command", ":", False)
    run_skip("skip: silent failure of unlisted command", "false", False)
    run_skip("skip: all commands of a list are listed", "cd /; cd /tmp", True)
    run_skip("skip: && chain of listed commands", "cd / && cd /tmp", True)
    run_skip("skip: listed then unlisted", "cd / && printf 'MIXED\\n'", False, b"MIXED")
    run_skip("skip: unlisted then listed", "printf 'MIXED2\\n'; cd /", False, b"MIXED2")
    run_skip("skip: comment and spacing in skip file", "pushd / >/dev/null", True)
    run_skip("skip: unlisted name is not skipped", "popd >/dev/null", False)
    run_skip("skip: compound command is not listed", "if true; then cd /; fi", False)
    run_skip("skip: command inside a function is not recorded", "cvb_test_function", False, b"FUNCTION_BLOCK_OK")

    # The list is re-read on every command, so edits apply immediately.
    with open(skip_file, "w", encoding="utf-8") as skip:
        skip.write("popd\n")
    run_skip("skip: removed entry is no longer skipped", "cd /", False)
    run_skip("skip: added entry is skipped", "popd 2>/dev/null; popd 2>/dev/null", True)

    os.unlink(skip_file)
    run_skip("skip: missing skip file skips nothing", "popd 2>/dev/null", False)

    # A skipped line must not leak into the next command's decision.
    with open(skip_file, "w", encoding="utf-8") as skip:
        skip.write("cd\n")
    run_skip("skip: skipped line does not leak into the next command", "cd /", True)
    run_skip("skip: next command starts clean", "printf 'CLEAN\\n'", False, b"CLEAN")

except Exception as error:
    print(f"FAIL: test runner stopped: {error}", file=sys.stderr, flush=True)

finally:
    if master is not None:
        try:
            os.write(master, b"\x04")
        except OSError:
            pass
        os.close(master)

    if child_pid is not None:
        try:
            os.waitpid(child_pid, 0)
        except ChildProcessError:
            pass

print(f"\n{passed} passed, {failed} failed", flush=True)
sys.exit(1 if failed else 0)
PY

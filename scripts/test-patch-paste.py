#!/usr/bin/env python3
"""End-to-end test of paste-to-apply: a patch pasted at an idle CVB prompt.

Drives the real binary in a PTY with bracketed-paste markers, as foot sends
them, a fake `wl-copy` and a fake (non-atomic) `apply_patch` on PATH.
"""

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
END_PREFIX = b"\x1b]777;CVB;END;"
PASTE_BEGIN = b"\x1b[200~"
PASTE_END = b"\x1b[201~"
DOT = "●".encode()  # the red dot CVB puts in front of the prompt

passed = 0
failed = 0


def check(name, condition, detail=""):
    global passed, failed
    if condition:
        passed += 1
        print(f"ok   {name}", flush=True)
    else:
        failed += 1
        print(f"FAIL {name}", flush=True)
        if detail:
            print(f"     {str(detail)[:500]!r}", flush=True)


FAKE_COPY = """#!{python}
import os
from pathlib import Path
import sys

directory = Path(os.environ["CVB_TEST_CLIPBOARD_DIR"])
number = len(sorted(directory.glob("capture-*.bin"))) + 1
(directory / f"capture-{number:03}.bin").write_bytes(sys.stdin.buffer.read())
"""

# A stand-in for the real apply_patch: reads the patch on stdin, writes each
# file as it goes (so, like the real one, it is not atomic) and prints absolute
# paths. Update hunks use their '-' lines to find the text and '+' lines to
# replace it.
FAKE_APPLY_PATCH = """#!{python}
import os
import sys

lines = sys.stdin.read().split("\\n")
cwd = os.getcwd()
changed = []
i = 0
while i < len(lines):
    line = lines[i]
    i += 1
    if line.startswith("*** Add File: "):
        path = line[len("*** Add File: "):]
        body = []
        while i < len(lines) and lines[i].startswith("+"):
            body.append(lines[i][1:])
            i += 1
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        open(path, "w").write("".join(b + "\\n" for b in body))
        changed.append("A " + os.path.join(cwd, path))
    elif line.startswith("*** Delete File: "):
        path = line[len("*** Delete File: "):]
        try:
            os.remove(path)
        except OSError:
            print("Failed to delete file " + os.path.join(cwd, path), file=sys.stderr)
            sys.exit(1)
        changed.append("D " + os.path.join(cwd, path))
    elif line.startswith("*** Update File: "):
        path = line[len("*** Update File: "):]
        dest = path
        if i < len(lines) and lines[i].startswith("*** Move to: "):
            dest = lines[i][len("*** Move to: "):]
            i += 1
        text = open(path).read().split("\\n")
        if text and text[-1] == "":
            text.pop()
        while i < len(lines) and not lines[i].startswith("*** "):
            old, new = [], []
            while i < len(lines) and not lines[i].startswith("*** "):
                if lines[i].startswith("@@") and (old or new):
                    break
                if lines[i].startswith("-"):
                    old.append(lines[i][1:])
                elif lines[i].startswith("+"):
                    new.append(lines[i][1:])
                i += 1
            at = next((s for s in range(len(text) - len(old) + 1) if text[s:s + len(old)] == old), -1)
            if at < 0:
                print("Failed to find expected lines in " + os.path.join(cwd, path) + ":", file=sys.stderr)
                print("\\n".join(old), file=sys.stderr)
                sys.exit(1)
            text[at:at + len(old)] = new
        os.makedirs(os.path.dirname(dest) or ".", exist_ok=True)
        open(dest, "w").write("".join(t + "\\n" for t in text))
        if dest != path:
            os.remove(path)
        changed.append("M " + os.path.join(cwd, dest))
print("Success. Updated the following files:")
for entry in changed:
    print(entry)
"""


class Session:
    def __init__(self, work, name, extra_env=None):
        self.work = work
        self.buffer = bytearray()
        self.clip = work / f"clip-{name}"
        self.clip.mkdir()
        self.project = work / f"project-{name}"
        self.project.mkdir()
        home = work / f"home-{name}"
        config = work / f"config-{name}"
        runtime = work / f"runtime-{name}"
        state = work / f"state-{name}"
        for d in (home, config, runtime, state):
            d.mkdir()
        (home / ".bashrc").write_text("")
        (config / "cvb").mkdir()
        (config / "cvb" / "limit").write_text("400000\n")
        for f in ("init", "history", "parent-history"):
            (work / f"{f}-{name}").write_text("")
        self.history = work / f"history-{name}"

        source = ROOT / "data" / "cvb.bash"
        env = os.environ.copy()
        env.update({
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config),
            "XDG_RUNTIME_DIR": str(runtime),
            "XDG_STATE_HOME": str(state),
            "CVB_INIT_FILE": str(work / f"init-{name}"),
            "CVB_HISTORY_FILE": str(self.history),
            "CVB_PARENT_HISTORY_FILE": str(work / f"parent-history-{name}"),
            "CVB_BASH_SOURCE": str(source),
            "CVB_BASH_SHA256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "CVB_TEST_CLIPBOARD_DIR": str(self.clip),
            "TERM": "xterm",
            "HISTFILE": str(home / ".bash_history"),
            "PATH": str(work / "bin") + os.pathsep + env.get("PATH", ""),
        })
        env.update(extra_env or {})

        pid, master = pty.fork()
        if pid == 0:
            os.chdir(self.project)
            binary = ROOT / "target" / "debug" / "cvb"
            os.execve(str(binary), [str(binary)], env)
        self.pid, self.master = pid, master
        fcntl.ioctl(master, 0x5414, struct.pack("HHHH", 24, 100, 0, 0))
        self.read_until_end()
        self.settle()

    # -- terminal plumbing -------------------------------------------------
    def pump(self, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if ready:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    return
                if not data:
                    return
                self.buffer.extend(data)

    def settle(self, quiet=0.3):
        while True:
            ready, _, _ = select.select([self.master], [], [], quiet)
            if not ready:
                return
            try:
                data = os.read(self.master, 65536)
            except OSError:
                return
            if not data:
                return
            self.buffer.extend(data)

    def read_until(self, marker, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            at = self.buffer.find(marker)
            if at >= 0:
                result = bytes(self.buffer[:at])
                del self.buffer[:at + len(marker)]
                return result
            if time.monotonic() > deadline:
                raise TimeoutError(f"timed out waiting for {marker!r}; got {bytes(self.buffer)[-300:]!r}")
            self.pump(0.1)

    def read_until_end(self, timeout=15):
        deadline = time.monotonic() + timeout
        marker_length = len(END_PREFIX) + 4
        while True:
            at = self.buffer.find(END_PREFIX)
            if at >= 0 and len(self.buffer) >= at + marker_length:
                status = self.buffer[at + len(END_PREFIX):at + len(END_PREFIX) + 3]
                terminator = self.buffer[at + len(END_PREFIX) + 3]
                if status.isdigit() and terminator == 0x07:
                    result = bytes(self.buffer[:at])
                    del self.buffer[:at + marker_length]
                    return result
            if time.monotonic() > deadline:
                raise TimeoutError(
                    f"timed out waiting for {END_PREFIX!r}; got {bytes(self.buffer)[-300:]!r}"
                )
            self.pump(0.1)

    def send(self, data):
        os.write(self.master, data)

    def paste(self, text):
        self.send(PASTE_BEGIN + text.encode() + PASTE_END)

    def captures(self):
        return [p.read_bytes() for p in sorted(self.clip.glob("capture-*.bin"))]

    def wait_capture(self, count, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            files = self.captures()
            if len(files) >= count:
                return files
            self.pump(0.1)
        raise TimeoutError(f"timed out waiting for capture {count}; terminal: {bytes(self.buffer)[-400:]!r}")

    # Event-driven waits: they return as soon as the thing they wait for has
    # happened and give up only after a generous deadline, so they never depend
    # on how long CVB takes.
    def wait_screen(self, marker, timeout=15):
        """Pumps until `marker` is in the screen buffer. Clear the buffer first
        when the marker could also be left over from an earlier step."""
        deadline = time.monotonic() + timeout
        while marker not in self.buffer:
            if time.monotonic() > deadline:
                raise TimeoutError(f"timed out waiting for {marker!r} on screen; got {bytes(self.buffer)[-400:]!r}")
            self.pump(0.05)
        return bytes(self.buffer)

    def wait_capture_where(self, test, since, timeout=15):
        """The first clipboard capture after the first `since` ones that `test` accepts."""
        deadline = time.monotonic() + timeout
        while True:
            for data in self.captures()[since:]:
                if test(data):
                    return data
            if time.monotonic() > deadline:
                raise TimeoutError(f"no matching capture after {since}; terminal: {bytes(self.buffer)[-400:]!r}")
            self.pump(0.05)

    def barrier(self, tag):
        """CVB handles input in order, so once a command typed now has been
        captured, everything typed before it has been handled."""
        out = self.run(f"printf 'BARRIER_%s\\n' {tag}").decode()
        return f"BARRIER_{tag}" in out

    def run(self, command):
        """Runs a command line and returns its clipboard capture."""
        before = len(self.captures())
        self.send(command.encode() + b"\n")
        return self.wait_capture(before + 1)[before]

    def paste_patch(self, body, wrap=True):
        before = len(self.captures())
        text = f"*** Begin Patch\n{body}*** End Patch" if wrap else body
        self.buffer.clear()
        self.paste(text)
        return self.wait_capture(before + 1)[before]

    def close(self):
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        os.close(self.master)

    # -- project helpers ----------------------------------------------------
    def write(self, rel, text):
        path = self.project / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def read(self, rel):
        return (self.project / rel).read_text()


# Two places where the hunk could belong, and a removed line the patch misquotes
# (`betaa`), so the picker offers both and the user has to choose. The fake
# apply_patch finds a hunk by its '-' lines alone, so the two places differ in
# that line (`beta1`, `beta2`): the corrected patch then lands only where chosen.
FLOW_FILE = "head\nalpha\nbeta1\ngamma\nmiddle\nalpha\nbeta2\ngamma\ntail\n"
FLOW_PATCH = "*** Begin Patch\n*** Update File: m.txt\n@@\n alpha\n-betaa\n+BETA\n gamma\n*** End Patch"
FLOW_SECOND = "head\nalpha\nbeta1\ngamma\nmiddle\nalpha\nBETA\ngamma\ntail\n"
BUNDLE_END = b"A higher textual score does not prove that a location is correct.\n"
CANCELLED = b"cancelled, nothing was applied"


FLOW_FIRST = "head\nalpha\nBETA\ngamma\nmiddle\nalpha\nbeta2\ngamma\ntail\n"
# The same kind of failure as a git diff: `thre` is misquoted and the header
# line number (9) is far off, so the picker offers the one real place.
DIFF_FILE = "one\ntwo\nthree\nfour\nfive\nsix\n"
DIFF_PATCH = ("diff --git a/y.txt b/y.txt\n--- a/y.txt\n+++ b/y.txt\n@@ -9,5 +9,5 @@\n"
              " one\n two\n-thre\n+3\n four\n five\n")
DIFF_FIXED = "one\ntwo\n3\nfour\nfive\nsix\n"
STALE = b"changed since the candidates were shown"


def open_flow_picker(s, path="m.txt", content=FLOW_FILE, patch=FLOW_PATCH):
    """Pastes the failing patch over a fresh file and returns the clipboard
    review bundle once the picker is ready for keys."""
    s.write(path, content)
    since = len(s.captures())
    s.buffer.clear()
    s.paste(patch)
    s.wait_capture_where(lambda d: d.startswith(b"CVB patch FAILED"), since)
    bundle = s.wait_capture_where(
        lambda d: d.startswith(b"CVB FAILED-HUNK REVIEW") and d.endswith(BUNDLE_END), since)
    s.wait_screen(b"all candidate comparisons copied")
    return bundle.decode()


def picker_flow(s):
    """The picker as a person uses it: browse, choose, confirm, cancel. Every
    step waits for what CVB prints or copies; nothing assumes how long it takes."""
    bundle = open_flow_picker(s)
    check("flow: the review bundle is copied and lists both places",
          "[1] lines 2-4" in bundle and "[2] lines 6-8" in bundle and "HUNK 1 OF 1" in bundle, bundle)

    # Browse with the arrow keys, choose with Enter, apply with Ctrl+B twice.
    s.buffer.clear()
    s.send(b"\x1b[C")
    screen = s.wait_screen(b"Candidate 2 of 2")
    check("flow: Right shows the second place", b"[2] lines 6-8" in screen, screen[-300:])
    s.buffer.clear()
    s.send(b"\r")
    screen = s.wait_screen(b"Not applied yet")
    check("flow: Enter shows the corrected patch and the preview, and changes nothing",
          b"would apply to 1 file(s)" in screen and s.read("m.txt") == FLOW_FILE, screen[-400:])
    since = len(s.captures())
    s.buffer.clear()
    s.send(b"\x02")
    s.wait_screen(b"press Ctrl+B once more")
    check("flow: the first Ctrl+B only arms", s.read("m.txt") == FLOW_FILE)
    s.send(b"\x02")
    report = s.wait_capture_where(lambda d: d.startswith(b"CVB patch APPLIED"), since).decode()
    check("flow: the second Ctrl+B applies the patch at the place that was browsed to",
          s.read("m.txt") == FLOW_SECOND, (report, s.read("m.txt")))
    check("flow: the shell works after applying", s.barrier("applied"))

    # Cancel while the corrected patch waits: the key goes on to the shell.
    open_flow_picker(s)
    s.buffer.clear()
    s.send(b"2")
    s.wait_screen(b"Not applied yet")
    since = len(s.captures())
    s.buffer.clear()
    s.send(b"echo after-confirm\n")
    screen = s.wait_screen(CANCELLED)
    typed = s.wait_capture_where(lambda d: b"after-confirm" in d, since)
    check("flow: a key at the confirmation cancels, reaches the shell, and nothing is applied",
          s.read("m.txt") == FLOW_FILE
          and not any(d.startswith(b"CVB patch APPLIED") for d in s.captures()[since:]),
          (screen[-300:], typed))

    # A key between the two Ctrl+B presses cancels too.
    open_flow_picker(s)
    s.buffer.clear()
    s.send(b"1")
    s.wait_screen(b"Not applied yet")
    s.buffer.clear()
    s.send(b"\x02")
    s.wait_screen(b"press Ctrl+B once more")
    since = len(s.captures())
    s.send(b"echo between-presses\n")
    s.wait_screen(CANCELLED)
    s.wait_capture_where(lambda d: b"between-presses" in d, since)
    check("flow: a key between the two presses cancels and nothing is applied",
          s.read("m.txt") == FLOW_FILE
          and not any(d.startswith(b"CVB patch APPLIED") for d in s.captures()[since:]))

    # q and a lone Escape cancel while browsing and hand nothing on.
    for name, key in (("q", b"q"), ("a lone Escape", b"\x1b")):
        open_flow_picker(s)
        s.buffer.clear()
        s.send(key)
        s.wait_screen(CANCELLED)
        check(f"flow: {name} cancels the picker and the shell works again",
              s.barrier("after-cancel") and s.read("m.txt") == FLOW_FILE)

    stale_flow(s)


def stale_flow(s):
    """A file that changed while the picker was open is not patched. The
    contents are compared, so the change below is one the corrected patch would
    still apply over; every key waits for CVB's answer to the one before."""
    def applied_since(since):
        return any(d.startswith(b"CVB patch APPLIED") for d in s.captures()[since:])

    def choose_and_confirm():
        s.buffer.clear()
        s.send(b"1")
        s.wait_screen(b"Not applied yet")

    def press_twice(since):
        s.buffer.clear()
        s.send(b"\x02")
        s.wait_screen(b"press Ctrl+B once more")
        s.send(b"\x02")
        return s.wait_capture_where(lambda d: STALE in d or d.startswith(b"CVB patch APPLIED"), since)

    # Modified after the corrected patch was previewed: the apply refuses.
    for name, path, content, patch in (
        ("apply_patch", "m.txt", FLOW_FILE, FLOW_PATCH),
        ("git diff", "y.txt", DIFF_FILE, DIFF_PATCH),
    ):
        open_flow_picker(s, path, content, patch)
        choose_and_confirm()
        modified = content + "added meanwhile\n"
        s.write(path, modified)
        since = len(s.captures())
        report = press_twice(since).decode()
        check(f"flow: a {name} file modified before the apply is rejected, not patched",
              report.startswith(f"CVB patch REJECTED: {path} changed since the candidates were shown")
              and s.read(path) == modified and not applied_since(since), report)
        check(f"flow: the shell works after a stale apply ({name})", s.barrier("stale-apply"))

    # Modified before the choice is previewed: the picker is dropped with the report.
    open_flow_picker(s)
    modified = FLOW_FILE + "added meanwhile\n"
    s.write("m.txt", modified)
    since = len(s.captures())
    s.buffer.clear()
    s.send(b"1")
    report = s.wait_capture_where(lambda d: STALE in d, since).decode()
    check("flow: a file modified before the preview is rejected and the picker is dropped",
          "CVB patch REJECTED: m.txt changed since the candidates were shown" in report
          and "nothing was changed" in report and s.read("m.txt") == modified
          and not applied_since(since), report)
    check("flow: keys reach the shell after a stale preview", s.barrier("stale-preview"))

    # Same bytes, newer modification time: not a change.
    open_flow_picker(s)
    s.write("m.txt", FLOW_FILE)
    later = time.time() + 3600
    os.utime(s.project / "m.txt", (later, later))
    choose_and_confirm()
    since = len(s.captures())
    report = press_twice(since).decode()
    check("flow: a file only touched since the picker opened is still patched",
          report.startswith("CVB patch APPLIED files=1") and s.read("m.txt") == FLOW_FIRST, report)
    check("flow: the shell works after the touched-file apply", s.barrier("touched"))


def main():
    build = subprocess.run(["cargo", "build", "--bin", "cvb"], cwd=ROOT, capture_output=True, text=True)
    if build.returncode:
        print(build.stdout, build.stderr, file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="cvb-patch-test-") as temporary:
        work = Path(temporary)
        (work / "bin").mkdir()
        for name, template in (("wl-copy", FAKE_COPY), ("apply_patch", FAKE_APPLY_PATCH)):
            path = work / "bin" / name
            path.write_text(template.replace("{python}", sys.executable))
            path.chmod(0o755)

        # ---------------------------------------------------------------- main
        try:
            s = Session(work, "main")
        except OSError as error:
            print(f"SKIP: interactive PTY unavailable: {error}")
            return 0
        try:
            check("session starts", True)
            s.write("src/a.rs", "one\ntwo\nthree\n")
            s.write("old.txt", "bye\n")
            s.write("b.txt", "p\nq\n")

            launch_captures = len(s.captures())
            capture = s.paste_patch(
                "*** Update File: src/a.rs\n@@\n-two\n+TWO\n"
                "*** Add File: new/dir/c.txt\n+hello\n"
                "*** Delete File: old.txt\n"
            ).decode()
            check("a pasted patch is applied (update, add in a new directory, delete)",
                  s.read("src/a.rs") == "one\nTWO\nthree\n"
                  and s.read("new/dir/c.txt") == "hello\n"
                  and not (s.project / "old.txt").exists())
            check("the clipboard holds the APPLIED report with real paths",
                  capture.startswith("CVB patch APPLIED files=3\n")
                  and f"M {s.project.resolve()}/src/a.rs" in capture
                  and "patch-scratch" not in capture,
                  capture)
            s.settle()
            screen = bytes(s.buffer)
            check("the result is shown on the terminal", b"CVB patch APPLIED files=3" in screen, screen[-300:])
            tail = screen.split(b"CVB patch APPLIED files=3", 1)[-1]
            check("the prompt is drawn again after the result", DOT in tail, tail[-200:])

            s.buffer.clear()
            after = s.run("printf 'AFTER_OK\\n'").decode()
            check("the shell works normally afterwards", "AFTER_OK" in after, after)
            check("the patch is not in the next command's capture",
                  "Begin Patch" not in after and "CVB patch" not in after, after)
            hist = s.run("history").decode()
            check("a pasted patch leaves no history entry", "Begin Patch" not in hist and "Update File" not in hist, hist)

            # -------------------------------------------------- failure: atomic
            before_a, before_b = s.read("src/a.rs"), s.read("b.txt")
            capture = s.paste_patch(
                "*** Update File: src/a.rs\n@@\n-one\n+ONE\n"
                "*** Update File: b.txt\n@@\n-does not exist\n+x\n"
            ).decode()
            check("a failing patch reports FAILED and that nothing was modified",
                  capture.startswith("CVB patch FAILED (apply_patch exit 1)\n")
                  and capture.endswith("No files were modified.\n")
                  and f"Failed to find expected lines in {s.project.resolve()}/b.txt:" in capture
                  and "patch-scratch" not in capture,
                  capture)
            check("a failing patch leaves every file untouched (the first file is not half-applied)",
                  s.read("src/a.rs") == before_a and s.read("b.txt") == before_b)
            s.settle()
            check("the prompt is back after a failure", DOT in bytes(s.buffer).split(b"CVB patch FAILED", 1)[-1])

            # ------------------------------------- the same patch pasted twice
            before_a = s.read("src/a.rs")
            capture = s.paste_patch(
                "*** Update File: src/a.rs\n@@\n one\n-two\n+TWO\n three\n"
            ).decode()
            check("a patch pasted twice fails and says its changes are already present",
                  capture.startswith("CVB patch FAILED (apply_patch exit 1)\n")
                  and "Note: every hunk of this patch appears to be already present" in capture
                  and f"{s.project.resolve()}/src/a.rs (1 of 1 hunks)" in capture
                  and capture.endswith("No files were modified.\n"),
                  capture)
            check("the second paste changed nothing", s.read("src/a.rs") == before_a)
            s.settle()

            # ------------------------------------------------------- refusals
            capture = s.paste_patch("*** Add File: b.txt\n+overwrite\n").decode()
            check("Add File over an existing file is rejected",
                  "CVB patch REJECTED: Add File target already exists: b.txt" in capture and s.read("b.txt") == before_b,
                  capture)
            capture = s.paste_patch("*** Delete File: ../outside\n").decode()
            check("an unsafe path is rejected", "REJECTED" in capture and "unsafe path" in capture, capture)
            capture = s.paste_patch("*** Begin Patch\n*** Update File: b.txt\n@@\n-p", wrap=False).decode()
            check("an incomplete (truncated) paste is rejected, not run",
                  "CVB patch REJECTED: incomplete paste" in capture and "4 MB" in capture and s.read("b.txt") == before_b,
                  capture)
            s.settle()
            check("the truncated paste did not reach the shell", b"command not found" not in bytes(s.buffer))

            # --------------------------------------------------------- move
            s.write("bin/tool.sh", "#!/bin/sh\necho old\n")
            (s.project / "bin/tool.sh").chmod(0o775)
            capture = s.paste_patch(
                "*** Update File: bin/tool.sh\n*** Move to: scripts/tool.sh\n@@\n-echo old\n+echo new\n"
            ).decode()
            moved = s.project / "scripts/tool.sh"
            check("a move applies and keeps the executable mode",
                  capture.startswith("CVB patch APPLIED files=1")
                  and moved.read_text() == "#!/bin/sh\necho new\n"
                  and (moved.stat().st_mode & 0o777) == 0o775
                  and not (s.project / "bin/tool.sh").exists(),
                  capture)

            # ------------------------------------------- markdown fence
            s.write("fenced.txt", "one\nold\n")
            capture = s.paste_patch(
                "```diff\n*** Begin Patch\n*** Update File: fenced.txt\n@@\n-old\n+new\n*** End Patch\n```\n",
                wrap=False,
            ).decode()
            s.settle()
            check("a patch in a markdown fence applies and the shell never sees the fence",
                  capture.startswith("CVB patch APPLIED files=1")
                  and s.read("fenced.txt") == "one\nnew\n"
                  and b"command not found" not in bytes(s.buffer),
                  capture)

            # ------------------------------------- wrong-directory hint
            os.makedirs(s.project / "sub", exist_ok=True)
            s.send(b"cd sub\n")
            s.settle()
            capture = s.paste_patch(
                "*** Update File: fenced.txt\n@@\n-new\n+newer\n"
            ).decode()
            check("a patch written for the parent directory is refused with a hint",
                  capture.startswith("CVB patch REJECTED:")
                  and "Update File target does not exist: fenced.txt" in capture
                  and "Note: fenced.txt exists in" in capture
                  and "a parent of this directory" in capture
                  and s.read("fenced.txt") == "one\nnew\n",
                  capture)
            s.send(b"cd ..\n")
            s.settle()

            # ----------------------------------- half-typed line is preserved
            s.settle()
            s.send(b"echo HALF_TYPED_LINE")
            s.settle()
            before = len(s.captures())
            s.buffer.clear()
            s.paste("*** Begin Patch\n*** Add File: half.txt\n+x\n*** End Patch")
            s.wait_capture(before + 1)
            s.settle()
            s.send(b"\n")
            payload = s.wait_capture(before + 2)[before + 1].decode()
            check("a half-typed command line survives the patch",
                  (s.project / "half.txt").exists() and "HALF_TYPED_LINE" in payload, payload)

            # ------------------------------------------- ordinary pastes pass
            s.settle()
            before = len(s.captures())
            s.paste("echo PLAIN_PASTE_OK")
            s.settle()
            s.send(b"\n")
            payload = s.wait_capture(before + 1)[before].decode()
            check("an ordinary paste reaches the shell untouched", "PLAIN_PASTE_OK" in payload, payload)

            before = len(s.captures())
            s.paste("echo 'text that mentions *** Begin Patch only'")
            s.settle()
            s.send(b"\n")
            payload = s.wait_capture(before + 1)[before].decode()
            check("a paste that merely mentions the marker passes through",
                  "mentions *** Begin Patch only" in payload and "REJECTED" not in payload, payload)

            # ------------------------------------------------- git diff pastes
            s.write("d.txt", "alpha\nbeta\ngamma\n")
            s.write("gone.txt", "bye\n")
            capture = s.paste_patch(
                "diff --git a/d.txt b/d.txt\nindex 111..222 100644\n--- a/d.txt\n+++ b/d.txt\n"
                "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n"
                "diff --git a/dir/run.sh b/dir/run.sh\nnew file mode 100755\nindex 000..111\n"
                "--- /dev/null\n+++ b/dir/run.sh\n@@ -0,0 +1,2 @@\n+#!/bin/sh\n+echo hi\n"
                "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\nindex 111..000\n"
                "--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
                wrap=False,
            ).decode()
            check("a pasted git diff is applied (update, new executable file, delete)",
                  s.read("d.txt") == "alpha\nBETA\ngamma\n"
                  and s.read("dir/run.sh") == "#!/bin/sh\necho hi\n"
                  and (s.project / "dir/run.sh").stat().st_mode & 0o777 == 0o755
                  and not (s.project / "gone.txt").exists(),
                  capture)
            check("the clipboard holds the APPLIED report for a diff, with real paths",
                  capture.startswith("CVB patch APPLIED files=3\n")
                  and f"M {s.project.resolve()}/d.txt" in capture
                  and f"A {s.project.resolve()}/dir/run.sh" in capture
                  and "patch-scratch" not in capture,
                  capture)
            s.settle()
            check("the prompt is drawn again after a diff result",
                  DOT in bytes(s.buffer).split(b"CVB patch APPLIED", 1)[-1])

            before_d = s.read("d.txt")
            s.write("e.txt", "one\n")
            capture = s.paste_patch(
                "diff --git a/d.txt b/d.txt\n--- a/d.txt\n+++ b/d.txt\n@@ -1 +1 @@\n-alpha\n+ALPHA\n"
                "diff --git a/e.txt b/e.txt\n--- a/e.txt\n+++ b/e.txt\n@@ -1 +1 @@\n-missing\n+x\n",
                wrap=False,
            ).decode()
            check("a failing git diff reports git apply's failure and that nothing was modified",
                  capture.startswith("CVB patch FAILED (git apply exit")
                  and capture.endswith("No files were modified.\n")
                  and "e.txt" in capture and "patch-scratch" not in capture,
                  capture)
            check("a failing git diff leaves every file untouched",
                  s.read("d.txt") == before_d and s.read("e.txt") == "one\n")

            capture = s.paste_patch(
                "diff --git a/d.txt b/d.txt\n--- a/d.txt\n+++ b/d.txt\n@@ -1,3 +1,3 @@\n",
                wrap=False,
            ).decode()
            check("a git diff cut right after a hunk header is rejected as incomplete, not run",
                  "CVB patch REJECTED: incomplete paste" in capture and s.read("d.txt") == before_d,
                  capture)
            s.settle()
            check("the cut diff did not reach the shell", b"command not found" not in bytes(s.buffer))

            # A diff whose header counts are wrong (LLMs miscount) is corrected, applied and says so.
            s.write("w.txt", "one\ntwo\nthree\nfour\n")
            capture = s.paste_patch(
                "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1,9 +1,2 @@\n one\n-two\n+TWO\n three\n",
                wrap=False,
            ).decode()
            check("a diff with wrong hunk header counts is applied and the report says it corrected them",
                  s.read("w.txt") == "one\nTWO\nthree\nfour\n"
                  and capture.startswith("CVB patch APPLIED files=1\n")
                  and "Note: corrected the line counts of 1 hunk header" in capture,
                  capture)

            s.settle()
            before = len(s.captures())
            s.paste("--- not a diff, just text")
            s.settle()
            s.send(b"\n")
            payload = s.wait_capture(before + 1)[before].decode()
            check("a paste that only starts with dashes passes through to the shell",
                  "---: command not found" in payload and "CVB patch" not in payload, payload)

            # ------------------------------ not at a prompt: passes through
            s.write("untouched.txt", "keep\n")
            s.send(b"cat > catted.txt\n")
            s.settle()
            before = len(s.captures())
            s.paste("*** Begin Patch\n*** Delete File: untouched.txt\n*** End Patch")
            s.settle()
            s.send(b"\n\x04")
            s.wait_capture(before + 1)
            s.settle()
            catted = s.read("catted.txt") if (s.project / "catted.txt").exists() else ""
            check("a patch pasted while a command runs is not applied",
                  s.read("untouched.txt") == "keep\n" and "*** Begin Patch" in catted,
                  catted)

            # --------------------------------- Escape key is not swallowed
            # CVB holds a lone Escape for a few milliseconds in case it starts a
            # paste marker. The terminal echoes it as ^[ once it is released.
            s.settle()
            s.send(b"cat -v\n")
            s.settle()
            s.buffer.clear()
            s.send(b"\x1b")
            s.pump(0.4)
            echoed = b"^[" in bytes(s.buffer)
            before = len(s.captures())
            s.send(b"\x04\x04")
            payload = s.wait_capture(before + 1)[before].decode()
            check("a lone Escape key is passed on after a moment", echoed and "^[" in payload, (echoed, payload))

            # ------------------------------------- a stalled paste is released
            s.write("stall.txt", "s\n")
            s.settle()
            s.buffer.clear()
            s.send(PASTE_BEGIN + b"*** Begin Patch\n*** Delete File: stall.txt\n")
            s.pump(2.8)
            # Readline shows pasted text only once the paste ends. If CVB were
            # still holding the paste, the end marker would now complete it and
            # it would be rejected as incomplete; if it was released, the text
            # reaches the shell and no CVB message appears.
            s.send(PASTE_END)
            s.settle()
            screen = bytes(s.buffer)
            s.send(b"\x15")
            s.settle()
            check("a patch paste that stalls is handed to the shell, never applied",
                  b"Delete File" in screen and b"CVB patch" not in screen
                  and (s.project / "stall.txt").exists(), screen[-300:])
        finally:
            s.close()

        # ------------------------------------------------------------ review gate
        gate = Session(work, "gate", {"CVB_PATCH_REVIEW": "on"})
        try:
            gate.write("g.txt", "one\ntwo\n")
            gate.write("h.txt", "aa\nbb\n")
            gate.settle()
            patch_g = "*** Begin Patch\n*** Update File: g.txt\n@@\n-two\n+TWO\n*** End Patch"
            patch_h = "*** Begin Patch\n*** Update File: h.txt\n@@\n-bb\n+BB\n*** End Patch"
            clips = len(gate.captures())

            gate.buffer.clear()
            gate.paste(patch_g)
            gate.settle()
            screen = bytes(gate.buffer)
            check("review gate: a pasted patch is shown, not applied",
                  b"CVB patch REVIEW (apply_patch, 6 lines, 1 file): update g.txt" in screen
                  and b"Not applied yet. Press Ctrl+B twice to apply" in screen
                  and gate.read("g.txt") == "one\ntwo\n"
                  and len(gate.captures()) == clips, screen[-400:])
            check("review gate: the patch text itself is on the screen", b"+TWO" in screen)
            check("review gate: the prompt is drawn again below the review", DOT in screen.rsplit(b"Not applied yet", 1)[-1])

            gate.buffer.clear()
            gate.send(b"\x02")
            gate.settle()
            check("review gate: the first Ctrl+B only arms",
                  b"press Ctrl+B once more" in bytes(gate.buffer)
                  and gate.read("g.txt") == "one\ntwo\n", bytes(gate.buffer)[-300:])

            before = len(gate.captures())
            gate.send(b"\x02")
            capture = gate.wait_capture(before + 1)[before].decode()
            check("review gate: the second Ctrl+B applies the patch and copies the report",
                  gate.read("g.txt") == "one\nTWO\n" and capture.startswith("CVB patch APPLIED files=1\n"),
                  (gate.read("g.txt"), capture))

            # Any other key cancels, and is the shell's.
            gate.write("g.txt", "one\ntwo\n")
            gate.settle()
            gate.paste(patch_g)
            gate.settle()
            gate.buffer.clear()
            before = len(gate.captures())
            gate.send(b"echo typed-after-review\n")
            payload = gate.wait_capture(before + 1)[before].decode()
            screen = bytes(gate.buffer)
            check("review gate: another key cancels and still reaches the shell",
                  b"cancelled, nothing was applied" in screen
                  and "typed-after-review" in payload
                  and gate.read("g.txt") == "one\ntwo\n", (payload, screen[-300:]))

            # A key between the two presses cancels too.
            gate.paste(patch_g)
            gate.settle()
            gate.send(b"\x02")
            gate.settle()
            before = len(gate.captures())
            gate.send(b"echo between\n")
            payload = gate.wait_capture(before + 1)[before].decode()
            gate.send(b"\x02")  # a stray Ctrl+B at the prompt is just readline's
            gate.settle()
            check("review gate: a key between the presses cancels",
                  "between" in payload and gate.read("g.txt") == "one\ntwo\n", payload)

            # A second paste replaces the first.
            gate.paste(patch_g)
            gate.settle()
            gate.paste(patch_h)
            gate.settle()
            before = len(gate.captures())
            gate.send(b"\x02\x02")
            capture = gate.wait_capture(before + 1)[before].decode()
            check("review gate: a second paste replaces the first review",
                  gate.read("h.txt") == "aa\nBB\n" and gate.read("g.txt") == "one\ntwo\n"
                  and "h.txt" in capture, (capture, gate.read("h.txt")))

            # A patch that cannot be parsed is rejected at once, as without the gate.
            before = len(gate.captures())
            capture = gate.paste_patch("*** Delete File: ../outside\n").decode()
            check("review gate: an unusable patch is rejected without a review",
                  capture.startswith("CVB patch REJECTED: line 2: unsafe path"), capture)

            # Ordinary pastes are untouched by the gate.
            gate.settle()
            gate.buffer.clear()
            gate.paste("echo plain paste")
            gate.settle()
            screen = bytes(gate.buffer)
            gate.send(b"\x15")
            gate.settle()
            check("review gate: an ordinary paste goes straight to the shell",
                  b"echo plain paste" in screen and b"REVIEW" not in screen, screen[-200:])
        finally:
            gate.close()

        # ------------------------------------------------------------ picker
        pick = Session(work, "picker")
        try:
            body = "*** Update File: x.txt\n@@\n alpha\n-betaa\n+BETA\n gamma\n"
            original = "alpha\nbeta\ngamma\ndelta\n"
            pick.write("x.txt", original)
            capture = pick.paste_patch(body).decode()
            check("picker: the failure report is copied as usual",
                  capture.startswith("CVB patch FAILED") and capture.endswith("No files were modified.\n"), capture)
            pick.settle()
            screen = bytes(pick.buffer)
            check("picker: the screen offers the nearest place",
                  b"did not match the files" in screen and b"[1] lines 1-3" in screen
                  and b"a line to remove differs" in screen and b"s skips" in screen, screen[-500:])
            below = screen.rsplit(b"Esc cancels.", 1)
            check("picker: the prompt is drawn again below it", len(below) == 2 and DOT in below[1], screen[-300:])

            pick.buffer.clear()
            pick.send(b"1")
            pick.settle()
            screen = bytes(pick.buffer)
            check("picker: choosing shows the corrected patch, a warning and the preview",
                  b"corrected patch" in screen and b"-beta" in screen and b"WARNING" in screen
                  and b"would apply to 1 file(s)" in screen and b"Not applied yet" in screen, screen[-600:])
            check("picker: nothing is changed before the confirmation", pick.read("x.txt") == original)

            before = len(pick.captures())
            pick.send(b"\x02")
            pick.settle()
            check("picker: the first Ctrl+B only arms", pick.read("x.txt") == original)
            pick.send(b"\x02")
            capture = pick.wait_capture(before + 1)[before].decode()
            check("picker: the second Ctrl+B applies the corrected patch",
                  capture.startswith("CVB patch APPLIED files=1") and pick.read("x.txt") == "alpha\nBETA\ngamma\ndelta\n",
                  capture)
            pick.settle()
            pick.buffer.clear()
            after = pick.run("printf 'AFTER_PICK\\n'").decode()
            check("picker: the shell works normally afterwards", "AFTER_PICK" in after, after)

            # Skip cancels the whole patch.
            pick.write("x.txt", original)
            pick.paste_patch(body)
            pick.settle()
            pick.buffer.clear()
            pick.send(b"s")
            pick.settle()
            check("picker: s cancels and nothing changes",
                  b"cancelled, nothing was applied" in bytes(pick.buffer) and pick.read("x.txt") == original,
                  bytes(pick.buffer)[-300:])
            # After cancelling, typing reaches the shell and the picker is gone.
            after = pick.run("printf 'S_%s\\n' ok").decode()
            check("picker: keys go to the shell after a cancel", "S_ok" in after, after)

            # Another key cancels and is handed on.
            pick.paste_patch(body)
            pick.settle()
            pick.buffer.clear()
            pick.send(b"echo typed-after-picker\n")
            pick.settle()
            screen = bytes(pick.buffer)
            check("picker: another key cancels and still reaches the shell",
                  b"cancelled, nothing was applied" in screen and b"typed-after-picker" in screen
                  and pick.read("x.txt") == original, screen[-300:])

            # A patch no place resembles gets no picker, only the report.
            pick.paste_patch("*** Update File: x.txt\n@@\n qqqqqq\n-wwwwww\n+eeeeee\n")
            pick.settle()
            check("picker: no suggestions, no picker", b"did not match the files" not in bytes(pick.buffer))
            # A git diff gets the same treatment, with renumbered headers.
            pick.write("y.txt", "one\ntwo\nthree\nfour\nfive\nsix\n")
            diff = ("diff --git a/y.txt b/y.txt\n--- a/y.txt\n+++ b/y.txt\n@@ -9,5 +9,5 @@\n one\n two\n-thre\n+3\n four\n five\n")
            capture = pick.paste_patch(diff, wrap=False).decode()
            pick.settle()
            screen = bytes(pick.buffer)
            check("picker: a failed git diff opens the picker too",
                  capture.startswith("CVB patch FAILED (git apply") and b"(the patch says line 9)" in screen, screen[-400:])
            before = len(pick.captures())
            pick.send(b"1\x02\x02")
            capture = pick.wait_capture(before + 1)[before].decode()
            check("picker: the corrected git diff applies",
                  capture.startswith("CVB patch APPLIED files=1")
                  and pick.read("y.txt") == "one\ntwo\n3\nfour\nfive\nsix\n", capture)
            pick.settle()
        finally:
            pick.close()

        # ------------------------------------------------------------ picker flow
        flow = Session(work, "flow")
        try:
            picker_flow(flow)
        finally:
            flow.close()

        # ------------------------------------------------------------ switched off
        off = Session(work, "off", {"CVB_PATCH_PASTE": "off"})
        try:
            off.write("a.txt", "x\n")
            off.settle()
            off.paste("*** Begin Patch\n*** Delete File: a.txt\n*** End Patch")
            off.settle()
            off.send(b"\x15")  # Ctrl-U: discard the pasted text without running it
            off.settle()
            check("CVB_PATCH_PASTE=off leaves a pasted patch to the shell",
                  (off.project / "a.txt").exists() and b"CVB patch" not in bytes(off.buffer))
        finally:
            off.close()

    print(f"\n{passed} passed, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

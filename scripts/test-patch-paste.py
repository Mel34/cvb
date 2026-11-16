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
END = b"\x1b]777;CVB;END\x07"
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
        self.read_until(END)
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

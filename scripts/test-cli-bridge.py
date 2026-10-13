#!/usr/bin/env python3
"""Integration tests for CVB's real CLI and clipboard bridge."""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PASSED = 0
FAILED = 0


def check(name, condition, detail=""):
    global PASSED, FAILED
    if condition:
        PASSED += 1
        print(f"ok   {name}", flush=True)
    else:
        FAILED += 1
        print(f"FAIL {name}", flush=True)
        if detail:
            print(f"     {detail}", flush=True)


def main():
    global FAILED

    build = subprocess.run(
        ["cargo", "build", "--bin", "cvb"],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    if build.returncode:
        print("FAIL: cargo build --bin cvb", file=sys.stderr)
        print(build.stdout, file=sys.stderr)
        print(build.stderr, file=sys.stderr)
        return 1

    binary = ROOT / "target" / "debug" / "cvb"

    with tempfile.TemporaryDirectory(prefix="cvb-cli-test-") as temporary:
        work = Path(temporary)
        home = work / "home"
        config = work / "config"
        fake_bin = work / "bin"
        home.mkdir()
        config.mkdir()
        fake_bin.mkdir()

        # Record every clipboard payload instead of touching the real clipboard.
        fake_wl_copy = fake_bin / "wl-copy"
        fake_wl_copy.write_text(
            f"""#!{sys.executable}
import os
import sys
from pathlib import Path

directory = Path(os.environ["CVB_TEST_CLIPBOARD_DIR"])
directory.mkdir(parents=True, exist_ok=True)
counter = directory / "counter"
number = int(counter.read_text()) + 1 if counter.exists() else 1
counter.write_text(str(number))
(directory / f"capture-{{number:03}}.bin").write_bytes(sys.stdin.buffer.read())

if os.environ.get("CVB_TEST_WL_COPY_FAIL") == "1":
    sys.exit(7)
"""
        )
        fake_wl_copy.chmod(0o755)

        base_env = os.environ.copy()
        base_env.update({
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config),
            "PATH": str(fake_bin) + os.pathsep + base_env.get("PATH", ""),
        })

        def invoke(label, args, *, input_bytes=None, fail_copy=False,
                   limit=None, cwd=None):
            clipboard_dir = work / "clipboard" / label.replace("/", "_")
            clipboard_dir.mkdir(parents=True, exist_ok=True)

            env = base_env.copy()
            env["CVB_TEST_CLIPBOARD_DIR"] = str(clipboard_dir)
            env.pop("CVB_TEST_WL_COPY_FAIL", None)
            if fail_copy:
                env["CVB_TEST_WL_COPY_FAIL"] = "1"

            if limit is not None:
                limit_file = config / "cvb" / "limit"
                limit_file.parent.mkdir(parents=True, exist_ok=True)
                limit_file.write_text(f"{limit}\n")

            result = subprocess.run(
                [str(binary), *map(str, args)],
                cwd=cwd or work,
                env=env,
                input=input_bytes,
                capture_output=True,
                timeout=15,
            )
            captures = sorted(clipboard_dir.glob("capture-*.bin"))
            payloads = [path.read_bytes() for path in captures]
            return result, payloads

        result, _ = invoke("help", ["--help"])
        check(
            "help option",
            result.returncode == 0 and b"Usage:" in result.stdout,
            f"status={result.returncode}, stdout={result.stdout!r}",
        )

        result, _ = invoke("version", ["--version"])
        check(
            "version option",
            result.returncode == 0 and result.stdout.startswith(b"cvb "),
            f"status={result.returncode}, stdout={result.stdout!r}",
        )

        # Relative-path file capture must preserve the original bytes.
        fixture = work / "single.txt"
        fixture.write_bytes(b"alpha\nbeta\n")
        result, payloads = invoke("single-file", [fixture.name])
        check(
            "single-file capture",
            result.returncode == 0 and payloads == [b"alpha\nbeta\n"],
            f"status={result.returncode}, payloads={payloads!r}, "
            f"stderr={result.stderr!r}",
        )

        # Small invalid UTF-8 is allowed through unchanged.
        binary_fixture = work / "binary.dat"
        binary_fixture.write_bytes(b"\x00\xffA\x80\n")
        result, payloads = invoke("binary-file", [binary_fixture.name])
        check(
            "binary file byte preservation",
            result.returncode == 0 and payloads == [b"\x00\xffA\x80\n"],
            f"status={result.returncode}, payloads={payloads!r}",
        )

        # Directory inventory should contain its header and nested file.
        directory = work / "inventory"
        (directory / "src").mkdir(parents=True)
        (directory / "src" / "main.rs").write_text("fn main() {}\n")
        result, payloads = invoke("directory", [directory])
        inventory_ok = (
            result.returncode == 0
            and len(payloads) == 1
            and b"path\tname\ttype\tsize\t" in payloads[0]
            and b"src/main.rs\tmain.rs\tfile\t" in payloads[0]
        )
        check(
            "directory inventory capture",
            inventory_ok,
            f"status={result.returncode}, payloads={payloads!r}, "
            f"stderr={result.stderr!r}",
        )

        # Multiple paths must retain labels and content in order.
        first = work / "first.txt"
        second = work / "second.txt"
        first.write_text("FIRST_CONTENT\n")
        second.write_text("SECOND_CONTENT\n")
        result, payloads = invoke(
            "multiple-paths", [first.name, second.name]
        )
        multi_ok = (
            result.returncode == 0
            and len(payloads) == 1
            and payloads[0].find(b"--- first.txt ---")
                < payloads[0].find(b"--- second.txt ---")
            and b"FIRST_CONTENT" in payloads[0]
            and b"SECOND_CONTENT" in payloads[0]
        )
        check(
            "multiple-path capture and ordering",
            multi_ok,
            f"status={result.returncode}, payloads={payloads!r}",
        )

        # A clipboard backend failure must be visible and nonzero.
        result, _ = invoke(
            "clipboard-failure", [fixture.name], fail_copy=True
        )
        check(
            "clipboard failure propagation",
            result.returncode == 1
            and b"wl-copy exited with status" in result.stderr,
            f"status={result.returncode}, stderr={result.stderr!r}",
        )

        # Non-path arguments must execute as a command and preserve its status.
        result, payloads = invoke(
            "command-passthrough",
            [
                "sh", "-c",
                "printf 'STDOUT_OK\\n'; "
                "printf 'STDERR_OK\\n' >&2; exit 23",
            ],
        )
        check(
            "command stdout/stderr and exit-status passthrough",
            result.returncode == 23
            and b"STDOUT_OK\n" in result.stdout
            and b"STDERR_OK\n" in result.stderr
            and not payloads,
            f"status={result.returncode}, stdout={result.stdout!r}, "
            f"stderr={result.stderr!r}, clipboard_calls={len(payloads)}",
        )

        # Exercise real chunking through the executable and fake clipboard.
        long_text = "αβ line with enough text for a useful boundary\n" * 160
        long_fixture = work / "long.txt"
        long_fixture.write_text(long_text, encoding="utf-8")
        result, payloads = invoke(
            "chunked-file",
            [long_fixture.name],
            input_bytes=b" " * 300,
            limit=350,
        )

        reconstructed = ""
        chunk_ok = result.returncode == 0 and len(payloads) > 1
        try:
            for part in payloads:
                text = part.decode("utf-8")
                chunk_ok = chunk_ok and len(text) <= 350
                _, body = text.split("---\n", 1)
                reconstructed += body
        except (UnicodeDecodeError, ValueError):
            chunk_ok = False

        chunk_ok = chunk_ok and reconstructed == long_text
        check(
            "oversized capture chunks and reconstructs exactly",
            chunk_ok,
            f"status={result.returncode}, parts={len(payloads)}, "
            f"reconstructed_chars={len(reconstructed)}, "
            f"expected_chars={len(long_text)}, stderr={result.stderr!r}",
        )

        # Cancelling after the first chunk must not send later chunks.
        result, payloads = invoke(
            "chunk-cancellation",
            [long_fixture.name],
            input_bytes=b"q",
            limit=350,
        )
        check(
            "chunk sequence cancellation stops after current part",
            result.returncode == 0 and len(payloads) == 1,
            f"status={result.returncode}, parts={len(payloads)}, "
            f"stderr={result.stderr!r}",
        )

        # A clipboard failure must stop the sequence immediately.
        result, payloads = invoke(
            "chunk-copy-failure",
            [long_fixture.name],
            input_bytes=b" " * 300,
            fail_copy=True,
            limit=350,
        )
        check(
            "chunk clipboard failure stops sequence",
            result.returncode == 1
            and len(payloads) == 1
            and b"wl-copy exited with status" in result.stderr,
            f"status={result.returncode}, parts={len(payloads)}, "
            f"stderr={result.stderr!r}",
        )

    print(f"\n{PASSED} passed, {FAILED} failed", flush=True)
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())

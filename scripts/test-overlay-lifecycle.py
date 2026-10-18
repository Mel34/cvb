#!/usr/bin/env python3
"""Integration test for unprivileged fuse-overlayfs isolation.

Uses disposable directories only. Does not modify the CVB working tree,
require root, or change the system clipboard.

Run: scripts/test-overlay-lifecycle.py
"""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

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


def run(command, *, timeout=10):
    """Run a command with captured output and a bounded execution time."""
    return subprocess.run(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        check=False,
    )


def test_snapshot_contract(root):
    snapshot_project = globals().get("snapshot_project")
    check("snapshot helper is implemented", callable(snapshot_project))
    if not callable(snapshot_project):
        return
    source = root / "snapshot-source"
    snapshot = root / "snapshot-copy"
    (source / "src").mkdir(parents=True)
    (source / "target").mkdir()
    (source / "src" / "main.rs").write_bytes(b"modified\n")
    (source / "untracked.txt").write_bytes(b"untracked\n")
    (source / "target" / "artifact").write_bytes(b"exclude\n")
    snapshot_project(source, snapshot)
    check("snapshot preserves modified file", (snapshot / "src" / "main.rs").read_bytes() == b"modified\n")
    check("snapshot preserves untracked file", (snapshot / "untracked.txt").read_bytes() == b"untracked\n")
    check("snapshot excludes target", not (snapshot / "target").exists())
    (snapshot / "src" / "main.rs").write_bytes(b"candidate\n")
    check("snapshot is isolated", (source / "src" / "main.rs").read_bytes() == b"modified\n")
def main():
    fuse_overlayfs = shutil.which("fuse-overlayfs")
    fusermount = shutil.which("fusermount3") or shutil.which("fusermount")

    check("fuse-overlayfs available", fuse_overlayfs is not None,
          "Install the fuse-overlayfs package if missing.")
    check("FUSE unmount helper available", fusermount is not None,
          "Expected fusermount3 or fusermount.")

    if fuse_overlayfs is None or fusermount is None:
        print(f"\n{PASSED} passed, {FAILED} failed", flush=True)
        return 1

    with tempfile.TemporaryDirectory(prefix="cvb-overlay-test-") as tmp:
        root = Path(tmp)
        test_snapshot_contract(root)
        lower = root / "lower"
        upper = root / "upper"
        work = root / "work"
        merged = root / "merged"

        for directory in (lower, upper, work, merged):
            directory.mkdir()

        original = b"baseline content\n"
        baseline_file = lower / "baseline.txt"
        baseline_file.write_bytes(original)

        # Record the exact lower-tree bytes before mounting.
        before = {
            path.relative_to(lower): path.read_bytes()
            for path in lower.rglob("*")
            if path.is_file()
        }

        check("disposable lower fixture created",
              before == {Path("baseline.txt"): original})

        mounted = False
        try:
            mount_result = run([
                fuse_overlayfs,
                "-o",
                f"lowerdir={lower},upperdir={upper},workdir={work}",
                str(merged),
            ], timeout=10)

            # A successful exit is necessary, but verify the merged view too.
            mounted = (
                mount_result.returncode == 0
                and (merged / "baseline.txt").is_file()
            )

            check(
                "mount succeeds and exposes lower file",
                mounted,
                f"exit={mount_result.returncode}, "
                f"stdout={mount_result.stdout!r}, "
                f"stderr={mount_result.stderr!r}",
            )

            if not mounted:
                return 1

            check(
                "merged view reads baseline",
                (merged / "baseline.txt").read_bytes() == original,
            )

            # Modify an existing lower file through the merged mount.
            changed = b"candidate content\n"
            (merged / "baseline.txt").write_bytes(changed)

            # Create a new file through the merged mount.
            new_content = b"candidate-only file\n"
            (merged / "candidate.txt").write_bytes(new_content)

            check(
                "existing-file modification is visible in merged view",
                (merged / "baseline.txt").read_bytes() == changed,
            )
            check(
                "new file is visible in merged view",
                (merged / "candidate.txt").read_bytes() == new_content,
            )

            # Neither operation may alter the lower layer.
            after = {
                path.relative_to(lower): path.read_bytes()
                for path in lower.rglob("*")
                if path.is_file()
            }

            check(
                "lower layer remains byte-identical",
                after == before,
                f"before={before!r}, after={after!r}",
            )

            check(
                "modified file is stored in upper",
                (upper / "baseline.txt").is_file()
                and (upper / "baseline.txt").read_bytes() == changed,
            )
            check(
                "new file is stored in upper",
                (upper / "candidate.txt").is_file()
                and (upper / "candidate.txt").read_bytes() == new_content,
            )

        except (OSError, subprocess.TimeoutExpired) as error:
            check("overlay operations complete", False, str(error))

        finally:
            if mounted:
                unmount_result = run(
                    [fusermount, "-u", str(merged)],
                    timeout=10,
                )

                check(
                    "overlay unmount succeeds",
                    unmount_result.returncode == 0,
                    f"exit={unmount_result.returncode}, "
                    f"stdout={unmount_result.stdout!r}, "
                    f"stderr={unmount_result.stderr!r}",
                )

        # Check after unmount that the original layer is still unchanged.
        final = {
            path.relative_to(lower): path.read_bytes()
            for path in lower.rglob("*")
            if path.is_file()
        }

        check(
            "lower layer remains unchanged after unmount",
            final == before,
            f"expected={before!r}, actual={final!r}",
        )

    print(f"\n{PASSED} passed, {FAILED} failed", flush=True)
    return 1 if FAILED else 0


if __name__ == "__main__":
    raise SystemExit(main())

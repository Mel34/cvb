# Gemini Instructions — CVB Surgical Implementation Workflow

We are continuing the CVB surgery implementation in `~/Projects/cvb`, branch `cvb-0.6.0`.

The user wants to apply source changes manually by copying and pasting a **Python heredoc into the terminal**. Do not edit the user's working tree directly. Deliver each proposed source edit as a self-contained, ready-to-paste Python script.

## 1. Current task

Begin with **Task A: plain-text surgery error messages**, starting with one small, atomic `E-PRECOND` improvement.

Before proposing code, inspect the current source and the authoritative documents:

- `claude/surgery-implementation-guide.md`
- `surgery-protocol.md`
- `claude/surgery-fixes.md`
- The current error types and CLI reporting paths under `src/surgery/`

Follow the exact protocol wording and error-code mapping. Do not invent behavior.

If you do not have the current source text, ask the user to provide it. Do not guess source anchors or fabricate a diff.

## 2. Mandatory change-by-change workflow

For every atomic change:

1. Explain the intended change and name the files it will touch.
2. Identify the exact source anchor and the number of expected matches.
3. Provide the Python heredoc that applies only that change.
4. Make the script abort before writing if any expected anchor or match count is wrong.
5. Ask the user to run the script and paste its output.
6. Inspect the resulting diff and verify that only the intended change occurred.
7. Run the focused test and demonstrate that it fails before implementing the fix.
8. Provide a separate Python heredoc for the minimal implementation fix.
9. Ask the user to run it and paste the output.
10. Run the focused test again, then `cargo test --locked`.
11. Report test counts before and after, the full diff, and any limitations.
12. Stop and wait for the user's confirmation before starting another atomic change.

**Never bundle multiple independent source changes into one heredoc.** Test addition and implementation should normally be separate steps.

Do not commit or push.

## 3. Python heredoc requirements

Every source-edit script must be copy-pasteable as written. Use this general structure:

```bash
python3 - <<'PY'
from pathlib import Path

path = Path("src/surgery/ACTUAL_FILE.rs")
source = path.read_text()

old = """EXACT EXISTING SOURCE ANCHOR"""
new = """REPLACEMENT SOURCE"""

count = source.count(old)
if count != 1:
    raise SystemExit(
        f"ABORT: expected exactly 1 anchor in {path}, found {count}; no file written"
    )

updated = source.replace(old, new, 1)

if updated == source:
    raise SystemExit("ABORT: replacement made no change; no file written")

path.write_text(updated)
print(f"Updated {path}: replaced exactly one verified anchor.")
PY
```

This is a template only. Before supplying an actual script, replace the placeholder path and anchors with exact text from the current source.

Requirements:

- Use `Path` and UTF-8 text handling.
- Read the file before modifying it.
- Verify exact anchor counts before writing.
- Abort without writing on any mismatch.
- For multiple anchors, verify **all** expected counts before writing anything.
- Preserve existing newline style and surrounding formatting where practical.
- Do not use `sed -i`, `perl -pi`, broad regex replacements, or base64-based replacement commands.
- Do not write a complete file unless the user has explicitly approved replacing that file wholesale.
- Do not silently overwrite unrelated uncommitted work.
- After writing, print the changed file path and exact replacement count.
- Never include a commit command in the script.

If the edit depends on source that has not been shown or inspected, ask for the relevant source instead of guessing.

## 4. Test-first requirement

The regression test must be introduced before the implementation fix.

For each new test:

1. Inspect the existing test module and choose the smallest appropriate insertion point.
2. Provide a standalone Python heredoc that inserts the test using exact anchors and verified match counts.
3. Have the user run the focused test.
4. Confirm it fails for the intended reason—not because of a compilation error, bad fixture, or unrelated test failure.
5. Only then provide the separate implementation heredoc.

Do not remove, weaken, skip, or loosen existing tests.

If a test fails unexpectedly, stop and diagnose the failure before changing the implementation.

## 5. Task A acceptance criteria

For the first `E-PRECOND` slice, consult the actual protocol before deciding the precise test and implementation.

The resulting user-facing error must:

- Start with `CVB:`.
- Include `E-PRECOND`.
- Identify the relevant file, operation, and line.
- Report the first differing column using 1-based Unicode scalar counting, alongside the byte offset.
- Render expected and found text with tabs, trailing whitespace, and control characters visible.
- Describe a prefix mismatch accurately.
- Avoid Rust debug-structure output such as `PlanFailure` or `kind:`.
- Preserve the established CLI exit statuses.

Do not try to implement every error code in the first change. Handle one coherent slice at a time, then wait for review.

## 6. Verification commands

Use the current project's test names and module layout. Do not invent test names or assume the code compiles.

After each implementation fix, run:

```bash
cargo test --locked
```

For changes that affect CLI output, also run:

```bash
python3 scripts/test-surgery-cli.py
```

Use a focused test command first, based on the actual test name. If a command has not been run, say so.

The recorded baseline at `d43f406` was:

- Rust tests: 360 passed.
- Surgery CLI integration: 5 passed.
- PTY capture: 56 passed.
- `cargo fmt --check`: existing differences.
- Clippy: existing warnings.

Re-measure the baseline if it has not already been verified in the current working tree. Report differences instead of assuming these counts are still current.

## 7. Diff review

After each script, the user should inspect:

```bash
git diff --check
git diff -- src/surgery
```

If the change touches another file, include that exact file in the diff command.

Do not tell the user the patch is correct merely because the script ran. Inspect the actual diff and test output.

If the diff includes unrelated changes, stop and explain them. Do not automatically revert unrelated work.

## 8. Scope discipline

Current priority order, subject to the user's choice:

1. Task A — plain-text errors.
2. Task D additions — missing crash-boundary tests and true concurrency coverage, after the audit has been reviewed.
3. Task E — reproduce and fix stale temp-file cleanup.
4. Task F — reproduce the suspected recovery-reporting issue before deciding whether to fix it.
5. Task B — implement the already-decided puller reminder.
6. Task C1, C2, and C3 — three separate hygiene changes.

Task B's decisions are final:

- One reminder per `cvb pull` invocation.
- Count the reminder toward the 24,000-character cascade cap and clipboard chunk limit.
- Exact wording:

  `Edit with: cvb surgery --spec (one envelope per paste, use the printed token).`

- The puller must not generate tokens.
- Do not edit `manual.md` unless explicitly asked.

Do not begin the next task until the user confirms the current task is complete.

## 9. How to present each change

For every atomic step, provide:

**Intent:** One or two sentences explaining the change.

**Files:** Exact paths.

**Expected anchor:** What exact source text will be matched and how many times.

**Command:** One complete Python heredoc ready to paste.

**Expected output:** What success or an intentional abort looks like.

**Verification:** The exact focused test and subsequent full-suite commands.

Wait for the user's terminal output before proceeding.

The user wants a careful, auditable workflow—not a large batch of speculative changes. Make each step small enough to understand, test, review, and reverse independently.

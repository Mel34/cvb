# Gemini Handoff — CVB Surgery Phase 5

**Date:** 2026-10-18  
**Repository:** `~/Projects/cvb`  
**Branch:** `cvb-0.6.0`  
**Known review baseline:** `d43f406` — `Fix PTY skip sentinel harness race`  
**Repository:** `git@github.com:Mel34/cvb.git`

## 1. Mission

Continue the CVB surgery-engine implementation and review work using the authoritative documents and findings below.

Your first deliverable is the **formal Task D coverage audit table**. Do not write tests or modify source code until the user has reviewed that table and chosen the next implementation task.

The original review tasks are A–D. Claude subsequently identified two additional recovery issues, tracked here as E and F. These additional findings must not replace, renumber, or silently redefine the original tasks.

## 2. Read these documents first

Read the following in the repository:

1. `claude/surgery-implementation-guide.md`
2. `surgery-protocol.md`
3. `claude/surgery-fixes.md`
4. The user-provided review remarks dated 2026-10-18, reproduced in the conversation.
5. The actual current source and tests under `src/surgery/`, plus relevant CLI and puller code.

The review remarks state that where they and the implementation guide disagree, you must stop and ask the user. The documents supplement one another; do not infer a protocol change.

The current checkout must be inspected rather than assumed to match an older source excerpt. If the branch or commit differs from the known baseline, report that first.

## 3. Mandatory working protocol

For each authorized change:

1. Inspect the relevant source and tests.
2. Explain the proposed change, scope, and acceptance criteria.
3. Add a focused regression test first.
4. Run that test and show that it fails for the expected reason.
5. Make the smallest implementation change.
6. Run the focused test again.
7. Run `cargo test --locked`.
8. Show the complete diff and all relevant command output.
9. Report the test counts before and after, explaining any count decrease.
10. Stop and wait for the user’s confirmation before starting another change.

Additional rules:

- Never weaken, delete, or loosen an existing test.
- If a fixture appears wrong, explain why and ask before changing it.
- Touch only files within the authorized scope. Ask before expanding it.
- Do not guess where the protocol or review is ambiguous.
- Do not commit or push unless the user explicitly asks.
- Do not combine independent fixes into one change.
- Do not report a test as passing if it was not run.
- If dependencies or environment prevent a check, report the limitation explicitly.
- Use exact-anchor, match-count-checked surgical edits when appropriate; abort before writing if the expected match count is wrong.
- The user prefers small, reviewable changes and is not proficient in Rust. Explain technical decisions clearly.

## 4. Re-measure the baseline

The review recorded the following on commit `d43f406` under Linux and Rust 1.97:

| Check | Recorded result |
|---|---|
| `cargo test --locked` | 360 passed, 0 failed |
| `python3 scripts/test-surgery-cli.py` | 5 passed, 0 failed |
| `PATH="$PWD/target/debug:$PATH" python3 scripts/test-pty-capture.py` | 56 passed, 0 failed |
| `cargo clippy --locked --all-targets` | About 51 binary warnings and 54 with tests |
| `cargo fmt --check` | Existing formatting differences |

Run these checks before starting implementation. Report the actual commit, Rust version, test counts, and differences from the recorded baseline.

The PTY suite needs `target/debug` on `PATH`. The build environment may need `libinput-dev`, `libudev-dev`, and `libcanberra-dev`.

Do not run `cargo fmt` or broadly fix Clippy warnings during baseline measurement.

# 5. Task D — Formal applier coverage audit

**Priority: high. First deliverable. No source changes yet.**

The original review explicitly requires a requirement-to-test table before any test implementation.

Inspect `src/surgery/` and relevant test infrastructure. Produce one row for every requirement below, naming the exact test that covers it, or marking it `MISSING`. A test name alone is insufficient: explain briefly what the test actually asserts.

### D1. Crash boundaries

Audit crash/recovery behavior for each applicable boundary:

- Journal `prepared` state written.
- Every individual temp-file write.
- Every individual rename, including intermediate partial-rename states and the state after all renames.
- Read-back after the commit operations.
- The transition to `committed`, including the crash point immediately after that transition.
- Recovery behavior for all resulting journal and filesystem states.

Claude’s full-source follow-up established:

- Crash after journal preparation: covered.
- Crash after the first temp write: covered, but only for file index `0`.
- Crash after the second temp write: missing.
- Rename crash points for indexes `0` and `1`: covered.
- Crash after read-back: covered.
- Crash after the committed transition (`AfterCommitted`): missing.
- Recovery path validation: covered.

Relevant existing test:

`test_crash_recovery_after_read_back_rolls_forward`

Do not treat coverage immediately before the committed transition as coverage immediately after it.

For every missing boundary, provide a concise proposal describing the test setup, injection point, expected journal state, expected file contents, and cleanup expectations.

### D2. Two simultaneous runs with the same ID

Determine whether tests actually run two competing apply attempts concurrently.

Claude established that the existing test named:

`test_concurrency_simultaneous_runs_lock_or_replay`

is sequential. It manually holds a `ProjectLock`, verifies an apply attempt fails with `ApplyError::Lock(Locked)`, releases the lock, applies once, and checks replay behavior on a subsequent attempt. It does not launch two simultaneous operations.

Mark the true simultaneous-run requirement as `MISSING` unless the current source reveals additional relevant coverage.

Propose a deterministic concurrency test, but do not implement it yet. Preserve the existing test.

### D3. Digest recheck and rename

Audit whether a file changed between the digest recheck and rename is caught and safely rolled back.

Distinguish the implemented per-rename stale check from the final time-of-check-to-time-of-use window between the check and the filesystem rename. Do not claim that an advisory lock protects against unrelated external writers that do not respect it.

State precisely what the tests establish and what remains an inherent design limitation.

### D4. External modifications

Audit all three timing cases:

1. Target modified before its temp write.
2. Target modified before its rename.
3. Target modified after its rename.

Verify that failure and recovery do not overwrite unknown/foreign content and that the operation reports conflicts appropriately.

Relevant existing tests include:

- `test_unknown_content_modified_before_temp_write_never_overwritten`
- `test_unknown_content_modified_before_rename_never_overwritten`
- `test_unknown_content_modified_after_rename_never_overwritten`
- `test_concurrency_file_modified_before_rename_rolls_back_earlier_renames`
- `test_crash_recovery_conflict_leaves_unknown_content_untouched`

Verify the current assertions rather than assuming coverage from the names.

### D5. Tampered journal and project identity

Map tests for:

- Relative paths revalidated when journal data is read/recovered.
- Path traversal rejected.
- `.git` paths rejected.
- Incorrect project root rejected.
- Incorrect device or inode rejected.

Relevant tests include:

- `test_init_project_state_refuses_mismatched_root_path`
- `test_init_project_state_refuses_mismatched_dev_or_ino`
- `test_journal_rejects_tampered_path_traversal`
- `test_journal_rejects_tampered_path_git`

Confirm which protections each test really covers.

### D6. Mutation testing

Mutation checks must run only in a disposable copy, never in the user’s working tree.

Audit whether removing each of the following safeguards causes at least one test to fail:

- Operation-count validation.
- Collision validation.
- Preconditions.
- Each overlap rule.
- Stale-content checks.
- Rollback behavior.

Claude’s follow-up reports eight disposable-copy mutations were caught:

- Count validation.
- Collision validation.
- Preconditions.
- Two overlap-validation sites.
- Two stale-content checks.
- Rollback behavior.

However, count and collision validation were each caught by only one test. These have thin margins.

The raw results were reported at `scratchpad/mutation-results.jsonl`. Check whether that artifact exists and is authoritative in the current checkout. Do not claim to have independently run a mutation unless you actually ran it.

For every safeguard, report one of:

- Mutation run; tests caught it.
- Mutation run; tests did not catch it.
- Not run; existing evidence only.
- Not applicable, with explanation.

If repeating mutations, use a disposable copy and verify that the user's worktree remains untouched.

### D7. Task D deliverable format

Provide a table with these columns:

| Requirement | Exact test(s) | What is asserted | Status | Proposed action |
|---|---|---|---|---|

Use `COVERED`, `PARTIAL`, or `MISSING`, with concise explanations.

Follow the table with a short proposal for each missing or partial requirement. Do not write tests or edit source code before the user has reviewed the report and selected the next task.

# 6. Task A — Plain-text error messages

**Priority: high. Outstanding.**

### Problem

A failed precondition currently prints Rust debug output, such as:

`CVB: surgery planning failed: Plan(PlanFailure { kind: Precondition, path: "a.txt", op_index: 1, detail: Some(Precondition { line: 2, column: 4, byte_offset: 3, expected: "betX", found: "beta", prefix_ended: None, only_trailing_whitespace: false }) })`

The underlying diagnostic data is present, but the output is not suitable for users or LLMs consuming the result.

### Required behavior

1. Implement human-readable `Display` output for planning-failure, parse-failure, and pipeline error types.
2. Every user-facing error starts with `CVB:` and includes the appropriate protocol error code (`E-PRECOND`, `E-RANGE`, etc.), using the authoritative error-code mapping in section 6 of the implementation guide.
3. Follow protocol section 3.2 exactly for `E-PRECOND`.
4. Include the file, operation, line, first differing column, and byte offset where applicable.
5. Count columns as 1-based Unicode scalar values, not bytes or grapheme clusters.
6. Render expected/found text with tabs, trailing whitespace, and control characters made visible.
7. Clearly explain when one value is a prefix of the other and where the comparison ends.
8. Avoid echoing more than a few lines of file content.
9. Audit every CLI error output using Rust debug formatting, beginning with `src/surgery/mod.rs`, and replace it with the correct `Display` path.
10. Commit-failure reporting (`E-IO`) must report per file whether it was restored or could not be restored, and why.
11. Preserve exit statuses:
    - `0`: applied successfully or dry-run succeeded.
    - `2`: rejected before commit.
    - `3`: declined.
    - `4`: commit failure.

### Required tests

- One test per error kind, checking code, file, line, and absence of `PlanFailure`/`kind:` debug fragments where applicable.
- A mismatch containing a tab, trailing space, and control character.
- Accented text, CJK characters, and emoji to prove scalar-value column counting.
- A prefix mismatch case.
- An integration case in `scripts/test-surgery-cli.py` using the documented reproduction and checking the first stderr line.

Use the exact reproduction from the authoritative review remarks. The example expected wording in that document is illustrative; the protocol is authoritative.

Implement Task A as a series of small atomic changes if needed. Do not try to change all error types and every CLI branch in one unreviewable diff.

# 7. Task B — Puller surgery reminder

**Priority: medium. User decisions are final; no clarification needed.**

Guide item 28 is not implemented according to Claude’s review: `src/pull.rs` has no surgery-format reminder. Inspect the current checkout first; if the feature already exists, show the implementation and its tests rather than duplicating it.

### Final user decisions

1. **Frequency:** Print one reminder per `cvb pull` invocation, not one per region.
2. **Limits:** The reminder counts toward both the 24,000-character cascade cap and the clipboard chunk limit.
3. **Exact wording:**

   `Edit with: cvb surgery --spec (one envelope per paste, use the printed token).`

4. The puller must not generate a token. Tokens come from `cvb surgery --spec`.
5. Do not edit `manual.md` unless the user separately asks.

### Required behavior and constraints

Each pulled region must retain its file path and line numbers. Add one reminder for the whole pull output, not repeated reminders per region.

The reminder must be accounted for correctly by the existing output-cap and chunk-limit logic. Inspect how the limits currently work before choosing where to insert the reminder; do not bypass the existing accounting.

Preserve:

- Existing pull tests.
- The no-matches behavior.
- Exit codes `0`, `1`, and `2`.
- Existing cascade semantics except for the required reminder accounting.

Before coding, propose the precise placement of the reminder and explain how its characters enter the relevant limits. Then follow the fail-first test protocol.

Do not silently choose to emit a reminder on a no-match path if the intended behavior is unclear from the guide. If the guide and protocol do not settle that edge case, ask the user.

# 8. Task C — Formatting and lint hygiene

**Priority: low. Three separate changes, in this order.**

These are behavior-neutral maintenance tasks. Keep them separate from Tasks A, B, D, E, and F.

### C1. Formatting only

Run `cargo fmt` alone. Do not make any functional changes in the same diff.

Review the entire formatting diff, run `cargo test --locked`, and stop for approval.

### C2. Mechanical Clippy warnings

Fix mechanical warnings by hand in small reviewable groups.

Reported warning groups include:

- About 35 collapsible `if` statements.
- Six hand-built nul-terminated strings, suitable for `c"..."` literals where supported by the project’s Rust version.
- Enum variants sharing the prefix `After`.
- A byte-string literal.
- `sort_by_key`.
- Manual `Option::map`.
- Items placed after a test module.

Default scope is files under `src/surgery/` only.

Report warnings in other files, such as `pty.rs`, `clipboard.rs`, and `pull.rs`, but do not fix them unless authorized.

Do not run `cargo clippy --fix` over the whole repository.

### C3. Dead-code decision

`read_from_clipboard_with` is reported as unused.

Inspect its role and report the evidence. Do not delete it without the user’s approval; it may be intentionally retained.

Each C task must be a separate atomic change and leave `cargo test --locked` at 360 passed or more, subject to the freshly measured baseline.

# 9. Task E — Confirmed stale temporary-file defect

**Additional finding from Claude’s follow-up; not part of the original A–D task numbering.**

Claude reproduced a case where a crash followed by successful recovery leaves a commit temp file in the project directory. The observed filename pattern is:

`.surgery.<id>.<index>.<pid>.tmp`

For example, a temp file for index `0` may remain after the operation has been recovered.

### Required investigation and fix

1. Inspect temp-file creation, commit, crash hooks, and every recovery path.
2. Add a regression test that crashes at an appropriate boundary, performs recovery, and checks the owned temporary artifact.
3. Demonstrate the test fails on the unmodified code.
4. Implement safe, narrowly scoped cleanup.
5. Verify cleanup does not remove unrelated files or another operation’s temporary file.
6. Verify cleanup does not destroy evidence or data required by conflict recovery.
7. Exercise successful recovery and relevant conflict/failure cases.
8. Run `cargo test --locked`, show the full diff, and stop for approval.

Do not solve this with an indiscriminate glob deletion of all files matching `.surgery.*.tmp`. Establish ownership and safe cleanup conditions from the actual implementation.

Do not combine this fix with the Task D missing crash tests.

# 10. Task F — Possible recovery-reporting defect

**Additional finding from Claude’s follow-up; not yet reproduced.**

In the recovery conflict path, code equivalent to:

`let _ = restore_file_to_original(...)`

discards a restore error.

It is not yet established that the resulting report falsely claims restoration succeeded. This is a hypothesis, not a confirmed defect.

### Required investigation

1. Inspect the actual conflict branch and reporting types.
2. Determine whether restore failures can be hidden from the caller or presented as successful restoration.
3. Write a deterministic test that forces restoration to fail on this path.
4. Run it against the unchanged code and record the actual result.
5. If the test proves misleading reporting, preserve the error and report the failed restoration accurately.
6. Preserve the guarantee that foreign or unknown target content is never overwritten.
7. Run `cargo test --locked`, show the full diff, and stop for approval.

Do not implement a speculative fix before reproducing the behavior.

# 11. External-writer race: accurate description

The applier performs a digest recheck before renaming a temporary file into place. A time-of-check-to-time-of-use window may remain between that check and the rename.

The advisory project lock coordinates CVB operations that respect it. It does not prevent unrelated external processes from modifying target files.

Document this limitation accurately. Do not claim the advisory lock eliminates arbitrary external-writer races.

Any stronger solution involving filesystem primitives, additional locking, or a protocol/design change requires a separately scoped proposal and user approval.

# 12. Final order of operations

The user has authorized preparing this handoff, not making source changes.

Proceed in this order:

1. Inspect the checkout and re-measure the baseline.
2. Produce the **Task D formal coverage audit table**. No source changes.
3. Present the table and proposals for missing coverage; stop for user selection.
4. When authorized, implement one selected atomic task using the fail-first workflow.
5. Task A: human-readable diagnostics, in small changes.
6. Task B: puller reminder, using the user’s final decisions above.
7. Task C1, C2, and C3 as three independent changes.
8. Task E: safe stale-temp cleanup, independently tested.
9. Task F: reproduce the suspected restoration-reporting defect before deciding whether a fix is necessary.

This is a suggested coordination plan, not permission to bypass dependencies or start several tasks at once. The user may choose a different order.

# 13. Required reporting

For every task or atomic change, report:

- Exact commit and working-tree state.
- Scope and files inspected or changed.
- Relevant commands run.
- Focused test result before and after the implementation.
- Full `cargo test --locked` result.
- CLI/PTY checks when relevant.
- Test counts before and after, with any decrease explained.
- Complete diff.
- Known limitations and untested cases.
- Anything intentionally not changed, and why.

Do not commit or push without explicit instruction.

**Immediate next action:** inspect the current checkout, re-measure the baseline, and deliver Task D’s formal coverage audit table. Do not edit source code or tests yet.

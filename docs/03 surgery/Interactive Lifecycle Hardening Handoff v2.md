# CVB 0.7.0 — Interactive Lifecycle Hardening Handoff

**Repository:** `Mel34/cvb`  
**Local path:** `~/Projects/cvb`  
**Branch:** `cvb-0.7.0`  
**Last confirmed commit:** `d720c2d` — `test: cover clipboard write failure during surgery`  
**Status:** Hardening in progress

## 1. Working protocol — mandatory

### Inspect GitHub first

**Code inspection should primarily happen directly on GitHub, against the latest pushed commit.**

Before proposing a change:

1. Identify the latest commit on `cvb-0.7.0`.
2. Inspect the relevant source files directly on GitHub.
3. Trace the production code path and existing regression tests.
4. Identify one concrete, demonstrable gap in the hardening plan.
5. Propose one atomic change and explain its purpose before generating a CVB surgery envelope.
6. Inspect the candidate diff before applying it.
7. Apply the candidate, run the focused test, and then run the appropriate broader verification.
8. Inspect the resulting diff and commit only with the user's explicit authorization.
9. After pushing, verify the remote branch and commit.

Do not routinely use `cvb pull` to inspect source code already available on GitHub. Avoid unnecessary local source dumps and broad pulls.

### When to use CVB pulls

Use a CVB pull when the change introduces code that has not yet been committed and pushed, and we need to inspect the actual resulting local code or its surrounding context.

The normal cycle is:

**GitHub inspection → atomic surgery → candidate review → apply → focused test → full verification → local diff review → authorized commit and push → GitHub verification.**

If the surgery result is ambiguous, incomplete, or inconsistent with the candidate, inspect the actual local changes before proceeding. Do not assume the applied result exactly matches the intended change.

### Atomic-change discipline

- One independently reviewable change at a time.
- Prefer regression tests that exercise production code paths.
- No unrelated refactoring during hardening.
- No commit or push without explicit permission.
- Never claim a change is verified merely because a candidate was generated or applied.
- Record verified outcomes separately from planned work.

### CVB Surgery Language (CSL)

Use the session's current CSL token supplied by the user or current terminal session. Do not reuse a token from an older session without confirmation.

Every structural envelope line must use the complete `CVB::<token>::` prefix. The `n=` payload count is optional for `OLD`, `ANCHOR`, and `NEW`; include it when necessary to disambiguate payload boundaries or preserve content reliably.

Inspect the candidate diff before applying it. After application, verify the relevant behavior through tests.

## 2. Last confirmed state

The following was reported by the user in the previous session:

- Branch: `cvb-0.7.0`
- Commit: `d720c2d`
- Commit message: `test: cover clipboard write failure during surgery`
- Push succeeded: `2aa3db7..d720c2d`, `cvb-0.7.0 -> cvb-0.7.0`
- `cargo test --locked --quiet`: **482 passed, 0 failed**
- `git diff --check` produced no errors.
- Final reviewed change was limited to `src/pty.rs`, adding 32 lines for one regression test.

These are the last confirmed results, not a substitute for checking the current remote HEAD at the start of the next session.

## 3. Recently completed hardening work

### A. Initialization-failure cleanup

Previously recorded as implemented.

The intended invariant is that failures occurring after `SurgeryBegin`, including terminal-guard or `/dev/tty` initialization failures, must not silently strand the other side in an active surgery lifecycle. Cleanup should be best-effort, while preserving the original error and intended exit status.

**Next-session requirement:** Inspect the implementation at the latest pushed GitHub HEAD and confirm its regression coverage. Do not reopen it without evidence of a gap.

### B. Production-path capture regression

Previously recorded as implemented.

The intended invariant is that capture tests exercise logic shared with the real production path rather than validating only a separate test implementation.

**Next-session requirement:** Verify the current implementation and tests on GitHub. Record the result in the final audit.

### C. Clipboard-write failure regression — completed in `d720c2d`

Added:

`clipboard_write_failure_is_propagated_without_changing_surgery_state`

The test injects a clipboard backend failure and asserts that:

- The error is propagated.
- The surgery proxy remains `Active` with the original token.
- The transcript's segments remain unchanged.

The focused test passed, followed by the complete suite of 482 tests. The change was committed and pushed.

This test covers the specific injected failure path; it does not establish that every clipboard, control-channel, or lifecycle failure path is fully hardened.

## 4. Remaining work

### Priority 1 — Audit control-channel EOF and lifecycle handling

Begin with `src/control.rs` and the relevant control-channel handling in `src/pty.rs`.

Inspect the pushed source on GitHub and trace actual callers before proposing changes.

Verify these cases:

- Clean EOF versus EOF during an incomplete frame.
- Truncated and malformed frames.
- Partial reads and writes, including interrupted system calls.
- Zero-byte writes and disconnects.
- Maximum frame or message sizes.
- How control-channel errors affect the parent/child lifecycle.
- Whether disconnects can leave surgery state active or prevent required cleanup.

Some low-level behavior has already been implemented, so the task is to establish which invariants are already satisfied and identify only demonstrable gaps.

**Deliverable:** A short audit table identifying the invariant, implementation location, existing test coverage, and result. Add a regression test only when the inspection reveals a meaningful coverage gap.

### Priority 2 — Audit the mandatory fd3 control channel

Inspect the production setup and teardown paths.

Verify:

- Failure to establish the expected control channel.
- Unexpected closure or loss of the channel.
- Invalid or incomplete control messages.
- Behavior when the child or parent exits unexpectedly.
- Whether errors propagate instead of being silently swallowed.
- Whether shutdown and cleanup remain bounded and do not leave stale lifecycle state.

Distinguish intentional best-effort cleanup from failures that must be reported.

Do not change code simply because an error is ignored: first establish whether that error is intentionally non-fatal and whether the original failure remains observable.

### Priority 3 — Audit token validation and message sequencing

Inspect token validation and the state machine in the pushed source.

Verify:

- Mismatched tokens are rejected.
- Messages that are invalid in the current state cannot silently alter state.
- `Active`, `Aborted`, and completed states have consistent transitions.
- A failed operation does not accidentally advance or corrupt state.
- Duplicate, delayed, or out-of-order lifecycle messages behave intentionally.

Reuse existing tests where they already establish the invariant. Add focused regression coverage for uncovered behavior rather than duplicating equivalent tests.

### Priority 4 — Audit cleanup and error precedence across the full lifecycle

Trace all production paths from `SurgeryBegin` through normal completion or failure.

Include:

- Successful completion.
- Explicit abort.
- Initialization failures.
- Clipboard failures.
- Control-channel failures.
- Unexpected process termination.
- Cleanup failures occurring while another error is already active.

Verify terminal restoration, state transitions, notification behavior, and exit-status semantics where applicable.

In particular, determine whether a cleanup failure can mask the original error, whether the other process can be left waiting indefinitely, and whether a lifecycle notification is required on each path.

Do not impose a universal rule that every cleanup error must override or be ignored. Follow the intended semantics established by the implementation and existing contract.

### Priority 5 — Final verification and evidence

Once the identified gaps have been addressed:

1. Run focused regression tests for each changed behavior.
2. Run `cargo test --locked --quiet`.
3. Run the project's established verification script, if applicable, and inspect any failures.
4. Run `git diff --check`.
5. Inspect the complete final diff and working-tree status.
6. Confirm that changes are limited to the intended files.
7. Commit and push only with explicit user authorization.
8. Verify the resulting commit on GitHub.
9. Repeat any required verification against the exact final pushed HEAD.

The final hardening report should distinguish code-inspection findings, tests that were executed, and behaviors that remain unverified.

## 5. Recommended starting action

Start the next session by checking the latest GitHub HEAD and inspecting the control-channel EOF path in `src/control.rs`, followed by its production callers in `src/pty.rs`.

Do not begin with a broad CVB pull. First establish whether the existing code already handles the suspected edge cases. Then select one specific gap, explain the proposed atomic fix or regression test, and wait for the user's approval before generating the surgery envelope.

## 6. Definition of done

Interactive lifecycle hardening is complete when:

- Every identified lifecycle invariant has an explicit audit result.
- Failure paths have appropriate regression coverage.
- Tests exercise production behavior.
- Control-channel and cleanup failures have intentional, documented semantics.
- No known gap remains unresolved without an explicit limitation.
- The complete verification suite passes.
- The final diff is clean and narrowly scoped.
- The exact tested code is committed and pushed.
- The final report clearly states what was verified and what was not.

**Guiding principle:** Inspect the code on GitHub first, make only evidence-driven atomic changes through CVB surgery, and use local pulls when the uncommitted surgery result itself needs inspection. Do not confuse successful tests with a completed audit.


# CVB 0.7.1 — Interactive Surgery Hardening Follow-up

**Repository:** `~/Projects/cvb`  
**Base branch:** `cvb-0.7.0`  
**Status:** Proposed  
**Purpose:** Close the remaining lifecycle, test-quality, and verification gaps identified during the final review of CVB 0.7.0.

## 1. Objectives

Address three shortcomings without reopening the completed interactive surgery implementation unnecessarily:

1. Guarantee that failures during interactive-mode initialization terminate an already-started surgery session.
2. Strengthen the capture-suppression regression test so that it exercises production logic rather than duplicating the implementation's condition.
3. Complete a reproducible final audit of the control channel, framing, token validation, lifecycle transitions, and regression results.

**Working principle:** One atomic change at a time. Inspect before editing, apply changes through CVB's interactive surgery workflow, and verify each change before proceeding.

## 2. Step A — Close the Interactive Initialization Lifecycle Gap

### A.1. Inspect the existing lifecycle

Review `src/surgery/mod.rs`, particularly `run_surgery_mode()`.

The current sequence sends `ControlMessage::SurgeryBegin` before initializing the terminal guard and opening `/dev/tty`. If either operation fails, the function returns without sending `ControlMessage::SurgeryEnd`.

The PTY proxy can therefore remain in `SurgeryProxyState::Active` after interactive surgery has exited.

### A.2. Define the required invariant

Once `SurgeryBegin` has been successfully sent, every handled initialization failure must attempt to send `SurgeryEnd { status: 1, ... }` before returning.

This applies to:

- Failure to enter `SurgeryTerminalGuard`.
- Failure to open `/dev/tty`.
- Any additional initialization failure discovered during inspection after `SurgeryBegin`.

A failed `SurgeryBegin` itself does not require `SurgeryEnd`, because the session was not successfully initialized through the control channel.

The cleanup must preserve the original initialization error and exit status. A secondary failure while sending `SurgeryEnd` must not obscure the primary error.

This guarantee concerns handled error paths; it does not promise cleanup after an uncatchable process termination or abrupt system failure.

### A.3. Implement the smallest safe change

Inspect the existing control-channel calls and terminal-guard cleanup semantics before editing.

Prefer a narrowly scoped fix that:

1. Preserves the current initialization order unless changing it is demonstrably necessary.
2. Sends the failure-status `SurgeryEnd` on the two identified error paths.
3. Retains existing terminal restoration behavior.
4. Avoids introducing a broad lifecycle abstraction or unnecessary refactoring.

If a shared cleanup helper materially simplifies the implementation, consider it only after inspecting the surrounding code.

### A.4. Add regression tests

Add deterministic tests for both initialization failures:

- Terminal-guard initialization fails after `SurgeryBegin`.
- `/dev/tty` opening fails after `SurgeryBegin`.

Each test must verify that the control-channel lifecycle ends with a failure-status `SurgeryEnd` and that the original failure remains observable.

Reuse existing PTY and CLI test infrastructure where practical. If either failure cannot be induced reliably in the current harness, introduce the smallest test seam needed to simulate it without weakening production behavior.

Also verify that the existing successful and declined-confirmation flows continue to behave as documented.

### A.5. Acceptance criteria

- Both previously unhandled error paths attempt `SurgeryEnd { status: 1, ... }`.
- Terminal restoration and existing exit-status behavior remain correct.
- New regression tests fail against the old behavior and pass against the fix.
- Existing surgery and PTY tests remain green.

## 3. Step B — Strengthen the Capture-Suppression Regression Test

### B.1. Inspect the current test

Review `src/pty.rs` and the test `terminal_capture_and_transcript_suppressed_when_not_ordinary`.

The existing test reproduces the conditional logic used by the production proxy. Such a test can continue passing if production behavior later diverges from the duplicated condition.

### B.2. Establish a shared production path

Refactor only enough to let production code and the regression test exercise the same capture-decision logic.

The shared logic must enforce the intended rule:

- Capture is permitted only when `capture.capturing` is true and `surgery_state` is `SurgeryProxyState::Ordinary`.
- Capture remains suppressed in `Active`, `Completed`, and `Aborted` states.
- Transcript accumulation remains consistent with the intended proxy lifecycle.

Prefer testing the production helper or production behavior directly over maintaining a second copy of its conditional logic.

Do not extract an abstraction solely to test a trivial Boolean expression if a stronger, practical test of the actual capture path is available.

### B.3. Add or adapt assertions

Verify that:

1. Ordinary state with capture enabled records output.
2. Ordinary state with capture disabled does not record output.
3. Active, Completed, and Aborted states do not accumulate terminal output in the ordinary capture buffer.
4. Surgery-state transitions do not leave unwanted terminal bytes in the transcript.
5. Existing ordinary capture behavior remains unchanged.

### B.4. Acceptance criteria

- The regression test exercises logic used by production.
- All relevant state combinations are covered.
- No new test-only duplication of the production decision is introduced.
- Existing PTY and capture tests pass.

## 4. Step C — Complete the Security and Lifecycle Audit

This is an audit, not a request to rewrite already functioning subsystems.

### C.1. Control-channel framing — Step D

Inspect `src/control.rs` and the PTY integration in `src/pty.rs`.

Verify:

- Header and payload framing is unambiguous.
- Partial reads and writes are handled correctly.
- Incomplete frames remain buffered until complete.
- Invalid message types, malformed payloads, and oversized frames fail safely.
- EOF and control-channel errors do not leave the proxy in an inconsistent state.

Add tests only for concrete gaps found during inspection.

### C.2. Mandatory control channel — Step A

Verify that:

- Interactive surgery requires the expected control channel on fd 3.
- Failure to establish or use the channel produces a clear error.
- The child receives the intended descriptor without accidentally retaining unwanted descriptors.
- An unrelated or invalid descriptor cannot silently substitute for the control channel.
- Existing no-session tests cover the actual CLI and PTY integration.

### C.3. Authoritative clipboard writer — Step B

Verify that:

- Clipboard writes are accepted only in the intended proxy state.
- Clipboard-write failures propagate through the control handler.
- The proxy does not report a successful write when the underlying clipboard operation fails.
- Failure handling leaves the session lifecycle consistent.
- The existing clipboard writer remains the authoritative writer for surgery output.

Do not introduce a second clipboard-writing path.

### C.4. Proxy lifecycle and token validation — Step C

Verify the permitted state transitions among `Ordinary`, `Active`, `Completed`, and `Aborted`.

Check that:

- A surgery session can begin only under the intended conditions.
- The session token is checked before activating surgery.
- Mismatched or missing tokens cannot activate the session.
- Clipboard writes and session-end messages are handled only in appropriate states.
- Repeated, stale, or out-of-order control messages do not produce invalid transitions.
- Terminal output and transcript handling remain consistent across transitions.

Document any existing behavior that is intentional rather than changing it merely for uniformity.

### C.5. Error handling and abort semantics — Steps E and G

Verify that:

- Clipboard-read and invalid-UTF-8 failures reach the mode state machine.
- An armed candidate disarms after a paste error.
- Interactive aborts display the documented outcome.
- Handled failures after session initialization terminate the proxy lifecycle.
- The specification printed by the CLI comes from the authoritative section of `manual.md`.

### C.6. Audit outcome

For each invariant, record one of:

- **Verified by inspection**
- **Verified by an existing test**
- **Verified by a new test**
- **Outstanding issue**

Do not mark an invariant verified merely because adjacent tests pass.

## 5. Step D — Verify the Exact Final Branch Head

After the fixes and audit, establish the exact commit being verified.

Run the following from `~/Projects/cvb`:

```bash
git status --short --branch
git log -1 --format='%H%n%s'
git diff --check
cargo test --locked pty
cargo test --locked surgery
python3 scripts/test-surgery-cli.py
cargo test --locked --quiet
```

If a command fails, stop and investigate before proceeding. Use detailed test output for diagnosis rather than stacking additional changes on top of a failure.

Then verify:

- The working tree and branch state are understood.
- The new regression tests are included in the final test run.
- The reported test counts come from actual output, not estimates.
- `git diff --check` is clean.
- The pushed branch contains the exact code that was tested.

If the code changes during the audit, rerun the affected tests and then the complete final suite.

GitHub CI results may supplement this evidence, but must correspond to the commit being reported.

## 6. Dogfooding Workflow

Use CVB's interactive surgery module for implementation rather than Python heredocs or broad scripted file replacement.

For each atomic change:

1. Inspect the relevant implementation, tests, and surrounding lifecycle.
2. State the specific defect and intended behavior.
3. Prepare a narrowly scoped surgery envelope.
4. Review the proposed diff before confirmation.
5. Apply the surgery through the interactive workflow.
6. Run the smallest relevant regression test.
7. Inspect the resulting diff and check for unintended modifications.
8. Continue only when the current change is understood and verified.

Use separate surgery operations for the lifecycle fix, lifecycle regression tests, capture-test improvement, and any concrete audit findings. Do not combine unrelated modifications into one large envelope.

If the surgery module itself prevents a safe or testable change, stop and diagnose that issue rather than bypassing the workflow silently.

## 7. Recommended Execution Order

| Order | Work item | Completion condition |
|---|---|---|
| 1 | Inspect initialization lifecycle and test harness | Failure paths and viable test seams identified |
| 2 | Fix initialization cleanup | Both handled failures attempt `SurgeryEnd` |
| 3 | Add lifecycle regression tests | Both tests reproduce the old defect and pass with the fix |
| 4 | Strengthen capture suppression test | Production decision logic is directly exercised |
| 5 | Audit framing and fd 3 setup | Concrete invariants checked and gaps recorded |
| 6 | Audit clipboard writer, token checks, and state transitions | Security and lifecycle findings resolved or documented |
| 7 | Run complete verification | Exact test output and commit identified |
| 8 | Commit and push | Remote branch contains the verified implementation |

If the audit reveals a new defect, give it its own narrowly scoped change and regression test. Reassess the remaining sequence before proceeding.

## 8. Completion Criteria

The follow-up is complete when:

- Both interactive initialization error paths are covered by regression tests.
- Capture suppression is tested through production logic.
- The control-channel, token, clipboard, and lifecycle audit has a documented outcome.
- All required tests pass on the exact final commit.
- The pushed branch matches the verified implementation.
- No unrelated refactoring or feature expansion has been introduced.

**Final deliverable:** A verified CVB hardening follow-up with reproducible test evidence and a clear distinction between inspected behavior, tested behavior, and any remaining limitations.


# Amendment 2 to design v2.3: reconciliation with the milestone 0 audit

Precedence: this amendment wins over amendment 1 (`...-amendment-insert-anchors.md`), which wins over `design-surgery-relocation-v2.3.md`, wherever they conflict. Source: the milestone 0 audit of `cvb-0.7.0` (read-only, source-based; no tests were run and no failure injection was performed).

## B1. Confirmed consistent with the design (no change)

- Coordinates: all ops for a file are validated against the original snapshot and applied as one simultaneous edit set in descending position. Ops need not be in file order. The same-snapshot invariant in 2.2 holds.
- Final newline: the planner already preserves the final-newline state (a terminator at the end if and only if the original had one; an unterminated last line is an ordinary line). Audit item 8 is closed. No design change.
- Text handling: target files must be valid UTF-8; NUL, a leading BOM, lone CR and mixed line endings are rejected at load. Envelope payloads are UTF-8 text with literal backslashes and no escape syntax; NUL and CR in payload blocks are rejected. Raw envelope payloads in v1 stay as designed.
- The diff renderer's output string is shared by the pager and the clipboard, so the new format needs to be implemented once.
- A journal retaining the original bytes of every file already exists.

## B2. Lock (replaces "Lock requirement" in v2.3 2.6)

Keep the existing persistent sidecar lock, `project.lock` in the project's state directory, taken with a non-blocking exclusive `flock` and held for the commit only. It is a separate file from the targets, so it keeps its identity when a target is replaced. My earlier recommendation of a parent-directory lock is withdrawn: it was made without knowing a stable sidecar already exists, and replacing a working mechanism needs a demonstrated reason.

Withdrawn as unnecessary: sorted acquisition across several locks (there is one project-wide lock, so no ordering problem) and the bounded 5-second wait. The existing behavior, immediate failure with `E-LOCK` and a clear message, stays unless usage shows a need to wait.

Requirements to verify and test:
1. CVB never removes and recreates the lock file while another process may hold it.
2. Every session that commits to the project tree uses the same lock. The lock location comes from the state directory (`XDG_STATE_HOME`, else `HOME`), and candidate sessions override state locations by design for their own workspace, which is fine for the workspace. When promotion replays changes into the real tree, it must take the real project's lock.
3. Tests: lock identity across target replacement; two competing processes; recovery after a partial failure.

## B3. Commit order (replaces the "Two-phase commit" steps in v2.3 2.6)

Today's sequence, under the lock: hash every planned target and compare, write the `Prepared` journal (digests and original bytes for every file), write and sync temp files, then for each file re-check its digest and rename, then best-effort directory sync, read-back verification, and mark `Committed`.

So an all-files hash check before the journal already exists. What is missing is a full re-check after the temp files are written and before the first rename. Changes:
- Add a final check of every target (re-hash all of them) after the temp files are built and before the first rename. Any mismatch aborts, removes the temp files, and writes nothing.
- Keep the existing per-file digest re-check immediately before each rename. It keeps each file's window small.
- Content hashes are the authoritative check. The inode, size and mtime tuple in v2.3 is dropped: the guarantee concerns bytes, and a hash already detects every byte change.
- Record ledger steps only after the journal reaches `Committed`. A failed or rolled-back commit then records nothing, and the file's bytes match the ledger's expectation again.

Guarantee, unchanged in spirit: a mismatch found by either check before the first rename writes nothing. A change by a non-cooperating writer after the final check and before that file's rename is still possible; a mismatch found at a later file's per-file re-check after earlier renames goes through the existing recovery. The window is narrowed, not closed, and is documented in the manual.

## B4. Rollback and recovery

Build on the existing journal. Recovery classifies each file as original, planned or other; it rolls back or forward when files are uniformly one state, restores planned files from retained originals when mixed, and on unknown content reports a conflict without overwriting that file and leaves the transaction `Prepared`, which blocks further surgery with `E-RECOVER`. Preserve all of this. Rollback is conditional, not unconditional, and the design says so.

Required: direct failure-injection tests for an ordinary rename failure (rollback succeeds) and for a restoration failure (the reported per-file state is correct). The existing crash-recovery tests simulate a crash by skipping immediate recovery and do not cover these.

Open item for the owner: the audit found no user-facing recovery or undo command, while an unfinished journal blocks further surgery. Confirm how a user clears a `Prepared` journal today. Adding such a command is out of scope for this work.

## B5. Symlinks and path changes

The loader walks each path component with `symlink_metadata` and rejects symlinks and non-regular files. The applier does not repeat that check before rename, and `rename` replaces the directory entry rather than writing through a link. Recommended hardening for milestone 2: repeat the path-component check as part of the final check (reject, write nothing). This adds a rejection on the safe side, so it needs the owner's approval.

## B6. Renderer

Existing diagnostics appear inline in changed lines (`[trailing-whitespace]`, `[tab-indent]`, `[zero-width]`, `[bidi]`, `[control]`), and escapes are mixed (`\u{...}` for zero-width and bidi, `\xNN` for some controls, backslashes not doubled). Replace all of it with the v2.3 1.5 scheme: diagnostics move to the notes section, escapes use `\u{h}` with doubled backslashes on transformed lines, each named in the notes. NUL, lone CR and a leading BOM cannot occur in a loaded target, so they never need rendering, and no invalid-byte escape is needed. The existing file-ending labels and summary lines move out of the body (header or notes). The existing `\ No newline at end of file` emission is kept and validated per 1.3.

## B7. Pull output

Pull emits `path:LINE: text` records, reads UTF-8, shows raw characters, and does not represent line terminators. v2.3's statement that pull follows the same escaping and naming rule is withdrawn: an LLM quotes envelope expected text from pull output, envelope text is raw, so pull stays raw.

Open item for the owner: the `: ` between the line number and the text has the same shape as the diff bug, a separator space before source text, and could make an LLM count one extra leading space. Decide on evidence (has ChatGPT shown this confusion with pull output?). Options: leave it; add a legend line saying the text starts after the first `: `; or drop the space after the second colon. Not part of milestone 1 unless the owner decides.

## B8. Test changes

Remove: inode/size/mtime checks, sorted multi-lock ordering and bounded-wait tests. Add: a full final check across all files (a mismatch on file B, found before the first rename, leaves file A unwritten); failure injection per B4; lock identity per B2; symlink replacement per B5 if adopted; ledger steps recorded only after `Committed`, and nothing after a rollback.

## B9. Handoff changes

Milestone 0 is complete and under review. Milestone 1 also unifies the escape scheme (B6). Milestone 2 follows B2 to B5. Milestone 6 records ledger steps after `Committed`.

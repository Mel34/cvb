# Handoff: surgery diff format, commit safety and anchor relocation

Supersedes `claude/handoff-surgery-relocation.md` (written against design v1; its rules and milestone order are out of date). Source of truth for every rule below: `claude/design-surgery-relocation-v2.3.md`. Give that document to the implementer together with this one. Section numbers below refer to it. This file is the work order; the design doc has the grammar, algorithms, edge cases and the full test matrix.

## Context

CVB is a Rust + Bash project. `cvb surgery` applies LLM-specified edits (replace/delete/insert, each with exact expected existing text and verified anchors) from a clipboard envelope, transactionally, with an interactive diff review. Two problems drive this work: the surgery diff puts an extra separator space after the +/- marker, which makes an LLM think payload lines carry an extra leading space; and line numbers in a later envelope go stale after an earlier surgery changes the file's line count.

Work is done as atomic changes with tests. Development branch as of 2026-10-21 was `cvb-0.7.0`; confirm the current branch before starting. Deliver each milestone as one commit the user applies with `git am`.

## Process rules

- Start with inspect-and-explain. Read first, report what you found, then make surgical changes with tests.
- One milestone per commit. Each is small, reviewable and independently tested. Preserve existing behavior unless the design deliberately changes it.
- After each milestone, report: what changed, what tests were added, anything that contradicted the design. If the code contradicts the design, stop and report instead of improvising.
- No implementation of a milestone before its prerequisites are reviewed (see ordering).

## Do not change

Bash history sync, shell-state restore, PTY lifecycle, the surgery protocol and session-token rules, the size caps (3 MiB per envelope, 8 MiB per file, 32 MiB aggregate), the all-or-nothing rule, mixed-line-ending rejection, and the rule that reviewed bytes equal committed bytes. `--yes` stays deferred. Envelope payloads and expected text stay raw bytes in v1.

## Milestone 0: read-only source audit (written report, no code)

Produce a report answering design section 5, items 1 to 8, with file and function references:

1. Do all ops in an envelope carry hints in one coordinate system (one snapshot, applied simultaneously), and how do the validator and protocol define ranges and anchors?
2. How does the applier write files today: locking, temp file and rename, permissions, fsync? What identity does any existing lock have across rename? Is the file re-checked just before the write?
3. How does a multi-file envelope achieve all-or-nothing today? What happens if a failure occurs after some files are renamed? Are originals retained for rollback?
4. What does the journal keep, how does rollback or undo work, and how do they interact with the file bytes?
5. Is the diff renderer shared between the screen pager and the clipboard output? What does pull output do with line numbers, gutters and unusual characters?
6. How are insert anchors represented, and must ops be in file order?
7. Are non-UTF-8 files accepted? How do envelope payloads treat backslashes and unusual characters? How are symlinked targets handled?
8. What does "final-newline state preserved" currently do when the last line of an unterminated file is deleted or replaced? Proposed definition: the result ends with a terminator if and only if the original did.

The report also proposes the lock mechanism (parent-directory lock, recommended, or sidecar file) and states whether multi-file rollback exists. Stop after delivering it and wait for review.

## Milestone 1: diff contract (rendering only)

Prerequisite: audit item 5. Implement design 1.2, 1.3 and 1.5:
- Changed lines are marker immediately followed by source text; context lines are one space plus the text. No separator.
- Hunk headers: omitted counts mean 1; the renderer emits the canonical form without `,1`; counts of 0 follow standard unified diff; the parser accepts both spellings.
- Op and relocation labels on the hunk header after the closing `@@`. One-line legend before the diff.
- Commentary only in a `=== notes ===` section after the diff, shown only when needed. Notes name op, line and side.
- Transformed lines (the escape class in 1.5): `\u{h}` escapes, doubled backslashes on those lines, each named in the notes. Parser unescape is a single left-to-right pass.
- `\ No newline at end of file` markers, validated per 1.3 (only after a line that is final on its side; reject misplaced markers). CRLF noted once per file. Paths with control characters rejected.
- A count-based parser that reads the renderer's output back, and the full test list in design 4.1: single-line hunks, adversarial source lines (`-- foo`, `=== notes ===`, `+++ b/x`, `--- a/x`, `@@ -1 +1 @@`, a literal no-newline line), escape edge cases, marker validity, CRLF, round trip.

Done when the golden and property tests pass and an LLM-facing diff contains no text that is not marker, source, header or notes.

## Milestone 2: commit-time verification and locking

Prerequisite: audit report reviewed; lock mechanism chosen. Implement design 2.6:
- Stable lock identity across rename (not a lock on the target's own inode). Sorted acquisition order for multiple files, bounded wait about 5 seconds, then abort with nothing written. Never held during interactive review.
- Two-phase commit: prepare (verify hash, capture inode/size/mtime, build temp files, retain originals), final check (re-stat and re-hash every file), commit (renames, directory sync), then record. Every check precedes the first rename.
- Rollback on failure after the first rename, integrated with the existing transaction mechanism and proven by failure injection.
- Document the residual race with non-cooperating writers plainly in the manual. Do not add the hard-link backup (deferred).

Tests: design 4.3, commit section (changed between planning and commit; multi-file mismatch leaves other files untouched; failure injection after first rename; lock identity across replacement; contention and lock ordering; candidate sessions share the lock; symlinked target).

## Milestone 3: mismatch diagnostics

No behavior change. On a precondition mismatch, report the original hint, the expected block (escaped and named per 1.5), the actual text at the hint plus or minus 2 lines, any exact candidates elsewhere in the file (read-only listing), and a remark when the expected text contains characters shown escaped. Build the whole-file exact search here; later milestones reuse it.

## Milestone 4: resolver and chooser

Implement design 2.1 to 2.5 as a pure function over (snapshot lines, parsed ops with hints, optional ledger view), ledger absent at this stage:
- Candidate set `C` from whole-file exact search of the full block (no window). Insert anchors resolve as one adjacent unit.
- Decision table: `|C| = 0` abort with diagnostics; `|C| = 1` use it, labeled `exact`, `shift-tracked` or `searched`; `|C| > 1` choose-required (no freshness evidence exists in v1, so the "hint known to be current" row is never taken).
- Chooser screen before the diff: up to 5 candidates with line numbers and two lines of context, sorted by distance from the hint, preselected one marked (stated line if it matches). Select by number, abort with `q`; Space and Enter do not select. More than 5 candidates aborts with "anchor too generic." Non-interactive and `--dry-run`: print candidates, apply nothing, exit non-zero.
- Joint validation of the resolved ops: overlapping footprints rejected; an op intersecting another insert's anchor lines rejected; two inserts at the same boundary rejected (both ops named); two ops with identical expected blocks where either needs relocation rejected with an explicit error and nothing written (documented v1 limitation).
- Relocated ops labeled in the diff. Relocation disabled when the applier replays changes mechanically (overlay promotion). Reviewed bytes equal committed bytes: the diff, confirmation and commit consume the same resolved plan.

Tests: design 4.2.

## Milestone 5: post-apply line map

Derive a piecewise original-to-current line map from the applied edit set (not from the ledger) and print it in the output the LLM sees, only for files that moved (design 2.7). Tests include shifts that cancel, replaced ranges, and the tail segment.

## Milestone 6: edit ledger (build only after reviewing chooser data)

Do not start until the user has seen how often the chooser appears and how often the preselected candidate is chosen. Then implement design 2.3: per-file hash chain, observation points (pull, planning, commit check), `NoView -> Valid -> Broken`, only a new pull revives `Broken`, steps freed on the first observed mismatch, mapping rules and the shift-tracked label and rewritten-region warning. The ledger never makes an automatic choice among duplicates. Tests: design 4.3 ledger and mapping sections, including the boundary cases and the property test.

## Milestone 7: end-to-end tests

Multi-operation envelopes, repeated blocks, insertions, deletions, replacements, ambiguous mappings, sequential envelopes on one file, and multi-file commits.

## Deferred (do not build)

Optional view tag; envelope escape syntax; hard-link backup; ordering rule for same-boundary inserts; joint assignment for identical blocks; indentation-heuristic note.

## Ordering

0 first, then wait for review. Then 1, 2 and 3 in order. 4 after 3. 5 any time after 2. 6 only when asked. 7 last. A milestone that depends on an audit answer that contradicts the design stops and reports.

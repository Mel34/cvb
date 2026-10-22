# Handoff: surgery diff gutter fix and anchor relocation

Read `claude/design-surgery-relocation.md` first; it is the source of truth for decisions and test cases. This file is the work order.

## Context

CVB is a Rust + Bash project. `cvb surgery` applies LLM-specified edits (replace/delete/insert, each with exact expected existing text and verified anchors) from a clipboard envelope, transactionally, with an interactive diff review. Work is done as atomic changes with tests. Development branch as of 2026-10-21 was `cvb-0.7.0`; confirm the current branch before starting. Patches are delivered as commits the user applies with `git am`.

Do not redesign: Bash history sync, shell-state restore, PTY lifecycle, the surgery protocol/token rules, the 3 MiB / 8 MiB / 32 MiB caps, or the all-or-nothing rule. `--yes` stays deferred.

## Process

Start with inspect-and-explain: locate the diff rendering code, the anchor/precondition verification code, and where applied surgeries and pull output are tracked. Report what you found and how you will fit the design in, then make surgical changes with tests. One milestone per commit.

## Milestones

1. **Diff gutter fix (independent, do first).** Render changed lines as marker immediately followed by the line (`-line`, `+line`), context lines as one space plus the line, matching unified diff exactly. Add the one-line legend to every diff (and to pull output if it has a gutter). Tests: no separator space on changed lines; legend present; output matches unified-diff semantics.
2. **Resolver tiers 1 and 3.** Stated-line exact match, then whole-file search for the full expected block (no window). Unique match: relocated op labeled `[searched]` with hint line, found line and offset. Absent: abort with diagnostics. Re-run overlap/ordering validation after resolution. Insert anchors resolve as one adjacent unit.
3. **Shift tracking (tier 2).** Per-file in-session ledger of applied surgeries (position, removed, added) plus the point where the file was last shown by pull. Order: stated line, then shift-translated line, then search. Applied automatically, labeled `[shift-tracked]`. A pull of the file resets its ledger.
4. **Ambiguous matches.** When the block matches several places, list up to 3 candidates sorted by distance from the hint, let the user choose by key or abort. Never auto-pick. Nothing is written until chosen.
5. **LLM feedback.** Mismatch diagnostics (expected text, actual text at hint plus or minus 2 lines) and a post-apply shift summary in the output the LLM sees.

## Constraints

- Exact content matching only; fuzzy is for location.
- Relocation always flagged in the diff and always confirmed; reviewed bytes equal committed bytes.
- Mixed envelopes apply together or not at all.
- Update the LLM-facing manual: line numbers are hints, text is truth, prefer multi-line anchors, the diff gutter is only the first column.

## Done when

All test cases listed in the design doc pass, the existing ~190 non-surgery tests and ~90 surgery tests still pass, and each milestone is its own commit.

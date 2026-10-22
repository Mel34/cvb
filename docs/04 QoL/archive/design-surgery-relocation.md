# CVB surgery: diff gutter fix and anchor relocation (design)

Status: design accepted in conversation 2026-10-22. No code yet. Companion handoff: `claude/handoff-surgery-relocation.md`.

## Problem 1: the diff's extra space confuses ChatGPT

Observed in the surgery diff output (`[space-indent]` marks the separator space):

```
- [space-indent]     if !is_left_pane {
+ [space-indent]     if !is_left_pane || app.system_upgrade_in_progress {
```

The diff prints a marker (`-`/`+`), then a separator space, then the source line. Every changed line therefore looks like it has one more leading space than the file. ChatGPT reads the diff, concludes the payload carries a leading space, and reports that the parser is adding indentation. The parser is not at fault; the display is.

### Decision

Make the diff follow the standard unified-diff convention exactly: the marker is immediately followed by the line bytes, with no separator.

- removed line: `-` + line
- added line: `+` + line
- context line: ` ` (one space) + line, as in unified diff

Add a one-line legend at the top of every diff and every pull output that has a gutter, e.g.: `Gutter is the first column only (+, -, or space); line text starts at column 2. Envelope payload lines never include the gutter.`

Rationale: the bug exists because CVB deviated from the format LLMs know best. Returning to it is the lowest-risk fix. If ChatGPT still stumbles on context lines (leading space), the fallback is an explicit non-space delimiter after the marker (for example `│`), plus the legend; decide only if the standard format proves insufficient.

Optional, low priority: on insert/replace, warn in the dry run when every payload line is indented exactly one space deeper than the anchor lines. Replace ops are already protected by exact precondition text.

## Problem 2: line numbers drift between surgeries

ChatGPT applies surgery A, line numbers in the file shift, and the follow-up surgery B carries anchors computed from the pre-A file, so B is rejected.

This revises the earlier rule "never search for a better location, mismatch means stop." The revised rule: line numbers are hints, the expected text is the truth, and relocation is allowed only when the text matches exactly, is labeled in the diff, and goes through the normal confirmation.

### Resolution order for each op

1. **Stated line.** Exact match of the expected block at the stated line. Same as today. Tried first so that hints the LLM already corrected are never double-shifted.
2. **Shift-tracked line (deterministic, automatic).** The session keeps a per-file ledger of edits applied by surgery (position, lines removed, lines added) and the point at which the file was last shown to the LLM (pull output). Translate the hint through every surgery applied to that file since it was last shown, then test for an exact match there. Applied automatically; labeled in the diff.
3. **Whole-file search.** Search the entire file (no window) for the full expected block, all lines, not a single line.
   - exactly one match: offer it as relocated
   - several matches: list up to 3 candidates with line numbers, sorted by distance from the hint; the user picks by key or aborts; never auto-pick
   - no match: abort the whole surgery with diagnostics (below)

Insert ops: the before and after anchors are one adjacent unit; search for them together. Single-line anchors like `}` will often be ambiguous, so multi-line anchors are preferable and the LLM-facing manual should say so.

### Invariants that do not change

- Content matching is exact. Fuzzy applies to location only. Whitespace-insensitive matching is out of scope for v1.
- All-or-nothing: any unresolved or ambiguous op aborts the envelope; nothing is written.
- After all ops are resolved to final positions, the existing overlap and ordering validation runs again. Two ops resolving to the same place are rejected.
- Relocation is never silent. Each relocated op is flagged in the diff, with how it was found:
  - `relocated: hint L120 -> L134 (+14) [shift-tracked]`
  - `relocated: hint L120 -> L88 (-32) [searched]`
- Interactive confirmation stays. Reviewed diff bytes are exactly the committed bytes. `--yes` stays deferred.
- Line endings and final-newline state preserved; size caps (3 MiB per envelope, 8 MiB per file, 32 MiB aggregate) unchanged.

### Feedback to the LLM

- **Rejection diagnostics.** On a mismatch with no exact match anywhere, report the expected text and the actual text at the hint line plus or minus 2 lines, so the model can correct itself on the retry.
- **Shift summary after a successful apply**, in the output the LLM sees, e.g. `src/pull.rs: +14 lines at L120; file now 812 lines`. The next surgery then starts from correct numbers.

### Test cases (minimum)

- Stated line matches: no relocation, no label.
- Surgery A inserts N lines above surgery B's anchor: B resolves via tier 2, labeled `[shift-tracked]`.
- Surgery A removes lines above B's anchor: negative shift works.
- LLM already corrected its numbers after A: tier 1 matches, no double shift.
- Hint wrong, block unique elsewhere in file: tier 3, labeled `[searched]`, far from the hint (no window).
- Block appears 2 or 3 times: candidates listed, nothing applied until chosen; abort works.
- Block absent: abort, nothing written, diagnostics show actual text near the hint.
- Two ops resolve to overlapping ranges: rejected.
- Insert with multi-line anchors relocated as a unit; anchors that no longer adjoin: rejected.
- Mixed envelope (one exact, one relocated): applied together or not at all.
- Diff gutter: changed lines contain no separator space; legend present; byte-for-byte equal to unified-diff semantics.
- Ledger reset: after a pull shows the file again, earlier surgeries no longer shift hints.

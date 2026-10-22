# CVB surgery: diff format and anchor relocation, design v2

Status: design review, no implementation. Supersedes `claude/design-surgery-relocation.md` (v1). Written to be self-contained: it can be handed to another reviewer who has not seen v1. It answers the six asks in ChatGPT's "Design Review and Proposed Direction" document.

Limit of this review: I have not read the CVB source. Section 5 is architectural advice from first principles and needs checking against the real planning/applier code.

## 0. Summary of the assessment

Both proposals are sound, and the three-tier idea (stated location, session edit history, whole-file exact search) is the right shape. Changes I recommend:

| Topic | Recommendation |
|---|---|
| Diff body | Pure unified diff, byte-exact. No annotation lines inside it. Diagnostics go in a separate notes section after the diff. |
| Tier precedence | An exact match at the stated line must NOT always win. (This corrects my own v1, which said it should.) |
| Session history | Key it by content hashes, not by a running line offset. Used as a locator, never as proof. |
| Implementation order | Stateless whole-file search before the edit ledger. |
| Shift summary | Piecewise map, not one "lines >= N shifted +K" line. |
| Promotion replay | Relocation must be off when surgery is used to replay changes mechanically (overlay promotion). |

## 1. Diff format

### 1.1 Findings

- Root cause of the reported bug: the diff prints marker, separator space, then source line, so every changed line has one more leading space than the file.
- The proposal's example repeats the hazard. The diagnostic line `  [diagnostic: leading spaces]` begins with a space, so by unified-diff rules it is a context line whose text is `[diagnostic...]`. Anything interleaved in the diff body is read by an LLM through the same first-column rule. Interleaving is the wrong place for commentary.
- Diagnostics such as "leading spaces" on every Rust line are noise. Report anomalies only.

### 1.2 Recommended format (firm)

1. The body is standard unified diff. Column 1 is the gutter: `' '` context, `-` removed, `+` added. Source text starts at column 2, byte for byte, with no separator.
2. File headers `--- a/path` and `+++ b/path`, then hunk headers `@@ -a,b +c,d @@`. Op labels and relocation labels live on the hunk header line after the closing `@@`, e.g. `@@ -118,3 +118,3 @@ op 2 replace [relocated: hint L120 -> L134 +14, shift-tracked]`.
3. A missing final newline uses the standard line `\ No newline at end of file`.
4. A one-line legend precedes the diff.
5. After the diff, a blank line and then a `=== notes ===` section, present only when there is something to say. Notes are never mixed into the body, and each note names the op, line and side.
6. The layout is uniform: every changed line is rendered the same way. Notes are conditional.

Column ruler, before and after (marker is column 1; source indentation is 4 spaces):

```
col:     1234567890
before:  -     if !is_left_pane {     marker, separator space, 4 source spaces
after:   -    if !is_left_pane {      marker, 4 source spaces
```

Sample output after the change:

```
cvb surgery (dry run): 2 ops, 1 file
Legend: column 1 is the gutter (' ' context, '-' removed, '+' added). Source text starts at column 2, byte for byte. Text after "=== notes ===" is commentary, never source.
--- a/src/ui.rs
+++ b/src/ui.rs
@@ -118,3 +118,3 @@ op 1 replace [exact @ L119]
     draw_header();
-    if !is_left_pane {
+    if !is_left_pane || app.system_upgrade_in_progress {
         render_right();

=== notes ===
op 1 L119 +: line ends with 2 trailing spaces
op 1 L121 +: indentation contains a tab (0x09); surrounding context uses spaces
file src/ui.rs: no newline at end of file (preserved)
```

### 1.3 Answers to the five questions

1. Exact format: section 1.2. The payload is whatever follows column 1, so the only rule the LLM needs is "drop one column."
2. Indentation diagnostics: conditional, and only for anomalies (tab/space mix against surrounding context, trailing whitespace). No per-line "leading spaces" notes.
3. Unusual characters:
   - Tabs, trailing whitespace: shown raw in the body, flagged in notes.
   - CRLF: `\r` not shown in the body. One note per file ("CRLF, preserved"). Mixed endings remain rejected.
   - Missing final newline: the standard `\ No newline` line.
   - Control characters (other than tab), bidi controls, zero-width and other invisible characters: recommend rendering them as `\u{XXXX}` escapes in the body, with a note that the line is shown escaped. This is the one deliberate departure from byte-exactness, because raw control characters can also act on the terminal (escape-sequence injection) and the LLM cannot reproduce them reliably anyway. Open question 1.
4. Familiar structure vs reliable extraction: yes, both. The body is plain unified diff; extraction is "strip column 1."
5. Tests: section 4.1.

## 2. Relocation

### 2.1 Principle

Hints locate, exact content verifies. Every tier is only a source of candidate locations. A location is acceptable only if the expected block matches there byte for byte. Nothing in this section weakens the precondition.

### 2.2 Coordinate systems

- **View coordinates (V):** the file version the LLM was last shown, and the line numbers it quotes. CVB cannot observe this. The heuristic is "the last pull of this file in this session," recorded as a content hash. The LLM may be citing an older pull still in its context, which is a further reason hints are never trusted alone.
- **Snapshot coordinates (S):** the current file at planning time. Every op in an envelope resolves against the same snapshot, and the resolved ops are applied as one simultaneous edit set. There is no sequential dependency between ops of one envelope.
- **Result coordinates (R):** the file after a successful apply.

Terms: hint `h` is the line number as stated in the envelope. `S` below is the stated line `h` read as a snapshot coordinate. `T` is `h` translated from V to the snapshot through the ledger.

### 2.3 Edit ledger (tier 2)

Per file, in memory for the session:

- `H_view`: hash of the file when it was last pulled (the V epoch).
- A chain of steps. Each step records `pre_hash`, `post_hash`, and its edit set. An edit is `(start, old_len, new_len)` in the pre-step coordinates of that step; an insertion is `old_len = 0`, `start` = the line it is inserted before.
- A step is committed only after a successful atomic apply. Cancelled or rejected surgeries leave nothing.

The ledger is **valid** when the chain is unbroken from `H_view` to the hash of the current file. Any change made outside surgery (editor, git checkout, undo, journal rollback) changes the hash and breaks the chain, so the ledger is discarded for that file. This is the guard against stale translation.

Mapping one line `n` through one step: for each edit with `n >= start + old_len`, add `new_len - old_len`; if `start <= n < start + old_len`, the line is inside an edited region and is **unmappable**. Edits within one step are simultaneous (all in that step's pre-coordinates); steps compose in order.

Mapping a block `[a, b]`: map `a` and `b`. The block is unmappable if any edit region overlaps it or an insertion falls strictly inside it (`a < start <= b`). An insertion at `start == a` shifts the block, and one at `start == b + 1` does not affect it.

A same-length replacement maps lines identically but changes their content. A later anchor must quote the new text. The exact check enforces this.

### 2.4 Algorithm per op

1. Compute `C`: all exact matches of the whole expected block (all lines) in the snapshot, by whole-file search with no window. Overlapping candidates (a repeated-line block) are distinct candidates.
2. Test whether `S` (the stated line) is in `C`. If the ledger is valid, compute `T` and test whether it is in `C`.
3. Decide:

| `|C|` | Condition | Result |
|---|---|---|
| 0 | | Abort. Diagnostics (2.6). |
| 1 | | Use it. Label `exact` if it equals `S`; `shift-tracked` if it equals `T`; otherwise `searched`. |
| >1 | ledger shows no edits since the view, and `S` in `C` | Use `S`. This is today's behavior. |
| >1 | ledger valid, `T` in `C`, and `S` not a different member of `C` (or `S == T`) | Use `T`, label `shift-tracked`. |
| >1 | `S` in `C` and `T` in `C` but `S != T` | Ambiguous: ask. |
| >1 | anything else (ledger broken, no view recorded, `T` unmappable or not in `C`, only `S` in `C` after edits) | Ambiguous: ask. |

The last row matters: after edits to the file, a stated line that happens to match an identical repeated block is exactly the silent wrong pick this feature exists to prevent.

4. Ambiguous: show up to 5 candidates, each with its line numbers and two lines of context, sorted by distance from the hint. The user picks by key or aborts. If `|C| > 5`, abort with "anchor too generic: N exact matches; quote more context." Under `--dry-run` or any non-interactive mode, print the candidates, apply nothing and exit non-zero. Thresholds are open question 2.
5. Joint validation after every op is resolved: overlap, ordering and dependencies, using the existing validators on the resolved ranges. Two ops resolving to overlapping ranges are rejected.
6. Confirmation. The diff shows every relocated op's label. The reviewed bytes remain exactly the committed bytes. Tier 2 is automatic: no separate prompt beyond the standard confirmation, but always labeled.

Insert ops: the before and after anchors form one adjacent unit; search for the concatenated block. If the pair no longer adjoins anywhere, abort.

Same-envelope duplicates: if two ops share identical expected blocks and either needs relocation, abort in v1. A joint assignment is a later problem.

Promotion replay: when the surgery applier is used mechanically (overlay promotion replaying net per-file changes), relocation is disabled. Replay must be exact or fail.

### 2.5 Reporting shifts after apply

A single "lines >= 120 shifted +14" line is wrong whenever an envelope has more than one edit. Report a piecewise map, original to current, only for regions that moved:

```
src/pull.rs: line map, original -> current (now 812 lines)
  1-119      -> 1-119    (same)
  120-299    -> 134-313  (+14)
  300-302    -> 314      (3 lines replaced by 1)
  303-end    -> 315-end  (+12)
```

Print it in the output the LLM sees, since that is its next set of hints.

### 2.6 Mismatch diagnostics

On abort, report: the original hint; the expected block, displayed safely (same escaping as 1.3); the actual text at the hint plus or minus 2 lines; exact candidates elsewhere in the file, if any (relevant when relocation was declined, for example under `--dry-run`); and how each location was derived (translated by the ledger, or found by search). If the ledger recorded that earlier surgeries removed or rewrote the requested text, say which surgery did.

## 3. Sequencing

1. Diff contract and golden tests. Rendering only, no change to matching or applying.
2. Mismatch diagnostics, including a read-only whole-file candidate listing in rejection messages. No behavior change. The search code built here is reused in step 3.
3. Resolver as a pure function over (snapshot lines, op hints, optional ledger), with the ledger initially absent. Whole-file search, the decision table, the ambiguity chooser, and integration with planning and joint validation.
4. Edit ledger with hash chain, adding tier 2. It only changes outcomes for repeated blocks and for labeling.
5. Post-apply line map.
6. End-to-end tests.

Why search before ledger (this swaps the proposed order): whole-file search is stateless and fixes the reported failure whenever the quoted block is unique, which multi-line anchors usually are. The ledger carries state that must be invalidated correctly, and its value is mainly disambiguating repeated blocks. It is better to land the simple mechanism first and add the stateful one on top.

## 4. Test matrix

### 4.1 Diff contract (golden and property)

- Every changed line is exactly marker + source bytes; no separator.
- Property: for random source lines (leading spaces 0, 1, 4, 8; tabs; trailing whitespace; empty; non-ASCII), stripping column 1 of each `+`/`-`/context line reproduces the source exactly. Exception: lines shown escaped, which are listed in notes.
- Empty added line renders as `+` alone; empty context line as a single space.
- Nothing appears in the body except `---`, `+++`, `@@` lines, gutter lines and `\ No newline`.
- Notes appear only when relevant, after `=== notes ===`, naming op, line and side.
- Legend present. Tab/space mix flagged. Trailing whitespace flagged. CRLF noted once per file. Missing final newline marked. Escaped control and invisible characters flagged.

### 4.2 Resolution

| Case | Expected |
|---|---|
| Block at stated line, unique | `exact`, no relocation label |
| Block at stated line, repeated elsewhere, file unedited since view | stated line wins |
| Prior surgery inserted N lines above; block unique | `shift-tracked` or `searched` (same location) |
| Prior surgery removed lines above | negative shift works |
| LLM already corrected its numbers after the prior surgery; block unique | `exact`, no double shift |
| Same, block repeated, stated and translated lines both match different places | ambiguous: ask |
| Block repeated, ledger valid, only translated line matches | use it, `shift-tracked` |
| Block repeated, ledger broken (external edit, rollback), stated line matches | ambiguous: ask |
| Prior edit overlaps the requested block | tier 2 skipped; search only |
| Hint far from block, block unique | `searched`, no distance limit |
| 2 to 5 candidates | list, user picks, abort works, nothing written before choice |
| More than 5 candidates | abort with "too generic" |
| Block absent | abort, diagnostics show actual text near hint |
| Overlapping candidates in a repeated-line block | each listed separately |
| Insert with multi-line anchors, anchors still adjacent | relocated as a unit |
| Insert anchors no longer adjacent | rejected |
| Two ops resolve to overlapping ranges | rejected |
| Mixed envelope, one exact and one relocated | applied together or not at all |
| Two ops in one envelope with identical expected blocks, one stale | abort (v1) |
| `--dry-run` with ambiguity | candidates printed, non-zero exit, nothing written |
| Promotion replay | relocation disabled; mismatch fails |
| Cancelled surgery | no ledger entry |
| Pull after surgery | ledger resets to the new view |
| Line endings, final newline, caps (3 MiB / 8 MiB / 32 MiB) | unchanged behavior |

## 5. Architecture (unverified against the code)

- Keep resolution as a pure function, separate from both parsing and the applier: input (snapshot lines, parsed ops with hints, optional ledger view), output either resolved ranges plus labels, an ambiguity set, or a mismatch with diagnostics. It is then testable without files or a clipboard.
- Resolution belongs in the planning stage, before the diff is built. The diff, the confirmation and the commit must all consume the same resolved plan so that "reviewed bytes equal committed bytes" cannot be broken by a second resolution.
- The applier should receive only resolved ranges and not know about hints.
- Where the ledger lives depends on how the session and journal are structured. The hash chain means it does not need to be trusted across process restarts, so it can be in memory only.
- Things the code reading must confirm: how ops are ordered and whether file order is required; how insert anchors are represented; whether the diff renderer is shared between the screen pager and the clipboard output (escaping must then be consistent); how the journal and any undo interact with committed ledger steps.

## 6. Open questions

1. Escaping control and invisible characters in the diff body (recommended) versus keeping the body byte-exact and relying on notes only. The trade-off is terminal safety versus strict byte fidelity.
2. Candidate display limit (5) and the "too generic" threshold.
3. Whether a note should flag added lines whose indentation is not a multiple of the file's indent unit. It would catch the original bug class directly, but it is heuristic and could be noisy.
4. Whether the view epoch should be "last pull" or "last time any content of this file reached the LLM" (including surgery diffs). The former is simpler.

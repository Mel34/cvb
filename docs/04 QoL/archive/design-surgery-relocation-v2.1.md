# CVB surgery: diff format and anchor relocation, design v2.1

Status: design revision, no implementation. Supersedes `claude/design-surgery-relocation-v2.md` (and v1). This document is complete and self-contained: a reviewer who has not seen v1 or v2 can use it alone.

Limit: I have not read the CVB source. Anything that depends on how the code is built is listed in section 5 as something to verify before coding, not assumed.

## 0. What changed since v2

| Topic | v2.1 |
|---|---|
| Diff body | A formal grammar. Body length is determined by hunk counts. Escaped lines are named in notes, and backslash is doubled on those lines. |
| Multiple exact candidates | Explicit location confirmation is required whenever drift is possible. The ledger preselects and never silently commits. |
| Commit safety | A layered commit protocol: lock, hash check, temp file, re-check immediately before rename, rename. The residual race is documented. |
| Same-envelope coordinates | Stated as an invariant to verify. Footprint rule and same-boundary insert rule defined. |
| Ledger | Hash points, state machine and invalidation made exact. |

## 1. Diff format

### 1.1 Findings

- The reported bug: the diff printed marker, separator space, then the source line, so changed lines carried one extra leading space.
- Commentary interleaved in the diff body is read by an LLM through the same first-column rule as source. An indented diagnostic line looks like a context line. Commentary does not belong in the body.
- Per-line "leading spaces" notes on every Rust line are noise. Report anomalies only.

### 1.2 Format

1. Body is standard unified diff. Column 1 is the gutter: `' '` context, `-` removed, `+` added. Source text starts at column 2, byte for byte, with no separator.
2. Op and relocation labels sit on the hunk header after the closing `@@`.
3. A missing final newline uses the standard line `\ No newline at end of file`.
4. A one-line legend precedes the diff.
5. Commentary appears only after the diff, in a `=== notes ===` section, present only when there is something to say.
6. The layout is uniform. Notes are conditional.

Before and after (marker is column 1; the source indentation is 4 spaces):

```
col:     1234567890
before:  -     if !is_left_pane {     marker, separator space, 4 source spaces
after:   -    if !is_left_pane {      marker, 4 source spaces
```

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

### 1.3 Grammar (renderer and parser contract)

```
output   := banner LF legend LF file+ notes?
file     := "--- " path LF "+++ " path LF hunk+
hunk     := "@@ -" start "," oldcount " +" start "," newcount " @@" label? LF body
body     := ( gutterline | nonewline )*
gutterline := (" " | "-" | "+") text LF
nonewline  := "\" " No newline at end of file" LF        ; follows the line it qualifies; not counted
notes    := LF "=== notes ===" LF note*
```

Counting rule: a consumer reads gutter lines until `oldcount` (lines with `' '` or `-`) and `newcount` (lines with `' '` or `+`) are both exhausted, treating a following `nonewline` line as part of the previous line. Nothing in the body is interpreted by its content, only by its position. After the counts are exhausted, the next line must be a `@@` line, a `--- ` line (next file), a blank line followed by the notes header, or end of output.

Why content cannot confuse it:
- Every body line has a gutter character in column 1. A removed source line `-- foo` renders as `--- foo` but is read as body, because the counts say it is.
- A source line `=== notes ===` renders as `+=== notes ===`, never as the notes header, which begins with `=` in column 1.
- A source line `+++ b/x` renders as `++++ b/x`, and a source line `@@ -1 +1 @@` as `+@@ -1 +1 @@`, both read as body by count.
- A source line equal to `\ No newline at end of file` renders as `+\ No newline at end of file`. The real marker has `\` in column 1.

Paths containing a newline or other control character are rejected in v1, so header lines stay one line each.

Escaped lines: control characters (other than tab), bidi controls, zero-width and other invisible characters, and lone `\r`, are rendered as `\u{XXXX}` in the body. A literal backslash on an escaped line is rendered `\\`, so a source string that actually contains the text `\u{200b}` cannot be mistaken for an escaped character. Every escaped line is named in the notes ("op 2 L140 +: shown escaped; `\` is doubled on this line"). Lines not named in the notes are raw. A consumer must unescape only the named lines.

### 1.4 Answers to the questions

1. Exact format: 1.2 and 1.3. The payload is whatever follows column 1.
2. Diagnostics are conditional and only for anomalies.
3. Tabs and trailing whitespace: raw in the body, flagged in notes. CRLF: `\r` not shown, one note per file, mixed endings still rejected. Final newline: the standard marker. Control and invisible characters: escaped as in 1.3.
4. Familiar structure and reliable extraction both hold.
5. Tests: 4.1.

## 2. Relocation

### 2.1 Principle

Hints locate, exact content verifies. Every tier is a source of candidate locations, and a location is acceptable only if the whole expected block matches there byte for byte. Exact bytes establish that a block is eligible. They do not establish which identical occurrence was intended, so when occurrences are multiple and drift is possible, a human chooses.

### 2.2 Coordinate systems and the same-envelope contract

- **V (view):** the file version the LLM was last shown, and the numbers it quotes. CVB cannot observe this. The heuristic is the last pull of the file in this session, recorded by hash. The LLM may be quoting an older pull still in its context.
- **S (snapshot):** the current file at planning time.
- **R (result):** the file after a successful apply.

Invariant to document, test and verify against the existing protocol: every op in one envelope carries hints in the same coordinate system, and all ops are resolved against the same snapshot. There is no sequential adjustment between ops of one envelope. The resolved ops are applied as one simultaneous edit set.

Footprint of an op: replaced or deleted lines, plus, for an insert, its before and after anchor lines (which must remain unmodified in the same envelope). Joint validation rejects:
- overlapping footprints;
- an op whose edit range intersects another op's anchor lines (this also covers an insert adjacent to a replaced range);
- two inserts at the same boundary (identical or overlapping anchor pairs). CVB does not define an order for them in v1; the envelope is rejected with a message naming both ops. A deterministic order is a later decision.

If the existing validator or protocol defines ranges differently, settle this contract before coding.

### 2.3 Edit ledger

Hash: SHA-256 of the file's raw bytes (line endings and final newline included, no normalization), per file.

Hash points:
- **Pull:** hash of the bytes the pull read. This is `H_view`, recorded whether the pull showed the whole file or a range.
- **Planning:** hash of the snapshot bytes the plan is built against.
- **Commit:** see 2.6. The file's hash is re-checked against the planning hash.
- **After commit:** `post_hash` is the hash of the exact bytes CVB wrote (the reviewed bytes).

State per file: `NoView -> Valid(H_view, steps) -> Broken`.
- A step is `pre_hash`, `post_hash`, and an edit set. An edit is `(start, old_len, new_len)` in the step's pre-coordinates; an insertion has `old_len = 0` and `start` is the line it is inserted before. Edits within a step are simultaneous. A step is committed only after a successful atomic apply; cancelled or rejected surgeries leave nothing.
- The ledger is valid when the current file's hash equals `H_view` (zero steps) or the last step's `post_hash`. CVB does not classify who made a change. A surgery's own write produces a hash the ledger already expects; any other change (editor, `git checkout`, rebase, undo, journal rollback) mismatches. Identical bytes restored by an external change match again, which is correct because identical bytes mean identical coordinates.
- **Broken:** entered on the first observed mismatch. The steps are freed immediately. Only a new pull of that file leaves `Broken`. Surgeries on a broken file append nothing. All lookups go through one function that returns nothing unless the chain is valid and ends at the current hash, so stale mapping cannot be reached.
- Mapping a line `n` through one step: for each edit with `n >= start + old_len`, add `new_len - old_len`. If `start <= n < start + old_len`, the line is inside an edited region and is unmappable. Steps compose in order.
- Mapping a block `[a, b]`: map both ends. The block is unmappable if an edit region overlaps it or an insertion falls strictly inside it (`a < start <= b`). An insertion at `start == a` shifts the block. One at `start == b + 1` does not affect it.
- A same-length replacement maps lines identically but changes their content. A later anchor must quote the new text, and the exact check enforces this.

The ledger is a locator, never proof. The translated block must still match exactly. A recorded overlap between the hint's region and an earlier surgery is also a warning signal: if the hint region was rewritten and the block turns up at a far unique location, the label adds "hint region was modified by an earlier surgery; verify intent."

### 2.4 Resolution algorithm per op

1. Compute `C`: every exact match of the whole expected block (all lines) in the snapshot, no window. Overlapping candidates in a repeated-line block are distinct.
2. Test whether the stated line `S` is in `C`. If the ledger is valid with at least one step, compute the translated line `T` and test it.
3. Decide:

| `|C|` | State | Result |
|---|---|---|
| 0 | any | Abort with diagnostics (2.7). |
| 1 | any | Use it. Label `exact` if it equals `S`, `shift-tracked` if it equals `T`, otherwise `searched`. Normal confirmation only. |
| >1 | `NoView`, or `Valid` with zero steps, and `S` in `C` | Use `S`, no extra prompt. This preserves today's behavior: nothing indicates drift. |
| >1 | anything else (ledger has steps, `Broken`, or `S` not in `C`) | **Choose-required.** |

Choose-required preselects: the candidate equal to `T` if the ledger is valid with steps and `T` is in `C`; otherwise the candidate equal to `S` if present; otherwise nothing. If both `S` and `T` are in `C` and differ, `T` is preselected and `S` is shown labeled as the stated line.

4. Joint validation of all resolved ops (2.2), then the normal confirmation. Relocated ops are labeled in the diff (`relocated: hint L120 -> L134 (+14), shift-tracked`, `searched`, or `chosen by user: L134 of {88,134,301}`). Reviewed bytes remain exactly the committed bytes.

Insert ops: the before and after anchors are one adjacent unit; search for the concatenated block. If the pair no longer adjoins anywhere, abort.

Same-envelope duplicates: two ops with identical expected blocks where either needs relocation abort in v1.

Promotion replay: relocation is disabled when the surgery applier is used mechanically (overlay promotion replaying net per-file changes). Replay is exact or fails.

### 2.5 Choose-required UX

Compared with the current review (dry-run diff, Space applies, `q` or Ctrl+C aborts): the chooser is a separate screen before the diff, shown only when choose-required applies. It lists up to 5 candidates, each with line numbers and two lines of context, sorted by distance from the hint, with the preselected one marked. The user selects by pressing the candidate's number or aborts with `q`. Space and Enter do not select, so a reflexive keypress cannot accept a location. After all ops are resolved, the plan is revalidated and the normal diff review follows, where Space applies.

- Cost: one extra deliberate keypress per ambiguous op, and only when the file may have drifted and the block is repeated. Unique matches and unedited files are unaffected.
- Benefit: location is a human decision, and the diff review then judges content. The ledger accelerates by preselecting but cannot commit alone.
- Rejected alternative: one screen where Space applies the preselected candidate. It fails the case where the reviewer sees the intended replacement and overlooks other identical occurrences.
- More than 5 candidates: abort with "anchor too generic: N exact matches; quote more context."
- `--dry-run` or any non-interactive mode: print the candidates, apply nothing, exit non-zero.

### 2.6 Commit protocol and concurrent writers

Goal invariant: CVB does not overwrite bytes it did not hash immediately before the replacement.

1. Take an advisory exclusive lock on the target (or a sidecar lock in the state directory). This covers other CVB sessions and candidate sessions. It does not stop editors.
2. Read the file's bytes and verify their hash equals the planning hash, otherwise abort with "file changed during review." Capture inode, size and mtime.
3. Build the output and write it to a temp file in the same directory (same filesystem), with permissions and ownership matched as far as possible. Flush to disk.
4. Immediately before the rename: re-stat the target (inode, size, mtime must equal step 2) and re-hash it. Any difference aborts and removes the temp file.
5. Rename the temp over the target and sync the directory.
6. Record the ledger step with `post_hash` (the hash of the bytes written).

Residual risk: a non-cooperating writer changing the file between step 4 and step 5. That window is on the order of microseconds and cannot be eliminated on POSIX without mandatory locking. It is narrowed to that window and documented. An optional detect-and-recover measure is to hard-link the original to a backup name before the rename and re-hash that inode afterwards. If it changed, an in-place write landed on the old inode and would be lost, so report it and keep the backup. This does not catch a writer that replaces the file by rename, which step 4's inode check covers. The existing journal may already provide a backup. For multi-file envelopes, the existing cross-file atomicity mechanism governs, and the per-file checks above apply to each file. Verify against the source (section 5).

### 2.7 Diagnostics and reporting

On abort: the original hint; the expected block, displayed safely (same escaping rules); the actual text at the hint plus or minus 2 lines; any exact candidates elsewhere (relevant when relocation was declined, for example under `--dry-run`); how each location was derived (translated or searched); and, if the ledger recorded that an earlier surgery rewrote the requested text, which one.

After a successful apply, print a piecewise line map for each file that moved, in the output the LLM sees:

```
src/pull.rs: line map, original -> current (now 812 lines)
  1-119      -> 1-119    (same)
  120-299    -> 134-313  (+14)
  300-302    -> 314      (3 lines replaced by 1)
  303-end    -> 315-end  (+12)
```

## 3. Sequencing

1. Diff contract, grammar and tests. Rendering only.
2. Commit-time verification (2.6). Independent of relocation, since the review delay exists today. Needs the source audit first.
3. Mismatch diagnostics with a read-only whole-file candidate listing in rejection messages. No behavior change. The search code built here is reused.
4. Resolver as a pure function with the ledger absent: whole-file search, the decision table (`Valid` with steps unreachable at this point), chooser, joint validation.
5. Edit ledger with hash chain (tier 2). It affects outcomes only for repeated blocks and labeling.
6. Post-apply line map.
7. End-to-end tests.

Search comes before the ledger because it is stateless and fixes the reported failure whenever the quoted block is unique. The ledger carries state that must be invalidated correctly and mainly serves repeated blocks.

## 4. Test matrix

### 4.1 Diff contract

- Changed lines are exactly marker plus source bytes.
- Property: for random source lines (leading spaces 0, 1, 4, 8; tabs; trailing whitespace; empty; non-ASCII), a parser that strips column 1 and honors the hunk counts reproduces the source. Lines shown escaped are the only exception and appear in notes.
- Empty added line renders as `+` alone; empty context line as a single space.
- Adversarial source lines, each read back by the count-based parser as body: `-- foo` (removed), `=== notes ===`, `+++ b/x`, `--- a/x`, `@@ -1 +1 @@`, `\ No newline at end of file`, a line starting with `\u{200b}` as literal text.
- Escaped lines: real zero-width character versus the literal text `\u{200b}` are distinguishable by notes and doubled backslash.
- Nothing in the output but headers, gutter lines, `\ No newline` markers and the notes section.
- Notes only when relevant, after `=== notes ===`, naming op, line and side. Legend present. CRLF noted once per file. Missing final newline marked. A path containing a control character is rejected.
- Renderer-parser round trip on every golden sample.

### 4.2 Resolution

| Case | Expected |
|---|---|
| Block at stated line, unique | `exact` |
| Stated line matches, repeated elsewhere, `NoView` or fresh ledger | stated line used |
| Same, ledger has steps | chooser required |
| Prior surgery inserted N lines above, block unique | `shift-tracked` or `searched` (same location) |
| Prior surgery removed lines above | negative shift works |
| LLM already corrected its numbers, block unique | `exact`, no double shift |
| Block repeated, stated and translated both match, differ | chooser, translated preselected, stated shown |
| Block repeated, ledger valid, only translated matches | chooser, translated preselected |
| Block repeated, ledger `Broken`, stated matches | chooser, stated preselected |
| Prior edit overlaps the block | tier 2 skipped; search only |
| Hint far from a unique block | `searched`, no distance limit |
| Hint region rewritten by earlier surgery, block found far away | label warns to verify intent |
| 2 to 5 candidates | chooser; Space and Enter do not select; number selects; `q` aborts; nothing written before choice |
| More than 5 candidates | abort, "too generic" |
| Block absent | abort, diagnostics show actual text near the hint |
| Overlapping candidates in a repeated-line block | each listed |
| Insert with multi-line anchors, still adjacent | relocated as a unit |
| Insert anchors no longer adjoin | rejected |
| Two ops with overlapping footprints | rejected |
| Op edit range intersects another insert's anchor | rejected |
| Two inserts at the same boundary | rejected, both ops named |
| Mixed envelope, one exact and one relocated | applied together or not at all |
| Identical expected blocks in two ops, one stale | abort (v1) |
| `--dry-run` with ambiguity | candidates printed, non-zero exit, nothing written |
| Promotion replay | relocation disabled; mismatch fails |
| Cancelled surgery | no ledger step |
| Pull after surgery | ledger resets to the new `H_view` |

### 4.3 Ledger and commit

- Hash matches after CVB's own write; chain stays valid across two successive surgeries.
- External edit between surgeries (including an identical-bytes restore): mismatch frees steps and moves to `Broken`; identical bytes restore validity only if the hash equals the chain end.
- `Broken` file: lookup returns nothing; surgery appends nothing; a new pull recovers.
- Undo or journal rollback: breaks the chain.
- Whole-file hash covers CRLF versus LF and final-newline changes.
- File modified between planning and commit: abort with "file changed during review," nothing written.
- File modified between the commit-time check and the rename (simulated with a hook): detected by the pre-rename re-check when it happens before step 4; the documented residual window is covered by the optional backup, if adopted.
- Same-envelope: all hints refer to the same snapshot; the ledger records one step per envelope with edits in snapshot coordinates.
- Line endings, final newline and size caps (3 MiB per envelope, 8 MiB per file, 32 MiB aggregate) unchanged.

## 5. Architecture and items to verify in the source

Advice from first principles:
- Resolution is a pure function over (snapshot lines, parsed ops with hints, optional ledger view). Its output is resolved ranges with labels, or an ambiguity set, or a mismatch with diagnostics. It is testable without files or a clipboard.
- It belongs in planning, before the diff is built. The diff, the confirmation and the commit consume the same resolved plan, so "reviewed bytes equal committed bytes" cannot be broken by a second resolution.
- The applier receives only resolved ranges and knows nothing about hints.
- The hash chain means the ledger does not need to survive a process restart; in-memory is enough.

Verify before coding:
1. Whether every op's line hints in an envelope refer to one snapshot, and how the existing protocol and validator define ranges and anchors (2.2).
2. How the applier writes files today: locking, temp file and rename, permission handling, fsync, and whether a re-check of the file just before the write already exists (2.6).
3. How multi-file envelopes keep all-or-nothing atomicity, and the journal's backup behavior.
4. Whether the diff renderer is shared between the screen pager and the clipboard output (escaping must be consistent).
5. How insert anchors are represented, and whether ops must be in file order.
6. How the journal and undo interact with committed ledger steps.

## 6. Open questions

1. Escape control and invisible characters in the diff body (recommended, with the doubled-backslash rule), versus keeping the body strictly byte-exact and relying on notes. The trade-off is terminal safety against strict fidelity.
2. Candidate display limit (5) and the "too generic" abort threshold.
3. Whether to add a note for added lines whose indentation is not a multiple of the file's indent unit. It would catch the original bug class directly but is heuristic and could be noisy.
4. Whether the view epoch is "last pull" (recommended, simpler) or "last time any content of this file reached the LLM," including surgery diffs.
5. The `NoView` / fresh-ledger row of the decision table keeps today's behavior of silently using the stated line when it matches among several. Decide whether that is acceptable or should also require choosing.
6. Whether to adopt the optional hard-link backup for the commit protocol, or rely on the existing journal.
7. A defined order for two inserts at the same boundary (v1 rejects them).

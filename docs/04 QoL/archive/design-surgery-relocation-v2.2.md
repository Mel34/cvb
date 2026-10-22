# CVB surgery: diff format and anchor relocation, design v2.2

Status: design revision, no implementation. Supersedes v2.1, v2 and v1 (`claude/design-surgery-relocation*.md`). This document is complete and self-contained: a reviewer who has not seen earlier versions can use it alone.

Limit: I have not read the CVB source. Anything that depends on how the code is built is listed in section 5 as something to verify before coding, not assumed.

## 0. What changed since v2.1

| Topic | v2.2 |
|---|---|
| Automatic choice among duplicates | Not allowed unless the hint is positively known to be current. View freshness is unknown in v1, so every multi-candidate case uses the chooser. A unique exact match stays automatic. A fresh or zero-step ledger no longer counts as evidence. |
| Diff grammar | Hunk counts may be omitted and then mean 1. Zero counts defined. Single-line hunks tested. |
| Reconstruction contract | New section 1.5: exactly how source lines are recovered from the rendering, including escapes and line endings. Terminal-safety escaping is kept. |
| Ledger invalidation | `Broken` is entered only on an observed mismatch and is revived only by a new pull. Change-and-restore between observations is invisible and stays valid. Stated explicitly. |
| Commit invariant | Reworded to the achievable guarantee. The race with non-cooperating writers is narrowed, not closed. The hard-link backup is deferred until the source audit. |
| Open questions | Resolved items moved to a decided list. New open items: the optional view tag and an envelope escape syntax. |

## 1. Diff format

### 1.1 Findings

- The reported bug: the diff printed marker, separator space, then the source line, so changed lines carried one extra leading space.
- Commentary interleaved in the diff body is read by an LLM through the same first-column rule as source. An indented diagnostic line looks like a context line. Commentary does not belong in the body.
- Per-line "leading spaces" notes on every Rust line are noise. Report anomalies only.

### 1.2 Format

1. Body is standard unified diff. Column 1 is the gutter: `' '` context, `-` removed, `+` added. Source text starts at column 2 with no separator.
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
Legend: column 1 is the gutter (' ' context, '-' removed, '+' added). Source text starts at column 2. Lines named in the notes as "shown escaped" are transformed (see notes). Text after "=== notes ===" is commentary, never source.
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
output     := banner LF legend LF file+ notes?
file       := "--- " path LF "+++ " path LF hunk+
hunk       := "@@ -" range " +" range " @@" label? LF body
range      := start ("," count)?          ; omitted count means 1
body       := ( gutterline | nonewline )*
gutterline := (" " | "-" | "+") text LF
nonewline  := "\" " No newline at end of file" LF   ; follows the line it qualifies; not counted
notes      := LF "=== notes ===" LF note*
```

Ranges:
- An omitted count means 1. `@@ -5 +5 @@` is a valid single-line hunk and equals `@@ -5,1 +5,1 @@`.
- A count of 0 is allowed; when it is 0, `start` is the line before the empty range, as in standard unified diff.
- The parser accepts both spellings. The renderer's canonical form omits `,1`, as GNU diff does.

Counting rule: a consumer reads gutter lines until `oldcount` (lines with `' '` or `-`) and `newcount` (lines with `' '` or `+`) are both exhausted, treating a following `nonewline` line as part of the previous line. Nothing in the body is interpreted by its content, only by its position. After the counts are exhausted, the next line must be a `@@` line, a `--- ` line (next file), a blank line followed by the notes header, or end of output.

Why content cannot confuse it:
- Every body line has a gutter character in column 1. A removed source line `-- foo` renders as `--- foo` but is read as body, because the counts say it is.
- A source line `=== notes ===` renders as `+=== notes ===`, never as the notes header, which begins with `=` in column 1.
- A source line `+++ b/x` renders as `++++ b/x`, and a source line `@@ -1 +1 @@` as `+@@ -1 +1 @@`, both read as body by count.
- A source line equal to `\ No newline at end of file` renders as `+\ No newline at end of file`. The real marker has `\` in column 1.

Paths containing a newline or other control character are rejected in v1, so header lines stay one line each.

### 1.4 Answers to the questions

1. Exact format: 1.2 and 1.3. The payload is the text after column 1, subject to 1.5.
2. Diagnostics are conditional and only for anomalies.
3. Tabs and trailing whitespace: raw in the body, flagged in notes. CRLF, control and invisible characters, final newline: see 1.5.
4. Familiar structure and reliable extraction both hold.
5. Tests: 4.1.

### 1.5 Reconstruction contract

The body is byte-exact except where this section says otherwise. The contract has four parts.

**Source line.** A source line is the bytes between line terminators, excluding the terminator. In a CRLF file the `\r` of a CRLF terminator is part of the terminator, not the line. Mixed line endings in one file remain rejected. The file's ending (LF or CRLF) is stated once per file in the notes when it is CRLF, and is applied by CVB when it writes payload lines, so payloads never contain terminators. A last line without a terminator is marked by `\ No newline at end of file`.

**Transformed lines.** A line is transformed when it contains a character in the escape class. The starting definition of the class is: C0 and C1 control characters other than TAB (including NUL, DEL and a lone `\r` not part of a CRLF terminator), Unicode format characters (category Cf: zero-width characters, bidirectional controls, soft hyphen, byte-order mark), line and paragraph separators (U+2028, U+2029), and space separators other than the plain space (category Zs, such as no-break space). The exact list is data in the implementation and is locked by golden tests.

Transformation rule: on a transformed line, each character in the class is rendered `\u{h}` (lowercase hex, no leading zeros, `0` for NUL), and every literal backslash is rendered `\\`. A line that is not transformed is raw, including any backslashes it contains.

**Naming.** Every transformed line is named in the notes (`op 2 L140 +: shown escaped`, or `file src/x.rs L140 context: shown escaped`). Lines not named are raw. This is the only way a consumer learns that unescaping applies, so a source line that literally contains the text `\u{200b}` and is not named is read as those literal bytes, while a named line containing a real zero-width space is read as that character.

**Reconstruction.** For a body line `B` (text after column 1):
- not named in the notes: source text = `B` exactly;
- named: source text = unescape(`B`), where `\\` becomes `\` and `\u{h}` becomes the scalar `h`. Any other backslash sequence on a named line is a renderer bug and a parser error.

The terminator is never part of `B`. A line's terminator and the file's final-newline state come from the file note and the marker.

Scope: the contract describes the rendering. Envelope payloads and expected text are raw bytes in v1, with no escape syntax. An expected block that contains a transformed line cannot be quoted faithfully by an LLM. In that case the mismatch diagnostic says the text contains characters shown escaped, and the LLM should anchor on neighboring lines. Whether to add an envelope escape syntax is open question 5. Any other LLM-facing rendering of source (pull output) follows the same escaping and naming rule, and what pull does today is a verification item (section 5).

The design assumes UTF-8 sources. If CVB accepts non-UTF-8 files today, invalid bytes need `\x{XX}` escaping under the same naming rule; verify the current behavior.

## 2. Relocation

### 2.1 Principle

Hints locate, exact content verifies. Every tier is a source of candidate locations, and a location is acceptable only if the whole expected block matches there byte for byte. Exact bytes establish that a block is eligible. They do not establish which identical occurrence was intended. So automatic selection among several exact matches requires a defensible indication that the hint is current, and without one a human chooses.

### 2.2 Coordinate systems and the same-envelope contract

- **V (view):** the file version the LLM was last shown, and the numbers it quotes. CVB cannot observe this. The initial baseline is the last pull of the file in this session, recorded by hash. Limitations, documented as such: the LLM may quote an older pull still in its context; the file may have changed between pulls; hints may come from the LLM's memory of earlier output; pulls in other sessions are unknown; a pull of a range does not show the rest of the file.
- **S (snapshot):** the current file at planning time.
- **R (result):** the file after a successful apply.

Invariant to document, test and verify against the existing protocol: every op in one envelope carries hints in the same coordinate system, and all ops are resolved against the same snapshot. There is no sequential adjustment between ops of one envelope. The resolved ops are applied as one simultaneous edit set.

Footprint of an op: replaced or deleted lines, plus, for an insert, its before and after anchor lines (which must remain unmodified in the same envelope). Joint validation rejects:
- overlapping footprints;
- an op whose edit range intersects another op's anchor lines (this also covers an insert adjacent to a replaced range);
- two inserts at the same boundary (identical or overlapping anchor pairs). CVB defines no order for them in v1; the envelope is rejected with a message naming both ops.

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
- **Observations.** CVB compares the file's current hash with the hash the ledger expects (`H_view` with zero steps, otherwise the last step's `post_hash`) only at observation points: a pull, planning, and the commit check. It does not watch the file between them.
- **Broken.** The first observation that finds a mismatch moves the file to `Broken` and frees the steps immediately. A later restoration of the old bytes does not revive the ledger: `Broken` persists, and only a new pull of that file, which records a new `H_view` and zero steps, leaves it. Surgeries on a `Broken` file append nothing.
- **Change and restore between observations.** If an external change and its exact reversal both happen between two observation points, CVB never sees the mismatch, the hash matches, and the ledger stays `Valid`. That is correct, because identical bytes mean identical coordinates. It is a property of observation, not a gap in the model.
- CVB does not classify who made a change. A surgery's own write yields a hash the ledger expects. Anything else (editor, `git checkout`, rebase, undo, journal rollback) mismatches when observed.
- All lookups go through one function that returns nothing unless the state is `Valid` and the observed hash equals the chain end, so a stale mapping cannot be reached.
- Mapping a line `n` through one step: for each edit with `n >= start + old_len`, add `new_len - old_len`. If `start <= n < start + old_len`, the line is inside an edited region and is unmappable. Steps compose in order.
- Mapping a block `[a, b]`: map both ends. The block is unmappable if an edit region overlaps it or an insertion falls strictly inside it (`a < start <= b`). An insertion at `start == a` shifts the block. One at `start == b + 1` does not affect it.
- A same-length replacement maps lines identically but changes their content. A later anchor must quote the new text, and the exact check enforces this.

The ledger is a locator, never proof. The translated block must still match exactly. With v2.2's selection rule (2.4) the ledger never selects among duplicates by itself. Its remaining roles are labeling (`shift-tracked`), preselecting in the chooser, and warning: if the hint's region was rewritten by an earlier surgery and the block turns up at a far unique location, the label adds "hint region was modified by an earlier surgery; verify intent."

### 2.4 Resolution algorithm per op

1. Compute `C`: every exact match of the whole expected block (all lines) in the snapshot, no window. Overlapping candidates in a repeated-line block are distinct.
2. Test whether the stated line `S` is in `C`. If the ledger is `Valid` with at least one step, compute the translated line `T` and test it.
3. Decide:

| `|C|` | Hint known to be current? | Result |
|---|---|---|
| 0 | n/a | Abort with diagnostics (2.7). |
| 1 | n/a | Use it. Label `exact` if it equals `S`, `shift-tracked` if it equals `T`, otherwise `searched`. Normal confirmation only. |
| >1 | yes, and `S` in `C` | Use `S`, no extra prompt. |
| >1 | no or unknown | **Choose-required.** |

"Known to be current" requires positive evidence that the hint was produced from the exact file bytes now on disk. CVB cannot observe what the LLM saw. A fresh ledger, a ledger with zero steps, or a stated line that happens to match is not such evidence. The only mechanism proposed for obtaining it is an optional view tag carried in the envelope (open question 4). Without it, hint currency is unknown in v1, the middle row is never taken, and every multi-candidate case is choose-required.

Choose-required preselects: the candidate equal to `T` if the ledger is valid with steps and `T` is in `C`; otherwise the candidate equal to `S` if present; otherwise nothing. If both `S` and `T` are in `C` and differ, `T` is preselected and `S` is shown labeled as the stated line. Preselection is a convenience for the chooser and is never an automatic decision.

4. Joint validation of all resolved ops (2.2), then the normal confirmation. Relocated ops are labeled in the diff (`relocated: hint L120 -> L134 (+14), shift-tracked`, `searched`, or `chosen by user: L134 of {88,134,301}`). Reviewed bytes remain exactly the committed bytes.

Insert ops: the before and after anchors are one adjacent unit; search for the concatenated block. If the pair no longer adjoins anywhere, abort.

Same-envelope duplicates: two ops with identical expected blocks where either needs relocation abort in v1.

Promotion replay: relocation is disabled when the surgery applier is used mechanically (overlay promotion replaying net per-file changes). Replay is exact or fails.

### 2.5 Choose-required UX

Compared with the current review (dry-run diff, Space applies, `q` or Ctrl+C aborts): the chooser is a separate screen before the diff, shown only when choose-required applies. It lists up to 5 candidates, each with line numbers and two lines of context, sorted by distance from the hint, with the preselected one marked. The user selects by pressing the candidate's number or aborts with `q`. Space and Enter do not select, so a reflexive keypress cannot accept a location. After all ops are resolved, the plan is revalidated and the normal diff review follows, where Space applies.

- Cost: one extra deliberate keypress per ambiguous op. Because v1 has no freshness evidence, this applies to every op whose expected block appears more than once in the file, including a repeated block that matches at the stated line. Unique blocks are unaffected, and multi-line anchors rarely repeat. The preselected candidate makes the common case a single keypress.
- Benefit: location is a human decision, and the diff review then judges content.
- Rejected alternative: one screen where Space applies the preselected candidate. It fails the case where the reviewer sees the intended replacement and overlooks other identical occurrences.
- More than 5 candidates: abort with "anchor too generic: N exact matches; quote more context."
- `--dry-run` or any non-interactive mode: print the candidates, apply nothing, exit non-zero.

Suggested measurement once implemented: count how often the chooser appears and how often the preselected candidate is the one chosen. That data decides whether an optional view tag is worth adding.

### 2.6 Commit protocol and concurrent writers

Guarantee, stated as achievable:
- CVB never writes over a version of the file it has not hashed and compared with its plan. If the file differs from the planned bytes at any check, CVB aborts and writes nothing.
- Writers that cooperate (other CVB sessions, including candidate sessions, which take the same lock) are fully excluded for the duration of the commit.
- A writer that does not cooperate, such as an editor, can still change the file after CVB's final re-check and before the rename completes. That window is small but nonzero and cannot be closed on POSIX without mandatory locking. In that case the other writer's change is lost. CVB narrows the window and does not claim to remove it.

Steps:
1. Take an advisory exclusive lock on the target (or a sidecar lock in the state directory).
2. Read the file's bytes and verify their hash equals the planning hash, otherwise abort with "file changed during review." Capture inode, size and mtime.
3. Build the output and write it to a temp file in the same directory (same filesystem), with permissions and ownership matched as far as possible. Flush to disk.
4. Immediately before the rename: re-stat the target (inode, size, mtime must equal step 2) and re-hash it. Any difference aborts and removes the temp file.
5. Rename the temp over the target and sync the directory.
6. Record the ledger step with `post_hash` (the hash of the bytes written).

Deferred: an extra detect-and-recover measure (hard-linking the original to a backup name before the rename and re-hashing it afterwards) is not part of this design. Whether it is needed depends on what the existing journal and write path already provide, so the decision waits for the source audit (section 5). For multi-file envelopes, the existing cross-file atomicity mechanism governs, and the per-file checks above apply to each file.

The limitation is documented in the CVB manual in plain language.

### 2.7 Diagnostics and reporting

On abort: the original hint; the expected block, displayed safely (same escaping and naming rules); the actual text at the hint plus or minus 2 lines; any exact candidates elsewhere (relevant when relocation was declined, for example under `--dry-run`); how each location was derived (translated or searched); a remark when the expected text contains characters shown escaped; and, if the ledger recorded that an earlier surgery rewrote the requested text, which one.

After a successful apply, print a piecewise line map for each file that moved, in the output the LLM sees:

```
src/pull.rs: line map, original -> current (now 812 lines)
  1-119      -> 1-119    (same)
  120-299    -> 134-313  (+14)
  300-302    -> 314      (3 lines replaced by 1)
  303-end    -> 315-end  (+12)
```

## 3. Sequencing

1. Diff contract, grammar, reconstruction contract and tests. Rendering only.
2. Commit-time verification (2.6). Independent of relocation, since the review delay exists today. Needs the source audit first.
3. Mismatch diagnostics with a read-only whole-file candidate listing in rejection messages. No behavior change. The search code built here is reused.
4. Resolver as a pure function, ledger absent: whole-file search, the decision table, chooser, joint validation.
5. Post-apply line map. It derives from the applied edit set and does not depend on the ledger.
6. Edit ledger with hash chain. With v2.2's rule it never makes an automatic choice, so its value is preselection, labels and warnings. It is worth building after the chooser's real frequency and preselection accuracy are known.
7. End-to-end tests.

Search comes before the ledger because it is stateless and fixes the reported failure whenever the quoted block is unique.

## 4. Test matrix

### 4.1 Diff contract

- Changed lines are exactly marker plus source text for non-transformed lines.
- Hunk headers: `@@ -5 +5 @@` (both counts omitted) parses as one-line hunks; `@@ -5,1 +5,1 @@` is accepted; the renderer emits the canonical form without `,1`; counts of 0 parse with `start` as the line before; a single-line hunk renders and round-trips.
- Property: for random source lines (leading spaces 0, 1, 4, 8; tabs; trailing whitespace; empty; non-ASCII), a parser that strips column 1 and honors the hunk counts reproduces the source for non-transformed lines.
- Empty added line renders as `+` alone; empty context line as a single space.
- Adversarial source lines, each read back by the count-based parser as body: `-- foo` (removed), `=== notes ===`, `+++ b/x`, `--- a/x`, `@@ -1 +1 @@`, `\ No newline at end of file`.
- Escape contract: a real zero-width space renders as `\u{200b}` and is named in the notes. A source line containing the literal text `\u{200b}` is rendered with the backslash doubled only if the line is transformed for another reason, and otherwise raw and unnamed. Reconstruction recovers the exact source text in both cases. A raw line containing `\\` (two backslashes) stays literal. Coverage of the class: NUL, DEL, a C1 control, a lone CR, no-break space, ZWJ, BOM, U+2028. Any other backslash sequence on a named line is a parser error.
- CRLF file: terminators are not in line text; the notes state CRLF once; payload reconstruction applies CRLF; mixed endings rejected. Last line without a terminator: marker present, round-trips.
- Nothing in the output but headers, gutter lines, `\ No newline` markers and the notes section.
- Notes only when relevant, after `=== notes ===`, naming op, line and side. Legend present. A path containing a control character is rejected.
- Renderer-parser round trip on every golden sample.

### 4.2 Resolution

| Case | Expected |
|---|---|
| Block at stated line, unique | `exact` |
| Block repeated; stated line matches; any ledger state (v1, no freshness evidence) | chooser required, stated line preselected |
| Block repeated; stated line does not match | chooser required, no preselection unless `T` matches |
| Prior surgery inserted N lines above, block unique | `shift-tracked` or `searched` (same location) |
| Prior surgery removed lines above | negative shift works |
| LLM already corrected its numbers, block unique | `exact`, no double shift |
| Block repeated; stated and translated both match, differ | chooser, translated preselected, stated shown |
| Block repeated; ledger valid, only translated matches | chooser, translated preselected |
| Block repeated; ledger `Broken` | chooser; stated line preselected if it matches |
| Prior edit overlaps the block | translation skipped; search only |
| Hint far from a unique block | `searched`, no distance limit |
| Hint region rewritten by earlier surgery, block found far away | label warns to verify intent |
| 2 to 5 candidates | chooser; Space and Enter do not select; number selects; `q` aborts; nothing written before choice |
| More than 5 candidates | abort, "too generic" |
| Block absent | abort, diagnostics show actual text near the hint |
| Expected text contains a transformed character | diagnostic says so |
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
| Pull after surgery | ledger resets to the new `H_view` with zero steps |
| If a view tag is adopted later: tag matches current hash and stated line is a candidate | stated line used without the chooser; a non-matching or absent tag uses the chooser |

### 4.3 Ledger and commit

- Hash matches after CVB's own write; the chain stays valid across two successive surgeries.
- External edit observed at planning: mismatch moves the file to `Broken` and frees the steps.
- Break then restore: the ledger stays `Broken` after the bytes are restored; only a new pull revives it.
- Change and exact restore between two observation points: the hash matches and the ledger stays `Valid`.
- `Broken` file: lookup returns nothing; surgery appends nothing.
- Undo or journal rollback: breaks the chain when observed.
- Whole-file hash covers CRLF versus LF and final-newline changes.
- File modified between planning and commit: abort with "file changed during review," nothing written.
- File modified between the step 2 check and the step 4 re-check (simulated with a hook): detected, abort, temp file removed, nothing written.
- File modified after the step 4 re-check and before the rename: not asserted as prevented. The test documents the residual window and its expected consequence.
- Cooperating writer (a second CVB session holding the lock): excluded.
- Same-envelope: all hints refer to the same snapshot; the ledger records one step per envelope with edits in snapshot coordinates.
- Line endings, final newline and size caps (3 MiB per envelope, 8 MiB per file, 32 MiB aggregate) unchanged.

## 5. Architecture and items to verify in the source

Advice from first principles:
- Resolution is a pure function over (snapshot lines, parsed ops with hints, optional ledger view). Its output is resolved ranges with labels, or an ambiguity set, or a mismatch with diagnostics. It is testable without files or a clipboard.
- It belongs in planning, before the diff is built. The diff, the confirmation and the commit consume the same resolved plan, so "reviewed bytes equal committed bytes" cannot be broken by a second resolution.
- The applier receives only resolved ranges and knows nothing about hints.
- The hash chain means the ledger does not need to survive a process restart; in-memory is enough.

Verify before coding:
1. Envelope coordinate semantics: whether every op's line hints in an envelope refer to one snapshot, and how the existing protocol and validator define ranges and anchors (2.2).
2. Applier behavior: how files are written today (locking, temp file and rename, permissions, fsync) and whether a re-check of the file just before the write already exists (2.6).
3. Journal behavior: backups it keeps, how multi-file envelopes keep all-or-nothing atomicity, and how the journal and undo interact with committed ledger steps.
4. Renderer sharing: whether the diff renderer is shared between the screen pager and the clipboard output, so escaping and naming are consistent. What pull output does today with line numbers, gutters and unusual characters.
5. Insert anchors: how they are represented, and whether ops must be in file order.
6. Text handling: whether non-UTF-8 files are accepted; how envelope payloads treat backslashes and unusual characters today.
7. Undo interactions: what undo or rollback does to file bytes, and so to the ledger's observed hashes.

## 6. Decisions and open questions

Decided:
- Escape control and invisible characters in the diff body, under the reconstruction contract in 1.5 (terminal safety is kept).
- Candidate display limit is 5. More than 5 aborts with "too generic."
- The indentation-heuristic note is deferred.
- The initial view baseline is the last pull, with the limitations in 2.2 documented.
- Same-boundary inserts are rejected in v1.
- A unique exact match is automatic. Several exact matches are automatic only with a defensible indication that the hint is current; where freshness is unknown, the chooser is required.
- Commit invariant is the achievable guarantee in 2.6; the hard-link backup is deferred until the source audit.

Open:
1. Source-dependent verification items in section 5, to be completed before any implementation.
2. Whether and when to build the edit ledger (step 6 of the sequencing), given that it no longer makes automatic choices.
3. Hard-link backup in the commit protocol: decide after the audit shows what the journal and write path already provide.
4. An optional view tag: pull output prints a short hash of the viewed bytes per file, and the envelope may echo it. A tag equal to the file's current hash would be positive evidence that the hint is current and would allow automatic use of the stated line among duplicates. It would reverse the earlier decision to drop revision tags from v1, and the LLM must copy it correctly, so it is left as a question. The chooser frequency measured in 2.5 should inform it.
5. An escape syntax for envelope text, so an LLM can quote or produce lines containing transformed characters. v1 is raw bytes with a diagnostic when a block cannot be quoted.
6. A defined order for two inserts at the same boundary (rejected in v1).

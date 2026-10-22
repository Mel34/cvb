# CVB surgery: diff format and anchor relocation, design v2.3

Status: design revision, no implementation. Supersedes v2.2, v2.1, v2 and v1 (`claude/design-surgery-relocation*.md`). This document is complete and self-contained: a reviewer who has not seen earlier versions can use it alone. It is intended to be the last design-only revision unless review finds a genuine contradiction; the handoff is regenerated from it.

Limit: I have not read the CVB source. Anything that depends on how the code is built is listed in section 5 as something to verify before coding, not assumed.

## 0. What changed since v2.2

| Topic | v2.3 |
|---|---|
| Commit lock | The lock must have a stable identity across atomic replacement (a lock on the target's own inode is not enough). Recommended: lock the parent directory. A sidecar lock file is the alternative. Lifecycle and contention rules stated. |
| Multi-file commit | Reconciled: all hash checks happen before the first rename (two-phase commit). A failure after renames start triggers rollback from retained originals, and that mechanism must be proven by failure injection. |
| View tag | Defined precisely (full-file hash, even for a range pull) and kept deferred until chooser frequency is measured. |
| Mapping tests | Boundary and interaction cases added, plus a property test. Final-newline semantics for deleting the last line of an unterminated file flagged for definition. |
| Diff parser | The unescape parse order is specified. Misplaced no-newline markers are parser errors. |
| Limitation | Two ops with identical expected blocks, where either needs relocation, remain rejected in v1 as a documented, deliberate limitation, with a test. |
| Sequencing | A read-only source audit is milestone 0. |

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
nonewline  := "\" " No newline at end of file" LF   ; qualifies the line before it; not counted
notes      := LF "=== notes ===" LF note*
```

Ranges:
- An omitted count means 1. `@@ -5 +5 @@` is a valid single-line hunk and equals `@@ -5,1 +5,1 @@`.
- A count of 0 is allowed; when it is 0, `start` is the line before the empty range, as in standard unified diff.
- The parser accepts both spellings. The renderer's canonical form omits `,1`, as GNU diff does.

Counting rule: a consumer reads gutter lines until `oldcount` (lines with `' '` or `-`) and `newcount` (lines with `' '` or `+`) are both exhausted, treating a following `nonewline` line as qualifying the previous line. Nothing in the body is interpreted by its content, only by its position. After the counts are exhausted, the next line must be a `@@` line, a `--- ` line (next file), a blank line followed by the notes header, or end of output.

No-newline marker validity: a marker is valid only immediately after a gutter line of the same hunk, and only when that line is the final line of the side or sides it qualifies, as determined by the counts: after a `-` line when it exhausts `oldcount`, after a `+` line when it exhausts `newcount`, after a context line when it exhausts both. At most one marker follows a given line. A marker at the start of a body, after another marker, or after a line that is not final on its side is a parser error and is never accepted merely because the grammar permits the token.

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

The body is byte-exact except where this section says otherwise.

**Source line.** A source line is the bytes between line terminators, excluding the terminator. In a CRLF file the `\r` of a CRLF terminator is part of the terminator, not the line. Mixed line endings in one file remain rejected. The file's ending (LF or CRLF) is stated once per file in the notes when it is CRLF, and is applied by CVB when it writes payload lines, so payloads never contain terminators. A last line without a terminator is marked by `\ No newline at end of file`.

**Transformed lines.** A line is transformed when it contains a character in the escape class. The starting definition of the class is: C0 and C1 control characters other than TAB (including NUL, DEL and a lone `\r` not part of a CRLF terminator), Unicode format characters (category Cf: zero-width characters, bidirectional controls, soft hyphen, byte-order mark), line and paragraph separators (U+2028, U+2029), and space separators other than the plain space (category Zs, such as no-break space). The exact list is data in the implementation and is locked by golden tests.

Transformation rule: on a transformed line, each character in the class is rendered `\u{h}` (lowercase hex, no leading zeros, `0` for NUL), and every literal backslash is rendered `\\`. A line that is not transformed is raw, including any backslashes it contains.

**Naming.** Every transformed line is named in the notes (`op 2 L140 +: shown escaped`, or `file src/x.rs L140 context: shown escaped`). Lines not named are raw. This is the only way a consumer learns that unescaping applies, so a source line that literally contains the text `\u{200b}` and is not named is read as those literal bytes, while a named line containing a real zero-width space is read as that character.

**Reconstruction.** For a body line `B` (text after column 1):
- not named in the notes: source text = `B` exactly;
- named: source text = unescape(`B`), by a single left-to-right pass. At each backslash, the next character decides: `\` yields a literal backslash and both characters are consumed; `u` followed by `{`, one or more hex digits and `}` yields that scalar and the whole sequence is consumed; anything else is a parser error. Consumed characters are never rescanned. For example, the source text `\u{41}` plus a zero-width space renders as `\\u{41}\u{200b}`, and reconstruction consumes `\\` first, then reads `u{41}` as literal text, then the escape.

The terminator is never part of `B`. A line's terminator and the file's final-newline state come from the file note and the marker.

Scope: the contract describes the rendering. Envelope payloads and expected text are raw bytes in v1, with no escape syntax. An expected block that contains a transformed line cannot be quoted faithfully by an LLM. In that case the mismatch diagnostic says the text contains characters shown escaped, and the LLM should anchor on neighboring lines. Whether to add an envelope escape syntax is deferred (section 6). Any other LLM-facing rendering of source (pull output) follows the same escaping and naming rule, and what pull does today is a verification item (section 5).

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
- Line numbering is the same whether or not the last line has a terminator: an unterminated last line is still a line.
- In a multi-file envelope, each file gets its own step, and all steps are recorded only after every file has committed (2.6).

The ledger is a locator, never proof. The translated block must still match exactly. With the selection rule (2.4) the ledger never selects among duplicates by itself. Its remaining roles are labeling (`shift-tracked`), preselecting in the chooser, and warning: if the hint's region was rewritten by an earlier surgery and the block turns up at a far unique location, the label adds "hint region was modified by an earlier surgery; verify intent." Because a wrong preselection is not harmless when duplicates exist, the mapping rules are covered by the boundary and property tests in 4.3.

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

"Known to be current" requires positive evidence that the hint was produced from the exact file bytes now on disk. CVB cannot observe what the LLM saw. A fresh ledger, a ledger with zero steps, or a stated line that happens to match is not such evidence. The only mechanism proposed for obtaining it is the optional view tag (2.8), which is deferred. In v1 hint currency is unknown, the middle row is never taken, and every multi-candidate case is choose-required.

Choose-required preselects: the candidate equal to `T` if the ledger is valid with steps and `T` is in `C`; otherwise the candidate equal to `S` if present; otherwise nothing. If both `S` and `T` are in `C` and differ, `T` is preselected and `S` is shown labeled as the stated line. Preselection is a convenience for the chooser and is never an automatic decision.

4. Joint validation of all resolved ops (2.2), then the normal confirmation. Relocated ops are labeled in the diff (`relocated: hint L120 -> L134 (+14), shift-tracked`, `searched`, or `chosen by user: L134 of {88,134,301}`). Reviewed bytes remain exactly the committed bytes.

Insert ops: the before and after anchors are one adjacent unit; search for the concatenated block. If the pair no longer adjoins anywhere, abort.

Documented v1 limitation: two ops in one envelope with identical expected blocks, where either needs relocation, abort with an explicit error naming both ops, and nothing is written. This is a deliberate restriction. A legitimate envelope that edits two separate occurrences of the same block is rejected until a joint-assignment rule is designed.

Promotion replay: relocation is disabled when the surgery applier is used mechanically (overlay promotion replaying net per-file changes). Replay is exact or fails.

### 2.5 Choose-required UX

Compared with the current review (dry-run diff, Space applies, `q` or Ctrl+C aborts): the chooser is a separate screen before the diff, shown only when choose-required applies. It lists up to 5 candidates, each with line numbers and two lines of context, sorted by distance from the hint, with the preselected one marked. The user selects by pressing the candidate's number or aborts with `q`. Space and Enter do not select, so a reflexive keypress cannot accept a location. After all ops are resolved, the plan is revalidated and the normal diff review follows, where Space applies.

- Cost: one extra deliberate keypress per ambiguous op. Because v1 has no freshness evidence, this applies to every op whose expected block appears more than once in the file, including a repeated block that matches at the stated line. Unique blocks are unaffected, and multi-line anchors rarely repeat. The preselected candidate makes the common case a single keypress.
- Benefit: location is a human decision, and the diff review then judges content.
- Rejected alternative: one screen where Space applies the preselected candidate. It fails the case where the reviewer sees the intended replacement and overlooks other identical occurrences.
- More than 5 candidates: abort with "anchor too generic: N exact matches; quote more context."
- `--dry-run` or any non-interactive mode: print the candidates, apply nothing, exit non-zero.

Suggested measurement once implemented: count how often the chooser appears and how often the preselected candidate is the one chosen. That data decides whether the view tag (2.8) is worth adding.

### 2.6 Commit protocol

#### Guarantees, stated as achievable

- If any hash or stat check fails in the prepare or final-check phases below, CVB aborts and writes nothing, for every file in the envelope.
- If a failure occurs after the first rename, CVB attempts to restore every already-committed file from retained originals. If restoration succeeds, the result is as if nothing was written. If it fails, CVB stops and reports exactly which files are in which state and where the originals are kept. This rollback must be proven, not assumed (see tests and the source audit).
- Writers that cooperate (other CVB sessions, including candidate sessions) are excluded for the duration of the commit by the lock below.
- A writer that does not cooperate, such as an editor, can still change a file after the final re-check and before that file's rename. That window is small but nonzero, and in a multi-file commit it lasts until the file's own rename. It cannot be closed on POSIX without mandatory locking. In that case the other writer's change to that file is lost. CVB narrows the window and does not claim to remove it. The limitation is documented in the CVB manual in plain language.

#### Lock requirement

The lock must keep one stable identity across atomic replacement. A lock held on the target file's own inode does not do this: after a rename replaces the inode, a second session that opens the path locks the new inode while the first still holds the old one, and the two locks do not exclude each other.

Recommended mechanism: an advisory exclusive lock on the target's parent directory, taken through a read-only directory descriptor. The directory's identity does not change when its children are renamed, no lock file has to be created, ignored by version control, or cleaned up, and the lock is released when the process exits or crashes.

Alternative: a sidecar lock file at a consistently derived path. It works but adds a lock file to manage, and its path must not be derived from per-session state locations that candidate sessions override, or the candidate and main sessions would use different locks. A path derived from the target's canonical location, or a shared runtime directory, avoids that.

Rules that apply to either mechanism:
- Canonicalize the target path first, and verify how the applier treats a symlinked target (whether rename replaces the link or its destination) before relying on a lock identity.
- For a multi-file envelope, acquire locks for every distinct parent directory (or sidecar) in sorted canonical-path order, to avoid deadlock between sessions.
- Never hold the lock during interactive review. It is taken at the start of prepare and released after the ledger steps are recorded and directories are synced.
- Contention: try without blocking, with a short bounded wait (about 5 seconds), then abort with "another cvb commit is in progress," writing nothing.

The choice between directory lock and sidecar is made at the source audit; the stable-identity requirement is mandatory either way.

#### Two-phase commit

A. Prepare. Acquire locks. For each file: read its bytes and verify the hash equals the planning hash, otherwise abort ("file changed during review"); capture inode, size and mtime; build the output; write it to a temp file in the same directory (same filesystem), matching permissions and ownership as far as possible; flush to disk. Retain the original bytes (in memory or the journal) for rollback.

B. Final check. For every file, re-stat (inode, size, mtime must equal the values captured in A) and re-hash. Any difference aborts, removes all temp files, and writes nothing.

C. Commit. Rename each temp over its target in a fixed order, then sync the directories. On a failure partway, roll back as described under Guarantees.

D. Record. Only after every rename succeeded, record one ledger step per file with `post_hash` (the hash of the bytes written).

All per-file checks happen in A and B, before the first rename. Individually atomic renames do not by themselves make an envelope atomic. Envelope atomicity comes from checking everything first and from the rollback mechanism, which integrates with the existing transaction mechanism and whose behavior is a prerequisite of the audit.

Deferred: an extra detect-and-recover measure (hard-linking the original to a backup name before the rename and re-hashing it afterwards) is not part of this design. Whether it is needed depends on what the existing journal and write path already provide, so the decision waits for the source audit.

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

### 2.8 Optional view tag (deferred)

Definition if adopted. The tag is a short prefix (at least 12 hex digits) of the SHA-256 of the entire raw file bytes at the time of the pull, printed in the pull output for each file even when only a range is displayed. The envelope may echo it per file. CVB compares it as a prefix of the file's current full-file hash.
- A matching tag is evidence that the quoted line numbers refer to that snapshot. It does not claim the model saw every line, and a partial-range hash would not establish that the whole file is unchanged, which is why the hash covers the full file.
- An absent or mismatched tag does not establish freshness, so the chooser applies.
- A matching tag never replaces exact-content verification.
- Residual: the tag authenticates the pull it was copied from, not each individual number the model quotes.

With a matching tag and `S` in `C`, the stated line could be used among duplicates without the chooser. The tag is not implemented in v1. It would reverse the earlier decision to drop revision tags from v1, so it waits for measured chooser frequency (2.5).

## 3. Sequencing

0. Read-only source audit (section 5), delivered as a written report. Nothing that depends on current applier, journal, or protocol behavior starts before it. It must settle the lock mechanism and multi-file failure behavior.
1. Diff contract, grammar, reconstruction contract and tests. Rendering only.
2. Commit-time verification and locking (2.6), integrated with the existing transaction mechanism. Independent of relocation, since the review delay exists today.
3. Mismatch diagnostics with a read-only whole-file candidate listing in rejection messages. No behavior change. The search code built here is reused.
4. Resolver as a pure function, ledger absent: whole-file search, the decision table, chooser, joint validation.
5. Post-apply line map. It derives from the applied edit set and does not depend on the ledger.
6. Edit ledger with hash chain. It never makes an automatic choice, so its value is preselection, labels and warnings. It is built after the chooser's real frequency and preselection accuracy are known.
7. End-to-end tests.

Search comes before the ledger because it is stateless and fixes the reported failure whenever the quoted block is unique. Each step is a small, reviewable change with tests.

## 4. Test matrix

### 4.1 Diff contract

- Changed lines are exactly marker plus source text for non-transformed lines.
- Hunk headers: `@@ -5 +5 @@` (both counts omitted) parses as one-line hunks; `@@ -5,1 +5,1 @@` is accepted; the renderer emits the canonical form without `,1`; counts of 0 parse with `start` as the line before; a single-line hunk renders and round-trips.
- Property: for random source lines (leading spaces 0, 1, 4, 8; tabs; trailing whitespace; empty; non-ASCII), a parser that strips column 1 and honors the hunk counts reproduces the source for non-transformed lines.
- Empty added line renders as `+` alone; empty context line as a single space.
- Adversarial source lines, each read back by the count-based parser as body: `-- foo` (removed), `=== notes ===`, `+++ b/x`, `--- a/x`, `@@ -1 +1 @@`, `\ No newline at end of file`.
- Escape contract: a real zero-width space renders as `\u{200b}` and is named in the notes. A raw, unnamed line containing the literal text `\u{200b}` reconstructs literally. A named line whose source has a literal backslash next to escape-like text, such as `\u{41}` plus a zero-width space (rendered `\\u{41}\u{200b}`), reconstructs exactly; also `\\` followed by `u{41}`, a backslash at the end of a transformed line, and two adjacent backslashes. Consumed characters are never rescanned. A raw line containing `\\` stays literal. Coverage of the class: NUL, DEL, a C1 control, a lone CR, no-break space, ZWJ, BOM, U+2028. Any other backslash sequence on a named line is a parser error.
- No-newline markers: valid after the last `-`, `+`, or context line on its side; rejected at the start of a body, after another marker, after a non-final line, after a line that does not exhaust the relevant count, and twice after the same line. Old side unterminated, new side terminated: `-foo`, marker, `+foo` round-trips.
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
| Identical expected blocks in two ops, one needs relocation | explicit error naming both ops, nothing written (documented v1 limitation) |
| `--dry-run` with ambiguity | candidates printed, non-zero exit, nothing written |
| Promotion replay | relocation disabled; mismatch fails |
| Cancelled surgery | no ledger step |
| Pull after surgery | ledger resets to the new `H_view` with zero steps |
| If a view tag is adopted later: matching tag and stated line is a candidate | stated line used without the chooser; an absent or non-matching tag uses the chooser; exact-content check still applies |

### 4.3 Ledger, mapping and commit

Ledger and observation:
- Hash matches after CVB's own write; the chain stays valid across two successive surgeries.
- External edit observed at planning: mismatch moves the file to `Broken` and frees the steps.
- Break then restore: the ledger stays `Broken` after the bytes are restored; only a new pull revives it.
- Change and exact restore between two observation points: the hash matches and the ledger stays `Valid`.
- `Broken` file: lookup returns nothing; surgery appends nothing.
- Undo or journal rollback: breaks the chain when observed.
- Whole-file hash covers CRLF versus LF and final-newline changes.

Mapping boundaries (unit tests on the mapping function with synthetic edit sets, plus operation-level tests where the v1 protocol allows the operation):
- Insertion before the first line (`start = 1`) and after the last line (`start = N + 1`). Both must map correctly in the function even though v1 insert ops cannot produce them, and v1 insert ops at either end are rejected.
- Deleting the last line; replacing the first line; replacing the whole file excluding the empty-file case.
- Files with and without a final newline: line numbering is identical; deleting the last line of an unterminated file leaves a result whose end state follows the final-newline rule (section 5, item 8).
- A replacement and another edit in the same envelope: both recorded in snapshot coordinates in one step; mapping through that step matches the result.
- Two successive envelopes: mapping composes in order.
- Multiple non-overlapping edits whose cumulative shifts cancel (for example +3 at line 10 and -3 at line 50): lines between are shifted, lines after map identically, and the line map reports it piecewise.
- An anchor whose first line is affected by an earlier insertion (`start == a`, shifts), an insertion at `start == b + 1` (no effect), an insertion strictly inside the block (unmappable), and an edit touching but not overlapping the block.
- Property test: for random files and random non-overlapping edit sets, every line that the mapping reports as mappable must have equal content in the result and the original. Unmappable lines are never reported mappable.

Commit:
- File modified between planning and commit: abort with "file changed during review," nothing written, for the whole envelope.
- Multi-file: a mismatch on file B found in prepare or final check leaves file A unwritten.
- Failure injection after the first rename (for example, the rename of file B fails): file A is restored from retained originals; if restoration is made to fail, the report names each file's state and the original's location.
- File modified between prepare and the final check (simulated with a hook): detected, abort, all temp files removed, nothing written.
- File modified after the final check and before the rename: not asserted as prevented. The test documents the residual window and its expected consequence.
- Lock identity across replacement: session 1 commits (replacing the target's inode) while session 2 waits; session 2 proceeds only after session 1 releases and then sees the new bytes. A lock on the target inode alone fails this test.
- Lock contention: bounded wait, then abort with nothing written. Multi-file lock ordering does not deadlock two sessions committing overlapping file sets in opposite orders.
- Candidate sessions and the main session contend on the same lock identity for the same target.
- Symlinked target: behavior matches the documented rule.
- Same-envelope: all hints refer to the same snapshot; the ledger records one step per file per envelope with edits in snapshot coordinates, recorded only after all files committed.
- Line endings, final newline and size caps (3 MiB per envelope, 8 MiB per file, 32 MiB aggregate) unchanged.

## 5. Architecture and items to verify in the source

Advice from first principles:
- Resolution is a pure function over (snapshot lines, parsed ops with hints, optional ledger view). Its output is resolved ranges with labels, or an ambiguity set, or a mismatch with diagnostics. It is testable without files or a clipboard.
- It belongs in planning, before the diff is built. The diff, the confirmation and the commit consume the same resolved plan, so "reviewed bytes equal committed bytes" cannot be broken by a second resolution.
- The applier receives only resolved ranges and knows nothing about hints.
- The hash chain means the ledger does not need to survive a process restart; in-memory is enough.

Verify before coding (milestone 0, a written report):
1. Envelope coordinate semantics: whether every op's line hints in an envelope refer to one snapshot, and how the existing protocol and validator define ranges and anchors (2.2).
2. Applier write path: how files are written today (locking, temp file and rename, permissions, fsync), what identity any existing lock has across rename, and whether a re-check of the file just before the write already exists (2.6).
3. Multi-file commit: how all-or-nothing is achieved today, what happens when a failure occurs after some files are renamed, and whether originals are retained for rollback. The two-phase protocol must integrate with it.
4. Journal behavior: backups it keeps, how rollback works, and how the journal and undo interact with committed ledger steps and with the file bytes the ledger observes.
5. Renderer sharing: whether the diff renderer is shared between the screen pager and the clipboard output, so escaping and naming are consistent. What pull output does today with line numbers, gutters and unusual characters.
6. Insert anchors: how they are represented, and whether ops must be in file order.
7. Text handling: whether non-UTF-8 files are accepted; how envelope payloads treat backslashes and unusual characters today; how symlinked targets are handled.
8. Final-newline semantics: the rule that "final-newline state is preserved" must be defined for deleting or replacing the last line of an unterminated file. The proposed definition: the result ends with a terminator if and only if the original did, so deleting the last line of an unterminated file removes the terminator of the new last line.

## 6. Decisions and deferrals

Decided:
- Escape control and invisible characters in the diff body, under the reconstruction contract in 1.5.
- Candidate display limit is 5. More than 5 aborts with "too generic."
- The indentation-heuristic note is deferred.
- The initial view baseline is the last pull, with the limitations in 2.2 documented.
- Same-boundary inserts are rejected in v1.
- Two ops with identical expected blocks, where either needs relocation, are rejected in v1 as a documented limitation.
- A unique exact match is automatic. Several exact matches are automatic only with a defensible indication that the hint is current; where freshness is unknown, the chooser is required.
- The commit lock must have a stable identity across replacement. All checks precede the first rename. The race with non-cooperating writers is narrowed, not closed.
- Envelope text stays raw bytes in v1, with a diagnostic when a block cannot be quoted.

Deferred:
- Optional view tag (2.8), until chooser frequency is measured.
- Whether and when to build the edit ledger (milestone 6).
- Hard-link backup in the commit protocol, until the audit shows what the journal and write path provide.
- Envelope escape syntax.
- A defined order for two inserts at the same boundary.
- A joint-assignment rule for two ops with identical expected blocks.
- The directory-lock versus sidecar choice, settled at the audit under the stable-identity requirement.

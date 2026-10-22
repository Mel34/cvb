# CVB surgery: diff format and anchor relocation, design v2.3

Status: Consolidated design following the milestone 0 source audit. This document supersedes conflicting statements in the original v2.3 design and its two amendments.

Precedence applied:

1. Amendment 2: reconciliation with the milestone 0 audit.
2. Insert-anchor amendment: the established single-anchor representation.
3. Original design v2.3: retained wherever it does not conflict with the above.

The original amendments remain useful as historical records, but this document is the current consolidated specification.

Audit limitation: The milestone 0 audit was read-only and source-based. It did not run tests or perform failure injection. Findings about existing source behavior are distinguished below from requirements that still need implementation, testing, or owner approval.

## 0. Consolidation summary

| Topic                 | Consolidated decision                                                                                                                                                                       |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Diff body             | Standard unified-diff gutter; source text begins immediately after the gutter character.                                                                                                    |
| Diff escaping         | Escape unsafe Unicode characters on transformed lines; double literal backslashes on those lines; identify every transformed line in the notes.                                             |
| Diff parser           | Parse body lines by hunk counts, not by interpreting their contents. Validate no-newline markers strictly.                                                                                  |
| Pull output           | Remains raw UTF-8 `path:LINE: text` records. It does not adopt the diff renderer's escaping scheme.                                                                                         |
| Coordinates           | All operations in an envelope resolve against one original snapshot and form one simultaneous edit set.                                                                                     |
| Insert representation | One contiguous anchor block, a `before` or `after` placement, and a payload. No second anchor is implied.                                                                                   |
| Duplicate matches     | A unique exact match is selected automatically. Multiple exact matches require a human choice in v1 because hint freshness is unknown.                                                      |
| Commit lock           | Use the existing persistent project-wide `project.lock` sidecar and non-blocking exclusive `flock`.                                                                                         |
| Commit verification   | Check all targets before journaling, retain per-file checks immediately before rename, and add a full all-target hash check after temporary files are prepared and before the first rename. |
| Recovery              | Preserve the existing journal and its conditional recovery behavior. Do not claim multi-file commits are inherently atomic.                                                                 |
| Final newline         | Preserve the original file's final-newline state, including when the last line is deleted.                                                                                                  |
| View tag              | Deferred until chooser frequency and preselection accuracy can be measured.                                                                                                                 |
| Remaining decisions   | Pull-output delimiter, final symlink/path-component hardening, and the user-facing procedure for clearing an unfinished `Prepared` transaction.                                             |

## 1. Diff format

### 1.1 Findings and principles

The original rendering defect was an extra separator space between the diff marker and the source text. This made changed source lines appear to have an additional leading space.

Commentary must not be interleaved with the diff body. The parser and an LLM both use the first column to identify source lines, so an indented diagnostic can be mistaken for a context line.

Diagnostics should be conditional and should report meaningful anomalies rather than emit repetitive notes for every ordinary line.

### 1.2 Format

1. The body is standard unified diff.
2. Column 1 is the gutter:
   - `' '` for context;
   - `'-'` for removed text;
   - `'+'` for added text.
3. Source text starts in column 2. There is no additional separator space.
4. Operation and relocation labels appear on the hunk header after the closing `@@`.
5. An unterminated final line uses the standard marker `\ No newline at end of file`.
6. A one-line legend precedes the diff.
7. Commentary appears only after the body in a `=== notes ===` section, and only when there is something to report.
8. The rendered diff string is shared by the pager and clipboard. The format and escaping must be implemented once, not independently for each output destination.

Example:

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

The source line's own leading spaces are preserved exactly. The single gutter character is not part of the source text.

### 1.3 Grammar and parser contract

```
output     := banner LF legend LF file+ notes?
file       := "--- " path LF "+++ " path LF hunk+
hunk       := "@@ -" range " +" range " @@" label? LF body
range      := start ("," count)?
body       := (gutterline | nonewline)*
gutterline := (" " | "-" | "+") text LF
nonewline  := "\ No newline at end of file" LF
notes      := LF "=== notes ===" LF note*
```

Range rules:

- An omitted count means one line. `@@ -5 +5 @@` is valid and equivalent to `@@ -5,1 +5,1 @@`.
- A count of zero is allowed. Its start value is the line before the empty range, as in standard unified diff.
- The parser accepts both explicit and omitted counts.
- The renderer's canonical output omits `,1`.

Body parsing is count-based. The parser reads gutter lines until the old and new line counts have both been exhausted. A context line consumes one line from each side; a removal consumes one old line; an addition consumes one new line. A no-newline marker qualifies the preceding gutter line and does not consume a line count.

After the counts are exhausted, the next line must be a hunk header, the next file header, a blank line followed by the notes header, or the end of output.

The parser must not infer structure from source text within the body. For example:

- A removed source line `-- foo` renders as `--- foo`.
- A source line `=== notes ===` renders as `+=== notes ===`.
- A source line `+++ b/x` renders as `++++ b/x`.
- A source line `@@ -1 +1 @@` renders as `+@@ -1 +1 @@`.
- A source line equal to `\ No newline at end of file` renders as `+\ No newline at end of file`.

Each is body text because the gutter and hunk counts establish its role.

No-newline marker validation:

A marker is valid only immediately after a gutter line of the same hunk and only when that line is the final line of the side or sides it qualifies:

- After `-` when it exhausts the old count.
- After `+` when it exhausts the new count.
- After a context line when it exhausts both counts.

At most one marker may follow a given line. A marker at the beginning of a body, after another marker, after a non-final line, or after a line that does not exhaust the relevant count is a parser error.

Paths containing newlines or other control characters are rejected in v1 so that file headers remain unambiguous.

### 1.4 Reconstruction contract

The diff body is byte-exact except for the explicit transformation rules below. Target files must be valid UTF-8; invalid-byte escaping is not needed for this v1 contract.

#### Source lines and line endings

A source line consists of the bytes between line terminators, excluding the terminator itself.

- In a CRLF file, the `\r` that belongs to a CRLF terminator is not part of the source line.
- Mixed line endings in one file are rejected.
- CRLF is reported once per file in the notes and is applied by CVB when reconstructing the payload.
- A final line without a terminator is marked with `\ No newline at end of file`.
- Line terminators are not included in operation payload text.

#### Transformed lines

A line is transformed if it contains at least one character in the escape class.

The escape class is:

- C0 and C1 control characters other than TAB, including DEL and a lone CR that is not part of a CRLF terminator;
- Unicode format characters, category `Cf`, including zero-width characters, bidi controls, soft hyphen, and byte-order mark;
- line and paragraph separators U+2028 and U+2029;
- space separators other than ordinary U+0020, category `Zs`, including no-break space.

The exact class is implementation data and must be locked by golden tests.

On a transformed line:

- Each character in the escape class is rendered as `\u{h}`, with lowercase hexadecimal, no leading zeros, and `0` for NUL.
- Every literal backslash is rendered as `\\`.
- The entire line is named in the notes as `shown escaped`.

A line without characters in the escape class remains raw, including its backslashes.

Examples of note entries:

```
op 2 L140 +: shown escaped
file src/x.rs L140 context: shown escaped
```

Lines not named in the notes are raw.

#### Reconstruction of transformed lines

For a body line `B`, meaning the text after the gutter:

- If the line is not named in the notes, reconstruct its source text as `B` exactly.
- If the line is named, reconstruct it with a single left-to-right unescape pass.

For a named line, the unescape rules are:

1. `\\` produces one literal backslash.
2. `\u{` followed by one or more hexadecimal digits and `}` produces the corresponding Unicode scalar value.
3. Any other backslash sequence is a parser error.
4. Consumed characters are never rescanned.

For example, source text containing the literal sequence `\u{41}` followed by a zero-width space renders as:

```
\\u{41}\u{200b}
```

The first `\\` reconstructs as a literal backslash. The subsequent `u{41}` remains literal text, and the final escape reconstructs the zero-width space.

This rule prevents literal escape-like source text from being mistaken for an encoded character.

The file's line terminators and final-newline state are reconstructed from the file-ending notes and no-newline marker.

#### Payload limitation

Envelope expected text and payloads remain raw UTF-8 text in v1, with no escape syntax. If an expected block contains characters that the diff renderer shows escaped, the mismatch diagnostic must state that the text contains characters shown escaped. The LLM should use neighboring lines as anchors instead of copying the displayed escape notation into the envelope.

Adding an envelope escape syntax is deferred.

### 1.5 Diagnostic behavior

On a mismatch, report:

- The original hint.
- The expected block, displayed safely.
- The actual text near the hint, including up to two lines of surrounding context.
- Exact candidate locations elsewhere in the file, where relevant.
- How each location was derived, such as translated or searched.
- A warning when the expected text contains characters shown escaped.
- Any available ledger information indicating that an earlier surgery rewrote the requested region.

After a successful apply, print a piecewise line map for every file whose line positions changed. For example:

```
src/pull.rs: line map, original -> current (now 812 lines)
  1-119      -> 1-119    (same)
  120-299    -> 134-313  (+14)
  300-302    -> 314      (3 lines replaced by 1)
  303-end    -> 315-end  (+12)
```

### 1.6 Pull output is a separate contract

Pull output currently consists of raw UTF-8 `path:LINE: text` records. It does not escape unusual characters or represent line terminators.

Unlike the diff renderer, pull output must remain raw in v1 because LLM-generated envelope text is raw and has no escape syntax. The diff renderer's escaping and naming scheme must not be applied to pull output as part of this work.

The space after the second colon is an open usability question. It may make source-leading spaces ambiguous to an LLM. The owner must decide whether to leave it, add a legend, or remove the space. This is outside milestone 1 unless explicitly approved.

## 2. Relocation

### 2.1 Principle

Hints locate; exact content verifies.

Every tier is a source of candidate locations, and a location is acceptable only if the whole expected block matches there byte for byte. Exact bytes establish eligibility but do not establish which occurrence was intended.

Automatic selection among multiple exact matches therefore requires defensible evidence that the hint is current. Without that evidence, a human chooses.

### 2.2 Coordinate systems and same-envelope contract

Three coordinate systems are relevant:

- V — view: the file version the LLM was last shown, and the line numbers it quotes. CVB cannot observe exactly what the LLM saw. The initial baseline is the last pull of the file in the session, recorded by hash.
- S — snapshot: the current file at planning time.
- R — result: the file after a successful apply.

Known limitations include older pulls remaining in model context, files changing between pulls, hints drawn from remembered output, other sessions' pulls being unknown, and range pulls not showing the rest of a file.

Same-snapshot invariant: all operations in one envelope are resolved against the same original snapshot. The protocol's line hints refer to that snapshot. Operations are not sequentially adjusted to account for one another.

After resolution and joint validation, the operations are applied as one simultaneous edit set in descending position order. Operations need not appear in file order in the envelope.

The existing source audit confirmed that the current implementation follows this coordinate and application model.

### 2.3 Insert representation

An insert operation consists of:

- One contiguous anchor block `A` containing one or more lines, verified byte for byte.
- A placement, either `before` or `after`.
- The payload to insert.

There is no second anchor. The existing protocol's single contiguous anchor block plus placement is the established v1 contract; relocation must not silently change it.

If anchor block `A` occupies lines `a..b`:

- `before` means insertion boundary `P` lies between lines `a - 1` and `a`.
- `after` means insertion boundary `P` lies between lines `b` and `b + 1`.

The corresponding ledger edit is:

```
(start = P's following line, old_len = 0, new_len = payload lines)
```

The placement remains unchanged when an anchor is relocated.

The diff label identifies the anchor and placement, for example:

```
relocated: anchor hint L120 -> L134, insert after
```

#### Resolving an insert

Resolve the contiguous anchor block exactly like any other expected block:

1. Search the whole snapshot for every exact match of the complete anchor block.
2. Apply the normal resolution decision table in section 2.5.
3. After selecting the candidate, compute insertion boundary `P` from the candidate and its `before` or `after` placement.

Do not search for a concatenated before/after anchor pair. Do not reject an insert merely because a hypothetical second anchor no longer adjoins the first; there is no second anchor in this representation.

#### Insert footprints and boundary conflicts

The footprint of an insert is its anchor block `a..b`. Its anchor lines are read-only within the envelope: another operation whose replace or delete range intersects those lines is rejected.

Two inserts whose resolved insertion points `P` are the same boundary are rejected, and the diagnostic names both operations. The comparison is made using resolved boundaries, not anchor identities.

For example:

- Insert after line 10 and insert before line 11 resolve to the same boundary and are rejected.
- Insert before `A` and insert after `A` resolve to different boundaries and are not rejected by the same-boundary rule alone.

An insert adjacent to a replaced or deleted range requires care. For example, insert after line 10 and replace line 11 share a boundary but do not edit the same line. If accepted by the existing validator, inserted text precedes the replacement. Relocation must preserve existing validator behavior and must not silently loosen existing restrictions or introduce new ones beyond the adopted rules.

Overlapping read-only anchors follow the existing validator's behavior. The audit must document which combinations are accepted and rejected.

V1 rejects inserts at the start or end of a file. Preserve this restriction even though the single-anchor representation can express those positions.

#### Weaker property of the existing representation

A two-sided anchor would verify text on both sides of the insertion point. A single anchor verifies only one side.

If an earlier surgery inserts lines immediately after `A`, a later `insert after A` resolves immediately after `A`, before those intervening lines. The text on the far side of the insertion point is not part of the anchor verification. The diff review still shows surrounding context so the reviewer can see what is adjacent.

This limitation is inherent in the established v1 protocol. An optional verified-neighbor anchor could be a future protocol extension, but is not part of this work.

### 2.4 Edit ledger and hash chain

All file hashes are SHA-256 over raw file bytes, including line endings and final-newline state, without normalization.

#### Hash points

- Pull: hash the bytes read by the pull. Record `H_view`, whether the pull displayed the whole file or only a range.
- Planning: hash the snapshot against which the plan is built.
- Commit: re-check the planned bytes according to section 3.
- After commit: `post_hash` is the hash of the exact bytes CVB wrote.

#### Per-file state

The ledger state is:

```
NoView -> Valid(H_view, steps) -> Broken
```

Each committed step contains:

- `pre_hash`;
- `post_hash`;
- an edit set in the step's pre-coordinates.

An edit is represented as `(start, old_len, new_len)`. For an insert, `old_len = 0` and `start` is the line before which the insertion is recorded.

Edits within a step are simultaneous. A step is recorded only after the transaction journal reaches `Committed`. A cancelled, rejected, failed, or rolled-back surgery records no step.

#### Observation points and invalidation

CVB compares the current file hash against the hash expected by the ledger at observation points: pull, planning, and commit check. It does not watch the file continuously.

- If no surgery has been recorded, the expected hash is `H_view`.
- Otherwise, the expected hash is the latest step's `post_hash`.
- On the first observed mismatch, the ledger becomes `Broken` and its steps are immediately freed.
- `Broken` persists even if the file is later restored to its former bytes. Only a new pull of that file establishes a new `H_view` and resets the ledger to zero steps.
- Surgeries on a `Broken` file append nothing.

If an external change and exact reversal both occur between observation points, CVB may never observe the mismatch. Matching bytes mean matching coordinates; this is an observation limitation, not a separate mapping rule.

CVB does not identify who changed a file. An editor, `git checkout`, rebase, undo, or journal rollback is an external change if the resulting bytes differ from the ledger's expected hash.

All lookups go through one function that returns no mapping unless the ledger is `Valid` and the observed hash matches the chain end.

#### Mapping rules

To map a line `n` through one step, apply each edit:

- If `n >= start + old_len`, add `new_len - old_len`.
- If `start <= n < start + old_len`, the line lies inside an edited region and is unmappable.
- Otherwise, the edit does not affect the line.

Steps compose in order.

To map a block `[a, b]`, map both endpoints. The block is unmappable if an edit region overlaps it or an insertion falls strictly inside it (`a < start <= b`). An insertion at `start == a` shifts the block; an insertion at `start == b + 1` does not affect it.

A same-length replacement leaves line positions unchanged but changes content. A later operation must quote the new text, and exact verification enforces that requirement.

Line numbering treats an unterminated last line as an ordinary line.

For a multi-file envelope, each file gets one ledger step. All steps are recorded only after every file has committed and the journal is `Committed`.

The ledger is a locator, never proof. Mapped blocks still require exact matching. The ledger does not select automatically among duplicates. Its roles are labels such as `shift-tracked`, chooser preselection, and warnings when an earlier surgery rewrote the hinted region.

### 2.5 Resolution algorithm

For each operation:

1. Compute `C`, the set of every exact whole-block match in the snapshot. Do not limit the search to a window. Overlapping candidates in repeated-line blocks are distinct candidates.
2. Check whether the stated line `S` is in `C`.
3. If the ledger is `Valid` with at least one step, compute translated line `T` and check whether it is in `C`.
4. Apply the following decision table.

| Number of exact candidates | Hint known to be current? | Result                           |
| -------------------------- | ------------------------- | -------------------------------- |
| `0`                        | Not applicable            | Abort with diagnostics.          |
| `1`                        | Not applicable            | Use the unique candidate.        |
| More than `1`              | Yes, and `S` is in `C`    | Use `S` without an extra prompt. |
| More than `1`              | No or unknown             | Require a human choice.          |

A hint is known to be current only if there is positive evidence it was produced from the exact file bytes now on disk. A fresh ledger, a ledger with zero steps, or a matching stated line does not prove this.

The only proposed freshness mechanism is the optional view tag in section 2.7, which remains deferred. In v1, hint freshness is unknown, so every multi-candidate case requires a human choice.

For a unique candidate, label it:

- `exact` if it matches `S`;
- `shift-tracked` if it matches `T`;
- `searched` otherwise.

For multiple candidates, preselect for convenience only:

1. Candidate `T`, if the ledger is valid with steps and `T` is a candidate.
2. Otherwise candidate `S`, if it is a candidate.
3. Otherwise no candidate.

If both `S` and `T` are candidates and differ, preselect `T` and identify `S` as the stated line. Preselection is never an automatic decision.

After resolution, jointly validate all operations, then present the normal confirmation diff. The same resolved plan must drive the diff, confirmation, and commit; no second resolution may alter the reviewed bytes.

Relocated operations are labeled in the diff, for example:

```
relocated: hint L120 -> L134 (+14), shift-tracked
searched
chosen by user: L134 of {88,134,301}
```

#### Identical expected blocks

Two operations in one envelope with identical expected blocks, where either operation requires relocation, are rejected in v1 with an explicit error naming both operations. Nothing is written.

This is a deliberate v1 limitation. A legitimate envelope that intends to edit two separate occurrences of the same block remains unsupported until a joint-assignment rule is designed.

Promotion replay is different: when the surgery applier is used mechanically to replay net per-file changes, relocation is disabled. Replay is exact or fails.

### 2.6 Choose-required user experience

The chooser is a separate screen before the diff review and appears only when an operation has multiple candidates.

It lists up to five candidates, each with line numbers and two lines of context, sorted by distance from the hint. Any preselected candidate is marked.

- Press the candidate's number to choose it.
- Press `q` to abort.
- Space and Enter do not select a candidate.
- No file is written before a choice is made.

Once every operation is resolved, the plan is revalidated and the ordinary diff review follows. Space applies the reviewed plan.

Because v1 has no freshness evidence, the chooser applies to every operation whose expected block appears more than once, even if one occurrence matches the stated line.

If there are more than five candidates, abort with:

```
anchor too generic: N exact matches; quote more context
```

In `--dry-run` or other non-interactive mode, print the candidates, apply nothing, and exit non-zero.

The chooser adds one deliberate keypress per ambiguous operation. The preselected candidate is a convenience, not an assertion that it is correct.

Measure chooser frequency and how often the preselected candidate is chosen before deciding whether the optional view tag is worth implementing.

### 2.7 Optional view tag — deferred

If adopted, a view tag is a short prefix of at least 12 hexadecimal digits of the SHA-256 hash of the entire raw file at pull time. Print it for each file even when the pull displays only a range. An envelope may echo the tag per file.

A matching tag is evidence that the quoted line numbers refer to that snapshot, but it does not establish that the LLM saw every line. A range-only hash would not establish that the rest of the file is unchanged, so the hash must cover the entire file.

- A matching tag never replaces exact-content verification.
- An absent or mismatched tag does not establish freshness, so the chooser remains required.
- A matching tag and a stated line that is an exact candidate could allow the stated occurrence to be used without the chooser.

The tag is not implemented in v1. Reconsider only after measuring chooser frequency and preselection accuracy.

## 3. Commit protocol, locking, and recovery

### 3.1 Existing lock mechanism

Use the existing persistent sidecar lock, `project.lock`, in the project's state directory.

The lock is acquired with a non-blocking exclusive `flock` and held for the commit only. It is separate from target files, so replacing a target does not change the lock's identity.

The earlier proposal to lock the parent directory is withdrawn. A stable sidecar mechanism already exists; replacing it would require a demonstrated reason.

The previous proposal to acquire multiple directory locks in sorted order and wait up to five seconds is also withdrawn. There is one project-wide lock, so multi-lock ordering is unnecessary. Existing contention behavior remains immediate failure with `E-LOCK` and a clear message unless usage establishes a need for waiting.

Requirements:

1. CVB must never remove and recreate the lock file while another process may hold it.
2. Every session that commits into the project tree must use the same project lock.
3. The lock location is derived from the state directory (`XDG_STATE_HOME`, otherwise `HOME`).
4. Candidate sessions may use their own state locations for their own workspaces.
5. When promotion replays changes into the real project tree, it must acquire the real project's lock.

The source audit identified these requirements for verification and testing; it did not demonstrate that every case passes.

### 3.2 Commit sequence

The existing write path already has a transaction journal that retains original bytes for every file. Preserve it.

The source-audited sequence is:

1. Acquire the project lock.
2. Hash every planned target and compare it against the planning hash.
3. Write the `Prepared` journal, including digests and original bytes for every file.
4. Write and sync temporary files.
5. Re-check each file's digest immediately before renaming it.
6. Rename each temporary file over its target.
7. Perform best-effort directory synchronization.
8. Verify the written bytes by reading them back.
9. Mark the transaction `Committed`.
10. Record ledger steps only after `Committed`.

The original v2.3 inode/size/mtime tuple is dropped. Content hashes are the authoritative check; the guarantee concerns bytes.

#### Required addition: final all-target hash check

After all temporary files are built and before the first rename, re-hash every target.

If any target's hash differs from the expected planning hash:

- Abort the envelope.
- Remove the temporary files.
- Write nothing to any target.

Keep the existing per-file digest re-check immediately before each rename. It narrows the race window for each file.

The initial all-files hash check before journaling remains in place. The new full check is an additional check after temporary files have been prepared and before the first rename.

### 3.3 Guarantees and residual race

If a mismatch is found during either all-target check before the first rename, the envelope aborts without writing any target.

The per-file check immediately before each rename also remains mandatory. If a mismatch is discovered on a later file after earlier files have already been renamed, the existing journal recovery mechanism handles the partial state.

A non-cooperating writer, such as an editor, can still change a file after the final all-target check and before that file's rename. In a multi-file commit, this window extends until each file's own rename. POSIX does not provide a general mechanism to eliminate this race with non-cooperating writers.

The design narrows this window but does not claim to close it. The limitation must be documented in the CVB manual in plain language.

### 3.4 Rollback and recovery

Recovery builds on the existing journal and its retained original bytes. It is conditional, not an unconditional overwrite.

Recovery classifies each target as:

- Original: bytes match the original state.
- Planned: bytes match the intended new state.
- Other: bytes match neither.

Preserve the existing recovery behavior:

- When files are uniformly in a recognized state, recovery rolls back or forward as appropriate.
- When files are mixed between planned and original states, planned files are restored from retained originals.
- Unknown content is not overwritten. Recovery reports a conflict and leaves the transaction in `Prepared`.
- An unfinished `Prepared` transaction blocks further surgery with `E-RECOVER`.

The source audit found that the journal already retains original bytes, but it did not prove every rollback path by failure injection.

Required tests:

1. Inject an ordinary rename failure after an earlier file has been renamed. Verify rollback succeeds and restores the original state.
2. Inject a restoration failure. Verify the report accurately identifies each file's state and where the retained original can be found.
3. Verify that a partial failure followed by recovery does not record ledger steps.
4. Verify that ledger steps are recorded only after the transaction reaches `Committed`.

Open owner decision: The audit found no user-facing recovery or undo command, while an unfinished `Prepared` transaction blocks further surgery. Confirm the supported manual procedure for clearing or recovering such a transaction. Adding a recovery command is outside this work's scope.

### 3.5 Symlinks and path changes

The loader walks path components using `symlink_metadata` and rejects symlinks and non-regular files.

The applier does not repeat the path-component check immediately before rename. Rename replaces the directory entry rather than writing through a symlink at that entry.

Open owner decision: Approve or decline additional milestone 2 hardening that repeats the path-component check during the final check and rejects a target if a symlink or disallowed path component is found. If adopted, the operation must abort and write nothing.

This additional rejection is recommended but is not treated as an approved requirement until the owner decides.

### 3.6 Lock and transaction tests

The following must be verified or tested:

- Lock identity remains stable across replacement of a target's inode.
- Two competing processes are mutually excluded by the same project lock.
- The lock is not removed and recreated while held.
- Candidate workspaces use their own workspace lock where appropriate, while promotion uses the real project's lock.
- A mismatch in any target during the initial all-target check leaves all targets unwritten.
- A mismatch in any target during the final all-target check, after temporary files are prepared but before the first rename, leaves all targets unwritten.
- A per-file mismatch immediately before rename triggers correct recovery if earlier files have already been renamed.
- An ordinary rename failure rolls back successfully.
- A restoration failure produces an accurate per-file report.
- A change after the final all-target check and before a file's rename is not claimed to be prevented.
- Symlink/path-component hardening is tested if the owner approves it.
- The transaction journal reaches `Committed` only after successful write and read-back verification.
- Ledger steps appear only after `Committed`, never after failure or rollback.
- Recovery behavior for original, planned, mixed, and unknown file states remains correct.

## 4. Sequencing

The milestone 0 source audit has been completed and its findings are incorporated into this document. The audit was read-only and did not run tests or failure injection.

The implementation sequence is:

### Milestone 1 — Diff contract

Implement the unified-diff format, strict parser grammar, transformed-line escaping, notes, and reconstruction tests.

The pager and clipboard must use the same rendered string. Pull output remains raw as specified in section 1.6.

### Milestone 2 — Commit-time verification and locking

Use the existing project-wide `project.lock` sidecar. Add the full all-target hash check after temporary-file preparation and before the first rename. Preserve the per-file pre-rename check, journal, recovery, and post-commit ledger-recording order.

Add direct failure-injection tests for ordinary rollback and restoration failure.

Obtain the owner's decision on final symlink/path-component hardening before adding that behavior.

### Milestone 3 — Mismatch diagnostics

Add a read-only whole-file candidate listing to rejection messages. This changes diagnostics, not resolution behavior. Reuse the search implementation in the resolver.

### Milestone 4 — Resolver and chooser

Implement the resolver as a pure function over the snapshot, parsed operations, hints, and optional ledger view.

Search the whole snapshot, apply the resolution decision table, display the chooser for ambiguity, and jointly validate all resolved operations.

Use the single contiguous insert anchor and preserve its `before` or `after` placement.

### Milestone 5 — Post-apply line map

Generate a piecewise line map from the applied edit set. This does not depend on the ledger.

### Milestone 6 — Edit ledger

Implement the hash chain and mapping only after the chooser's real frequency and preselection accuracy are known, unless existing implementation constraints require earlier integration.

The ledger never makes an automatic choice. It provides labels, preselection, and warnings.

Record steps only after the transaction journal reaches `Committed`.

### Milestone 7 — End-to-end tests

Exercise the full workflow, including rendering, parsing, relocation, review, commit, journal recovery, and ledger behavior.

Each milestone should remain a small, reviewable change with tests. The stateless whole-file search comes before the ledger because it solves unique-block relocation without introducing persistent mapping state.

## 5. Test matrix

Tests in this section describe required behavior. They are not claims that the tests have already been run or passed.

### 5.1 Diff contract

- Changed lines are exactly the gutter marker plus source text for non-transformed lines.
- Hunk headers:
  - `@@ -5 +5 @@` parses as one-line ranges.
  - `@@ -5,1 +5,1 @@` is accepted.
  - The renderer emits the canonical form without `,1`.
  - Zero counts parse with `start` as the line before the empty range.
  - Single-line hunks render and round-trip.
- Property test: random non-transformed source lines with leading spaces (0, 1, 4, 8), tabs, trailing whitespace, empty text, and non-ASCII text reconstruct exactly after removing only the gutter and honoring hunk counts.
- Empty added line renders as `+`; empty context line renders as one space.
- Adversarial source lines round-trip as body text:
  - `-- foo` on a removed line.
  - `=== notes ===`.
  - `+++ b/x`.
  - `--- a/x`.
  - `@@ -1 +1 @@`.
  - `\ No newline at end of file`.
- Escape contract:
  - A real zero-width space renders as `\u{200b}` and the line is named in the notes.
  - An unnamed line containing literal `\u{200b}` reconstructs literally.
  - A literal backslash adjacent to escape-like text on a transformed line reconstructs exactly.
  - Test `\\` followed by `u{41}`, a backslash at the end of a transformed line, and two adjacent backslashes.
  - Consumed characters are never rescanned.
  - A raw line containing `\\` remains literal.
  - Cover NUL, DEL, a C1 control, lone CR where representable in the renderer's unit tests, no-break space, ZWJ, BOM, U+2028, and U+2029.
  - Any unsupported backslash sequence on a named line is a parser error.
- No-newline markers:
  - Valid after the final `-`, `+`, or context line on its side.
  - Rejected at the beginning of a body.
  - Rejected after another marker.
  - Rejected after a non-final line or a line that does not exhaust the relevant count.
  - Rejected when repeated after the same line.
  - An unterminated old side and terminated new side round-trip correctly.
- CRLF:
  - Terminators are not part of line text.
  - Notes state CRLF once per file.
  - Reconstruction applies CRLF.
  - Mixed endings are rejected.
- A final line without a terminator has the correct marker and round-trips.
- Output contains only the defined headers, gutter lines, no-newline markers, and optional notes section.
- Notes appear only when relevant and identify the operation or file, line, side, and transformation where applicable.
- Legend is present.
- Paths containing control characters are rejected.
- Renderer/parser round-trip covers every golden sample.
- Pager and clipboard use the same rendered diff string.

### 5.2 Pull output

- Pull output emits raw UTF-8 `path:LINE: text` records.
- Pull output does not escape characters or represent line terminators.
- The diff renderer's transformed-line naming rules are not applied to pull output.
- The delimiter decision remains open: leave the `: ` separator, add a legend, or remove the space. Do not change this behavior without the owner's decision.

### 5.3 Resolution and chooser

| Case                                                                               | Expected behavior                                                                                                                |
| ---------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| Block at stated line, unique                                                       | `exact`                                                                                                                          |
| Block repeated; stated line matches; no freshness evidence                         | Chooser required; stated line preselected                                                                                        |
| Block repeated; stated line does not match                                         | Chooser required; no preselection unless translated candidate matches                                                            |
| Prior surgery inserted lines above a unique block                                  | `shift-tracked` or `searched`, depending on whether translation is valid                                                         |
| Prior surgery removed lines above                                                  | Negative shift maps correctly                                                                                                    |
| LLM already corrected its numbers; unique block                                    | `exact`, no double shift                                                                                                         |
| Block repeated; stated and translated candidates both match at different locations | Chooser; translated candidate preselected, stated candidate shown                                                                |
| Block repeated; only translated candidate matches                                  | Chooser; translated candidate preselected                                                                                        |
| Block repeated; ledger is `Broken`                                                 | Chooser; stated line preselected if it matches                                                                                   |
| Prior edit overlaps the block                                                      | Translation skipped; whole-file search only                                                                                      |
| Hint far from a unique block                                                       | `searched`, with no distance limit                                                                                               |
| Hint region rewritten by an earlier surgery; block found elsewhere                 | Label warns to verify intent                                                                                                     |
| Two to five candidates                                                             | Chooser; Space and Enter do not select; number selects; `q` aborts; nothing written before choice                                |
| More than five candidates                                                          | Abort with the "too generic" diagnostic                                                                                          |
| Block absent                                                                       | Abort; diagnostics show actual text near the hint                                                                                |
| Expected text contains a transformed character                                     | Diagnostic says the text contains characters shown escaped                                                                       |
| Overlapping candidates in a repeated-line block                                    | Each is listed                                                                                                                   |
| Multi-line contiguous insert anchor relocates                                      | Entire anchor block relocates as one block                                                                                       |
| Insert placement is `before`                                                       | Placement remains `before` after relocation                                                                                      |
| Insert placement is `after`                                                        | Placement remains `after` after relocation                                                                                       |
| Repeated insert anchor                                                             | Chooser required; selected candidate plus placement determines `P`                                                               |
| Insert after line 10 and insert before line 11                                     | Rejected as the same resolved boundary; both operations named                                                                    |
| Insert before `A` and insert after `A`                                             | Not rejected by the same-boundary rule alone; existing validator still applies                                                   |
| Insert next to a replaced or deleted range                                         | Matches the existing validator's documented behavior                                                                             |
| Another edit intersects an insert's anchor                                         | Rejected                                                                                                                         |
| Insert at start of file                                                            | Existing v1 rejection preserved                                                                                                  |
| Insert at end of file                                                              | Existing v1 rejection preserved                                                                                                  |
| Overlapping operation footprints                                                   | Rejected                                                                                                                         |
| Edit range intersects another insert's anchor                                      | Rejected                                                                                                                         |
| Two inserts at the same resolved boundary                                          | Rejected, naming both operations                                                                                                 |
| Mixed envelope with one exact and one relocated operation                          | Applied together or not at all                                                                                                   |
| Identical expected blocks in two operations, with either requiring relocation      | Explicit error naming both operations; nothing written                                                                           |
| `--dry-run` with ambiguity                                                         | Candidates printed, non-zero exit, nothing written                                                                               |
| Promotion replay                                                                   | Relocation disabled; mismatch fails                                                                                              |
| Cancelled surgery                                                                  | No ledger step                                                                                                                   |
| Pull after surgery                                                                 | Ledger resets to new `H_view` with zero steps                                                                                    |
| Optional view tag, if adopted later                                                | Matching tag plus stated candidate can bypass chooser; missing or mismatched tag uses chooser; exact-content check still applies |

### 5.4 Ledger and mapping

#### Ledger state and observations

- Hash matches after CVB's own write; chain remains valid across two successive surgeries.
- External edit observed at planning moves the file to `Broken` and frees the steps.
- Break then restore: ledger remains `Broken` after restoration; only a new pull revives it.
- Change and exact restore between observation points: matching hash leaves ledger `Valid`.
- A `Broken` file returns no mapping and records no new step.
- Undo or journal rollback breaks the chain when the mismatch is observed.
- Whole-file hash distinguishes CRLF from LF and detects final-newline changes.
- Failed or rolled-back commits record no step.
- A step is recorded only after the journal reaches `Committed`.

#### Mapping boundaries

Use unit tests on synthetic edit sets and operation-level tests wherever the v1 protocol permits the operation.

- Insertion before the first line (`start = 1`) and after the last line (`start = N + 1`) map correctly in the mapping function even though v1 insert operations reject those positions.
- Deleting the last line.
- Replacing the first line.
- Replacing the whole file, excluding the empty-file case.
- Files with and without final newlines have identical line-numbering semantics.
- Deleting the last line of an unterminated file follows the final-newline rule in section 5.5.
- A replacement and another edit in the same envelope are recorded in snapshot coordinates as one step, and mapping matches the result.
- Two successive envelopes compose correctly.
- Multiple non-overlapping edits whose cumulative shifts cancel map intermediate and later lines correctly and produce a correct piecewise line map.
- An anchor whose first line is affected by an earlier insertion (`start == a`) shifts correctly.
- An insertion at `start == b + 1` does not affect the block.
- An insertion strictly inside a block makes it unmappable.
- An edit touching but not overlapping the block follows the specified mapping rule.
- Property test: for random files and random non-overlapping edit sets, every line reported as mappable has equal content in the result and original. Unmappable lines are never reported mappable.

### 5.5 Final-newline behavior

The planner already preserves the final-newline state: the result ends with a terminator if and only if the original did. An unterminated last line is an ordinary line, including when that line is deleted.

Tests must cover:

- Files originally ending with a newline.
- Files originally lacking a final newline.
- Deleting the last line in both cases.
- Replacing the last line in both cases.
- Reconstructing both states through the diff marker and file-ending rules.
- Ledger hashes that distinguish the two states.

This audit item is closed at the design level; no new final-newline rule is required.

### 5.6 Commit, lock, and recovery

- A file modified between planning and commit causes an abort with `file changed during review`, with nothing written.
- A mismatch on file B during the initial all-target check leaves file A unwritten.
- A mismatch on file B during the final all-target check, after temporary files are prepared and before the first rename, leaves file A unwritten.
- Inject a rename failure after an earlier file has been renamed. Verify retained originals restore the original state.
- Inject a restoration failure. Verify the report identifies each file's state and where originals are retained.
- A change after the final all-target check but before a target's rename is not asserted to be prevented. The residual race and expected consequence are documented.
- Lock identity remains stable when a commit replaces the target's inode.
- Two competing processes use the same lock and cannot commit concurrently.
- Candidate sessions and the main session use the correct lock for their respective workspaces; promotion uses the real project's lock.
- Symlinked target behavior matches the approved path-handling rule.
- If final symlink/path-component hardening is adopted, a changed path is rejected before writing.
- All hints refer to the same snapshot.
- Ledger records one step per file per envelope in snapshot coordinates, only after all files commit and the journal reaches `Committed`.
- Rollback records no ledger steps.
- Line endings, final newline, and existing size caps remain unchanged: 3 MiB per envelope, 8 MiB per file, and 32 MiB aggregate.

## 6. Architecture and source-audit findings

### 6.1 Architecture principles

The resolver should be a pure function over:

```
(snapshot lines, parsed operations with hints, optional ledger view)
```

Its result is either:

- resolved ranges with labels;
- a set of candidates requiring a choice; or
- a mismatch with diagnostics.

It should be testable without files or clipboard access.

Resolution belongs in planning, before the diff is built. The diff, confirmation, and commit must consume the same resolved plan so that reviewed bytes cannot diverge from committed bytes through a second resolution.

The applier receives resolved ranges and does not interpret hints.

The ledger need not survive a process restart; its hash chain can be held in memory.

### 6.2 Confirmed by the milestone 0 audit

The source audit found:

1. All operations for a file are validated against the original snapshot and applied as one simultaneous edit set in descending position order. Operations need not be in file order.
2. The existing project-wide persistent `project.lock` sidecar is acquired with non-blocking exclusive `flock` and held for commit.
3. The current write path hashes targets before journaling, writes a `Prepared` journal with digests and original bytes, writes and syncs temporary files, re-checks each target immediately before rename, performs best-effort directory sync and read-back verification, and marks the transaction `Committed`.
4. A full re-check of every target after temporary files are prepared but before the first rename is missing and must be added.
5. The existing journal retains original bytes for every file.
6. Recovery classifies files as original, planned, or other; mixed planned/original states can be restored from retained originals; unknown content is not overwritten and leaves the transaction `Prepared`.
7. The diff-rendering output string is shared between the pager and clipboard.
8. Pull output is raw UTF-8 `path:LINE: text` and does not escape characters or represent line terminators.
9. Insert operations use one contiguous anchor range and a `before` or `after` placement.
10. Target files must be valid UTF-8. NUL, a leading BOM, lone CR, and mixed line endings are rejected at load.
11. Envelope payloads are UTF-8 text with literal backslashes and no escape syntax. NUL and CR in payload blocks are rejected.
12. The planner preserves the original final-newline state.

These are source-audit findings, not proof that the required integration tests pass.

### 6.3 Outstanding verification

The following work remains necessary:

- Direct failure-injection tests for ordinary rename failure and restoration failure.
- Lock-identity and competing-process tests.
- Tests for the new full all-target hash check.
- Verification that no lock-file lifecycle operation can split lock identity across processes.
- Confirmation of the user-facing recovery procedure for a lingering `Prepared` transaction.
- Owner decision on repeating symlink/path-component checks immediately before rename.
- Owner decision on pull-output delimiter handling.

## 7. Decisions and deferrals

### 7.1 Decided

- The diff body uses standard unified-diff gutter rules without a separator space between the gutter and source text.
- The renderer escapes unsafe Unicode characters on transformed lines, doubles literal backslashes on those lines, and names every transformed line in the notes.
- The parser uses hunk counts rather than source-text content to identify body lines.
- The renderer's output string is shared between the pager and clipboard.
- Pull output remains raw UTF-8 `path:LINE: text`; it does not use diff escaping.
- Candidate display limit is five. More than five candidates aborts with a "too generic" diagnostic.
- Unique exact matches are selected automatically.
- Multiple exact matches require a human choice in v1 because hint freshness is unknown.
- Same-boundary inserts are rejected in v1 based on resolved insertion points.
- Insert operations use one contiguous anchor block and a `before` or `after` placement.
- Insert operations at the start or end of a file remain rejected in v1.
- Two operations with identical expected blocks, where either needs relocation, are rejected in v1 as a documented limitation.
- All operations in an envelope resolve against one snapshot and are applied as one simultaneous edit set.
- The persistent project-wide `project.lock` sidecar remains the commit lock.
- Commit verification includes a full all-target hash check after temporary-file preparation and before the first rename, as well as the existing per-file pre-rename check.
- The journal's conditional recovery behavior and retained original bytes remain part of the design.
- Ledger steps are recorded only after the transaction journal reaches `Committed`.
- The planner preserves the original final-newline state.
- Envelope payloads remain raw UTF-8 text with literal backslashes and no escape syntax.
- The commit race with non-cooperating writers is narrowed, not eliminated.

### 7.2 Deferred

- Optional view tag, until chooser frequency and preselection accuracy are measured.
- Whether and when to implement the edit ledger, subject to implementation dependencies and measurement.
- Envelope escape syntax.
- A joint-assignment rule for multiple operations with identical expected blocks.
- A second verified neighboring anchor for insert operations.
- Pull-output delimiter changes, unless the owner approves them.
- Final symlink/path-component hardening immediately before rename, pending owner approval.
- A new user-facing recovery or undo command for unfinished `Prepared` transactions; adding one is outside this work.
- Any extra hard-link backup mechanism unless later evidence shows the existing journal and write path need it.

### 7.3 Explicitly withdrawn from the original design

- Parent-directory locking as the recommended mechanism: replaced by the existing project-wide `project.lock` sidecar.
- Sorted acquisition of multiple locks: unnecessary with one project-wide lock.
- A bounded five-second lock wait: preserve the existing immediate `E-LOCK` behavior.
- Inode/size/mtime tuple comparison as a commit guarantee: use content hashes as the authoritative check.
- Two-sided insert anchors and concatenated anchor-pair searching: preserve the existing single-anchor representation.
- Escaping and naming transformed lines in pull output: pull remains raw.
- Requiring an insert's second anchor to remain adjacent: no second anchor exists in the established v1 representation.

## 8. Acceptance criteria

The design is ready to serve as the implementation specification when the following are respected:

1. The diff format, parser grammar, transformation rules, and reconstruction contract agree.
2. Pull output remains a distinct raw-text contract.
3. The resolver and commit use one immutable resolved plan.
4. All operations in an envelope use the same snapshot coordinates.
5. Insert relocation preserves the single contiguous anchor plus placement representation.
6. The chooser remains mandatory for ambiguous blocks without freshness evidence.
7. The existing project-wide sidecar lock is used consistently.
8. The new final all-target hash check occurs after temporary-file preparation and before the first rename.
9. Existing per-file re-checks and journal recovery remain intact.
10. Ledger steps are recorded only after the transaction is committed.
11. Final-newline state is preserved.
12. Failure-injection and concurrency tests verify behavior rather than assuming it.
13. Open owner decisions are resolved explicitly instead of being silently converted into implementation requirements.

End of consolidated design.

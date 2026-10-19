# CVB surgery protocol, v1 (draft spec)

Status: DRAFT rev 3 for review (rev 2: guarantee wording, normative line model, diff visibility, column numbering, locking and journal, six open questions settled; rev 3: single-transition commit record, deterministic parser error precedence, unknown-content rule on every recovery path). Design only; no implementation yet. This is the contract between three parties: the LLM that produces surgery blocks, the user who pastes them, and `cvb surgery`'s parser/planner/applier.

## 0. Principles

1. **Validation is all-or-nothing.** Every operation and every precondition validates before any file is touched. Any parse or planning failure means zero changes. A failure during commit is a different class: CVB attempts recovery from the journal and reports, file by file, what was restored and what could not be (section 6.1).
2. **Never search for a better location.** An edit applies at exactly the stated lines or not at all. No fuzzy matching, no relocation, no "closest match".
3. **Structure never depends on content.** Payload is located by declared line counts, not by scanning for terminators. Payload is raw: no escaping, no encoding.
4. **Ambiguity is rejection.** Anything malformed, truncated, duplicated, or unclear is rejected with a precise reason. Nothing is guessed or repaired.
5. **The reviewed diff is the commit.** The exact bytes shown during review are the bytes written. No recomputation after confirmation.
6. **Clipboard text is untrusted input.** It is produced by a model and passes through chat UIs. Human review of the diff is the final safeguard.

No finite marker is guaranteed absent from arbitrary source. Collisions are made negligible by the token (section 2) and made harmless by counts (principle 3); a residual collision becomes a rejection, never silent corruption.

## 1. Scope of v1

- Existing files only. Operations: `replace`, `delete`, `insert`.
- Not in v1: create, delete or rename files; start-of-file or end-of-file inserts; empty files; non-UTF-8 files; mixed line endings; `--yes`; undo (the journal format must not preclude it).
- Known cost of this scope: appending at the end of a file must be written as a `replace` of the last line with itself plus the new lines.

## 2. Framing

### 2.1 Structural lines

Every structural line has the exact form:

    CVB::<TOKEN>::<KEYWORD>[ <key>=<value>]...

- `<TOKEN>`: 8 to 32 characters from `[A-Za-z0-9]`. Issued by cvb (the thread puller prints one, and `cvb surgery --spec` prints one) and copied by the LLM; a self-chosen token is permitted if it satisfies the charset and length. One token per envelope. Every structural line of an envelope carries the same token.
- Fields are separated by single spaces. Keys are lowercase. Values contain no whitespace. Field order is fixed per keyword (below). Unknown or duplicate keys are rejected.
- No leading or trailing whitespace on structural lines.
- Pure ASCII. No markdown-sensitive characters beyond `::`.

### 2.2 Keywords and grammar

    envelope  := BEGIN  file+  END
    file      := FILE  op+
    op        := replace | delete | insert
    replace   := OP replace  OLD  NEW  ENDOP
    delete    := OP delete   OLD  ENDOP
    insert    := OP insert   ANCHOR  NEW  ENDOP

| Line | Fields | Meaning |
|---|---|---|
| `BEGIN` | `v=1 id=<ID>` | Opens the envelope. `ID` is `[A-Za-z0-9._-]{1,64}`, unique per surgery. |
| `FILE` | `path=<PATH>` | Starts the operations for one file. |
| `OP replace` | `lines=A-B` | Replace lines A..B. |
| `OP delete` | `lines=A-B` | Delete lines A..B. |
| `OP insert` | `after=A-B` or `before=A-B` | Insert relative to the anchor lines A..B. |
| `OLD` | `n=K` | Followed by exactly K payload lines: the expected current text of A..B. K must equal B-A+1. |
| `ANCHOR` | `n=K` | Followed by exactly K payload lines: expected current text of A..B. K must equal B-A+1. |
| `NEW` | `n=M` | Followed by exactly M payload lines (M >= 1): the new text. |
| `ENDOP` | none | Closes the op. |
| `END` | `ops=N` | Closes the envelope. N is the total number of `OP` lines. |

All numbers are decimal, no sign, no leading zeros. `lines=A-B` always uses both numbers (`10-10` for one line). A, B >= 1 and A <= B.

Between structural lines there is nothing: no blank lines, no commentary. The only non-structural lines inside an envelope are the counted payload lines.

### 2.3 Payload

- Payload lines are taken verbatim, exactly as many as declared, with line terminator removed. Tabs, trailing spaces and leading spaces are significant.
- After a counted block, the next line must be a structural line for this envelope's token. Section 2.4 fixes exactly which error is reported otherwise.
- A payload line must not itself be a structural line for this envelope's token, i.e. must not begin with `CVB::<TOKEN>::`. Otherwise: `E-COLLISION` (the message also hints that a count may be too large, since an over-long block swallows the following structural lines). Lines beginning `CVB::` with a different token are ordinary payload; this matters because CVB's own source and docs will contain such lines.
- Payload must not contain NUL, or a lone CR after clipboard normalisation (section 5). Otherwise: `E-BYTES`.

### 2.4 Parse order and error precedence

The parser is one forward pass over the envelope's lines and fails fast: the first error in document order is the only one reported. Within a single line, the checks below run in the listed order. The same input always yields the same code.

**Before the pass.** `E-LIMIT` (on the normalised input), then `E-NOENV`. Then the BEGIN line is parsed (shape, token, fields; failure is `E-SYNTAX`) and `v` is checked: an unsupported version is `E-VERSION` and parsing stops there, since the grammar of other versions is unknown.

**When a block header (`OLD`, `ANCHOR`, `NEW`) is read, before its payload.** A malformed number is `E-SYNTAX`. A line range with A = 0 or A > B is `E-RANGE`. An `n` that disagrees with the range length (`OLD`, `ANCHOR`) is `E-COUNT`.

**At payload line i of a declared n:**

1. Input exhausted: `E-TRUNC`.
2. Line begins `CVB::<TOKEN>::` with this envelope's exact token: `E-COLLISION`. An over-long count surfaces here, because the block reaches the next structural line (including `END`) before it reaches the end of input.
3. Line contains NUL or a lone CR: `E-BYTES`.

**At a structural position** (the start, or after BEGIN, FILE, ENDOP, or a finished block):

1. Input exhausted: `E-TRUNC`.
2. Line begins `CVB::<TOKEN>::` with this envelope's exact token: check keyword, fields, field order and position against the grammar. Failure is `E-SYNTAX`, except when this is the final line of the input (a single trailing empty line from a final newline is ignored), in which case failure is `E-TRUNC`: the usual cause is a cut-off paste, and the message quotes the line.
3. Line has structural shape (optional leading whitespace, then `CVB::`, token-shaped text, `::`) but is not an exact-token line starting in column 1: `E-SYNTAX` (wrong token, or indentation), with a hint.
4. Any other line directly after a counted block: `E-COUNT` (the usual cause is a count that is too small).
5. Any other line elsewhere, including a blank line: `E-STRAY`.

**After `END`.** `ops=` disagreeing with the parsed op count is `E-SYNTAX`. A later BEGIN-shaped line, of any token, is `E-MULTI`.

**Division of labour.** The parser is pure and has no filesystem access. It owns everything decidable from the text alone: grammar, counts, lexical path rules (`E-PATH`), range well-formedness, duplicate `FILE` paths after normalisation (`E-DUPFILE`), op totals, `E-MULTI`, `E-LIMIT`, `E-VERSION`. The planner owns everything that needs file bytes or cross-operation reasoning against them: file existence and type, symlinks, bounds against file length (`E-RANGE`), preconditions, overlaps, boundaries, and `E-NOOP`.

## 3. Operations and line-number semantics

### 3.1 Numbering

- Lines are 1-based.
- **Line model (normative).** A terminator (LF, or CRLF in a CRLF file) ends a line; it does not begin a new one. The final-newline state is stored separately as a boolean. There is never a phantom empty line after a final terminator, and an implementation must not use a splitter that produces one. Required cases:

  | File bytes | Lines | Final newline |
  |---|---|---|
  | (empty) | 0 (rejected) | n/a |
  | `alpha\n` | 1: `alpha` | yes |
  | `alpha` | 1: `alpha` | no |
  | `alpha\n\n` | 2: `alpha`, empty | yes |
  | `\n` | 1: empty | yes |
  | `alpha\nbeta` | 2: `alpha`, `beta` | no |
  | `alpha\r\nbeta\r\n` (CRLF file) | 2: `alpha`, `beta` | yes |

  The result of an edit is its lines joined by the file's terminator, followed by one terminator if and only if the final-newline state is yes.
- All line numbers in an envelope refer to the file as it is on disk at planning time, never to the file mid-edit. The planner resolves every op against that original and builds the result in one pass.

### 3.2 Preconditions (mandatory)

For `replace` and `delete`, the OLD block must equal lines A..B of the file, byte for byte, line by line. For `insert`, the ANCHOR block must equal lines A..B of the file. Comparison is exact (after the terminator is stripped). No normalisation of whitespace, quotes or tabs. On mismatch: `E-PRECOND`, reporting file, first differing line, first differing column, and both values with control characters and trailing whitespace made visible.

Diagnostic positions: lines are numbered as in 3.1. Columns are 1-based counts of Unicode scalar values within the line (not bytes, not grapheme clusters). The 0-based byte offset within the line is printed alongside. If one string is a proper prefix of the other, the first differing column is the position just past the shorter one, and the message says which side ends there.

### 3.3 Effects

- `replace lines=A-B`: lines A..B are replaced by NEW. NEW may be longer or shorter. (NEW must be non-empty; use `delete` to remove.)
- `delete lines=A-B`: lines A..B are removed. Deleting every line in the file is rejected (`E-EOF`).
- `insert after=A-B`: NEW is placed immediately after line B. `insert before=A-B`: NEW is placed immediately before line A. The anchor is read-only and verified; it is never modified.

### 3.4 Boundaries and conflicts

Define a gap g as the position after line g (gap 0 is before line 1; gap N is after the last line). An `insert after=A-B` targets gap B; `insert before=A-B` targets gap A-1.

- Inserts targeting gap 0 or gap N are rejected: `E-EOF`.
- Two `replace`/`delete` ranges in the same file must not overlap: `E-OVERLAP`.
- Two inserts must not target the same gap: `E-OVERLAP` (their order would be ambiguous).
- An insert's gap must not lie inside a `replace`/`delete` range (gap g with A <= g < B): `E-OVERLAP`. A gap at the boundary (A-1 or B) is allowed; inserted text lands outside the replaced text.
- An insert's anchor lines must not overlap any `replace`/`delete` range: `E-OVERLAP`. Anchors may overlap other anchors.
- A `replace` whose NEW equals its OLD is rejected as `E-NOOP`; it is almost certainly a model error.

### 3.5 Line endings and final newline

- A file must be uniformly LF or uniformly CRLF. A mixed file, or any lone CR, is rejected (`E-FILE`). A file with no terminators at all is treated as LF.
- Payload from the clipboard is compared and stored without terminators. New lines are written with the file's own convention.
- The final-newline state is a property of the file and is preserved: the result ends with a terminator if and only if the original did. This also holds when the last line is replaced or deleted.
- Files with a UTF-8 BOM, non-UTF-8 content, or NUL bytes are rejected in v1 (`E-FILE`).

## 4. Paths

- `path` must match `[A-Za-z0-9._/-]+`, be relative, and contain no empty, `.`, or `..` segments, no backslashes. Otherwise `E-PATH`.
- Resolved against the project root (current directory, or `--root`). After resolution it must still be inside the root, no component may be a symlink, and it must be an existing regular file. Otherwise `E-PATH` or `E-FILE`.
- Paths under `.git/` and under CVB's own state directory are rejected.
- A path may appear in at most one `FILE` section (compared after normalisation): `E-DUPFILE`. A `FILE` section must contain at least one op.

## 5. `cvb surgery`: input behaviour

Invocation: `cvb surgery [--dry-run]` reads the clipboard. `--stdin` (testing and non-GUI sessions) reads stdin instead; the rules are identical.

Extraction, in order:

1. Normalise CRLF to LF in the entire input. Size limits apply to the normalised input (proposed: 4 MiB total, 500 ops); exceeded is `E-LIMIT`.
2. Find the first line matching `CVB::[A-Za-z0-9]{8,32}::BEGIN ` at line start. None found: `E-NOENV`. The token is taken from this line. Everything before it (chat prose, an opening code fence) is ignored.
3. Parse strictly from that line per section 2 and 3, reading payload by count.
4. The envelope ends at its `END` line. Which error is reported for truncated, malformed or miscounted input is fixed by section 2.4; a truncation report names the last structural line successfully read.
5. Everything after `END` (prose, a closing code fence) is ignored, except that any further line shaped like a `BEGIN` line (any token) is `E-MULTI`: two envelopes in one paste is ambiguous.
6. Version other than `v=1`: `E-VERSION`.

Leading whitespace on a structural line (for example a chat UI that indented the block) is `E-SYNTAX`, not repaired.

## 6. Pipeline

1. **Parse** (pure): clipboard text to an in-memory structure, or a rejection.
2. **Plan** (pure): structure plus the contents of each target file to a plan: for each file, new bytes plus a digest of the original bytes. All preconditions, bounds, overlap and boundary checks happen here. Errors from several ops may be reported together.
3. **Review**: render the diff from the plan (section 6.2); print the surgery `id`, the file list and a short plan digest. `--dry-run` stops here with exit status 0. Otherwise ask for confirmation. If there is no TTY, refuse (`E-NOTTY`) unless `--dry-run`.
4. **Lock and recheck**: after confirmation, take the project lock without waiting (`E-LOCK` if held). Under the lock: refuse if an earlier transaction in this project is unfinished (`E-RECOVER`); verify `id` has not been applied (`E-REPLAY`); re-read each file and verify it still matches the digest taken at planning time (`E-STALE` if not).
5. **Commit** (section 6.3): journal first, then temp files, then renames, then journal finalised. If any step fails, recovery runs from the journal and the report states per file what was restored and what could not be: `E-IO`.

The bytes in step 5 are the bytes the diff in step 3 was rendered from.

### 6.1 What is guaranteed, precisely

- **Parse or planning rejection** (every code except `E-IO`): zero files changed.
- **Commit failure** (`E-IO`): CVB attempts recovery from the journal and reports each file as restored, or as not restorable with the reason. A single-file surgery is atomic via rename. A multi-file surgery is not atomic across files; the guarantee is the recovery attempt and an honest report.
- **Unknown content is never overwritten, on any path.** This holds for the normal failure path inside a running commit and for crash recovery alike: a file is restored only if its current content is exactly the planned new bytes. Anything else is left untouched and reported as a conflict.
- **Out of scope:** another process modifying a target file during the commit window (below). CVB narrows that window and detects it where it can, but a rename cannot be made conditional on the file's current content.

### 6.2 Review rendering

The renderer takes only the original bytes and the planned new bytes of each file: nothing else, so the diff cannot differ from what is written. The review must make invisible differences visible, using markers kept apart from the line text (a gutter, or colour with a plain-text fallback), never substituted silently into it:

- Added, removed or changed trailing whitespace on any changed line.
- Tabs versus spaces in changed indentation.
- Final-newline state, with a "no newline at end of file" marker where it applies (unchanged by design in v1, but displayed).
- The file's line-ending convention (LF or CRLF) in the per-file header.
- Control characters, zero-width characters and bidirectional override characters in changed lines, shown as escapes.
- A per-file summary: lines removed, lines added, bytes before and after.

### 6.3 Journal, lock and commit

**Location.** A per-user state directory (default `$XDG_STATE_HOME/cvb/surgery/`, falling back to `~/.local/state/cvb/surgery/`), one subdirectory per project, keyed by a hash of the canonical project root. The canonical root (and device and inode) are recorded inside the journal and checked on every use. Directories are mode 0700 and files 0600.

**Contents.** For each transaction: the surgery `id`; a state (`prepared`, `committed`, `rolled-back`); for every target file its project-relative path, a digest of the original, a copy of the original bytes, and a digest of the planned new bytes. There is no separate applied-`id` set; the journals are the record. An `id` counts as applied if any journal in the project with that `id` is `prepared` or `committed`. A `rolled-back` journal does not block reuse of its `id`.

**The journal is not a source of paths.** It stores only project-relative paths that passed section 4. Recovery revalidates every path against the current root with the same rules and refuses on any mismatch.

**Lock.** One lock per project, held from the start of step 4 to the end of commit. It is an advisory lock the operating system releases when the process dies (so a crash never leaves a stale lock). Acquisition does not wait. The applied-`id` check and the journal update both happen under it, so two simultaneous runs cannot both apply the same surgery.

**Commit sequence (under the lock):**

1. Write the journal in state `prepared`: originals, digests, planned digests. fsync it and its directory.
2. For each file: write the new bytes to a temp file in the target's directory, preserve mode, fsync.
3. For each file, immediately before its rename, re-read the target and check it still matches the original digest; if not, abort without renaming it and run the recovery of step 5 for the files already renamed. Then rename the temp file into place.
4. fsync the directories and re-read each file to confirm it equals the planned bytes. Then mark the journal `committed` as one atomic state transition: write the new state to a temp file, fsync, rename over the state file, fsync the directory. This single transition is the commit point and is also the replay record (the `id` is applied because its journal is `committed`), so there is no second write that a crash could separate from it.
5. On any failure in 2 to 4, before the commit point: run recovery (below). Restore only files whose content is exactly the planned new bytes; leave anything else untouched and report it as a conflict. Mark the journal `rolled-back` only when every file is verified back at its original bytes; otherwise leave it `prepared` with the conflicts recorded, so the project stays blocked (`E-RECOVER`) until they are resolved.

**Recovery.** The same procedure serves the in-process failure path and crash recovery. It decides from the journal state and the current digest of each file, classifying each file as ORIGINAL, PLANNED, or OTHER:

- Journal `committed` or `rolled-back`: nothing to do. Replay protection holds in the first case because the journal itself is the record.
- Journal `prepared`, every file PLANNED: the crash fell after the renames and before the transition. Roll forward: verify, then perform the `committed` transition. The user approved exactly these bytes.
- Journal `prepared`, every file ORIGINAL: mark `rolled-back`.
- Journal `prepared`, a mix of ORIGINAL and PLANNED: restore each PLANNED file to its original (each restore is a temp write and rename, with the digest recheck immediately before), then mark `rolled-back` once every file is ORIGINAL.
- Any file OTHER: that file is untouched and reported as a conflict with its path and the digests expected. The other files are handled as above. The journal stays `prepared`.

A `prepared` journal found at the start of a run makes `cvb surgery` refuse with `E-RECOVER`. A `cvb surgery recover` subcommand is proposed for after the applier exists; until then the refusal message says where the journal is.

**Concurrency honesty.** The lock serialises CVB against itself. Other programs are not locked out; the per-rename recheck makes the window microseconds wide, and the post-commit read-back detects a lost update where it can, but it cannot be closed completely.

## 7. Rejection catalogue

Every code except `E-IO` is a pre-commit rejection and leaves all files unchanged. `E-IO` is a commit failure with a per-file recovery report (section 6.1). Each prints the code, a one-line reason, and a location (envelope line number or file:line). Control characters in echoed text are escaped.

| Code | Condition |
|---|---|
| `E-NOENV` | No BEGIN line found |
| `E-TRUNC` | Input ended mid-block or before a valid END, or the final line is an incomplete structural line |
| `E-SYNTAX` | Malformed exact-token structural line; wrong token or indentation on a structural-shaped line; bad number or field; wrong `ops=` |
| `E-STRAY` | Plain text, or a blank line, where a structural line was required (not directly after a counted block) |
| `E-COUNT` | Declared `n` inconsistent with range, or plain text where a structural line was required directly after a counted block |
| `E-COLLISION` | Payload line is structural for this token |
| `E-MULTI` | A second BEGIN-shaped line after END |
| `E-VERSION` | Unsupported `v` |
| `E-LIMIT` | Size or op-count limit exceeded |
| `E-BYTES` | NUL or lone CR in payload |
| `E-PATH` | Disallowed path |
| `E-FILE` | Missing, not a regular file, symlink, empty, non-UTF-8, BOM, mixed line endings |
| `E-DUPFILE` | Same file in two FILE sections |
| `E-RANGE` | Line numbers out of bounds |
| `E-PRECOND` | OLD or ANCHOR text does not match the file at the stated lines |
| `E-OVERLAP` | Conflicting ranges, gaps or anchors |
| `E-EOF` | Start/end-of-file insert, or delete of every line |
| `E-NOOP` | Replace with identical text |
| `E-NOTTY` | Confirmation needed but no terminal |
| `E-STALE` | A file changed between planning and commit |
| `E-REPLAY` | This `id` was already applied |
| `E-LOCK` | Another surgery holds this project's lock |
| `E-RECOVER` | An earlier transaction in this project did not finish; recover before applying anything else |
| `E-IO` | Commit failure; per-file report of restored and not-restorable files |

Exit status (proposed): 0 applied or dry-run OK; 2 rejected before commit, nothing changed (including `E-LOCK` and `E-RECOVER`); 3 user declined; 4 commit failure (per-file recovery report printed).

## 8. Worked examples

Token `k7Q2xw9R` in all examples.

### 8.1 Replace

Original `data/cvb.bash`, lines 12 to 13:

    umask 077
    snapshot=$(mktemp)

Surgery:

    CVB::k7Q2xw9R::BEGIN v=1 id=demo-replace-01
    CVB::k7Q2xw9R::FILE path=data/cvb.bash
    CVB::k7Q2xw9R::OP replace lines=12-13
    CVB::k7Q2xw9R::OLD n=2
    umask 077
    snapshot=$(mktemp)
    CVB::k7Q2xw9R::NEW n=3
    old_umask=$(umask)
    umask 077
    snapshot=$(mktemp)
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::END ops=1

### 8.2 Delete

    CVB::k7Q2xw9R::BEGIN v=1 id=demo-delete-01
    CVB::k7Q2xw9R::FILE path=src/pty.rs
    CVB::k7Q2xw9R::OP delete lines=38-39
    CVB::k7Q2xw9R::OLD n=2
        eprintln!("CVB DEBUG start");
        eprintln!("CVB DEBUG end");
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::END ops=1

### 8.3 Insert after an anchor

Insert a line after line 20, with a two-line anchor (lines 19-20) to make the position unambiguous:

    CVB::k7Q2xw9R::BEGIN v=1 id=demo-insert-01
    CVB::k7Q2xw9R::FILE path=data/cvb.bash
    CVB::k7Q2xw9R::OP insert after=19-20
    CVB::k7Q2xw9R::ANCHOR n=2
    history -w "$snapshot"
    : > "$session"
    CVB::k7Q2xw9R::NEW n=1
    chmod 600 "$session"
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::END ops=1

Result: the new line lands between original lines 20 and 21. The anchor lines are unchanged.

### 8.4 Two files, several ops (line numbers all refer to the originals)

    CVB::k7Q2xw9R::BEGIN v=1 id=demo-multi-01
    CVB::k7Q2xw9R::FILE path=src/pty.rs
    CVB::k7Q2xw9R::OP delete lines=88-90
    CVB::k7Q2xw9R::OLD n=3
    <three lines>
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::OP replace lines=12-12
    CVB::k7Q2xw9R::OLD n=1
    <one line>
    CVB::k7Q2xw9R::NEW n=2
    <two lines>
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::FILE path=README.md
    CVB::k7Q2xw9R::OP insert before=40-40
    CVB::k7Q2xw9R::ANCHOR n=1
    <one line>
    CVB::k7Q2xw9R::NEW n=3
    <three lines>
    CVB::k7Q2xw9R::ENDOP
    CVB::k7Q2xw9R::END ops=3

Ops within a file may be in any order; the planner sorts them and applies bottom-up in effect, which is why numbering never shifts.

### 8.5 Rejected: incomplete paste

If the paste stops after the `NEW` payload of the last op and the `ENDOP` and `END` lines are missing, the result is `E-TRUNC`, naming the last structural line read. Nothing is modified.

### 8.6 Rejected: stale source

If line 13 of `data/cvb.bash` now reads `snapshot=$(mktemp -p /tmp)`, example 8.1 fails with `E-PRECOND` at `data/cvb.bash:13`, showing the expected and actual line and the first differing column. Nothing is modified.

## 9. Teaching the LLM

`cvb surgery --spec` prints a compact version of sections 2, 3 and 8.1 to 8.3 plus a fresh token. The thread puller prints each pulled region with its file, line numbers, and a one-line reminder to use the surgery format with a token. The LLM is told: copy OLD text from the puller output, never retype it from memory; count payload lines carefully; one envelope per message.

## 10. Conformance tests (one per safeguard, mutation-tested in the style of test-history.sh)

- Each rejection code has at least one input that triggers it, and a check that no file changed.
- Removing the count check, the collision check, the precondition check, any overlap rule, the stale check, or the rollback makes at least one test fail.
- Self-hosting: a surgery whose payload contains `CVB::` structural-looking lines with other tokens applies correctly; with the envelope's own token it is `E-COLLISION`.
- Round trips: LF, CRLF, no-final-newline, and a file whose last line is replaced or deleted preserve line endings and final-newline state.
- Diff authority: a test that the planned bytes equal the written bytes, and that the renderer's only inputs are the old and planned bytes.
- Line model: every row of the table in 3.1, round-tripped through plan and write, including the delete-last-line and replace-last-line cases with and without a final newline.
- Columns: multi-byte characters (accented letters, CJK, emoji) place the reported column on the character, not the byte; prefix cases report the shorter side.
- Review visibility: a surgery that only adds trailing whitespace, only changes tabs to spaces, or only inserts a zero-width or bidi character is shown with the corresponding marker.
- Concurrency: two simultaneous runs with the same `id` produce exactly one application and one `E-LOCK` or `E-REPLAY`; a file modified between recheck and rename is detected at the per-rename check and rolled back.
- Crash points: a simulated crash between every pair of consecutive commit steps, including journal `prepared` written, each temp write, each rename, the read-back, and the single `committed` transition. At each point recovery yields exactly one of: committed (all planned bytes), rolled back (all original bytes), or an unresolved conflict report with unknown content untouched. Replay of the same `id` is rejected whenever the journal is `prepared` or `committed`.
- Unknown content: a target modified by another party before its temp write, before its rename, and after its rename is never overwritten by the commit's failure path or by recovery, and is reported as a conflict.
- A tampered journal path is refused.
- Error precedence: one test per rule in 2.4, including EOF mid-payload (`E-TRUNC`); a payload line carrying the envelope's token (`E-COLLISION`); an over-long count that swallows `END` (`E-COLLISION`); a too-small count followed by plain text (`E-COUNT`); a wrong-token structural line (`E-SYNTAX`); an indented structural line (`E-SYNTAX`); a cut-off final line (`E-TRUNC`); a blank line between ops (`E-STRAY`); and an input with two different errors, which reports the earlier one in document order.

## 11. Decisions on the rev 1 open questions

| Question | Decision |
|---|---|
| Journal location | Per-user state directory, one subdirectory per project, project identity recorded inside (6.3) |
| Blank lines between structural lines | Rejected in v1 (`E-STRAY`) |
| Clipboard backend | Clipboard by default; `--stdin` for tests and pipelines |
| Size limits | 4 MiB and 500 operations to start |
| `E-NOOP` | Hard rejection |
| Undo | Command deferred; journal designed so undo can be added later |

## 12. Open items remaining

1. Exact diff glyphs and colour scheme (requirements are fixed in 6.2; rendering is an implementation choice).
2. Whether `cvb surgery recover` ships with the applier or immediately after it.
3. Which clipboard mechanisms the default backend wraps on each platform.

## 13. Implementation order

1. Pure parser plus tests (framing, counts, malformed fields, token collisions, truncation, multiple envelopes). No filesystem access.
2. Pure planner plus tests (line model, preconditions, overlap rules, boundaries, line endings, columns). Takes file bytes as input.
3. Diff renderer plus tests.
4. Applier: journal, lock, commit, recovery, with the crash and concurrency tests.
5. Thread-puller integration: tokens, revision tags, `--spec`.

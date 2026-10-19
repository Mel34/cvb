# CVB surgery and overlay workflow: implementation guide (instructions for the implementer)

Written 2026-10-17. Inputs: `Overlays.md` (architecture), `surgery-protocol.md` (spec, rev 3), `surgery.rs` (uploaded copy), `claude/surgery-fixes.md` (earlier review-fix plan), `main.rs` (grep only).

You are continuing the implementation of CVB's transactional surgery engine and, after it, the overlay-based verification workflow built on top of it. This guide does three things: it reconciles the two specs (section 2), reports how far `surgery.rs` has got (section 3), and gives you an ordered work plan (section 4). Sections 5 to 8 are working rules, an error-code map, questions you must put to the user, and done criteria.

## 0. How to use this guide

1. Read, in order: this guide, `surgery-protocol.md`, `Overlays.md`, `claude/surgery-fixes.md`, then `surgery.rs`.
2. **Precedence.** The reconciliation ledger (section 2) wins over `Overlays.md` and `surgery-protocol.md` wherever they disagree, until Phase 0 has amended those files. After Phase 0, the amended files are authoritative and the ledger is history.
3. **Scope now.** You may work on Phases 0 to 5. Phase 6 (overlay milestones B to G) is design context only: do not start it until the user says so, because it depends on decisions in section 7.
4. The uploaded `surgery.rs` may be older than the user's local copy. Before anything else, ask the user to paste the current file (or confirm the uploaded one is current) and the output of `cargo test` for it. The baseline in section 3 is about 82 tests in `surgery.rs` (counted by hand).
5. Files that exist but were not available to the reviewer: `cli.rs`, `Cargo.toml`, `scripts/verify.sh`, `keyboard.rs`, `keybind.rs`, `sound.rs`. Ask for `cli.rs` and `Cargo.toml` before Phase 4, and for `scripts/verify.sh` before any Phase 6 work.

## 1. The goal in brief

An LLM proposes source edits as a paste in the surgery envelope format. `cvb surgery` parses it, plans it against the real files, shows a diff, and on confirmation commits it all-or-nothing with a journal for recovery. Later, an orchestration layer will run that same engine against an OverlayFS candidate of the working tree, run checks and hands-on tests there, and only after an explicit human acceptance promote the verified changes to the real tree. Rejected candidates are discarded and leave the real tree untouched.

Two layers, kept separate:

- **Surgery engine** (parse, plan, render, apply, recover). Pure where possible, deterministic, independently testable, knows nothing about OverlayFS, Cargo or promotion. Contract: `surgery-protocol.md`.
- **Orchestration** (baseline, workspace, verification, acceptance, promotion, discard). Contract: `Overlays.md`. It invokes the engine with a root directory and never reaches into its internals.

## 2. Reconciliation ledger

Each item gives the conflict or gap, the resolution, and the text amendment that Phase 0 applies. Items that were marked **[confirm]** have since been accepted by the user (section 7).

**R1. Terminology: "planner" versus "engine".**
`Overlays.md` calls the thing to be completed "the surgery planner" and lists applying edits to files among its duties. The protocol uses "planner" for the pure plan stage only; writing is the applier (protocol 6.3).
Resolution: "surgery engine" means parse, plan, render, apply, recover. "Planner" means the pure plan stage only. Overlays Milestone A is "complete the surgery engine": protocol section 13 steps 1 to 4, plus the CLI wiring of section 5 and `--root`. It cannot be only the planner, because Milestone C writes into the candidate and that needs the applier.
Amend: Overlays sections 5, 10 and 12 (Milestone A); protocol header.

**R2. The engine is root-agnostic.**
The protocol mentions `--root` in section 4 but the invocation in section 5 omits it. Resolution: `cvb surgery [--dry-run] [--stdin] [--root DIR] [--spec]`. The engine must work when DIR is an OverlayFS merged mount: no assumption that DIR is a Git repository, never read an overlay's upper or work directories, store no absolute path except in the journal key (R5).
Amend: protocol section 5.

**R3. Two human gates, two different objects.**
Protocol 6 step 3 requires interactive diff confirmation per surgery (`E-NOTTY` without a terminal, `--yes` deferred). `Overlays.md` section 8 requires explicit acceptance of a whole candidate after hands-on testing. Resolution: both remain. Gate 1 (diff confirmation) authorizes entering a change into the candidate; it stays in force even inside the overlay. Gate 2 (acceptance) authorizes promotion. Neither implies the other. `--yes` stays deferred; revisit only after Overlays Milestone G, and only with the user's say-so.
Amend: Overlays sections 5 and 8; protocol section 1 (note on `--yes`).

**R4. Two state machines, one mapping.**
Surgery journal states: `prepared`, `committed`, `rolled-back`. Overlay lifecycle (Overlays 11): baseline established, candidate prepared, surgery applied, pre-verification passed, under hands-on verification, accepted or rejected, promoted or discarded.
Mapping: "surgery applied" means every envelope applied to the candidate ended with a `committed` journal. Any `E-IO`, `E-RECOVER` or a journal left `prepared` inside a candidate marks the candidate failed. (`E-LOCK` and the other pre-commit rejections change nothing, so they only fail that one surgery attempt, not the candidate.) In the overlay workflow the resolution for a failed candidate is to discard it and keep findings, not to run `cvb surgery recover`. The `recover` subcommand (protocol open item 2) is still needed for direct use on a real tree and ships right after the applier; it does not block the overlay milestones.
Amend: Overlays section 11; protocol section 12.

**R5. Journal, lock and state placement under overlays.**
The journal key is a hash of the canonical project root plus device and inode (protocol 6.3). For a candidate that root is the merged mount path. Rules:
- Each candidate gets its own merged mountpoint path (include the candidate id). Never reuse a path, otherwise a stale `prepared` journal from a dead candidate would block the next one with `E-RECOVER`, or confuse the device/inode check.
- The engine's state directory (journal, lock) lives outside the merged tree and outside the overlay's upper and work directories. For a candidate, the orchestrator runs the engine with `XDG_STATE_HOME` pointing into that candidate's own directory, so candidate journals are kept apart from the user's real ones.
- The real tree's journal never records candidate ids.
- `E-REPLAY` is per root, so a fresh candidate may reuse an `id` that an earlier, discarded candidate used. That is intended.
Amend: protocol 6.3 (one sentence per rule); Overlays section 4.

**R6. Promotion in v1 is a replay of net per-file changes through the engine's applier, not a copy of the overlay diff. (Confirmed, D6.)**
Overlays section 9 lists additions, modifications and deletions. Surgery v1 only modifies existing regular files (protocol section 1). A candidate is baseline plus N envelopes and is immutable after surgery (a failed candidate is replaced by a fresh one, Overlays 8), so the committed surgery journals inside the candidate are an exact manifest of what changed: path, original digest, planned digest, bytes. Promotion therefore works like this:
1. Build the manifest from the candidate's `committed` journals; for files touched by several envelopes take the net change from the baseline digest to the final candidate bytes.
2. For each file, check that the candidate's current bytes match the final planned digest (nothing edited the sources during testing) and that the base's current bytes match the baseline digest recorded when the candidate was created. Any mismatch aborts the whole promotion before anything is written (conflict; no clever reconciliation, per Overlays 9).
3. Render the baseline-to-candidate diff with the same renderer and ask for confirmation (the reviewed bytes are the committed bytes).
4. Commit to the real tree through the same applier: lock, journal, temp files, renames, recovery. Use a promotion id such as `promote-<candidate-id>`.
5. Anything else that differs between the merged view and the baseline (build outputs, caches) is never promoted. A *source* file that differs and is not in the manifest is reported and blocks promotion until the user decides (decision D7).
Additions, deletions, renames, mode changes and symlinks are out of scope until surgery itself supports them (protocol v2); Overlays Milestone F's list is then added in stages. Reading the OverlayFS upper directory directly is never done (whiteouts, opaque directories and `overlay.*` xattrs make it unsafe, as Overlays 9 already says).
Amend: Overlays sections 9 and 12 (Milestone F).

**R7. The baseline and the lower layer.**
Overlays 3 defines the baseline as the live working tree including uncommitted work. The kernel's OverlayFS documentation says that modifying a lower or upper layer while the overlay is mounted gives undefined behaviour (no crash, but no guarantee what the merged view shows). So "treat the baseline as immutable" is a hard requirement, not a style note. Two options (decision D1): lower layer is the live tree and the user does not edit it during an experiment, or lower layer is a snapshot (reflink copy) of the tree. Either way, record digests of every file the surgery will touch before mounting (known after parse and plan), and recheck them at promotion (R6). Optionally record a full manifest (path, size, mtime, digest) of non-ignored files to warn about *any* base change. Do not rely on Git status or commit ids.
Amend: Overlays sections 3 and 14.

**R8. One path denylist.**
Protocol section 4 rejects "paths under `.git/` and CVB's state directory". The user has decided: reject any path containing a `.git` component, compared case-insensitively (`.gitignore`, `.gitattributes`, `.github/...`, `src/git.rs` stay valid), because hooks are code execution. Also reject paths under CVB's state directory and under any candidate workspace directory if one is ever inside the root (by design they are not).
Amend: protocol section 4.

**R9. Input size cap.**
Protocol says 4 MiB measured on the normalised input. Decided: 3 MiB, measured on the input as received. Reason: GNOME Mutter's clipboard silently truncates content over 4 MB, so a truncated paste must arrive above the cap and be rejected as `E-LIMIT`, never parse as a smaller valid envelope. The `E-LIMIT` message includes the received size and the cap and says that if the size is near 4 MB the clipboard may have truncated the paste and the change should be split into several envelopes. The 500-operation cap is unchanged.
Amend: protocol sections 2.4, 5 (step 1), 7 (E-LIMIT row), 11 (size row).

**R10. Code ahead of spec, spec ahead of code.**
Document these behaviours that the code already has: a whole-file no-op (net result equals the original) is `E-NOOP`; `NEW n=0` is `E-COUNT`; a count that overflows `usize` is `E-LIMIT`; file problems found by the loader map to `E-FILE` (missing, not regular, symlink, empty, non-UTF-8, BOM, NUL, mixed endings, lone CR). Update the protocol header: it still says "Design only; no implementation yet". Set it to rev 4 and list parser, planner and loader as implemented.
Amend: protocol header, 2.2, 3.4, 7.

**R11. Verification hazards.**
Mutation-style checks (protocol 10 mentions the `test-history.sh` style) rewrite source files in place: they run only inside the candidate (or a temp copy), never on the real tree. Build output must not pollute the candidate or promotion: prefer `CARGO_TARGET_DIR` outside the project tree (decision D3). Engine tests keep using temp directories.
Amend: Overlays section 6.

**R12. Candidate runtime isolation.**
The candidate `cvb` binary, run by hand in phase 5 of Overlays, shares user-global resources with the installed `cvb`: the capture hotkey, the clipboard, the state directory (surgery journals) and, once built, the CVB Master registry and sockets. Hands-on testing must run the candidate with isolated state (R5's `XDG_STATE_HOME`, plus a separate config and hotkey), or testing the candidate can disturb the user's daily tool. Exact mechanism is decision D4; it must be settled before Milestone E.
Amend: Overlays sections 7 and 14.

**R13. Failure vocabulary.**
Engine rejections use the `E-*` codes of protocol section 7. Orchestration failures (baseline changed, mount failed, verification failed, promotion conflict) get a separate `O-*` family and are never mixed into `E-*`. Findings and logs record the surgery `id`s applied to the candidate.
Amend: Overlays section 11.

**R14. Findings survive discard.**
Proposed layout (confirm in D8): one directory per candidate under the user state directory, with `workspace/` (upper, work, merged mountpoint, engine state) and `keep/` (logs, `findings.md`, copies of surgery journals). Discard copies the engine journals into `keep/` and deletes only `workspace/`. Never delete anything outside the candidate's own directory.
Amend: Overlays section 14.

## 3. State of `surgery.rs`

Measured against protocol section 13 (implementation order). Percentages are my rough estimates of effort done, not measurements.

| Step | Protocol 13 | State |
|---|---|---|
| 1 | Pure parser plus tests | about 85 percent. Works for the main grammar; gaps in 2.4 error precedence (G1 to G3), one real defect (unbounded allocation, fixes step 1) |
| 2 | Pure planner plus tests | about 80 percent. Line model, preconditions, overlap, boundaries, endings, no-write guarantee are in and tested. Missing: diagnostics with line and column, digests, typed load errors |
| 3 | Diff renderer | not started |
| 4 | Applier: journal, lock, commit, recovery, crash and concurrency tests | not started. This is the largest remaining piece |
| 5 | Thread-puller integration, tokens, `--spec` | not started. `main.rs` still prints a placeholder (`println!("Paste CSL block")`) for `cvb surgery`, and `mod surgery;` is declared |

Overall: roughly 30 percent of Milestone A by effort (the applier and its crash tests dominate what is left). Overlay milestones B to G: no code in these files; the user has earlier OverlayFS experience in the POT project.

**What exists:** `Envelope`, `FileSection`, `Operation`, `InsertPosition`; `ParseError` (13 kinds) and `PlanError` (9 kinds); `parse_envelope`, `parse_begin`, `parse_file`, `parse_count`, `parse_range`, `valid_token`; `load_originals`; `plan_envelope`; `apply_operation`; `parse_load_plan` (pure: never writes); `Plan`, `PlannedFile`, `PipelineError`.

**Review-fix plan (`claude/surgery-fixes.md`): none of its nine steps appears to be applied in the uploaded file.**

| Fix step | Evidence it is still open |
|---|---|
| 1 Unbounded allocation | `read_payload` calls `Vec::with_capacity(count)` with no check against remaining lines |
| 2 Wrong-for-the-reason fixtures | `rejects_insertion_before_first_line` still uses a literal `\\n` fixture; three malformed-END/ENDOP tests start with `"\\`; `malformed_begin_header_is_syntax_not_no_envelope` uses a literal `\\n`; `mibibytes` names remain |
| 3 Non-cloning validator | `apply_operation` still returns a cloned `Vec` that `plan_envelope` throws away |
| 4 Deduplicate validation | three separate path validators and two line-ending detectors; no `.git` rule |
| 5 `Parser` struct | `lines`, `index`, `token` still threaded through free functions; `format!` per payload line |
| 6 Error context | errors are unit variants with no line, path or operation info; loader errors are `String` |
| 7 File organisation | single file; test modules sit mid-file with items after them |
| 8 Target file cap | no `MAX_TARGET_BYTES` |
| 9 Clipboard truncation | `MAX_INPUT_BYTES` is 4 MiB; no truncation hint; no prefix test |

**Conformance gaps the fix plan does not cover (protocol 2.4, 3, 4, 6):**

- **G1.** After a counted block, any non-structural line (including a blank line) must be `E-COUNT`. Today `parse_replace` and friends return `Syntax`, or `Truncated` on the last line. Example: a count one too small followed by plain text.
- **G2.** A line with structural shape but leading whitespace or a wrong token, at a structural position, must be `E-SYNTAX`. Today an indented structural line falls through to `Stray`.
- **G3.** A cut-off final line (`CVB::tok::FIL`, `...OP repl`, `...OLD n=`, bare `...END`) must be `E-TRUNC`. Today only the END and ENDOP lines get that treatment; others return `Syntax`.
- **G4.** Trailing whitespace on any structural line is `E-SYNTAX`. Today `FILE path=src/a.rs ` returns `InvalidPath` because a space is outside the path whitelist.
- **G5.** `E-PRECOND` diagnostics per 3.2: first differing line, first differing column (1-based Unicode scalar values) plus the byte offset, both values with control characters and trailing whitespace made visible, and the "one side ends here" rule for prefixes. Fix step 6 asks for most of this; add the column, byte offset and prefix rules explicitly.
- **G6.** `Plan` carries no digest of the original bytes, no digest of the planned bytes, and no plan digest for the review step. Needs decision D2 first.
- **G7.** Protocol 2.4 runs `E-LIMIT` on the *normalised* input before `E-NOENV`. The code checks the *raw* input length. R9 settles this in favour of raw, so only the spec text changes; no code change is needed beyond fix step 9.

## 4. Work plan

Order matters. Each numbered item is one atomic change with its own tests.

### Phase 0: confirm and amend the specs

1. The decisions in section 7 are settled. Ask the user only for the outstanding files listed at the end of section 7 that Phase 0 needs (the current `surgery.rs` and `cargo test` output).
2. After approval, rewrite `surgery-protocol.md` and `Overlays.md` with the amendments named in the ledger, then show the user the diff of each file. Do not touch any other file. Keep the files' structure and numbering so existing references stay valid; mark the protocol rev 4 and the Overlays status as "agreed direction, implementation in progress, see implementation guide".

### Phase 1: harden the parser and planner

3. Execute `claude/surgery-fixes.md` steps 1 to 9 in its own order, under its own ground rules (tests first, show failing test, one step, full diff, wait for confirmation). Three adjustments: step 4's `.git` rule is R8; step 8's file cap is D10 (8 MiB per file plus 32 MiB aggregate, not 16 MiB); step 9's input cap is R9 (3 MiB as received).
4. After fix step 6, add G1 to G5 as separate atomic changes, each with failing tests first:
   - G1, G2, G3, G4: one test per rule in protocol 2.4, plus the specific cases in section 10 of the protocol ("error precedence" bullet). Also a test where an input has two different errors and the earlier one in document order is reported.
   - G3 property test (extends fix step 9): for every proper prefix of a valid multi-file envelope, `parse_envelope` is `Err`. Additionally assert `Truncated` for each prefix whose last line is a cut-off structural line or whose input ends mid-block. Do not assert a specific kind for prefixes that happen to form a complete but contradictory line (for example `n=1` cut from `n=12`); only assert `Err` there.
   - G5: columns computed in Unicode scalar values (test with accented letters, CJK and emoji), byte offset alongside, prefix case, and visible escaping of tabs, trailing spaces and control characters.
5. Add the digest fields (G6), SHA-256 via `sha2` (D2).
6. Do the module split (fix step 7) before Phase 2 so that new code lands in `surgery/render.rs`, `surgery/apply.rs`, `surgery/journal.rs` and `surgery/lock.rs` from the start. If you reach Phase 2 and step 7 is not yet done, stop and do it.

### Phase 2: diff renderer (protocol 6.2, 13 step 3)

7. Pure function whose only inputs are the original and the planned bytes of each file. It must not take the operation list: that is how the diff is guaranteed to match what is written.
8. Must make visible, with markers kept apart from line text: added, removed or changed trailing whitespace; tab versus space in changed indentation; final-newline state ("no newline at end of file"); LF or CRLF in the per-file header; control, zero-width and bidirectional override characters as escapes; per-file summary (lines removed and added, bytes before and after). Plain-text fallback when colour is unavailable.
9. Tests from protocol section 10: a surgery that only adds trailing whitespace, only changes tabs to spaces, or only inserts a zero-width or bidi character shows the matching marker; renderer inputs are exactly old and planned bytes.
10. Diff algorithm: use the `similar` crate (D2), with line-based diffing. Test it on large inputs.

### Phase 3: applier (protocol 6, 6.1, 6.3, 13 step 4)

Build in this order, each step atomic:

11. State directory resolution (`$XDG_STATE_HOME/cvb/surgery/`, falling back to `~/.local/state/cvb/surgery/`), project key from the canonical root plus device and inode, modes 0700 and 0600, and a refusal if the recorded root, device or inode disagrees.
12. Journal format and atomic state transitions (write temp, fsync, rename, fsync directory). Journal holds: surgery `id`; state; per file the relative path, original digest, a copy of the original bytes and the planned digest. Paths stored in it are relative and must be revalidated with the section 4 rules on every read (tamper test).
13. Project lock: non-blocking advisory lock the OS releases on process death (`E-LOCK`). Check the toolchain: `std::fs::File::try_lock` is in std on recent Rust (verify the project's version); otherwise use `libc::flock`. Held from the start of step 4 of the pipeline through the end of commit.
14. Pipeline step 4 checks, under the lock: unfinished earlier transaction (`E-RECOVER`), applied `id` (`E-REPLAY`: any journal with that `id` in `prepared` or `committed`; `rolled-back` does not block), digests of the files against the plan (`E-STALE`).
15. Commit happy path: write journal `prepared`, temp files in each target's directory preserving mode, fsync, per-rename digest recheck, rename, fsync directories, read-back, then the single atomic `committed` transition.
16. Recovery classifier: each file is ORIGINAL, PLANNED or OTHER; apply the rules in 6.3 exactly. It is the same code for the in-process failure path and crash recovery. Never overwrite unknown content on any path.
17. Fault-injection seam so crash tests are possible: the commit function takes a hook (compiled for tests) called between every pair of steps; a test "crashes" by returning at that point without running cleanup, then calls the recovery entry point on fresh state. One test per crash point listed in protocol 10: journal `prepared` written, each temp write, each rename, the read-back, the `committed` transition. Each must end in exactly one of: committed, rolled back, or conflict report with unknown content untouched.
18. Concurrency tests: two simultaneous runs with the same `id` produce one application and one `E-LOCK` or `E-REPLAY`; a file modified between the recheck and the rename is caught at the per-rename check and rolled back; a target modified before its temp write, before its rename and after its rename is never overwritten by failure or recovery and is reported as a conflict.
19. Mutation-style checks from protocol 10: removing the count check, collision check, precondition check, any overlap rule, the stale check or the rollback must make at least one test fail. Run them the way `test-history.sh` does, and only in a copy, never on the user's tree.
20. `E-IO` report: per file, restored or not restorable with the reason. Exit statuses: 0 applied or dry-run OK, 2 rejected before commit (including `E-LOCK`, `E-RECOVER`), 3 user declined, 4 commit failure.

### Phase 4: CLI and input (protocol 5)

21. Needs `cli.rs` and `Cargo.toml` from the user. Replace the `Command::Surgery` placeholder in `main.rs`. Flags: `--dry-run`, `--stdin`, `--root DIR`, `--spec`.
22. Input: clipboard through the existing `clipboard.rs` API, or stdin with `--stdin`. Extraction rules exactly as protocol section 5, with the R9 size handling.
23. Confirmation must read from `/dev/tty`, not from stdin, because with `--stdin` the input has already consumed stdin. If `/dev/tty` cannot be opened: `E-NOTTY`, unless `--dry-run`.
24. `--dry-run` stops after the review step with exit 0 and writes nothing, not even a journal.
25. Surgery must never run inside the PTY-owning capture process in a way that a panic could take the session down; the allocation fix (fix step 1) is the prerequisite. Ask the user which process it runs in before wiring.

### Phase 5: thread-puller integration (protocol 9, 13 step 5)

26. Tokens: generate 8 to 32 `[A-Za-z0-9]` characters from `/dev/urandom` (avoid adding a random-number crate unless D2 allows it). One token per envelope.
27. `cvb surgery --spec` prints a compact version of protocol sections 2, 3 and examples 8.1 to 8.3, plus a fresh token, plus the size cap and the "one envelope per paste, several envelopes for larger changes" rule.
28. The puller prints each pulled region with file path, line numbers and a one-line reminder to use the surgery format with a token.
29. Protocol 13 step 5 mentions "revision tags". They are dropped from v1 (D11): remove the phrase from the protocol in Phase 0 and do not implement them.

### Phase 6: overlay milestones (design context only, do not start without the user's go)

Follow `Overlays.md` section 12, A to G, with these reconciled constraints:

- **A** = Phases 1 to 5 above.
- **B (prototype lifecycle).** Independently of surgery: baseline manifest, create the snapshot (D1), create candidate, change a file in it, run a command in the merged view, discard, prove the baseline is byte-identical, and record the snapshot cost. Mounting needs privileges (root, or an unprivileged user namespace on a recent kernel, or `fuse-overlayfs`); take the approach that already worked in POT as the starting point and ask the user for that code. Upper and work directories must be on the same filesystem, and the work directory must be empty.
- **C (integrate surgery).** Run the engine with `--root <merged>` and `XDG_STATE_HOME` for the candidate (R2, R5). Verify with a baseline digest check that nothing outside the overlay changed.
- **D (pre-verification).** Run `scripts/verify.sh`, `cargo test`, `cargo build` from the merged root (R11). Capture output and exit status. A failing required check blocks advancement.
- **E (acceptance and rejection).** Candidate runtime isolation (R12). Findings template from Overlays 7. Explicit accept or reject command. Reject removes `workspace/` and keeps `keep/` (R14).
- **F (promotion).** Implement as R6 describes. Test: modification promoted; base changed since baseline (abort, nothing written); candidate edited after acceptance (abort); interrupted promotion (recovered through the same journal); promotion of a file that has been modified by two envelopes.
- **G (end-to-end).** Realistic CVB change, deliberate failures, both outcomes proven.

## 5. Working rules

- One atomic change at a time. For each: write the failing test, show that it fails, make it pass, run `cargo test` and `cargo clippy`, show the full diff, stop, and wait for the user's confirmation.
- Do not weaken, delete or loosen an existing test to make a change pass. If a fixture is wrong, fix the fixture and explain.
- Do not change v1 rules: existing files only, replace, delete and insert, mandatory exact preconditions, no insert before the first or after the last line, all-or-nothing, line endings and final newline preserved, mixed endings rejected, no `--yes`.
- Touch only the files a step names. Before Phase 4, that means `surgery.rs` (and the modules it is split into) plus the two `.md` files in Phase 0.
- Never search for a better location, never repair malformed input, never guess. Ambiguity in a spec or in this guide is a question for the user, not a design decision for you.
- Never write to disk in the parser, planner or renderer. The only writer is the applier.
- Never call anything that touches the user's real working tree in a test. Use temp directories with the process id in the name, as the existing tests do.
- Keep error messages plain text, one problem per message, naming file, operation and line; escape control characters in echoed text; never echo more than a few lines of file content.
- Report test counts before and after each step. A step that lowers the count needs an explanation.

## 6. Error-code map

Code enums to protocol codes, so the two vocabularies stay in sync.

| Protocol code | `surgery.rs` today | Notes |
|---|---|---|
| E-NOENV | `ParseError::NoEnvelope` | |
| E-TRUNC | `ParseError::Truncated` | G3 widens when it applies |
| E-SYNTAX | `ParseError::Syntax` | G2 and G4 widen it |
| E-STRAY | `ParseError::Stray` | not after a counted block (G1) |
| E-COUNT | `ParseError::Count` | G1 widens it; `NEW n=0` also lands here |
| E-COLLISION | `ParseError::Collision` | |
| E-MULTI | `ParseError::MultipleEnvelopes` | |
| E-VERSION | `ParseError::Version` | |
| E-LIMIT | `ParseError::Limit` | also count overflow (R10); message per R9 |
| E-BYTES | `ParseError::InvalidBytes` | |
| E-PATH | `ParseError::InvalidPath`, `PlanError::InvalidPath` | add `.git` rule (R8) |
| E-DUPFILE | `ParseError::DuplicateFile`, `PlanError::DuplicateFile` | |
| E-FILE | `PlanError::MissingFile`, `PlanError::InvalidFile`, loader `String` errors | give the loader typed errors (fix steps 4 and 6) |
| E-RANGE | `ParseError::Range`, `PlanError::Range` | message must state the file's line count |
| E-PRECOND | `PlanError::Precondition` | G5 diagnostics |
| E-OVERLAP | `PlanError::Overlap` | message names both operations |
| E-EOF | `PlanError::EndOfFile` | message says to use a replace that includes the first or last line |
| E-NOOP | `PlanError::NoOp` | also whole-file no-op (R10) |
| E-NOTTY, E-STALE, E-REPLAY, E-LOCK, E-RECOVER, E-IO | none yet | applier and CLI phases |

## 7. Decisions (answered by the user on 2026-10-17)

The user accepted every recommendation. Treat these as settled; do not reopen them without the user.

- **D1. Lower layer = snapshot.** The overlay's lower layer is a reflink copy (`cp --reflink=auto`) of the working tree without `target/`, taken when the candidate is created. The real tree may be edited freely during an experiment. Milestone B must measure the copy cost; if a plain copy workspace turns out to be as good as an overlay, report that to the user rather than silently dropping OverlayFS.
- **D2. Dependencies.** Allowed: `sha2` (SHA-256 digests) and `similar` (diff renderer). The journal uses a hand-rolled length-prefixed format; no serde. Check `Cargo.toml`: reuse `libc` or `nix` if already present, otherwise use std's `File::try_lock` if the toolchain has it.
- **D3. Build output.** One shared `CARGO_TARGET_DIR` under the state directory for all candidates, never the real `target/`. Pre-verification runs cargo with `--locked`, so `Cargo.lock` drift fails loudly.
- **D4. Runtime isolation.** The candidate `cvb` runs with `XDG_STATE_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` overridden (never `XDG_RUNTIME_DIR`), its own hotkey in its own config, and the candidate id shown in its prompt. The clipboard cannot be isolated, so prefer `--stdin` for surgery tests. Requirement to pass on to CVB Master Draft 0.3: every socket and path derives from an overridable base directory.
- **D5. `--yes`** stays deferred until after Overlays Milestone G.
- **D6. Promotion by replay** (R6) is confirmed.
- **D7. Unexpected source differences** in a candidate (a source file no surgery journal mentions) block promotion by default and are listed. They are promoted only after explicit per-file approval, which appears in the reviewed diff.
- **D8. Candidate layout** is as in R14: `workspace/` and `keep/` per candidate. `keep/` is always retained. `workspace/` is discarded on reject by default; a `--keep` flag retains a failed workspace for diagnosis, and it is pruned when the next candidate is created.
- **D9. Mounting.** Reuse the approach POT already uses; do not require `sudo`. If POT's approach needs privileges, prefer `fuse-overlayfs` over user-namespace mounts: its mount is visible to the whole session, whereas a namespace mount is visible only to commands launched inside that namespace. Kernel details here are from memory: verify in the Milestone B prototype. **Still outstanding: the user must supply the POT overlay code.**
- **D10. Target-file caps.** 8 MiB per target file and 32 MiB aggregate across all targets in one envelope (the journal stores a copy of every original). This replaces the 16 MiB proposed in `claude/surgery-fixes.md` step 8. Both are constants; the aggregate check applies before any file is read in full, using `metadata.len()`.
- **D11. Revision tags** are dropped from v1. Remove the phrase from protocol 13 step 5. The puller's OLD text is the precondition.

**Still needed from the user before the relevant phase:** the current `surgery.rs` and `cargo test` output (Phase 0), `Cargo.toml` and `cli.rs` (Phase 4), `scripts/verify.sh` and the POT overlay code (Phase 6).

## 8. Definition of done

- **Phase 0:** both `.md` files amended per the ledger, user has reviewed the diffs, open decisions recorded.
- **Phase 1:** all nine fix steps and G1 to G5 merged as separate atomic changes; every behaviour fix has a test that failed before the change; no existing test removed; `cargo test` and `cargo clippy` clean.
- **Phase 2:** renderer merged; the review-visibility tests in protocol 10 pass.
- **Phase 3:** crash-point, concurrency, unknown-content and tampered-journal tests pass; mutation checks show each safeguard is covered; in every failure case the report names which files were restored and which could not be.
- **Phase 4 and 5:** `cvb surgery` works end to end from the clipboard and from `--stdin`; a real multi-file envelope from the thread puller applies and its journal says `committed`; `--dry-run` writes nothing.
- **Milestone A (the sum of Phases 0 to 5):** the user signs off, and the repository is at a clean checkpoint. Only then does Phase 6 begin.

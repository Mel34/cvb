# CVB parallel implementation plan: interactive surgery (v2) and overlays (instructions for implementers)

Written 2026-10-19 by Claude (Sonnet 5.5) with the user, against `cvb-0.7.0` at `d8fc688`. You have not seen the design conversations, so this document tells you what to do, in what order, who reviews whom, and where the two efforts must not collide. It sequences work that is specified elsewhere; it does not repeat that work.

Read, in this order: this plan; `claude/interactive-surgery.md` (revision 2, the surgery work in detail); `claude/cvb-context.md` (the text `cvb context` will print); `Overlays.md` (overlay architecture); `claude/surgery-implementation-guide.md` (sections 2 and 7 and Phase 6: reconciliation ledger R1 to R14 and decisions D1 to D11); `surgery-protocol.md`; `claude/review-remarks.md`. Where documents disagree: this plan decides order; `claude/interactive-surgery.md` decides surgery behaviour; the guide's decisions D1 to D11 decide overlay design until the user amends them.

The guide said Phase 6 (overlay milestones B to G) must not start until the user says so. **The user has now said so**, in the order given below. Earlier phases are done.

## 0. Ground rules (apply to every work package)

- One atomic change at a time. For each: write the failing test, show that it fails, make it pass, run the full checks (section 1), show the full diff, stop, and wait for the user's confirmation.
- Never weaken, delete or loosen an existing test. If an intended behaviour change makes an old test wrong, update it, say so, and keep its intent.
- Touch only the files a package names. Never run `cargo fmt` over the whole tree; format only what you touched.
- Never repair malformed input or guess. An ambiguity is a question for the user.
- Do not commit or push unless the user says so. Do not edit Project docs; list proposed amendments in your report.
- Rejected experiments must never alter the real working tree. Overlay and recovery code never deletes anything it did not create, and never recursively deletes a path while a mount is active on or under it.
- Report test counts before and after every package, and every existing test you changed with the reason.
- A package marked **SPIKE** is read-only plus measurement: report findings to the user and stop. No code until the user answers.

## 1. Baseline (measured on `cvb-0.7.0` at `d8fc688`)

| Check | Command | Result |
|---|---|---|
| Unit tests | `cargo test --locked` | 380 passed, 0 failed |
| Surgery CLI | `python3 scripts/test-surgery-cli.py` | 6 passed, 0 failed |
| PTY capture | `PATH="$PWD/target/debug:$PATH" python3 scripts/test-pty-capture.py` | 56 passed, 0 failed |
| Overlay lifecycle | `python3 scripts/test-overlay-lifecycle.py` (needs `fuse-overlayfs`) | 12 passed, 1 failed: "snapshot helper is implemented" (test written first; expected red) |
| Clippy | `cargo clippy --locked --all-targets` | 1 warning (`append_inventory_entry`, `src/main.rs`) |

Re-measure before you start and report first if your numbers differ. Build needs `libinput-dev`, `libudev-dev`, `libcanberra-dev`.

## 2. Tracks, owners and files

Two implementers, **A** and **B** (the user assigns who is who). They never grade their own work: every package is reviewed by the other implementer before the user sees it.

| Track | Scope | Files it may touch |
|---|---|---|
| **S** (surgery v2) | `claude/interactive-surgery.md` steps 1 to 6 | `src/surgery/parse.rs`, `mod.rs`, `token.rs`, `confirm.rs` (or new `keys.rs`, after asking), `src/pull.rs` (reminder only), `src/cli.rs`, `src/main.rs` (new subcommands), `manual.md`, `cvb.1`; steps 4 and 5 also `src/control.rs`, `src/pty.rs`, `src/clipboard.rs` |
| **O** (overlays) | Overlays Milestones B to G | `scripts/test-overlay-lifecycle.py`, new files under `scripts/`, a new `src/overlay/` module; `src/cli.rs` and `src/main.rs` only for subcommand dispatch |
| **H** (applier hardening) | package H1 below | `src/surgery/apply.rs`, `src/surgery/journal.rs` |

**Collision rules.**
- Only one track at a time may edit `src/pty.rs`, `src/control.rs` or `src/clipboard.rs`. Track S owns them for steps 4 and 5; track O (Milestone E) takes them afterwards, or earlier by the user's say-so.
- `src/cli.rs` and `src/main.rs` are shared. Keep edits there to the smallest dispatch change and rebase before each package.
- Track O builds its code in a new module so it does not collide with `src/surgery/`.

## 3. Packages and order

### Wave 0: before anything (both)

- Re-measure the baseline (section 1).
- Ask the user for the items in section 5 that block Wave 1.

### Wave 1 (in parallel)

**Implementer A: S1, then S2.** Exactly as specified in `claude/interactive-surgery.md`:
- **S1** opt-in `n=` counts (parser).
- **S2** session token, `CVB_TOKEN`, `E-NOSESSION` and `E-TOKEN`, the commands `cvb context`, `cvb spec`, `cvb token`, the launch clipboard, the reworded pull reminder, test-script updates. The text of `cvb context` is `claude/cvb-context.md`; its last section is what `cvb spec` prints. Embed it the way `manual.md` is embedded. `manual.md` is superseded by it and should be updated or retired as the user decides.

**Implementer B: O-B1 to O-B5, then H1, then S3 and the S4 spike.**

- **O-B1 snapshot helper.** Make the existing red test pass: define `snapshot_project(source, snapshot)` in `scripts/test-overlay-lifecycle.py`. Contract (already in the test): preserves modified and untracked files; excludes `target`; the copy is isolated (writing in it never changes the source). Decision from the user: the lower layer is a reflink snapshot of the tree without `target/`. Use `cp --reflink=auto` semantics and **never hardlinks** (a write through a hardlink changes the source). Add a test for the case where reflink is unsupported: it must fall back to a real copy, and say so.
- **O-B2 baseline manifest.** Record path, size, mtime and SHA-256 for every non-ignored source file at snapshot time, written outside the tree. Test: detects a modification, an addition and a deletion made in the base after the snapshot.
- **O-B3 lifecycle.** Using the snapshot as `lowerdir`, prove: the merged view shows the working tree including uncommitted and untracked work; commands run inside the merged view; modifications land in `upper`; the snapshot and the base stay byte-identical; discard returns to the unchanged baseline. No `sudo`. Run cargo inside the merged view with one shared `CARGO_TARGET_DIR` outside the tree (decision D3) and `--locked`.
- **O-B4 robustness.** Stale mount left by a crashed run is detected and cleaned up; unmount while busy; cleanup refuses symlinks and refuses any path outside the candidate directory; cleanup never deletes while a mount is active (add a test that proves the guard, using a fake mount marker).
- **O-B5 measurement report (the gate for Milestone C).** Measure and report: filesystem type and reflink support of the user's tree; snapshot time and disk use with and without reflink; time of a full `cargo build --locked` and `cargo test --locked` on a plain copy versus through `fuse-overlayfs`. The guide requires this: if a plain copy is as good as an overlay, report that to the user rather than silently dropping OverlayFS. Also report whether any test in the suite needs `.git` inside the candidate (`cvb pull` uses Git-tracked files, so pull tests probably do) and what including or excluding `.git` costs.
- **H1 applier durability** (details in section 4).
- **S3 key reader** (as specified; standalone, no proxy changes) and then the **S4 SPIKE**. The S4 spike must also list what overlay Milestone E will need from the proxy (candidate id in the prompt, candidate cvb with its own hotkey and XDG overrides), so the control messages are designed once for both uses.

**Review pairing in Wave 1:** B reviews A's S1 and S2; A reviews B's O-B1 to O-B5, H1 and S3.

### Gate G1 (the user decides)

Before Wave 2: S1 and S2 reviewed and accepted; the user can launch cvb and use `cvb context`; the O-B5 report is read and the overlay-or-copy decision made; the S4 spike report is answered; H1's open question (section 4) is answered.

### Wave 2 (in parallel)

**Implementer A: O-C1 SPIKE, then O-C (surgery against the candidate).**
- **O-C1 SPIKE.** Report, from the code and by experiment: where surgery's journal, lock and replay state live when `--root` points at a merged overlay directory (how they are keyed); whether the candidate's committed journals can serve as the promotion manifest (guide R6); whether renames, `fsync`, directory sync and the advisory lock behave on `fuse-overlayfs` on the user's machine. (In the sandbox where this plan was written, directory and file `fsync` and `flock` all worked on a `fuse-overlayfs` mount, but that is one kernel and one version.)
- **O-C.** Run the completed planner against the candidate via `--root <merged>` (with `CVB_TOKEN` set in the test environment). Confirm that every change stays in the overlay and the base is untouched, including after deliberate failures (stale file, precondition mismatch, crash hooks).

**Implementer B: S4 (implement), then S5 and S6.** As specified, after the user answers the spike. B reviews A's O-C; A reviews B's S4 and S5.

### Wave 3

- **O-D pre-verification runner** (Milestone D). Run `scripts/verify.sh`, `cargo test --locked`, `cargo build --locked` in the candidate; capture output and exit statuses; do not advance on failure; never promote build output or caches. Needs `scripts/verify.sh` from the user (section 5). Assign to A.
- Starting Milestone E waits for S5 to be accepted (the proxy is then free).

### Waves 4 to 6 (outline; the user will re-plan these)

- **Milestone E** (acceptance and rejection): candidate cvb with isolated XDG state, config and cache (never `XDG_RUNTIME_DIR`), its own hotkey, the candidate id in its prompt; recorded findings; explicit accept or reject; `keep/` always retained, `workspace/` discarded on reject unless `--keep` (decision D8).
- **Milestone F** (promotion): promotion by replay through the surgery applier (decision D6): net change per file from baseline digest to candidate bytes; abort the whole promotion on any mismatch before writing; unexpected source differences block promotion until approved per file (D7); additions, deletions and renames stay out of scope until protocol v2. Needs H1 done and tested first.
- **Milestone G**: realistic end-to-end runs including deliberate failures. `--yes` stays deferred until after G (D5).

## 4. Package H1: applier durability (implementer B, early)

**Why now.** Promotion (Milestone F) replays through the applier, and the 0.7.0 branch made journal directory-sync failures propagate while the applier still discards its own (`let _ = …sync_all()`, around `apply.rs` lines 390 and 577).

1. **SPIKE first.** List every place in `apply.rs` and `journal.rs` that discards or propagates a file or directory sync result. Classify each as before the journal's `committed` transition (a failure should roll back like any other I/O error) or after it (files are already renamed; the right report is "changes applied, durability not confirmed", not "commit failure"). Report and stop.
2. After the user answers: make the sync calls injectable (as `write_journal_atomic_with_dir_sync` already is), propagate or report per the classification, and write one test per call site. Change the 0.7.0 journal path so its message says the changes are applied but durability was not confirmed.
3. Run the applier tests against a `fuse-overlayfs` mount as well, since promotion and candidate surgery will meet that filesystem.

**Open question for the user (blocks step 2):** exit status and wording for "applied but durability not confirmed". Proposed: keep exit status 4, with a message that says plainly the files were written and recovery is not needed.

## 5. What the user must supply or decide

| Needed | By | Why |
|---|---|---|
| The POT overlay code (guide D9, still outstanding), or confirmation it is not needed because `fuse-overlayfs` is already proven by `scripts/test-overlay-lifecycle.py` | Wave 1 | D9 says reuse POT's mounting approach |
| `scripts/verify.sh` | Wave 3 (O-D) | pre-verification command set |
| Output of `stat -f -c %T .` in the project, and whether `cp --reflink=always` works there | Wave 1 (O-B5) | decides the snapshot strategy |
| Which implementer is A and which is B | Wave 0 | assignment |
| Answers listed in `claude/interactive-surgery.md`, section 3 | Wave 1 | command names (settled), manual size |
| H1 exit status and wording | before H1 step 2 | see section 4 |
| Overlay-or-copy decision | Gate G1 | from the O-B5 report |

## 6. What to report back (every package)

Test counts before and after; the full diff; the commands you ran and their output; every existing test you changed and why; anything you chose not to do; for spikes, findings only. If you could not run something (the PTY suite needs the system packages in section 1; the overlay tests need `fuse-overlayfs` and `/dev/fuse`), say so rather than reporting a pass.

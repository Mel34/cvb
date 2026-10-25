# CVB overlays: implementation plan v2 (on top of a completed surgery relocation v2.3)

Status: plan only, no code. Written 2026-10-24 by Claude (Sonnet 5.5) with the user. The user's decisions of the same day on ignore policy, candidate limit, command names, failed-workspace retention, the recovery command, ledger carry-over, the single-implementer process and the exit status for a sync warning after commit are part of the plan below. POT's overlay code (`github.com/Mel34/pot`) has been read; section 3.5 records what it contributes.

Supersedes: Phase 6 of `docs/04 QoL/surgery-implementation-guide.md` and the overlay waves and packages (O-B1 to O-B5, O-C1, O-C, O-D, Milestones E to G) of `docs/04 QoL/surgery-v2-and-overlays-plan.md`. Keeps unchanged: decisions D1 to D11 and ledger items R2, R3, R7, R8, R11 to R14 of the guide. Refines R4, R5 and R6 (section 3). The principles stay in `Overlays.md`.

Precedence: for anything about surgery behaviour, the v2.3 documents (`docs/04 QoL/design-surgery-relocation-v2.3-amendment-2-audit.md`, then `docs/04 QoL/design-surgery-relocation-v2.3-amendment-insert-anchors.md`, then `docs/04 QoL/design-surgery-relocation-v2.3.md`); for overlay decisions, D1 to D11; for order and packages, this plan; for principles, `Overlays.md`.

Premise: milestones 0 to 7 of `docs/04 QoL/handoff-surgery-relocation-v2.3.md` are implemented and merged. Milestone 6 (the edit ledger) is the only one that may be absent, because it was gated on chooser data. Nothing below depends on it.

Limit: I have not read the CVB source. Everything that depends on how the code is built is marked **verify** and is settled by the spike in section 5, not assumed.

## 1. What this plan relies on from v2.3

| v2.3 capability | How overlays use it | Verify |
|---|---|---|
| One diff renderer (unified diff, notes section, one escape scheme), shared by pager and clipboard; its inputs are original and planned bytes only | Candidate surgery review, and the promotion review of baseline versus candidate | The renderer can be called with two byte strings and no operation list |
| `project.lock` sidecar, final all-files re-hash before the first rename, per-file re-check, journal reaching `Committed`, `E-RECOVER` | Candidate surgeries commit under the candidate's own lock. Promotion commits to the real tree under the real project's lock | Where the lock path comes from (state directory), so that the candidate's XDG override gives it a separate lock and promotion can still open the real one explicitly |
| Resolver with relocation and chooser; relocation disabled when the applier replays mechanically | Candidate surgeries use relocation unchanged. Promotion never relocates | Whether "exact only" is an explicit mode or a flag on the resolver call, and whether the applier can take a ready-made plan without parsing an envelope |
| Post-apply line map | Printed after every candidate surgery, as in direct mode | None |
| Edit ledger (if built) | Lives in the candidate session, keyed by root and file | The key includes the root, so a pull of the merged tree and a pull of the real tree never share a chain |
| Journal retaining original bytes, committed journals as a manifest | Candidate journals are the provenance record for promotion (guide R6) | How journals are keyed under `--root` (section 5, spike S1) |
| Existing journal recovery classifier (original, planned or other per file) | Reused by the new `cvb surgery recover` command (package RC1) | The classifier is callable outside the commit path |
| Existing validator rules (`E-EOF`, `E-OVERLAP`, caps of 3 MiB, 8 MiB and 32 MiB) | Unchanged inside a candidate | None |

Known gap carried over from the audit: there is no user-facing recovery command for a `Prepared` journal, and an unfinished journal blocks further surgery. Promotion is the first workflow where a user can meet that state on the real tree after an interrupted run, so this plan closes the gap with package RC1 before promotion is used on real work.

## 2. What v2.3 changes about the overlay plan

1. **No separate commit-hardening track.** Locking, the final check and failure injection are v2.3 milestone 2. Overlays reuse them. The remaining question from the old package H1 (the applier discarding some file and directory sync results) is a prerequisite, checked in P1 below, because promotion's failure reporting depends on it.
2. **Surgery in a candidate is mostly verification.** Relocation, chooser, diff and journal work against any root. The work is proving it on a merged mount, and proving that nothing escapes the overlay.
3. **Promotion builds its plan directly.** It never produces an envelope or runs the resolver. It constructs a plan from baseline digests and candidate bytes and hands it to the applier. This is what makes "relocation disabled during replay" true by construction rather than by a flag.
4. **The chooser stays as designed in candidate mode.** A wrong pick would be caught later by the promotion diff and hands-on testing, which is an argument for softening it. It is not softened until chooser frequency data exists.
5. **Pulls inside a candidate read the merged tree.** Otherwise the pull baseline hash comes from the real tree and the ledger sees a mismatch on the first observation.
6. **Line numbers survive promotion, and the ledger is not carried over.** The LLM works against the candidate. After a successful promotion the real files are byte-identical to the candidate's, so the line numbers it holds are valid for the real tree. The promotion report says so and does not print a line map. The ledger is per session and in memory, and it is not handed from the candidate session to the main session in v1. A cross-process handover adds a channel for little value; it is reconsidered only if chooser data shows the ledger is worth having and the user routinely continues in the main session after promoting.

## 3. Operating model

### 3.1 Process model

- The orchestrator is the `cvb candidate` command family. "Candidate" is the concept; "overlay" is only the mechanism. Verbs: `new`, `enter`, `verify`, `run`, `status`, `accept`, `reject`, `promote`, `discard`, `list`. Shell tab completion (the planned QoL item) covers the length.
- `candidate enter` launches a candidate cvb session whose working root is the merged mount. It is a full cvb session, not a plain shell: it overrides `XDG_STATE_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` (never `XDG_RUNTIME_DIR`), uses its own hotkey from its own config, shows the candidate id in the prompt, and generates its own session token (D4). Surgery and pull in that session therefore act on the merged tree by default (**verify** root detection; otherwise pass `--root` explicitly).
- The surgery engine stays orchestration-free, and the orchestrator never reaches into its internals (guide section 1).
- One **active** candidate per project in v1. A second `candidate new` fails with `O-ACTIVE`. This keeps the baseline, the lock and promotion simple; two candidates are compared by running them one after the other, which reflink snapshots make cheap. A candidate stays active until it is promoted or discarded.
- A candidate does not survive a reboot or an unmount in v1. The journal key includes the device and inode of the merged root (guide R5), which a remount can change. After a reboot the candidate is `Orphaned`: it can be inspected, discarded or kept for diagnosis, not resumed. **Verify** the key behavior in spike S1.

### 3.2 Layout, retention and ignore policy (D8, R14)

```
<state>/cvb/candidates/<id>/
  record            hand-rolled text record: id, state, baseline digest of the project key, timestamps
  baseline.manifest path, size, SHA-256 of every non-ignored source file at snapshot time
  workspace/        snapshot/ upper/ work/ merged/ state/    (the engine's XDG state for this candidate)
  keep/             logs/ findings.md journals/ (copies) verify/ (command output and exit statuses)
<state>/cvb/candidates-target/   one shared CARGO_TARGET_DIR (D3), never inside a tree
```

Rules that every package obeys: each candidate uses a fresh mountpoint path that contains its id; the engine's state lives outside the merged tree and outside `upper` and `work`; cleanup deletes only inside `<id>/workspace/`, refuses symlinks and refuses any path outside it; cleanup never deletes anything while a mount is active on or under the path; the real tree's journal never records candidate ids. Digests, not mtimes, identify change (copy-up on an overlay changes timestamps).

**Retention.** `keep/` is always retained.
- A **rejected** candidate has its `workspace/` discarded by default. `--keep` retains it for diagnosis, and a kept workspace is pruned when the next candidate is created.
- A **failed** candidate (an engine or infrastructure fault: `E-IO`, `E-RECOVER`, a left-behind `Prepared` journal, or a verification runner failure) is unmounted but keeps its `workspace/` on disk by default. It is removed only by an explicit `candidate discard`, and it remains the active candidate until then, so it is never pruned silently. The reason is that a rejection says the change was bad, while a failure may mean a CVB bug, and the upper layer and journals are the evidence. Reflink snapshots keep the disk cost small.

**Ignore policy.** `.git/` is hard-coded as always excluded: it is never compared and never promoted (consistent with R8), and it is not configurable. A candidate ignore list at XDG config `cvb/candidate-ignore`, in the style of the capture skip list, names further excluded paths; its default content is `target/` only. Every other path that differs from the baseline and appears in no surgery journal is an unexpected source difference: it blocks promotion and is listed (OV8). `Cargo.lock` and `.cargo/` are deliberately not ignored, because with `--locked` they change only through a surgery, which has a journal. Editor swap files appear only if someone opens an editor inside the merged tree; they are added to the config if that happens. Ignore patterns only suppress the unexpected-difference check. They never cause a file to be promoted.

### 3.3 Lifecycle (Overlays section 11, refined)

`Prepared` (snapshot and mount ready) → `Editing` (one or more surgeries committed) → `Verified` (pre-verification passed) → `UnderTest` → `Accepted` or `Rejected` → `Promoted` or `Discarded`. `Failed` is a terminal state reachable from any step; it ends only by `discard`.

Mapping from surgery (R4, refined): the candidate becomes `Failed` on `E-IO`, `E-RECOVER` or a journal left `Prepared` in the candidate's state. `E-LOCK` and every other pre-commit rejection fail only that one attempt. A sync warning after `Committed` (exit status 5, see P1) is not a failure: the surgery counts as applied. A failed candidate is not repaired with `cvb surgery recover`; it is inspected and discarded, and its findings are kept. The `recover` command (RC1) is for the real tree, where an interrupted promotion or an ordinary interrupted surgery leaves a `Prepared` journal.

Acceptance and promotion are separate explicit commands (resolves the open item in Overlays section 14). The two gates stay as R3 says: the diff confirmation of each surgery, and the human acceptance of the whole candidate. `--yes` stays deferred until after Milestone G.

### 3.4 Error family (R13)

`O-ACTIVE`, `O-MOUNT`, `O-STALE-MOUNT`, `O-CLEANUP-REFUSED`, `O-VERIFY`, `O-NOTACCEPTED`, `O-DRIFT` (unexpected source difference or candidate bytes changed after surgery), `O-BASE-CHANGED` (a promoted file differs from its baseline digest), `O-PROMOTE-FAILED`. These are never mixed into `E-*`. Findings and logs record the surgery ids applied to the candidate.

### 3.5 What POT contributes (D9)

Read from `github.com/Mel34/pot` (`src/overlay.rs`, `src/helper.rs`, `src/bin/pot-helper.rs`, `src/kitchen.rs`, `src/worker.rs`, `data/org.archlinux.pot.helper.policy`).

**How POT mounts.** It uses the kernel's OverlayFS, mounted by a separate root helper, `pot-helper mount-overlay <lower> <overlay_root>`, started through `pkexec` under a polkit action that asks for admin authentication (`auth_admin_keep`). The overlay root holds `upper/`, `work/` and `merged/`, and the mount options are `lowerdir,upperdir,workdir,index=off`. This needs root. D9 and the ground rules forbid `sudo`, so CVB does not copy the privileged helper. The mount mechanism stays as D9 decided: `fuse-overlayfs`, started by the user, with `fusermount3 -u` to unmount (**verify** exact options in spike S1). A polkit helper like POT's remains an alternative only if `fuse-overlayfs` fails the S2 measurements.

**What CVB takes from POT, as rules for OV2 and OV3:**
- Validate overlay paths before mounting. POT rejects `,`, `:` and `\` in the lower, upper and work paths because OverlayFS options cannot express them, and has a test for it. CVB applies the same check to every path it passes to the mount command (**verify** whether `fuse-overlayfs` has the same limits).
- Refuse a suspicious overlay root: absolute, and at least three path components.
- Detect a mount by reading `/proc/self/mountinfo`, matching the mount point field, with the octal unescaping (`\040` for a space) and its test. Reuse that logic rather than shelling out to `mount`. For a `fuse-overlayfs` mount the filesystem type differs but the mount point field is the same.
- Before mounting, check for a stale mount at the same path and unmount it first, saying so.
- Own the mounted overlay in a value that unmounts when dropped, and keep that value outside the panic-catching scope so a panic cannot leak a mount. POT's worker does exactly this, with a comment explaining why.
- Unmount with one retry and a lazy-unmount fallback, so a straggling process cannot leave a stale mount behind (for `fuse-overlayfs`: `fusermount3 -u`, then `-uz`).
- Never change the lower layer while a mount is active. POT removes stale packages from its work directory before mounting for this reason, with a comment. CVB's lower layer is the reflink snapshot, which nothing writes after creation (D1).
- Failures in the unmount path are logged through a diagnostic channel and never printed over the terminal UI (POT's `diag.rs`); the candidate session needs the same.

**Where CVB must differ from POT:**
- POT's `mount-overlay` deletes stale `upper/` and `work/` with `remove_dir_all`, and `cleanup-overlay` does the same, without first checking that nothing is mounted. CVB's rule is stricter (section 3.2): never delete recursively while a mount is active on or under the path.
- POT's helper changes the owner of the new directories to the lower layer's owner because it runs as root. A user-run `fuse-overlayfs` needs none of that.
- POT's lower layer is a prepared work directory. CVB's is a reflink snapshot of the working tree, taken at `candidate new` (D1). POT uses `cp -a --reflink=auto` for copying git sources, which is the same reflink call CVB uses for the snapshot, but not as the overlay's lower layer.
- POT has one overlay per recipe. CVB has one active candidate per project, with ids in every mount path (R5).

## 4. Ground rules

The ground rules and the reporting format of `docs/04 QoL/surgery-v2-and-overlays-plan.md` (sections 0 and 6) still apply, except for its two-implementer arrangement, which is withdrawn: one atomic change at a time, failing test first, never weaken a test, touch only the files a package names, report test counts, no commits unless told, spikes report and stop. Added for overlays:

- **One implementer, one sequence.** ChatGPT implements, sequentially, one package at a time. There are no parallel tracks and no A and B roles. Each package is delivered as one commit that the user applies with `git am`, because sessions cannot push to `Mel34/cvb`.
- **Review.** The implementer reports and does not grade its own work. The user reviews each package, with Claude reviewing the diffs and reports through the Project documents. Gemini has no GitHub access, so anything for it is pasted in full.
- Rejected experiments must never alter the real working tree. Test this with a byte comparison of the whole non-ignored tree before and after, not by spot checks.
- No `sudo`. Mounting follows D9 and section 3.5.
- The lower layer is a reflink snapshot, never hardlinks (D1). When reflink is unavailable, fall back to a real copy and say so.
- Nothing in a test touches the user's real tree or the user's real state directory. Use temp directories with the process id in the name, and `XDG_*` overrides.
- Promotion never reads `upper/`, `work/` or whiteouts. It compares merged-view bytes to the baseline manifest.

## 5. Packages and order

The order below is strictly sequential. A package starts when the one before it is merged and confirmed.

### Wave 0: re-baseline and spikes (read-only, no code)

**W0.1 Re-measure.** Run the full suites on the current branch and report counts. Report which of the old packages O-B1 to O-B5 already exist (the old plan's overlay lifecycle script had one red test, "snapshot helper is implemented") and what `scripts/test-overlay-lifecycle.py` covers now. Do not redo anything that exists.

**W0.2 Spike S1: surgery against a merged root.** Report, from the code and by experiment on the user's machine:
1. How the journal, lock and replay state are keyed when the root is a merged mount, and what happens to the key after unmount and remount.
2. Whether the candidate's committed journals contain enough to serve as the promotion provenance record (path, original digest, planned digest, bytes, surgery id), and how to read them without the engine.
3. How project root and lock location are derived, and exactly what a promotion needs in order to take the real project's lock while running inside a candidate-overridden environment.
4. Whether `rename`, file and directory `fsync`, `flock` and the final re-hash behave on `fuse-overlayfs` here (the old plan saw them work in one sandbox).
5. Whether `pull` and the ledger work from the merged root with no real tree reference.
6. Whether the applier can take a ready-made plan, whether the renderer can take two byte strings, and whether the resolver has an explicit exact-only mode.
7. Whether the loader's file rules (UTF-8, NUL, BOM, lone CR, mixed endings, regular file, symlink components, caps, `.git` component) live in one reusable function.
8. Whether `.git` inside the snapshot is needed by any test in the suite (pull uses Git-tracked files), and the cost of including it.
9. Whether the existing journal recovery classifier can be called on its own, for RC1.
10. The exact `fuse-overlayfs` and `fusermount3` invocations that work for the user without privileges, and whether it has the same path-character limits as POT's kernel mount options (section 3.5).

**W0.3 Spike S2: overlay or copy (old O-B5), if not already reported.** Filesystem type and reflink support; snapshot time and disk use with and without reflink; full `cargo build --locked` and `cargo test --locked` times on a plain copy versus through `fuse-overlayfs`. If a plain copy is as good, say so (D1); do not drop OverlayFS silently. The user's probe results for this are still pending, so nothing in the plan is decided on it yet.

**P1 Applier sync check and the sync warning (old H1, scoped down).**
- *Check.* List every place in `apply.rs` and `journal.rs` where a file or directory sync result is discarded or propagated, classified as before or after the `Committed` transition. Report whether v2.3 milestone 2 already settled it.
- *Rule.* A sync failure before the `Committed` transition is an ordinary I/O error and rolls back like any other. A sync failure after it does not undo anything: the files are in place and verified by read-back, only the final flush to disk is unconfirmed. That case is reported as **applied with a warning, exit status 5**. The exit statuses become: 0 applied or dry-run OK, 2 rejected before commit (including `E-LOCK`, `E-RECOVER`), 3 declined by the user, 4 commit failure, 5 applied with a warning. Status 5 is never used for a failure, and status 4 is never used when the files changed.
- *Message,* calm and without recovery instructions, since nothing needs recovering:
  > Changes written to 2 files and verified by read-back. CVB could not confirm the flush to disk (`<error>`). Nothing needs recovery; running `sync` settles it.
- The same wording and status apply wherever the situation arises, including a promotion report. The ledger records the surgery as applied, and a candidate is not marked `Failed` for it (section 3.3).
- If v2.3 milestone 2 already implements this differently, report the difference instead of changing the behavior.

**Gate G0 (the user decides):** S1, S2 and P1 read; overlay-or-copy decided from the S2 data; the isolation requirements for CVB Master Draft 0.3 noted (every socket and path derives from an overridable base directory, D4).

### Wave 1: isolation layer (no surgery involved)

**OV1 Snapshot and baseline manifest.** `snapshot_project(source, snapshot)`: preserves modified and untracked files, excludes `target/`, includes `.git` if S1 item 8 says so, isolated from the source (writes never reach it). `baseline.manifest` records path, size and SHA-256 for every non-ignored file (section 3.2), written outside the tree. Tests: modification, addition and deletion in the base after the snapshot are detected; reflink-unsupported fallback is a real copy and says so; hardlinks are never used.

**OV2 Workspace manager.** `candidate new`, mount, unmount, `discard`, `list`, the candidate record and its state transitions, the retention rules of section 3.2, and the mount rules taken from POT in section 3.5. Tests: merged view shows uncommitted and untracked work; commands run in the merged view; changes land in `upper`; snapshot and base stay byte-identical; discard returns to an unchanged baseline; `O-ACTIVE` on a second candidate, including while a failed candidate is waiting for `discard`; a path containing `,`, `:` or `\` is refused before any mount is attempted; the mountinfo parser handles escaped spaces.

**OV3 Robustness.** A stale mount from a crashed run is detected (`O-STALE-MOUNT`) and cleaned up; unmount while busy falls back to the lazy form; a panic in the owning code does not leave a mount behind; cleanup refuses symlinks and paths outside the candidate directory; cleanup never deletes while a mount is active (prove the guard with a fake mount marker); orphaned candidates after a reboot are listed and discardable.

### Wave 2: surgery in a candidate

**OV4 Candidate session.** `candidate enter`: isolated XDG state, config and cache; own hotkey; id in the prompt; its own token; root and pull rooted at the merged tree. Needs the proxy files (`pty.rs`, `control.rs`, `clipboard.rs`). It starts only after the proxy changes of the surgery v2 track (S4 and S5 in the old plan) are merged, since the same files are involved and there are no parallel tracks. Prerequisite: the session-token and context commands from the surgery v2 track are merged (**verify**). The clipboard cannot be isolated, so automated tests use `--stdin`.

**OV5 Surgery-in-candidate conformance.** Run the finished engine against the merged root and prove: every change stays in the overlay and the base is byte-identical, including after deliberate failures (stale file, precondition mismatch, chooser abort, final-check abort, crash hooks at each point). A sequence of envelopes on one candidate, with relocation and the line map, works as in direct mode. The `Failed` mapping of section 3.3 fires on `E-RECOVER`, `E-IO` and a left-behind `Prepared` journal, and does not fire on exit status 5. Candidate journals are copied into `keep/journals/` on every committed surgery, not only at discard.

### Wave 3: pre-verification

**OV6 Verification runner.** Runs `scripts/verify.sh`, `cargo test --locked` and `cargo build --locked` from the merged root with the shared `CARGO_TARGET_DIR`; captures output and exit status into `keep/verify/`; a failing required check blocks advancement to `Verified`. Mutation-style checks run only inside the candidate. After the run, re-hash the candidate's source files and compare with the surgery journals' final digests: any difference is `O-DRIFT`, because verification must not change sources (this also catches a `verify.sh` that rewrites files). Needs `scripts/verify.sh` from the user.

### Wave 4: acceptance

**OV7 Hands-on and decision.** `candidate run` builds and launches the candidate binary and prints its identity (version, candidate id, binary digest) so it cannot be confused with the installed `cvb`. A `findings.md` template from Overlays section 7 lives in `keep/`. `accept` and `reject` are explicit and recorded. `reject` follows the retention rules of section 3.2 (workspace discarded unless `--keep`; `keep/` retained). Accept never promotes.

### Wave 5: recovery command and promotion

**RC1 `cvb surgery recover`.** Done first in this wave, before any promotion code, so it is already in place when OV10 exists. A thin wrapper over the existing journal classifier, usable on any real tree.
- `recover --status` is read-only. It prints the journal id and state and, per file, whether it is original, planned or other.
- `recover` applies the existing rules unchanged: roll back or forward when the files are uniformly in one state, restore planned files from retained originals when the state is mixed, and never overwrite unknown content (that file is reported as a conflict and the transaction stays `Prepared`). It asks for confirmation and refuses without a terminal (`E-NOTTY`). There is no `--force`.
- Tests: each classifier outcome reached through the command, a refused run without a terminal, unknown content left untouched, and recovery after each crash point of the existing failure-injection tests.

Promotion works on the baseline-to-candidate difference of merged-view bytes and never reads the overlay's upper layer.

**OV8 Manifest builder (pure, no writes).**
- Truth comes from bytes: compare every non-ignored file in the merged view with `baseline.manifest`.
- Provenance comes from journals: a file that differs and appears in some committed candidate journal is a **promotable file**; the journals' original digest must equal the baseline manifest digest and consecutive steps must chain (post digest of step k equals pre digest of step k+1), otherwise `O-DRIFT`.
- A file that differs from baseline but appears in no journal is an **unexpected source difference** (D7). A file touched by journals whose bytes equal the baseline (changed, then reverted) is a net no-op and is omitted.
- Candidate bytes must equal the last planned digest in the journals, otherwise `O-DRIFT`.
- `.git` and the paths in the ignore policy (section 3.2) are excluded from comparison.
- In v1 an unexpected addition, deletion, rename, mode change or symlink cannot be promoted at all (guide R6); it blocks promotion unless the path is covered by the ignore policy. An unexpected modification is promoted only after explicit per-file approval, shown in the reviewed diff (D7).

**OV9 Conflict check and plan construction.**
- For every promotable file, the real tree's current bytes must equal the baseline manifest digest, otherwise `O-BASE-CHANGED`, nothing written (Overlays section 9: abort rather than reconcile).
- A real-tree change in a file that is not being promoted is informational ("base changed elsewhere since the baseline: N files"), never blocking (D1 allows editing the real tree during an experiment).
- Because promotion bypasses the planner, the planner's file rules are re-applied to the candidate bytes: valid UTF-8, no NUL, no leading BOM, no lone CR, no mixed line endings, regular file, no symlink components, no `.git` component, caps of 8 MiB per file and 32 MiB aggregate, path rules. Reuse the single loader function from S1 item 7; do not copy the rules.
- Build the plan directly from baseline digest, planned bytes and planned digest. No envelope, no hints, no resolver.

**OV10 Review and commit.**
- Render the baseline-to-candidate diff with the v2.3 renderer, including the D7 approvals and the surgery ids as provenance. Space applies, `q` or Ctrl+C aborts. Non-interactive: `E-NOTTY`. `--dry-run`: print the plan, write nothing. No `--yes`.
- Review runs without the lock. Then take the **real project's** lock (not the candidate's), re-hash every promoted file against the plan (final check), and commit through the applier with id `promote-<candidate-id>`; v2.3's per-file re-check and rollback apply as they do for any surgery. Hold the lock for the commit only.
- Result messages: success states that the promoted files are byte-identical to the candidate's, so the LLM's line numbers remain valid and the main session needs a fresh pull only if its ledger reports a mismatch. A sync failure after `Committed` is reported as exit status 5 with the P1 message, and the candidate counts as promoted. A failure before that leaves the candidate `Accepted` and not promoted, with `keep/` intact. An interrupted promotion leaves a `Prepared` journal on the real tree, which is cleared with `cvb surgery recover` (RC1).
- Residual risk, to document in the manual: the real tree is edited by non-cooperating writers (editors) during an experiment, so the narrowed-not-closed race from v2.3 amendment 2 B3 matters most here. The hash checks abort cleanly on drift detected before the renames; a change after the final check and before a file's rename can still lose that other writer's edit.

### Wave 6: end to end

**OV11 Milestone G.** Starts only when RC1 is merged and P1 is settled. Realistic CVB changes through the whole lifecycle, with deliberate failures (including an interrupted promotion recovered with RC1), proving both outcomes: accepted candidates promote and the real files equal the candidate's; rejected candidates leave the baseline unchanged (whole-tree byte comparison). `--yes` stays deferred until the user decides after this wave. Update `manual.md`, `cvb.1` and the context text for the candidate commands, for `cvb surgery recover` and for exit status 5, and add the residual-race paragraph.

## 6. Inputs still needed from the user

| Needed | By | Why |
|---|---|---|
| Probe results for filesystem type and reflink support (and the S2 measurements) | Gate G0 | decides overlay or copy (D1); not decided yet |
| `scripts/verify.sh` | OV6 | the pre-verification command set |

## 7. Deferred (do not build)

`--yes`; multiple simultaneous candidates; resuming a candidate after remount or reboot; promotion of additions, deletions, renames, mode changes and symlinks (until surgery protocol v2); reading the overlay upper layer; clever conflict reconciliation; ledger carry-over across promotion; softening the chooser in candidate mode; a view tag; a privileged (polkit) mount helper unless `fuse-overlayfs` fails the S2 measurements.

## 8. Definition of done

Each package: failing tests first, then green; full suites and `cargo clippy` clean; counts reported; reviewed by the user and Claude; confirmed by the user. The plan is done when Milestone G runs both outcomes on a realistic change, the whole-tree byte comparison proves a rejected candidate left nothing behind, a promoted candidate's files are byte-identical to the candidate's, and an interrupted promotion has been recovered with `cvb surgery recover`.

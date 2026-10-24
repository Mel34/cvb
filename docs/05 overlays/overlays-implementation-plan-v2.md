# CVB overlays: implementation plan v2 (on top of a completed surgery relocation v2.3)

Status: plan only, no code. Written 2026-10-24 by Claude (Sonnet 5.5) with the user. Revised the same day: document references use the repository archive path `docs/04 QoL/`, and section 6 now carries recommended answers.

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
| Existing validator rules (`E-EOF`, `E-OVERLAP`, caps of 3 MiB, 8 MiB and 32 MiB) | Unchanged inside a candidate | None |

Known gap carried over from the audit: there is no user-facing recovery command for a `Prepared` journal, and an unfinished journal blocks further surgery. Promotion is the first workflow where a user can meet that state on the real tree after an interrupted run. See gate G1.

## 2. What v2.3 changes about the overlay plan

1. **No separate commit-hardening track.** Locking, the final check and failure injection are v2.3 milestone 2. Overlays reuse them. The remaining durability question from the old package H1 (the applier discarding some file and directory sync results) is a prerequisite, checked in P1 below, because promotion's failure reporting depends on it.
2. **Surgery in a candidate is mostly verification.** Relocation, chooser, diff and journal work against any root. The work is proving it on a merged mount, and proving that nothing escapes the overlay.
3. **Promotion builds its plan directly.** It never produces an envelope or runs the resolver. It constructs a plan from baseline digests and candidate bytes and hands it to the applier. This is what makes "relocation disabled during replay" true by construction rather than by a flag.
4. **The chooser stays as designed in candidate mode.** A wrong pick would be caught later by the promotion diff and hands-on testing, which is an argument for softening it. Do not soften it until chooser frequency data exists.
5. **Pulls inside a candidate read the merged tree.** Otherwise the pull baseline hash comes from the real tree and the ledger sees a mismatch on the first observation.
6. **Line numbers survive promotion.** The LLM works against the candidate. After a successful promotion the real files are byte-identical to the candidate's, so the line numbers it holds are valid for the real tree. The promotion report says so and does not print a line map.

## 3. Operating model

### 3.1 Process model

- The orchestrator is a set of `cvb candidate` subcommands (names are a proposal, gate G0): `new`, `shell`, `verify`, `run`, `status`, `accept`, `reject`, `promote`, `discard`, `list`. See Q3 for a recommended rename of `shell`.
- `candidate shell` launches a candidate cvb session whose working root is the merged mount. It overrides `XDG_STATE_HOME`, `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` (never `XDG_RUNTIME_DIR`), uses its own hotkey from its own config, shows the candidate id in the prompt, and generates its own session token (D4). Surgery and pull in that session therefore act on the merged tree by default (**verify** root detection; otherwise pass `--root` explicitly).
- The surgery engine stays orchestration-free, and the orchestrator never reaches into its internals (guide section 1).
- One **active** candidate per project in v1. A second `candidate new` fails with `O-ACTIVE`. This keeps promotion conflicts and lock reasoning simple. Revisit after Milestone G.
- A candidate does not survive a reboot or an unmount in v1. The journal key includes the device and inode of the merged root (guide R5), which a remount can change. After a reboot the candidate is `Orphaned`: it can be inspected, discarded or kept for diagnosis, not resumed. **Verify** the key behavior in spike S1.

### 3.2 Layout (D8, R14)

```
<state>/cvb/candidates/<id>/
  record            hand-rolled text record: id, state, baseline digest of the project key, timestamps
  baseline.manifest path, size, SHA-256 of every non-ignored source file at snapshot time
  workspace/        snapshot/ upper/ work/ merged/ state/    (the engine's XDG state for this candidate)
  keep/             logs/ findings.md journals/ (copies) verify/ (command output and exit statuses)
<state>/cvb/candidates-target/   one shared CARGO_TARGET_DIR (D3), never inside a tree
```

Rules that every package obeys: each candidate uses a fresh mountpoint path that contains its id; the engine's state lives outside the merged tree and outside `upper` and `work`; cleanup deletes only inside `<id>/workspace/`, refuses symlinks and refuses any path outside it; cleanup never deletes anything while a mount is active on or under the path; the real tree's journal never records candidate ids. Digests, not mtimes, identify change (copy-up on an overlay changes timestamps).

### 3.3 Lifecycle (Overlays section 11, refined)

`Prepared` (snapshot and mount ready) → `Editing` (one or more surgeries committed) → `Verified` (pre-verification passed) → `UnderTest` → `Accepted` or `Rejected` → `Promoted` or `Discarded`. `Failed` is a terminal state reachable from any step.

Mapping from surgery (R4, refined): the candidate becomes `Failed` on `E-IO`, `E-RECOVER` or a journal left `Prepared` in the candidate's state. `E-LOCK` and every other pre-commit rejection fail only that one attempt. A failed candidate is discarded and its findings kept; `cvb surgery recover` is not part of the overlay flow.

Acceptance and promotion are separate explicit commands (resolves the open item in Overlays section 14). The two gates stay as R3 says: the diff confirmation of each surgery, and the human acceptance of the whole candidate. `--yes` stays deferred until after Milestone G.

### 3.4 Error family (R13)

`O-ACTIVE`, `O-MOUNT`, `O-STALE-MOUNT`, `O-CLEANUP-REFUSED`, `O-VERIFY`, `O-NOTACCEPTED`, `O-DRIFT` (unexpected source difference or candidate bytes changed after surgery), `O-BASE-CHANGED` (a promoted file differs from its baseline digest), `O-PROMOTE-FAILED`. These are never mixed into `E-*`. Findings and logs record the surgery ids applied to the candidate.

## 4. Ground rules

The ground rules and the reporting format of `docs/04 QoL/surgery-v2-and-overlays-plan.md` (sections 0 and 6) still apply: one atomic change at a time, failing test first, never weaken a test, touch only the files a package names, report test counts, no commits unless told, spikes report and stop. Added for overlays:

- Rejected experiments must never alter the real working tree. Test this with a byte comparison of the whole non-ignored tree before and after, not by spot checks.
- No `sudo`. Mounting follows D9 (reuse what POT does, prefer `fuse-overlayfs`).
- The lower layer is a reflink snapshot, never hardlinks (D1). When reflink is unavailable, fall back to a real copy and say so.
- Nothing in a test touches the user's real tree or the user's real state directory. Use temp directories with the process id in the name, and `XDG_*` overrides.
- Promotion never reads `upper/`, `work/` or whiteouts. It compares merged-view bytes to the baseline manifest.
- Reviewers: the two implementers never grade their own work, as before.

## 5. Packages and order

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

**W0.3 Spike S2: overlay or copy (old O-B5), if not already reported.** Filesystem type and reflink support; snapshot time and disk use with and without reflink; full `cargo build --locked` and `cargo test --locked` times on a plain copy versus through `fuse-overlayfs`. If a plain copy is as good, say so (D1); do not drop OverlayFS silently.

**P1 Applier durability check (old H1, scoped down).** List every place in `apply.rs` and `journal.rs` where a file or directory sync result is discarded or propagated, classified as before or after the `Committed` transition. Report whether v2.3 milestone 2 already settled it. If not, it is a prerequisite for OV10, with the old open question (exit status and wording for "applied, durability not confirmed", proposed exit 4 with plain wording).

**Gate G0 (the user decides):** S1, S2 and P1 read; overlay-or-copy decided; command names; one-active-candidate; the POT overlay code supplied or declared unnecessary (D9); the candidate ignore policy (section 6, Q1); the isolation requirements for CVB Master Draft 0.3 noted (every socket and path derives from an overridable base directory, D4).

### Wave 1: isolation layer (no surgery involved)

**OV1 Snapshot and baseline manifest.** `snapshot_project(source, snapshot)`: preserves modified and untracked files, excludes `target/`, includes `.git` if S1 item 8 says so, isolated from the source (writes never reach it). `baseline.manifest` records path, size and SHA-256 for every non-ignored file, written outside the tree. Tests: modification, addition and deletion in the base after the snapshot are detected; reflink-unsupported fallback is a real copy and says so; hardlinks are never used.

**OV2 Workspace manager.** `candidate new`, mount, unmount, `discard`, `list`, the candidate record and its state transitions. Tests: merged view shows uncommitted and untracked work; commands run in the merged view; changes land in `upper`; snapshot and base stay byte-identical; discard returns to an unchanged baseline; `O-ACTIVE` on a second candidate.

**OV3 Robustness.** A stale mount from a crashed run is detected (`O-STALE-MOUNT`) and cleaned up; unmount while busy; cleanup refuses symlinks and paths outside the candidate directory; cleanup never deletes while a mount is active (prove the guard with a fake mount marker); orphaned candidates after a reboot are listed and discardable.

### Wave 2: surgery in a candidate

**OV4 Candidate session.** `candidate shell`: isolated XDG state, config and cache; own hotkey; id in the prompt; its own token; root and pull rooted at the merged tree. Needs the proxy files (`pty.rs`, `control.rs`, `clipboard.rs`); only one track may edit them at a time (collision rule from the old plan). Prerequisite: the session-token and context commands from the surgery v2 track are merged (**verify**). The clipboard cannot be isolated, so automated tests use `--stdin`.

**OV5 Surgery-in-candidate conformance.** Run the finished engine against the merged root and prove: every change stays in the overlay and the base is byte-identical, including after deliberate failures (stale file, precondition mismatch, chooser abort, final-check abort, crash hooks at each point). A sequence of envelopes on one candidate, with relocation and the line map, works as in direct mode. The `Failed` mapping of section 3.3 fires on `E-RECOVER`, `E-IO` and a left-behind `Prepared` journal. Candidate journals are copied into `keep/journals/` on every committed surgery, not only at discard.

### Wave 3: pre-verification

**OV6 Verification runner.** Runs `scripts/verify.sh`, `cargo test --locked` and `cargo build --locked` from the merged root with the shared `CARGO_TARGET_DIR`; captures output and exit status into `keep/verify/`; a failing required check blocks advancement to `Verified`. Mutation-style checks run only inside the candidate. After the run, re-hash the candidate's source files and compare with the surgery journals' final digests: any difference is `O-DRIFT`, because verification must not change sources (this also catches a `verify.sh` that rewrites files). Needs `scripts/verify.sh` from the user.

### Wave 4: acceptance

**OV7 Hands-on and decision.** `candidate run` builds and launches the candidate binary and prints its identity (version, candidate id, binary digest) so it cannot be confused with the installed `cvb`. A `findings.md` template from Overlays section 7 lives in `keep/`. `accept` and `reject` are explicit and recorded; `reject` removes `workspace/` and keeps `keep/`; `--keep` retains a rejected workspace for diagnosis, and it is pruned when the next candidate is created. Accept never promotes. Retention of a `Failed` workspace: see Q4.

### Wave 5: promotion

Promotion works on the baseline-to-candidate difference of merged-view bytes and never reads the overlay's upper layer.

**OV8 Manifest builder (pure, no writes).**
- Truth comes from bytes: compare every non-ignored file in the merged view with `baseline.manifest`.
- Provenance comes from journals: a file that differs and appears in some committed candidate journal is a **promotable file**; the journals' original digest must equal the baseline manifest digest and consecutive steps must chain (post digest of step k equals pre digest of step k+1), otherwise `O-DRIFT`.
- A file that differs from baseline but appears in no journal is an **unexpected source difference** (D7). A file touched by journals whose bytes equal the baseline (changed, then reverted) is a net no-op and is omitted.
- Candidate bytes must equal the last planned digest in the journals, otherwise `O-DRIFT`.
- `.git`, the shared target directory and the ignore policy (Q1) are excluded from comparison.
- In v1 an unexpected addition, deletion, rename, mode change or symlink cannot be promoted at all (guide R6); it blocks promotion unless the path is covered by the ignore policy. An unexpected modification is promoted only after explicit per-file approval, shown in the reviewed diff (D7).

**OV9 Conflict check and plan construction.**
- For every promotable file, the real tree's current bytes must equal the baseline manifest digest, otherwise `O-BASE-CHANGED`, nothing written (Overlays section 9: abort rather than reconcile).
- A real-tree change in a file that is not being promoted is informational ("base changed elsewhere since the baseline: N files"), never blocking (D1 allows editing the real tree during an experiment).
- Because promotion bypasses the planner, the planner's file rules are re-applied to the candidate bytes: valid UTF-8, no NUL, no leading BOM, no lone CR, no mixed line endings, regular file, no symlink components, no `.git` component, caps of 8 MiB per file and 32 MiB aggregate, path rules. Reuse the single loader function from S1 item 7; do not copy the rules.
- Build the plan directly from baseline digest, planned bytes and planned digest. No envelope, no hints, no resolver.

**OV10 Review and commit.**
- Render the baseline-to-candidate diff with the v2.3 renderer, including the D7 approvals and the surgery ids as provenance. Space applies, `q` or Ctrl+C aborts. Non-interactive: `E-NOTTY`. `--dry-run`: print the plan, write nothing. No `--yes`.
- Review runs without the lock. Then take the **real project's** lock (not the candidate's), re-hash every promoted file against the plan (final check), and commit through the applier with id `promote-<candidate-id>`; v2.3's per-file re-check and rollback apply as they do for any surgery. Hold the lock for the commit only.
- Result messages: success states that the promoted files are byte-identical to the candidate's, so the LLM's line numbers remain valid and the main session needs a fresh pull only if its ledger reports a mismatch. A failure leaves the candidate `Accepted` and not promoted, with `keep/` intact. An interrupted promotion is handled by the same journal recovery as any surgery.
- Residual risk, to document in the manual: the real tree is edited by non-cooperating writers (editors) during an experiment, so the narrowed-not-closed race from v2.3 amendment 2 B3 matters most here. The hash checks abort cleanly on drift detected before the renames; a change after the final check and before a file's rename can still lose that other writer's edit.

**Gate G1 (before Wave 6):** the user has decided how a `Prepared` journal on the real tree is cleared after an interrupted promotion. The audit found no user-facing command. Either ship `cvb surgery recover` (guide R4, deferred in the v2.3 handoff) before promotion is used on real work, or document the manual procedure and accept it. P1 must be settled. See Q5 for the recommended scope of the command.

### Wave 6: end to end

**OV11 Milestone G.** Realistic CVB changes through the whole lifecycle, with deliberate failures, proving both outcomes: accepted candidates promote and the real files equal the candidate's; rejected candidates leave the baseline unchanged (whole-tree byte comparison). `--yes` stays deferred until the user decides after this wave. Update `manual.md`, `cvb.1` and the context text for the candidate commands, and add the residual-race paragraph.

## 6. Open questions, with recommended answers

Each recommendation is Claude's, pending the user's decision. Where adopted, the body text above is amended accordingly.

- **Q1. Ignore policy.** Which paths in the merged view are never compared or promoted besides `.git` and `target/`?
  - **Recommendation:** hard-code `.git/` (not configurable; it is never compared and never promoted, consistent with R8). Add a candidate ignore list at XDG config `cvb/candidate-ignore`, in the style of the capture skip list, whose default content is `target/` only. Every other path that differs from baseline and appears in no surgery journal is an unexpected source difference: it blocks promotion and is listed.
  - **Why:** a false block costs one visible message and one config line. A false ignore silently loses or promotes junk, which is the failure the whole workflow exists to prevent. `Cargo.lock` and `.cargo/` are deliberately not ignored: with `--locked` they change only through a surgery, and that has a journal. Editor swap files appear only if someone opens an editor inside the merged tree; add them to the config if that happens. Ignore patterns only suppress the unexpected-difference check. They never cause a file to be promoted.
- **Q2. One active candidate per project in v1.**
  - **Recommendation:** accept. One baseline, one lock, one promotion at a time. The cost is that two candidates cannot be compared side by side, and reflink snapshots are cheap enough to run them one after the other. Revisit after Milestone G.
- **Q3. Command names.**
  - **Recommendation:** keep `cvb candidate <verb>` ("candidate" is the concept; "overlay" is only the mechanism), with one change: rename `shell` to `enter`, because it starts a full cvb session with its own hotkey, token and prompt, not a plain shell. Verbs: `new`, `enter`, `verify`, `run`, `status`, `accept`, `reject`, `promote`, `discard`, `list`. The length is handled by the planned bash tab completion.
- **Q4. Retention of a `Failed` workspace.**
  - **Recommendation:** treat the two cases differently. `Rejected`: discard `workspace/` by default and keep `keep/`; `--keep` retains it (D8). `Failed` (an engine or infrastructure fault such as `E-IO`, `E-RECOVER`, a left-behind `Prepared` journal, or a verification runner failure): unmount, but retain `workspace/` on disk by default, and have it removed only by an explicit `candidate discard`. A failed candidate stays the active one until then, which fits `O-ACTIVE` and means it is never pruned silently.
  - **Why:** a rejected candidate says the change was bad; a failed one says CVB may have a bug, and its upper layer and journal are the evidence. With reflink snapshots the disk cost is small, and `keep/` already holds copies of the journals and logs.
- **Q5. Recovery command (gate G1).**
  - **Recommendation:** ship `cvb surgery recover` before promotion is used on real work, as a thin wrapper over the existing journal classifier. Scope: `recover --status` is read-only and prints the journal id and state and, per file, original, planned or other; `recover` runs the existing rules (roll back or forward when files are uniformly one state, restore planned files from retained originals when mixed, never overwrite unknown content), asks for confirmation, and refuses without a terminal. No `--force`. Schedule it as a package in wave 5 that can run in parallel with OV8 and OV9, since it depends only on the journal.
  - **Why:** an interrupted promotion leaves `E-RECOVER` blocking all surgery on the real tree, and without the command the only way out is editing a journal by hand. Direct mode needs it for the same reason, and the classifier already exists, so the cost is small.
- **Q6. Ledger carry-over across promotion.**
  - **Recommendation:** no carry-over in v1 (section 2, item 6). The ledger is per session and in memory, and promoted files are byte-identical to the candidate's, so the LLM's numbers stay valid. A cross-process handover adds a channel for little value. Reconsider only if the chooser data shows the ledger is worth having and you routinely continue in the main session after promoting.

## 7. Deferred (do not build)

`--yes`; multiple simultaneous candidates; resuming a candidate after remount or reboot; promotion of additions, deletions, renames, mode changes and symlinks (until surgery protocol v2); reading the overlay upper layer; clever conflict reconciliation; ledger carry-over across promotion; softening the chooser in candidate mode; a view tag.

## 8. Definition of done

Each package: failing tests first, then green; full suites and `cargo clippy` clean; counts reported; reviewed by the other implementer; confirmed by the user. The plan is done when Milestone G runs both outcomes on a realistic change, the whole-tree byte comparison proves a rejected candidate left nothing behind, and a promoted candidate's files are byte-identical to the candidate's.

# CVB: Transactional Surgery and Human-Gated Verification

**Status:** Architectural guide — agreed direction, implementation pending
**Project:** CVB (Capture, View, Bridge)
**Prerequisite:** Complete and verify the surgery planner against `surgery-protocol.md`

## 1. Purpose

CVB surgery should evolve beyond a tool that applies edits directly to project files. The intended workflow will let us propose source changes, build and test them in isolation, exercise the resulting binary, and promote the changes to the real working tree only after successful human acceptance.

The central principle is:

**A proposed change must earn its way into the working tree. Automated checks establish readiness; hands-on adversarial testing establishes acceptance; explicit promotion makes the change permanent.**

The original project files must remain untouched throughout preparation, automated pre-verification, and hands-on verification. If the candidate fails, we should be able to discard the experiment and return to the exact baseline from which it began.

## 2. Architectural overview

The intended workflow is:

1. Establish the baseline.
2. Create an isolated OverlayFS workspace.
3. Apply surgery to the candidate workspace.
4. Run automated pre-verification.
5. Build and exercise the candidate binary.
6. Record findings and decide whether to accept or reject.
7. Promote accepted changes, or discard rejected changes.

Conceptually:

`Baseline → Isolated candidate → Surgery → Pre-verification → Hands-on verification → Accept or reject`

The two outcomes are:

* **Accept:** safely promote the approved candidate changes to the base working tree.
* **Reject:** preserve the findings, discard the candidate workspace, and return to the unchanged baseline.

Automated success alone must never trigger promotion.

## 3. Phase 1 — Establish the baseline

Before creating the candidate, CVB must establish exactly which project state is being tested.

The baseline is the actual working tree, including existing uncommitted changes—not merely the current Git commit.

Requirements:

* Preserve all pre-existing tracked and untracked work.
* Record enough baseline information to detect changes that occur while the candidate is being tested.
* Never assume that Git can safely restore the original state; the user may have work that is not committed.
* Treat the baseline as immutable for the duration of the experiment.

The isolation mechanism must protect the baseline from both surgery and commands executed during verification.

## 4. Phase 2 — Create the isolated candidate

Use OverlayFS to provide a candidate filesystem based on the baseline.

The expected arrangement is:

* **Lower layer:** the baseline filesystem.
* **Upper layer:** candidate modifications and filesystem changes.
* **Merged view:** the project tree visible to surgery, the compiler, tests, and the user running the candidate.

OverlayFS has already been experimented with successfully in the POT project. CVB should build on that experience rather than inventing a separate isolation mechanism without reason.

The candidate workspace must have a clearly defined lifecycle. It must be possible to identify it, use it, accept it, reject it, and clean it up without ambiguity.

Important: OverlayFS provides filesystem isolation, but it does not automatically make promotion transactional. That must be designed separately.

## 5. Phase 3 — Apply surgery to the candidate

Once the isolated workspace is ready, apply `cvb surgery` to the merged candidate filesystem, not the real base tree.

The surgery planner remains responsible for its existing domain:

* Parse and validate the surgery protocol.
* Validate operation ranges, preconditions, boundaries, and conflicts.
* Enforce file-integrity and path-safety rules.
* Produce the proposed file contents.
* Report failures without partially modifying targets.

The orchestration layer is responsible for preparing the workspace and deciding what happens after surgery.

**Keep these responsibilities separate.** The planner should remain deterministic and independently testable. It should not become responsible for mounting OverlayFS, running Cargo, collecting acceptance findings, or promoting files.

A failed surgery operation must not damage the baseline. Whether the failed candidate is retained for diagnosis or discarded should be an explicit lifecycle decision.

## 6. Phase 4 — Automated pre-verification

After surgery succeeds, run the automated checks against the candidate's merged filesystem.

For the CVB project, the initial checks should include the applicable project verification suite, Rust tests, and build checks—for example:

* `scripts/verify.sh`
* `cargo test`
* `cargo build`

The precise command set and ordering should be finalized against the project's actual verification workflow. These commands are examples of the intended checks, not a claim that every command must always be run separately.

Requirements:

* Execute checks from the candidate project root.
* Capture command output, logs, and exit statuses.
* Do not advance if a required check fails.
* Ensure the checks operate on the same candidate that will later be promoted.
* Avoid accidentally promoting build artifacts, caches, or unrelated filesystem changes.

This phase answers:

**Is the candidate ready for hands-on testing?**

It does not answer whether the candidate is accepted.

## 7. Phase 5 — Hands-on and adversarial verification

Once pre-verification passes, build and run the candidate binary in a way that makes it clear which version is being exercised.

Petar takes the binary for a spin. Petar and Chadgpt then work together to challenge the implementation and try to break it.

This is an acceptance phase, not simply another automated test run.

We should investigate:

* Expected behavior and regressions.
* Edge cases and unusual inputs.
* Unexpected interactions with the environment.
* Failure handling and recovery.
* Behaviors that the automated tests do not cover.
* Any discrepancy between the intended design and actual user experience.

The candidate remains isolated throughout this phase. Running it must not silently change the base project.

### Findings and reproduction notes

Whenever we find a defect, record enough information to reproduce and understand it:

* What we tried.
* What we expected.
* What actually happened.
* Steps to reproduce the problem.
* Relevant output or logs, where available.
* Whether the behavior appears to be a regression or is still undetermined.
* Any questions or design decisions raised by the finding.

The findings become input to the next development iteration.

If verification fails, we take notes, return to the drawing board, and prepare a new candidate from the unchanged baseline.

We should preserve the findings even if the failed overlay is discarded. Retaining a failed candidate for further diagnosis can be useful, but it must not be confused with accepting or promoting it.

This phase answers:

**Does the candidate behave well enough for us to accept it?**

## 8. Phase 6 — Explicit acceptance or rejection

The workflow must have an unambiguous acceptance decision.

### Accepted

Acceptance requires an explicit decision after hands-on testing. Passing tests, completing a build, or reaching the end of a script is not sufficient.

Once accepted, the workflow advances to promotion.

### Rejected

If a defect is found or the candidate is otherwise unacceptable:

1. Record the findings.
2. Preserve any useful diagnostic output.
3. Reject the candidate.
4. Discard its filesystem changes.
5. Return to the original baseline.
6. Use the findings to guide the next attempt.

The intended outcome is that a failed experiment leaves no source modifications behind in the base working tree.

The acceptance decision must remain separate from the mechanics of applying changes to the base.

## 9. Phase 7 — Safe promotion

Promotion is the point at which the approved candidate becomes real working-tree state.

This phase deserves special care. Copying files directly out of an OverlayFS upper directory is not a reliable general-purpose merge strategy. OverlayFS may represent deletions and other changes through special metadata, and the upper layer can contain generated files that are not part of the intended source change.

Promotion should operate on the verified difference between the baseline and the candidate's merged view.

Requirements:

* Promote only approved changes.
* Account correctly for additions, modifications, deletions, and any other supported change types.
* Exclude build outputs, caches, and unrelated modifications unless explicitly approved.
* Detect if the base has changed since the baseline was established.
* Abort promotion if concurrent changes make the result unsafe to apply.
* Avoid overwriting new work or silently resolving conflicts.
* Handle promotion failures without leaving an unexplained partially applied result.

The candidate diff must be reviewed or validated against a clearly defined policy before promotion.

For the initial implementation, **abort on baseline conflicts rather than attempting clever automatic reconciliation**. Safety and predictability are more important than maximizing the number of changes that can be promoted automatically.

Promotion is a distinct operation from verification. A candidate can be accepted but still require intervention if the base has changed or promotion cannot proceed safely.

## 10. Component responsibilities

Keep the architecture modular.

| Component           | Responsibility                                                                   |
| ------------------- | -------------------------------------------------------------------------------- |
| Surgery planner     | Parse, validate, and calculate proposed edits according to `surgery-protocol.md` |
| Workspace manager   | Create, identify, mount, and clean up the isolated candidate                     |
| Verification runner | Execute pre-verification commands against the candidate and collect logs         |
| Acceptance workflow | Support hands-on testing and record findings and the final decision              |
| Promotion mechanism | Validate and apply approved candidate differences safely                         |
| Discard mechanism   | Remove candidate changes without damaging the baseline                           |

These may initially be coordinated by one command or script. They should nevertheless have distinct responsibilities and explicit failure behavior.

## 11. Lifecycle and failure handling

The workflow should make its state visible and unambiguous.

Suggested lifecycle states:

1. **Baseline established**
2. **Candidate prepared**
3. **Surgery applied**
4. **Pre-verification passed**
5. **Under hands-on verification**
6. **Accepted or rejected**
7. **Promoted or discarded**

Failures at any stage must have a defined outcome.

In particular:

* A failed surgery must not affect the baseline.
* A failed automated check must prevent advancement to hands-on acceptance.
* A failed hands-on test must permit rejection and rollback.
* A rejected candidate must not be promoted.
* A conflict discovered during promotion must abort the operation safely.
* Cleanup must not delete baseline data or unrelated user files.

Logs and findings should survive candidate disposal where they are needed to understand the failure.

## 12. Implementation strategy

Do not combine the entire architecture into the surgery planner while it is still being completed.

Proceed incrementally:

### Milestone A — Complete the surgery planner

Finish conformance with `surgery-protocol.md`. Add tests for genuine coverage gaps, fix defects exposed by those tests, run the complete verification suite, inspect the diff, and establish a clean checkpoint.

### Milestone B — Prototype the OverlayFS lifecycle

Independently prove that CVB can:

* Establish a baseline that includes existing working-tree changes.
* Create an isolated candidate.
* Modify files in the candidate without changing the baseline.
* Run commands inside the merged view.
* Discard the candidate and recover the unchanged baseline.

Test this lifecycle before coupling it to surgery.

### Milestone C — Integrate surgery

Run the completed planner against the isolated candidate. Confirm that all resulting source modifications remain in the overlay.

### Milestone D — Integrate pre-verification

Run the required tests and build checks inside the candidate. Capture diagnostics and enforce the transition from preparation to hands-on verification.

### Milestone E — Implement acceptance and rejection

Support the human-led test cycle, explicit acceptance, recorded findings, and safe candidate disposal.

### Milestone F — Implement and test promotion

Develop the candidate-diff and promotion mechanism separately. Test modifications, additions, deletions, conflicts, concurrent base changes, and interrupted or failed promotion before relying on it for real work.

### Milestone G — Exercise the complete workflow

Run realistic CVB changes through the entire lifecycle, including deliberate failures. Confirm both outcomes:

* Accepted candidates can be promoted safely.
* Rejected candidates leave the baseline unchanged.

## 13. Design principles

The following principles guide implementation decisions:

1. **Isolation before modification.** Never perform experimental surgery on the real base tree.
2. **Preserve existing work.** The baseline includes the user's actual working-tree state, not just a Git commit.
3. **Verify the real candidate.** Test the same merged filesystem that will be promoted.
4. **Automated checks are necessary, not sufficient.** A passing build is readiness, not acceptance.
5. **Human-gated promotion.** Only an explicit acceptance decision can authorize promotion.
6. **Failure must be recoverable.** Rejection must restore the original baseline without requiring manual source repair.
7. **Preserve findings.** Failed experiments should teach us something even when their candidate work is discarded.
8. **Separate concerns.** Planning, isolation, verification, acceptance, and promotion should not become one inseparable operation.
9. **Prefer conservative conflict handling.** Abort rather than overwrite concurrent work.
10. **Keep the implementation incremental.** Prove each subsystem independently before connecting it to the next.

## 14. Decisions still to be finalized

This guide records the agreed direction. The following details should be resolved during implementation rather than guessed in advance:

* The exact OverlayFS mount layout, storage locations, permissions, and cleanup behavior.
* How to launch the candidate binary and guarantee its identity.
* The final pre-verification command set.
* How logs and findings are named, stored, and associated with a candidate.
* Whether failed candidates are discarded immediately or can be retained for diagnosis.
* Which filesystem changes are eligible for promotion.
* How baseline identity and concurrent changes are detected.
* How promotion handles filesystem metadata, permissions, symlinks, and interruption.
* Whether acceptance and promotion initially require separate explicit commands.
* What guarantees are required if promotion fails partway through.

These decisions must preserve the central safety contract: no rejected experiment should alter the baseline, and no accepted candidate should overwrite changes made outside the experiment.

## 15. Definition of success

The architecture is successful when Petar and Chadgpt can propose a change through CVB surgery, test and build it in an isolated filesystem, exercise the resulting binary, and make an informed acceptance decision.

If the candidate fails, we retain the findings and discard the experiment without repairing the original tree.

If the candidate succeeds, we promote only the approved changes after a final safety check.

**The result is a transactional, human-gated development workflow: propose, isolate, test, challenge, accept or reject, and then promote or discard.**

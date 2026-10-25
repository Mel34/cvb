# CVB Surgery Relocation — Milestones 4, 5, and 6
## Decisions and completion guide

**Project:** `Mel34/cvb`, branch `cvb-0.7.0`  
**Design reference:** `docs/04 QoL/design-surgery-relocation-v2.3.md`  
**Purpose:** Record implementation decisions and guide the remaining work across Milestones 4–6.

## 1. Governing principle

Our implementation and explicitly agreed decisions are the source of truth. Claude's design document is a reference, not an authority that overrides a better implementation we have chosen.

The goal is to build a safe, practical surgery-relocation workflow. We should preserve sound existing behavior and avoid introducing changes merely to match wording in the design document.

Work incrementally:
- Inspect before editing.
- Make one atomic change at a time.
- Explain the gap and proposed fix before preparing a CVB surgery envelope.
- Review the dry run before applying.
- Run focused tests, then the full verification suite.
- After successful verification, commit and push without an additional confirmation request.

## 2. Milestone 4 — Resolver and ambiguity chooser

### 2.1 Preserve the interactive design

The interactive ambiguity chooser is intentional and should be retained.

When an expected block has multiple exact matches, the user must be able to inspect the candidates and explicitly choose the correct occurrence. A suggested candidate is a convenience, not an automatic resolution.

Do not change the workflow merely to reproduce the design document's separate non-interactive/dry-run behavior. Do not introduce a new non-interactive resolution policy unless we independently identify a concrete need and explicitly decide to implement it.

### 2.2 Keep candidate discovery exact

The resolver must search the entire original file snapshot for exact matches of the complete expected block. Overlapping matches count as distinct candidates.

A unique exact match may be selected automatically. Multiple exact matches require a human choice under the current design.

The resolver must not silently treat the stated line hint as proof that a particular occurrence is correct. Hint freshness is not established merely because the stated line matches an exact candidate.

### 2.3 Candidate ordering: defer the final tie-break

We identified a possible gap between the current file-order candidate list and the design's distance-from-hint ordering. However, **we will not finalize the equal-distance tie-break before Milestone 6 provides ledger evidence**.

The ledger may provide useful evidence about the direction of prior localized activity around the hinted location. That could help us prefer a more plausible candidate when multiple candidates are equally distant from the hint.

Until the ledger is implemented and its information is understood:
- Do not encode an arbitrary permanent preference for the earlier or later line.
- Preserve exact candidate identity; overlapping matches must remain distinct.
- Avoid treating a provisional ordering rule as a final relocation policy.
- If ordering work is undertaken before Milestone 6, keep it separable from the future directional preference and do not prematurely settle the tie-break.

After Milestone 6, revisit candidate ordering and suggested preselection together, using the actual ledger data and mapping semantics.

### 2.4 Candidate preselection

The chooser's initial selection and its suggested-candidate marker must reflect a deliberate policy.

Current behavior falls back to candidate index zero when the hint is absent. We should not silently select the first candidate merely because the hint is missing.

For now, the baseline rule is:
- If the hint is an exact candidate, it may be used as the initial suggestion.
- If the hint is not among the candidates, do not silently designate the first candidate as though it were supported by the hint.
- Once ledger evidence exists, consider using it to guide the suggestion.
- A suggestion must never bypass the user's explicit choice.

The exact UI representation of “no candidate suggested” should be decided after inspecting the chooser state model and its constraints.

### 2.5 Candidate context and excessive matches

The chooser should make candidates easy to distinguish without requiring the user to search manually through a long file listing.

The intended experience is:
- Show candidate line numbers.
- Show two lines of surrounding context for each candidate.
- Keep candidate selection and the current suggestion visually distinct.
- Preserve navigation and cancellation behavior.

When the number of exact matches exceeds the chooser's supported limit of five, the user should receive a specific, actionable diagnostic:

`anchor too generic: N exact matches; quote more context`

Do not let an internal chooser-construction error become the user-facing explanation.

### 2.6 Identical expected blocks

The design reference specifies rejecting two operations in the same envelope when their expected blocks are identical and at least one requires relocation, with an error naming both operations.

Before implementing this, inspect the current resolver and tests to establish whether this rule already exists or conflicts with an intentional implementation decision. If it remains an agreed requirement, add a focused test for the rejection and its diagnostic.

### 2.7 Milestone 4 completion criteria

- Exact candidate discovery and overlapping-match behavior are tested.
- Multiple matches require explicit user selection.
- Candidate preselection does not silently choose candidate one when the hint is absent.
- Candidate context is useful and easy to compare.
- More-than-five cases produce the intended actionable diagnostic.
- The identical-expected-block rule is implemented and tested if confirmed as part of our contract.
- Resolver behavior remains based on the original snapshot, with joint validation before the plan is applied.
- No arbitrary equal-distance tie-break is committed ahead of ledger-informed design.

## 3. Milestone 5 — Preserve the resolved-plan safety contract

Milestone 5 work must preserve the separation between resolution, validation, review, and application.

Before making changes, inspect the milestone definition and current implementation to determine its exact scope. Do not infer missing requirements from milestone numbering alone.

The following are invariants to preserve wherever the milestone touches these areas:

- All operations resolve against one original file snapshot.
- A candidate choice identifies a specific exact match.
- The selected candidate must be validated; an invalid or stale choice must fail rather than be silently substituted.
- The complete operation set must be jointly validated before constructing the final proposed file.
- The reviewed diff and the applied plan must be the same resolved plan. Do not resolve a second time after review in a way that can change the bytes being committed.
- Cancellation, failed resolution, or failed validation must not write the target file.
- Existing restrictions and conflict checks must not be loosened accidentally while implementing relocation.

Add focused tests for the actual Milestone 5 requirements once the source audit establishes the remaining gaps. Avoid duplicating existing coverage without a reason.

## 4. Milestone 6 — Ledger and evidence-informed relocation

Milestone 6 is important not only as a feature milestone but as the source of evidence that may improve relocation suggestions.

### 4.1 Ledger as evidence, not proof

Use the ledger's documented hash chain, edit mapping, and validity rules. A ledger mapping is a locator, never proof that a particular occurrence is correct.

A mapped location must still pass exact-content verification. A ledger must not automatically resolve duplicate candidates merely because one candidate appears to correspond to a translated hint.

A broken or unavailable ledger must not be treated as valid evidence.

### 4.2 Use localized activity carefully

Our working hypothesis is that prior surgery activity is usually localized. Therefore, edits recorded around a hint may help distinguish plausible candidates when simple distance from the hint is insufficient.

After inspecting the implemented ledger, determine what it can actually establish:
- Whether the ledger is valid for the current file snapshot.
- Whether prior edits affect the hinted block or nearby coordinates.
- How the mapping behaves when edits occur before, after, or inside the relevant region.
- Whether the available information supports a directional preference for one candidate over another.

Do not equate the raw number of operations with the amount of displacement. A large edit and many small edits can have different effects. Use the actual edit footprints and mapping semantics.

### 4.3 Keep suggestion separate from resolution

Ledger-informed behavior may improve:
- Candidate ordering.
- Which candidate is initially suggested.
- Explanations of why a candidate is suggested.

It must not silently replace the human decision when multiple exact matches remain and hint freshness is unknown.

If the ledger supplies no defensible directional evidence, fall back to a deterministic, transparent behavior without pretending it is more certain than it is.

### 4.4 Decide the tie-break only after inspecting the ledger

Once Milestone 6 is implemented and tested, return to the deferred equal-distance decision.

The design should consider:
1. The hint's relationship to the exact candidates.
2. The ledger's valid coordinate mapping and recorded edits.
3. Whether activity is concentrated before or after the hinted region.
4. Whether that evidence meaningfully favors a candidate.
5. A deterministic fallback when evidence is absent, ambiguous, or invalid.

Document the policy and test it against examples where edits occurred predominantly before the hint, predominantly after it, on both sides, and inside or across the relevant region. The latter cases may make directional inference unreliable.

## 5. Recommended execution order

1. Finish the Milestone 4 source and test audit.
2. Implement confirmed, ledger-independent chooser fixes as separate atomic changes.
3. Avoid finalizing equal-distance candidate preference.
4. Audit and complete Milestone 5 according to its actual requirements, preserving the resolved-plan safety contract.
5. Implement and test Milestone 6's ledger behavior.
6. Revisit candidate ordering and preselection using real ledger semantics.
7. Run the full suite and all required checks.
8. Commit and push after successful verification.

## 6. Working rules for future sessions

- Use `cvb pull` and `cvb pull callers:NAME` for local source and call-site inspection when working through the CVB workflow.
- Inspect the exact current source and tests before proposing an envelope.
- Use one standalone CVB surgery envelope per atomic change; omit optional `n=` counts unless necessary.
- Dry-run first, review the result, then apply.
- Do not claim an operation was applied until the tool output and source confirm it.
- Avoid unrelated cleanup and speculative refactoring.
- After a successful full verification, commit and push automatically.

## Final decision summary

**Milestone 4:** Improve the chooser without changing the intentional interactive resolution model.

**Milestone 5:** Preserve the safety contract around candidate selection, joint validation, review, and application; establish the exact remaining work from source before editing.

**Milestone 6:** Implement trustworthy ledger mapping and use it as evidence to improve suggestions. Then revisit equal-distance ordering and directional preselection. The ledger informs the human choice; it does not replace it.


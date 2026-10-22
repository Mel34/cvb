# Amendment to design v2.3: insert anchor representation

Applies to `claude/design-surgery-relocation-v2.3.md` and the milestone 4 text of `claude/handoff-surgery-relocation-v2.3.md`. Raised by the milestone 0 audit. Not a new full revision; the sections below replace the matching v2.3 text.

## The discrepancy

v2.3 describes an insert as having a "before anchor" and an "after anchor" that form one adjacent unit, searched as a concatenated block. The existing protocol, per the audit, represents an insert as one contiguous anchor range plus a placement (before or after that range). These are different contracts. v2.3 assumed the first because of how its author read the original decision wording ("exact before/after anchor lines, multiple allowed"); the source implements the second. The source is the established contract, and relocation must not silently change the insert semantics. This amendment restates the relevant design rules under the implemented representation.

## A1. Representation (replaces the insert wording in 2.2 and 2.4)

An insert op is:
- one contiguous anchor block `A` of one or more lines, verified byte for byte (the "multiple allowed" lines);
- a placement, `before` or `after`;
- the payload.

The insertion point `P` is a boundary between lines. With `A` occupying lines `a..b`: placement `before` gives `P` between line `a - 1` and `a`; placement `after` gives `P` between `b` and `b + 1`. In ledger terms the edit is `(start = P's following line, old_len = 0, new_len = payload lines)`, so the mapping rules in 2.3 are unchanged.

## A2. Resolution of insert ops

Resolve the anchor block `A` exactly like any other expected block: `C` is every whole-file exact match of `A`; the decision table in 2.4 applies unchanged (`|C| = 0` abort, `|C| = 1` use, `|C| > 1` choose-required in v1). The placement is preserved when the anchor is relocated. After a candidate is chosen, `P` is computed from that candidate and the placement. The diff label names the anchor lines and the resulting point, for example `relocated: anchor hint L120 -> L134, insert after`.

The sentence "if the pair no longer adjoins anywhere, abort" and the concatenated-block search no longer apply: there is no second anchor.

## A3. Footprints and same-boundary inserts (replaces the corresponding rules in 2.2)

- The footprint of an insert is its anchor block `a..b`. Anchor lines are read-only within the envelope: an edit (replace or delete) whose range intersects another insert's anchor lines is rejected.
- Same-boundary rule, restated: two inserts whose resolved insertion points `P` are the same boundary are rejected, naming both ops. This must be computed from the resolved `P`, not from the anchors, because different anchors can give the same boundary (insert after line 10 and insert before line 11). An insert after `A` and an insert before `A` give different boundaries and are not rejected by this rule.
- Insert next to a replaced or deleted range: an insert after line 10 and a replacement of line 11 share a boundary but not a line. The result is deterministic (inserted text precedes the replacement). Whether such a combination is accepted is decided by what the existing validator does today. The audit must report which combinations it accepts and rejects. Relocation must not loosen any existing restriction and must not add one that did not exist, except the rules already adopted in the design (same-boundary insert rejection, identical-block abort).
- Overlap between two read-only anchors (for example `insert before A` and `insert after A`): follow the existing validator; the audit reports it.
- File boundaries: v1 rejects inserts at the start or end of a file. The audit confirms what the source rejects today and why, since a single-anchor `before` the first line or `after` the last line is expressible in this representation. Keep the existing rejection.

## A4. Property that is weaker than v2.3, stated openly

v2.3's two-sided anchors would have verified the lines on both sides of the insertion point. A single anchor verifies only one side. If an earlier surgery inserted lines immediately after `A`, a later "insert after A" lands directly after `A`, before those lines, with the line on the far side unchecked. The diff review still shows context lines on both sides, so the reviewer sees what is adjacent. This property is inherent in the existing protocol and is not changed by this work. An optional second anchor (verified neighbor) is a possible later protocol extension, deferred and not part of this plan.

## A5. Test changes (4.2 and 4.3)

Remove: "Insert with multi-line anchors, still adjacent: relocated as a unit" and "Insert anchors no longer adjoin: rejected".

Add:
- Multi-line contiguous anchor relocates as one block, and placement `before` and `after` are both preserved after relocation.
- Repeated anchor block: chooser required; the chosen candidate plus placement determines `P`.
- Same boundary from different anchors (insert after line 10, insert before line 11): rejected, both ops named.
- Insert before `A` and insert after `A` in one envelope: not rejected by the same-boundary rule (subject to the existing validator).
- Insert next to a replaced range: matches the existing validator's behavior.
- Edit whose range intersects an insert's anchor: rejected.
- Start-of-file and end-of-file inserts: existing rejection preserved.
- Ledger: an insert contributes `(P, 0, n)`; mapping tests unchanged.

## A6. Milestone 4 handoff text

Replace "Insert anchors resolve as one adjacent unit" with "Insert ops resolve their single contiguous anchor block, preserve the placement (before or after), and compute the insertion point from the chosen candidate." Replace the footprint sentence with A3. Everything else in milestone 4 stands.

## Open question for the owner

Confirm that the implemented representation (one contiguous anchor range plus a before or after placement) is the intended v1 contract. If the intent was two-sided verification (a line on each side of the insertion point), that is a protocol change and a separate decision.

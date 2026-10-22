# CVB Continuation — Surgery Planner, Section 3.5

## Where we left off

We're working in `~/Projects/cvb`, branch `cvb-0.5.0-ps0`, on `src/surgery.rs` and the authoritative `surgery-protocol.md`.

The last committed checkpoint is `39366bf` (`Fix surgery edit ordering at shared boundaries`). Since then, we added one uncommitted regression test:

`allows_insertions_with_overlapping_anchors_at_distinct_gaps`

It verifies that insert anchors may overlap other insert anchors when they target distinct gaps. The focused test passed, and the user reported full verification: **131 tests passed**, `cargo check` succeeded with 26 pre-existing dead-code warnings, and `git diff --check` passed. The uncommitted diff should contain only that test. Do not commit without permission.

## Next task: Section 3.5 — File format and content rules

Resume the surgery planner work after fixing `cvb pull`.

Section 3.5 requires:

1. Reject mixed LF/CRLF endings and lone CR characters with `E-FILE`. Files without terminators are treated as LF.
2. Exclude line terminators from payload comparisons and storage, while preserving the file's original line-ending convention in output.
3. Preserve final-newline state exactly, including when the final line is replaced or deleted.
4. Reject UTF-8 BOMs with `E-FILE`.
5. Reject invalid UTF-8 with `E-FILE`.
6. Reject NUL bytes with `E-FILE`.

First inspect the actual `src/surgery.rs` implementation and existing tests using the repaired `cvb pull`. Identify the first uncovered or incorrectly handled rule, add a focused regression test, establish its result, then make the smallest necessary implementation change. Validate after each atomic change.

## Workflow and collaboration rules

* Fix `cvb pull` first; then use it to retrieve the relevant source context and reinitialize the assistant's context from this handoff.
* `cvb pull` currently joins all arguments into one search pattern. We agreed it should instead treat multiple arguments as independent search terms, matching **any** term (OR semantics), while retaining matching-line output and Rust-function expansion. Avoid duplicate function output. Handle `rg` and `grep` consistently.
* Use one atomic change at a time, with a separate validation command immediately after each edit.
* Prefer surgical Python scripts in complete, executable terminal commands.
* Do not fall back to manual `grep` when `cvb pull` can do the retrieval.
* Don't claim tests passed without actual output.
* Inspect the resulting diff before declaring a change complete.
* Don't stage or commit unless explicitly asked.

When cvb pull is fixed, use it to retrieve src/surgery.rs around the file-format parsing and validation logic, along with the relevant tests. Then continue from section 3.5—no need to revisit the already-tested insertion-boundary rules.


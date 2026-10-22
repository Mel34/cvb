# surgery.rs: review fixes (instructions for the implementer)

You are fixing issues found in a review of `surgery.rs` in the CVB project (Rust). Work through the steps below in order. Each step is one atomic change with its own tests.

## Ground rules

- Do one step at a time. After each step, all existing tests must pass (`cargo test`) before you move on. Show the full diff for the step, then stop and wait for confirmation.
- Tests first: for every bug fix, write the failing test, show that it fails, then fix.
- Do not change behavior beyond what a step describes. The v1 rules stay as they are: existing files only, replace/delete/insert, mandatory exact preconditions, no inserts before the first or after the last line, all-or-nothing, line endings and final newline preserved, mixed endings rejected.
- Touch only `surgery.rs` (and, in step 7, the new files it is split into). Do not touch `main.rs`, `pull.rs` or other files.
- Do not weaken or delete an existing test to make a step pass. If a test is wrong, fix its fixture and explain why.
- If anything below is ambiguous, ask instead of guessing.

---

## Step 1. Bug: unbounded allocation from declared counts

**Problem.** `read_payload` calls `Vec::with_capacity(count)` where `count` comes straight from the paste. `NEW n=99999999999` is only checked for `!= 0`, and an `OLD n=` with a matching huge `lines=1-99999999999` passes the range check. Both reach the allocation and cause a capacity-overflow panic or allocation abort instead of a `ParseError`. If surgery runs inside the cvb process that owns the PTY, this can take the session down.

**Fix.** At the top of `read_payload`, before allocating, return `ParseError::Truncated` if `count > lines.len() - *index` (use `saturating_sub` if `*index` can exceed `lines.len()`). Keep the per-line loop as is. Either keep `with_capacity` (now bounded) or drop it.

**Tests to add** (each must return an `Err`, never panic):
- `NEW n=18446744073709551615` in a replace.
- `OLD n=` huge with matching `lines=1-<huge>` in a replace, in a delete, and `ANCHOR n=` in an insert.
- `NEW n=` larger than the remaining lines but small (e.g. `n=5` with 2 lines left) gives `Truncated`.

---

## Step 2. Fix test fixtures that pass for the wrong reason

These are escaping slips. Fix the fixtures; do not change the assertions unless noted.

1. `rejects_insertion_before_first_line` (in `plan_envelope_tests`): the fixture `"one\\ntwo\\nthree\\n"` is a literal backslash-n, so the file is a single line and the test only passes because the boundary check fires before the precondition check. Change it to `"one\ntwo\nthree\n"`.
2. `malformed_final_end_line_is_truncated`, `malformed_end_line_before_more_input_is_syntax`, `malformed_final_endop_is_truncated`: each input starts with `"\\` followed by a newline, which puts a stray `\` line before BEGIN. Change the start to `"\` (line continuation) like the other tests.
3. `malformed_begin_header_is_syntax_not_no_envelope`: the input `"CVB::k7Q2xw9R::BEGIN \\n"` contains a literal backslash-n. Change it to a header with an empty field list, `"CVB::k7Q2xw9R::BEGIN \n"` (real newline), and confirm it still returns `Syntax`.
4. Optional naming fixes: `mibibytes` should be `mebibytes`. `accepts_input_at_four_mebibytes_before_normalization` asserts `NoEnvelope`, so rename it to say what it checks (for example `input_at_limit_passes_size_check`).

After fixing, confirm each affected test still passes **for the intended reason**. For item 1, add a second test, `rejects_insertion_before_first_line_with_mismatched_anchor`, only if it helps show the order of checks. Do not add it otherwise.

---

## Step 3. Refactor: `apply_operation` becomes a non-cloning validator

**Problem.** `plan_envelope` calls `apply_operation` only to validate and discards the result, then re-implements the splicing bottom-up. So the `applies_*_in_memory` tests cover code that never produces real output, and every call does `original.to_vec()`, cloning every line.

**Fix.**
- Replace `apply_operation` with `validate_operation(original: &[String], operation: &Operation) -> Result<(), PlanError>`. It does the same checks (range, precondition, no-op replace, whole-file delete, end-of-file inserts, empty `new`) with no cloning and no splicing.
- Remove the duplicate `insertion_at_boundary` block in `plan_envelope`. `validate_operation` already returns `EndOfFile`. Keep the error priority the same as today (end-of-file check before the precondition check is the current behavior in `plan_envelope`, so preserve it inside `validate_operation`, and write a test that pins the order).
- Make `plan_envelope` the only place edits are applied.
- Convert the `apply_operation_tests` into tests of `validate_operation` (errors) plus, where they used to check output, tests through `plan_envelope` (output). No coverage may be lost: every old assertion must still exist in some form.
- Add one test with a large file (e.g. 50,000 lines) and 500 non-overlapping replace ops to show planning completes quickly (no timing assertion; just make sure it finishes).

---

## Step 4. Deduplicate validation

**Problem.** Path validation exists in `parse_file`, `load_originals` and `plan_envelope`. Duplicate-file detection exists in all three. Line-ending validation is implemented twice with different algorithms. The duplicate check in `load_originals` is unreachable from the parser and untested.

**Fix.**
- One `fn validate_relative_path(path: &str) -> Result<(), ()>` (or returning a small error) used by all three. `parse_file` keeps its stricter character whitelist as an extra check on top.
- **Decided by the user: reject any path with a `.git` component** (e.g. `.git/hooks/pre-commit`, `.git/config`, `sub/.git/hooks/x`, or a bare `.git`), because hooks are code execution. Put this rule in `validate_relative_path` so the parser, loader and planner all enforce it, compare the component case-insensitively (`eq_ignore_ascii_case(".git")`), and report it as `InvalidPath`. Do not reject look-alikes: `.gitignore`, `.gitattributes`, `.github/workflows/ci.yml` and `src/git.rs` must stay valid. Tests: one rejected case per example above, one accepted case per look-alike, and one end-to-end test through `parse_load_plan` showing a `.git/hooks/` target is rejected with no file touched.
- One `fn detect_newline(contents: &str) -> Result<Newline, NewlineError>` (LF or CRLF; rejects lone CR and mixed endings), used by both `load_originals` and `plan_envelope`.
- Keep the defensive checks in `load_originals` and `plan_envelope` only if each has a test that reaches it directly (construct an `Envelope` by hand). Otherwise remove it and say so.
- Existing tests must pass unchanged.

---

## Step 5. Introduce a `Parser` struct

**Problem.** `lines`, `index` and `token` are threaded through every parse function, and the `CVB::{token}::` prefix is `format!`ed again on every payload line. `parse_replace`, `parse_delete` and `parse_insert` repeat the same OLD/NEW/ANCHOR header handling.

**Fix.**
- `struct Parser<'a> { lines: &'a [&'a str], index: usize, token: &'a str, prefix: String }` with methods such as `peek`, `next_line`, `expect_header(name) -> Result<usize, ParseError>` (parses `CVB::<token>::<NAME> n=<count>`), `read_payload(count)`, `consume_endop`.
- Build the prefix once. No `format!` inside per-line loops.
- `parse_replace`, `parse_delete` and `parse_insert` share the header helpers. Remove the unneeded lifetime annotations on `read_payload`.
- Behavior and error kinds must not change. All existing tests pass unchanged.
- Keep the public functions the tests call (`parse_envelope`, `parse_begin`, `parse_file`, `parse_count`, `parse_range`, `valid_token`) with the same signatures. If a private function the tests call directly (for example `read_payload`) moves into the struct, update those tests minimally.

---

## Step 6. Error context

**Why.** The reader of these errors is an LLM that must repair its own paste, plus the user. A bare `Syntax` or `Precondition` gives it nothing to act on.

**Design constraints.**
- Keep the existing `ParseError` and `PlanError` enums as the *kind*. Do not delete variants, so the existing assertions about kinds remain meaningful.
- Wrap them with location data, for example:
  - `ParseFailure { kind: ParseError, line: usize /* 1-based, in the pasted input */, detail: Option<String> }`
  - `PlanFailure { kind: PlanError, path: String, op_index: usize /* 1-based within the file section */, detail: Option<PlanDetail> }`
- Add `impl Display` (and `std::error::Error`) for the new types. The `Display` output is what the user and the LLM will read. It must be plain text, one problem per message, and name the file, operation and line.
- Provide a test helper (e.g. `fn kind(r: Result<..>) -> Result<.., ParseError>`) so existing kind assertions stay short.

**Required content.**
- Parse errors: the 1-based line number in the input where parsing failed, and for `Count` the declared and expected counts.
- `Precondition`: file path, operation number, the 1-based file line of the first mismatching line, the expected line and the found line (both shown with visible markers for trailing whitespace and tabs, e.g. escaped or quoted).
- When the mismatching line differs **only** in trailing whitespace, say so explicitly ("differs only in trailing whitespace").
- `EndOfFile`: the message must say what to do instead: include the first or last line in a replace operation rather than inserting before/after it.
- `Overlap`: name the two operation numbers involved.
- `Range`: say the file's actual line count.
- Messages must never include more than a few lines of file content; truncate long lines (e.g. at 200 characters) with an ellipsis.

**Tests.** For each required content item, one test that checks the rendered message contains the specific facts (path, op number, line numbers, the whitespace-only remark). Existing tests keep passing via the helper.

---

## Step 7. File organization

- Split into a module directory: `surgery/mod.rs` (public types and `parse_load_plan`), `surgery/parse.rs`, `surgery/load.rs`, `surgery/plan.rs`. Keep `pub(crate)` visibility as it is.
- Tests live in each file's `#[cfg(test)] mod tests` at the **bottom** of that file, after all non-test items. Nothing may follow a test module (this clears Clippy's `items_after_test_module`).
- Move `PipelineError` and `parse_load_plan` after the types they reference. Replace repeated `std::collections::HashMap` / `HashSet` with `use` statements at module scope.
- No behavior change. `cargo test` and `cargo clippy` must pass with the same test count as before the split.

---

## Step 8. Target file size cap

- In `load_originals`, check `metadata.len()` against a constant `MAX_TARGET_BYTES` (propose 16 MiB) **before** reading, and return a load error naming the file and the limit.
- Tests: a file at the limit loads; one byte over is rejected without reading it (do not actually allocate 16 MiB in the test; make the limit a parameter or a `cfg(test)` constant).

---

## Step 9. Clipboard truncation (GNOME Mutter 4 MB limit)

**Context.** The clipboard in GNOME's Mutter silently truncates content over 4 MB. cvb only ever sees the truncated text and gets no signal. `MAX_INPUT_BYTES` is currently 4 MiB, which is at or above that limit, so the `Limit` error can never fire for an oversized paste: the clipboard cuts it first, and the result is reported as an "incomplete paste".

**Fix.**
- Add `const CLIPBOARD_TRUNCATION_BYTES: usize = 4_000_000;` (the conservative reading of "4 MB") and set `MAX_INPUT_BYTES` below it (propose `3 * 1024 * 1024`). Keep the comment explaining why.
- Because the cap is now below the truncation point, any paste the clipboard truncated arrives larger than the cap and is rejected with `Limit`. The `Limit` message must therefore include the received size and the cap, and say: "if the size is close to 4 MB the clipboard may have truncated the paste; split the change into several smaller envelopes".
- Update the existing size tests to use the constant instead of a hard-coded `4 * 1024 * 1024`.

**Truncation must never parse as success.** The END line with its operation count is what guarantees this. Add a property-style test: build a valid multi-file, multi-operation envelope, then for every prefix of it that stops before the end of the END line (cut at every byte offset, including mid-line and mid-`ops=` number), assert `parse_envelope` returns `Err` (the kind may be `Truncated` or `Syntax`). The one prefix allowed to succeed is the one containing the complete END line. If any shorter prefix parses, fix the parser.

**Protocol note.** The instructions given to the LLM must state the size cap, and that one paste holds exactly one envelope (`MultipleEnvelopes` is rejected), so a larger change needs several envelopes, each applied all-or-nothing on its own.

---

## Questions to put to the user before implementing

1. Already decided by the user: reject any path with a `.git` component (see step 4). Do not ask.
2. Is 16 MiB the right target-file cap?
3. Already decided by the user: the input cap is 3 MiB (`3 * 1024 * 1024`). Do not ask. It is below Mutter's limit under either reading of "4 MB", so the exact limit doesn't matter.

---

## Requirements for the commit stage (do not implement now; keep in mind)

When the write step is built, it must:

1. Re-read every target file immediately before writing and compare byte-for-byte against `plan.original`. Abort the whole surgery on any difference. Loading and confirming have a human-speed gap between them, and the symlink check in `load_originals` is check-then-read.
2. Write the reviewed bytes exactly (`plan.files[i].proposed`), with no recomputation after confirmation.
3. Be as close to atomic as possible across multiple files: stage temp files in the same directory as each target (preserving permissions), fsync them, then rename in sequence. Keep backups of originals, and if any rename fails, roll back the earlier ones from the backups and report what happened.
4. Never follow symlinks at write time either (re-check each path component).
5. Report clearly which files changed, or that nothing did.

---

## Definition of done

- All nine steps merged as separate atomic changes, each with tests.
- Every fix in steps 1, 3, 6, 8 and 9 has at least one test that failed before the change.
- No existing test removed; `cargo test` and `cargo clippy` clean.
- No behavior change outside what is listed here.

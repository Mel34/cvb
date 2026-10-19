# CVB surgery: review remarks to address (instructions for implementers)

Written 2026-10-18 by Claude (Sonnet 5.5) after reviewing branch `cvb-0.6.0` at commit `d43f406` of `github.com/Mel34/cvb`. This document is addressed to the other LLMs working on the surgery engine. You have not seen the review conversation, so everything you need is here.

Read this together with `claude/surgery-implementation-guide.md` (the plan), `surgery-protocol.md` (the contract) and `claude/surgery-fixes.md`. Where this document and the guide disagree, stop and ask the user. This document adds work; it does not change v1 rules.

## 0. Ground rules (same as the guide, section 5)

- One atomic change at a time. For each: write the failing test, show that it fails, make it pass, run `cargo test --locked`, show the full diff, stop, and wait for the user's confirmation.
- Never weaken, delete or loosen an existing test. If a fixture is wrong, fix the fixture and explain.
- Touch only the files a task names. If a fix needs another file, ask first.
- Report test counts before and after every step. A step that lowers the count needs an explanation.
- Do not guess. An ambiguity in the protocol or in this document is a question for the user.
- Do not commit or push unless the user tells you to.

## 1. Baseline you must not regress

Measured on `d43f406` (Linux, Rust 1.97). Re-measure before you start; if your numbers differ, report that first.

| Check | Command | Result |
|---|---|---|
| Unit tests | `cargo test --locked` | 360 passed, 0 failed |
| Surgery CLI | `python3 scripts/test-surgery-cli.py` | 5 passed, 0 failed |
| PTY capture | `PATH="$PWD/target/debug:$PATH" python3 scripts/test-pty-capture.py` | 56 passed, 0 failed |
| Clippy | `cargo clippy --locked --all-targets` | about 51 warnings for the binary, 54 with tests |
| Format | `cargo fmt --check` | differences present |

Build notes: the build needs the `libinput-dev`, `libudev-dev` and `libcanberra-dev` system packages. The PTY suite runs `cvb` through a shell, so `target/debug` must be on `PATH` or the file-bridge test fails with `cvb: command not found`.

## 2. Suggested division of labour

Two implementers should not grade their own work. One LLM does Tasks A, B and C. The other does Task D (the audit) independently, without reading the first LLM's tests beforehand, and then reviews the first LLM's diffs. The user decides who does what.

## 3. Tasks

### Task A: plain-text error messages (priority: high)

**Problem.** A failed precondition prints a Rust debug dump instead of a message. Reproduce with a scratch directory:

```
mkdir proj st && printf 'alpha\nbeta\ngamma\n' > proj/a.txt
T=Tok1234567890abcd
printf 'CVB::%s::BEGIN v=1 id=spot-1\nCVB::%s::FILE path=a.txt\nCVB::%s::OP replace lines=2-2\nCVB::%s::OLD n=1\nbetX\nCVB::%s::NEW n=1\nBETA\nCVB::%s::ENDOP\nCVB::%s::END ops=1\n' $T $T $T $T $T $T $T > bad.env
XDG_STATE_HOME=$PWD/st target/debug/cvb surgery --dry-run --stdin --root proj < bad.env
```

Current output (exit status 2, correct):

```
CVB: surgery planning failed: Plan(PlanFailure { kind: Precondition, path: "a.txt", op_index: 1, detail: Some(Precondition { line: 2, column: 4, byte_offset: 3, expected: "betX", found: "beta", prefix_ended: None, only_trailing_whitespace: false }) })
```

The data is all there, which is good, but a person or an LLM reading this has to decode a struct. The guide (section 5) and protocol (3.2) require plain text, one problem per message, naming the file, operation and line, with control characters escaped.

**Required.**
1. Implement `Display` for the planning-failure, parse-failure and pipeline error types so that the CLI prints readable text. The message starts with `CVB:` and names the protocol code (`E-PRECOND`, `E-RANGE`, and so on, per the guide's error-code map in section 6).
2. For `E-PRECOND`, follow protocol 3.2 exactly: first differing line, first differing column (1-based, counted in Unicode scalar values) together with the byte offset, both the expected and the found text with tabs, trailing whitespace and control characters made visible, and the "one side ends here" wording when one value is a prefix of the other. Do not echo more than a few lines of file content.
3. Audit every place the CLI prints an error with `{:?}` or `{error:?}` (start with `src/surgery/mod.rs`) and replace each with `Display`. Include the commit-failure (`E-IO`) report: it must say, per file, whether the file was restored or could not be restored, and why.
4. Do not change exit statuses (0 applied or dry-run OK, 2 rejected before commit, 3 declined, 4 commit failure).

Illustrative target, not mandatory wording (check the protocol text, not this example, for what must be present):

```
CVB: E-PRECOND: a.txt, operation 1: line 2 does not match.
  first difference at column 4 (byte offset 3)
  expected: "betX"
  found:    "beta"
```

**Tests.** One test per error kind asserting the rendered text contains the code, file, line and no `PlanFailure`/`kind:` debug fragments; a test with a tab, a trailing space and a control character in the mismatching line; a test with accented, CJK and emoji text proving columns are scalar values; a prefix-case test; an integration test (extend `scripts/test-surgery-cli.py` or add a case there) that runs the reproduction above and checks the first line of stderr.

### Task B: puller reminder, guide item 28 (priority: medium)

**Status.** Items 26 (tokens) and 27 (`cvb surgery --spec`) are on the branch. Item 28 is not: `src/pull.rs` has no surgery-format reminder. If it already exists locally, show it instead of redoing it.

**Required (guide section 4, item 28).** Each pulled region is printed with its file path and line numbers, followed by a one-line reminder to answer with the surgery format and a token.

**Questions to put to the user before writing code** (do not decide these yourself):
1. Does the reminder count toward the 24,000-character cascade cap and the clipboard chunk limit? The cap is enforced in `pull.rs`.
2. Is the reminder printed once per `cvb pull` or once per region?
3. Exact wording. A suggestion that points to the spec instead of embedding a token: `Edit with: cvb surgery --spec (one envelope per paste, use the printed token).` Tokens must not be generated by the puller; they come from `--spec`.

**Constraints.** Do not break existing pull tests, the `no matches` path, or the exit codes 0, 1 and 2. Do not edit `manual.md` unless asked.

### Task C: lint and format hygiene (priority: low, separate commits)

Behaviour-neutral. Three separate atomic changes, in this order, each leaving `cargo test --locked` at 360 passed or more:
1. `cargo fmt` alone, no other change, so the diff is reviewable.
2. Clippy warnings that are mechanical. Largest groups in the current output: about 35 collapsible `if` statements; 6 hand-built nul-terminated strings (use `c"..."` literals); then single instances: enum variants sharing the prefix `After`, a byte-string literal, `sort_by_key`, a manual `Option::map`, and items placed after a test module.
3. Dead code: `read_from_clipboard_with` is reported as never used. Do not delete it without asking; it may be kept deliberately.

**Scope.** Default to files under `src/surgery/` only. Warnings in other files (`pty.rs`, `clipboard.rs`, `pull.rs` and so on) are listed in your report, not fixed, unless the user says otherwise. Do not run `cargo clippy --fix` over the whole tree; apply fixes by hand per group so each diff stays reviewable.

### Task D: coverage audit of the applier (priority: high, report first)

**Why.** The review confirmed that crash-recovery tests exist but did not verify the concurrency and mutation requirements. The reviewer found test names by pattern only, so a name that is absent here is not proof that coverage is absent.

**Method.** Produce a table, one row per requirement, mapping each to a test name in `src/surgery/` or to `MISSING`. Do not write any test until the user has seen the table.

Rows to cover (guide section 4, items 15 to 19; protocol section 10):
- Crash after each of: journal `prepared` written; each temp-file write; each rename; the read-back; the `committed` transition. Tests seen by name so far: after journal prepared, after temp write, after a partial rename mix, after all renames, after read-back, conflict leaves unknown content untouched, already-committed or rolled-back is a no-op, and a commit-failure report with a not-restorable file. Not confirmed: a crash during the `committed` transition itself, and crash points between each individual pair of files.
- Two simultaneous runs with the same `id`: one applies, the other gets `E-LOCK` or `E-REPLAY`.
- A file modified between the digest recheck and the rename is caught by the per-rename check and rolled back.
- A target modified before its temp write, before its rename and after its rename is never overwritten by failure or recovery and is reported as a conflict.
- Tampered journal: relative paths revalidated on read, wrong root, device or inode refused.
- Mutation-style checks: removing the count check, collision check, precondition check, any overlap rule, the stale check, or the rollback must make at least one test fail. These run only in a copy of the repository, never on the user's tree, in the style of `scripts/test-history.sh`. State for each safeguard whether a mutation was actually run and what failed.

**Deliverable.** The table, plus for each `MISSING` row a one-paragraph proposal. The user then picks which to implement, as separate atomic changes.

## 4. What to report back

For each task: the test counts before and after, the full diff, the commands you ran, and anything you chose not to do and why. If you could not run something (for example the PTY suite needs the system packages in section 1), say so rather than reporting a pass.

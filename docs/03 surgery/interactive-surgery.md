# CVB interactive surgery: settled design and implementation steps (instructions for implementers)

Written 2026-10-18 by Claude (Sonnet 5.5) with the user, against `cvb-0.6.0` at `eb00ed8`. You have not seen the design conversation, so everything you need is here. Read it together with `surgery-protocol.md`, `claude/surgery-implementation-guide.md` and `claude/review-remarks.md`. Where this document changes a rule in those files, this document wins and the change is listed in step 7 so the user can amend them.

## 0. Ground rules (unchanged)

- One atomic change at a time. For each: write the failing test, show that it fails, make it pass, run `cargo test --locked`, show the full diff, stop, and wait for the user's confirmation.
- Never weaken, delete or loosen an existing test. If a behaviour change is intended and an old test encodes the old behaviour, update the test, say so explicitly, and keep its intent.
- Touch only the files a step names. Do not run `cargo fmt` over the whole tree (it currently reports differences in unrelated files). Format only what you touched.
- Never repair malformed input and never guess. An ambiguity here is a question for the user.
- Do not commit or push unless the user says so. Do not edit the Project docs; list proposed doc changes in your report.
- Report test counts before and after every step.

## 1. Baseline (measured at `eb00ed8`)

| Check | Command | Result |
|---|---|---|
| Unit tests | `cargo test --locked` | 379 passed, 0 failed |
| Surgery CLI | `python3 scripts/test-surgery-cli.py` | 6 passed, 0 failed |
| PTY capture | `PATH="$PWD/target/debug:$PATH" python3 scripts/test-pty-capture.py` | 56 passed, 0 failed |
| Clippy | `cargo clippy --locked --all-targets` | 1 warning (`append_inventory_entry` in `src/main.rs`, too many arguments; outside surgery) |
| Format | `cargo fmt --check` | differences in files you do not own; ignore |

Earlier review remarks are already addressed according to the commit history (`e187aaa`, `cbfd6fd`, `5cbce5b`, `b68ff68`, `d09171c`): readable error messages, the missing crash and concurrency tests, temporary-file cleanup in recovery, restore failures propagated, and a surgery reminder on pull output. I have not re-reviewed those changes. Do not redo them. Step 2 replaces the pull reminder.

## 2. The workflow (settled by the user)

`cvb surgery` is one-shot. The dry run is automatic and is the evidence the LLM reviews. The keypress is the user's "we are happy with this change".

0. `cvb pull` prints a token at the top of its output (on the clipboard with the rest).
1. The user copies the envelope from the LLM.
2. The user runs `cvb surgery`.
3. Surgery reads, parses, loads and plans the envelope (an automatic dry run). It writes nothing to disk and holds no lock.
4. The result goes to the clipboard immediately: the diff on success, a plain-text error on rejection (then it exits).
5. On success the envelope and plan stay in memory in that process.
6. The process waits for one keypress from `/dev/tty`: `a` apply, `x` abort, `d` show the diff again. Ctrl-C and end-of-input abort.
7. On `a` it takes the project lock, rechecks digests and commits through the existing applier. When it finishes, the clipboard holds a one-line outcome (for example `CVB surgery APPLIED id=… files=2`, or `ABORTED id=…`, or the failure report).

Settled decisions:

- **Dry-run is the default step, not a separate invocation.** `--dry-run` stays as the strict non-interactive form: prints the diff, writes nothing, never prompts, same exit statuses as today.
- **No saved state.** No pending buffer on disk. No lock while waiting. Exit statuses are unchanged (0 applied or dry-run OK, 2 rejected before commit, 3 declined, 4 commit failure).
- **`--yes` stays deferred.** A human keypress is still required to apply.
- **Counts are opt-in per block.** `OLD`, `ANCHOR` and `NEW` headers take an optional `n=`. Protocol version stays `v=1`; this only accepts more, and every existing envelope stays valid.
- **Token.** Issued by pull, valid until the next pull, and reusable for several envelopes from one pull. It is a convention, not enforced. Its purpose is to be a delimiter that provably does not occur in the code being edited. With counts off by default it is the only thing that ends a payload block, so it matters more, not less.
- **Clipboard after the session.** A short summary or status line.

Out of scope: an edit-in-`$EDITOR` key, undo from the journal, a persistent pending buffer, token enforcement, overlay integration.

## 3. Open questions for the user (ask before the step that needs them)

1. **id prefix (step 2).** The LLM chooses each envelope's `id`, and a repeated id fails with `E-REPLAY` after the first commit. Pull could also print an id prefix, and the LLM appends `-1`, `-2` and so on. Proposed, not confirmed.
2. **`cvb surgery --token` (step 2).** A flag that prints one fresh token, for edits made without a preceding pull. Proposed, not confirmed.

## 4. Steps

### Step 1: opt-in counts (parser) [`src/surgery/parse.rs`, spec text in `src/surgery/mod.rs`]

Grammar after this step:

```
CVB::TOKEN::OLD          block ends at the next token-prefixed line
CVB::TOKEN::OLD n=3      strict, exactly as today
```

Same for `ANCHOR` and `NEW`. Each block decides for itself.

- If `n=` is present, behaviour and error precedence must be exactly as today, including the G1 to G3 rules. All existing count tests must pass untouched.
- If `n=` is absent, the block is every line up to the next line that starts with `CVB::<token>::`. That line must then be the structural line legal at that position; if it is not, report the same error kinds the parser uses for a wrong structural line today.
- Empty blocks: an `OLD` or `ANCHOR` block with no lines, and a `NEW` block with no lines, keep the error kind the code already uses for `n=0`. Say in your report which kind that is.
- There is no parser-level check of OLD length against the range when counts are absent. The planner's exact-text precondition is authoritative. Add a test that a range longer or shorter than the OLD block yields `E-PRECOND`, and that the message makes the length difference clear (the existing "one side ends here" wording may already cover this).
- Truncation must still always be an error. Extend the existing property test (every proper prefix of a valid multi-file envelope is `Err`) to count-less and mixed envelopes.
- A blank line at the end of a block is significant. Add tests where a `NEW` block ends with a blank line, and where it was dropped (the diff must show the difference). The spec text (step 2) tells LLMs to use `n=` on a block that needs a trailing blank line, because chat code fences often drop trailing blank lines.
- Add tests for: each operation kind without counts; mixed (one block with `n=`, one without) in one envelope; payload lines that look like control lines but carry a different token; a payload line that starts with the real token prefix (document which error results).
- Update the protocol: grammar (2), error precedence (2.4), examples. Mark the revision. Do not edit the Project file; give the user the proposed text.

### Step 2: token on pull, static spec [`src/pull.rs`, `src/surgery/mod.rs`, `src/surgery/token.rs`]

- **Pull output.** On successful content captures only (the same cases that get the reminder today), print at the top:
  - line 1: `CVB surgery token: <16 characters>`
  - line 2: a fixed reminder that the format was given earlier in the conversation, to use the token on every control line, one envelope per paste, and that the token is valid until the next pull. Exact wording is yours; keep it to one line.
  - If the user confirmed the id prefix (open question 1): line 3 `id prefix: <short>`.
- **Generation.** Use `token::generate_token`. Build the body first, then regenerate the token (bounded retries, then a clear error) until it does not occur anywhere in the body. Then prepend the header.
- **Budget.** The 24,000-character cascade cap and the chunk limit must still hold. Reuse the `run_with_reserved_chars` mechanism with the new header length instead of the old end-of-output reminder, and remove the old reminder. Because the header is at the top, it lands in Part 1 of a chunked paste. Add a test for that.
- **No-match, error and non-content paths** get no header.
- **`--spec`.** Print the specification once with a literal `<TOKEN>` placeholder instead of a generated token, so it can be saved once in project instructions. State in it that the token comes from the latest pull and is reused until the next pull, that `n=` is optional, and the trailing-blank-line advice from step 1. Remove the generation from `--spec`.
- **Tests:** header exactly once and at the top; token absent from the body; token valid per `valid_token`; header absent on no-match and errors; cap respected including the header; `--spec` output contains the placeholder and no real token.
- If open question 2 is confirmed, add `cvb surgery --token`.

### Step 3: single-keypress reader [`src/surgery/confirm.rs`]

- Add a key reader on `/dev/tty` using termios raw (non-canonical) mode. Keys: `a`, `x`, `d`. Anything else is ignored with a short hint. Ctrl-C and end-of-input abort.
- The terminal state must be restored on every exit path: normal return, abort, error, panic, and signal. Test it end to end in a PTY by comparing `stty -g` before and after.
- Keep the confirmation logic testable with injected streams, as `prompt_confirmation_with` is today. Do not delete the existing line-based prompt tests; say whether the old prompt remains as a fallback.
- Update `scripts/test-surgery-cli.py` to drive keys instead of `y` plus Enter. List every changed test in your report.

### Step 4: clipboard control messages (spike first) [`src/control.rs`, `src/pty.rs`, `src/clipboard.rs`]

This is the risky step: it touches the PTY proxy. **Do a read-only spike and report to the user before writing code.**

Why it is needed (from reading the code at `eb00ed8`): the clipboard is written when the command ends (the END marker dispatches the rendered transcript). Today the diff would reach the clipboard only after the user's keypress, and the capture at END would overwrite it with the terminal UI. The file bridge already uses the control socket (fd 3), the pathop marker and literal transcript segments. Surgery needs the same kind of channel.

Proposal to verify or reject in the spike:
- A message that makes the proxy write a given text to the clipboard immediately (through `clipboard::process`, so chunking still applies), with an acknowledgement so the client knows it finished before it waits.
- A message that sets the text the proxy dispatches at the command's END instead of rendering the transcript, once, then clears.
- If neither is received (surgery run outside CVB, or a crash), nothing changes: normal capture applies and the diff is simply printed.
- Detection of whether surgery runs inside a CVB session: `CVB_ACTIVE` is exported by `cvb.bash`, and fd 3 should be a socket (check with `fstat`). Decide and test.
- Check whether the pathop marker is needed for these messages, and why the file bridge emits it.
- Message size: read the framing in `control.rs`. A diff can be megabytes. Either chunk it or raise the limit, and test with a payload of at least 1 MiB.

Tests: encode and decode unit tests; a PTY test in which `cvb surgery` runs in a live session and the fake clipboard shows the diff while the process is still waiting, and, after `x`, shows only the outcome line with no UI text.

### Step 5: wire it [`src/surgery/mod.rs`, `src/main.rs` only if needed]

Refactor `run()` into small testable pieces (prepare, present, wait, apply) without changing the pure parse, load and plan code.

- Rejection: the plain-text error goes to the clipboard through step 4 and to stderr, exit 2.
- Success: the plain-text diff (no colour) plus a header line (id, files, lines added and removed) goes to the clipboard; the diff also prints to the terminal; wait for a key.
- `a`: take the lock, run `apply_plan`, then set the outcome line. `x`: set `ABORTED`. Commit failure: the failure report becomes the clipboard text.
- No `/dev/tty`: `E-NOTTY` on stderr, exit 2, unless `--dry-run`.
- The envelope stays in memory only. No journal, lock or file is written before `a`.
- `--dry-run` and `--stdin` keep working as before.
- Tests: all four exit statuses; no file written before `a`; stale file after the diff but before `a` yields `E-STALE` and an unchanged file; abort leaves files untouched.

### Step 6: documentation

Update `manual.md` and `cvb.1` for the new workflow. List for the user the amendments needed in `surgery-protocol.md` (grammar, precedence, revision), in `claude/surgery-implementation-guide.md` (the confirmation gate is now a keypress, item 24 wording, token wording, counts), and in `Overlays.md` if it describes confirmation. Do not edit them yourself.

## 5. Suggested split of work

Two implementers should not grade their own work. One LLM does steps 1 and 2; the other does step 3 and the step 4 spike. After the user answers the spike, one of them implements step 4 and the other reviews it. Step 5 is done by whoever did step 4, and the other reviews the diff before the user does.

## 6. What to report back

For each step: test counts before and after, the full diff, the commands you ran, every existing test you changed and why, and anything you chose not to do. If you could not run something (the PTY suite needs `libinput-dev`, `libudev-dev` and `libcanberra-dev`, and `target/debug` on `PATH`), say so rather than reporting a pass.

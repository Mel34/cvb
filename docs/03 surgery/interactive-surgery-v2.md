# CVB interactive surgery, revision 2: session token and surgery mode (instructions for implementers)

Written 2026-10-19 by Claude (Sonnet 5.5) with the user. This replaces revision 1 (`claude/interactive-surgery.md`, written 2026-10-18). You have not seen the design conversation, so everything you need is here. Read it together with `surgery-protocol.md`, `claude/surgery-implementation-guide.md` and `claude/review-remarks.md`. Where this document changes a rule in those files, this document wins and the change is listed in step 6 so the user can amend them.

What changed since revision 1: the token is now one per cvb session instead of one per pull; `cvb surgery` is now a modal "surgery mode" in which the user pastes the envelope (instead of copying it first and running a one-shot command); the keys are Ctrl+C, Ctrl+V and Ctrl+B, with build armed by a first press and fired by a second. Step 1 (opt-in counts) is unchanged. Later the same day the user settled three more points: the launch clipboard is a context initialization of up to 20,000 characters (workflow instructions, cvb usage including surgery, the spec and the token), the pager is reused for diffs, and surgery works only inside a session, so every form of it requires `CVB_TOKEN`. The commands for re-supplying context are `cvb context` (the whole context initialization), `cvb spec` (spec and token) and `cvb token` (the token only).

## 0. Ground rules (unchanged)

- One atomic change at a time. For each: write the failing test, show that it fails, make it pass, run `cargo test --locked`, show the full diff, stop, and wait for the user's confirmation.
- Never weaken, delete or loosen an existing test. If a behaviour change is intended and an old test encodes the old behaviour, update the test, say so explicitly, and keep its intent.
- Touch only the files a step names. Do not run `cargo fmt` over the whole tree (it reports differences in unrelated files). Format only what you touched.
- Never repair malformed input and never guess. An ambiguity here is a question for the user.
- Do not commit or push unless the user says so. Do not edit the Project docs; list proposed doc changes in your report.
- Report test counts before and after every step.

## 1. Baseline (measured on `cvb-0.7.0` at `d8fc688`)

| Check | Command | Result |
|---|---|---|
| Unit tests | `cargo test --locked` | 380 passed, 0 failed |
| Surgery CLI | `python3 scripts/test-surgery-cli.py` | 6 passed, 0 failed |
| PTY capture | `PATH="$PWD/target/debug:$PATH" python3 scripts/test-pty-capture.py` | 56 passed, 0 failed |
| Overlay lifecycle | `python3 scripts/test-overlay-lifecycle.py` (needs `fuse-overlayfs`) | 12 passed, 1 failed: "snapshot helper is implemented". Expected: the test was written first and `snapshot_project` does not exist yet. Not part of this work. |
| Clippy | `cargo clippy --locked --all-targets` | 1 warning (`append_inventory_entry` in `src/main.rs`, too many arguments; outside surgery) |
| Format | `cargo fmt --check` | differences in files you do not own; ignore |

Build needs `libinput-dev`, `libudev-dev`, `libcanberra-dev`. Re-measure before you start and report first if your numbers differ.

## 2. The workflow (settled by the user)

1. **Session start.** cvb generates one token for the session, exports it to the child shell as `CVB_TOKEN`, and puts the **context initialization** text on the clipboard. The user pastes it into the LLM's context at the start of the conversation. It contains, in this order: (a) the workflow instructions (this section, written for an LLM: pull, plan, tell the user to start surgery mode, produce an envelope, wait for the diff, amend or finish); (b) instructions on how to use cvb, surgery included (`manual.md`); (c) the surgery specification with the session token, and the token again on its own clearly marked line at the very end, so it is not lost when the text arrives in pages. Target size: at most 20,000 characters in total (the user has found ChatGPT handles that safely). It goes through the normal clipboard path, so it is paged by the chunk pager (about 9,750 characters per page by default).
2. The token is valid for the whole cvb session and is reused for every envelope in it. It is **public**: it goes into an LLM conversation, so it is a delimiter and a session identifier, never a secret. It is expected to double as the session id in the future master/slave architecture; anything that needs authentication there must use a separate private secret.
3. Three small commands re-supply the session's context to an LLM, so the user never has to restart cvb to get it. All print the same session token, never a new one.
   - `cvb context`: the whole context initialization (workflow, cvb usage, spec, token), identical to the launch text.
   - `cvb spec`: the surgery specification plus the token.
   - `cvb token`: the token only.
4. The LLM inspects code with `cvb pull`, the user and LLM agree a plan, the LLM tells the user to start surgery mode.
5. The user runs `cvb surgery`. cvb enters **surgery mode**: a full-screen-style modal loop on `/dev/tty`, like the pull pager.
6. The LLM produces an envelope using the session token. The user copies it and presses **Ctrl+V** in surgery mode (or the terminal's own paste, Ctrl+Shift+V; both do the same thing).
7. cvb checks that the envelope carries the session token, then runs the automatic dry run (parse, load, plan: nothing is written, no lock, no journal). The plain-text diff (or the plain-text error) goes to the clipboard immediately and is printed. The envelope and plan stay in memory in this process.
8. The user pastes the diff to the LLM. They decide to apply, amend or abort.
   - **Apply:** Ctrl+B arms build, a second Ctrl+B within 3 seconds fires it. Any other key disarms.
   - **Amend:** copy the new envelope, press Ctrl+V.
   - **Abort:** Ctrl+C (or `q`). Nothing is changed.
9. On a successful apply, or on abort, the loop closes and the clipboard holds a one-line outcome.

### Keys

| Key | In surgery mode |
|---|---|
| Ctrl+V, or a bracketed paste | Take the envelope (clipboard read for Ctrl+V; the pasted payload for a bracketed paste) and dry-run it |
| Ctrl+B | Candidate present: arm build. Armed: build (apply). No candidate: print "nothing to build" |
| any other key while armed | disarm; the key itself is then handled normally |
| Ctrl+C, `q` | Abort (discard candidate, close the mode) |
| Space, ←, → | Chunk navigation of the diff on the clipboard, as the pull pager does today (see step 4 for how this is shared) |

Why two presses: C, V and B are adjacent keys, and the likeliest slip is reaching for Ctrl+V to paste an amended envelope and hitting Ctrl+B. Apply has no undo, so a single slip must cost nothing. The status line always shows the keys and the armed state, for example `^C cancel  ^V paste  ^B build` and, when armed, `^B again to build, any other key disarms`. The armed state expires after 3 seconds.

### State machine

States: `Idle` (no candidate), `Candidate` (valid dry run held), `Armed` (candidate, build armed).

- **Paste with a recognisable envelope** (its `BEGIN` line carries the session token): any previous candidate is discarded first, then the dry run runs. Success: `Candidate`. Rejection (parse, load or plan error): the error goes to the clipboard and screen, state `Idle`. A superseded envelope must never stay appliable.
- **Paste that is not an envelope for this session** (no `BEGIN` line, or another token): report it (`E-TOKEN` for a wrong token, naming both tokens and suggesting `cvb token`; a plain "clipboard does not contain an envelope" otherwise) and **keep the current state**. This protects a good candidate from an accidental Ctrl+V while the clipboard still holds our own diff.
- **Build fired:** take the project lock, recheck digests, commit through the existing applier.
  - Success: outcome line `CVB surgery APPLIED id=<id> files=<n>` on the clipboard, close, exit 0.
  - `E-STALE` (a file changed since the dry run): nothing is written, report on the clipboard, discard the candidate, state `Idle`, stay in the mode.
  - Commit failure (exit 4 class): the failure report becomes the clipboard text, close, exit 4.
- **Abort:** clipboard `CVB surgery ABORTED id=<id>` (or `ABORTED (no envelope)`), close, exit 3.
- No file, journal or lock is written before build is fired. Exit statuses are unchanged: 0 applied (or non-interactive dry-run OK), 2 rejected before commit (non-interactive forms), 3 declined or aborted, 4 commit failure.

### Token rules

- **Surgery works only inside a cvb session.** The token does not exist before a session is launched, and it is mandatory. Every form of `cvb surgery` (interactive mode, `--dry-run`, `--stdin`, `--spec`) and the commands `cvb context`, `cvb spec` and `cvb token` require `CVB_TOKEN`. Without it they refuse with a clear message (`E-NOSESSION`, to be added to the protocol's error codes) and exit 2, before reading any input.
- The envelope's token must equal `CVB_TOKEN`. Mismatch is `E-TOKEN`, also new. It is checked first, from the `BEGIN` line, before other parse errors.
- Inside the envelope nothing else changes: every control line must still carry the same token as its `BEGIN` line, and protocol version stays `v=1`.
- Automated tests and scripts that need surgery outside a live session set `CVB_TOKEN` explicitly in the environment of the process they run. That is the only supported way to run surgery without the proxy, and it is deliberate: tests then exercise the same token check as real use.
- There is no longer a static spec with a placeholder: a spec is only meaningful with the session's token.
- Collisions: a fixed token cannot be regenerated when it occurs in code. It can only occur in text written during the session (for example an example envelope saved into a doc). The parser already rejects a payload line that starts with the real token prefix, so the cost is a clean rejection, not corruption. Make sure that error message says what happened.

### Settled earlier and still true

- `--dry-run` stays the strict non-interactive form: prints the diff, writes nothing, never prompts.
- `--yes` stays deferred.
- Counts (`n=`) are opt-in per block on `OLD`, `ANCHOR` and `NEW`; protocol `v=1` is unchanged and every existing envelope stays valid.
- Clipboard after a session: a short summary or status line.
- `id` is chosen by the LLM. Ids are burned only by a successful commit (`E-REPLAY` stays as it is today), so repeated amend attempts do not consume them. If ids still collide across sessions in practice, report it; do not change the journal.

Out of scope: an edit-in-`$EDITOR` key, undo from the journal, a persistent pending buffer, overlay integration, master/slave authentication.

## 3. Open questions for the user (ask before the step that needs them)

Settled by the user on 2026-10-19: the launch clipboard is the context initialization described in section 2 (workflow instructions, cvb usage including surgery, the spec and the token), up to about 20,000 characters, paged by the existing pager; the pager is reused for the diff; surgery is allowed only inside a session.

Still open:

1. **Command names (step 2).** The user chose `cvb context`, `cvb spec` and `cvb token` on 2026-10-19. Before adding them, check that none collides with an existing subcommand or with how `cvb` dispatches unknown arguments, and ask if one does.
2. **Size of `manual.md` (step 2).** Report its current size first. If the manual alone leaves too little room for the workflow text and the spec inside 20,000 characters, ask the user before cutting anything.
3. **Pager sharing (step 4 spike):** report where the chunk pager lives and how surgery mode can drive it. The user has agreed in principle to reuse it.

## 4. Steps

### Step 1: opt-in counts (parser) [`src/surgery/parse.rs`, spec text in `src/surgery/mod.rs`]

Unchanged from revision 1. Grammar after this step:

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
- A blank line at the end of a block is significant. Add tests where a `NEW` block ends with a blank line, and where it was dropped (the diff must show the difference). The spec text tells LLMs to use `n=` on a block that needs a trailing blank line, because chat code fences often drop trailing blank lines.
- Add tests for: each operation kind without counts; mixed (one block with `n=`, one without) in one envelope; payload lines that look like control lines but carry a different token; a payload line that starts with the real token prefix (document which error results).
- Update the protocol: grammar (2), error precedence (2.4), examples. Mark the revision. Do not edit the Project file; give the user the proposed text.

### Step 2: session token, `cvb token`, spec [`src/main.rs`, `src/pty.rs` or `src/shell.rs` (wherever the child environment is set), `src/pull.rs`, `src/surgery/mod.rs`, `src/surgery/token.rs`]

- **Generation.** At session start, `token::generate_token()` once; export `CVB_TOKEN` to the child bash alongside the existing `CVB_ACTIVE` mechanism. Find where that is set and report the file.
- **Launch clipboard.** Assemble the context initialization (section 2, item 1) and put it on the clipboard at launch through the normal clipboard path, so the pager applies. Workflow instructions go in a new text file embedded in the binary the same way `manual.md` is (name and location are your choice; report them). Write the workflow text for an LLM reader: imperative, short, no history. Report the assembled size.
- **Size guard.** A unit test assembles the text and fails if it exceeds 20,000 characters, so a growing manual cannot silently break the budget. The test also asserts the token appears in the spec and again on the final marked line, and that the token does not occur anywhere else in the manual or workflow text.
- **`cvb context`, `cvb spec`, `cvb token`.** Three new subcommands (section 2, item 3). `cvb token` prints exactly the token and a newline. `cvb spec` prints the surgery specification and the token. `cvb context` prints the whole context initialization. Build the launch text and `cvb context` from one function so they cannot drift; a test asserts they are byte-identical. None of them ever generates a token. Without `CVB_TOKEN` all three refuse (`E-NOSESSION`, exit 2).
- **`cvb surgery --spec`.** Kept as an alias that prints exactly what `cvb spec` prints, so existing `--spec` tests keep their intent (they only gain `CVB_TOKEN` in the environment). The old generated-token behaviour and the `<TOKEN>` placeholder are removed.
- **Output of `cvb context` and `cvb spec`.** Until step 4 exists they print to stdout and the normal capture puts them on the clipboard (pages included). Once step 4's "dispatch this text at END" message exists, switch both to hand the text over that way and print a single status line (for example `context initialization copied, 3 pages`), so 20,000 characters are not dumped on the terminal. Note this in your step 4 report.
- **Enforcement.** Add `E-NOSESSION` (no `CVB_TOKEN`: refuse every surgery form before reading input, exit 2) and `E-TOKEN` (envelope token differs from `CVB_TOKEN`, checked first from the `BEGIN` line, names both tokens and says `cvb token` prints the current one). Update `scripts/test-surgery-cli.py` and any other script or test that runs the surgery CLI to set `CVB_TOKEN` in the environment; list every changed test and say that its intent is unchanged.
- **Spec text.** Update it: the token is the session token given at the start of the conversation and is reused for every envelope; one envelope per paste; `n=` is optional; use `n=` on a block that needs a trailing blank line; the user pastes envelopes into `cvb surgery` mode.
- **Pull.** The pull output stays as it is, except the existing surgery reminder line at the end is reworded: it must not contain a token; it says to use the surgery format and the session token from the start of the conversation, and that `cvb token` prints it again. The reminder already counts inside the 24,000-character cap; keep that mechanism and its tests, adjusting only the expected text.
- **Tests:** token generated once per session and stable across commands in it; `CVB_TOKEN` present in the child and valid per `valid_token`; `cvb token` prints exactly the token and a newline; `cvb spec` contains the token and the full spec; `cvb context` output equals the launch text byte for byte; `cvb context`, `cvb spec`, `cvb token` and every surgery form without `CVB_TOKEN` give `E-NOSESSION`, exit 2, and read no input; wrong token gives `E-TOKEN` with both tokens named and no file touched; pull reminder has no token and the cap still holds; the 20,000-character guard above; the launch clipboard in a PTY test holds the context initialization (first page contains the workflow text, the token appears on the last page's final line); a PTY test that two commands in one session print the same token.

### Step 3: key reader, bracketed paste, arming [`src/surgery/confirm.rs`, or a new `src/surgery/keys.rs` if you ask first]

- Read `/dev/tty` in raw (non-canonical) mode **without ISIG**, so Ctrl+C arrives as the byte `0x03`. Enable bracketed paste (`ESC[?2004h`) on entry and disable it (`ESC[?2004l`) on every exit path.
- Decode into events: `Paste(text)` for a bracketed paste (everything between `ESC[200~` and `ESC[201~` is payload and is **never** interpreted as keys, whatever it contains), `PasteKey` (`0x16`), `Build` (`0x02`), `Abort` (`0x03` or `q`), `Space`, `Left`, `Right`, `Other`.
- Bracketed-paste payload is capped at 3 MiB (the envelope input limit); over the cap is an error event, not a hang.
- Arming: pure logic with an injected clock. First `Build` in `Candidate` arms; a second within 3 seconds fires; any other event disarms; the timeout disarms.
- The terminal state must be restored on every exit path: normal return, abort, error, panic, SIGTERM, SIGHUP. Test it end to end in a PTY by comparing `stty -g` before and after.
- Keep the logic testable with injected streams, as `prompt_confirmation_with` is today. Do not delete the existing line-based prompt tests; say whether the old prompt remains.
- Tests: a pasted envelope containing `a`, `x`, `q`, spaces, tabs and unicode never produces a key event; a 3 MiB paste; a paste split across reads; an unterminated paste; Ctrl+B while idle; arm then timeout; arm then paste; arm then Ctrl+B fires; Ctrl+C in each state.
- Update `scripts/test-surgery-cli.py` as needed; list every changed test.

### Step 4: control messages and pager (spike first) [`src/control.rs`, `src/pty.rs`, `src/clipboard.rs`]

This is the risky step: it touches the PTY proxy. **Do a read-only spike and report to the user before writing code.**

Why it is needed (from reading the code at `eb00ed8`): the clipboard is written when the command ends (the END marker dispatches the rendered transcript). Surgery mode needs to write the diff to the clipboard while the process keeps running, and when it closes the capture at END would overwrite the clipboard with the terminal UI. The file bridge already uses the control socket (fd 3), the pathop marker and literal transcript segments.

Proposal to verify or reject:
- A message that makes the proxy write a given text to the clipboard immediately (through `clipboard::process`, so chunking still applies), with an acknowledgement so the client knows it finished.
- A message that sets the text the proxy dispatches at the command's END instead of rendering the transcript, once, then clears.
- If neither is received (surgery run outside CVB, or a crash), normal capture applies and the diff is simply printed.
- Detection of a cvb session: `CVB_ACTIVE`/`CVB_TOKEN` plus a socket check on fd 3 (`fstat`). Decide and test.
- Check whether the pathop marker is needed for these messages, and why the file bridge emits it.
- Message size: read the framing in `control.rs`. A diff can be megabytes. Either chunk it or raise the limit, and test a payload of at least 1 MiB.
- **Pager.** Chunk navigation (Space, ←, →, default chunk 9,750 characters) lives in `src/clipboard.rs`. Report where it runs (proxy or the child process) and how surgery mode can show a multi-chunk diff with the same keys. Also report how `cvb token` and the launch clipboard text can arrive clean, without the prompt or command echo around them.

Tests: encode and decode unit tests; a PTY test in which `cvb surgery` runs in a live session and the fake clipboard shows the diff while the process is still waiting, and, after abort, shows only the outcome line with no UI text.

### Step 5: the mode [`src/surgery/mod.rs`, `src/main.rs` only if needed]

- Implement the state machine in section 2 as a pure function `step(state, event, now) -> (state, actions)` with a thin effects layer around it (clipboard writes, printing, dry run, apply). Unit-test the pure part exhaustively: every state against every event.
- Do not change the pure parse, load and plan code, or the applier.
- Success output to clipboard: plain-text diff (no colour) with a header line (id, files, lines added and removed); the diff also prints to the terminal.
- Rejection: plain-text error on clipboard and stderr; state per section 2.
- `--dry-run` and `--stdin` keep their behaviour, with the session-token requirement from step 2. No `/dev/tty`: `E-NOTTY`, exit 2, unless `--dry-run`.
- Tests: all four exit statuses; no file written before build is fired; stale file after the diff but before build yields `E-STALE`, an unchanged file, and the mode stays open; abort leaves files untouched; a non-envelope paste keeps a held candidate; a recognisable envelope that fails discards the held candidate; wrong token gives `E-TOKEN` with both tokens named; single Ctrl+B never applies.

### Step 6: documentation

Update `manual.md` and `cvb.1` for the new workflow (remember `manual.md` is part of the 20,000-character launch text). List for the user the amendments needed in `surgery-protocol.md` (grammar, precedence, `E-TOKEN`, `E-NOSESSION`, revision), in `claude/surgery-implementation-guide.md` (the confirmation gate is now armed keypresses, item 24 wording, token wording, counts, item 28 reminder), and in `Overlays.md` if it describes confirmation. Do not edit them yourself.

## 5. Suggested split of work

Two implementers should not grade their own work. One LLM does steps 1 and 2; the other does step 3 and the step 4 spike. After the user answers the spike, one of them implements step 4 and the other reviews it. Step 5 is done by whoever did step 4, and the other reviews the diff before the user does.

## 6. What to report back

For each step: test counts before and after, the full diff, the commands you ran, every existing test you changed and why, and anything you chose not to do. If you could not run something (the PTY suite needs the system packages in section 1, and `target/debug` on `PATH`), say so rather than reporting a pass.

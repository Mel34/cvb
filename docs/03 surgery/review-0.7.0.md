# CVB 0.7.0 review (branch `cvb-0.7.0`, head 31537e3, 2026-10-21)

## Verdict

The 0.7.0 goals from the handoff are met and the code is in good shape. Startup context initialization lives in Rust, `manual.md` stays an external runtime file, the launch path creates no shell-history entry, and the token rule (inherit if present, generate if absent, no second init path) is implemented exactly once, in `main.rs` before the PTY child exists. Remaining issues are mostly small. The two that matter most are a failing `cargo clippy` and the lone-Esc behavior in surgery mode.

## What I ran

| Check | Result |
|---|---|
| `cargo build --locked` | OK, 2 dead-code warnings |
| `cargo test --locked` | 484 passed, 0 failed |
| `scripts/test-command-lifecycle.sh` | 34 passed |
| `scripts/test-cli-bridge.py` | 11 passed |
| `scripts/test-surgery-cli.py` | 10 passed, 3 failed (see finding 10) |
| `cargo clippy --all-targets` | **fails**: 1 error, 18 warnings |

Not run: `test-pty-capture.py` (verify.sh stopped at the surgery failures). The sandbox has no Wayland session.

## What is good

- Token handling (`main.rs:36-48`): inherit-or-generate, exported before the PTY is spawned. `generate_token` reads `/dev/urandom` with rejection sampling for base62, 16 characters (about 95 bits).
- `context::initialize()` fails soft: the session still starts, and the user is never told to paste something that was not prepared.
- `cvb context`, `cvb spec`, `cvb token` all share one assembly function. A test pins the launch clipboard and `cvb context` as byte-identical.
- The 20,000 character cap is enforced, and the manual is 15.5 KB with 35 `{TOKEN}` placeholders, so there is headroom.
- Bracketed paste is enabled in surgery mode, so pasting through the terminal does not turn spaces in the envelope into Space (apply) events.

## Findings

### 1. `cargo clippy` fails (`src/surgery/keys.rs:131`)
`clippy::never_loop` is deny-by-default, and `next_event` wraps a `match` in a `loop` where every arm returns. Remove the `loop`. `scripts/verify.sh` does not run clippy, so it never caught this; adding `cargo clippy --all-targets -- -D warnings` after the cleanup below would keep it that way.

### 2. A lone Esc stalls surgery mode (`keys.rs`, the `0x1b` arm)
After an Esc byte the reader blocks until it has 6 bytes (`START_MARKER.len()`), so a bare Esc keypress holds the event loop until more keys arrive. A following Ctrl+C or `q` is buffered but not acted on until then, which undermines "q/Ctrl-C aborts". I read this from the code and did not reproduce it. Fix: after Esc, `poll` the tty for about 50 ms and treat silence as a plain Esc.
Related: unrecognised CSI sequences drain a fixed 3 bytes (`\x1b[1;5D` leaves `1;5D` behind). Harmless today, but a leftover `q` would be read as Abort. Parse to the CSI final byte (0x40 to 0x7E) instead.

### 3. Dead code left over from the refactor (`context.rs`)
`prepare_with` and `load_manual` are unused in the non-test build, and `prepare_context_with` replaced them. If tests still call the old pair, they are testing a path production does not take. I did not check which tests use them.

### 4. `load_manual_text` has behavior that differs under `#[cfg(test)]`, plus a cwd fallback (`context.rs:124-157`)
- Under test with `bash_source == None` it reads the repo's `manual.md`; production takes a different branch.
- If `CVB_BASH_SOURCE` is unset (for example `CVB_TOKEN` exported by hand), it falls back to `locate_manual(".")`, which accepts `./manual.md` or `../manual.md` from whatever directory you are in. That can pick up another project's manual. Return an error instead.

### 5. `get_spec_text` assumes the spec is the last section (`context.rs:166-180`)
It finds the header string, takes everything to end of file, and strips two leading spaces per line. A section added after Section 8 would silently ship inside `cvb spec`. Add an end marker, or a test that fails if anything follows the spec.

### 6. `data/cvb.bash` environment and directory handling
- `CVB_PARENT_HISTORY_FILE` is exported but never restored or unset on exit, unlike `CVB_ACTIVE` and `CVB_HISTORY_FILE`. The parent shell keeps a variable pointing at a deleted file.
- `mkdir -p "$runtime_dir"` runs before `umask 077`. With `XDG_RUNTIME_DIR` unset the directory is `/tmp/cvb`, shared and predictable. Use `mkdir -p -m 700` and check ownership. The files inside are created 0600, so this is hardening rather than a leak.

### 7. `cvb.1` is corrupted
It contains 7 literal form-feed bytes (0x0C) where `\fB` and `\fR` should be, so it renders as `R[Bfile:IPATHR]` and `BCVB_TOKENR`. A `\f` was interpreted as an escape somewhere in the editing pipeline (shell, heredoc or script string). That is the class of problem surgery exists to avoid, so it is worth checking how that edit was made. Restore the escapes, and consider a small test that fails on control bytes in `cvb.1`.

### 8. Documentation gaps
- `cvb --help` (`main.rs:61-84`) lists only `cvb`, `cvb <PATH>` and `pull`. It omits `surgery`, `context`, `spec` and `token`.
- `README.md` does not mention `cvb token`, `cvb context`, `cvb spec`, or the install requirement that `manual.md` sit beside `cvb.bash` (or one directory up in the dev layout).

### 9. Error-handling churn in surgery mode
About 25 of the last 30 commits are `fix: report X flush failures`. `run_surgery_mode` now has roughly 25 `return 2` sites, each repeating an `eprintln!` plus a `SurgeryEnd` send with its own nested error branch (`surgery/mod.rs` is 2,290 lines, 66 `eprintln!`). The behavior is right (fail closed), but a single helper such as `fail_surgery(&control, code, msg)` would cut hundreds of lines, make the next change reviewable, and stop the same pattern being copied again. Failures of a tty or stderr flush are also very unlikely in practice, so I would stop adding cases individually.

### 10. Test environment and small hygiene
- Three interactive surgery CLI tests failed here with exit 2 (`interactive mode paste and abort`, `two-press apply`, `stale file recovery`). The clipboard layer calls `wl-paste`, and my sandbox has no Wayland session or `wl-clipboard`, so I think this is environmental, but I did not confirm it. Consider a skip (like the PTY harness's `SKIP:` line) when no clipboard backend is available.
- `env.sh` hardcodes `/home/anon/Projects/cvb/target/debug`.
- 16 clippy `collapsible_if` warnings, one `too_many_arguments` (8/7), one `io::Error::other` suggestion and one single-element loop remain.

## Suggested order
1. Findings 1, 3 and 7 (mechanical, small, and they get clippy and the man page green).
2. Finding 2 (user-facing, safety-adjacent).
3. Findings 4, 5 and 6 (small hardening).
4. Finding 8 (docs), then 9 as a pure refactor with the existing 484 tests as the safety net.

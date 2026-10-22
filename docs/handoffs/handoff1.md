We're continuing work on CVB 0.5.0 in `~/Projects/cvb`, branch `cvb-0.5.0-ps0`.

**Problem:** Multiline pasted commands involving multiple `cvb PATH` captures can leave clipboard capture stuck on the last path's contents. The reproduction is:

```bash
cvb /tmp/cvb-suppression-a.txt
cvb /tmp/cvb-suppression-b.txt
printf 'CVB_CANARY_BRACKETED_ON_802\n'
```

With Readline bracketed paste enabled, clipboard remains `CVB_FILE_B_CONTENT`. With bracketed paste disabled in the instrumented session, clipboard ends with `CVB_CANARY_BRACKETED_OFF_801`.

**Confirmed event imbalance with bracketed paste enabled:** target request increments suppression from 0→1; a subsequent START increments it 1→2; another target request increments it 2→3; another START increments it 3→4; one END decrements it to 3. Suppression remains active.

**Relevant source:** `data/cvb-control.bash` sets `PS0` to a START marker and `PROMPT_COMMAND` to `cvb_control_prompt`, which emits the END marker. In `src/pty.rs`, START increments suppression when suppression is already active; `TargetRequest` also increments suppression. END decrements suppression only once. PTY output/markers are processed before control messages in each poll cycle.

An isolated Bash 5.3.20 experiment confirmed that a three-command paste produces three PS0 START markers but only one PROMPT_COMMAND END marker. A DEBUG trap fires before commands but also runs for prompt-handler commands unless guarded; RETURN is not a general command-completion hook.

**Diagnostics:** opt-in logging is already added to `src/pty.rs` via `CVB_DEBUG_LOG`. Start a fresh instrumented session with:
```bash
: > /tmp/cvb-debug.log
CVB_DEBUG_LOG=/tmp/cvb-debug.log cvb
```
The installed binary is already the new diagnostic build. Keep the original stuck session alive. The diagnostic source passed `cargo check`; existing dead_code warnings remain in `src/surgery.rs`. No source fix has been applied yet.

**Potential solutions:** (1) temporary workaround: disable Readline bracketed paste in CVB's generated shell configuration; (2) preferred long-term direction: fix marker/suppression lifecycle so pasted command blocks cannot leave suppression stuck regardless of bracketed-paste setting; (3) instrument TargetResponse, all marker events, suppression depth, capture state, and PTY/control ordering further before choosing.

**Next task:** inspect the current marker scanner, PTY loop, and request/response lifecycle. Explain the exact event sequence and propose the smallest justified protocol correction. Do not patch the counter speculatively. Add a regression test for two path captures plus a canary in one pasted block, then proceed one atomic change at a time. Follow the CVB Coding Protocol: exact source anchors/count checks for Python surgery, verify written source, run tests/checks, preserve unrelated changes, and commit clean checkpoints. Use CVB-native tools first; ask rather than guess.

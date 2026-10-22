# CVB — Current Project Plan, Design Choices & Continuation Context

**Project:** CVB (Capture → View → Bridge)  
**Path:** `~/Projects/cvb`  
**Platform:** Arch Linux / Wayland / COSMIC  
**Language:** Rust, edition 2024  
**Current version:** `0.1.0`

## 1. Purpose

CVB is a Unix terminal utility for reliable **LLM ↔ human ↔ terminal ↔ clipboard** exchange.

Guiding question:

> Does this make the exchange more reliable, more useful, or less intrusive?

CVB is deliberately not an AI agent, shell replacement, terminal logger/database, ANSI stripper, or command parser. Prefer simple predictable mechanisms ("dumb is good") over heuristics.

## 2. CLI

```text
cvb on
cvb off
cvb --help
cvb --version
cvb <path>
cvb <command> [args...]
```

There are no `file` / `directory` subcommands. Path determines the operation.

For target resolution, CVB gives existing filesystem paths precedence over
external commands. Thus `cvb Cargo.toml` bridges the file when it exists;
otherwise `cvb Cargo.toml` is treated as an external command. Explicit paths
such as `./foo`, `../foo`, and `/tmp/foo` are always path targets.

This is intentional CVB-specific behavior rather than a requirement to mimic
normal shell command resolution exactly.

Potential unresolved collision: commands literally named `on` or `off`.

## 3. Resident architecture

`cvb on` creates a child interactive Bash inside a PTY:

```text
user terminal
    │
    ▼
CVB parent
    │
    ├── PTY master ◄────────► child Bash / PTY slave
    │
    └── private control socket ◄────────► child Bash FD 3
```

The parent proxies terminal I/O. Interactive programs must continue to work normally. SIGWINCH/resize is forwarded and terminal modes restored.

`cvb off` is an ordinary command in the child shell and effectively exits when Bash is idle. There is no out-of-band escape mechanism.

Separate terminal sessions may run independent CVB instances.

## 4. Parent shell state

Chosen model: **hybrid state handoff**.

Child Bash:
1. loads normal `.bashrc`;
2. sources a temporary parent-state file.

The state file contains:
- aliases,
- functions,
- `set +o` shell options,
- `shopt -p` options.

Environment is inherited naturally.

Not cloned:
- traps,
- transient shell internals.

CVB owns prompt/control hooks, command-boundary hooks and capture state.

Parent wrapper: `data/cvb.bash`.

Temporary state files live under:

```text
$XDG_RUNTIME_DIR/cvb/
```

## 5. Child Bash initialization

`src/shell.rs` creates a temporary rcfile which sources:

```text
~/.bashrc
parent-state-file
data/cvb-control.bash
```

The control script is loaded from:

```text
data/cvb-control.bash
```

using `env!("CARGO_MANIFEST_DIR")`.

A previous attempt used `OnceLock::get_or_try_init`; this was removed because that API is unstable in the user's Rust toolchain.

`cargo check` currently passes.

## 6. Control channel

Chosen mechanism: **`socketpair()`**.

Child receives one endpoint as FD 3.

The protocol is length-prefixed:

```text
4-byte big-endian payload length
1-byte message type
payload
```

Message types:

```text
START = 0x01
END   = 0x02
EXIT  = 0x03
```

START:

```text
8-byte u64 command ID
UTF-8 command text
```

END:

```text
8-byte u64 command ID
4-byte i32 exit status
```

EXIT:

```text
empty
```

Maximum frame size: 1 MiB.

Rust type:

```text
pub enum ControlMessage {
    Start { id: u64, command: String },
    End { id: u64, status: i32 },
    Exit,
}
```

`src/control.rs` has incremental decoding and tests.

Last confirmed full test result:

```text
12 passed
0 failed
```

Important future issue: parent control FD is nonblocking, while current `ControlChannel::send()` uses blocking-style `write_all`. This is acceptable for the current unused/small outbound path but should be made robust before relying on parent-to-child messages.

## 7. Command boundaries

Chosen lifecycle:

```text
DEBUG trap / Bash-side guard
        │
        ▼
START <complete original command line>
        │
        ▼
command executes
        │
        ▼
PROMPT_COMMAND
        │
        ▼
END <exit status>
```

The command text is the complete original Bash input line. Rust must not reconstruct argv or normalize/parse it.

Therefore:

```text
cmd1 && cmd2
cmd1 || cmd2
cmd1 ; cmd2
cmd1 | cmd2
$(...)
```

are each one top-level capture.

Background jobs end when Bash returns to the prompt. CVB does not track them afterward.

Command substitution belongs to the top-level command. Only output reaching the PTY is captured.

A no-output command still produces a clipboard record such as:

```text
$ result=$(some-command)
[no output; exit 0]
```

## 8. Current `data/cvb-control.bash`

The currently proposed script is:

```text
# CVB control channel

CVB_CONTROL_FD=3
CVB_COMMAND_ID=0
CVB_COMMAND_ACTIVE=0

cvb_control_write_byte() {
    printf '%b' "\x$1" >&"$CVB_CONTROL_FD"
}

cvb_control_write_u32() {
    local value=$1

    printf '%b' "\x${value:0:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:2:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:4:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:6:2}" >&"$CVB_CONTROL_FD"
}

cvb_control_write_u64() {
    local value=$1

    printf '%b' "\x${value:0:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:2:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:4:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:6:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:8:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:10:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:12:2}" >&"$CVB_CONTROL_FD"
    printf '%b' "\x${value:14:2}" >&"$CVB_CONTROL_FD"
}

cvb_control_start() {
    local command=$1
    local payload_length

    ((CVB_COMMAND_ID++))
    CVB_COMMAND_ACTIVE=1

    payload_length=$(printf '%08x' $((9 + ${#command})))

    cvb_control_write_u32 "$payload_length"
    cvb_control_write_byte 01
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    printf '%s' "$command" >&"$CVB_CONTROL_FD"
}

cvb_control_end() {
    local status=$1
    local status_hex

    ((CVB_COMMAND_ACTIVE)) || return

    status_hex=$(printf '%08x' "$status")

    cvb_control_write_u32 0000000d
    cvb_control_write_byte 02
    cvb_control_write_u64 "$(printf '%016x' "$CVB_COMMAND_ID")"
    cvb_control_write_u32 "$status_hex"

    CVB_COMMAND_ACTIVE=0
}

cvb_control_exit() {
    cvb_control_write_u32 00000001
    cvb_control_write_byte 03
}

cvb_control_debug() {
    ((CVB_COMMAND_ACTIVE)) && return
    [[ ${BASH_COMMAND} == cvb_control_* ]] && return

    cvb_control_start "$BASH_COMMAND"
}

cvb_control_prompt() {
    local status=$?

    cvb_control_end "$status"
}

trap 'cvb_control_debug' DEBUG
PROMPT_COMMAND='cvb_control_prompt'
```

**This is not yet considered correct.**

First task in the next session is to test it against actual child Bash.

Specifically investigate:
- DEBUG trap repetition;
- whether `CVB_COMMAND_ACTIVE` is sufficient;
- DEBUG events caused by hook functions;
- preservation of existing `PROMPT_COMMAND`;
- signed/negative exit-status encoding;
- exact `BASH_COMMAND` behavior;
- functions and compound commands;
- nested command execution.

Do not build clipboard integration on top of this until it is proven.

## 9. PTY implementation

`src/pty.rs` currently has:
- `forkpty`;
- PTY proxy;
- socketpair;
- child FD 3 setup;
- raw `bash --rcfile <rcfile> -i` exec;
- polling of stdin, PTY master, SIGWINCH pipe and control socket.

Resize was runtime-tested successfully.

Known future PTY cleanup:
- save/restore original SIGWINCH disposition instead of always restoring `SIG_DFL`;
- avoid losing final PTY output when HUP occurs;
- improve static signal-pipe handling;
- handle control socket HUP/EOF deliberately.

Temporary diagnostic:

```text
CVB child exited with status 0
```

should eventually disappear.

## 10. Clipboard

Planned runtime dependency:

```text
wl-clipboard
```

Clipboard is transport, not a database.

Each completed top-level command updates the clipboard exactly once after authoritative END.

Format:

```text
$ git status
On branch master
nothing to commit, working tree clean
[exit 0]
```

No-output:

```text
$ mkdir foo
[no output; exit 0]
```

Capture is full combined PTY output, sanitized to plain text.

Quiet mode never changes capture.

## 11. Interactive programs

Full-screen/interactive programs are not automatically captured.

Future configurable hotkey:
- captures current logical screen;
- strips styles/cursor metadata;
- updates clipboard once.

Normal and alternate screen states remain separate.

No promise to capture outer terminal scrollback.

Blank rows are trimmed only at top/bottom; internal blank rows remain.

Interactive whitelist uses exact executable basename matching.

`sudo` / `doas` inspect the next executable.

No arbitrary recursive wrapper parsing.

User config replaces built-in whitelist rather than merging.

## 12. Configuration

Hierarchy:

```text
built-in defaults
    ↓
/etc/cvb/config.toml
    ↓
~/.config/cvb/config.toml
```

User wins.

Lists replace rather than merge.

Example:

```text
/usr/share/doc/cvb/config.toml.example
```

Config is read when resident machinery starts; changes apply on the next `cvb on`.

No active user config is installed by default.

## 13. File/directory bridging

Future:

```text
cvb <path>
```

bridges file/directory contents to clipboard.

No separate `file` / `directory` commands.

Future completion may use optional `fzf`, with Bash owning completion/selection and CVB doing the bridge.

## 14. History

Future resident history:
- child Bash gets private/temporary history;
- session history is handed back to parent on exit;
- avoids duplicate pre-session history.

Exact import mechanism is not settled.

Do not implement yet.

## 15. One-shot mode

Future:

```text
cvb <command> [args...]
```

must preserve Bash semantics where needed.

The external binary cannot see parent aliases/functions directly. The shell wrapper/state handoff mechanism is the model to use.

Do not reimplement Bash parsing in Rust.

## 16. Current source status

```text
src/cli.rs          implemented
src/control.rs      implemented + tested
src/shell_state.rs  implemented
src/shell.rs        implemented enough for current child setup; cargo check passes
src/pty.rs          PTY/control integration working
data/cvb.bash       parent state serialization wrapper
data/cvb-control.bash newly extracted; next thing to test
```

Current test baseline:

```text
12 tests passed
0 failed
```

## 17. Development discipline

For CVB:
- work one atomic change at a time;
- compile/test after each change;
- user is not a proficient Rust programmer;
- give exact file/path and replacement instructions;
- use whole-file replacement when practical/safest;
- use ordinary Markdown code blocks for source code, not writing blocks;
- do not dump unnecessarily huge code;
- test actual behavior rather than trusting plausible-looking code;
- do not redesign settled architecture without a concrete reason.

## 18. Immediate next step

Do **not** implement more architecture yet.

Start the next session by testing `data/cvb-control.bash`.

Minimum command cases:

```text
echo hello
true
false
echo one && echo two
echo one ; echo two
echo one | cat
result=$(printf hello)
result=$(true)
sleep 0.1 &
echo done
function foo { echo foo; }; foo
```

Verify:
- exactly one START per top-level command;
- exactly one END;
- IDs increment correctly;
- END status is correct;
- command text is the original complete input line;
- no hook recursion;
- no duplicate START/END.

Only after this is reliable should clipboard/capture integration proceed.

---

# CONTINUATION PROMPT

Paste this into a future ChatGPT instance:

> Continue my Rust project **CVB (Capture → View → Bridge)** at `~/Projects/cvb`.
>
> CVB is an Arch Linux/Wayland/COSMIC terminal utility for reliable **LLM ↔ human ↔ terminal ↔ clipboard** exchange. Its guiding test is: **Does this make the exchange more reliable, more useful, or less intrusive?**
>
> The architecture is already designed and should not be restarted unless implementation reveals a concrete flaw.
>
> Current architecture:
> - `cvb on` creates a child interactive Bash inside a PTY.
> - CVB proxies terminal I/O.
> - A Unix `socketpair()` provides a private bidirectional control channel.
> - Child Bash gets the control socket as FD 3.
> - Child Bash loads normal `.bashrc`, serialized parent shell state, then `data/cvb-control.bash`.
> - Parent state includes aliases, functions, shell options and shopt options.
> - Command boundaries use Bash DEBUG for START and PROMPT_COMMAND for END.
> - START contains the complete original Bash input line.
> - END contains command ID and signed exit status.
> - Rust must not reconstruct argv or parse/normalize shell syntax.
> - Clipboard capture will eventually occur exactly once after authoritative END.
> - Interactive/full-screen programs are not automatically captured; a future hotkey captures whitelisted screens.
>
> Current implementation:
> - `src/cli.rs`: done.
> - `src/control.rs`: done and tested.
> - `src/shell_state.rs`: done.
> - `src/shell.rs`: child rcfile/control-script loading works; `cargo check` passes.
> - `src/pty.rs`: PTY proxy, resize and control socket integration work.
> - `data/cvb.bash`: parent-shell state serialization wrapper exists.
> - `data/cvb-control.bash`: newly extracted Bash hook; **this is the immediate unfinished area**.
>
> Important: a previous `OnceLock::get_or_try_init` implementation was removed because it is unstable in this Rust toolchain. Do not reintroduce it.
>
> **Immediate next task:** test `data/cvb-control.bash` in the real child Bash. Verify exactly one START and one END per top-level command, correct IDs, original command text and signed exit status. Pay special attention to DEBUG trap recursion/repetition and preservation of existing `PROMPT_COMMAND`.
>
> Do not implement clipboard/capture integration until the Bash control protocol is proven correct.
>
> Work incrementally: one atomic change at a time, compile/test between changes. Give exact practical Rust editing instructions. For CVB source code, use ordinary Markdown code blocks, never writing blocks.

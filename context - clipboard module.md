# CVB — Session Context / Memory Core

## What this document is

This document initializes a future ChatGPT session working on the Rust project **CVB**. Treat it as the authoritative working context for continuing development.

The user calls the assistant **“chadgpt.”**

The user is not proficient in Rust. Give instructions as one atomic change at a time, preferably with an exact editor search line and a complete replacement block/function or whole-file replacement. After each change, compile/test before moving on.

For source code, use ordinary Markdown code blocks without language tags. For terminal commands, provide plain commands. Do not append `| wl-copy` or similar clipboard plumbing to commands.

The user prefers direct, practical collaboration with minimal ceremony.

---

# Project

**CVB** = **Capture → View → Bridge**

Repository:

`https://github.com/Mel34/cvb`

Working directory:

`~/Projects/cvb`

CVB is a Rust terminal-to-LLM exchange utility descended from the user's older C utility `wcp`.

Its purpose is reliable exchange between:

* the terminal
* terminal command output
* terminal-screen contents
* files/directories
* the Wayland clipboard
* an LLM

CVB is explicitly **not** intended to be:

* a shell replacement
* a terminal logger
* a terminal database
* an AI agent

---

# Current release / development state

Current project version is **0.2.0** in Cargo metadata.

The eventual target for the currently developed feature set is **CVB 0.3.0**.

Important clean commits:

`a99c608` — Add physical hotkey detection and EIS input injection

`8bf1b4f` — Add glob support for target paths

These commits provide safe rollback points.

Do NOT blindly restore `pty.rs` to these commits because the later `vt100` terminal-emulation work is valuable and must be retained.

---

# Existing CVB capabilities

CVB currently has:

1. Resident Bash session started by `cvb on`
2. `cvb off`
3. Command capture
4. Command-output postprocessing
5. File/path bridging
6. Multiple paths
7. Directory inventories
8. Ignore list at:
   `~/.config/cvb/ignore`
9. Rust-side glob handling
10. Passive physical keyboard observation
11. `Ctrl+Shift+A` detection
12. COSMIC RemoteDesktop portal access
13. EIS keyboard input injection
14. Terminal screen emulation using `vt100`

The shell runs in a PTY.

The PTY dimensions are synchronized with the terminal and SIGWINCH is already handled.

---

# Physical keyboard hotkey

CVB passively observes the physical keyboard through `/dev/input/event2` using `evdev`.

It does NOT grab the keyboard.

The hotkey is:

**Ctrl+Shift+A**

Relevant key codes:

* Ctrl: 29 / 97
* Shift: 42 / 54
* C: 30
* A detection is the current hotkey path

Polling was deliberately made nonblocking using:

`PollTimeout::ZERO`

This allows keyboard observation while the PTY remains responsive.

The hotkey has been proven to work while `micro` is active.

---

# EIS / RemoteDesktop input injection

CVB uses the COSMIC/Wayland RemoteDesktop portal and EIS to inject keyboard input.

The portal registration uses:

`io.github.mel34.cvb`

The portal now identifies the application as:

**CVB wants to control your…**

instead of:

**Unknown Applications wants to…**

This is intentional and is considered solved.

The desktop file is:

`io.github.mel34.cvb.desktop`

Contents:

```text
[Desktop Entry]
Type=Application
Name=CVB
Comment=Capture, View, and Bridge terminal content
Exec=cvb
Terminal=true
NoDisplay=true
X-GNOME-UsesNotifications=false
```

The desktop file is intended to ship with the 0.3.0 release.

The current distribution PKGBUILD installs it under:

`/usr/share/applications/io.github.mel34.cvb.desktop`

Do not revisit the portal identity work unless something actually breaks.

---

# Terminal screen capture

This is the major 0.3.0 feature.

The correct architecture is:

## Ordinary commands

Existing command capture remains responsible for ordinary command output.

## Interactive/TUI applications

The PTY byte stream is simultaneously fed into a terminal emulator.

CVB uses:

**`vt100 = "0.16"`**

The parser maintains the current virtual terminal screen, including:

* cursor position
* line contents
* ANSI/VT escape processing
* alternate screen
* TUI applications

The PTY currently does:

```text
PTY bytes
   ↓
vt100::Parser
   ↓
virtual terminal screen
```

The parser is initialized with the PTY's current row/column dimensions.

The master PTY branch calls:

```text
terminal.process(&buffer[..count]);
```

for every PTY read.

---

# Screen extraction

A helper named:

`screen_to_text()`

converts the visible `vt100::Screen` into plain text.

Its behavior:

* iterate rows
* iterate columns
* concatenate cell contents
* trim trailing spaces from each line
* remove trailing blank rows
* join rows using `\n`

It intentionally returns **plain text**, not an image.

This was tested successfully with `micro`.

A real live screen capture produced:

```text
alternate screen: true
rows: 29
cols: 147
text length: 886
text:
  1 .TH CVB 1 "September 2026" "CVB 0.2.0" "User Commands"
  2 .SH NAME
  3 cvb \- terminal-to-LLM exchange utility
  ...
 27 .B on
cvb.1 (1,1) | ft:man | unix | utf-8 ...
```

Therefore:

**vt100 screen tracking is proven to work.**

The `micro` alternate screen is correctly represented.

Unit test:

`screen_to_text_extracts_visible_content`

passes.

---

# Test status

Before the clipboard redesign, the complete test suite reached:

**20 passed, 0 failed**

The tests include:

* control protocol encoding/decoding
* partial frames
* multiple frames
* output postprocessing
* CR handling
* backspace handling
* CSI stripping
* OSC stripping
* empty-output marker
* screen-to-text extraction
* ignore-list parsing
* directory inventory
* multiple files
* file + directory handling

The latest known test result was:

```text
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

---

# Current clipboard problem

CVB currently uses external `wl-copy`.

There are two independent places that spawn it:

1. ordinary command capture
2. hotkey screen capture

The screen capture path currently:

1. detects Ctrl+Shift+A
2. gets `terminal.screen()`
3. calls `screen_to_text()`
4. spawns `wl-copy`
5. writes the text to its stdin
6. waits for `wl-copy`

This has produced confusing behavior.

Diagnostics conclusively established that the problem is NOT:

* keyboard detection
* `vt100`
* alternate-screen handling
* screen extraction
* the actual text passed to the clipboard

During a live `micro` test:

`/tmp/cvb-hotkey-debug` contained:

```text
hotkey detected
```

`/tmp/cvb-screen-debug` contained the correct visible `micro` screen.

`/tmp/cvb-clipboard-debug` contained the exact same correct screen contents.

Therefore the data reaching the clipboard-writing stage was correct.

The expected `/tmp/cvb-wl-copy-debug` was never created.

The code at that point was waiting for:

```text
child.wait()
```

This strongly indicates that treating `wl-copy` as a short-lived producer process is the wrong abstraction.

`wl-copy` is fundamentally a Wayland clipboard owner and can remain alive while providing the selection.

The earlier hypothesis that `screen_to_text()` might be failing was disproven. Do not revisit that hypothesis.

---

# Architectural decision: replace wl-copy

The project has now decided to **stop using external `wl-copy` for CVB's clipboard functionality**.

Instead, CVB will implement its own **small, in-process, text-only Wayland clipboard owner**.

This is now the intended direction.

Do NOT continue adding diagnostics to `wl-copy`.

The goal is not to clone the complete `wl-copy` utility.

The goal is a narrow CVB-specific clipboard component supporting:

* Wayland clipboard selection
* UTF-8 text
* appropriate text MIME types
* clipboard ownership
* responding when another application requests the text
* remaining alive as the clipboard owner
* detecting when ownership is lost
* clean shutdown

No need initially for:

* primary selection
* X11
* file copying
* arbitrary MIME formats
* clipboard persistence modes
* command-line interface
* compatibility features unrelated to CVB

The implementation should ideally become something conceptually like:

```text
capture screen
      ↓
screen_to_text()
      ↓
clipboard::set_text()
      ↓
Wayland data source
      ↓
compositor
```

The same clipboard module should eventually serve both:

* ordinary command-output capture
* terminal-screen capture

This removes the duplicated `wl-copy` subprocess logic.

---

# Desired clipboard API

The exact API has not yet been chosen.

A likely conceptual API is:

```text
clipboard::set_text(text)
```

or a small clipboard-owner object.

The implementation must account for the fact that the clipboard data source needs to remain available after `set_text()` returns.

A future implementation should therefore likely be tied to CVB's existing main event loop rather than launching a detached helper process.

The key design question is:

**How should clipboard ownership/event processing coexist with CVB's existing PTY/control/keyboard polling loop?**

Prefer an architecture that integrates cleanly rather than introducing another uncontrolled background process.

---

# Important Wayland clipboard concept

The clipboard is not simply:

```text
write bytes → process exits
```

Instead, an application becomes the selection owner and provides the data when another client requests it.

Therefore the future implementation must understand the Wayland data-device/data-source lifecycle.

The expected sequence is roughly:

```text
CVB
  ↓
create data source
  ↓
advertise text MIME type(s)
  ↓
set selection
  ↓
remain available
  ↓
another client requests MIME type
  ↓
CVB sends text through the provided fd
  ↓
ownership eventually changes
  ↓
CVB cleans up
```

The exact Rust API depends on the chosen Wayland crate.

---

# Existing dependencies

Relevant dependencies include:

* ashpd
* evdev
* nix
* tokio
* glob
* vt100

The project already has Wayland-related functionality through the portal/EIS stack.

Before introducing another dependency, inspect the existing dependency tree and consider whether an already-used crate can provide the needed low-level Wayland functionality.

Do not invent APIs.

If using a new crate, verify its current API/documentation before coding.

---

# Debug cleanup

The current diagnostic scaffolding is temporary and should be removed.

Temporary files used during investigation:

* `/tmp/cvb-hotkey-debug`
* `/tmp/cvb-screen-debug`
* `/tmp/cvb-clipboard-debug`
* `/tmp/cvb-wl-copy-debug`

Temporary diagnostic writes in `pty.rs` should be removed once the clipboard redesign begins.

Do NOT remove the actual `vt100` integration or its unit test.

The debug code is disposable; the terminal emulator work is not.

Because the last clean checkpoints are committed, use Git to help identify/remove diagnostic-only changes, but do not restore `pty.rs` wholesale to the older commits.

---

# Current pty architecture

`run_pty()`:

* creates signal pipe
* creates control socketpair
* makes control nonblocking
* installs SIGWINCH handling
* obtains terminal size
* initializes tokio runtime
* initializes `Input`
* initializes `HotkeyMonitor`
* forks PTY
* parent calls `proxy()`

`proxy()` maintains:

* stdin
* stdout
* PTY master
* signal pipe
* control channel
* command output buffer
* `vt100::Parser`
* command capture state
* hotkey monitoring

PTY reads are sent both to:

```text
vt100 parser
```

and:

```text
real stdout
```

When command capture is active, PTY bytes are also accumulated into the command-output buffer.

---

# Ordinary command output

Existing command output postprocessing:

* strips OSC sequences
* strips CSI sequences
* preserves carriage returns
* preserves backspaces
* returns `[CVB: no output]\n` for empty output

Do not casually merge terminal-screen extraction with ordinary command-output postprocessing.

They serve different purposes:

**command output** = PTY byte stream processed as command output

**screen capture** = rendered virtual terminal state

---

# 0.3.0 feature set

The intended 0.3.0 feature set is:

1. command capture
2. file/path bridging
3. glob support
4. passive physical hotkey detection
5. EIS input injection
6. terminal-screen capture
7. text-only clipboard ownership

The terminal-screen feature must work with interactive applications such as:

* `micro`
* `htop`
* other alternate-screen/TUI applications

No screenshots.
No OCR.
No image clipboard.

Everything remains text.

---

# Immediate next task

Do NOT start by changing random parts of `pty.rs`.

First:

1. Remove the temporary debug scaffolding.
2. Preserve the working `vt100` parser and `screen_to_text()`.
3. Introduce a dedicated clipboard module.
4. Determine the most appropriate Rust Wayland API/crate for a minimal text-only clipboard owner.
5. Implement the smallest working clipboard ownership path.
6. Compile/test.
7. Integrate it into the existing CVB event architecture.
8. Then use it for both screen capture and ordinary command capture.
9. Test with `micro` while it is still open.
10. Test ordinary command output.
11. Test clipboard replacement by another application.

Continue one atomic change at a time.

---

# Guiding principle

The project has reached the point where the individual pieces are proven:

```text
physical hotkey        ✅
EIS injection          ✅
PTY                    ✅
vt100 screen tracking  ✅
TUI screen extraction  ✅
20 tests               ✅
```

The remaining problem is the clipboard ownership mechanism.

The decision is:

**CVB should own its own text clipboard rather than outsourcing clipboard ownership to `wl-copy`.**

Build the smallest correct Wayland clipboard machine necessary for CVB, keep it text-only, keep it in-process, and make it a reusable part of CVB's architecture.

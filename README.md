# CVB

**Capture → View → Bridge**

CVB is a small terminal utility for reliable exchange between a human, the terminal, the clipboard, and an LLM.

It can:

* run a resident Bash session that captures completed command output;
* capture the currently visible terminal screen with a configurable hotkey;
* bridge a file directly to the clipboard;
* recursively inventory a directory as TSV and copy the result to the clipboard;
* search source files and retrieve relevant code with `cvb pull`;
* apply pasted patches transactionally, with review and failed-hunk recovery;
* execute external commands through the normal command-line interface.

CVB is designed around simple, predictable mechanisms. It is not an AI agent, shell replacement, terminal logger, or general-purpose terminal emulator.

## Requirements

* Linux
* Bash
* Wayland
* `wl-clipboard`
* `git`, for `cvb pull` and for pasted unified diffs
* `apply_patch` in `PATH`, for pasted `apply_patch` patches
* the libraries `libinput`, `libudev` and `libcanberra` (CVB links them; building needs their development files, for example `libinput-dev`, `libudev-dev` and `libcanberra-dev` on Debian and Ubuntu)
* Rust/Cargo when building from source

## Building

Clone the repository and build with Cargo:

```text
cargo build --release
```

The resulting binary is:

```text
target/release/cvb
```

## Resident capture session

Running `cvb` with no arguments starts an interactive child Bash session.

Source the CVB Bash wrapper first:

```text
source data/cvb.bash
```

Then:

```text
cvb
```

The active CVB session is indicated by a red dot in the prompt.

Commands entered inside the session are captured when they complete. Their visible terminal output is copied to the Wayland clipboard.

To end the session, type `exit` at its prompt.

The parent shell is left running normally after the CVB child session exits. Commands entered during the session are added to the parent shell's history.

### Shell state

When a session starts, CVB transfers useful parent-shell state into the child Bash session, including:

* aliases
* shell functions
* shell options
* `shopt` settings
* inherited environment variables

CVB does not attempt to clone transient shell internals such as arbitrary traps.

## Screen capture

While a resident CVB session is running, CVB maintains a representation of the terminal screen as it is displayed.

Press the configured screen-capture hotkey to copy the currently visible terminal screen to the Wayland clipboard.

The default binding is:

```text
Ctrl+Shift+Space
```

Screen capture works for both ordinary shell sessions and full-screen interactive terminal applications such as `htop` and `micro`.

The captured text represents the terminal's current visible screen rather than command history or terminal scrollback.

### Keybind configuration

CVB reads screen-capture bindings from:

```text
~/.config/cvb/keybind
```

If `XDG_CONFIG_HOME` is set, the file is instead located at:

```text
$XDG_CONFIG_HOME/cvb/keybind
```

If the file does not exist, CVB creates it with the default binding.

Each non-empty, non-comment line defines one screen-capture binding. Multiple bindings are allowed.

For example:

```text
Ctrl+Shift+Space
Ctrl+Shift+N
```

Supported modifiers are:

* `Ctrl`
* `Shift`
* `Alt`
* `Super`

The key is separated from modifiers with `+`.

Named special keys include:

* `Enter`
* `Escape`
* `Tab`
* `Space`
* `Backspace`
* `Delete`
* `Insert`
* `Home`
* `End`
* `PageUp`
* `PageDown`
* `Left`
* `Right`
* `Up`
* `Down`
* `F1` through `F24`

The following punctuation keys can be named explicitly:

* `Plus`
* `Minus`
* `Comma`
* `Period`
* `Slash`
* `Backslash`
* `Semicolon`
* `Apostrophe`
* `LeftBracket`
* `RightBracket`
* `Backtick`

Blank lines and lines beginning with `#` are ignored.

The configuration is watched while CVB is running. Changes take effect without restarting the resident session.

CVB observes the configured key combinations but does not consume the underlying keyboard events. The terminal or application may therefore also receive the key combination.

## File bridging

An existing filesystem path takes precedence over an external command.

For example:

```text
cvb /path/to/file.txt
```

copies the complete file contents to the clipboard.

Relative and absolute paths are supported.

## Directory bridging

A directory is recursively represented as TSV:

```text
path	type	size
one.txt	file	4
subdir	dir	-
subdir/two.txt	file	4
```

Paths are relative to the selected directory. Regular files report their byte size; directories use `-`.

Symlinks and other filesystem object types are currently skipped.

For example:

```text
cvb ~/Projects/cvb
```

copies the resulting inventory to the clipboard.

### Ignore list

CVB creates `~/.config/cvb/ignore` on first directory inventory. If `XDG_CONFIG_HOME` is set, the file is instead located at `$XDG_CONFIG_HOME/cvb/ignore`.

The file contains one directory name per line. Blank lines and lines beginning with `#` are ignored. A directory name is excluded wherever it occurs in the directory tree.

The initial file excludes common version-control directories:

```text
.git
.hg
.svn
.bzr
.jj
```

The file is the complete source of truth: remove an entry to include that directory, or add entries such as `target` or `node_modules` to exclude them.

CVB does not interpret glob patterns, regular expressions, or `.gitignore` syntax.

## Searching code with `cvb pull`

`cvb pull` searches the files of the Git repository, tracked or untracked but not ignored, and prints line-numbered units: the smallest enclosing function, type or heading section for Rust, Bash, Python, Zig, C, C++, JavaScript, TypeScript, Go, Java, Lua and Markdown, and plain lines for any other file.

```text
cvb pull foo bar          units containing foo OR bar
cvb pull +foo +bar        units containing all + terms
cvb pull foo -test        leave out units containing test
cvb pull file:PATH        the whole file
cvb pull file:A file:B foo  search the named files
cvb pull file:PATH head:50  first 50 lines (tail:N: last N)
cvb pull file:PATH lines:10-40
cvb pull file:PATH lines:10-300 foo  hits inside the range, whole units returned
cvb pull callers:NAME     syntactic call sites (every supported language except Markdown)
cvb pull +foo cascade:2   also the units the matches depend on
```

Matching is case-sensitive; `lines:` cannot be combined with search terms, and only one `file:` scope is allowed. The operating manual (`manual.md`) describes the output and the limits in full.

## Pasting patches

Inside a CVB session, paste a patch at the shell prompt, either in `apply_patch` format (`*** Begin Patch` ... `*** End Patch`) or as a unified diff, the output of `git diff` (`diff --git a/... b/...`, or `--- a/...` followed by `+++ b/...`). CVB applies an `apply_patch` patch with your installed `apply_patch` and a diff with `git apply` (both found in `PATH`), prints the result, copies it to the clipboard and draws the prompt again. You type no command, and nothing is added to your history.

```text
CVB patch APPLIED files=2
Success. Updated the following files:
M /home/you/project/src/main.rs
A /home/you/project/src/new.rs
```

- **All or nothing.** `apply_patch` itself can leave a half-applied tree when a later file fails. CVB runs it on a scratch copy and commits the result as one transaction (journal, project lock, rollback). If anything fails, your files are untouched: the clipboard holds `CVB patch FAILED (apply_patch exit N)`, `apply_patch`'s own output with the paths of your real tree, and `No files were modified.`
- **What is supported.** `Update File`, `Add File`, `Delete File` and `Move to`; in a diff: modified, new (with the mode it states, such as `100755`), deleted and renamed files. `Add File` (or a new file in a diff) is refused when the path already exists. A move keeps the file's mode. Files must be UTF-8 text.
- **Diffs.** The paths need the default `a/` and `b/` prefixes. Refused with a reason: binary patches, copies, symbolic links and submodules, mode changes, merge diffs and quoted file names. LLMs often miscount the line counts in the `@@` headers, so CVB does not trust them: it recounts every hunk from its lines, rewrites only the headers that were wrong and says so in the report (`Note: corrected the line counts of N hunk headers ...`). A diff has no end marker, so a paste cut inside a hunk is noticed only when the hunk has no lines at all; check the diff if it came through a lossy channel.
- **Where it applies.** The patch root is the shell's current directory. Paths must be relative, without `..` or a `.git` component, and must not pass through a symlink. A patch that touches a file someone else changed while it was being applied is refused.
- **Only at a prompt.** A paste is examined only while the shell is waiting for a command, not inside an editor, `ssh` or a running program. A paste that is not exactly one patch reaches the shell unchanged. A patch without its `*** End Patch` line, or a diff that ends right after a hunk header, is refused and never reaches the shell.
- **Pasted twice?** When a patch fails because its changes are already in the files, the failure text says so (`Note: every hunk of this patch appears to be already present ...`). The note is only an explanation; the outcome is still FAILED with nothing modified.
- **Refusals** read `CVB patch REJECTED: reason`.

This needs a terminal that sends bracketed paste (most do; Bash enables it by default). Set `CVB_PATCH_PASTE=off` to turn the feature off.

**Review before applying.** With `CVB_PATCH_REVIEW=on` (also `1`, `true`, `yes`) a recognized patch is shown instead of applied: its text (the first and last 150 lines when it is longer than 300), a summary line naming the files and what happens to each, and any correction CVB made. Nothing is changed and the clipboard is untouched. Press `Ctrl+B` twice to apply it; any other key cancels, and that key goes on to the shell. Pasting another patch while one waits replaces it. Reading the end of the shown text is also how to notice a patch the clipboard cut short.

**When hunks do not match.** If a patch fails because some hunks do not match the files (for example, a line was misquoted or the file moved), CVB reports the failure and changes nothing. It then offers up to five candidate locations for each failing hunk. Candidates where only context lines differ come first; places where a line to be removed differs or is missing are offered only when there is nothing better, and the screen says how many were left out. Use Left/Right (or Up/Down) to browse candidates and Enter to select the displayed candidate; number keys select a candidate directly. The complete candidate comparisons are also sent through the clipboard, so they can be pasted into an LLM conversation for semantic review. Candidates are suggestions, not automatic decisions: textual similarity does not prove that a location is correct. After all choices, CVB shows the corrected patch and previews its effect. If a line to be removed differs from the one the patch named, CVB warns that the actual file line will be removed; if a line to be removed is not in the file at all, CVB warns that this removal is dropped and nothing is deleted for it. Press `Ctrl+B` twice to apply the corrected patch. If a file the patch changes has different contents from when the candidates were shown (compared by content, so touching a file does not count), CVB refuses to preview or apply: it reports `CVB patch REJECTED: <file> changed since the candidates were shown`, changes nothing, and closes the picker, so the patch has to be pasted again. `s` skips and cancels the whole patch; `q`, Escape, or an unrecognized key cancels and passes the remaining input to the shell. There is no overall inactivity timeout; Escape is disambiguated from a split arrow-key sequence with a short 30 ms wait. This recovery works for `apply_patch` patches and unified diffs.

## External commands

When the target is not an existing filesystem path, CVB executes it as an external command:

```text
cvb cargo test
```

CVB does not attempt to reimplement Bash parsing. Shell syntax that requires Bash remains the responsibility of Bash.

Common terminal pagers are disabled while executing commands so their output can be captured directly.

## Target resolution

CVB intentionally gives existing filesystem paths precedence over external commands.

Thus, if `Cargo.toml` exists in the current directory:

```text
cvb Cargo.toml
```

bridges the file.

If it does not exist, the same invocation is treated as an external command named `Cargo.toml`.

Explicit paths such as `./foo`, `../foo`, and `/tmp/foo` are path targets.

## Clipboard

CVB uses `wl-copy` for clipboard integration.

When a captured command produces no visible terminal output, CVB copies a message reporting success or failure, including the exit status. For example:

```text
The command completed successfully (exit 0) and produced no visible output.
The command failed (exit 1) and produced no visible output.
```

Visible terminal output is preserved and labeled `Terminal output:`.

### Skip list

Some commands, such as `cd`, normally print nothing. Commands listed in the skip list leave the clipboard untouched when they produce no visible output.

CVB creates `~/.config/cvb/skip` when a resident session starts. If `XDG_CONFIG_HOME` is set, the file is instead located at `$XDG_CONFIG_HOME/cvb/skip`.

The file contains one command name per line. Blank lines and text after `#` are ignored. The initial file lists `cd`.

With `cd` listed:

```text
cd existing-directory      clipboard unchanged
cd /nonexistent            clipboard: bash: cd: /nonexistent: No such file or directory
cd existing && ls          clipboard: output of ls
```

A command line is skipped only when every top-level command in it is listed (matched by its first word) and the line produced no visible output. Any output, including error messages, is captured as usual. Commands inside functions or compound commands such as `if` are not matched; those lines are captured normally.

The file is read on every command, so edits take effect immediately in running sessions. Screen captures are not affected.

Screen captures copy the textual contents of the currently visible terminal screen.

## CLI

```text
cvb
cvb pull [file:PATH]... [cascade:N] <search-term>...
cvb pull file:PATH [head:N|tail:N]
cvb pull file:PATH lines:N|START-END... [head:N|tail:N]
cvb pull file:PATH lines:N|START-END... <search-term>...
cvb pull callers:NAME [file:PATH]...
cvb context
cvb --help
cvb --version
cvb <path>
cvb <command> [args...]
```

`cvb` with no arguments starts a resident session (see above); `cvb context` prints the operating manual that a session puts on the clipboard when it starts.

There are intentionally no separate `file` or `directory` subcommands.

## Installation

For an Arch Linux package, the project provides a PKGBUILD in the separate packaging repository.

The installed Bash wrapper is intended to be sourced by the user's Bash environment rather than modifying shell startup files automatically.

## License

CVB is free software licensed under the GNU General Public License, version 3 or later.

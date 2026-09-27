# CVB

**Capture → View → Bridge**

CVB is a small terminal utility for reliable exchange between a human, the terminal, the clipboard, and an LLM.

It can:

* run a resident Bash session that captures command output and copies each completed command to the Wayland clipboard;
* capture the currently visible terminal contents with **Ctrl+Shift+A**;
* bridge a file directly to the clipboard;
* recursively inventory a directory as TSV and copy the result to the clipboard;
* execute external commands through the normal command-line interface.

CVB is designed around simple, predictable mechanisms. It is not an AI agent, shell replacement, terminal logger, or general-purpose terminal emulator.

## Requirements

* Linux
* Bash
* Wayland
* `wl-clipboard`
* `libcanberra`
* Rust/Cargo when building from source

## Building

Clone the repository and build with Cargo:

```
cargo build --release
```

The resulting binary is:

```
target/release/cvb
```

## Resident capture session

The `cvb on` mode starts an interactive child Bash session.

Source the CVB Bash wrapper first:

```
source data/cvb.bash
```

Then:

```
cvb on
```

The active CVB session is indicated by a red dot in the prompt.

Commands entered inside the session are captured when they complete. The combined terminal output of each command is copied to the Wayland clipboard.

To end the session:

```
cvb off
```

The parent shell is left running normally after the CVB child session exits. Commands entered during the session are added to the parent shell's history.

### Terminal snapshot

While a resident capture session is active, **Ctrl+Shift+A** captures the currently visible terminal contents.

CVB does not take a graphical screenshot. Instead, it coordinates with the terminal emulator to obtain its textual representation of the visible terminal:

1. CVB detects Ctrl+Shift+A through the Wayland Input Capture portal.
2. CVB temporarily pauses command-output capture and injects Ctrl+C.
3. The terminal emulator creates its textual terminal snapshot and places it on the Wayland clipboard.
4. CVB detects the resulting Wayland clipboard selection event.
5. CVB plays the configured `screen-capture` sound, then resumes capture.

This preserves terminal text, including formatting represented by the terminal emulator, rather than producing an image.

The terminal snapshot feature requires a compositor and terminal emulator that support the corresponding input-capture and clipboard protocols.

### Shell state

When a session starts, CVB transfers useful parent-shell state into the child Bash session, including:

* aliases
* shell functions
* shell options
* `shopt` settings
* inherited environment variables

CVB does not attempt to clone transient shell internals such as arbitrary traps.

## File bridging

An existing filesystem path takes precedence over an external command.

For example:

```
cvb /path/to/file.txt
```

copies the complete file contents to the clipboard.

Relative and absolute paths are supported.

## Directory bridging

A directory is recursively represented as TSV:

```
path	type	size
one.txt	file	4
subdir	dir	-
subdir/two.txt	file	4
```

Paths are relative to the selected directory. Regular files report their byte size; directories use `-`.

Symlinks and other filesystem object types are currently skipped.

For example:

```
cvb ~/Projects/cvb
```

copies the resulting inventory to the clipboard.

### Ignore list

CVB creates `~/.config/cvb/ignore` on first directory inventory. If `XDG_CONFIG_HOME` is set, the file is instead located at `$XDG_CONFIG_HOME/cvb/ignore`.

The file contains one directory name per line. Blank lines and lines beginning with `#` are ignored. A directory name is excluded wherever it occurs in the directory tree.

The initial file excludes common version-control directories:

```
.git
.hg
.svn
.bzr
.jj
```

The file is the complete source of truth: remove an entry to include that directory, or add entries such as `target` or `node_modules` to exclude them.

CVB does not interpret glob patterns, regular expressions, or `.gitignore` syntax.

## External commands

When the target is not an existing filesystem path, CVB executes it as an external command:

```
cvb cargo test
```

CVB does not attempt to reimplement Bash parsing. Shell syntax that requires Bash remains the responsibility of Bash.

Common terminal pagers are disabled while executing commands so their output can be captured directly.

## Target resolution

CVB intentionally gives existing filesystem paths precedence over external commands.

Thus, if `Cargo.toml` exists in the current directory:

```
cvb Cargo.toml
```

bridges the file.

If it does not exist, the same invocation is treated as an external command named `Cargo.toml`.

Explicit paths such as `./foo`, `../foo`, and `/tmp/foo` are path targets.

## Clipboard

CVB uses `wl-copy` for normal clipboard output.

For a captured command that produces no terminal output, CVB copies:

```
[CVB: no output]
```

followed by a newline.

For terminal snapshots, CVB uses the Wayland `ext-data-control-v1` protocol to detect the clipboard selection created by the terminal emulator.

## CLI

```
cvb on
cvb off
cvb --help
cvb --version
cvb <path>
cvb <command> [args...]
```

There are intentionally no separate `file` or `directory` subcommands.

## Installation

For an Arch Linux package, the project provides a PKGBUILD in the separate packaging repository.

The installed Bash wrapper is intended to be sourced by the user's Bash environment rather than modifying shell startup files automatically.

## License

CVB is free software licensed under the GNU General Public License, version 3 or later.

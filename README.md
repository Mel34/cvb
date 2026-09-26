# CVB

**Capture → View → Bridge**

CVB is a small terminal utility for reliable exchange between a human, the terminal, the clipboard, and an LLM.

It can:

- run a resident Bash session that captures command output and copies each completed command to the Wayland clipboard;
- bridge a file directly to the clipboard;
- recursively inventory a directory as TSV and copy the result to the clipboard;
- execute external commands through the normal command-line interface.

CVB is designed around simple, predictable mechanisms. It is not an AI agent, shell replacement, terminal logger, or general-purpose terminal emulator.

## Requirements

- Linux
- Bash
- Wayland
- `wl-clipboard`
- Rust/Cargo when building from source

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

The `cvb on` mode starts an interactive child Bash session.

Source the CVB Bash wrapper first:

```text
source data/cvb.bash
```

Then:

```text
cvb on
```

The active CVB session is indicated by a red dot in the prompt.

Commands entered inside the session are captured when they complete. The combined terminal output of each command is copied to the Wayland clipboard.

To end the session:

```text
cvb off
```

The parent shell is left running normally after the CVB child session exits. Commands entered during the session are added to the parent shell's history.

### Shell state

When a session starts, CVB transfers useful parent-shell state into the child Bash session, including:

- aliases
- shell functions
- shell options
- `shopt` settings
- inherited environment variables

CVB does not attempt to clone transient shell internals such as arbitrary traps.

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

For a captured command that produces no terminal output, CVB copies:

```text
[CVB: no output]
```

followed by a newline.

## CLI

```text
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

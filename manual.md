# CVB Manual
## A practical guide for humans and LLMs

CVB means Capture → View → Bridge. It is a Linux terminal utility for copying command output, terminal contents, source code, and file information into the clipboard for use by a human or a large language model (LLM).

CVB is human-directed. It is not an autonomous agent or a replacement for the shell. The human chooses what to capture, what to share, and what changes to make.

## 1. Quick reference

| Task | Command or action |
|---|---|
| Start a capture session | `cvb` |
| End the session | `exit` |
| Capture a screen | Press `Ctrl+Shift+Space` |
| Copy a file | `cvb path/to/file` |
| Copy several files or paths | `cvb path/to/file1 path/to/file2` |
| Inventory a directory | `cvb path/to/directory` |
| Search tracked source | `cvb pull search-term` |
| Show help | `cvb --help` |
| Show version | `cvb --version` |

CVB uses the Wayland clipboard through `wl-copy`. The clipboard receives text, not an image screenshot.

## 2. Capture command output

Start a session:

    cvb

Run commands as you normally would inside the session. When a command completes, CVB copies its visible terminal output to the clipboard.

For example:

    cargo test
    cargo check
    git status --short

Paste the clipboard contents into your LLM conversation when you want help interpreting results or investigating a problem.

End the session with:

    exit

Your parent shell is restored when the session ends.

## 3. Capture the visible screen

Press the default hotkey:

    Ctrl+Shift+Space

CVB copies the current visible terminal screen as text. This is useful for sharing the current state of a terminal or an interactive application such as `htop` or `micro`.

Screen capture is different from command-output capture. It captures the current screen, not a record of the command that produced it.

### Configure the hotkey

The configuration file is normally:

    ~/.config/cvb/keybind

If `XDG_CONFIG_HOME` is set, use the corresponding configuration directory under it.

The file can contain multiple key bindings, one per line. Blank lines and lines beginning with `#` are ignored. Configuration changes are watched while the capture session runs.

The default binding may also reach the terminal or foreground application.

## 4. Capture files and directories

### Copy a file

    cvb src/main.rs

Copies the file's contents to the clipboard.

### Copy multiple paths

    cvb README.md src/main.rs data/cvb.bash

Copies the requested files and directory inventories into one combined output, with headers identifying each path.

### Inventory a directory

    cvb src/

Copies a recursive TSV inventory containing:

- Relative path and entry name
- Entry type and size
- Modified and available creation timestamps
- Symlink status and target
- Permissions, owner, and group

Symlinks are listed rather than followed as directories.

The default ignore list includes `.git`, `.hg`, `.svn`, `.bzr`, and `.jj`.

The ignore-list file is normally:

    ~/.config/cvb/ignore

Use one directory name per line. Blank lines and lines beginning with `#` are ignored. Matching directory names are excluded wherever they occur. This is a name-based list, not `.gitignore` syntax.

Use a directory inventory to share project structure. Capture individual files when their actual contents matter.

## 5. Search source with cvb pull

Run the command from inside the Git repository you want to search.

    cvb pull surgery

CVB searches Git-tracked files and returns matching lines with file and line information. For supported source files, it can include the enclosing function or method. For Markdown, it expands matches to their surrounding section.

### Query operators

- **Bare terms** are alternatives: a result can match any bare term.
- **`+term`** requires that term to be present.
- **`-term`** excludes results containing that term.
- **Quoted phrases** let you search for a phrase, including when using an operator.

For example:

    cvb pull surgery protocol
    cvb pull +surgery +protocol
    cvb pull +surgery -protocol
    cvb pull '"line ending"'
    cvb pull +'"line ending"' +UTF-8

The first query matches either `surgery` or `protocol`. The second requires both terms. The third requires `surgery` and excludes matches containing `protocol`.

Filtering applies to matching records before surrounding Markdown sections are expanded. As a result, an expanded section may still display the excluded word in its surrounding context; that does not necessarily mean a line containing the excluded term matched.

Search terms are literal, and only Git-tracked files are searched. Untracked files are not included. To share an untracked file, capture it directly instead.

## 6. Using CVB with an LLM

CVB bridges information from your environment into the conversation. It does not give the LLM direct access to your computer or repository.

A practical workflow:

1. Describe the problem and the intended result.
2. Use CVB to capture the relevant source, output, or project structure.
3. Paste the clipboard contents into the conversation.
4. Ask for an explanation or a proposed change.
5. Apply one small, reviewable change at a time.
6. Run the relevant checks or tests.
7. Share the actual results and inspect the diff before proceeding.

Choose the capture method that fits the task:

- **Command output:** build results, test failures, errors, and command results.
- **Screen capture:** the current visible terminal state.
- **File capture:** the contents of known files.
- **Directory inventory:** project structure and file metadata.
- **Source search:** matching definitions and surrounding function context.

Provide enough context to answer the question, but avoid unrelated output dumps. If the evidence is insufficient, capture the next relevant file or function.

## 7. Collaboration principles

When helping with CVB or another codebase, an LLM should:

- Treat source code, tests, repository state, and actual command output as evidence.
- Never invent file contents, command results, test results, or repository state.
- Distinguish verified facts from assumptions and proposals.
- Make one atomic change at a time.
- Validate edits and run relevant checks after each change.
- Inspect the resulting diff and preserve unrelated user changes.
- Explain proposed changes and their consequences.
- Report failures honestly.
- Leave consequential design decisions under human control.

A successful build does not by itself prove that runtime behavior is correct. Verify the behavior that matters.

## 8. Basic troubleshooting

**The clipboard did not update**
- Check that `wl-copy` is installed and working in the Wayland session.
- Confirm that the requested command or capture produced content.
- For screen capture, check the configured hotkey.

**Source search found nothing**
- Confirm that you're in the intended Git repository.
- Check the spelling of the literal search phrase.
- Remember that untracked files are excluded.

**A directory inventory is missing entries**
- Check the ignore-list configuration.
- Remember that matching directory names are excluded recursively and symlinks are not followed.

For unexpected behavior, capture the relevant terminal output and share it with the LLM rather than guessing at the cause.

## 9. Final principle

CVB makes it easier to share useful evidence from a terminal with a human or an LLM.

Keep the process simple: capture relevant context, understand the proposed change, verify the result, and keep the human in control.

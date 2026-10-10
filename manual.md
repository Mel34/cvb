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

Use `cvb pull` to find relevant code or documentation in the current Git
repository. It returns matching lines with file and line information, and can
expand those matches to useful surrounding context. This lets an LLM locate
definitions and documentation without needing the entire repository pasted
into the conversation.

Run the command from inside the repository you want to search:

    cvb pull surgery

### Query operators

Queries are case-sensitive and use literal terms and phrases.

- **Bare terms** are alternatives: a result can match any bare term.
- **`+term`** makes a term required.
- **`-term`** excludes matching records containing that term.
- **Quoted phrases** search for a phrase rather than treating its words as
  independent terms.
- Operators can be combined in one query.

Examples:

    cvb pull surgery protocol
    cvb pull +surgery +protocol
    cvb pull +surgery -protocol
    cvb pull '"line ending"'
    cvb pull +'"line ending"' +UTF-8

Interpret these queries as follows:

- `surgery protocol` searches for either term.
- `+surgery +protocol` requires both terms.
- `+surgery -protocol` requires `surgery` and excludes matches containing
  `protocol`.
- `'"line ending"'` searches for the phrase `line ending`.
- `+'"line ending"' +UTF-8` requires both the phrase and `UTF-8`.

Quote phrases so the shell passes them to CVB as a single query phrase.
Keep the operator attached to the term or quoted phrase it modifies.

### Context expansion

A match can be more useful when returned with the code or documentation around
it. CVB supports context expansion for recognized source constructs and
Markdown sections.

- **Rust and shell source:** where supported, a match can expand to its
  enclosing function or method.
- **Markdown (`.md` and `.markdown`):** a match expands to its enclosing
  section, starting at the nearest heading at or above the match and continuing
  through that section's nested subsections until the next heading of equal or
  higher level.
- **Fenced code blocks in Markdown:** heading-like lines inside fenced code
  blocks are not treated as section boundaries.
- **Repeated matches:** matches that resolve to the same Markdown section are
  deduplicated rather than returning that section repeatedly.

For example:

    cvb pull PS0
    cvb pull +history -duplicate
    cvb pull '"query operators"'

Use a narrow query to find the relevant definition, implementation, or
documentation. Context expansion provides nearby structure, but it does not
guarantee that every dependency or caller is included.

### Caller discovery

Use `callers:NAME` to find syntactic call sites for a Rust function or method
name across tracked Rust source files:

    cvb pull callers:run

Restrict the scan to one tracked Rust file with `file:PATH`:

    cvb pull file:src/main.rs callers:run

Results include the file, line, and enclosing function when available.
Function calls are matched by the final name in a path. Method calls are
reported as **method-call candidates**. These are syntactic matches, not
semantic proof that a particular implementation is invoked. Results can
include false positives and may miss indirect calls.

Unreadable or unparseable Rust files produce warnings. Matches are still
reported if the scan is incomplete. If no matches are found during an
incomplete scan, CVB warns that no reliable conclusion can be drawn.

`callers:` accepts one target and an optional `file:PATH` scope. It cannot
be combined with `lines:` or ordinary search terms.

Exit status:

- `0`: at least one caller match was found.
- `1`: the scan completed without finding a match.
- `2`: invalid arguments, a scan failure, or no matches from an incomplete scan.

### Filtering and search scope

Filtering is applied to matching records before Markdown sections are expanded.
An expanded section may therefore contain surrounding context with a word
excluded by `-term`. That does not mean a line containing the excluded term
matched the query.

Search is limited to Git-tracked files in the current repository. Use `file:PATH`
to restrict a search to one exact tracked file:

    cvb pull file:surgery-protocol.md protocol
    cvb pull file:src/surgery.rs +load_originals -test

The `file:PATH` scope can appear alongside the normal query terms and operators.
It restricts which files are searched; `+term` and `-term` continue to filter
matching records as usual. Only one `file:` scope is allowed per command, and
the path must match a Git-tracked file exactly. A missing path produces an
error rather than falling back to a repository-wide search. Directory prefixes
and glob patterns are not supported as file scopes.

To print specific inclusive line ranges from one tracked file, use
`lines:START-END`. A single line can be selected with `lines:LINE`. You can
provide multiple `lines:` selectors in one command. Ranges are printed in the
order requested; overlapping ranges and duplicate lines are preserved. Line
numbers start at 1, and `lines:` cannot be combined with search terms:

    cvb pull file:src/pull.rs lines:96-110
    cvb pull file:src/pull.rs lines:96-110 lines:1300-1335
    cvb pull file:manual.md lines:155

Untracked files and files outside the repository are not searched. To share an
untracked file or a known file's complete contents, capture it directly:

    cvb path/to/file

### Recommended workflow for an LLM

1. **Start with the question.** Search for a distinctive function, symbol,
   error message, protocol name, or documentation phrase.
2. **Combine terms when useful.** Use required terms to narrow the search,
   alternatives to explore related names, and exclusions to remove unwanted
   matches.
3. **Read the returned context.** Use the enclosing function or Markdown
   section to understand the match before proposing a change.
4. **Refine the query.** If the result is too broad or incomplete, search for
   a more specific symbol or phrase.
5. **Capture complete files when needed.** If the relevant logic spans multiple
   functions or files, inspect those files directly rather than assuming the
   search result contains the entire implementation.

Treat `cvb pull` as a repository navigation and context-retrieval tool. Search
results are evidence to inspect, not a substitute for checking the relevant
implementation, callers, tests, and actual command output.

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

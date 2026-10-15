# CVB — LLM Operating Manual

CVB is human-directed. The user runs commands and shares captured output; the LLM has no direct access to the user's machine.

---

# 1. CVB Reference

CVB bridges the terminal session and repository contents to the clipboard for LLM consumption.

---

### Retrieving Information (`cvb pull`)

`cvb pull` performs case-sensitive syntactic search over Git-tracked files. Matching and filtering operate across **syntactic units** (functions, methods, markdown sections), not isolated lines.

#### Command Formats

* `cvb pull QUERY...` — search tracked files for matching syntactic units.
* `cvb pull file:PATH` — capture the complete contents of one Git-tracked file. Path can be relative, absolute, or `./`-prefixed. Empty files output `<path>: file is empty.`.
* `cvb pull file:PATH QUERY...` — restrict unit search to that tracked file. Missing paths error; there is no repository-wide fallback.
* `cvb pull file:PATH lines:N` or `lines:START-END` — capture inclusive line ranges (read once, UTF-8 verified). Multiple line selectors are allowed; overlaps and duplicates are preserved. Do not combine line selectors with search terms.
* `cvb pull callers:NAME` — find syntactic Rust call-site candidates in tracked `.rs` files (optionally scoped with `file:PATH`). Scans for function calls, method candidates, and calls inside macro bodies (e.g. `assert_eq!(foo(), ...)` as `macro-argument candidate`). Results are syntactic candidates, not type-resolved proof.
* `cvb FILE...` — bridge file contents directly to clipboard, including untracked files. Directory arguments produce structured inventories, not file contents.

#### Query Syntax & Boolean Logic

| Pattern | Meaning | Example | Behavior / Scope |
| :--- | :--- | :--- | :--- |
| `term1 term2` | **OR** (alternatives) | `CRLF BOM` | Matches unit if **at least one** bare term appears. |
| `+term` | **AND** (required) | `+parse_query +SearchQuery` | **Must** appear in the unit. Multiple `+` terms can sit on different lines within the same unit. |
| `-term` | **NOT** (exclusion) | `-test -mock` | Excludes the **entire unit** if the term appears anywhere within it. |
| `"phrase"` | **Exact phrase** | `"line ending"` | Matches exact phrase with spaces (shell argument preserved). |
| `"--flag"`, `"-1"` | **Literal `-`/`+`** | `"--dry-run"` or `\--dry-run` | Enclose in quotes or prefix with `\` to search literal terms starting with `-` or `+`. |

*Syntax Rules:*
* **No exclusion-only queries:** `-tests` alone is rejected (`exit 2`); exclusions require at least one positive term.
* **No negated selectors:** `-file:`, `-lines:`, and `-callers:` are rejected (`exit 2`). Exclusions only apply to content terms.

#### Unit Boundaries (Where `+` and `-` Match)

* **Rust:** Smallest enclosing function or method. Struct declarations, enum declarations, and `impl` blocks count as separate units.
* **Bash:** Smallest enclosing `function_definition`.
* **Markdown:** Current heading section, including all nested subsections.
* **Fallback / Non-AST:** Matches single lines. If a parser fails on a syntax error, CVB emits a warning and falls back to line-level matching for that file.

---

### Interpreting `cvb pull` Output & Diagnostics

All `cvb pull` output begins with a parsed query echo on the first line. Check this line first to verify argument boundaries and operator grouping.

#### 1. Success (`exit 0`)
```text
Query: (CRLF OR BOM) AND split_inclusive AND NOT tests
-tests: excluded 2 units
  Rust function: src/pull.rs:120-185
  CRLF: 125, 140
  +split_inclusive: 162
    120 | fn parse(...) {
    ...
```
* **Exclusion Note:** If `-term` filtered any candidates, `-term: excluded N units` precedes the result blocks.
* **Hit Line Header:** Listed above each unit block (e.g. `CRLF: 125, 140`) to explain why the unit matched and reveal term proximity.

#### 2. No Matches (`exit 1`)
```text
Query: (CRLF OR BOM) AND split_inclusive AND NOT tests
No matches.
  CRLF: 12 units, BOM: 3 units, +split_inclusive: 0 units
  -tests: would have removed 4 units
```
Use the per-term count breakdown to guide your retry:
* **`0 units`**: Term does not exist anywhere in the tracked files (check spelling, casing, or naming).
* **`>0 units` for all terms**: Terms exist independently across the codebase, but never co-occur inside the same function/unit. Broaden your query or search terms separately.
* **`would have removed N units`**: The negative filter `-term` killed candidate units that otherwise matched.

#### 3. Search Errors (`exit 2`)
Occurs on malformed selectors, unclosed quotes, exclusion-only queries, or missing tracked file scopes.

---

### Capture and Transcript Behavior

* One capture session can span multiple shell commands until CVB's automatic `END` marker finalizes it.
* Ordinary terminal output and multiple `cvb FILE...` bridge calls can be mixed in one capture. Their transcript sections preserve chronological order.
* Terminal output is labeled `[Terminal output]` and rendered through the terminal emulator. Bridged file contents remain literal; directory arguments contribute inventories.
* Each bridged path has its own labeled section. If a path fails, its error is included in the transcript and processing continues with later paths.
* CVB assembles the transcript and dispatches it to the clipboard at the `END` marker, rather than replacing the clipboard separately for each bridged path.
* Search excludes untracked files. When unit context is insufficient, pull the whole file using `cvb pull file:PATH` or `cvb FILE...`.

## 2. CVB coding protocol

The rules below are the coding protocol for this project. If any rule or requirement is unclear, ask the user rather than guess.

- **Format:** Use plain Markdown and ordinary code fences. Keep commands copyable. Never use WritingBlocks for coding instructions.
- **Atomicity:** Make one logically isolated change at a time. Do not bundle unrelated edits or refactors.
- **Inspect first:** Use CVB-native search and inspection where practical. Consult this manual for syntax. Inspect the current source before editing.
- **Surgical edits:** Use a complete, copyable Python heredoc command. Check exact anchors and occurrence counts before writing; abort on any mismatch. Write atomically, re-read and verify the result, and run follow-up checks from the same script only after edit and post-write verification succeed. Report explicit success or failure with actual check results.
- **Full-file rewrites:** Provide the complete replacement file in an ordinary Markdown code block for the user to replace manually. Never use base64 replacement commands.
- **Validation:** Run relevant `cargo check` and targeted tests; run broader tests when appropriate. Inspect resulting source and diffs, wait for actual output, and never assume success. A successful build alone does not prove runtime correctness.
- **Preservation:** Keep unrelated user changes. Do not overwrite, discard, or reformat existing work unnecessarily.
- **Commands:** Group related commands into one copyable block when practical. Never append `| wl-copy` unless explicitly requested for clipboard testing. Be careful not to overwrite clipboard output needed from an earlier command.
- **Focus:** Keep explanations and output focused. Do not expand scope without reason.
- **Checkpoints:** After stable, tested changes, remind the user to review `git status --short` and commit a clean checkpoint.
- **Evidence:** Never invent source, command output, test results, or repository state. Distinguish verified facts from hypotheses. Keep consequential decisions under human control.
- **Ask rather than guess:** This is the catch-all rule for every uncertainty. If anything is unclear or not 100% certain—including syntax, requirements, source behavior, context, or how to proceed—ask the user for clarification or the relevant context before acting. Never invent details or silently assume.

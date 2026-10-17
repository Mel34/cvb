# CVB — LLM Operating Manual

CVB is human-directed: the user runs commands and shares captured output. The LLM has no direct access to the user's machine. Treat terminal output and bridged files as evidence only of what they actually show.

# 1. CVB Reference

CVB bridges terminal activity and repository content into a transcript that can be shared with an LLM.

## `cvb pull`

`cvb pull` searches Git-tracked files. Matching is case-sensitive and, where supported, operates on syntactic units rather than individual lines. CVB pull: only one file: scope is allowed.

- `cvb pull QUERY...` — search tracked files.
- `cvb pull [file:PATH] [cascade:N] QUERY...` — search, optionally expanding Rust dependencies. 
- `cvb pull file:PATH` — capture one tracked file; empty files are reported as empty.
- `cvb pull file:PATH QUERY...` — restrict search to a tracked file. Invalid paths are errors; don't assume a repository-wide fallback.
- `cvb pull file:PATH lines:N` or `lines:START-END` — capture inclusive line ranges. Multiple selections may preserve overlaps. Don't combine `lines:` with search terms.
- `cvb pull callers:NAME` — find syntactic Rust call-site candidates in tracked `.rs` files; `file:PATH` can scope the scan. Candidates are not proof of resolved runtime calls.
- `cvb FILE...` — bridge file contents, including untracked files. Directory arguments produce structured inventories, not recursive file dumps.

Paths and selectors are command syntax, not search terms. If a command errors, use the error output; don't assume partial results were produced.

### Query syntax

| Syntax | Meaning | Example |
|---|---|---|
| `term1 term2` | OR alternatives | `CRLF BOM` |
| `+term` | Required term; all required terms must occur in the unit | `+parse_query +SearchQuery` |
| `-term` | Exclude units containing the term | `-test -mock` |
| `"phrase with spaces"` | Phrase search | `"line ending"` |
| `"--dry-run"` | Quote a literal term that would otherwise be parsed as an operator | `"--dry-run"` |

Bare terms are OR alternatives; required terms are ANDed with the query. Matching is case-sensitive. Exclusion-only queries are rejected: at least one positive term is required. Negated selectors (`-file:`, `-lines:`, `-callers:`, `-cascade:`) are unsupported; exclusions apply to content terms. `cascade:N` is reserved for cascade control; quote or escape text resembling a directive according to the shell and parser.

### Search units

- **Rust:** smallest enclosing recognized item; functions, methods, types and `impl` blocks may be separate units.
- **Bash:** function definitions where supported.
- **Markdown:** a heading section includes nested subsections until the next heading at the same or higher level.
- **Fallback:** unparseable files may yield line-level results and warnings. A warning means results may be incomplete.

## `cascade:N` dependency expansion

Cascade performs deterministic local expansion from matching Rust units. It indexes Git-tracked Rust source and follows dependencies it can identify syntactically; it does not use an LLM or require network access.

- Depth `0`: seeds only. Depth `1`: dependencies of seeds. Higher depths follow dependencies of newly admitted units up to the requested maximum.
- Valid depths: `0`–`5`; only one `cascade:` directive is allowed.
- `cascade:0` bypasses expansion and its header/report. Results are intended to match a normal pull; don't rely on byte identity without a confirming test.
- Cascade cannot be combined with `lines:`, `callers:`, or a lone `file:PATH` whole-file request.
- With `file:PATH` plus search and cascade, the file scopes initial seeds; resolution may inspect tracked Rust files elsewhere.
- Non-Rust, line-fallback and test-code seeds are not traversed as ordinary Rust seeds.

### Resolver limits

The resolver recognizes selected syntactic relationships, including free-function calls, some associated-function/method calls, and project-defined types in signatures. Resolution depends on syntax, imports and statically inferable receiver types. It is not a Rust type checker or complete compiler call graph.

- Callbacks, unknown receiver types, ambiguous names, external/standard-library calls, trait dispatch, generic constraints and macro expansion may not resolve to a project definition.
- Macro contents may produce candidates, not confirmed targets. Trait implementations are not automatically included merely because a trait appears.
- Test functions and `#[cfg(test)]` code are excluded from the dependency index.
- Unreadable or unparseable tracked Rust files can make the index incomplete; warnings may be emitted.
- High-use utilities called by at least 8 distinct units can be included as leaves: their bodies are included, but their callees are not expanded.

A missing unit does not prove that a dependency doesn't exist. Ambiguous or unresolved relationships may be summarized in the report.

### Cascade budgets

- Maximum depth: **5**.
- Maximum added dependency units: **25**.
- Maximum dependency-unit size: **200 lines**; larger candidates may be omitted with a `cvb pull file:PATH lines:START-END` hint.
- Utility threshold: **8** distinct calling units; qualifying utilities are leaves.
- Maximum cascade addition: **24,000 characters**.
- Report/engine-text reserve: **1,500 characters**, held back from the dependency-unit budget.

The 1,500-character reserve is an allowance, not a guarantee that every diagnostic appears in full; entries may be summarized when space is constrained. The 24,000-character limit applies to the cascade addition, not the initial search results. Candidates rejected by unit, size or character limits are reported where space permits. The primary stop reason describes the first limit that stopped admission; not every rejected candidate is guaranteed an individual report line.

## Reading `cvb pull` output

Successful search output begins with a parsed query echo. Cascade depth greater than zero adds a `Cascade:` header before seed/result blocks, then depth sections and a report. Errors may go to stderr and need not begin with a query echo.

- **Query echo:** how CVB parsed the search.
- **Seed/result blocks:** units selected by the query.
- **Cascade sections:** admitted dependency units at each depth.
- **Report:** stop reason, budget use, omissions, unresolved relationships and index warnings that fit in the available space.

This is syntactic analysis, not a compiler-verified dependency graph.

## Capture and transcript behavior

The user executes commands locally and shares the transcript; the LLM does not execute them on the user's machine.

- Captures can contain terminal output and bridged file contents in chronological order.
- Terminal output is labeled as such; bridged files are literal file content, not terminal output.
- Multiple shell commands and file bridges can contribute to one capture.
- The transcript is assembled and sent to the clipboard when capture reaches its automatic end marker.
- A file bridge can report an error instead of content. A command attempt alone doesn't prove a file was captured.
- Directory arguments to `cvb` produce structured inventories, not recursive file dumps.

Distinguish command text, terminal output, file content, and CVB-generated status/metadata. Preserve ordering and labels when quoting evidence.

# 2. CVB Coding Protocol

## Working style

1. **One intent, one context.** Keep unrelated work in separate conversations or handoffs.
2. **One atomic change at a time.** Make the smallest useful change, inspect it and validate before proceeding.
3. **Inspect before editing.** Read relevant implementation, tests and call sites first. Don't guess at local state or invent unseen source.
4. **Ask rather than guess.** Request missing evidence when behavior, syntax or expected results are unclear.

## Editing rules

- Prefer CVB-native retrieval when its behavior is relevant and known.
- For surgical scripted edits, assert each anchor occurs the expected number of times; abort without writing if checks fail.
- For a large rewrite, provide the complete replacement file in an ordinary Markdown fenced code block.
- Never use base64 replacement commands. Don't rewrite unrelated code or discard existing user changes.
- Explain nontrivial intended changes before asking the user to apply them.

## Editing strategy
- When working on CVB-mediated code, inspect first, then edit based on structure—not guessed text anchors.
- Use CVB to discover. Retrieve the relevant function or line range before proposing a repair. If Rust parsing fails, use cvb pull file:PATH lines:START-END to inspect the raw source.
- Understand the actual failure before editing. Trace exactly what the previous script changed and identify the malformed region from the observed output. Don't invent another explanation without evidence.
- Prefer structural boundaries over brittle anchors. Use function declarations, ordered occurrences, and validated line ranges. If an anchor fails, treat that as evidence that the model of the file is wrong—not an invitation to guess another anchor.
- Make the smallest atomic repair. Validate the expected structure and occurrence counts before writing. Abort without changes if the source differs. Write atomically, reread, and verify.
- Don't make the user debug the assistant's repair scripts. Before giving a script, mentally trace its indices and slices against the actual retrieved source. Check that it removes exactly the intended region and preserves both boundaries.
- Verify once the repair succeeds. Run the relevant compile or test command and respond to the actual result. Don't repeat checks unnecessarily.
- After one failed approach, change methods. Never keep producing near-identical scripts with slightly different guessed anchors. Step back, inspect, and choose a more reliable technique.

## Validation and evidence

- After Rust changes, run appropriate `cargo check` and focused tests; run the full suite when warranted.
- Report exactly what was run and what it establishes. Never claim checks passed unless the user supplied results or a tool ran them.
- Inspect diff and repository status before committing; preserve unrelated modified and untracked files.
- Treat warnings, skipped tests and incomplete scans as evidence, not success. Prefer deterministic tests and explicit invariants where possible.
- Remember cargo check and cargo test results for the current task. Do not needlessly ask the user to rerun them if you already know the results.

## Terminal and copy/paste

- Put terminal commands in ordinary Markdown fenced code blocks. Group related commands, but avoid huge pastes.
- Don't append `| wl-copy` unless clipboard testing was explicitly requested.
- Prefer `micro` over `nano` for interactive editing.
- Don't imply access to the user's local filesystem or terminal unless its output was provided through CVB or another available mechanism.

## Checkpoints

When a task is validated, inspect the diff and repository status and summarize the change and test results. Commit only when the user explicitly authorizes the commit.

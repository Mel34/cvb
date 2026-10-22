# CVB — LLM Operating Manual

CVB is human-directed. The user runs commands and shares captured output; the LLM has no direct access to the user's machine. Treat terminal output and bridged files as user-provided evidence, not as proof of anything beyond what they show.

---

# 1. CVB Reference

CVB bridges terminal activity and repository content into a transcript that can be shared with an LLM.

## Retrieving Information (`cvb pull`)

`cvb pull` searches Git-tracked files. Search is case-sensitive. Matching and exclusions operate on syntactic units where supported, rather than necessarily on individual matching lines.

### Command formats

- `cvb pull QUERY...` — search tracked files for matching units.
- `cvb pull [file:PATH] [cascade:N] QUERY...` — search for initial units and, when requested, expand Rust dependency context.
- `cvb pull file:PATH` — capture one Git-tracked file. An empty file is reported as empty.
- `cvb pull file:PATH QUERY...` — restrict the search to the specified tracked file. A missing or invalid path is an error; do not assume the command silently searches the whole repository.
- `cvb pull file:PATH lines:N` or `lines:START-END` — capture inclusive line ranges. Multiple line selectors can be supplied; overlapping or duplicate selections may be preserved. Do not combine `lines:` with search terms.
- `cvb pull callers:NAME` — find syntactic Rust call-site candidates in tracked `.rs` files; `file:PATH` can scope the scan. These are syntactic candidates, not proof of resolved runtime calls.
- `cvb FILE...` — bridge file contents directly, including untracked files. Directory arguments produce structured inventories rather than recursively pasting all file contents.

Paths and selectors are command syntax, not search terms. If a command is rejected, use the error message rather than assuming a partial result was produced.

### Query syntax and Boolean logic

| Pattern | Meaning | Example | Behavior |
| :--- | :--- | :--- | :--- |
| `term1 term2` | OR alternatives | `CRLF BOM` | A unit matches if at least one bare term matches. |
| `+term` | Required term | `+parse_query +SearchQuery` | Every required term must occur in the unit; they may occur on different lines of that unit. |
| `-term` | Exclusion | `-test -mock` | Excludes a unit when the excluded term occurs in it. |
| `"phrase"` | Phrase containing spaces | `"line ending"` | Searches for the phrase as one argument. |
| `"--flag"`, `"-1"` | Literal leading operator characters | `"--dry-run"` | Quote a literal term that would otherwise be parsed as an operator. |

Query matching is case-sensitive. Exclusion-only queries are rejected: at least one positive term is required. Negated selectors such as `-file:`, `-lines:`, `-callers:`, and `-cascade:` are not supported; exclusions apply to content terms.

The `cascade:N` directive is reserved for cascade control. To search for text that resembles a directive, quote or escape it according to the shell and CVB query parser; do not assume an unquoted `cascade:N` is treated as an ordinary search term.

### Unit boundaries

- **Rust:** The smallest enclosing recognized item is used. Functions/methods and declarations such as structs, enums, and `impl` blocks can be distinct units.
- **Bash:** Function definitions are treated as units where the parser supports them.
- **Markdown:** A heading section includes its nested subsections until the next heading at the same or higher level.
- **Fallback:** Where the relevant parser cannot parse a file, matching may fall back to line-level results and emit a warning. Treat a warning as evidence that the result may be incomplete.

## Context expansion (`cascade:N`)

`cascade:N` opts into deterministic, local expansion from matching Rust units. The implementation builds an index of Git-tracked Rust source and follows dependencies that it can identify syntactically. It does not use an LLM or require network access for the analysis.

### Depth

- **Depth 0:** Initial search results (seeds).
- **Depth 1:** Dependencies identified directly from eligible seeds.
- **Depth 2 and later:** Dependencies of units admitted at the preceding depth, up to the requested maximum.

The expansion is syntactic, not a complete Rust type checker or compiler call graph. A dependency can exist even if the cascade cannot resolve or include it.

### Command constraints and scope

- Valid cascade depths are `0` through `5`; only one `cascade:` directive is allowed.
- `cascade:0` bypasses expansion and its cascade header/report. Its search result content is intended to match an ordinary pull; do not rely on byte identity unless the relevant implementation/test confirms it.
- Cascade cannot be combined with `lines:`, `callers:`, or a lone `file:PATH` request for whole-file capture.
- When `file:PATH` is combined with a search and `cascade:N`, the file scope selects initial seeds. Dependency resolution can then inspect tracked Rust files elsewhere in the repository.
- Non-Rust seeds, line-only fallback seeds, and test-code seeds are not traversed as ordinary Rust dependency seeds.

### What the resolver can and cannot establish

The resolver recognizes selected syntactic relationships, including free-function calls, some associated-function and method calls, and project-defined struct/enum types in signatures. Resolution depends on the available syntax and indexing information, including imports and statically inferable receiver types.

It is not a full semantic analysis. In particular:

- Calls through callbacks, unknown receiver types, ambiguous names, external functions, standard-library functions, trait dispatch, generic constraints, and macro expansion may not be resolved to a concrete project definition.
- Macro contents may yield syntactic candidates; a candidate is not proof that a call resolves to a particular target.
- Trait implementations are not automatically swept into the result just because a trait or trait object appears.
- Test functions and code under `#[cfg(test)]` are excluded from the dependency index.
- Unreadable or unparseable tracked Rust files can make the index incomplete; the output may include warnings.
- A function identified as a high-use utility can be included as a leaf: its body is included, but its callees are not expanded further.

Ambiguous or unresolved relationships may be summarized in the cascade report. Do not interpret absence from the bundle as proof that no dependency exists.

### Budgets and limits

The implementation defines these limits:

- Maximum depth: **5**.
- Maximum added dependency units: **25**.
- Maximum dependency unit size: **200 lines**; larger candidates are omitted and may include a `cvb pull file:PATH lines:START-END` hint.
- Utility threshold: **8** distinct calling units; qualifying utilities are treated as leaves.
- Maximum cascade addition: **24,000 characters**.
- Diagnostic/report reserve: **1,500 characters**, held back from the dependency-unit content budget.

The 1,500-character reserve is an allowance for the report and other engine-generated cascade text, not a guarantee that every possible diagnostic item will be listed in full. The implementation may summarize omitted or unresolved entries when space is constrained. The 24,000-character limit applies to the cascade addition as rendered by the implementation; the initial search results are separate and are not counted as added dependency content.

Candidates rejected by the unit, size, or character budgets are reported where space permits. The primary stop reason describes the first budget limit that stopped admission. Do not infer that every rejected candidate will necessarily have an individual report line if the report itself reaches its reserve limit.

## Interpreting `cvb pull` output

Successful search output begins with a parsed query echo. A cascade run with depth greater than zero adds a `Cascade:` header before the seed/result blocks, followed by cascade depth sections and a report. Errors can instead be written to stderr and need not begin with a query echo.

Treat the output as structured evidence:

- Query echo: how CVB parsed the search.
- Seed/result blocks: units selected by the query.
- Cascade depth sections: additional dependency units admitted at each depth.
- Cascade report: stop reason, budget use, omissions, unresolved relationships, and any index warnings that fit within the available report space.

A reported candidate, unresolved relationship, or omitted unit is a statement about the syntactic analysis and its limits—not a compiler-verified dependency graph.

---

## Capture and transcript behavior

CVB is designed for a human-driven workflow. The user executes commands locally and then shares the resulting transcript; the LLM does not execute commands on the user's machine.

- A capture can contain terminal output and bridged file contents in chronological order.
- Terminal output is labeled as terminal output. Bridged file contents are included as literal file content rather than being interpreted as terminal output.
- Multiple shell commands and multiple file bridges can contribute to one capture.
- The transcript is assembled and dispatched to the clipboard when the capture reaches its automatic end marker.
- A file bridge can report an error instead of file contents. Do not infer that a requested file was captured merely because a command was attempted.
- Directory arguments to `cvb` produce structured inventories; they are not equivalent to bridging every file in the directory.

When reasoning from a capture, distinguish command text, terminal output, file content, and CVB-generated status/metadata. Preserve ordering and labels when quoting evidence.

---

# 2. CVB Coding Protocol

These rules govern assistance with CVB development.

## Working style

1. **One intent, one context.** Keep unrelated work in separate conversations or handoffs.
2. **One atomic change at a time.** Make the smallest change that advances the current intent. Inspect and validate it before moving to the next change.
3. **Inspect before editing.** Read the relevant implementation, tests, and call sites first. Do not guess at local state or invent source that has not been inspected.
4. **Ask rather than guess.** If required source, expected behavior, command syntax, or test results are unclear, ask for the missing evidence.

## Editing rules

- Prefer CVB-native retrieval tools when their behavior is relevant and known.
- Use surgical edits for localized changes. For scripted edits, assert that each anchor occurs the expected number of times and abort without writing if the check fails.
- For a large rewrite, provide the complete replacement file in an ordinary Markdown fenced code block.
- Do not use base64 replacement commands.
- Do not rewrite unrelated code or discard the user's existing modifications.
- Explain the intended change before asking the user to apply it when the change is nontrivial.

## Validation and evidence

- After a Rust code change, run an appropriate `cargo check` and the relevant focused tests; run the broader test suite when warranted by the change.
- Report exactly what was run and what the output establishes. Do not claim a build or test passed unless the user supplied the result or a tool actually ran it.
- Inspect the diff and repository status before a commit. Preserve unrelated modified and untracked files.
- Treat warnings, skipped tests, and incomplete scans as meaningful evidence; do not silently turn them into success.
- Prefer deterministic tests and explicit invariants over subjective inspection when behavior can be tested.

## Terminal and copy/paste conventions

- Put terminal commands in ordinary Markdown fenced code blocks so they can be copied cleanly.
- Group related commands into one block, but avoid unnecessarily large pastes.
- Do not append `| wl-copy` unless clipboard testing was explicitly requested.
- Prefer `micro` over `nano` when an interactive editor is needed.
- Do not imply that the assistant can see the local filesystem or terminal unless the user has provided that output through CVB or another available mechanism.

## Checkpoints

- Keep changes reviewable and checkpointed.
- Before committing, summarize the change, the tests performed, and any known limitations.
- Do not commit, push, or overwrite user work without explicit instruction.

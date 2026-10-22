# CVB quality-of-life pass: handoff

Audience: an LLM implementer working in the CVB repository (Rust + Bash). Design only; no code is supplied. Six independent tasks, each in its own section so they can be split across sessions.

## 0. Ground rules (apply to every task)

- **Baseline.** Work on the branch the user names (expected: `cvb-0.7.0`). The copy of `pull.rs` stored in the Claude Project is older than the branch (no `SearchUnit`, `resolve_unit`, `run_capture`; structs still expand with their impls). Line numbers and function names in this document come from that older copy and from the earlier pull-cascade design doc; verify against the real tree first.
- **Inspect and explain first.** Before changing anything, read the relevant code and report: what exists, what you propose to touch, anything in this document that does not match the tree. Then make the change.
- **Atomic changes with tests.** One task per change set, each ending green. Follow the existing test styles: pure unit tests, plus end-to-end tests in a temporary git repo through `run_capture`.
- **Backward compatibility is the top priority.** When none of the new options are used, `cvb pull` output, stderr and exit codes must be byte-identical to today. Capture golden outputs for a corpus of existing queries (bare, quoted, `+`, `-`, `\`-escaped terms, `file:`, `file:`+`lines:`, whole-file, `callers:`) before the first change and re-check after each one. Existing tests must pass unmodified, except those whose assertions encode behaviour a task deliberately changes (called out per task).
- **New directives follow the existing pattern.** They are stripped from the argument list before query parsing, so they can never become search terms. `+name:` / `\name:` / quoted forms stay literal search terms, as `file:` and `lines:` behave today. `-name:` is rejected, as `-file:` and `-lines:` are.
- **Error style.** `CVB pull: ...` messages, exit code 2 for usage errors, 1 for no matches, 0 otherwise.
- **Do not touch:** surgery, the control protocol, PTY/history lifecycle, the chunker's behaviour (except where Task 5 re-uses it), and anything not listed below.
- **Docs.** Each task updates `manual.md`, the usage text and `cvb.1` where they describe the changed behaviour.

## Recommended order

1. Task 4 (untracked files), because it changes the file listing the others build on.
2. Task 1 (multiple scopes).
3. Task 3 (`lines:` with search terms) and Task 2 (head/tail), independent, same output stage.
4. Task 6 (tab completion), after the option set is final.
5. Task 5 (`repaste`), independent; can go any time.

---

## Task 1. Multiple `file:` scopes

**Goal.** `cvb pull file:a.rs file:b.rs term` searches both files.

**Today.** `extract_file_scope` and `extract_callers_scope` hold one optional path and fail on a second `file:` with "only one file: scope is allowed."

**Spec.**
- Accept any number of `file:` arguments. Scope becomes a list; `filter_file_scope` keeps files whose path equals any entry.
- Duplicates are deduplicated silently. Output order does not depend on the order of `file:` arguments (results are already sorted by path/line; keep it that way).
- If any entry matches no file, fail with the existing "file scope matches no ... file" message naming that path. No partial results, no silent skipping.
- `callers:` accepts multiple `file:` scopes the same way; each must still be a `.rs` file.
- A lone `file:PATH` (whole-file capture) stays a single-file operation. With two or more `file:` and no search terms or `lines:`, report a clear error rather than concatenating files (out of scope for this pass).
- `lines:` requires exactly one `file:` (see Task 3). With several scopes, error: `lines: requires exactly one file: scope.`
- Remove the "only one file:" rejection only for the search path; keep empty-path (`file:`) rejection.

**Tests.** Two files, both match; one file matches; duplicate scopes; one missing path fails whole command; order-independence; `callers:` with two scopes; `lines:` with two scopes errors; the updated test(s) that asserted "multiple scopes rejected" now assert the new behaviour.

---

## Task 2. `head:N` / `tail:N`

**Goal.** Get the first or last N lines of a file without knowing its length.

**Spec.**
- New directives `head:N` and `tail:N`, N a positive integer (digits only, at least 1).
- They apply to **whole-file capture** (`cvb pull file:PATH head:50`) and to **`lines:` output**, where they trim the combined output. Specifically for whole-file: print lines 1..N or the last N lines.
- Combined with search terms, `callers:` or each other, reject with a clear error (`head:/tail: cannot be combined with search terms.` and so on). Rationale: with search results it is unclear whether N counts units, lines inside units or hits. Do not invent semantics.
- Both `head:` and `tail:` together: error.
- N larger than the file: return the whole file, no error. (This is deliberately friendlier than the current out-of-range `lines:` error, which stays unchanged.)
- Output format is identical to existing line-range output: `path:LINE: text` with original line numbers (`tail:20` on a 300-line file prints 281..300).
- Empty file: same as today's whole-file behaviour (no output, exit 0).

**Tests.** Head/tail on short and long files; N equal to, below and above file length; N=0 and non-numeric rejected; combinations rejected; whole-file output unchanged when neither directive is used.

---

## Task 3. `lines:` combined with search terms

**Goal and intended use.** An LLM tends to request a line range repeatedly when it should have asked for the whole function. Letting `lines:` act as a restriction on a normal search, with the normal unit expansion, makes `cvb pull file:src/x.rs lines:100-300 "foo"` return the complete enclosing function(s) of hits inside that range, even where the function extends beyond the range.

**Decided semantics.**
- `lines:` plus search terms means: the search runs within the stated line ranges of the single `file:` scope; unit expansion then applies exactly as in an ordinary pull, so a returned unit may extend beyond the requested range.
- Multiple `lines:` ranges are OR'd (a hit in any range counts).
- `lines:` without search terms behaves exactly as today (raw lines).
- Requires exactly one `file:` scope (error otherwise, as in Task 1).

**Implementation guidance.**
- Remove the "lines: cannot be combined with search terms" rejection for the `file:`+`lines:`+terms case. Keep the `callers:` + `lines:` rejection.
- Implement as a restriction on search *hits*: after the search returns `(path, line)` records, keep only those whose line falls in one of the ranges, then continue through the existing pipeline (query operator filtering, unit resolution, dedupe, output).
- Validate ranges against the file length as the raw-lines path does. Decide and document one behaviour for a range that exceeds the file (follow the existing error unless there is a reason not to).
- **Open point to report back, do not decide silently:** the branch evaluates `+required` / `-excluded` / alternatives over *all lines of a resolved unit*. With a `lines:` restriction, decide whether operators evaluate over the whole unit (consistent with today) or only the in-range lines. Recommended default: seeds (hits) must come from in-range lines, operators evaluated as today over the unit. Explain the choice in the report and in the manual.
- Output should make it visible that the restriction was applied (for example in the `Query:` echo), so the reader understands why a unit larger than the range appears.

**Tests.** Hit inside range returns the whole unit; hit outside range is ignored; multiple ranges; range inside a function body; no hit in range gives the normal "no matches" result and exit 1; `lines:` without terms unchanged; `callers:` + `lines:` still rejected; missing/multiple `file:` errors; Markdown and Bash units behave the same way.

---

## Task 4. Pull must not require Git-tracked files

**Goal.** Pull works on new, never-added files, and outside a Git repository.

**Today.** `tracked_files()` runs `git ls-files -z` and errors when not in a repo or when empty. `filter_file_scope` also requires the path to be tracked ("matches no Git-tracked file").

**Spec.**
- **Explicit `file:PATH`:** accepted if the path exists on disk and is a regular file, whether or not it is tracked, and whether or not the current directory is a Git repository. The user named it, so ignore rules do not apply.
- **Repo-wide search (no `file:`):** inside a repo, use the tracked files plus untracked files that are not ignored (the equivalent of `git ls-files -z --cached --others --exclude-standard`; verify exact flags and NUL handling). Outside a repo, search the current directory tree while honouring ignore files where practical (for example via `rg`, which already ignores `.git` and respects `.gitignore`); if that is not practical, fall back to a plain directory walk that skips `.git` and `target`. Report which strategy you chose.
- **Safety.** Reject `file:` paths that contain a `.git` path component (consistent with the surgery rule), and paths that resolve outside the working directory. Binary or unreadable files produce the existing read-error style message.
- Messages: update "no Git-tracked file" wording to something accurate (for example `file scope matches no file: PATH`). Error for empty file listing changes accordingly.
- `callers:` and any future index built from the file list inherit the new behaviour; confirm `callers:` still skips non-`.rs` files.
- Keep ordering deterministic (sorted).

**Tests.** Untracked file via `file:`; untracked file found by repo-wide search; ignored file not found by repo-wide search but reachable via explicit `file:`; non-repo directory; `.git` component rejected; path escaping the working directory rejected; the existing tests that asserted rejection of untracked paths are updated to the new messages.

---

## Task 5. `cvb repaste`

**Goal.** After the chunk pager has been dismissed or cleared, `cvb repaste` re-triggers the pager with the whole last output so it can be pasted again.

**Decided behaviour.**
- It re-pastes the **whole** last output, not just the last chunk, and re-triggers the pager (same chunking, same navigation, same position indicator as a fresh output).
- It is an ordinary command (`cvb repaste`), invoked after the pager has already finished or been cleared.
- It works **only within the current session**. No persistence across sessions, no cross-session lookup.

**Implementation guidance.**
- Inspect-and-explain first: find where the pager (`src/clipboard.rs`, `chunk_messages`, `process`, `process_with`) receives its text, and where the session keeps the last output, if anywhere. Report whether the session already retains the last payload after the pager is cleared and where, before deciding where to store it.
- Retain the last payload in session scope only (in memory in the session process, or session-scoped state that is cleaned up with the session). Do not write it to a global location that survives sessions.
- Define "last output" precisely and document it: the last payload that was sent to the pager/clipboard for the user, not the output of `cvb repaste` itself (repasting twice should paste the same thing twice).
- Behaviour when there is nothing to repaste: clear message, non-zero exit.
- `repaste` must not itself be recorded as new capturable output, and must not leave surprising shell-history entries (compare how the startup context avoids this in 0.7.0).
- Do not change the chunker's chunk-size logic, limits or protocol framing.

**Tests.** Repaste after normal output reproduces identical chunks; repaste twice is stable; nothing-to-repaste message and exit code; a large (multi-chunk) payload repastes with the same chunk count; payload retention does not outlive the session (as far as the harness can test it).

---

## Task 6. Bash tab completion

**Goal.** `cvb su<TAB>` completes to `cvb surgery`; options of `pull` complete too.

**Spec.**
- Single source of truth: derive the subcommand list from the same table `cli::parse` uses (or introduce one) rather than maintaining a hand-written list. Add a subcommand `cvb completions bash` that prints the completion script, usable as `eval "$(cvb completions bash)"` or saved into the bash-completion directory. Report if the CLI parser makes a table impractical.
- Complete top-level subcommands, including the new `repaste`.
- For `cvb pull`, complete selector prefixes (`file:`, `lines:`, `callers:`, `head:`, `tail:`) and file paths after `file:`.
- `:` is in `COMP_WORDBREAKS`; the script must handle it (for example by re-joining words with the bash-completion helper where available, with a fallback that works without the `bash-completion` package). Test both.
- Where the shell wrapper (`cvb.bash`) is sourced, decide whether to register completion there automatically; if so it must be cheap at shell startup (no `cvb` subprocess on every shell start if avoidable).
- Do not alter shell-state, history sync or PTY behaviour.

**Tests.** A test that every subcommand known to the parser appears in the generated script (guards against rot); the script passes `bash -n`; a scripted completion test (set `COMP_WORDS`/`COMP_CWORD`, call the function, assert `COMPREPLY`) for `su`, `pull fi`, and `pull file:src/`.

---

## Final report expected from each task

What changed, files touched, tests added/updated (and why any pre-existing test changed), golden-output result, any decision taken in an "open point", and anything noticed but deliberately not done.

# cvb pull: Review Findings and Query Design

Oct 15, 2026 · @Petar

## Purpose and scope

`cvb pull` is queried by an LLM, so its worst failure is a silent false negative: the model reads "no matches" as "this code does not exist" and moves on. Every decision below favors results that are strict, visible and explained over results that are merely convenient.

This document records the review of `pull.rs` (read on 2026-10-15), the design for two query-semantics questions (bare terms next to `+`, and line-level versus unit-level matching), and the remaining findings. It is design only; no code is proposed. The review comes from reading the source, not from compiling or running it.

## Current behavior

Today every operator is evaluated against a single line of text. Bare terms are alternatives (OR), `+term` must also appear on that line, and `-term` must not appear on it.

| Query part | Meaning today | Notes |
| --- | --- | --- |
| Bare term | At least one bare term appears on the line | Each bare term is searched separately and the results are merged |
| `+term` | Term must appear on the same line | With no bare term, the first `+` term drives the search |
| `-term` | Term must not appear on the same line | Filters hit lines only |
| Quoted phrase | One term containing spaces | Quotes are parsed after the arguments are joined with spaces |
| `file:PATH` | Restricts the search to one tracked file | Exact path match only |
| `lines:N` or `lines:A-B` | Prints those lines | Cannot be combined with search terms |
| `callers:NAME` | Rust call sites found through the syntax tree | Cannot be combined with other selectors or terms |

The flow is: list tracked files, run `rg` (or `grep` as fallback) once per bare term, concatenate the results, filter each hit line with the query, remove duplicates, then expand each hit to its enclosing function, struct with its impls, Bash function or Markdown section.

Exclusions apply to the hit line only. The expanded block can therefore still contain the excluded text, and the test `excluded_terms_filter_matches_before_markdown_expansion` pins this behavior.

## Design A: bare terms next to `+` terms

Keep the strict semantics, where `+a b` means `a AND b`, and make the interpretation visible instead of changing it. The current tests (`query_plus_terms_are_required`, `query_combines_or_required_and_excluded_terms`) already pin this behavior.

The risk is that a model assumes the Google or Lucene reading, where `+a b` makes only `a` required and `b` merely optional. Under the strict reading that misreading produces too few hits, which a diagnostic can expose. Under the optional reading a misreading would silently return more than intended, which nothing exposes.

1. **Echo the parsed query** as the first line of every result and in the no-match message, for example `Query: (CRLF OR BOM) AND split_inclusive AND NOT tests`. A model that meant "b optional" sees the mismatch immediately and can retry.
2. **Put one example per operator in the usage text,** plus one combined example. Models imitate examples more reliably than they follow descriptions.
3. **Add no second syntax.** Explicit `AND` and `OR` words would give models two ways to say the same thing and more room to mix them up.

See Diagnostics for where the echo goes and what else accompanies it.

## Design B: unit-level matching for `+` and `-`

Move `+` and `-` from line-level to unit-level matching. A model thinks in terms of "the function that handles X and mentions Y", so `+a +b` should match when both appear anywhere in the same function. At line level it returns nothing when the terms sit on different lines, and the model concludes the code is absent.

**What a unit is.** The unit reuses the ranges the expansion step already computes, so no new parsing is needed.

| File type | Unit | Existing source |
| --- | --- | --- |
| Rust | Smallest enclosing function. A hit in a struct or enum makes the unit the type plus its impls | `rust_expansion_ranges` |
| Bash | Smallest enclosing function definition | `bash_function_range` |
| Markdown | Nearest heading section, including nested subsections | `markdown_section_range` |
| Anything else, or a hit outside any unit | The single line, exactly as today | none |

**Match rule.** A unit matches when it contains at least one bare term (if any were given), every `+` term, and no `-` term anywhere in its text. The first search still finds candidate hit lines from the bare terms, or from the first `+` term when there are no bare terms. Each hit maps to its unit, units are deduplicated by path and line range, and the filter runs over the whole unit.

**Output.** Each matching unit is printed once as an expanded block, as today. Above the block, list the lines where each query term hit, for example `CRLF: 41, 57` and `+split_inclusive: 63`. This shows the model why the unit matched, and excluded terms never appear because an excluded unit is not printed.

**Why this also fixes exclusion.** Today `-tests` removes a hit line but the expanded block can still show the excluded text, which is confusing. At unit level a unit is either in or out.

**Risks and mitigations**

- **Loose units.** A struct with all its impls, a long Markdown section or a long function makes AND easy to satisfy by accident. The per-term hit lines in the output let the model judge how close the terms really are. A proximity limit such as `near:N` is possible later and is not needed now.
- **Over-broad exclusion.** One incidental mention in a comment can hide a whole unit. The diagnostics report how many units each `-` term removed, so the effect is visible.
- **Parse failures.** If `syn` or the Bash parser fails on a file, fall back to line level for that file and say so in the output. Silent fallback would hide a change in meaning.
- **Behavior change.** Queries that only matched same-line combinations will match more. The existing single-line tests of `matches_query` stay valid for line units, and the exclusion-then-expansion test must be rewritten to the new rule.

## Diagnostics for LLM consumers

Three small additions let the model tell "the code is not there" from "my query was wrong". They apply to both Design A and Design B.

1. **Parsed-query echo.** The first line of every result states how the query was read, for example `Query: (CRLF OR BOM) AND split_inclusive AND NOT tests`.
2. **Per-term counts on no-match.** The model otherwise cannot distinguish a term that does not exist from terms that never co-occur. Example:

```
Query: (CRLF OR BOM) AND split_inclusive AND NOT tests
No matches.
  CRLF: 12 units, BOM: 3 units, +split_inclusive: 0 units
  -tests: would have removed 4 units
```

3. **Exclusion note on success.** When a `-` term removed anything, add one line such as `-tests: excluded 3 units`, so the model knows the result was filtered.

The counts need one extra search per term, but only on the no-match path, so successful queries cost nothing more. The exit codes stay as they are: 0 for matches, 1 for no matches, 2 for errors.

The echo and counts should go to stdout, not stderr. The project description says CVB puts usable output on the clipboard, and stderr may not reach the model in every setup. This is an assumption to confirm (see Open decisions).

## Other operator and parsing findings

**Exclusion-only queries match every line.** `cvb pull -tests` passes `parse_query`, and `run` then searches for the empty string `""`, which matches every line of every tracked file. Every line not containing "tests" is then kept and expanded. Reject it with a clear message such as "exclusions need at least one positive term".

**Literal terms starting with `-` or `+` cannot be searched.** `--dry-run`, `-> bool`, `-1` and `+=` lose their first character and become an exclusion or a requirement. Quotes do not help, because they are removed during tokenizing, before the prefix check. The operator should count only when the sign is the first unquoted character of the token, so that `"-foo"` is a literal. A backslash escape is the alternative.

**Argument boundaries are lost.** `run` joins the arguments with spaces and tokenizes again. So `cvb pull "foo bar"` becomes an OR of `foo` and `bar`, and a phrase works only with literal quote characters inside the shell quotes. Treating each argument as one token matches what the shell already did: `"foo bar"` is one phrase and `"+foo bar"` is one required phrase. The cost is that a model that learned the embedded-quote form would search for the quote characters themselves. Accepting one layer of surrounding quotes on an argument, plus the query echo, softens that.

**Negated selectors fall through silently.** `-file:src/x.rs`, `-lines:5` and `-callers:x` are not recognized as selectors, so they become content exclusions of the literal text. A user expecting a file exclusion gets no warning. Reject the negated forms with an explicit message.

**Selector prefixes take over bare terms.** Any argument starting with `file:`, `lines:` or `callers:` is read as a selector, so a bare term of that form cannot be searched. The form `+file:foo` does work as a literal required term, because the `+` stops the selector check, but this is accidental and undocumented. Either document it or make it deliberate.

## Robustness and other findings

The first three rows can mislead the model or lose results; the rest are smaller.

| Finding | Impact | Recommendation |
| --- | --- | --- |
| Any search exit code 2 discards all results. `rg` exits 2 on any error, even when it found matches, and `git ls-files` lists tracked files that are deleted in the working tree | A mid-refactor repo can make the whole pull fail with "search failed" | Drop files that no longer exist before searching, or keep partial results and print a warning. `git grep -F` would also avoid this and the argument-length limit on large repos |
| `callers:` misses calls inside macros and functions used as values. `syn` does not descend into macro bodies, so `assert_eq!(foo(), 1)` and `println!("{}", foo())` are skipped, and so are `.map(foo)` style references | "No callers found" can be wrong, and the model will believe it | Add `visit_macro` to scan the tokens for the identifier and report a "macro-argument candidate". Document that bare function references are not covered, or report them as candidates |
| Result order follows the query order, not the files. Results are grouped by bare term | Output is not deterministic across equivalent queries, and a function block appears at its first hit only | Sort by path, then line number, before expansion |
| Path and line parsing differs between `matching_content` (first `:digits:`) and `expand_rust_matches` (`splitn(3, ':')`) | A path containing a colon breaks one or both | Use `rg --null` and `grep -Z` so the path is separated by a NUL byte |
| `file:` scope needs an exact path. `file:./src/pull.rs` and absolute paths fail | A reasonable path from the model is rejected | Normalize the scope relative to the current directory before matching |
| An empty file produces no output and exit 0 for whole-file capture | The model cannot tell empty from broken | Print a note such as "file is empty" |
| The usage text omits the operators and `callers:` | Models learn the syntax from the usage text | Add one example each (see Design A) |
| `expand_rust_matches` also handles Bash and Markdown | The name misleads the next reader | Rename it, for example `expand_matches` |
| `read_line_range` re-reads the file for every range, and non-UTF-8 files fail with a read error | Minor cost and an unexplained failure | Read once per file and name the cause in the error |

## Test plan

Each change below should land as its own atomic change with the tests listed.

- **Exclusion-only query:** `parse_query` or `run` rejects `-tests` alone, and the error names the missing positive term.
- **Literal operator characters:** `--dry-run`, `-> bool`, `+=` and `-1` reach the search as literal terms when quoted or escaped, while an unquoted `-tests` is still an exclusion.
- **Argument tokens:** one shell argument containing a space becomes one phrase; `"+foo bar"` becomes one required phrase.
- **Negated selectors:** `-file:x`, `-lines:5` and `-callers:x` are rejected with a message.
- **Query echo:** the parsed query line appears for a matching query and a no-match query, with OR, AND and NOT in the expected places.
- **No-match counts:** terms that exist but never co-occur report a nonzero count each; a missing term reports zero; removed units are reported for `-` terms.
- **Unit-level AND:** two `+` terms on different lines of the same function match; the same terms in different functions do not.
- **Unit-level NOT:** an excluded term anywhere in a function removes that whole function; the expanded output never contains the excluded text.
- **Unit fallbacks:** a file that fails to parse falls back to line level and the output says so; a hit outside any function stays a single-line unit.
- **Struct and impls:** a `+` term in an impl block satisfies a query whose other term is in the struct declaration (pending the decision on combined or separate ranges).
- **Deleted tracked files:** a tracked path missing from the working tree does not abort the search.
- **Ordering:** equivalent queries with the bare terms swapped give identical output.
- **Callers in macros:** a call inside `assert_eq!` is reported as a macro-argument candidate.
- **End to end:** at least one test runs `run` in a temporary Git repository, since none exist today.

## Open decisions and order of work

Four decisions are needed before unit-level matching can be built.

1. **Struct plus impls.** Is a type's unit the declaration and all its impls as one combined text, or does each range count on its own? Combined makes AND easier to satisfy; separate keeps units tighter.
2. **Where diagnostics go.** Stdout is recommended so the model always sees them. Confirm that this matches how CVB delivers output to the model.
3. **Argument tokenizing.** Treat each shell argument as one token, keep the embedded-quote parser, or accept both by stripping one layer of surrounding quotes.
4. **Loose units.** Accept the risk with per-term hit lines for now, or plan a `near:N` limit in a later change.

Suggested order, smallest and most independent first:

1. Reject exclusion-only queries.
2. Fix literal `-` and `+` terms, and reject negated selectors.
3. Add the parsed-query echo and the usage examples.
4. Handle search exit code 2 and deleted tracked files; sort output by path and line.
5. Add per-term counts on no-match.
6. Build unit-level matching (needs decisions 1 and 2).
7. Add the macro blind-spot fix in `callers:`.
8. Clear the small items: path parsing, scope normalization, empty-file note, rename, single read per file.

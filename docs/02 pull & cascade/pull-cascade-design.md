# Design: `cvb pull` Cascade — Deterministic Context Expansion

Status: **draft for review. No code changed, nothing committed or pushed.**

Tags used throughout: **[V]** verified in the current source, **[M]** measured by a throwaway analyzer (outside the repo), **[P]** proposed design choice, **[?]** not verified.

## 0. Baseline and method

- **Baseline inspected:** branch `cvb-0.6.0` @ `9e6c8c4` of `Mel34/cvb` (`Cargo.toml` version 0.6.0). **[V]**
- **Two drifts worth knowing about.** `master` is at 0.3.1 and has no `pull` at all. The `pull.rs` stored in the Claude Project is older than the branch (it has no `SearchUnit`/`resolve_unit`/`run_capture` and still expands structs with their impls). Line numbers below are from the branch. If your working tree is ahead of `9e6c8c4`, re-check them. **[V]**
- **Files read in full or in the relevant parts:** `src/pull.rs` (2,491 lines), `src/cli.rs`, `src/main.rs`, `src/clipboard.rs`, the capture path in `src/pty.rs`, `Cargo.toml`, `manual.md`.
- **Measurements** come from a ~150-line scratch analyzer (syn visitor, name-based resolution) kept in the session scratchpad, not in the repo. It approximates the proposed resolver; it is not the resolver. `#[cfg(test)]` modules were excluded. Sizes exclude the 9-character `  {n:>5} | ` prefix that pull adds to every line, so real output is roughly 25% larger. **[M]**

## 1. Current architecture

### 1.1 Pipeline **[V]**

| Stage | Where | Behavior |
|---|---|---|
| Entry | `cli::parse` → `Command::Pull(args.collect())`; `main.rs` → `pull::run` → `run_in_dir` → `run_capture(args, dir) -> (status, stdout, stderr)` (pull.rs:736–752) | `args` are raw argv tokens after the shell has already removed quotes. `run_capture` returns strings, so it is testable without I/O. |
| Mode dispatch | `run_capture` 756–884 | (a) `extract_callers_scope` (440) → `run_callers` returns early. (b) A lone `file:PATH` is a whole-file capture (768). (c) `extract_file_scope` (133) strips `file:` (exactly one, exact tracked-path match) and `lines:` (needs `file:`), and **rejects `-file:`, `-lines:`, `-callers:`** (141). (d) `lines:` prints raw ranges and returns (848). |
| Query parse | `parse_query` (236) | Per-token: leading `\` makes a literal alternative; a fully quoted token is an alternative; `+t` required; `-t` excluded; anything else is an alternative (OR). Exclusion-only queries are rejected. |
| Candidate search | `tracked_files` (8, `git ls-files -z`) → `filter_file_scope` (173) → `run_search` (316, `rg --fixed-strings --null`, grep fallback) → `parse_search_records` (360) | One search per alternative (or the first required term). Records sorted and deduped by (path, line). **Only Git-tracked files are searched.** |
| Unit resolution | `resolve_unit` (617) → `rust_unit_range` (1210, syn), `bash_unit_range` (1157, tree-sitter-bash), `markdown_unit_range` (1551) | Each hit line is widened to the **smallest enclosing unit**: Rust fn / method / struct / enum / impl / trait (inline `mod` recursed); Bash function; Markdown section. Key is `UnitKey{path,start,end}`. Parse failure or no enclosing unit → label `"Line"` (plus a warning on parse failure). |
| Unit filtering | 984–1050 | `alternatives`/`+required`/`-excluded` are evaluated over **all lines of the unit**, not per line. Exclusions are counted for diagnostics. Units sorted by `UnitKey`. |
| No-match | 1052–1088 | Echo, `No matches.`, per-term unit counts via `count_units_for_term`, exit 1. |
| Output | 1090–1136 | `Query: …` echo; `-term: excluded N unit(s)`; per unit `  <Label>: path:start-end`, `  term: hit,lines`, then `  {n:>5} | text` for every line. `"Line"` units print `path:line: text`. Warnings (`CVB pull: warning: …`) go inline into stdout. Exit codes 0 / 1 / 2. |
| Downstream | `pty.rs:869–879` → `clipboard::process` (clipboard.rs:169) | Command output is captured via the PTY transcript, then split by `chunk_messages` (142–167) when longer than the copy limit: **9,750 chars by default**, read from `~/.config/cvb/limit` (clipboard.rs:9–70). Over-limit output becomes numbered `[Part n/m …]` pastes. |

### 1.2 Facts that constrain the design **[V]**

1. **Dependencies are already in place.** `syn 2` (`full`, `visit`), `proc-macro2` (`span-locations`), `tree-sitter`, `tree-sitter-bash`. Cascade needs no new dependency.
2. **Precedent for syntactic analysis exists.** `rust_callers` (1348) is a syn `Visit` that finds call sites, plus a `proc_macro2` token scan with `is_call_invocation` (1303) for calls inside macro bodies. It labels results *candidates*, warns for every file it has to skip as unparseable, and when no callers are found *and* a file was skipped it reports `caller scan incomplete; no reliable no-callers conclusion` (exit 2) instead of claiming there are none. Cascade should inherit exactly this honesty convention.
3. **`cascade:3` today is silently a literal search term** (falls through to `alternatives`, line 289). Existing escapes for literal searching are `\cascade:3` or a quoted `'"cascade:3"'`; `file:`, `lines:` and `callers:` already have the same property, so a new directive is consistent with precedent.
4. **`+file:x` is not a selector.** `extract_file_scope` only matches tokens that *start with* `file:`, so `+file:x` becomes a required literal. `+cascade:3` would behave the same unless special-cased.
5. **`rust_unit_range` discards names and owners** and only enumerates fn, struct, enum, impl (and its methods), trait (and its methods) and inline mods. Consts, statics, type aliases, `macro_rules!`, and fns nested inside fn bodies are not units; hits in them fall back to a single `"Line"`.
6. **Test code is ordinary code to pull.** `#[cfg(test)]` functions are units and match searches. (`clipboard.rs` has 22 tests, six of which call `process_with`.) So the handoff's example, `cvb pull "process_with" file:src/clipboard.rs cascade:3`, would have the definition, its caller `process`, **and six test functions as seeds**: eight seeds, of which only two are worth expanding.
7. **A struct unit no longer carries its impls** on this branch (the older Project copy did). A cascade that wants "type + its methods" is new behavior.
8. **`file:` means "restrict the search", and a missing path is an error with no repository-wide fallback** (manual.md:21). It has no notion of restricting anything beyond the search.

### 1.3 Not verified **[?]**

- How `render_transcript(&transcript, rows, cols)` treats very long lines or very long output (it is vt100-based). A large bundle goes through it; wrapping behavior is unexamined.
- Whether your local working tree equals `9e6c8c4`.

## 2. Proposed architecture

### 2.1 Smallest viable insertion **[P]**

```
run_capture
  ├─ NEW  extract_cascade_directive(args) -> (Option<Cascade>, rest)   // before callers/file/query parsing
  ├─ … everything up to `matching_units` is untouched …                // ends at line 1050
  ├─ print initial results EXACTLY as today                            // lines 1090–1134, unchanged
  └─ NEW  if let Some(c) = cascade { stdout.push_str(&cascade::run(seeds, files, c)) }
```

- **Absence of the directive is a no-op by construction:** the directive is stripped first; `None` skips the new branch. Existing semantics cannot change as a side effect, and a golden-output test can prove it (§8).
- **Initial results print first, unmodified.** The cascade appends additional sections after them, so `cascade:0` and "no directive" are byte-identical and existing parsers of the output keep working.
- **New module `src/cascade.rs`**, pure: input is seed units + tracked file list + options; output is a `Bundle` (items, unresolved, omitted, stop reason) plus a renderer. `run_capture` already returns strings, so end-to-end tests follow the existing temp-git-repo pattern (pull.rs:2429).
- **One shared refactor is required** (called out because it touches existing code): `rust_unit_range` must be split into a shared `rust_items()` enumerator yielding `(kind, name, owner, start, end, in_test_mod)`, with `rust_unit_range` becoming a filter over it. Reason: a cascaded unit must produce a `UnitKey` **identical** to the one pull produces for the same item, otherwise the same function can appear twice with different spans. Guard with golden tests before and after.

### 2.2 What cascade does not touch **[P]**

Query parsing semantics, `callers:`, `lines:`, whole-file capture, Bash/Markdown unit logic, the clipboard/chunker, the control protocol, surgery.

## 3. Dependency discovery

### 3.1 Approaches compared

| Approach | For | Against | Verdict |
|---|---|---|---|
| Regex / ripgrep identifier scan | Trivial, no parse | Matches comments, strings, field names, std methods; cannot tell a call from a mention | Rejected as the primary method |
| **`syn` visitor** (what `rust_callers` already does) | Already a dependency; real call/method/path nodes; comments and strings ignored; macro bodies reachable via the existing token scan; spans give unit keys | Whole-file parse must succeed; no name resolution, no types; `cfg` not evaluated | **Recommended** |
| `tree-sitter-rust` | Error-tolerant (partial results on broken files) | New dependency | Defer; revisit only if parse-failure rate matters |
| rust-analyzer / LSP | Real resolution | Heavy, stateful, not a simple local deterministic step | Out of scope |

### 3.2 Edge extraction **[P]**

From each unit, one syn visit collects:

| Edge | Source node | v1? |
|---|---|---|
| Callee, path call | `ExprCall` with `Expr::Path` (`foo()`, `Self::foo()`, `Type::foo()`, `module::foo()`) | **Yes** (priority 1) |
| Callee, method call | `ExprMethodCall` | **Only when the receiver type is syntactically known** (see 3.3 rule 5) |
| Callee inside macro | Token scan reused from `rust_callers` (`is_call_invocation`) | Yes, labeled *candidate* |
| Function used as a value | `ExprPath` in argument position (`.map(parse_line)`) | Yes, lower confidence |
| Type in the unit's **signature** (params, return) and struct-literal / `Type::assoc` paths | `Path` nodes with an uppercase first segment | **Yes, project types only, declaration only (no impls)** |
| Callers, tests, trait impls, error paths | — | **No** (see 3.5) |

### 3.3 Resolution rules **[P]**

Resolution is **syntactic and qualifier-aware**, against an index of all tracked `.rs` files (functions, methods with owner type, structs, enums, traits; `#[cfg(test)]` modules and `#[test]` fns excluded from the index). Every edge ends in exactly one class: **resolved** (one candidate), **ambiguous** (≥2), **external** (no project definition), or **unresolvable** (dynamic).

1. `Self::f` and `self.f()` → owner = enclosing impl's self type.
2. `Type::f` with uppercase `Type` → candidates with that owner only. If `Type` is not a project type (e.g. `Command::new`, `Vec::new`), the edge is **external**. This single rule removes the largest source of false matches for common names such as `new`.
3. `module::f` with a lowercase qualifier → followed only if the qualifier is `crate`, `self`, `super`, or a project module name (a `src/*.rs` stem or inline `mod`); otherwise external.
4. Bare `f()` → free fn in the same file, then a `use`-imported name (minimal `use` map), then a project-wide **unique** free fn. A bare call whose name is a **parameter of the enclosing fn** is classed **unresolvable (callback)**.
5. `recv.f()` → followed only if the receiver type is known from `self`, a parameter's type annotation, or an annotated `let`; then resolved by owner. Otherwise: **"method call, receiver type unknown"** (listed, not followed).
6. Anything else (generics, trait dispatch, function pointers, macro-generated, cfg-duplicated items) → **ambiguous or unresolvable**, reported, never guessed.

**Evidence for rule 5 and against name-only method matching [M].** On `cvb-0.6.0` (148 fns/methods, 36 type decls, tests excluded), of 2,262 non-macro call sites: 1,774 have no project definition (external), 365 match exactly one project name, 123 match a name defined more than once. Among the "unique" matches, the three highest-fan-in project "callees" are `len` (37 distinct caller fns), `push` (28) and `as_bytes` (11); the next is 5. Those three are almost certainly std methods sharing a name with a CVB method (inferred from the gap, **[?]** not checked per call site). Following them by name would flood a bundle with `ClipboardBuffer::len`. By contrast, `process_with` takes `buffer: &ClipboardBuffer`, so rule 5 correctly resolves `buffer.len()` and `buffer.as_bytes()` there.

### 3.4 Strengths and limits (stated in every cascaded output)

- **Strengths:** deterministic, no network, no new dependency, same behavior and wording conventions as `callers:`, understands real call syntax rather than text.
- **Cannot see or resolve:** trait-method dispatch and `dyn`; generic callbacks (`process_with`'s `copy`/`copy_sequence` are `FnMut` parameters, **[V]** clipboard.rs:178–205); function pointers; closures stored in variables; macro-generated items and calls the token scan cannot recognize; items hidden by `cfg` (syn does not evaluate it, so both variants appear as ambiguity); `use` aliases/globs beyond the minimal map; `#[path]` modules; fns nested inside fn bodies (not units today); unparseable files (skipped, flagged).
- **Never claimed:** completeness. The report footer says absence from the bundle does not imply absence of a dependency; and if any indexed file failed to parse it says the index is incomplete (mirroring `caller scan incomplete`).

### 3.5 Recommended initial scope and why

| Relationship | Decision | Reason |
|---|---|---|
| Callees (fn, resolved) | **v1 default** | The stated principal use case. |
| Types in signatures (declaration only) | **v1 default** | Needed to read the code; cheap; avoids pulling whole impls. |
| Callers | Not v1 | Fans *up*, answers a different question; already available as `cvb pull callers:NAME`. Unresolved-callback notes point to it. |
| Tests | Not v1 | Consumers, not explanation; large; matched by plain `pull` when wanted. |
| Trait declarations/impls | Not v1 | Dispatch is the hardest case to resolve honestly; list as "not followed". |
| Error types and state definitions | Only via the signature/struct-literal rule | Anything broader is the "follow everything" trap. |

## 4. Traversal algorithm **[P]**

Bounded, level-synchronous breadth-first search. Depth 0 is the ordinary pull result.

```
seeds     = initial matching units, in current order
frontier  = [u in seeds if u is Rust and not in a test module]    // test/Bash/Markdown/"Line" seeds are shown, not followed
included  = { key(u) → depth 0 for u in seeds }                   // dedupe on UnitKey, shared with pull
for depth in 1..=N:
    cands = []
    for u in frontier (sorted by path, start):
        for edge in extract_edges(u):
            match resolve(edge):
                resolved(t)  → cands.push((t, edge, from=u))
                other        → tally in report (deduped by name)
    merge cands by target key; keep best reason; count distinct referrers
    drop targets already in `included`                            // also prevents cycles and self-recursion
    sort by rank (below)
    for t in sorted cands:
        admit if within budget, else record in omitted(reason)
    frontier = admitted this level
    if frontier is empty: stop(converged)
stop(depth-limit) otherwise
```

**Rank (lexicographic, each part explainable in the output):**
1. Confidence: path/`Self::`/typed-receiver resolved › function-as-value › macro-argument candidate.
2. Edge kind: callee › signature type.
3. Locality: same file › same directory › elsewhere.
4. Number of distinct referrers in the frontier (more is better).
5. **Utility demotion:** a target called from ≥ `H` distinct units repo-wide (default `H = 8`) is a likely helper (the "logging" case). It is ranked last and included as a **leaf**: its body appears, but its own callees are not followed (reason: `utility, called from 14 units`).
6. Tie-break: smaller unit first, then `(path, start)` for determinism.

**Unit granularity:** an item's own span (fn/method/struct/enum/trait), identical to pull's `UnitKey`. An `impl` seed (hit on the impl header) contributes edges from all its methods; its fan-out is controlled by rank and budget like anything else.

**Stopping conditions** (each is named in the output): depth limit, converged (no new candidates), unit budget, character budget. Every omitted candidate is listed with its reason; nothing is dropped silently.

**Determinism:** all orderings are total (path, start); no hash-order iteration reaches the output; running twice yields byte-identical output.

## 5. CLI semantics **[P]**

**Syntax:** `cascade:N`, `N` = digits only, `0 ≤ N ≤ 5`, any position among the arguments.

| Case | Behavior |
|---|---|
| No directive | Identical to today (§2.1). |
| `cascade:0` | **Byte-identical** to no directive (no header, no report). Alternative in D8. |
| `cascade:N` token | Recognized as a directive **before** `parse_query`, so it can never become a search term. |
| `cascade:`, `cascade:x`, `cascade:-1`, `cascade:6` | Error, exit 2, `CVB pull: cascade: must be followed by a depth from 0 to 5.` |
| Two `cascade:` tokens | Error (mirrors "only one file: scope is allowed"). |
| `-cascade:N` | Error in the same family as `-file:` (extend the checks at pull.rs:141, 240, 442). |
| `+cascade:N`, `\cascade:N`, `'"cascade:N"'` | Literal search term, as `+file:` etc. behave today. |
| Quoted/`+`/`-` terms, OR semantics | Unchanged. They select **seeds only**. |
| `-term` exclusions | Filter seeds only. Cascaded units are not filtered by them; test code is already excluded from the index, which covers the usual `-test` intent. |
| **`file:PATH`** | **Restricts the initial search only; dependencies resolve across all tracked `.rs` files.** Every item shows its path, so leaving the scope is visible. See D1. |
| `lines:` | v1: error `cascade: cannot be combined with lines:` (mirrors the `callers:` rule, pull.rs:490). |
| `callers:` | v1: error (otherwise the existing message would say "cannot be combined with search terms"). |
| Lone `file:PATH cascade:N` | v1: explicit error (whole-file capture has no seeds). Today this is a search for the literal text `cascade:N` inside that file; once the directive is stripped first it would otherwise fall into "search query is empty", so it needs its own message. |
| No initial matches | Output and exit code 1 unchanged; cascade not attempted. |
| Non-Rust or test seeds | Printed as today; reported as `N initial units not followed (non-Rust / test code)`. |
| Exit code | 0 whenever initial results exist, even if budget was hit or references are unresolved (those are reported in-band). Failure to parse part of the index adds a warning in the report, not a changed exit code. |

Existing diagnostics stay: the first line is still the `Query:` echo, so users can still check argument boundaries first. For cascade runs only, one extra line follows it (see §6).

## 6. Output format **[P]**

Illustrative, for `cvb pull "fn process_with" file:src/clipboard.rs cascade:2`. Names and line spans are from `cvb-0.6.0` (**[V]** for spans of `process_with` 178–205, `chunk_messages` 142–167, `split_chunk_payloads` 86–140, `chunk_prefix` 74–84, `ClipboardBuffer` 5–7, `len` 276–278, `as_bytes` 280–282); source lines are elided with `[…]` here for brevity, the real output prints every line. Budget counters are illustrative.

```
Query: fn process_with
Cascade: depth 2 | limits 25 units, 24000 chars | scope: initial search in src/clipboard.rs, dependencies repo-wide
  Rust function: src/clipboard.rs:178-205
  fn process_with: 178
    178 | fn process_with<C, S>(
    […]
    205 | }

== CASCADE depth 1 (4 units) ==
  Rust function: src/clipboard.rs:142-167
  cascade: depth 1 | callee of process_with (call at :198) | resolved by name in same file
    142 | fn chunk_messages(text: &str, limit: usize) -> Result<Vec<String>, String> {
    […]
  Rust function: src/clipboard.rs:276-278
  cascade: depth 1 | method on ClipboardBuffer, called at :190 | receiver type from parameter `buffer: &ClipboardBuffer`
    […]
  Rust function: src/clipboard.rs:280-282
  cascade: depth 1 | method on ClipboardBuffer, called at :188 | receiver type from parameter `buffer: &ClipboardBuffer`
    […]
  Rust struct: src/clipboard.rs:5-7
  cascade: depth 1 | type in signature of process_with
      5 | pub(crate) struct ClipboardBuffer {
      6 |     contents: Vec<u8>,
      7 | }

== CASCADE depth 2 (2 units) ==
  Rust function: src/clipboard.rs:86-140
  cascade: depth 2 | callee of chunk_messages (call at :150) | resolved by name in same file
    […]
  Rust function: src/clipboard.rs:74-84
  cascade: depth 2 | callee of chunk_messages (call at :159) | resolved by name in same file
    […]

== CASCADE report ==
Stopped: depth limit (2). Budget: 6 of 25 added units, 4,312 of 24,000 chars. Nothing omitted.
Not followed, unresolved (2):
  copy(…)           src/clipboard.rs:190   call through parameter `copy` (callback; target not statically known)
  copy_sequence(…)  src/clipboard.rs:204   call through parameter `copy_sequence` (callback)
Not followed, external or std: 14 distinct names, not listed.
Not followed, by scope: callers, tests, trait impls. See `cvb pull callers:process_with`.
Analysis is syntactic (no types, trait dispatch, generics, macro expansion or cfg). A dependency missing from this bundle may still exist.
```

**Budget exhaustion looks like this** (illustrative, for a different run, `cvb pull "fn run_capture" cascade:2`; it replaces the "Nothing omitted" part):

```
Stopped: character budget (24000) at depth 2. Omitted (2):
  src/pull.rs:499-592    run_callers      callee of run_capture      94 lines  budget:chars  → cvb pull file:src/pull.rs lines:499-592
  src/pull.rs:1210-1274  rust_unit_range  callee of resolve_unit     65 lines  budget:chars  → cvb pull file:src/pull.rs lines:1210-1274
```

A unit over the size cap is listed the same way with reason `unit-too-large`; for example, reaching `run_capture` (752–1137, 386 lines) as a callee of `run_in_dir` would print `… 386 lines  unit-too-large  → cvb pull file:src/pull.rs lines:752-1137`.

Design notes: initial results keep today's exact block shape; each cascaded block adds **one** `cascade:` line (depth, relationship, how it was resolved) so the reason is attached to the code; the report is always emitted, even with a zero budget, and its space is reserved before any unit is admitted so it can never be cut off; omitted items carry a ready-to-run `cvb pull … lines:` command, which keeps the human in charge of the next step.

## 7. Resource budgets **[P]**, with data **[M]**

Reference data: bundle size in characters (excluding the ~25% numbering overhead) for seeds in this repo, tests excluded, name-only vs. unique-name matching, at depth 3:

| Seed | Units (strict) d1 / d2 / d3 | Chars name-only d3 | Chars strict d1 / d2 / d3 |
|---|---|---|---|
| `process_with` | 4 / 6 / 7 | 8,038 | 1,634 / 3,875 / 4,008 |
| `chunk_messages` | 4 / 5 / 5 | 9,026 | 3,089 / 3,222 / 3,222 |
| `rust_callers` | 6 / 6 / 6 | 12,425 | 6,621 / 6,621 / 6,621 |
| `run_internal_target` | 5 / 8 / 10 | 15,672 | 4,186 / 9,786 / 9,993 |
| `expand_matches` | 5 / 9 / 12 | 18,451 | 6,041 / 9,043 / 9,660 |
| `resolve_unit` | 4 / 9 / 12 | 18,989 | 6,446 / 9,581 / 10,198 |
| `run_capture` (386 lines) | 15 / 23 / 29 | 55,789 | 31,694 / 42,134 / 46,998 |

Observations: depth 3 usually saturates; name-only matching roughly doubles size on several seeds (up to 3× on `chunk_messages`); and one coordinator function (`run_capture`) already produces about 3× the default copy limit at depth 1 and nearly 5× at depth 3. A depth limit alone is not a bound.

| Control | Proposed default | Rationale |
|---|---|---|
| Max depth | 5 (hard error above); `N` always explicit | Data show saturation by 3; 5 leaves headroom without inviting runaway. |
| Max added units | **25** | Covers the full depth-3 reach of 6 of 7 sample seeds, caps coordinators. |
| Max added chars (rendered, incl. line prefixes and headers) | **24,000** | About 2.5 × the default 9,750-char copy limit, so at most about three pastes. An extra paste costs a keypress, far less than an LLM round trip; much more starts to defeat the point and strains chat input limits. Applies to cascade additions; initial results are never trimmed. |
| Max unit size | **200 lines**; larger units are listed as omitted with a `lines:` hint | One 400-line function should not consume the bundle. |
| Utility demotion `H` | **8** distinct caller units | In this small repo only the three std-name collisions exceed it, so it is unvalidated for larger repos (D15). |
| Report reserve | 1,500 chars, taken first | Guarantees the stop reason is always visible. |
| Index guard | Warn when more than N tracked `.rs` files are parsed (N to be chosen) | Cost is one syn parse per file per run; trivial here (11 files, 9.8k lines), unknown for large repos (D13). |

When the budget is nearly exhausted: candidates are admitted in rank order; one that does not fit is recorded as omitted and the next smaller one is tried; admission stops when nothing fits; the stop reason names which limit was hit.

Defaults are constants in v1 and printed in the `Cascade:` header line. Whether to make them configurable (config file like `~/.config/cvb/limit`, or derive from it) is D7.

## 8. Testing strategy

Pattern: follow the existing temp-git-repo end-to-end style (`run_end_to_end_in_temporary_git_repo`, pull.rs:2429) via `run_capture`, plus pure unit tests on `cascade.rs`. Add `scripts/test-cascade.sh` in the style of `scripts/test-pull-exclude.sh`.

- **Compatibility (highest priority).** Golden outputs for a corpus of queries run before and after the change must be byte-identical: bare/quoted/`+`/`-`/`\`-escaped terms, `file:`, `file:`+`lines:`, whole-file, `callers:`. The existing 56 pull tests stay green unmodified. `cascade:0` equals no-directive, byte for byte. The `rust_items()` refactor lands behind these goldens.
- **Directive parsing.** Position independence; invalid values; duplicates; `-cascade:`; `+cascade:`/`\cascade:` literal behavior; combinations with `callers:`, `lines:`, whole-file.
- **Edge extraction.** Per syntax form: path call, `Self::`, `Type::`, `module::`, method with typed receiver, method with unknown receiver, macro argument, function-as-value, signature types; comments/strings ignored.
- **Resolution.** Each rule in 3.3, including: `Command::new`/`Vec::new` external; two project `new`s ambiguous; unique name with wrong qualifier external; parameter callback unresolvable; same name in a `#[cfg(test)]` module not indexed; cfg-duplicated items reported ambiguous.
- **Traversal.** Linear chain; diamond (one emission, both reasons); self-recursion; mutual recursion within and across files; depth boundaries (`cascade:1` yields only d1; N larger than the chain stops "converged"); a callee that is also an initial hit appears once at depth 0.
- **Scope.** Seed in `file:` scope, callee outside it: followed and labeled; callee in an unparseable file: reported and flagged incomplete.
- **Budgets.** Exactly at and one over the unit and char limits; oversized unit omitted with a correct `lines:` hint; zero budget still prints the report; utility demotion makes a leaf; the sum of emitted chars never exceeds the budget.
- **Properties.** No `UnitKey` emitted twice; every key within file bounds; output byte-identical across runs and across file enumeration order.
- **Seeds.** Test-module, Bash, Markdown and `"Line"` seeds printed but not followed, with the report count.
- **Realistic dogfooding.** (1) Replay 10 past CVB tasks (e.g. the surgery and chunker work) and count follow-up pulls needed before vs. after; (2) track a noise ratio: admitted units the LLM never referenced; (3) run on a second, larger Rust codebase (your `pot` TUI is a candidate) to validate `H`, index cost and defaults; (4) review the `cascade:` reason lines for wrong links.

## 9. Implementation plan

Each stage is independently reviewable, ends green, and is committed only on your authorization.

| Stage | Content | Touches existing code? |
|---|---|---|
| S0 | Approve decisions in §10 | — |
| S1 | `extract_cascade_directive` + validation + error messages + extended negated-selector checks; `cascade:0` ≡ plain; `N ≥ 1` returns a clear "not implemented" error until S7 | small, in `run_capture` |
| S2 | Refactor `rust_unit_range` into shared `rust_items()`; golden tests prove no change | yes (refactor) |
| S3 | `cascade::index`: tracked `.rs` → definitions (kind, name, owner, span, test flag, param types); fixtures | no |
| S4 | Edge extraction per unit (share the macro token scan with `rust_callers` instead of copying it) | small |
| S5 | Resolver with the rules in 3.3 and classification; fixtures for each rule | no |
| S6 | Traversal, ranking, dedupe, stopping; cycle/diamond/depth fixtures; determinism property tests | no |
| S7 | Budgets, report, renderer, wiring into `run_capture`; end-to-end tests; golden outputs | `run_capture` hook |
| S8 | `format_usage`, `manual.md`, `cvb.1`; `scripts/test-cascade.sh`; dogfooding report; tuning of defaults | docs |

## 10. Open questions and assumptions

**Decisions needing your approval** (recommendation first):

| # | Question | Recommendation |
|---|---|---|
| D1 | `file:` scope: initial search only (A), or also limits where dependencies resolve (B)? | **A**, with paths shown on every item. B could be added later as an explicit option. |
| D2 | Auto-include ambiguous candidates (e.g. 2 definitions named `new`)? | No: list with locations; the LLM or you can pull one. |
| D3 | Follow method calls whose receiver type is unknown? | No (§3.3 rule 5, evidence in §3.3). |
| D4 | Include signature types in v1, or callees only? | Include (declaration only, no impls). |
| D5 | Test seeds are shown but not followed; test modules excluded from the index. | Yes. |
| D6 | Accept bare `cascade` without a number (e.g. default depth 2)? | No, explicit `N` only. |
| D7 | Budgets: constants, config file, or derived from the clipboard copy limit (which couples pull to a private clipboard setting)? | Constants in v1, printed in the header. |
| D8 | `cascade:0` byte-identical, or print a one-line note that the directive was honored? | Byte-identical (matches the handoff). |
| D9 | Layout: grouped by depth (proposed) or ordered by file/line for source readability? | Depth groups; revisit after dogfooding. |
| D10 | Support `cascade` with `lines:` (seeds = units enclosing the range) later? | Later; reject now. |
| D11 | Bash cascade (function calls in `cvb.bash`, via tree-sitter-bash)? | Out of v1. |
| D12 | Separate opt-ins for callers/tests/trait impls later (syntax not proposed here)? | Defer. |
| D13 | Index all tracked `.rs` files, or only files reachable from seeds? Large-repo guard threshold? | All tracked `.rs`, with a warning guard; measure on a larger repo. |
| D14 | Unit-size cap of 200 lines. | Accept as starting value. |
| D15 | Utility-demotion `H = 8`, leaf-only. | Unvalidated; tune in dogfooding. |
| D16 | Share the macro token scan with `rust_callers` (touches existing code) vs. duplicate it? | Share, behind goldens. |
| D17 | Exit code when the index was incomplete but initial results exist: keep 0 + warning (proposed) or use 2 as `callers:` does for a no-result case? | Keep 0 + report warning. |

**Assumptions:** the `cvb-0.6.0` branch matches your working tree; `pull`'s tracked-files-only rule stands (an untracked new file is invisible to cascade too); syn remains an acceptable dependency; defaults above are starting values to be confirmed by dogfooding, not conclusions.

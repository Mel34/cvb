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
* `cvb pull [file:PATH] [cascade:N] QUERY...` — search for initial seed units and expand context by following dependencies up to depth `N` (`0 <= N <= 5`).
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
| `cascade:N` | **Context expansion** | `cascade:2` | Expands syntactic dependencies up to depth `N` (`0 <= N <= 5`). |

*Syntax Rules:*
* **No exclusion-only queries:** `-tests` alone is rejected (`exit 2`); exclusions require at least one positive term.
* **No negated selectors:** `-file:`, `-lines:`, `-callers:`, and `-cascade:` are rejected (`exit 2`). Exclusions only apply to content terms.
* **Literal cascade terms:** `+cascade:N` requires the literal text `"cascade:N"`. `\cascade:N` or `"cascade:N"` searches for it as an alternative.

#### Unit Boundaries (Where `+` and `-` Match)

* **Rust:** Smallest enclosing function or method. Struct declarations, enum declarations, and `impl` blocks count as separate units.
* **Bash:** Smallest enclosing `function_definition`.
* **Markdown:** Current heading section, including all nested subsections.
* **Fallback / Non-AST:** Matches single lines. If a parser fails on a syntax error, CVB emits a warning and falls back to line-level matching for that file.

---

### Context Expansion (`cascade:N`)

`cascade:N` deterministically expands context around matching search results by following Rust call graphs and type dependencies across all Git-tracked `.rs` files. It is offline, local, and strictly bounded.

#### Depth Tiers

* **Depth 0:** Initial matching units (seeds) found by your query terms and optional `file:` filter.
* **Depth 1:** Direct syntactic dependencies referenced by Depth 0 seeds (callees, signature types).
* **Depth 2:** Direct syntactic dependencies of units admitted at Depth 1.
* **Depth d ($3 \le d \le N$):** Direct dependencies of units admitted at Depth $d-1$.

#### CLI Constraints

* `cascade:0` is **byte-identical** to omitting the directive (no headers, no report).
* Depths $>5$ or invalid numbers error (`exit 2`). Only one `cascade:` directive is allowed per command.
* Cannot be combined with `lines:`, `callers:`, or lone `file:PATH` whole-file captures (`exit 2`).
* When combined with `file:PATH`, the file scope restricts **initial seeds (Depth 0) only**; cascade traverses dependencies repo-wide.

#### Resolution Rules

1. **Followed and Expanded:**
   * **Free function calls:** Bare `foo()` resolved to the same file, to an explicit `use` import, or to a unique project-wide free function.
   * **`Self::method()`:** Resolved deterministically against the enclosing `impl` block's target type.
   * **Associated functions:** `Type::new()` where `Type` is defined within the project.
   * **Typed methods:** `recv.method()` only when the receiver type is syntactically determinable (from `self`, typed parameters `&T`, smart pointers `Box`/`Arc`/`Rc`, or annotated `let` bindings).
   * **Macro candidates:** Calls inside macro bodies are identified via token scanning and labeled as candidate leads.
   * **Signature types:** Struct and enum definitions appearing in parameter types or return types (including inside containers like `Result<Vec<T>, E>`). Declarations only; `impl` blocks are not pulled.

2. **Not Followed (Reported in Footer):**
   * **Unknown receiver methods:** Calls like `obj.len()` without a statically provable receiver type are isolated under unresolved methods to prevent stdlib noise.
   * **Callbacks:** Bare calls matching a parameter name (e.g. `callback(...)`) are identified as unresolvable callbacks.
   * **Trait dispatch:** Dynamic dispatch (`dyn Trait`) and generic constraints without static resolution are not guessed. Automatic sweeps across all implementations of a trait are not performed.
   * **Test code:** Functions marked `#[test]` and modules guarded by `#[cfg(test)]` (including compound expressions like `#[cfg(all(test, ...))]`) are excluded from the dependency index.
   * **External & std calls:** Tallied by distinct name count in the footer.
   * **Ambiguous names:** Multiple matching definitions are listed with candidate locations (`path:start-end`) rather than guessed.

#### Budgets & Limits

* **Max Depth:** 5.
* **Max Added Units:** 25 units. Subsequent candidates are omitted as `budget:units`.
* **Max Added Size:** 24,000 characters (rendered). Subsequent candidates are omitted as `budget:chars`.
* **Max Unit Size:** 200 lines. Units $>200$ lines are omitted as `unit-too-large` with an actionable `lines:` command hint.
* **Utility Demotion ($H$):** Functions called by $\ge 8$ distinct units repo-wide become leaf nodes (body included, callees not followed).
* **Report Reserve:** 1,500 characters reserved up-front to guarantee diagnostic report visibility.

---

### Interpreting `cvb pull` Output & Diagnostics

All `cvb pull` output begins with a parsed query echo on the first line. For cascade runs with $N \ge 1$, a `Cascade:` header line immediately follows the query echo.

#### 1. Success (`exit 0`) — Normal Search
```text
Query: (CRLF OR BOM) AND split_inclusive AND NOT tests
-tests: excluded 2 units
  Rust function: src/pull.rs:120-185
  CRLF: 125, 140
  +split_inclusive: 162
    120 | fn parse(...) {
    ...

# Cascade Context Expansion (cascade:N)

cvb pull [file:PATH] [cascade:N] <search-term>...

## Overview

cvb pull can deterministically expand context around matching search results by following Rust call graphs and type dependencies across all Git-tracked files in the repository.

Rather than requiring multiple round-trip queries or pulling entire files, cascade:N expands context outward up to N steps using conservative syntactic analysis (syn) running offline on the local CPU.

- Deterministic & Local: Output is byte-identical across repeated runs for any given Git-tracked source snapshot and query.
- Strictly Bounded: Enforces unit limits, character budgets, per-unit line caps, and utility helper demotion to prevent bundle explosion.
- Transparent Honesty: Clearly distinguishes resolved code from ambiguous, dynamic, or external dependencies in a structured trailing report.

## Understanding Depth Levels

Cascade organizes dependencies into level-synchronous breadth-first tiers:

- Depth 0: The initial search results (seeds) matching your query terms and optional file: filter.
- Depth 1: Direct syntactic dependencies referenced by the Depth 0 seeds (callees, signature types).
- Depth 2: Direct syntactic dependencies of units admitted at Depth 1.
- Depth d (3 <= d <= N): Direct syntactic dependencies of units admitted at Depth d-1.

## Syntax & CLI Rules

- cascade:N: Expands dependencies up to depth N (0 <= N <= 5). Allowed anywhere among query arguments.
- cascade:0: Byte-identical to omitting the directive entirely (no cascade headers, no cascade report).
- cascade:6+, cascade:x, cascade:-1: Error (exit code 2). Depth must be an integer from 0 to 5.
- Multiple cascade: directives: Error (exit code 2). Only one cascade: directive permitted per command.
- -cascade:N: Error (exit code 2). Negated selectors are rejected; '-' exclusions only apply to content search terms.
- +cascade:N: Recognized as a required search term (+term), requiring the literal text "cascade:N" in matching units.
- \cascade:N or "cascade:N": Recognized as an alternative search term, searching for literal "cascade:N".
- file:PATH: Restricts the initial search seeds only (Depth 0). Dependencies resolve across all Git-tracked .rs files repository-wide.
- lines: or callers:: Error (exit code 2). Cascade cannot be combined with line ranges or caller searches.
- Lone file:PATH cascade:N: Error (exit code 2). Whole-file capture has no seeds to expand.

## Dependency Discovery & Resolution Rules

Cascade operates syntactically against an index of all Git-tracked .rs files. Items are resolved conservatively according to the following rules:

1. Followed and Expanded:
- Free function calls: Bare foo() calls resolved first to the same file, then to an explicit use import, then to a unique project-wide function.
- Self::method(): Resolved deterministically against the enclosing impl block's target type.
- Associated functions & constructors: Type::new() where Type is defined within the project.
- Typed method calls: recv.method() only when receiver type is syntactically determinable (e.g. from self, an explicit parameter type like buffer: &ClipboardBuffer, smart pointers Box/Arc/Rc, or an annotated local variable).
- Macro call candidates: Call invocations inside macro bodies are identified via token scanning and labeled as macro-argument candidates (syntactic leads, not proof of invocation).
- Signature types: Struct and enum definitions appearing anywhere in function parameter types or return types—including inside container generics such as Result<Vec<MyType>, Error>. Declaration only; impl blocks are not pulled.

2. Not Followed (Reported in Footer):
- Unknown-receiver method calls: Calls like obj.len() where obj's concrete type cannot be proven syntactically are listed under unresolved methods rather than guessed, preventing stdlib method flood (e.g. len, push).
- Callbacks & function pointers: Bare calls matching a parameter name (e.g. callback(...)) are identified as callbacks and reported.
- Dynamic & trait dispatch: Trait method calls via dynamic dispatch (dyn Trait) or generic constraints without static resolution are not guessed. Automatic sweeps across all implementations of a trait are not performed.
- Test code: Functions marked #[test] and modules guarded by #[cfg(test)] (including compound expressions like #[cfg(all(test, ...))]) are excluded from the dependency index. Test seeds match search but are not expanded.
- External & standard library calls: Names not originating in the project (e.g. Command::new, std::fs::read) are tallied and omitted from output.
- Ambiguous names: Functions or methods defined in multiple locations without qualifiers are listed with their candidate locations (path:start-end) so you can pull them explicitly if desired.

## Resource Budgets & Safeguards

To prevent overloading LLM context windows or clipboard buffer limits:

- Max Depth: 5. Hard ceiling. Traversal halts when depth N is reached.
- Max Added Units: 25 units. Traversal stops admitting new units; subsequent candidates are reported as budget:units.
- Max Added Size: 24,000 characters. Total rendered output cap for cascaded units. Subsequent candidates are reported as budget:chars.
- Max Unit Size: 200 lines. Units exceeding 200 lines are omitted with reason unit-too-large and an actionable lines: command hint.
- Utility Demotion (H): 8 callers. Functions called by 8 or more distinct units repo-wide become leaf nodes (body included, callees not followed).
- Report Reserve: 1,500 characters. Reserved up-front from the character budget to guarantee diagnostic visibility. (Note: does not hard-truncate the report itself).

Every omitted unit provides a ready-to-run follow-up command:
  src/pull.rs:752-1137  run_capture  callee of run_in_dir  386 lines  unit-too-large  -> cvb pull file:src/pull.rs lines:752-1137

## Output Format

Query: fn process_with
Cascade: depth 2 | limits 25 units, 24000 chars | scope: initial search in src/clipboard.rs, dependencies repo-wide
  Rust function: src/clipboard.rs:178-205
  fn process_with: 178
    178 | fn process_with<C, S>(
    [...]
    205 | }

== CASCADE depth 1 (3 units) ==
  Rust function: src/clipboard.rs:142-167
  cascade: depth 1 | callee of process_with (call at :198) | resolved by name in same file
    142 | fn chunk_messages(text: &str, limit: usize) -> Result<Vec<String>, String> {
    [...]
  Rust function: src/clipboard.rs:276-278
  cascade: depth 1 | method on ClipboardBuffer, called at :190 | receiver type from parameter `buffer: &ClipboardBuffer`
    [...]
  Rust struct: src/clipboard.rs:5-7
  cascade: depth 1 | type in signature of process_with | type in signature of process_with
      5 | pub(crate) struct ClipboardBuffer {
      6 |     contents: Vec<u8>,
      7 | }

== CASCADE depth 2 (2 units) ==
  Rust function: src/clipboard.rs:86-140
  cascade: depth 2 | callee of chunk_messages (call at :150) | resolved by name in same file
    [...]

== CASCADE report ==
Stopped: depth limit (2). Budget: 5 of 25 added units, 3,980 of 24,000 chars. Nothing omitted.
Not followed, unresolved (2):
  copy(…)           src/clipboard.rs:190   call through parameter `copy` (callback; target not statically known)
  copy_sequence(…)  src/clipboard.rs:204   call through parameter `copy_sequence` (callback; target not statically known)
Not followed, external or std: 14 distinct names, not listed.
Not followed, by scope: callers, tests, trait impls. See `cvb pull callers:process_with`.
Analysis is syntactic (no types, trait dispatch, generics, macro expansion or cfg). A dependency missing from this bundle may still exist.

## Examples

Search for a function and pull its direct dependencies (Depth 1):
    cvb pull "fn chunk_messages" cascade:1

Restrict initial search to a specific file, but expand dependencies repo-wide to Depth 2:
    cvb pull "fn process_with" file:src/clipboard.rs cascade:2

Combine boolean content filters with cascade:
    cvb pull +split_inclusive -test cascade:2

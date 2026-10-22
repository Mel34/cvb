# Handoff: CVB Pull Cascade — Deterministic Context Expansion

## Objective

Design an extension to CVB's existing `cvb pull` command that automatically retrieves relevant source-code dependencies, reducing the repeated human/LLM back-and-forth currently required to assemble enough code context for a programming task.

The deliverable for this assignment is a **design document**, not an implementation. Inspect the existing CVB architecture and base the proposal on the actual code rather than assumptions.

## 1. Problem Statement

CVB's current retrieval workflow is human-directed:

1. The user runs `cvb pull` to retrieve a function or other relevant source section.
2. The LLM inspects the result and identifies a referenced function, type, or implementation that it needs to understand the code.
3. The LLM asks the user to perform another `cvb pull`.
4. The cycle repeats until sufficient context has been assembled.

This works, but each additional request costs time, interrupts reasoning, and adds conversational overhead.

We want CVB to anticipate many of these predictable follow-up retrievals locally, using deterministic source-code analysis on the user's CPU.

**The objective is to reduce round trips, not to replace the LLM's reasoning.**

The LLM remains responsible for interpreting the assembled code, understanding the task, evaluating alternatives, and making engineering decisions. CVB's responsibility is to collect a more complete and coherent source context before that reasoning begins.

## 2. Proposed User-Facing Syntax

The proposed syntax is:

`cvb pull "search term" cascade:3`

Here, `cascade:3` requests up to three additional levels of automatic retrieval beyond the initial search.

A combined example:

`cvb pull "process_with" file:src/clipboard.rs cascade:3`

The precise syntax and argument-parsing behavior must be checked against the existing implementation before finalizing the specification.

### Depth semantics

Define the initial search as depth 0:

- **Depth 0:** Perform the ordinary `cvb pull` search.
- **Depth 1:** Analyze the initial results and retrieve relevant dependencies.
- **Depth 2:** Analyze newly retrieved definitions and retrieve their relevant dependencies.
- **Depth 3:** Expand one further level, then stop.

`cascade:0` should behave like ordinary retrieval, with no additional expansion.

The requested depth is a maximum, not a target. If no useful dependencies remain, stop early.

The ordinary `cvb pull` behavior must remain unchanged when the cascade directive is absent.

## 3. Core Architectural Principle: Deterministic, Not LLM-Based

The cascade engine must not call an LLM, depend on an AI API, or require network access.

All analysis should run locally and deterministically.

Candidate techniques include:

- Parsing Rust source syntax.
- Identifying function definitions and function calls.
- Resolving references to definitions within the repository.
- Identifying referenced structs, enums, traits, and other project-defined types.
- Following relevant relationships between modules.
- Deduplicating discovered symbols and source sections.
- Ranking candidate retrievals using explicit, explainable rules.
- Stopping at the configured depth or a resource budget.

Investigate whether a proper Rust parser, such as `syn`, fits the existing architecture and dependency policy. Compare this with simpler approaches where appropriate. Do not assume that regular expressions alone can reliably resolve Rust source relationships.

The system must acknowledge that complete resolution is difficult. Macros, trait dispatch, generics, function pointers, dynamic dispatch, conditional compilation, and generated code can obscure dependencies. It must not claim complete dependency coverage when it cannot establish it.

## 4. Retrieval Strategy

Investigate a bounded, breadth-first traversal of source relationships.

At each depth:

1. Inspect definitions and relevant source sections already retrieved.
2. Extract candidate dependencies.
3. Resolve candidates to actual definitions when possible.
4. Rank candidates by relevance to understanding the original results.
5. Retrieve the most useful candidates.
6. Deduplicate them against everything already included.
7. Continue until the requested depth or a stopping condition is reached.

The first priority should be following callees and their definitions, because the principal use case is answering:

"What does this code call, and what do I need to inspect to understand how it works?"

Also evaluate the value of retrieving:

- Referenced type definitions.
- Trait declarations and implementations, when resolvable.
- Relevant callers.
- Tests exercising the code.
- Error-handling paths and supporting state definitions.

Do not assume all these relationships should be followed by default. Recommend a focused initial scope and explain why.

Avoid blindly following every reference. For example, a call to a generic logging helper should not automatically cause the entire logging subsystem to be retrieved. The design must prioritize relationships likely to explain the target behavior.

## 5. Output Requirements

The cascade should produce one coherent context bundle, suitable for an LLM to inspect without requiring repeated retrieval requests.

The output should distinguish:

- Initial search results.
- Automatically discovered definitions.
- The depth at which each item was discovered.
- The reason each item was included, where practical.
- Unresolved references and significant limitations.
- Anything omitted because of a resource budget.

Preserve the existing `cvb pull` output conventions where feasible. Avoid unnecessary formatting changes or unrelated redesigns.

Deduplicate overlapping definitions and sections. Avoid emitting the same function multiple times because it was discovered through different paths.

## 6. Resource Limits and Stopping Rules

A depth limit alone is insufficient: a single function may reference many other functions, each of which references many more.

Recommend explicit controls for:

- Maximum cascade depth.
- Maximum number of discovered symbols or source sections.
- Maximum total output size.
- Deduplication and cycle prevention.
- Candidate prioritization when the budget is nearly exhausted.
- Early termination when no useful candidates remain.

Budget exhaustion must be visible in the output. CVB must not silently truncate a bundle in a way that makes it appear complete.

The initial implementation should be predictable, bounded, and easy to debug.

## 7. Compatibility and Scope

This is an opt-in enhancement to existing `cvb pull`.

Requirements:

- No cascade unless explicitly requested.
- Existing query operators and syntax must continue to work, including quoted phrases, required terms, excluded terms, and file restrictions.
- `cascade:N` must be parsed as a control directive, not treated as an ordinary search term.
- Existing search semantics must not change as a side effect of implementing cascade.
- Avoid changes to unrelated CVB functionality.

The exact interaction between `cascade:N` and file restrictions needs careful consideration: a file restriction may limit the initial search without necessarily limiting where discovered dependencies can be resolved. Analyze the current behavior and propose clear semantics rather than guessing.

## 8. Design Document Deliverables

Produce a concise but technically substantive design document containing:

1. **Current architecture:** Explain how `cvb pull` currently parses queries, searches source, expands sections, and formats results, with references to the actual files and functions inspected.
2. **Proposed architecture:** Identify the smallest viable insertion point for cascade in the existing retrieval pipeline.
3. **Dependency discovery:** Explain how candidate symbols are extracted and resolved, including the strengths and limitations of the proposed method.
4. **Traversal algorithm:** Define depth, traversal order, deduplication, ranking, and stopping conditions.
5. **CLI semantics:** Specify how `cascade:N` interacts with all existing query operators and file restrictions.
6. **Output format:** Show a concrete example of a cascaded result.
7. **Resource budgets:** Recommend sensible initial defaults and explain their rationale.
8. **Testing strategy:** Cover unit tests, repository fixtures, depth-boundary tests, cycles, duplicates, unresolved symbols, query compatibility, output limits, and realistic dogfooding.
9. **Implementation plan:** Break implementation into small, reviewable stages.
10. **Open questions:** Explicitly list unresolved design decisions and assumptions requiring approval.

## 9. Working Discipline

This assignment is for architectural investigation and documentation only.

- Inspect the actual current CVB source before proposing changes.
- Do not modify Rust source code.
- Do not introduce dependencies.
- Do not commit or push anything.
- Do not silently decide ambiguous behavioral questions.
- Clearly distinguish verified facts about the existing implementation from proposed design choices.

The intended result is a document we can review together before authorizing implementation.

## Guiding Principle

CVB should do the predictable, mechanical work of finding relevant source context locally. The LLM should spend its effort reasoning about that context rather than repeatedly asking the user to retrieve one more function.

The success criterion is not a theoretically complete call graph. It is a substantial reduction in retrieval round trips while preserving correctness, transparency, bounded resource usage, and the existing human-directed nature of CVB.

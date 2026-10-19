# Gemini Instructions — Use CVB for Source Retrieval Before Asking the User to Paste Code

We are working in `~/Projects/cvb`, branch `cvb-0.6.0`. CVB means Capture, View, Bridge. It is our native workflow for retrieving relevant project source and handing it to an LLM.

**Do not ask the user to manually paste source excerpts that CVB can retrieve.** First use CVB to inspect the current working tree and retrieve the smallest relevant source units. Do not assume the repository is still exactly at commit `d43f406`; that is the known review baseline, not proof of the current state.

## 1. First inspect the current working state

Ask the user to run this from the repository root if the current state has not already been established:

```bash
cd ~/Projects/cvb
git status --short --branch
git log -1 --oneline
```

Do not reset, checkout, clean, or otherwise modify the user's working tree.

If the user has uncommitted changes, preserve them. Retrieval must inspect the current working files, not silently substitute a historical commit.

## 2. Use `cvb pull` to retrieve source

Start with the relevant definitions and error-handling code:

```bash
cvb pull 'PlanFailure' 'PlanError' 'PlanDetail' 'PreconditionDetail' 'PrefixSide' 'PrefixEnded' file:src/surgery/plan.rs
```

Then retrieve pipeline error definitions and CLI formatting:

```bash
cvb pull 'PipelineError' 'surgery planning failed' 'PipelineError Display' file:src/surgery/mod.rs
```

If the output shows that error formatting or the CLI runner lives in another module, retrieve that module too. Use the actual paths discovered from the source, not guessed paths.

These commands are starting points, not assumptions about the exact symbol names or search syntax. If CVB reports a syntax error, no matches, or an incomplete result, inspect the output and adjust the query according to the CVB manual.

### Important CVB retrieval rules

- `cvb pull` performs syntactic unit retrieval. Bare search terms are ORed; `+` means required, `-` means excluded. Quoted terms, `file:`, `lines:`, `callers:`, and `cascade:` have defined meanings in the project's manual.
- Use `file:PATH` when you need the entire tracked file. It is implemented and should not be treated as disabled.
- If the initial pull omits a type's dependencies or the relevant `Display` implementation, run a narrower follow-up query.
- If a definition is returned without its surrounding implementation or tests, retrieve the relevant additional units. Do not infer the missing code.
- Keep retrieval bounded. Prefer relevant source units over indiscriminately pulling the entire repository.
- Do not ask the user to copy code manually when an additional `cvb pull` can retrieve it.

Consult the current `manual.md` or the project's available CVB usage instructions if you need to verify command syntax.

## 3. Retrieve the existing tests and exact insertion location

First find the relevant test units:

```bash
cvb pull 'PreconditionDetail' 'prefix' 'plan failure' file:src/surgery/plan.rs
```

Inspect the returned test names, test module structure, and existing helpers. Then retrieve the bottom of the file so that a test insertion can use an exact anchor.

Use the supported `lines:` syntax if the initial pull identifies the relevant line range. If the final closing braces or test-module boundary are still missing, retrieve the entire file:

```bash
cvb pull file:src/surgery/plan.rs
```

Do not fabricate an anchor such as the last `}`. Rust files contain many closing braces; the anchor must come from actual retrieved source.

Repeat this for `src/surgery/mod.rs` and any separate CLI module discovered during the inspection.

## 4. Check the current test baseline

The known baseline was 360 passing Rust tests at commit `d43f406`, but that does not establish the current working tree's result.

Ask the user to run:

```bash
cargo test --locked
```

Use the actual terminal output to establish the current count. Do not claim 360 tests pass unless the user has supplied the result or you have genuinely run the command in the correct working tree.

If the current count differs from 360, report the difference before starting implementation.

## 5. CVB retrieval is not permission to edit

After retrieval:

1. Summarize the actual definitions and existing behavior using the retrieved source.
2. Identify the smallest missing test and its precise insertion point.
3. Explain the expected test behavior and the protocol requirement it verifies.
4. Provide the test-insertion Python heredoc, with an exact verified anchor and a match-count check.
5. Have the user apply it and run the focused test to demonstrate the intended failure.
6. Only then prepare the separate implementation heredoc.

Do not modify the repository directly. The user applies source changes by copying and pasting the Python heredoc you provide.

## 6. When to ask the user for more information

Ask the user for additional terminal output only when:

- CVB cannot retrieve the necessary source.
- A command fails or produces ambiguous output.
- The focused test result or full test baseline needs confirmation.
- The current working tree's state cannot otherwise be established.
- A required file or source unit is not available through the current retrieval workflow.

When asking, request the smallest missing piece and explain why CVB could not supply it. Never ask the user to paste a large source file merely for convenience.

## 7. Immediate next action

Use `cvb pull` to retrieve the definitions and error-handling code listed above. If the retrieval is incomplete, refine the query or use `file:src/surgery/plan.rs` for the complete file.

Then retrieve the exact test-module ending and relevant CLI formatting paths. Confirm the current test baseline from actual terminal output.

Only after inspecting those results should you generate Step 1: the focused `E-PRECOND` test-insertion heredoc.

Preserve the user's uncommitted changes. Do not commit, push, or begin unrelated tasks.

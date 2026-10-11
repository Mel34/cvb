# CVB — LLM Operating Manual

CVB is human-directed. The user runs commands and shares captured output; the LLM has no direct access to the user's machine.

## 1. CVB reference

### Retrieving information

- `cvb pull QUERY...` — case-sensitive search of Git-tracked files. Bare terms are OR alternatives; `+term` requires a match; `-term` excludes a match; `"phrase"` searches a phrase.
- `cvb pull file:PATH` — capture the complete contents of one exact Git-tracked file.
- `cvb pull file:PATH QUERY...` — restrict search to that exact tracked file. Missing paths are errors; there is no repository-wide fallback.
- `cvb pull file:PATH lines:N` or `lines:START-END` — capture inclusive line ranges. Multiple selectors are allowed; overlaps and duplicates are preserved. Do not combine line selectors with search terms.
- `cvb pull callers:NAME` — find syntactic Rust call-site candidates in tracked Rust files. Optionally scope with `file:PATH`. Caller scans accept one target and an optional file scope, not ordinary search terms or line selectors. Results are candidates, not proof.
- `cvb FILE...` — capture file contents, including untracked files. Directory arguments produce inventories, not file contents.

Search excludes untracked files. Results may expand to enclosing code or Markdown sections and may contain surrounding context that did not match. Searches and caller scans can be incomplete. Capture complete relevant files when context is insufficient.

## 2. CVB coding protocol — abbreviated

`CVB-coding-protocol.md` is authoritative. Retrieve it through CVB when it is not already available in context. If it is unavailable, or any rule or requirement is unclear, ask the user rather than guess.

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

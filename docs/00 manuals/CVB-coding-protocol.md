CVB Coding Protocol
1. Plain Markdown only
- Use ordinary Markdown and standard Markdown code fences.
- Never use WritingBlocks for coding instructions.
- Keep commands and code easy to copy.
2. One atomic change at a time
- Make one logically isolated change per step.
- No unrelated edits or bundled refactors.
- Verify each change before proceeding to the next.
3. Surgical Python edits
- Inspect the current source before editing.
- Provide surgical edits as a complete, copyable terminal command using a Python heredoc:
python - <<'PY' ... PY
- Validate exact source anchors and expected occurrence counts before writing.
- Abort without modifying the file if any precondition fails.
- Make edits atomically to prevent partial writes.
- Re-read the resulting file and verify the change after writing.
- Run follow-up verification commands from within the same Python script, but only after the edit and post-write verification succeed. Examples include cargo check and targeted tests.
- If any precondition, edit, write, or post-write verification fails, abort and do not run the follow-up commands.
- Print explicit success or failure status, including the actual results of follow-up checks.
- Never claim success without evidence.
4. Full-file rewrites
- Provide the complete replacement file in an ordinary Markdown code block.
- The user manually replaces the entire file.
- Do not provide base64 replacement commands.
5. Copyable terminal commands
- Every terminal command the user must execute goes in an ordinary, copyable Markdown code block.
- This includes commands mentioned in follow-up instructions.
- Never leave a required command only inline.
6. Group related commands
- Group commands for the same editing or verification step into one copyable code block whenever practical.
- Avoid unnecessary command-by-command clipboard exchanges.
7. Verify every change
- Run relevant cargo check and targeted tests.
- Run broader tests when appropriate.
- Wait for the actual output before proceeding.
- Prefer verification that is part of the same fail-fast Python workflow when editing files.
8. Never assume success
- Never claim an edit or test ran without confirmation.
- Review the resulting source, not just test results.
- Do not silently treat a command's lack of visible errors as proof of success.
9. Stay focused
- Avoid giant explanations, dumps, and backslash-heavy output.
- Keep the work focused on the current atomic change.
- Do not expand the scope without a reason.
10. Clipboard commands
- Never append | wl-copy unless explicitly requested for clipboard testing.
- Be especially careful when commands themselves produce clipboard output: a subsequent command can overwrite the previous result.
11. Preserve existing work
- Do not overwrite unrelated changes.
- Do not discard or reformat existing work unnecessarily.
- Preserve user modifications unless explicitly asked to change them.
12. Commit checkpoints
- After stable, tested changes, remind the user to check git status --short and commit a clean checkpoint.
- Do not forget commit discipline during longer refactors.
13. CVB-native tools first
- Follow the CVB syntax documented in the manual provided at the start of each session.
- Prefer CVB commands for repository search, source inspection, and context retrieval over Unix commands whenever practical.
- Consult the manual instead of guessing syntax.
- Use CVB's ability to fetch multiple files in one invocation when appropriate, especially when clipboard contents matter.
14. Ask rather than guess
- If a protocol rule is missing, forgotten, ambiguous, or if you're unsure how to proceed, ask the user for clarification or the relevant manual/context.
- Never invent syntax.
- Never silently assume.

# CVB Surgery: Manual for LLMs

`cvb surgery` applies code edits you specify directly to the user's files.
You write one envelope; the user copies it; CVB validates and applies it.
It is all-or-nothing: if anything doesn't match, nothing is written.

## Rules
1. Existing files only. You cannot create, delete, or rename files.
2. Ops: `replace`, `delete`, `insert`. Nothing else.
3. Every op needs exact preconditions. You must quote the existing text
   you expect at the target lines, character for character. If the file
   differs, the whole envelope is rejected. Never guess or paraphrase;
   if you haven't seen the current file contents, ask for them first
   (e.g. via `cvb pull`).
4. Paths are relative to the repo root. Any path containing a `.git`
   component is rejected.
5. Size limits: 3 MiB per envelope (split larger changes into several
   envelopes), 8 MiB per target file, 32 MiB total target files per envelope.
6. Line endings and the final-newline state of each file are preserved.
   Files with mixed line endings are rejected. Empty files are rejected.
7. Output the envelope as plain text in a single code block, with nothing
   inside it except the protocol. Put explanations outside the block.

## Envelope format (draft syntax)

<<<CVB-SURGERY v1>>>
@@ path/to/file.rs
replace 12-14
expect:
<exact existing lines 12-14>
with:
<new lines>
delete 30-31
expect:
<exact existing lines 30-31>
insert after 40
before-anchor:
<exact line(s) immediately above the insertion point>
after-anchor:
<exact line(s) immediately below the insertion point>
text:
<lines to insert>
<<<END-CVB-SURGERY>>>


- Line numbers are 1-based and refer to the file as the user currently has it.
  All ops in a file are addressed against the original file, not against
  the result of earlier ops. Do not renumber for your own earlier edits.
- Ops within a file must not overlap.
- `insert` needs verified anchors on both sides (multiple anchor lines
  allowed). CVB checks them at the stated location and never searches for a
  better one. Inserting at the very start or end of a file is not supported
  in v1; use a `replace` that includes the neighbouring line instead.
- Preserve indentation exactly in `expect` and `with` text.

## What happens after the user pastes
- If the envelope is malformed, truncated, or ambiguous, it is rejected.
- Otherwise CVB builds the full result in memory, shows a diff, and asks for
  confirmation. The bytes in the reviewed diff are exactly what is written.
- `--dry-run` shows the plan without modifying anything.

## If rejected
The user will paste the error back. Typical causes: `expect` text didn't
match (file changed or you misquoted it), line numbers off, anchor mismatch,
overlapping ops, size cap. Re-read the current file section, then send a
fresh, complete envelope. Do not send a partial correction; every envelope
stands alone.

## Good practice
- Keep edits small and focused; one logical change per envelope.
- Use the smallest `expect` range that is still unambiguous.
- Don't reformat untouched code.
- State briefly, outside the block, what the envelope changes and why.

# CVB Surgery: Manual for LLMs

`cvb surgery` applies code edits you specify directly to the user's existing files.

You propose an envelope; the user reviews it; CVB validates it and, after confirmation, applies it. The operation is atomic: if validation fails, no target files are modified.

## Rules

1. **Existing files only.** You may replace, delete, or insert text in existing files. You cannot create, delete, or rename files.
2. **Only three operations.** The supported operations are `replace`, `delete`, and `insert`. Do not invent other operation types.
3. **Exact preconditions are mandatory.** Every operation must quote the existing text needed to verify the edit, character for character. If you have not seen the current contents, ask the user to provide them—typically using `cvb pull`.
4. **Never guess the old text.** Copy the original lines from the supplied source. Do not paraphrase, retype from memory, or silently normalize indentation or whitespace.
5. **Paths are relative to the repository root.** Do not use absolute paths. Any path containing a `.git` component is rejected.
6. **Respect size limits.** Maximum envelope size: 3 MiB. Maximum target file size: 8 MiB per file. Maximum combined size of target files in one envelope: 32 MiB. Split larger changes into separate envelopes.
7. **Preserve file characteristics.** CVB preserves line endings and the final-newline state. Files with mixed line endings are rejected. Empty files are rejected.
8. **One complete envelope per message.** Output the protocol as plain text in a single fenced code block. Put explanations outside the block. Never send a partial correction.
9. **Use the supplied token.** Obtain a fresh token from `cvb surgery --spec`. Every structural line in an envelope must use the same token.
10. **Use a unique envelope ID.** The ID must not have been applied previously to the same project root. If an earlier envelope was already applied, use a new ID for the next envelope, even if the new proposal is a correction or follow-up.
11. **Every operation must make a real change.** Avoid replacing text with identical text or proposing a result that leaves the target file byte-for-byte unchanged. If the planner reports `E-NOOP`, revise the proposal so it produces a real change.
12. **Keep operations separate.** All operation locations refer to the original files, not the result of earlier operations in the same envelope. Operations within one file must not overlap.

## Envelope format: CSL v1

The envelope begins with `BEGIN` and ends with `END`. Each structural line includes the same token.

The following token is illustrative only. For a real proposal, use the fresh token printed by `cvb surgery --spec`.

```text
CVB::k7Q2xw9R::BEGIN v=1 id=demo-replace-01
CVB::k7Q2xw9R::FILE path=data/cvb.bash
CVB::k7Q2xw9R::OP replace lines=12-13
CVB::k7Q2xw9R::OLD n=2
umask 077
snapshot=$(mktemp)
CVB::k7Q2xw9R::NEW n=3
old_umask=$(umask)
umask 077
snapshot=$(mktemp)
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1
```

### Envelope structure

- `BEGIN v=1 id=...` starts an envelope. The ID must be 1–64 characters using ASCII letters, digits, `.`, `_`, or `-`.
- `FILE path=...` selects an existing file relative to the repository root.
- `OP ...` declares one operation and its original-file line range or insertion position.
- `OLD n=N` introduces the exact old text, with `N` payload lines.
- `NEW n=N` introduces the replacement or inserted text, with `N` payload lines.
- `ANCHOR n=N` introduces the exact anchor text required for an insertion.
- `ENDOP` terminates an operation.
- `END ops=N` terminates the envelope and declares the total number of operations.

Payload lines are the actual file text, not protocol metadata. Do not add indentation, quoting, or explanatory text to them. Count payload lines carefully. The count excludes the structural line introducing the payload.

## Supported operations

### 1. Replace existing lines

Syntax:

```text
CVB::<token>::OP replace lines=12-13
CVB::<token>::OLD n=2
<exact existing line 12>
<exact existing line 13>
CVB::<token>::NEW n=2
<replacement line 1>
<replacement line 2>
CVB::<token>::ENDOP
```

The `lines` range is inclusive and 1-based. The OLD payload must match the current contents at that exact location. NEW supplies the replacement.

For a single line, use a range such as `lines=12-12`. A bare `lines=12` is not valid syntax.

### 2. Delete existing lines

Syntax:

```text
CVB::<token>::OP delete lines=38-39
CVB::<token>::OLD n=2
<exact existing line 38>
<exact existing line 39>
CVB::<token>::ENDOP
```

The OLD payload must match the current contents at the specified location. A delete operation has no NEW payload.

For a single line, use a range such as `lines=38-38`.

### 3. Insert after existing lines

Syntax:

```text
CVB::<token>::OP insert after=19-20
CVB::<token>::ANCHOR n=2
<exact existing line 19>
<exact existing line 20>
CVB::<token>::NEW n=1
<new line to insert>
CVB::<token>::ENDOP
```

The `after` range identifies the existing anchor lines immediately preceding the insertion point. ANCHOR must match those lines exactly at the specified location. NEW contains the lines to insert.

For a single anchor line, use a range such as `after=20-20`.

### 4. Insert before existing lines

Syntax:

```text
CVB::<token>::OP insert before=21-22
CVB::<token>::ANCHOR n=2
<exact existing line 21>
<exact existing line 22>
CVB::<token>::NEW n=1
<new line to insert>
CVB::<token>::ENDOP
```

The `before` range identifies the existing anchor lines immediately following the insertion point. ANCHOR must match those lines exactly at the specified location. NEW contains the lines to insert.

For a single anchor line, use a range such as `before=21-21`.

In v1, insertion at the very beginning or end of a file is not supported. Use a replace operation that includes neighboring lines when necessary.

## Line numbers and operation ordering

- Line numbers are 1-based and refer to the file as it exists when the user provides it.
- Every operation in an envelope addresses the **original file**, not the result of earlier operations.
- Do not renumber later operations to account for your own proposed edits.
- Operations within the same file must not overlap.
- For insertions, use the original line numbers and verified anchor text at the stated location. CVB does not search elsewhere for a similar anchor.
- Preserve indentation, spaces, tabs, and all other characters in quoted existing text.
- `lines`, `after`, and `before` require an inclusive range in the form `START-END`. For a single line, repeat the same number on both sides of the hyphen.

## Before proposing an envelope

1. Inspect the relevant source using the material the user supplied or requested through `cvb pull`.
2. Identify the smallest change that accomplishes the task.
3. Verify the exact original lines and their line numbers.
4. Ensure the operations do not overlap and that all insertion anchors are present at the specified location.
5. Obtain a fresh token from `cvb surgery --spec`.
6. Choose a unique envelope ID that has not already been applied to the same project root.
7. Produce one complete, syntactically valid envelope. Ensure every structural line uses the same token and every payload count is correct.

## What happens when the user applies it

CVB parses and validates the envelope, checks the operation preconditions, builds the proposed result in memory, and renders a diff for review.

The user must confirm before changes are applied. The reviewed diff represents the bytes that will be written. With `--dry-run`, CVB renders the proposed plan without modifying target files.

If validation fails, CVB rejects the envelope without applying partial changes. Reusing an ID that was already applied to the same project root is rejected as a replay.

## If the envelope is rejected

The user may paste the error message back to you. Use the error code and details to identify the likely problem.

| Error code | Meaning / response |
|---|---|
| `E-SYNTAX` | The envelope's structure or operation syntax is invalid. Check the exact keywords, ranges, token consistency, and operation framing. |
| `E-PRECOND` | An operation's expected source text or anchor does not match. Request the current source and rebuild the envelope using the actual contents. |
| `E-COUNT` | A declared payload line count does not match the supplied payload. Recount OLD, NEW, and ANCHOR lines. |
| `E-LIMIT` | A size or other enforced limit has been exceeded. Reduce the envelope or split the work into smaller envelopes. |
| `E-REPLAY` | The envelope ID has already been applied to this project root. Choose a new ID for a new proposal. |
| `E-OVERLAP` | Operations overlap or conflict. Rework the operation ranges so they are compatible. |
| `E-NOOP` | The proposed result makes no actual change. Revise the proposal to produce the intended change. |
| `E-NOENV` | The input does not contain a recognized envelope. Check that the complete protocol was supplied. |

This table is a practical guide, not a substitute for the actual error message. Follow the code and details reported by CVB; if the cause remains unclear, ask the user for the complete error output.

### Recovery procedure

1. Read the error carefully.
2. Request the current relevant source if needed.
3. Re-evaluate the proposed change against the current file.
4. Generate a **fresh, complete envelope** with a fresh token and a unique ID.

Do not send only a corrected fragment. Every envelope must stand alone.

## Good practice

- Keep edits small and focused: one logical change per envelope.
- Use the smallest reliable OLD payload that establishes the required precondition.
- Never invent source lines or assume a file has remained unchanged.
- Do not reformat untouched code.
- Keep explanations outside the protocol code block.
- Prefer asking for more source over making an uncertain edit.

**Golden rule:** quote what exists, specify exactly what should change, and let CVB verify the result before anything is written.

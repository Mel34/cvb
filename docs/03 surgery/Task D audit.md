Bro, I’ve got all four parts. I’ll treat the pasted source as the evidence for Task D and distinguish verified coverage from tests that still need their bodies or implementation inspected. I won’t propose code changes as if they’re approved.

## Task D — Surgery applier coverage audit

The excerpts establish substantial existing coverage, bro. The main concern is not simply whether a test exists, but whether it exercises the precise failure boundary Claude specified.

### 1. Crash and recovery coverage

| Requirement                                       | Evidence in the supplied source                                                                                                                                 | Audit                                                                                                      |
| ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| Crash after the `prepared` journal is written     | Existing crash/recovery tests were identified in the earlier review; the excerpt here also shows journal-state transition tests.                                | Needs exact crash-test body confirmation                                                                   |
| Crash during each temporary-file write            | A crash-after-temp-write test was identified; `test_unknown_content_modified_before_rename_never_overwritten` also injects an external edit after a temp write. | Partial — external-edit coverage is not equivalent to crash coverage at every write                        |
| Crash after each individual rename                | `test_concurrency_file_modified_before_rename_rolls_back_earlier_renames`; `test_unknown_content_modified_after_rename_never_overwritten`                       | Partial — the supplied multi-file test exercises one specific rename boundary, not necessarily every index |
| Crash after all renames                           | An after-all-renames recovery test was identified in the earlier review.                                                                                        | Needs exact test-body confirmation                                                                         |
| Crash after read-back                             | An after-read-back test was identified in the earlier review.                                                                                                   | Needs exact test-body confirmation                                                                         |
| Crash during the `committed` journal transition   | `test_journal_atomic_state_transitions` verifies successful transitions to `Committed` and `RolledBack`.                                                        | Not established — a successful transition test does not simulate a crash during that transition            |
| Recovery does not overwrite unknown content       | `test_unknown_content_modified_after_rename_never_overwritten` checks both the immediate failure and subsequent recovery.                                       | Verified for this scenario                                                                                 |
| Already-committed or rolled-back journal handling | `test_pipeline_preconditions_allows_replayed_id_rolled_back`, `test_journal_replay_check`, and the earlier recovery-test inventory                              | Partial — replay behavior is covered; exact recovery no-op behavior needs confirmation from the test body  |

### 2. Concurrency and file races

| Requirement                                                                                   | Evidence in the supplied source                                           | Audit                                                                                                                                                                                |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Two simultaneous runs with the same ID: one applies and the other gets `E-LOCK` or `E-REPLAY` | `test_concurrency_simultaneous_runs_lock_or_replay`                       | Partial — it manually acquires a lock, verifies rejection, then applies twice sequentially. It does not establish a genuinely simultaneous two-run race                              |
| File changes between digest recheck and rename are caught                                     | `test_concurrency_file_modified_before_rename_rolls_back_earlier_renames` | Partial — the hook modifies the second file after the first rename; this checks a per-rename stale-content scenario, but the precise digest-check-to-rename race should be confirmed |
| Target modified before temp write is preserved                                                | `test_unknown_content_modified_before_temp_write_never_overwritten`       | Verified for the tested stale-file scenario                                                                                                                                          |
| Target modified before rename is preserved                                                    | `test_unknown_content_modified_before_rename_never_overwritten`           | Verified for the injected scenario                                                                                                                                                   |
| Target modified after rename is preserved during failure and recovery                         | `test_unknown_content_modified_after_rename_never_overwritten`            | Verified                                                                                                                                                                             |
| Multi-file rollback restores earlier changes while preserving the externally modified file    | `test_concurrency_file_modified_before_rename_rolls_back_earlier_renames` | Verified for the tested two-file case                                                                                                                                                |

The concurrency test deserves particular attention. Despite its name, it doesn't start two competing workers at the same time. That doesn't make the existing test useless: it verifies lock rejection and replay protection. It simply does not demonstrate race behavior under actual concurrent execution.

### 3. Tampered journals and project identity

| Requirement                                                               | Evidence in the supplied source                                                          | Audit                                                                                                                                   |
| ------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Relative paths are revalidated when a journal is read                     | `test_journal_rejects_tampered_path_traversal`; `test_journal_rejects_tampered_path_git` | Partial — both exercise `decode_journal`; confirm the read/recovery path also rejects tampered on-disk journals before touching targets |
| Wrong project root is refused                                             | `test_init_project_state_refuses_mismatched_root_path`                                   | Verified for project-state metadata, not by itself proof of the journal-recovery path                                                   |
| Wrong device or inode is refused                                          | `test_init_project_state_refuses_mismatched_dev_or_ino`                                  | Verified for project-state metadata, with the same distinction about journal recovery                                                   |
| Journal identity and path checks prevent writes into an unrelated project | Path traversal, `.git` path, and project identity tests provide relevant coverage        | Partial — the end-to-end refusal path needs confirmation                                                                                |

The existing journal tests are useful, but direct decoder tests and project-metadata tests don't automatically establish that recovery validates every condition before any filesystem mutation. That distinction is important for a safety audit.

### 4. Mutation-style checks

Claude specifically requested running these checks in a copy of the repository, never in your working tree.

| Safeguard to mutate         | Mutation run? | Result          |
| --------------------------- | ------------- | --------------- |
| Remove a count check        | No            | Not established |
| Remove a collision check    | No            | Not established |
| Remove a precondition check | No            | Not established |
| Remove an overlap rule      | No            | Not established |
| Remove a stale-file check   | No            | Not established |
| Remove rollback protection  | No            | Not established |

These aren't necessarily missing tests; they are unperformed audit checks. We mustn't claim a safeguard is adequately tested until the corresponding mutation demonstrably causes a test failure. The supplied snippets don't establish that result.

## Proposed next steps — no changes made

I'd separate genuine coverage gaps from audit work still needed.

1. Crash-boundary coverage: Verify the existing crash-injection tests first. If a specific boundary is missing—especially the `committed` transition—add a focused test for that boundary.
2. Concurrent same-ID runs: Add a real two-worker concurrency test only if the existing synchronization and locking implementation leaves that behavior untested.
3. Digest-check race: Inspect the exact check/rename sequence. If the implementation cannot close the race safely, this is a potential implementation issue, not merely a test gap.
4. Journal validation on recovery: Confirm that malformed paths and mismatched project identity are rejected before recovery changes any target file.
5. Mutation checks: Use an isolated repository copy, make one temporary mutation at a time, run the targeted tests, record the failing test, and discard the copy afterward.

One important qualification: the source excerpts establish several tests and their behavior, but they do not establish every requested crash boundary or any mutation-test result. I would not label the entire audit complete yet.

Bro, I recommend starting with the committed-transition crash boundary as the first candidate, but only after verifying the existing crash-hook implementation and tests. That keeps us aligned with the one-atomic-change-at-a-time protocol: first confirm whether coverage is genuinely missing, then show you the proposed test, and wait for your approval before editing anything.

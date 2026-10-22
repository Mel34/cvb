# CVB Phase 5 — Handoff for Gemini

## Current state

Repository: `~/Projects/cvb`  
Branch: `cvb-0.6.0`  
Latest commit: `65d7968 Add tokenized surgery protocol specification`  
The commit has been pushed.

Phase 4 is complete. Phase 5 Items 26 and 27 are implemented.

### Item 26 — Token generation
- Added `src/surgery/token.rs` and registered it in `src/surgery/mod.rs`.
- `generate_token()` reads `/dev/urandom` and uses unbiased Base62 rejection sampling.
- Tokens are 16 ASCII alphanumeric characters.

### Item 27 — `cvb surgery --spec`
- Added `spec_text(token: &str)` in `src/surgery/mod.rs`.
- `cvb surgery --spec` generates a fresh token, prints the protocol specification, and exits.
- Token generation happens before root resolution or input acquisition, so this path avoids clipboard/stdin reads, `/dev/tty`, locks, journals, and target-file side effects.
- The specification documents the protocol grammar, examples, safety/validation rules, limits, error codes, and workflow.
- Expanded `scripts/test-surgery-cli.py` to verify spec output and token consistency.
- Verification: 360 Rust tests passed, `cargo check` passed, and all 5 surgery CLI integration tests passed.
- Four previously known PTY harness failures remain explicitly deferred; do not reopen them as part of this work.

## Next: Phase 5, Item 28 — Thread-puller integration

We have not implemented Item 28 yet. We need to review the exact requirements from the implementation guide/protocol before proposing code.

Initial inspection of `src/pull.rs` shows:
- `run_capture()` starts around line 767.
- Explicit `file:` / `lines:` captures print `path:line: content` and return early around lines 872–907.
- Whole-file captures establish a line range around lines 845–870.
- Ordinary matched units are formatted around lines 1140–1173, with path and line spans in their headers.
- Cascade output is appended afterward around lines 1175–1179.

These are observations, not a decision about which pull modes Item 28 must change. Avoid assuming that every output path should be modified without checking the specification.

## Request to Gemini

Please review the Phase 5 requirements, especially protocol 9, step 5, Items 26–29, and focus on Item 28.

**Do not edit any files yet.** Report:
1. The exact expected behavior for Item 28.
2. Which pull output paths should be integrated, and which should remain unchanged.
3. How generated tokens and displayed file/line spans should be represented.
4. Any risks to existing `cvb pull` output compatibility, cascade output, or tests.
5. A minimal implementation plan and targeted tests.

Keep the change atomic. Inspect before editing, explain the proposed change first, then make a surgical patch with exact-anchor and match-count checks. Run focused tests before the full suite. Do not mix unrelated cleanup into this item.

The user wants a design review first and will decide when implementation begins.

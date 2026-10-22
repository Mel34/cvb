# CVB Master Protocol — Draft 0.2

Oct 14, 2026 · @Petar

## Settled design

This draft folds the agreed revisions into one design-only contract. It specifies behavior and message shapes, not Rust code, and every numeric value is a proposed starting point to be tuned during implementation.

| Area | Agreed design |
| --- | --- |
| Client identity | One client per wrapper/PTY, with a nesting guard |
| Session model | Independent liveness, focus and activity fields |
| Focus selection | Most recently focused client; reject if that client is unavailable |
| Heartbeats | Independent response path, or verified capture progress |
| Capture lifecycle | Received → Dispatched → Capturing → Validating → Complete or Failed |
| Request behavior | Blocking `cvb capture`, bounded deadline, nonzero exit on failure |
| Capture snapshot | Taken when the client receives the request; sequence or offset recorded in metadata |
| Integrity | Byte length, SHA-256, and a hard transcript-size limit |
| Storage | Private `$XDG_RUNTIME_DIR` directory (0700), files 0600, bounded count and total size |
| Control IPC | Unix domain socket, `SO_PEERCRED` uid check, versioned hello, length-prefixed JSON frames |
| Restarts | Fresh session generation, client reconnect with backoff |
| Browser API | Separate, read-only loopback HTTP interface |
| Clipboard | Master writes after commit, only while the request remains eligible |

## Session record

The master keeps one record per connected wrapper. Liveness, focus and activity are three independent groups of fields, so "capturing but no longer focused" and "focused but suspect" are both representable.

| Field | Type | Meaning |
| --- | --- | --- |
| `client_id` | opaque string | Unique per connection, assigned by the master |
| `generation` | integer | Increases each time the same terminal identity registers again |
| `terminal_id` | string | Wrapper-reported identity of the PTY, used only for display and nesting checks |
| `protocol_version` | integer | Version announced in the hello |
| `capabilities` | list of strings | Features the wrapper supports, such as chunked results |
| `liveness` | enum | `connecting`, `live`, `suspect`, `disconnected` |
| `last_seen` | monotonic timestamp | Last message of any kind received |
| `last_heartbeat_ack` | monotonic timestamp | Last valid reply to a ping |
| `focus_state` | enum | `unknown`, `in`, `out`, as last reported |
| `focus_seq` | integer | Master-assigned order of the latest focus-in from this client |
| `activity` | enum | `idle`, `capturing` |
| `active_request_id` | string or none | Request currently being served, if any |

A client is **eligible** for capture when `liveness` is `live` and `activity` is `idle`. Eligibility alone never selects a client; selection follows the focus rule below.

The master also holds two global values outside any single record:

- `last_focused_client`: the `client_id` of the most recent focus-in, kept even after that client disconnects.
- `request_seq`: a counter that increases with every capture request, used for clipboard ordering.

## Focus selection

The master selects the most recently focused client, and rejects the request if that client cannot serve it. It never falls back to the second most recent.

**Updating focus.** On a focus-in from client C, the master sets `C.focus_state = in`, increments a global focus counter into `C.focus_seq`, and sets `last_focused_client = C`. A focus-out only sets `C.focus_state = out`. It does not change `last_focused_client`, because a user sitting in the browser still means "the terminal I was just in".

**Selecting on a capture request:**

1. If `last_focused_client` is unset, fail with `no_focus_known`.
2. If that client's record is gone or its `liveness` is `disconnected` or `suspect`, fail with `focused_client_unavailable`.
3. If its `activity` is `capturing`, fail with `focused_client_busy`.
4. Otherwise select it and dispatch.

Focus never expires with time. Disconnection does not clear `last_focused_client`, so the master can report an accurate failure instead of silently capturing a different terminal. The next focus-in from any live client replaces the reference.

**Missed events.** A closing terminal or a crash can drop a focus-out, so the master treats focus as "latest focus-in wins" and relies on socket EOF and heartbeats, not on balanced in/out pairs.

## Capture lifecycle

Each capture request has its own `request_id`, independent of the client and transcript IDs. `Received` includes selecting the client, since selection is instant inside the master. A request reaches `Complete` only after the master has validated and committed the transcript.

&#91;embedded content: capture request lifecycle · 5 states, 1 failure state\]

The forward path has one exit at each stage. Every failure ends in `Failed` with a specific code, and the master never retries on another client.

| Failure code | Raised when |
| --- | --- |
| `no_focus_known` | No client has ever reported focus |
| `focused_client_unavailable` | The last-focused client is gone, `suspect` or `disconnected` |
| `focused_client_busy` | The last-focused client is already capturing |
| `client_no_ack` | No `capture_ack` arrives within the dispatch-ack timeout |
| `client_lost` | The serving client becomes `suspect` or `disconnected` mid-request |
| `deadline_exceeded` | The capture deadline passes before the request completes |
| `integrity_failed` | A chunk is missing, duplicated or out of order, or the length or digest does not match |
| `too_large` | The transcript exceeds the single-transcript limit |
| `master_unavailable` | Returned by `cvb capture` itself when no master is running; not a master state |

## Messages and framing

A stream socket keeps bytes in order but does not preserve message boundaries, so every message is a length-prefixed frame: a 4-byte big-endian length followed by that many bytes of UTF-8 JSON. Frames over the maximum size (proposed: 1 MiB for control messages) are rejected and the connection is closed. Every message carries a `type` and, where relevant, the `request_id` or `client_id` it belongs to.

On connect, the master checks the peer's uid with `SO_PEERCRED` and refuses any uid other than its own. The first message from a client must be a `hello`. A protocol version the master does not support gets an `error` with code `version_unsupported` and the connection is closed.

| Message | Direction | Key fields |
| --- | --- | --- |
| `hello` | client → master | `protocol_version`, `terminal_id`, `capabilities` |
| `welcome` | master → client | `client_id`, `generation`, `heartbeat_interval_ms` |
| `ping` / `pong` | master ↔ client | `nonce` |
| `focus` | client → master | `state` (`in` or `out`), client-side timestamp for diagnostics only |
| `capture_request` | master → client | `request_id`, `request_seq`, `deadline_ms` |
| `capture_ack` | client → master | `request_id`, `accepted`, `reason` if rejected |
| `capture_result` | client → master | `request_id`, `status`, `transcript_meta` (`byte_length`, `sha256`, `content_type`, `capture_offset`), then content or chunks |
| `chunk` | client → master | `request_id`, `index`, `total`, `data` |
| `capture_cancel` | master → client | `request_id` (sent only when the master abandons a request after timeout) |
| `error` | either | `code`, `message` |
| `bye` | either | optional `reason` |

The `cvb capture` command uses the same socket as a short-lived second kind of client. It sends `capture_command` and waits for a `capture_outcome` carrying either the `transcript_id` or a failure code, then exits 0 or nonzero accordingly.

Malformed JSON, an unknown required field, or a message that is invalid for the connection's current state produces an `error` and closes that connection. It never affects other sessions.

## Timeouts and restarts

All values are proposed starting points. Failure is final in v1: the user presses the hotkey again.

| Timer | Proposed value | On expiry |
| --- | --- | --- |
| Heartbeat interval | 2 s | Send `ping` |
| Heartbeat timeout | 6 s with no valid `pong` | `liveness` becomes `suspect` |
| Suspect grace | 6 s further | `liveness` becomes `disconnected`, connection closed |
| Dispatch ack | 2 s after `capture_request` | Request fails with `client_no_ack` |
| Capture deadline | 30 s from `Received` | Request fails with `deadline_exceeded`, master sends `capture_cancel` |
| Reconnect backoff (client) | 0.5 s doubling to 10 s, with jitter | Retry registration as a new generation |

**Heartbeats during large captures.** The client answers `pong` from a path independent of the capture work, or counts each `chunk` as liveness. A long capture must not push a healthy client into `suspect`.

**Suspect during a request.** If the serving client becomes `suspect` or `disconnected` mid-request, the request fails immediately with `client_lost`. It never becomes a silent success, and the master does not retry on another client.

**Master restart.** The wrapper sees EOF, keeps running the shell normally, and reconnects with backoff. It registers as a fresh generation with a fresh `hello`. The master starts with no sessions, no focus history and no in-flight requests. Transcripts persisted under the runtime directory are re-indexed on startup; any partially written file is discarded.

**Wrapper restart.** A new wrapper process is a new client with a new `client_id`. If it takes over the same `terminal_id`, the old record is marked `disconnected` as soon as the new `hello` arrives.

**Master absent.** With no master running, a wrapper stays usable as a plain shell wrapper. `cvb capture` fails fast with `master_unavailable`.

**Version mismatch after upgrade.** The master rejects an unsupported `protocol_version` with a clear error. The wrapper keeps working standalone and logs that it needs restarting.

## Transcripts: integrity, storage, retention

**Transport integrity versus capture correctness.** The byte length and SHA-256 prove that the transcript the master holds is exactly what the client sent, complete and uncorrupted. They do not prove that the capture itself was right: that the right terminal content was read, or that nothing scrolled away before the snapshot. `Validating` means the first claim only. Capture correctness rests on the existing capture engine and its tests.

**Commit rule.** A transcript becomes retrievable only after the master has verified length and digest over the fully reassembled payload. Chunked transfers need every `index` from 0 to `total - 1` exactly once; a missing, duplicated or out-of-order chunk fails the request with `integrity_failed`. A partial transcript is never stored as complete.

**Metadata kept per transcript:** `transcript_id`, originating `client_id` and `terminal_id`, `request_id` and `request_seq`, `byte_length`, `sha256`, `content_type`, `capture_offset`, and the received and committed timestamps.

**Storage layout.** Transcripts live under a private directory in `$XDG_RUNTIME_DIR`, created with mode 0700. Each transcript is one file (mode 0600) plus a small metadata file, written to a temporary name and renamed into place so a crash never leaves a half-written transcript that looks complete.

| Limit | Proposed value |
| --- | --- |
| Maximum single transcript | 8 MiB |
| Maximum retained transcripts | 50 |
| Maximum total retained bytes | 128 MiB |
| Eviction | Oldest committed first, when either cap is exceeded |

A transcript larger than the single-transcript limit fails with `too_large` and is not stored. These caps are starting values; the previously tested 295,000-byte payload is well inside them. Because the directory is on tmpfs, transcripts do not survive logout or reboot, which is intended. Terminal output often contains secrets, so anything on durable disk would be a separate, deliberate decision.

## Clipboard and browser delivery

**Clipboard order of operations.** The master commits the transcript first. Only then does it decide whether clipboard delivery is still eligible. The clipboard is a convenience copy, never the source of truth.

A completed request is eligible for clipboard delivery when both hold:

1. It completed before its own deadline.
2. Its `request_seq` is higher than that of any request already delivered to the clipboard, so a slower, older capture can never overwrite the clipboard after a newer request has won.

If either condition fails, the transcript stays retrievable by ID and the user gets a notification instead.

**Known limitation.** This rule stops stale CVB requests from overwriting newer CVB requests. It cannot reliably detect an intervening manual copy by the user, because Wayland offers no universally reliable, cheap way to read a clipboard generation. A capture that finishes just after the user copies something else can still replace it. The deadline keeps that window short, and the limitation should be documented, not hidden.

**Browser API boundary.** The userscript talks to a separate service, never to the control socket:

- Read-only: list recent transcript metadata and fetch one transcript by ID. No capture commands, no client registration, no administrative operations, no deletion in v1.
- Binds to `127.0.0.1` only.
- Requires a random token, stored in a user-only file and supplied by the userscript on every request.
- Validates the `Host` header (against DNS rebinding) and the `Origin`, and answers CORS only for the allowed origin.
- Implemented as a separate code path, or later as a separate read-only process, so the browser has no route to control operations even if the HTTP layer has a bug.

A throwaway "hello" endpoint should be tested with Tampermonkey (`GM_xmlhttpRequest` with a narrow `@connect`) before the API shape is finalized.

## Still open

These are deliberately left for review or small spikes before implementation:

- **Terminal tabs.** Does GNOME Console send focus-in and focus-out to each tab's PTY on tab switch, or only on window focus changes? The focus model assumes per-PTY events.
- **Multiplexers.** tmux forwards focus events only with `focus-events` enabled, and panes inside one window blur the one-session-one-focus assumption. Decide whether to support or document as unsupported.
- **What persists the clipboard.** Whether the compositor, a clipboard manager or a leftover helper process keeps the content after the owner exits, and whether native Wayland and XWayland apps behave the same.
- **Hotkey path.** A GNOME custom shortcut running `cvb capture`: measured latency to the master, and whether triggering it disturbs focus.
- **Heartbeat path in the wrapper.** Which mechanism (separate thread or task, or chunk progress as liveness) fits the existing capture code with the least change.
- **Numeric limits.** Every timeout and cap above is a starting value to tune once real captures run through the master.
- **Capture offset.** The exact meaning of `capture_offset` depends on how the existing capture engine tracks position; confirm during code inspection.
- **Master startup.** Starting with the graphical session and restarting on crash, including environment variables the clipboard write needs.

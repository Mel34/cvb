# CVB Master — Architecture Review

Oct 14, 2026 · @Petar

## Verdict

The core idea holds: a single authority for capture requests removes the race between sessions instead of patching it. Three assumptions need testing first, though, because each could break the plan on GNOME Wayland: who owns the hotkey, who writes the clipboard, and how focus is detected.

## What I'd change

**1. The master probably shouldn't listen for the hotkey at all.** Wayland deliberately blocks apps from grabbing global keys. A daemon-side listener means either evdev (needs input-group permissions and is a keylogger-shaped privilege) or the GlobalShortcuts portal (compositor support varies). The simpler route is a GNOME custom shortcut bound to a tiny command like `cvb capture`, which sends one message to the master's socket. The compositor owns the hotkey and the master owns the decision. That also makes step 5 almost trivial.

**2. "Only the master publishes to the clipboard" is the riskiest assumption.** As far as I know, GNOME's compositor has historically not supported the data-control protocols that let a windowless background process set the clipboard. Tools like `wl-copy` work around this, but they can be flaky from a daemon. Verify this on your exact GNOME version before building on it. If it's a problem, the fix keeps the race elimination intact: the master picks the winning session and tells that client to write the clipboard, for example via OSC 52 through its own terminal. The master authorizes, the winner writes. This determines the shape of step 6, so spike it first.

**3. Resolve the focus question early instead of deferring it.** Everything else depends on it, and the answer is fairly clear. The CVB wrapper already sits on the PTY, so it can enable terminal focus reporting (DECSET 1004) and receive focus-in and focus-out sequences. That works under Wayland because the terminal emulator sends them, not the compositor. Two caveats: some terminals and multiplexers handle it oddly, and you must decide what the hotkey does when no terminal is focused. If the user is in the browser, "most recently focused live session" is probably the right answer. Say so explicitly in the protocol.

## What I'd keep as is

- The implementation sequence: inspect, protocol, master, client, hotkey, clipboard, registry, browser. Only add a step 0 for the two spikes.
- Failing safely instead of picking a terminal arbitrarily.
- Capture-request IDs, with results validated against the outstanding request.
- Reusing the existing PTY capture engine rather than rewriting it.

## Smaller notes

- **Degrade gracefully.** If the master isn't running, a CVB client should still work standalone or fail with a clear message, so a crashed daemon doesn't break the terminal workflow. Socket activation via systemd, or auto-spawn on first client, helps.
- **Liveness.** Socket EOF already reports a dead client immediately. Heartbeats only catch hung-but-connected clients, so keep them but treat them as secondary.
- **Large transcripts.** Don't push big captures inline through the control channel. Use length-prefixed frames at minimum, or pass a file path or descriptor.
- **Retention.** Start in memory with a size cap. If you later persist, put it under `$XDG_RUNTIME_DIR` (tmpfs, mode 0600) before considering permanent disk. Terminal output often contains secrets, so durable storage deserves a deliberate decision.
- **Consumer API.** Native messaging needs a real browser extension, so with Tampermonkey the choice is effectively localhost HTTP. That makes security the real work:
  - Bind to 127.0.0.1 only.
  - Require a random token stored in a user-only file, pasted into the script once.
  - Validate the `Host` header to block DNS rebinding.
  - Use `GM_xmlhttpRequest` with a narrow `@connect` instead of opening up CORS.
  - Test with a throwaway "hello" endpoint before designing the API around it.

## Where to start

Run two small spikes before the protocol work, since their results shape it. Then proceed with code inspection as planned.

1. **Clipboard spike:** can a background process on your GNOME version set the clipboard reliably?
2. **Focus spike:** does focus reporting (DECSET 1004) work in your terminal and deliver focus-in and focus-out events to the wrapper?
3. **Code inspection:** map the existing hotkey-to-capture-to-clipboard path and find the smallest seam for master/client communication.

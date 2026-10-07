# CVB Design

## Terminal State and Command Capture

CVB uses one persistent terminal emulator as the authoritative terminal state.

The terminal emulator is responsible for both:

- representing what the user sees;
- retaining the terminal's scrollback/history for capture.

Command capture is derived from the terminal emulator's history rather than maintaining a separate command-output capture path.

Bash supplies command-boundary events only. It should not reconstruct or extract command output.

The intended flow is:

    command starts
          |
          v
        START
          |
          v
    +----------------------+
    |                      |
    |      Rust PTY        |
    |          |           |
    |          v           |
    |  terminal emulator   |
    |          |           |
    |          v           |
    |      scrollback      |
    |                      |
    +----------------------+
          |
          v
        END

At START, Rust records the relevant terminal position/state.

The command then runs normally through the PTY. All terminal output is processed through the same persistent terminal emulator that drives the visible terminal state.

At END, Rust derives the command's captured output from the terminal emulator's retained history.

## Why the Previous Approach Was Abandoned

The earlier command-capture implementation used the persistent `vt100` parser but attempted to extract command output from the current screen using a bounded region.

This worked for output that remained within the terminal's available screen area, but it could not reliably reconstruct output that had scrolled away.

For example:

    seq 100

was captured only partially, demonstrating that the approach was fundamentally limited by terminal height/scrollback handling.

We then considered manually stitching together successive `vt100` screen states to reconstruct output across scrolling.

That approach was ultimately rejected because it would require CVB to implement terminal-history behavior around a terminal emulator that does not natively provide the required scrollback semantics.

## Terminal Emulator Dependency

There are forks/variants of `vt100` that implement the scrollback functionality required by CVB.

The preferred solution is therefore to use a terminal-emulator implementation with native scrollback/history support rather than continuing to add hacks around upstream `vt100`.

This keeps the architecture centered on one authoritative terminal state and avoids maintaining parallel screen/capture representations.

## Bash Responsibilities

Bash is authoritative only for command boundaries.

Its responsibilities are:

- identify when a user command starts;
- send START to Rust;
- signal when the command has completed.

Bash should not:

- capture command output;
- inspect terminal screen contents;
- reconstruct scrollback;
- strip prompts or commands from captured output.

## Rust Responsibilities

Rust owns:

- the PTY;
- the persistent terminal emulator;
- terminal state and scrollback;
- command-boundary state;
- deriving captured output from terminal history;
- the visible terminal output path;
- clipboard/bridge behavior.

The visible terminal and command capture must ultimately derive from the same terminal-emulator state.

## Architectural Goal

The goal is to eliminate separate command-capture and screen-view code paths.

There should be one terminal stream:

    PTY bytes
        |
        v
    terminal emulator
       / \
      /   \
     v     v
  visible  capture

This makes terminal behavior consistent between what the user sees and what CVB captures, while allowing captured output to extend beyond the visible terminal height.

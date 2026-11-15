//! Paste-to-apply `apply_patch` support.
//!
//! The pieces are pure and independently testable; they are wired into the PTY
//! proxy in a later step.

pub(crate) mod paste;

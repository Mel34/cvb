//! Paste-to-apply `apply_patch` support.
//!
//! The pieces are independently testable; they are wired into the PTY proxy in a
//! later step.

pub(crate) mod guard;
pub(crate) mod parse;
pub(crate) mod paste;
pub(crate) mod run;

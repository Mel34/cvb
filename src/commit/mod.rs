//! The commit engine: applies a planned set of file changes all-or-nothing.
//!
//! A [`plan::Plan`] holds the original and proposed contents of each file. `apply`
//! checks them against the disk under a per-project lock, journals the intent,
//! writes through temporary files, renames, and rolls back on failure; `journal`
//! is the on-disk record and `lock` the per-project lock.

#![allow(dead_code)]

pub(crate) mod apply;
pub(crate) mod journal;
pub(crate) mod lock;
pub(crate) mod plan;

/// Largest file the engine will read or write (bytes).
pub(crate) const MAX_TARGET_BYTES: u64 = 8 * 1024 * 1024;
/// Largest total size of all files in one plan (bytes).
pub(crate) const MAX_AGGREGATE_BYTES: u64 = 32 * 1024 * 1024;

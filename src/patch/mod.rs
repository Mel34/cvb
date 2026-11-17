//! Paste-to-apply support for `apply_patch` patches and `git diff` output.
//!
//! A patch pasted at an idle prompt is recognized (`paste`, `input`), parsed and
//! checked (`parse`, `guard`), applied all-or-nothing (`run`) and its result shown
//! and copied (`deliver`). `pty.rs` `proxy()` is the only caller.

pub(crate) mod already;
pub(crate) mod deliver;
pub(crate) mod gitdiff;
pub(crate) mod guard;
pub(crate) mod input;
pub(crate) mod parse;
pub(crate) mod paste;
pub(crate) mod run;

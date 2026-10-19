//! Startup context initialization.
//!
//! When a CVB session starts, the LLM operating manual is placed on the
//! clipboard so the user can paste it into the LLM to begin. Rust owns this
//! step: it reads the manual and hands it to the clipboard directly, so no
//! shell command runs and no shell-history entry is created.
//!
//! The manual is an external, editable file read at runtime. It is deliberately
//! not embedded in the binary.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::clipboard::{self, ClipboardBuffer};

const MANUAL_FILE: &str = "manual.md";

const READY_MESSAGE: &str =
    "CVB: initialization context is ready on the clipboard. Paste it into your LLM to begin.";

const NOT_PREPARED_MESSAGE: &str = "CVB: warning: initialization context was not prepared";

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The whole manual is on the clipboard.
    Ready,
    /// The manual was larger than the copy limit, so the chunk pager already
    /// walked the user through pasting each part.
    SentInParts,
}

/// Prepares the initialization context for a new session.
///
/// Failure is reported but never fatal: the session still starts, and the user
/// is not told to paste anything that was not prepared.
pub(crate) fn initialize() {
    let Some(bash_source) = env::var_os("CVB_BASH_SOURCE") else {
        eprintln!("{NOT_PREPARED_MESSAGE}: CVB_BASH_SOURCE is not set");
        return;
    };

    let token = match env::var("CVB_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            eprintln!("{NOT_PREPARED_MESSAGE}: CVB_TOKEN is not set");
            return;
        }
    };

    match prepare_context_with(
        Path::new(&bash_source),
        &token,
        clipboard::needs_chunking,
        clipboard::process,
    ) {
        Ok(Outcome::Ready) => eprintln!("{READY_MESSAGE}"),
        Ok(Outcome::SentInParts) => {}
        Err(error) => eprintln!("{error}\n{NOT_PREPARED_MESSAGE}"),
    }
}

fn prepare_with(
    bash_source: &Path,
    will_chunk: impl FnOnce(&ClipboardBuffer) -> bool,
    process: impl FnOnce(&ClipboardBuffer) -> Result<(), String>,
) -> Result<Outcome, String> {
    let buffer = load_manual(bash_source)?;
    let chunked = will_chunk(&buffer);

    process(&buffer)?;

    Ok(if chunked {
        Outcome::SentInParts
    } else {
        Outcome::Ready
    })
}

fn prepare_context_with(
    bash_source: &Path,
    token: &str,
    will_chunk: impl FnOnce(&ClipboardBuffer) -> bool,
    process: impl FnOnce(&ClipboardBuffer) -> Result<(), String>,
) -> Result<Outcome, String> {
    let manual = load_manual(bash_source)?;
    let manual = std::str::from_utf8(manual.as_bytes())
        .map_err(|error| format!("CVB: manual is not valid UTF-8: {error}"))?;
    let context = build_context(manual, token)?;
    let buffer = ClipboardBuffer::new(context.into_bytes());
    let chunked = will_chunk(&buffer);

    process(&buffer)?;

    Ok(if chunked {
        Outcome::SentInParts
    } else {
        Outcome::Ready
    })
}

fn build_context(manual: &str, token: &str) -> Result<String, String> {
    build_context_with_limit(manual, token, clipboard::configured_copy_limit())
}

fn build_context_with_limit(
    manual: &str,
    token: &str,
    limit: usize,
) -> Result<String, String> {
    let workflow = concat!(
        "Use the CVB surgery envelope format from the start of this conversation.\n",
        "Inspect the relevant source before proposing edits. Make one atomic change per envelope.\n",
        "Submit exactly one envelope per paste to `cvb surgery`; use a unique id per envelope.\n",
        "Reuse the session token below for every envelope. Never print or disclose the token.\n",
        "If you need the token again, run `cvb token`.\n",
    );
    let spec = crate::surgery::spec_text(token);
    let context = format!(
        "{manual}\n\nWORKFLOW INSTRUCTIONS\n{workflow}\n{spec}\n\nCVB SESSION TOKEN: {token}"
    );
    let chars = context.chars().count();

    if chars > limit {
        return Err(format!(
            "CVB: initialization context is {chars} characters, exceeding the configured limit of {limit}"
        ));
    }

    Ok(context)
}

fn load_manual(bash_source: &Path) -> Result<ClipboardBuffer, String> {
    let path = locate_manual(bash_source)?;

    let contents = fs::read(&path)
        .map_err(|error| format!("CVB: unable to read manual {}: {error}", path.display()))?;

    if contents.is_empty() {
        return Err(format!("CVB: manual is empty: {}", path.display()));
    }

    Ok(ClipboardBuffer::new(contents))
}

/// Finds `manual.md` next to the loaded `cvb.bash` (installed layout), falling
/// back to the directory above it (development layout, `data/cvb.bash`).
fn locate_manual(bash_source: &Path) -> Result<PathBuf, String> {
    let dir = match bash_source.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };

    let beside = dir.join(MANUAL_FILE);
    if beside.is_file() {
        return Ok(beside);
    }

    let above = dir.join("..").join(MANUAL_FILE);
    if above.is_file() {
        return Ok(above);
    }

    Err(format!(
        "CVB: manual not found: {} (also looked in {})",
        beside.display(),
        above.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);

            let path = env::temp_dir().join(format!(
                "cvb-context-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Installed layout: cvb.bash and manual.md in one directory.
    fn installed_layout(root: &Path, manual: &str) -> PathBuf {
        fs::write(root.join(MANUAL_FILE), manual).unwrap();
        let bash = root.join("cvb.bash");
        fs::write(&bash, "# wrapper\n").unwrap();
        bash
    }

    /// Development layout: data/cvb.bash with manual.md one level up.
    fn development_layout(root: &Path, manual: &str) -> PathBuf {
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(root.join(MANUAL_FILE), manual).unwrap();
        let bash = root.join("data").join("cvb.bash");
        fs::write(&bash, "# wrapper\n").unwrap();
        bash
    }

    #[test]
    fn locates_manual_beside_the_wrapper() {
        let dir = TempDir::new("beside");
        let bash = installed_layout(dir.path(), "manual");

        assert_eq!(
            locate_manual(&bash).unwrap(),
            dir.path().join(MANUAL_FILE)
        );
    }

    #[test]
    fn falls_back_to_the_directory_above_the_wrapper() {
        let dir = TempDir::new("above");
        let bash = development_layout(dir.path(), "manual");

        let found = locate_manual(&bash).unwrap();

        assert_eq!(fs::read_to_string(found).unwrap(), "manual");
    }

    #[test]
    fn manual_beside_the_wrapper_wins_over_the_one_above() {
        let dir = TempDir::new("prefer");
        let bash = development_layout(dir.path(), "above");
        fs::write(dir.path().join("data").join(MANUAL_FILE), "beside").unwrap();

        let found = locate_manual(&bash).unwrap();

        assert_eq!(fs::read_to_string(found).unwrap(), "beside");
    }

    #[test]
    fn missing_manual_is_a_clear_error_and_nothing_is_copied() {
        let dir = TempDir::new("missing");
        let bash = dir.path().join("cvb.bash");
        fs::write(&bash, "# wrapper\n").unwrap();
        let copied = Cell::new(false);

        let error = prepare_with(
            &bash,
            |_| false,
            |_| {
                copied.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("manual not found"), "{error}");
        assert!(!copied.get(), "clipboard must not be touched");
    }

    #[test]
    fn empty_manual_is_an_error_and_nothing_is_copied() {
        let dir = TempDir::new("empty");
        let bash = installed_layout(dir.path(), "");
        let copied = Cell::new(false);

        let error = prepare_with(
            &bash,
            |_| false,
            |_| {
                copied.set(true);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.contains("empty"), "{error}");
        assert!(!copied.get(), "clipboard must not be touched");
    }

    #[test]
    fn copies_the_manual_bytes_exactly() {
        let dir = TempDir::new("exact");
        let bash = installed_layout(dir.path(), "# Manual\n\nüñí\n");
        let seen = RefCell::new(Vec::new());

        let outcome = prepare_with(
            &bash,
            |_| false,
            |buffer| {
                seen.borrow_mut().extend_from_slice(buffer.as_bytes());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, Outcome::Ready);
        assert_eq!(seen.into_inner(), "# Manual\n\nüñí\n".as_bytes());
    }

    #[test]
    fn launch_context_contains_workflow_spec_and_session_token() {
        let manual = "MANUAL_SENTINEL";
        let token = "ContextToken1234";
        let context = build_context(manual, token).expect("build launch context");
        let expected_token_line = format!("CVB SESSION TOKEN: {token}");

        assert!(
            context.contains(manual)
                && context.contains(
                    "Use the CVB surgery envelope format from the start of this conversation."
                )
                && context.contains("CVB Surgery Language (CSL), protocol version 1")
                && context.contains(&format!("CVB::{token}::BEGIN"))
                && context.lines().last() == Some(expected_token_line.as_str()),
            "launch context is missing required content; actual context: {context:?}"
        );
    }

    #[test]
    fn launch_context_respects_exact_character_limit() {
        let manual = "MANUAL_SENTINEL";
        let token = "ContextToken1234";
        let context = build_context_with_limit(manual, token, usize::MAX)
            .expect("build context with generous limit");
        let exact_limit = context.chars().count();

        assert!(
            build_context_with_limit(manual, token, exact_limit).is_ok(),
            "context should fit when the limit equals its character count"
        );

        let error = build_context_with_limit(manual, token, exact_limit - 1)
            .expect_err("context should exceed a limit one character too small");
        assert!(
            error.contains(&format!("configured limit of {}", exact_limit - 1)),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn manual_is_read_at_run_time_not_embedded() {
        let dir = TempDir::new("runtime");
        let bash = installed_layout(dir.path(), "first edition");

        let capture = |expected: &str| {
            let seen = RefCell::new(Vec::new());
            prepare_with(
                &bash,
                |_| false,
                |buffer| {
                    seen.borrow_mut().extend_from_slice(buffer.as_bytes());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(String::from_utf8(seen.into_inner()).unwrap(), expected);
        };

        capture("first edition");
        fs::write(dir.path().join(MANUAL_FILE), "second edition").unwrap();
        capture("second edition");
    }

    #[test]
    fn clipboard_failure_is_propagated() {
        let dir = TempDir::new("clipfail");
        let bash = installed_layout(dir.path(), "manual");

        let error = prepare_with(
            &bash,
            |_| false,
            |_| Err("CVB: unable to start wl-copy: not found".to_string()),
        )
        .unwrap_err();

        assert!(error.contains("wl-copy"), "{error}");
    }

    #[test]
    fn oversized_manual_reports_that_it_was_sent_in_parts() {
        let dir = TempDir::new("parts");
        let bash = installed_layout(dir.path(), "manual");

        let outcome = prepare_with(&bash, |_| true, |_| Ok(())).unwrap();

        assert_eq!(outcome, Outcome::SentInParts);
    }

    #[test]
    fn paste_instruction_is_only_in_the_success_message() {
        assert!(READY_MESSAGE.contains("Paste it into your LLM"));
        assert!(!NOT_PREPARED_MESSAGE.to_lowercase().contains("paste"));
    }

    #[test]
    fn bash_wrapper_no_longer_initializes_context() {
        // Invoking `cvb manual.md` from Bash is what left a history entry.
        let wrapper = include_str!("../data/cvb.bash");

        assert!(!wrapper.contains("manual"), "wrapper still references the manual");

        for line in wrapper.lines().filter(|line| line.contains("command cvb")) {
            assert!(
                line.trim_end().ends_with("command cvb \"$@\""),
                "wrapper must only forward the user's own arguments: {line}"
            );
        }
    }
}

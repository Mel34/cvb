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

pub(crate) const CONTEXT_MAX_CHARS: usize = 20_000;
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
    /// The user quit the chunk pager before completing the sequence.
    Cancelled,
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
        clipboard::process_with_outcome,
    ) {
        Ok(Outcome::Ready) => eprintln!("{READY_MESSAGE}"),
        Ok(Outcome::SentInParts | Outcome::Cancelled) => {}
        Err(error) => eprintln!("{error}\n{NOT_PREPARED_MESSAGE}"),
    }
}


fn prepare_context_with(
    bash_source: &Path,
    token: &str,
    will_chunk: impl FnOnce(&ClipboardBuffer) -> bool,
    process: impl FnOnce(
        &ClipboardBuffer,
    ) -> Result<clipboard::ChunkSequenceOutcome, String>,
) -> Result<Outcome, String> {
    let manual = load_manual_text(Some(bash_source))?;
    let context = assemble_context(&manual, token)?;
    let buffer = ClipboardBuffer::new(context.into_bytes());
    let chunked = will_chunk(&buffer);

    match process(&buffer)? {
        clipboard::ChunkSequenceOutcome::Completed if chunked => Ok(Outcome::SentInParts),
        clipboard::ChunkSequenceOutcome::Completed => Ok(Outcome::Ready),
        clipboard::ChunkSequenceOutcome::Cancelled => Ok(Outcome::Cancelled),
    }
}

pub(crate) fn assemble_context(manual: &str, token: &str) -> Result<String, String> {
    assemble_context_with_limit(manual, token, CONTEXT_MAX_CHARS)
}

pub(crate) fn assemble_context_with_limit(
    manual: &str,
    token: &str,
    limit: usize,
) -> Result<String, String> {
    let context = manual.replace("{TOKEN}", token);
    let chars = context.chars().count();

    if chars > limit {
        return Err(format!(
            "CVB: initialization context is {chars} characters, exceeding the configured limit of {limit}"
        ));
    }

    Ok(context)
}

pub(crate) fn load_manual_text(bash_source: Option<&Path>) -> Result<String, String> {
    #[cfg(test)]
    if bash_source.is_none() {
        let test_manual = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANUAL_FILE);
        if test_manual.is_file() {
            let contents = fs::read_to_string(&test_manual)
                .map_err(|error| format!("CVB: unable to read manual {}: {error}", test_manual.display()))?;
            if contents.is_empty() {
                return Err(format!("CVB: manual is empty: {}", test_manual.display()));
            }
            return Ok(contents);
        }
    }

    let path = match bash_source {
        Some(source) => locate_manual(source)?,
        None => {
            let env_source = env::var_os("CVB_BASH_SOURCE").map(PathBuf::from);
            match env_source {
                Some(ref s) => locate_manual(s)?,
                None => locate_manual(Path::new("."))?,
            }
        }
    };

    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("CVB: unable to read manual {}: {error}", path.display()))?;

    if contents.is_empty() {
        return Err(format!("CVB: manual is empty: {}", path.display()));
    }

    Ok(contents)
}

/// The single source of truth for launch context and `cvb context`.
pub(crate) fn get_context_initialization(token: &str) -> Result<String, String> {
    let manual = load_manual_text(None)?;
    assemble_context(&manual, token)
}

/// Extracts Section 8 (the surgery specification) from `manual.md` with `{TOKEN}` replaced.
pub(crate) fn get_spec_text(token: &str) -> Result<String, String> {
    let manual = load_manual_text(None)?;
    const SPEC_HEADER: &str = "CVB Surgery Language (CSL), protocol version 1";
    let spec_start = manual.find(SPEC_HEADER).ok_or_else(|| {
        "CVB: surgery specification header not found in manual.md".to_string()
    })?;
    let raw = manual[spec_start..].replace("{TOKEN}", token);
    let unindented = raw
        .lines()
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Ok(unindented)
}

/// Prints the surgery specification (Section 8) populated with the session token.
pub(crate) fn print_spec() -> ! {
    let token = match std::env::var("CVB_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            eprintln!("CVB: E-NOSESSION: no active surgery session (CVB_TOKEN not set).");
            std::process::exit(2);
        }
    };
    match get_spec_text(&token) {
        Ok(text) => {
            print!("{text}");
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
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

        let error = prepare_context_with(
            &bash,
            "ContextToken1234",
            |_| false,
            |_| {
                copied.set(true);
                Ok(clipboard::ChunkSequenceOutcome::Completed)
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

        let error = prepare_context_with(
            &bash,
            "ContextToken1234",
            |_| false,
            |_| {
                copied.set(true);
                Ok(clipboard::ChunkSequenceOutcome::Completed)
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

        let outcome = prepare_context_with(
            &bash,
            "ContextToken1234",
            |_| false,
            |buffer| {
                seen.borrow_mut().extend_from_slice(buffer.as_bytes());
                Ok(clipboard::ChunkSequenceOutcome::Completed)
            },
        )
        .unwrap();

        assert_eq!(outcome, Outcome::Ready);
        assert_eq!(seen.into_inner(), "# Manual\n\nüñí\n".as_bytes());
    }

    #[test]
    fn launch_context_contains_workflow_spec_and_session_token() {
        let manual = "MANUAL_SENTINEL {TOKEN} END";
        let token = "ContextToken1234";
        let context = assemble_context(manual, token).expect("build launch context");

        assert!(
            context.contains("MANUAL_SENTINEL ContextToken1234 END"),
            "launch context is missing required content; actual context: {context:?}"
        );
    }

    #[test]
    fn launch_context_respects_exact_character_limit() {
        let manual = "MANUAL_SENTINEL {TOKEN}";
        let token = "ContextToken1234";
        let context = assemble_context_with_limit(manual, token, usize::MAX)
            .expect("build context with generous limit");
        let exact_limit = context.chars().count();

        assert!(
            assemble_context_with_limit(manual, token, exact_limit).is_ok(),
            "context should fit when the limit equals its character count"
        );

        let error = assemble_context_with_limit(manual, token, exact_limit - 1)
            .expect_err("context should exceed a limit one character too small");
        assert!(
            error.contains(&format!("configured limit of {}", exact_limit - 1)),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn context_initialization_within_20000_char_budget_and_token_placement() {
        let manual_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANUAL_FILE);
        let manual = fs::read_to_string(&manual_path).expect("read manual.md");
        let test_token = "TestToken1234567";
        let context = assemble_context(&manual, test_token).expect("assemble context within budget");

        assert!(
            context.chars().count() <= CONTEXT_MAX_CHARS,
            "Context initialization exceeds 20,000 chars: {} chars",
            context.chars().count()
        );

        let expected_token_line = format!("CVB SESSION TOKEN: {test_token}");
        assert_eq!(context.lines().last(), Some(expected_token_line.as_str()));
        assert!(context.contains(&format!("CVB::{test_token}::BEGIN")));

        // Assert token is not in the un-interpolated template
        assert!(!manual.contains(test_token));
    }

    #[test]
    fn context_assembly_is_byte_identical_between_calls() {
        let manual_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANUAL_FILE);
        let manual = fs::read_to_string(&manual_path).expect("read manual.md");
        let token = "TestToken1234567";
        let first = assemble_context(&manual, token).unwrap();
        let second = assemble_context(&manual, token).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn manual_is_read_at_run_time_not_embedded() {
        let dir = TempDir::new("runtime");
        let bash = installed_layout(dir.path(), "first edition");

        let capture = |expected: &str| {
            let seen = RefCell::new(Vec::new());
            prepare_context_with(
                &bash,
                "ContextToken1234",
                |_| false,
                |buffer| {
                    seen.borrow_mut().extend_from_slice(buffer.as_bytes());
                    Ok(clipboard::ChunkSequenceOutcome::Completed)
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

        let error = prepare_context_with(
            &bash,
            "ContextToken1234",
            |_| false,
            |_| Err("CVB: unable to start wl-copy: not found".to_string()),
        )
        .unwrap_err();

        assert!(error.contains("wl-copy"), "{error}");
    }

    #[test]
    fn context_preparation_preserves_pager_cancellation() {
        let dir = TempDir::new("cancelled");
        let bash = installed_layout(dir.path(), "manual {TOKEN}");
        let outcome = prepare_context_with(
            &bash,
            "ContextToken1234",
            |_| true,
            |_| Ok(clipboard::ChunkSequenceOutcome::Cancelled),
        );

        assert_eq!(outcome, Ok(Outcome::Cancelled));
    }

    #[test]
    fn oversized_manual_reports_that_it_was_sent_in_parts() {
        let dir = TempDir::new("parts");
        let bash = installed_layout(dir.path(), "manual");

        let outcome = prepare_context_with(&bash, "ContextToken1234", |_| true, |_| Ok(clipboard::ChunkSequenceOutcome::Completed)).unwrap();

        assert_eq!(outcome, Outcome::SentInParts);
    }

    #[test]
    fn paste_instruction_is_only_in_the_success_message() {
        assert!(READY_MESSAGE.contains("Paste it into your LLM"));
        assert!(!NOT_PREPARED_MESSAGE.to_lowercase().contains("paste"));
    }

    #[test]
    fn bash_wrapper_no_longer_initializes_context() {
        let wrapper = include_str!("../data/cvb.bash");

        assert!(!wrapper.contains("manual"), "wrapper still references the manual");

        for line in wrapper.lines().filter(|line| line.contains("command cvb")) {
            assert!(
                line.trim_end().ends_with("command cvb \"$@\""),
                "wrapper must only forward the user's own arguments: {line}"
            );
        }
    }

    #[test]
    fn spec_text_contains_token_and_unindented_control_lines() {
        let token = "TestToken1234567";
        let spec = get_spec_text(token).expect("get spec text");

        assert!(spec.starts_with("CVB Surgery Language (CSL), protocol version 1"));
        assert!(spec.contains(&format!("CVB::{token}::BEGIN")));
        assert!(!spec.lines().any(|l| l.starts_with("  CVB::")));
        let expected_token_line = format!("CVB SESSION TOKEN: {token}");
        assert_eq!(spec.lines().last(), Some(expected_token_line.as_str()));
    }

    #[test]
    fn context_text_equals_launch_text_byte_for_byte() {
        let manual_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MANUAL_FILE);
        let manual = fs::read_to_string(&manual_path).expect("read manual.md");
        let token = "TestToken1234567";
        let launch_text = assemble_context(&manual, token).unwrap();
        let context_cmd_text = get_context_initialization(token).unwrap();
        assert_eq!(launch_text, context_cmd_text);
    }
}

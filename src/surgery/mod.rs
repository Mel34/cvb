#![allow(dead_code)]

use std::path::{Component, Path};

pub(crate) mod apply;
pub(crate) mod confirm;
pub(crate) mod journal;
pub(crate) mod lock;
pub(crate) mod load;
pub(crate) mod parse;
pub(crate) mod plan;
pub(crate) mod render;
pub(crate) mod diff_parse;
pub(crate) mod keys;
pub(crate) mod mode;
pub(crate) mod pager;
pub(crate) mod token;
#[allow(unused_imports)]
pub(crate) use apply::*;
#[allow(unused_imports)]
pub(crate) use confirm::*;
#[allow(unused_imports)]
pub(crate) use journal::*;
#[allow(unused_imports)]
pub(crate) use lock::*;
pub(crate) use load::*;
pub(crate) use parse::*;
pub(crate) use plan::*;
#[allow(unused_imports)]
pub(crate) use render::*;
#[allow(unused_imports)]
pub(crate) use keys::*;
#[allow(unused_imports)]
pub(crate) use token::*;

pub(crate) fn check_input_size(raw_len: usize) -> Result<(), String> {
    if raw_len > MAX_INPUT_BYTES {
        let mut msg = format!(
            "E-LIMIT: input size ({raw_len} bytes) exceeds the 3 MiB cap ({MAX_INPUT_BYTES} bytes)."
        );
        if raw_len >= CLIPBOARD_TRUNCATION_BYTES.saturating_sub(100_000) {
            msg.push_str(
                " The received size is near 4 MB; the clipboard may have truncated it. Split the change into several envelopes."
            );
        } else {
            msg.push_str(" Split the change into several envelopes.");
        }
        return Err(msg);
    }
    Ok(())
}

pub(crate) fn read_input(stdin: bool) -> Result<Vec<u8>, (String, i32)> {
    let bytes = if stdin {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin()
            .take(MAX_INPUT_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| (format!("CVB: failed to read stdin: {e}"), 2))?;
        buf
    } else {
        crate::clipboard::read_from_clipboard()
            .map_err(|e| (e, 2))?
    };

    check_input_size(bytes.len()).map_err(|e| (e, 2))?;
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Envelope {
    pub token: String,
    pub id: String,
    pub files: Vec<FileSection>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileSection {
    pub path: String,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operation {
    Replace {
        start: usize,
        end: usize,
        old: Vec<String>,
        new: Vec<String>,
    },
    Delete {
        start: usize,
        end: usize,
        old: Vec<String>,
    },
    Insert {
        position: InsertPosition,
        start: usize,
        end: usize,
        anchor: Vec<String>,
        new: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsertPosition {
    Before,
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    Syntax,
    Range,
    Count,
    Version,
    Truncated,
    NoEnvelope,
    Collision,
    Stray,
    MultipleEnvelopes,
    Limit,
    InvalidBytes,
    DuplicateFile,
    InvalidPath,
}

impl ParseError {
    pub(crate) fn protocol_code(&self) -> &'static str {
        match self {
            ParseError::Syntax => "E-SYNTAX",
            ParseError::Range => "E-RANGE",
            ParseError::Count => "E-COUNT",
            ParseError::Version => "E-VERSION",
            ParseError::Truncated => "E-TRUNC",
            ParseError::NoEnvelope => "E-NOENV",
            ParseError::Collision => "E-COLLISION",
            ParseError::Stray => "E-STRAY",
            ParseError::MultipleEnvelopes => "E-MULTI",
            ParseError::Limit => "E-LIMIT",
            ParseError::InvalidBytes => "E-BYTES",
            ParseError::DuplicateFile => "E-DUPFILE",
            ParseError::InvalidPath => "E-PATH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseFailure {
    pub kind: ParseError,
    pub line: usize,
    pub detail: Option<String>,
}

impl PartialEq<ParseError> for ParseFailure {
    fn eq(&self, other: &ParseError) -> bool {
        self.kind == *other
    }
}

impl PartialEq<ParseFailure> for ParseError {
    fn eq(&self, other: &ParseFailure) -> bool {
        *self == other.kind
    }
}

impl std::fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.kind.protocol_code();
        let line_prefix = if self.line > 0 {
            format!("line {}: ", self.line)
        } else {
            String::new()
        };
        if let Some(detail) = &self.detail {
            write!(f, "CVB: {code}: {line_prefix}{detail}")
        } else {
            write!(f, "CVB: {code}: {line_prefix}parse error")
        }
    }
}

impl std::error::Error for ParseFailure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrefixSide {
    ExpectedEnds,
    FoundEnds,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanDetail {
    Precondition {
        line: usize,
        column: usize,
        byte_offset: usize,
        expected: String,
        found: String,
        prefix_ended: Option<PrefixSide>,
        only_trailing_whitespace: bool,
        candidates: Vec<usize>,
        context: Vec<(usize, String)>,
    },
    EndOfFile {
        suggestion: String,
    },
    Overlap {
        other_op_index: usize,
    },
    Range {
        file_lines: usize,
    },
    AmbiguousMatches {
        hint: usize,
        candidates: Vec<usize>,
    },
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanFailure {
    pub kind: PlanError,
    pub path: String,
    pub op_index: usize,
    pub detail: Option<PlanDetail>,
}

impl PartialEq<PlanError> for PlanFailure {
    fn eq(&self, other: &PlanError) -> bool {
        self.kind == *other
    }
}

impl PartialEq<PlanFailure> for PlanError {
    fn eq(&self, other: &PlanFailure) -> bool {
        *self == other.kind
    }
}

fn format_visible_line(s: &str) -> String {
    let truncated = if s.chars().count() > 200 {
        let mut t: String = s.chars().take(200).collect();
        t.push_str("...");
        t
    } else {
        s.to_string()
    };

    let non_trailing_len = truncated.trim_end_matches([' ', '\t']).len();
    let (prefix, trailing) = truncated.split_at(non_trailing_len);

    let mut result = String::with_capacity(truncated.len());
    for c in prefix.chars() {
        match c {
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            '\n' => result.push_str("\\n"),
            '\0' => result.push_str("\\0"),
            c if (c as u32) < 32 || c as u32 == 127 => {
                result.push_str(&format!("\\x{:02x}", c as u8));
            }
            c => result.push(c),
        }
    }
    for c in trailing.chars() {
        match c {
            ' ' => result.push_str("\\x20"),
            '\t' => result.push_str("\\t"),
            '\r' => result.push_str("\\r"),
            '\n' => result.push_str("\\n"),
            c => result.push(c),
        }
    }
    result
}

impl std::fmt::Display for PlanFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(PlanDetail::Precondition {
                line,
                column,
                byte_offset,
                expected,
                found,
                prefix_ended,
                only_trailing_whitespace,
                candidates,
                context,
            }) => {
                if expected.contains('\n') {
                    let mut expected_escaped = false;
                    let mut expected_lines = expected.split('\n');
                    let first_expected = expected_lines.next().unwrap_or("");
                    let escaped_first_expected =
                        crate::surgery::render::escape_line_content(first_expected);
                    expected_escaped |= escaped_first_expected != first_expected;
                    write!(
                        f,
                        "CVB: E-PRECOND: {}: operation {}: precondition mismatch at line {}, column {} (byte offset {}):\n  expected: \"{}\"\n     found: \"{}\"",
                        self.path,
                        self.op_index,
                        line,
                        column,
                        byte_offset,
                        escaped_first_expected,
                        format_visible_line(found)
                    )?;
                    for expected_line in expected_lines {
                        let escaped =
                            crate::surgery::render::escape_line_content(expected_line);
                        expected_escaped |= escaped != expected_line;
                        write!(f, "\n            \"{}\"", escaped)?;
                    }
                    if expected_escaped {
                        write!(f, "\n  note: expected text contains escaped characters")?;
                    }
                } else {
                    write!(
                        f,
                        "CVB: E-PRECOND: {}: operation {}: precondition mismatch at line {}, column {} (byte offset {}):\n  expected: \"{}\"\n     found: \"{}\"",
                        self.path,
                        self.op_index,
                        line,
                        column,
                        byte_offset,
                        format_visible_line(expected),
                        format_visible_line(found)
                    )?;
                }
                if let Some(side) = prefix_ended {
                    match side {
                        PrefixSide::ExpectedEnds => write!(f, "\n  note: expected ends here")?,
                        PrefixSide::FoundEnds => write!(f, "\n  note: found ends here")?,
                    }
                }
                if *only_trailing_whitespace {
                    write!(f, "\n  note: differs only in trailing whitespace")?;
                }
                if !candidates.is_empty() {
                    write!(f, "\n  exact expected-block matches elsewhere:")?;
                    for candidate in candidates {
                        write!(f, " {candidate}")?;
                    }
                }
                write!(f, "\n  actual context:")?;
                for (context_line, content) in context {
                    write!(
                        f,
                        "\n    {context_line}: \"{}\"",
                        format_visible_line(content)
                    )?;
                }
                Ok(())
            }
            Some(PlanDetail::EndOfFile { suggestion }) => {
                write!(
                    f,
                    "CVB: E-EOF: {}: operation {}: {suggestion}",
                    self.path, self.op_index
                )
            }
            Some(PlanDetail::Overlap { other_op_index }) => {
                write!(
                    f,
                    "CVB: E-OVERLAP: {}: operation {} overlaps with operation {}",
                    self.path, self.op_index, other_op_index
                )
            }
            Some(PlanDetail::Range { file_lines }) => {
                write!(
                    f,
                    "CVB: E-RANGE: {}: operation {}: line range out of bounds (file has {file_lines} lines)",
                    self.path, self.op_index
                )
            }
            Some(PlanDetail::AmbiguousMatches { hint, candidates }) => {
                write!(
                    f,
                    "CVB: E-AMBIGUOUS: {}: operation {}: expected block at hinted line {} matches multiple locations:",
                    self.path, self.op_index, hint
                )?;
                for candidate in candidates {
                    write!(f, " {candidate}")?;
                }
                Ok(())
            }
            Some(PlanDetail::Other(msg)) => {
                if self.op_index > 0 {
                    write!(
                        f,
                        "CVB: {}: {}: operation {}: {msg}",
                        self.kind.protocol_code(),
                        self.path,
                        self.op_index
                    )
                } else {
                    write!(
                        f,
                        "CVB: {}: {}: {msg}",
                        self.kind.protocol_code(),
                        self.path
                    )
                }
            }
            None => {
                if self.op_index > 0 {
                    write!(
                        f,
                        "CVB: {}: {}: operation {}",
                        self.kind.protocol_code(),
                        self.path,
                        self.op_index
                    )
                } else {
                    write!(
                        f,
                        "CVB: {}: {}",
                        self.kind.protocol_code(),
                        self.path
                    )
                }
            }
        }
    }
}

impl std::error::Error for PlanFailure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadError {
    UnsafePath(String),
    DuplicatePath(String),
    Inspect { path: String, message: String },
    SymlinkTarget(String),
    NotRegularFile(String),
    FileTooLarge { path: String, size: u64, limit: u64 },
    AggregateTooLarge { size: u64, limit: u64 },
    Read { path: String, message: String },
    InvalidUtf8(String),
    NulByte(String),
    Bom(String),
    LoneCr(String),
    MixedLineEndings(String),
}

impl LoadError {
    pub(crate) fn protocol_code(&self) -> &'static str {
        match self {
            LoadError::UnsafePath(_) => "E-PATH",
            LoadError::DuplicatePath(_) => "E-DUPFILE",
            LoadError::FileTooLarge { .. } | LoadError::AggregateTooLarge { .. } => "E-LIMIT",
            _ => "E-FILE",
        }
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self.protocol_code();
        match self {
            LoadError::UnsafePath(path) => write!(f, "CVB: {code}: unsafe target path '{path}'"),
            LoadError::DuplicatePath(path) => write!(f, "CVB: {code}: duplicate target path '{path}'"),
            LoadError::Inspect { path, message } => {
                write!(f, "CVB: {code}: unable to inspect '{path}': {message}")
            }
            LoadError::SymlinkTarget(path) => {
                write!(f, "CVB: {code}: symlink target is not allowed: '{path}'")
            }
            LoadError::NotRegularFile(path) => {
                write!(f, "CVB: {code}: target is not a regular file: '{path}'")
            }
            LoadError::FileTooLarge { path, size, limit } => {
                write!(
                    f,
                    "CVB: {code}: target file '{path}' is too large ({size} bytes, limit is {limit} bytes)"
                )
            }
            LoadError::AggregateTooLarge { size, limit } => {
                write!(
                    f,
                    "CVB: {code}: aggregate target file size ({size} bytes) exceeds limit ({limit} bytes)"
                )
            }
            LoadError::Read { path, message } => {
                write!(f, "CVB: {code}: unable to read '{path}': {message}")
            }
            LoadError::InvalidUtf8(path) => write!(f, "CVB: {code}: invalid UTF-8 in '{path}'"),
            LoadError::NulByte(path) => write!(f, "CVB: {code}: NUL byte is not allowed: '{path}'"),
            LoadError::Bom(path) => write!(f, "CVB: {code}: UTF-8 BOM is not allowed: '{path}'"),
            LoadError::LoneCr(path) => write!(f, "CVB: {code}: lone CR is not allowed: '{path}'"),
            LoadError::MixedLineEndings(path) => {
                write!(f, "CVB: {code}: mixed LF/CRLF line endings: '{path}'")
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// A fully validated proposal. Creating a plan never writes to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub token: String,
    pub id: String,
    pub digest: String,
    pub files: Vec<PlannedFile>,
}

/// Original and proposed contents for one existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedFile {
    pub path: String,
    pub original: String,
    pub original_digest: String,
    pub proposed: String,
    pub proposed_digest: String,
    pub operations: Vec<Operation>,
}

/// Failures the pure planner can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanError {
    InvalidPath,
    DuplicateFile,
    MissingFile,
    InvalidFile,
    Range,
    Precondition,
    Overlap,
    EndOfFile,
    AmbiguousMatches,
    InvalidChoice,
    NoOp,
}

impl PlanError {
    pub(crate) fn protocol_code(&self) -> &'static str {
        match self {
            PlanError::Precondition => "E-PRECOND",
            PlanError::EndOfFile => "E-EOF",
            PlanError::Overlap => "E-OVERLAP",
            PlanError::Range => "E-RANGE",
            PlanError::AmbiguousMatches => "E-AMBIGUOUS",
            PlanError::InvalidChoice => "E-INVALID-CHOICE",
            PlanError::NoOp => "E-NOOP",
            PlanError::InvalidFile | PlanError::MissingFile => "E-FILE",
            PlanError::InvalidPath => "E-PATH",
            PlanError::DuplicateFile => "E-DUPFILE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::Crlf => "\r\n",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewlineError {
    LoneCr,
    Mixed,
}

pub(crate) fn detect_newline(contents: &str) -> Result<Newline, NewlineError> {
    let bytes = contents.as_bytes();
    let mut has_lf = false;
    let mut has_crlf = false;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                if bytes.get(index + 1) == Some(&b'\n') {
                    has_crlf = true;
                    index += 2;
                } else {
                    return Err(NewlineError::LoneCr);
                }
            }
            b'\n' => {
                has_lf = true;
                index += 1;
            }
            _ => index += 1,
        }
    }

    if has_lf && has_crlf {
        return Err(NewlineError::Mixed);
    }

    if has_crlf {
        Ok(Newline::Crlf)
    } else {
        Ok(Newline::Lf)
    }
}

/// Validates relative paths: rejects empty, backslashes, absolute paths,
/// non-normal components (`.` / `..`), and `.git` components (case-insensitive).
pub(crate) fn validate_relative_path(path: &str) -> Result<(), PlanError> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.ends_with('/') {
        return Err(PlanError::InvalidPath);
    }

    let p = Path::new(path);
    if p.is_absolute() {
        return Err(PlanError::InvalidPath);
    }

    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(PlanError::InvalidPath);
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(PlanError::InvalidPath);
        }
    }

    for component in p.components() {
        match component {
            Component::Normal(c) => {
                if c.to_str().is_some_and(|s| s.eq_ignore_ascii_case(".git")) {
                    return Err(PlanError::InvalidPath);
                }
            }
            _ => return Err(PlanError::InvalidPath),
        }
    }

    Ok(())
}

/// Error from parsing, loading, or planning a surgery envelope.
#[derive(Debug)]
pub(crate) enum PipelineError {
    Parse(ParseFailure),
    Load(LoadError),
    Plan(PlanFailure),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::Parse(err) => write!(f, "{err}"),
            PipelineError::Load(err) => write!(f, "{err}"),
            PipelineError::Plan(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PipelineError {}

/// Planning and diff rendering happen before confirmation. Persistent project
/// state is initialized only after the user confirms the proposed changes.
pub(crate) fn find_begin_token(input: &str) -> Option<&str> {
    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("CVB::") {
            if let Some((token, suffix)) = rest.split_once("::BEGIN") {
                if suffix.is_empty() || suffix.starts_with(' ') {
                    return Some(token);
                }
            }
        }
    }
    None
}

fn enter_surgery_mode(root: std::path::PathBuf, session_token: String) -> ! {
    let exit_code = run_surgery_mode(root, session_token);
    std::process::exit(exit_code);
}

fn start_surgery(
    control: &crate::control::ControlChannel,
    session_token: &str,
    enter_terminal: impl FnOnce() -> Result<
        crate::surgery::keys::SurgeryTerminalGuard,
        (String, i32),
    >,
    open_tty: impl FnOnce() -> std::io::Result<std::fs::File>,
) -> Result<
    (crate::surgery::keys::SurgeryTerminalGuard, std::fs::File),
    (String, i32),
> {
    if let Err(err) = control.send(&crate::control::ControlMessage::SurgeryBegin {
        token: session_token.to_string(),
    }) {
        return Err((
            format!(
                "CVB: E-NOSESSION: failed to initialize surgery session on control channel: {err}"
            ),
            2,
        ));
    }

    let guard = match enter_terminal() {
        Ok(guard) => guard,
        Err((message, code)) => {
            if let Err(send_error) =
                control.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                return Err((
                    format!(
                        "{message}; additionally failed to report surgery cleanup: {send_error}"
                    ),
                    2,
                ));
            }
            return Err((message, code));
        }
    };

    let tty = match open_tty() {
        Ok(file) => file,
        Err(error) => {
            if let Err(send_error) =
                control.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                return Err((
                    format!(
                        "CVB: E-NOTTY: interactive terminal confirmation requires /dev/tty: {error}; additionally failed to report surgery cleanup: {send_error}"
                    ),
                    2,
                ));
            }
            return Err((
                format!(
                    "CVB: E-NOTTY: interactive terminal confirmation requires /dev/tty: {error}"
                ),
                2,
            ));
        }
    };

    Ok((guard, tty))
}

fn format_ambiguity_report(
    path: &str,
    op_index: usize,
    hint: usize,
    candidates: &[usize],
    lines: &[&str],
) -> String {
    let mut report = format!(
        "CVB ambiguity report\nFile: {path}\nOperation: {op_index}\nHint line: {hint}\nCandidates (in proximity order):\n"
    );

    for (index, candidate) in candidates.iter().enumerate() {
        report.push_str(&format!("\n{}. Line {}\n", index + 1, candidate));
        let start = candidate.saturating_sub(4).max(1);
        let end = candidate.saturating_add(3).min(lines.len());
        for number in start..=end {
            let marker = if number == *candidate { ">" } else { " " };
            if let Some(line) = lines.get(number - 1) {
                report.push_str(&format!("{marker} {number:>5} | {line}\n"));
            }
        }
    }

    report
}

fn choose_ambiguous_match<R: std::io::Read>(
    root: &Path,
    reader: &mut crate::surgery::keys::EventReader<R>,
    failure: &PlanFailure,
    control: &crate::control::ControlChannel,
) -> Result<Option<usize>, String> {
    use crate::surgery::mode::{ChooserState, ChooserTransition};
    use std::io::Write as _;

    let (hint, candidates) = match &failure.detail {
        Some(PlanDetail::AmbiguousMatches { hint, candidates }) => (*hint, candidates.clone()),
        _ => return Err("CVB: chooser called without ambiguous-match details".into()),
    };
    if candidates.len() > 5 {
        return Err(format!(
            "CVB: anchor too generic: found {} exact matches; narrow the anchor to at most 5 candidates",
            candidates.len()
        ));
    }

    let contents = std::fs::read_to_string(root.join(&failure.path))
        .map_err(|error| format!("CVB: unable to read chooser context for {}: {error}", failure.path))?;
    let lines: Vec<&str> = contents.lines().collect();
    if lines.is_empty() {
        return Err(format!("CVB: chooser target is empty: {}", failure.path));
    }

    let report = format_ambiguity_report(&failure.path, failure.op_index, hint, &candidates, &lines);
    control
        .send(&crate::control::ControlMessage::ClipboardWrite { text: report })
        .map_err(|error| format!("CVB: unable to copy ambiguity report: {error}"))?;

    let suggested_index = candidates.iter().position(|line| *line == hint);
    let mut chooser = ChooserState::new(candidates, suggested_index)
        .ok_or_else(|| "CVB: chooser requires between 2 and 5 valid candidates".to_string())?;
    chooser.set_context_line_count(lines.len());
    let selected_line = chooser.selected_line();
    chooser.focus_line(selected_line);

    loop {
        let selected_line = chooser.selected_line();
        let suggested_line = chooser.suggested_line();
        let offset = chooser.scroll_offset();
        let end = offset.saturating_add(15).min(lines.len());

        eprint!(
            "\r\x1b[2J\x1b[H[CVB chooser] file={} op={}\r\n",
            failure.path, failure.op_index
        );
        for index in 0..5 {
            if let Some(line) = chooser.candidate_lines().get(index) {
                let selected = if *line == selected_line { ">" } else { " " };
                let suggested = if Some(*line) == suggested_line { "*" } else { " " };
                let slot = format!("{selected}{suggested}{}:L{line}", index + 1);
                eprint!("{slot:<12}");
            } else {
                eprint!("{:<12}", "");
            }
        }
        eprint!("\u{2190}/\u{2192} or 1\u{2013}5 select  Enter confirm  Esc/q cancel  \u{2191}/\u{2193} PgUp/PgDn Home/End scroll\r\n");
        eprint!("Context lines {}-{} of {}\r\n", offset + 1, end, lines.len());

        for (index, line) in lines.iter().enumerate().take(end).skip(offset) {
            let number = index + 1;
            let selected_marker = if number == selected_line { '>' } else { ' ' };
            let suggested_marker = if Some(number) == suggested_line { '*' } else { ' ' };
            eprint!("{selected_marker}{suggested_marker} {number:>5} | {line}\r\n");
        }
        std::io::stderr()
            .flush()
            .map_err(|error| format!("CVB: failed to display ambiguity chooser: {error}"))?;

        let event = reader
            .next_event()
            .map_err(|error| format!("CVB: failed to read chooser input: {error}"))?;
        let previous_line = chooser.selected_line();
        let transition = chooser.step(event);
        let current_line = chooser.selected_line();
        if current_line != previous_line {
            chooser.focus_line(current_line);
        }

        match transition {
            ChooserTransition::Selected(line) => {
                eprint!("\r\x1b[2J\x1b[H");
                return Ok(Some(line));
            }
            ChooserTransition::Aborted => {
                eprint!("\r\x1b[2J\x1b[H");
                return Ok(None);
            }
            ChooserTransition::Changed | ChooserTransition::Ignored => {}
        }
    }
}

fn run_surgery_mode(root: std::path::PathBuf, session_token: String) -> i32 {
    use std::io::Write as _;
    use crate::surgery::keys::{Event, EventReader, SurgeryTerminalGuard};
    use crate::surgery::mode::{step, step_dry_run_result, ModeAction, ModeState, CandidateData};
    use crate::surgery::pager::DiffPager;
    use crate::control::{ControlChannel, ControlMessage};

    let control = match ControlChannel::client_on_fd(3) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("CVB: E-NOSESSION: interactive surgery requires an active CVB session ({err}).");
            return 2;
        }
    };

    let (_guard, tty) = match start_surgery(
        &control,
        &session_token,
        SurgeryTerminalGuard::enter,
        || std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty"),
    ) {
        Ok(resources) => resources,
        Err((message, code)) => {
            eprintln!("{message}");
            return code;
        }
    };

    let mut reader = EventReader::new_terminal(tty, _guard.raw_fd());
    let mut state = ModeState::Idle;
    let mut held_plan: Option<Plan> = None;
    let mut held_envelope: Option<Envelope> = None;

    eprint!("\r\x1b[2K[CVB surgery] ^C cancel  ^V paste  ^B build\r\n");
    if let Err(error) = std::io::stderr().flush() {
        eprintln!("CVB: failed to flush surgery startup banner: {error}");
        if let Err(send_error) = control.send(&ControlMessage::SurgeryEnd {
            status: 1,
            outcome: String::new(),
        }) {
            eprintln!(
                "CVB: additionally failed to report surgery cleanup: {send_error}"
            );
        }
        return 2;
    }

    loop {
        let event = match reader.next_event() {
            Ok(ev) => ev,
            Err(err) => {
                eprintln!("CVB: error reading terminal event: {err}");
                if let Err(send_error) = control.send(&ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                }) {
                    eprintln!(
                        "CVB: control channel failed while ending surgery after terminal-event read error: {send_error}"
                    );
                    return 2;
                }
                return 3;
            }
        };

        let now = std::time::Instant::now();
        let (next_state, actions) = step(state, event, now, &session_token);
        state = next_state;

        let mut pending_actions = actions;
        while !pending_actions.is_empty() {
            let action = pending_actions.remove(0);
            match action {
                ModeAction::ReadClipboard => {
                    match crate::clipboard::read_from_clipboard() {
                        Ok(bytes) => match std::str::from_utf8(&bytes) {
                            Ok(text) => {
                                let (s, acts) = step(state, Event::Paste(text.to_string()), std::time::Instant::now(), &session_token);
                                state = s;
                                pending_actions.extend(acts);
                            }
                            Err(err) => {
                                let (s, acts) = step(
                                    state,
                                    Event::PasteError(format!("CVB: clipboard content is not valid UTF-8: {err}")),
                                    std::time::Instant::now(),
                                    &session_token,
                                );
                                state = s;
                                pending_actions.extend(acts);
                            }
                        },
                        Err(err) => {
                            let (s, acts) = step(
                                state,
                                Event::PasteError(format!("CVB: unable to read clipboard: {err}")),
                                std::time::Instant::now(),
                                &session_token,
                            );
                            state = s;
                            pending_actions.extend(acts);
                        }
                    }
                }
                ModeAction::IgnoreNonEnvelope { message } => {
                    eprintln!("\r\x1b[2K{message}");
                    eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] ");
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush ignored-envelope message: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::RunDryRun(payload) => {
                    held_plan = None;
                    held_envelope = None;

                    let planning_result: Result<(Envelope, Plan), String> = {
                        let mut choices = std::collections::HashMap::new();
                        loop {
                            match parse_load_plan_with_choices(&root, &payload, &choices) {
                                Ok(result) => break Ok(result),
                                Err(PipelineError::Plan(failure)) => {
                                    if matches!(
                                        &failure.detail,
                                        Some(PlanDetail::AmbiguousMatches { .. })
                                    ) {
                                        match choose_ambiguous_match(&root, &mut reader, &failure, &control) {
                                            Ok(Some(line)) => {
                                                choices.insert(
                                                    (failure.path.clone(), failure.op_index),
                                                    line,
                                                );
                                            }
                                            Ok(None) => {
                                                break Err(
                                                    "CVB: chooser aborted; no changes applied".into()
                                                );
                                            }
                                            Err(error) => break Err(error),
                                        }
                                    } else {
                                        break Err(failure.to_string());
                                    }
                                }
                                Err(error) => break Err(error.to_string()),
                            }
                        }
                    };

                    match planning_result {
                        Ok((envelope, plan)) => {
                            let diff = render_plan_diff(&plan, RenderOptions { color: false });
                            let full_diff = format!(
                                "CVB surgery candidate id={} files={}\n{}",
                                envelope.id,
                                plan.files.len(),
                                diff
                            );
                            eprintln!("\r\x1b[2K{full_diff}");

                            let limit = crate::clipboard::configured_copy_limit();
                            match DiffPager::new(&full_diff, limit) {
                                Ok(pager) => {
                                    let candidate = CandidateData {
                                        id: envelope.id.clone(),
                                        files_count: plan.files.len(),
                                        diff: full_diff,
                                        pager,
                                    };
                                    held_plan = Some(plan);
                                    held_envelope = Some(envelope);
                                    let (s, acts) = step_dry_run_result(Ok(candidate));
                                    state = s;
                                    pending_actions.extend(acts);
                                }
                                Err(err) => {
                                    eprintln!("\r\x1b[2K{err}");
                                    let (s, acts) = step_dry_run_result(Err(err));
                                    state = s;
                                    pending_actions.extend(acts);
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("\r\x1b[2K{err}");
                            let (s, acts) = step_dry_run_result(Err(err));
                            state = s;
                            pending_actions.extend(acts);
                        }
                    }
                }
                ModeAction::ArmBuild => {
                    eprint!("\r\x1b[2K[^B again to build, any other key disarms] ");
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush arm-build prompt: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::FireBuild => {
                    let plan = match held_plan.take() {
                        Some(p) => p,
                        None => {
                            eprintln!("\r\x1b[2KCVB: no candidate plan available to build");
                            state = ModeState::Idle;
                            continue;
                        }
                    };
                    let envelope = match held_envelope.take() {
                        Some(e) => e,
                        None => {
                            eprintln!("\r\x1b[2KCVB: no candidate envelope available to build");
                            state = ModeState::Idle;
                            continue;
                        }
                    };

                    let state_base = match resolve_state_dir() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("\r\x1b[2K{e}");
                            let clipboard_error = control
                                .send(&ControlMessage::ClipboardWrite {
                                    text: e.to_string(),
                                })
                                .err();
                            let end_error = control
                                .send(&ControlMessage::SurgeryEnd {
                                    status: 1,
                                    outcome: String::new(),
                                })
                                .err();

                            if let Some(error) = clipboard_error {
                                eprintln!(
                                    "\r\x1b[2KCVB: control channel failed while reporting state-directory error: {error}"
                                );
                                return 2;
                            }
                            if let Some(error) = end_error {
                                eprintln!(
                                    "\r\x1b[2KCVB: control channel failed while ending surgery after state-directory error: {error}"
                                );
                                return 2;
                            }
                            return SurgeryExitStatus::RejectedBeforeCommit.code();
                        }
                    };

                    let project_state = match init_project_state(&state_base, &root) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("\r\x1b[2K{e}");
                            let clipboard_error = control
                                .send(&ControlMessage::ClipboardWrite {
                                    text: e.to_string(),
                                })
                                .err();
                            let end_error = control
                                .send(&ControlMessage::SurgeryEnd {
                                    status: 1,
                                    outcome: String::new(),
                                })
                                .err();

                            if let Some(error) = clipboard_error {
                                eprintln!(
                                    "\r\x1b[2KCVB: control channel failed while reporting project-state initialization error: {error}"
                                );
                                return 2;
                            }
                            if let Some(error) = end_error {
                                eprintln!(
                                    "\r\x1b[2KCVB: control channel failed while ending surgery after project-state initialization error: {error}"
                                );
                                return 2;
                            }
                            return SurgeryExitStatus::RejectedBeforeCommit.code();
                        }
                    };

                    match apply_plan(&project_state, &plan) {
                        Ok(_) => {
                            let outcome = format!(
                                "CVB surgery APPLIED id={} files={}\n",
                                envelope.id,
                                plan.files.len()
                            );
                            if let Err(send_error) = control.send(&ControlMessage::SurgeryEnd {
                                status: 0,
                                outcome: outcome.clone(),
                            }) {
                                eprintln!(
                                    "\r\x1b[2KCVB: surgery was applied, but completion reporting failed: {send_error}"
                                );
                                return 2;
                            }
                            eprint!("\r\x1b[2K{outcome}");
                            return SurgeryExitStatus::Applied.code();
                        }
                        Err(error) => {
                            let is_stale = match &error {
                                ApplyError::Stale { .. } => true,
                                ApplyError::CommitFailed(report) if report.message.contains("E-STALE") => true,
                                _ => false,
                            };

                            if is_stale {
                                let error_text = error.to_string();
                                if let Err(send_error) = control.send(&ControlMessage::ClipboardWrite {
                                    text: error_text.clone(),
                                }) {
                                    eprintln!(
                                        "\r\x1b[2KCVB: control channel failed while reporting stale-plan error: {send_error}"
                                    );
                                    return 2;
                                }
                                eprintln!("\r\x1b[2K{error}");
                                state = ModeState::Idle;
                                held_plan = None;
                                held_envelope = None;
                                eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] ");
                                if let Err(error) = std::io::stderr().flush() {
                                    eprintln!(
                                        "\r\x1b[2KCVB: failed to flush stale-plan recovery prompt: {error}"
                                    );
                                    if let Err(send_error) =
                                        control.send(&ControlMessage::SurgeryEnd {
                                            status: 1,
                                            outcome: String::new(),
                                        })
                                    {
                                        eprintln!(
                                            "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                                        );
                                    }
                                    return 2;
                                }
                            } else {
                                let report = error.to_string();
                                let clipboard_error = control
                                    .send(&ControlMessage::ClipboardWrite {
                                        text: report,
                                    })
                                    .err();
                                let end_error = control
                                    .send(&ControlMessage::SurgeryEnd {
                                        status: 1,
                                        outcome: String::new(),
                                    })
                                    .err();

                                eprintln!("\r\x1b[2K{error}");
                                if let Some(send_error) = clipboard_error {
                                    eprintln!(
                                        "\r\x1b[2KCVB: control channel failed while reporting apply-plan error: {send_error}"
                                    );
                                    return 2;
                                }
                                if let Some(send_error) = end_error {
                                    eprintln!(
                                        "\r\x1b[2KCVB: control channel failed while ending surgery after apply-plan error: {send_error}"
                                    );
                                    return 2;
                                }
                                return error.exit_status().code();
                            }
                        }
                    }
                }
                ModeAction::Disarm => {
                    eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] ");
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush disarm prompt: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::NothingToBuild => {
                    eprint!("\r\x1b[2Knothing to build\r\n[^C cancel  ^V paste  ^B build] ");
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush nothing-to-build prompt: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::UpdatePagerChunk { chunk, status_line } => {
                    if let Err(error) = control.send(&ControlMessage::ClipboardWrite {
                        text: chunk.clone(),
                    }) {
                        eprintln!(
                            "\r\x1b[2KCVB: control channel failed while updating surgery clipboard: {error}"
                        );
                        return 2;
                    }

                    if let Some(status) = status_line {
                        eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] {status} ");
                    } else {
                        eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] ");
                    }
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush surgery terminal output: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::ReportDryRunError(err) => {
                    if let Err(error) = control.send(&ControlMessage::ClipboardWrite {
                        text: err.clone(),
                    }) {
                        eprintln!(
                            "\r\x1b[2KCVB: control channel failed while reporting dry-run error: {error}"
                        );
                        return 2;
                    }
                    eprint!("\r\x1b[2K[^C cancel  ^V paste  ^B build] ");
                    if let Err(error) = std::io::stderr().flush() {
                        eprintln!(
                            "\r\x1b[2KCVB: failed to flush dry-run recovery prompt: {error}"
                        );
                        if let Err(send_error) =
                            control.send(&ControlMessage::SurgeryEnd {
                                status: 1,
                                outcome: String::new(),
                            })
                        {
                            eprintln!(
                                "\r\x1b[2KCVB: additionally failed to report surgery cleanup: {send_error}"
                            );
                        }
                        return 2;
                    }
                }
                ModeAction::Abort { outcome } => {
                    let clipboard_error = control
                        .send(&ControlMessage::ClipboardWrite {
                            text: "CVB surgery aborted\n".to_string(),
                        })
                        .err();
                    let end_error = control
                        .send(&ControlMessage::SurgeryEnd {
                            status: 1,
                            outcome: String::new(),
                        })
                        .err();

                    eprint!("\r\x1b[2K{outcome}");
                    if let Some(error) = clipboard_error {
                        eprintln!(
                            "\r\x1b[2KCVB: control channel failed while reporting surgery abort: {error}"
                        );
                        return 2;
                    }
                    if let Some(error) = end_error {
                        eprintln!(
                            "\r\x1b[2KCVB: control channel failed while ending aborted surgery: {error}"
                        );
                        return 2;
                    }
                    return SurgeryExitStatus::UserDeclined.code();
                }
            }
        }
    }
}

pub(crate) fn verify_envelope_token(input: &str, session_token: &str) -> Result<(), String> {
    if let Some(token) = find_begin_token(input) {
        if token != session_token {
            return Err(format!(
                "CVB: E-TOKEN: envelope token '{token}' does not match active session token '{session_token}'; run `cvb token` to print the current token."
            ));
        }
    }
    Ok(())
}

pub(crate) fn run(
    dry_run: bool,
    stdin: bool,
    root: Option<std::path::PathBuf>,
    spec: bool,
) -> ! {
    if spec {
        crate::context::print_spec();
    }

    let session_token = match std::env::var("CVB_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => {
            eprintln!("CVB: E-NOSESSION: no active surgery session (CVB_TOKEN not set).");
            std::process::exit(2);
        }
    };

    let root = match root {
        Some(root) => root,
        None => match std::env::current_dir() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("CVB: unable to determine project root: {error}");
                std::process::exit(2);
            }
        },
    };

    if !stdin && !dry_run {
        enter_surgery_mode(root, session_token);
    }

    let input = match read_input(stdin) {
        Ok(input) => input,
        Err((message, code)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    };

    let input = match std::str::from_utf8(&input) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("CVB: surgery input is not valid UTF-8: {error}");
            std::process::exit(2);
        }
    };

    if let Err(error) = verify_envelope_token(input, &session_token) {
        eprintln!("{error}");
        std::process::exit(2);
    }

    let (envelope, plan) = match parse_load_plan(&root, input) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    if envelope.token != session_token {
        eprintln!(
            "CVB: E-TOKEN: envelope token '{}' does not match active session token '{}'; run `cvb token` to print the current token.",
            envelope.token,
            session_token
        );
        std::process::exit(2);
    }

    let diff = render_plan_diff(&plan, RenderOptions { color: false });
    let mut control = match crate::control::ControlChannel::client_on_fd(3) {
        Ok(control) => Some(control),
        Err(error) => {
            eprintln!("CVB: E-NOSESSION: surgery requires an active CVB session ({error}).");
            std::process::exit(2);
        }
    };
    if let Some(ref mut c) = control {
        if let Err(send_error) =
            c.send(&crate::control::ControlMessage::SurgeryBegin {
                token: session_token.clone(),
            })
        {
            eprintln!("CVB: failed to report surgery start: {send_error}");
            std::process::exit(2);
        }
        if let Err(send_error) =
            c.send(&crate::control::ControlMessage::ClipboardWrite {
                text: diff.clone(),
            })
        {
            eprintln!("CVB: failed to send surgery diff to clipboard bridge: {send_error}");
            std::process::exit(2);
        }
    }
    use std::io::Write as _;
    if let Err(error) = std::io::stdout().write_all(diff.as_bytes()) {
        if let Some(ref mut c) = control {
            if let Err(send_error) =
                c.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                eprintln!(
                    "CVB: unable to write surgery diff: {error}; additionally failed to report surgery cleanup: {send_error}"
                );
                std::process::exit(2);
            }
        }
        eprintln!("CVB: unable to write surgery diff: {error}");
        std::process::exit(2);
    }

    if let Err(error) = std::io::stdout().flush() {
        if let Some(ref mut c) = control {
            if let Err(send_error) =
                c.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                eprintln!(
                    "CVB: unable to flush surgery diff: {error}; additionally failed to report surgery cleanup: {send_error}"
                );
                std::process::exit(2);
            }
        }
        eprintln!("CVB: unable to flush surgery diff: {error}");
        std::process::exit(2);
    }

    let confirmation = match prompt_confirmation(dry_run) {
        Ok(result) => result,
        Err((message, code)) => {
            if let Some(ref mut c) = control {
                if let Err(send_error) =
                    c.send(&crate::control::ControlMessage::SurgeryEnd {
                        status: 1,
                        outcome: String::new(),
                    })
                {
                    eprintln!(
                        "{message}; additionally failed to report surgery cleanup: {send_error}"
                    );
                    std::process::exit(2);
                }
            }
            eprintln!("{message}");
            std::process::exit(code);
        }
    };

    if dry_run {
        if let Some(ref mut c) = control {
            if let Err(send_error) =
                c.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                eprintln!("CVB: failed to report dry-run cleanup: {send_error}");
                std::process::exit(2);
            }
        }
        std::process::exit(0);
    }

    if confirmation == ConfirmResult::Declined {
        if let Some(ref mut c) = control {
            if let Err(send_error) =
                c.send(&crate::control::ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                })
            {
                eprintln!("CVB: failed to report declined-confirmation cleanup: {send_error}");
                std::process::exit(2);
            }
        }
        std::process::exit(SurgeryExitStatus::UserDeclined.code());
    }

    let state_base = match resolve_state_dir() {
        Ok(path) => path,
        Err(error) => {
            if let Some(ref mut c) = control {
                if let Err(send_error) =
                    c.send(&crate::control::ControlMessage::SurgeryEnd {
                        status: 1,
                        outcome: String::new(),
                    })
                {
                    eprintln!(
                        "{error}; additionally failed to report surgery cleanup: {send_error}"
                    );
                    std::process::exit(2);
                }
            }
            eprintln!("{error}");
            std::process::exit(SurgeryExitStatus::RejectedBeforeCommit.code());
        }
    };

    let state = match init_project_state(&state_base, &root) {
        Ok(state) => state,
        Err(error) => {
            if let Some(ref mut c) = control {
                if let Err(send_error) =
                    c.send(&crate::control::ControlMessage::SurgeryEnd {
                        status: 1,
                        outcome: String::new(),
                    })
                {
                    eprintln!(
                        "{error}; additionally failed to report surgery cleanup: {send_error}"
                    );
                    std::process::exit(2);
                }
            }
            eprintln!("{error}");
            std::process::exit(SurgeryExitStatus::RejectedBeforeCommit.code());
        }
    };

    match apply_plan(&state, &plan) {
        Ok(_) => {
            if let Some(ref mut c) = control {
                let outcome = format!(
                    "CVB surgery APPLIED id={} files={}\n",
                    envelope.id,
                    plan.files.len()
                );
                if let Err(send_error) =
                    c.send(&crate::control::ControlMessage::SurgeryEnd {
                        status: 0,
                        outcome,
                    })
                {
                    eprintln!("CVB: failed to report applied surgery completion: {send_error}");
                    std::process::exit(2);
                }
            }
            std::process::exit(SurgeryExitStatus::Applied.code());
        }
        Err(error) => {
            let status = error.exit_status().code();
            if let Some(ref mut c) = control {
                if let Err(send_error) =
                    c.send(&crate::control::ControlMessage::SurgeryEnd {
                        status: 1,
                        outcome: String::new(),
                    })
                {
                    eprintln!(
                        "{error}; additionally failed to report surgery failure: {send_error}"
                    );
                    std::process::exit(2);
                }
            }
            eprintln!("{error}");
            std::process::exit(status);
        }
    }
}

/// Parses an envelope, reads its target files, and builds an in-memory plan.
/// This function never writes to disk.
pub(crate) fn parse_load_plan(root: &Path, input: &str) -> Result<(Envelope, Plan), PipelineError> {
    parse_load_plan_with_choices(root, input, &std::collections::HashMap::new())
}

/// Parses an envelope, loads its original files, and builds a plan using explicit candidate choices.
/// This function never writes to disk.
pub(crate) fn parse_load_plan_with_choices(
    root: &Path,
    input: &str,
    choices: &std::collections::HashMap<(String, usize), usize>,
) -> Result<(Envelope, Plan), PipelineError> {
    let envelope = parse_envelope(input).map_err(PipelineError::Parse)?;
    let originals = load_originals(root, &envelope).map_err(PipelineError::Load)?;
    let plan =
        plan_envelope_with_choices(&envelope, &originals, choices).map_err(PipelineError::Plan)?;

    Ok((envelope, plan))
}

#[cfg(test)]
pub(crate) trait ErrorKind<K> {
    fn kind(&self) -> K;
}

#[cfg(test)]
impl ErrorKind<ParseError> for ParseFailure {
    fn kind(&self) -> ParseError {
        self.kind
    }
}

#[cfg(test)]
impl ErrorKind<PlanError> for PlanFailure {
    fn kind(&self) -> PlanError {
        self.kind
    }
}

#[cfg(test)]
pub(crate) fn kind<T, E: ErrorKind<K>, K>(r: Result<T, E>) -> Result<T, K> {
    r.map_err(|e| e.kind())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{ControlChannel, ControlMessage};
    use crate::surgery::keys::SurgeryTerminalGuard;
    use std::fs;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::path::PathBuf;

    fn control_pair() -> (ControlChannel, ControlChannel) {
        let mut fds = [0; 2];
        let result = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM,
                0,
                fds.as_mut_ptr(),
            )
        };
        assert_eq!(result, 0, "socketpair must succeed");

        unsafe {
            (
                ControlChannel::new(OwnedFd::from_raw_fd(fds[0])),
                ControlChannel::new(OwnedFd::from_raw_fd(fds[1])),
            )
        }
    }

    #[test]
    fn startup_terminal_entry_failure_sends_surgery_end() {
        let (control, mut peer) = control_pair();
        let (reader, _writer) = nix::unistd::pipe().expect("pipe must succeed");

        let result = start_surgery(
            &control,
            "test-token",
            || SurgeryTerminalGuard::enter_on(&reader),
            || panic!("TTY open must not run after terminal-entry failure"),
        );

        assert!(matches!(result, Err((ref message, 2)) if message.starts_with("E-NOTTY")));

        drop(control);
        assert_eq!(
            peer.receive().unwrap(),
            vec![
                ControlMessage::SurgeryBegin {
                    token: "test-token".to_string(),
                },
                ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                },
            ]
        );
    }

    #[test]
    fn startup_tty_open_failure_sends_surgery_end() {
        let (control, mut peer) = control_pair();
        let pty = nix::pty::openpty(None, None).expect("openpty must succeed");

        let result = start_surgery(
            &control,
            "test-token",
            || SurgeryTerminalGuard::enter_on(&pty.slave),
            || Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "injected TTY failure",
            )),
        );

        assert!(
            matches!(result, Err((ref message, 2))
                if message.starts_with("CVB: E-NOTTY:")
                    && message.contains("injected TTY failure"))
        );

        drop(control);
        assert_eq!(
            peer.receive().unwrap(),
            vec![
                ControlMessage::SurgeryBegin {
                    token: "test-token".to_string(),
                },
                ControlMessage::SurgeryEnd {
                    status: 1,
                    outcome: String::new(),
                },
            ]
        );
    }

    #[test]
    fn startup_begin_failure_does_not_send_surgery_end() {
        let (control, mut peer) = control_pair();
        let oversized_token = "x".repeat(5 * 1024 * 1024);

        let result = start_surgery(
            &control,
            &oversized_token,
            || panic!("terminal entry must not run after begin failure"),
            || panic!("TTY open must not run after begin failure"),
        );

        assert!(
            matches!(result, Err((ref message, 2))
                if message.starts_with(
                    "CVB: E-NOSESSION: failed to initialize surgery session"
                ))
        );

        drop(control);
        assert!(peer.receive().unwrap().is_empty());
    }

    #[test]
    fn ambiguity_report_preserves_candidate_order_and_context_boundaries() {
        let lines = [
            "line 1", "line 2", "line 3", "line 4", "line 5",
            "line 6", "line 7", "line 8", "line 9", "line 10",
        ];

        let report =
            format_ambiguity_report("src/example.rs", 7, 5, &[1, 10], &lines);

        let first_candidate = report
            .find("\n1. Line 1\n")
            .expect("first candidate must be reported");
        let second_candidate = report
            .find("\n2. Line 10\n")
            .expect("second candidate must be reported");

        assert!(first_candidate < second_candidate);

        assert!(report.contains("File: src/example.rs"));
        assert!(report.contains("Operation: 7"));
        assert!(report.contains("Hint line: 5"));
        assert!(report.contains(">     1 | line 1"));
        assert!(report.contains("      2 | line 2"));

        let first_context = &report[first_candidate..second_candidate];
        let second_context = &report[second_candidate..];

        assert!(first_context.contains("line 1\n"));
        assert!(first_context.contains("line 4\n"));
        assert!(!first_context.contains("line 5\n"));

        assert!(second_context.contains("line 6\n"));
        assert!(second_context.contains("line 10\n"));
        assert!(second_context.contains(">    10 | line 10"));
        assert!(!second_context.contains("line 5\n"));
    }

    #[test]
    fn ambiguity_chooser_rejects_more_than_five_candidates_as_too_generic() {
        let failure = PlanFailure {
            kind: PlanError::AmbiguousMatches,
            path: "ambiguous.rs".into(),
            op_index: 1,
            detail: Some(PlanDetail::AmbiguousMatches {
                hint: 1,
                candidates: vec![1, 2, 3, 4, 5, 6],
            }),
        };
        let (control, _peer) = control_pair();
        let mut reader = crate::surgery::keys::EventReader::new(
            std::io::Cursor::new(b"q".to_vec()),
        );

        let error = choose_ambiguous_match(
            std::path::Path::new("."),
            &mut reader,
            &failure,
            &control,
        )
        .expect_err("six candidates must be rejected before opening the chooser");

        assert!(error.starts_with("CVB: anchor too generic:"), "{error}");
        assert!(error.contains("6 exact matches"), "{error}");
    }

    #[test]
    fn ambiguity_chooser_copies_report_and_aborts_without_modifying_file() {

        use std::collections::HashMap;
        use std::io::Cursor;

        let base = std::env::temp_dir().join(format!(
            "cvb-ambiguity-clipboard-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();

        let original = concat!(
            "fn first() {\n",
            "    let marker = \"ambiguous-anchor\";\n",
            "    println!(\"first\");\n",
            "}\n",
            "fn second() {\n",
            "    let marker = \"ambiguous-anchor\";\n",
            "    println!(\"second\");\n",
            "}\n",
        );
        let target = root.join("ambiguous.rs");
        fs::write(&target, original).unwrap();

        let envelope = Envelope {
            token: "testtoken123".into(),
            id: "ambiguity-clipboard-test".into(),
            files: vec![FileSection {
                path: "ambiguous.rs".into(),
                operations: vec![Operation::Replace {
                    start: 2,
                    end: 2,
                    old: vec!["    let marker = \"ambiguous-anchor\";".into()],
                    new: vec!["    let marker = \"changed-anchor\";".into()],
                }],
            }],
        };
        let originals = HashMap::from([(
            "ambiguous.rs".to_string(),
            original.to_string(),
        )]);
        let failure = plan_envelope(&envelope, &originals)
            .expect_err("duplicate anchor must require an explicit choice");

        assert!(matches!(
            &failure.detail,
            Some(PlanDetail::AmbiguousMatches { hint: 2, candidates })
                if candidates == &[2, 6]
        ));

        let (control, mut peer) = control_pair();
        let mut reader = EventReader::new(Cursor::new(b"q".to_vec()));

        let result = choose_ambiguous_match(&root, &mut reader, &failure, &control)
            .expect("chooser should handle an abort event");
        assert_eq!(result, None);

        let messages = peer.receive().unwrap();
        assert_eq!(messages.len(), 1);
        let report = match &messages[0] {
            ControlMessage::ClipboardWrite { text } => text,
            other => panic!("expected ClipboardWrite, got {other:?}"),
        };

        assert!(report.contains("CVB ambiguity report"));
        assert!(report.contains("Operation: 1"));
        assert!(report.contains("Hint line: 2"));
        assert!(report.contains("1. Line 2"));
        assert!(report.contains(">     2 |     let marker = \"ambiguous-anchor\";"));
        assert!(report.contains("2. Line 6"));
        assert!(report.contains(">     6 |     let marker = \"ambiguous-anchor\";"));
        assert_eq!(fs::read_to_string(&target).unwrap(), original);

        drop(control);
        drop(peer);
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn check_input_size_accepts_within_cap() {
        assert!(check_input_size(MAX_INPUT_BYTES).is_ok());
        assert!(check_input_size(100).is_ok());
    }

    #[test]
    fn check_input_size_rejects_over_cap_with_e_limit_and_truncation_warning() {
        let err = check_input_size(MAX_INPUT_BYTES + 1).unwrap_err();
        assert!(err.starts_with("E-LIMIT:"), "{err}");
        assert!(err.contains("exceeds the 3 MiB cap"), "{err}");

        let err_trunc = check_input_size(3_950_000).unwrap_err();
        assert!(err_trunc.contains("clipboard may have truncated"), "{err_trunc}");
    }

    #[test]
    fn parse_load_plan_rejects_git_target_without_modifying_file() {
        let base: PathBuf =
            std::env::temp_dir().join(format!("cvb-parse-load-plan-git-{}", std::process::id()));
        let root = base.join("project");
        let git_hooks = root.join(".git").join("hooks");

        if base.exists() {
            fs::remove_dir_all(&base).unwrap();
        }
        fs::create_dir_all(&git_hooks).unwrap();

        let hook_file = git_hooks.join("pre-commit");
        let sentinel = "#!/bin/sh\nexit 0\n";
        fs::write(&hook_file, sentinel).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=git-rejection\n",
            "CVB::testtoken123::FILE path=.git/hooks/pre-commit\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "#!/bin/sh\n",
            "CVB::testtoken123::NEW n=1\n",
            "malicious payload\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Parse(ref failure)) if failure.kind == ParseError::InvalidPath => {}
            other => panic!("expected invalid-path parse error for .git target, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&hook_file).unwrap(),
            sentinel,
            "git target rejection must not modify the file"
        );

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parse_load_plan_builds_plan_without_modifying_file() {
        let root: PathBuf =
            std::env::temp_dir().join(format!("cvb-parse-load-plan-{}", std::process::id()));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("example.rs");
        let original = "fn greet() {\n    println!(\"hello\");\n}\n";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=test-id\n",
            "CVB::testtoken123::FILE path=example.rs\n",
            "CVB::testtoken123::OP replace lines=2-2\n",
            "CVB::testtoken123::OLD n=1\n",
            "    println!(\"hello\");\n",
            "CVB::testtoken123::NEW n=1\n",
            "    println!(\"goodbye\");\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        let result = parse_load_plan(&root, input);
        let (envelope, plan) = match result {
            Ok(value) => value,
            Err(PipelineError::Parse(error)) => {
                panic!("pipeline parsing failed: {error:?}")
            }
            Err(PipelineError::Load(error)) => {
                panic!("pipeline loading failed: {error}")
            }
            Err(PipelineError::Plan(error)) => {
                panic!("pipeline planning failed: {error:?}")
            }
        };

        assert_eq!(envelope.id, "test-id");
        assert_eq!(plan.id, "test-id");
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "example.rs");
        assert_eq!(plan.files[0].original, original);
        assert_eq!(
            plan.files[0].proposed,
            "fn greet() {\n    println!(\"goodbye\");\n}\n"
        );

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "planning must not modify the original file"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_malformed_envelope_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-malformed-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("example.rs");
        let original = "keep this file unchanged\n";
        fs::write(&file, original).unwrap();

        match parse_load_plan(&root, "not an envelope") {
            Err(PipelineError::Parse(ref failure)) if failure.kind == ParseError::NoEnvelope => {}
            other => panic!("expected missing-envelope parse error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "a parse failure must not modify the target file"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_missing_target_without_modifying_other_files() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-missing-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("keep.rs");
        let original = "leave this file untouched\n";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=missing-target\n",
            "CVB::testtoken123::FILE path=missing.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "old\n",
            "CVB::testtoken123::NEW n=1\n",
            "new\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Load(_)) => {}
            other => panic!("expected target-file load error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "a load failure must not modify other files"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_plan_error_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-plan-error-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("example.rs");
        let original = "actual contents\n";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=plan-error\n",
            "CVB::testtoken123::FILE path=example.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "different contents\n",
            "CVB::testtoken123::NEW n=1\n",
            "replacement contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Plan(_)) => {}
            other => panic!("expected planning error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "a planning failure must not modify the target file"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_later_file_plan_error_without_modifying_either_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-multi-plan-error-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let first_file = root.join("first.rs");
        let second_file = root.join("second.rs");
        let first_original = "first actual contents\n";
        let second_original = "second actual contents\n";

        fs::write(&first_file, first_original).unwrap();
        fs::write(&second_file, second_original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=multi-plan-error\n",
            "CVB::testtoken123::FILE path=first.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "first actual contents\n",
            "CVB::testtoken123::NEW n=1\n",
            "first replacement contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::FILE path=second.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "wrong second contents\n",
            "CVB::testtoken123::NEW n=1\n",
            "second replacement contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=2",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Plan(_)) => {}
            other => panic!("expected planning error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&first_file).unwrap(),
            first_original,
            "an earlier valid operation must not modify its target"
        );
        assert_eq!(
            fs::read_to_string(&second_file).unwrap(),
            second_original,
            "a later planning failure must not modify its target"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_duplicate_target_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-duplicate-target-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("example.rs");
        let original = "original contents\n";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=duplicate-target\n",
            "CVB::testtoken123::FILE path=example.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "original contents\n",
            "CVB::testtoken123::NEW n=1\n",
            "first replacement\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::FILE path=example.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "original contents\n",
            "CVB::testtoken123::NEW n=1\n",
            "second replacement\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=2",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Parse(ref failure)) if failure.kind == ParseError::DuplicateFile => {
            }
            other => panic!("expected duplicate-file parse error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            original,
            "duplicate target rejection must not modify the file"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_parent_traversal_without_touching_outside_file() {
        let base: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-traversal-{}",
            std::process::id()
        ));
        let root = base.join("project");
        let outside = base.join("outside.rs");

        if base.exists() {
            fs::remove_dir_all(&base).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let sentinel = "outside sentinel\n";
        fs::write(&outside, sentinel).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=parent-traversal\n",
            "CVB::testtoken123::FILE path=../outside.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "outside sentinel\n",
            "CVB::testtoken123::NEW n=1\n",
            "changed contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Parse(ref failure)) if failure.kind == ParseError::InvalidPath => {}
            other => panic!("expected invalid-path parse error, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            sentinel,
            "parent traversal must not modify a file outside the project"
        );

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn parse_load_plan_rejects_symlink_target_without_modifying_outside_file() {
        use std::os::unix::fs::symlink;

        let base: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-symlink-{}",
            std::process::id()
        ));
        let root = base.join("project");
        let outside = base.join("outside.rs");
        let link = root.join("linked.rs");

        if base.exists() {
            fs::remove_dir_all(&base).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let sentinel = "outside sentinel\n";
        fs::write(&outside, sentinel).unwrap();
        symlink(&outside, &link).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=symlink-target\n",
            "CVB::testtoken123::FILE path=linked.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "outside sentinel\n",
            "CVB::testtoken123::NEW n=1\n",
            "changed contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Load(_)) => {}
            other => panic!("expected load error for symlink target, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            sentinel,
            "rejecting a symlink target must not modify its destination"
        );

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn parse_load_plan_rejects_symlink_parent_without_modifying_outside_file() {
        use std::os::unix::fs::symlink;

        let base: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-symlink-parent-{}",
            std::process::id()
        ));
        let root = base.join("project");
        let outside_dir = base.join("outside");
        let outside_file = outside_dir.join("example.rs");

        if base.exists() {
            fs::remove_dir_all(&base).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();

        let sentinel = "outside sentinel\n";
        fs::write(&outside_file, sentinel).unwrap();
        symlink(&outside_dir, root.join("src")).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=symlink-parent\n",
            "CVB::testtoken123::FILE path=src/example.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "outside sentinel\n",
            "CVB::testtoken123::NEW n=1\n",
            "changed contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Load(_)) => {}
            other => panic!("expected load error for symlinked parent directory, got {other:?}"),
        }

        assert_eq!(
            fs::read_to_string(&outside_file).unwrap(),
            sentinel,
            "rejecting a symlinked parent must not modify the outside file"
        );

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_directory_target_without_modifying_directory() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-directory-target-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let sentinel_file = target_dir.join("keep.txt");
        fs::write(&sentinel_file, "keep me\n").unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=directory-target\n",
            "CVB::testtoken123::FILE path=target\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "placeholder\n",
            "CVB::testtoken123::NEW n=1\n",
            "changed contents\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Load(_)) => {}
            other => panic!("expected load error for directory target, got {other:?}"),
        }

        assert!(target_dir.is_dir(), "target directory must remain intact");
        assert_eq!(
            fs::read_to_string(&sentinel_file).unwrap(),
            "keep me\n",
            "rejecting a directory target must preserve its contents"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_invalid_utf8_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-invalid-utf8-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("invalid.rs");
        let original: &[u8] = b"valid prefix\n\xff\xfe\n";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=invalid-utf8\n",
            "CVB::testtoken123::FILE path=invalid.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "valid prefix\n",
            "CVB::testtoken123::NEW n=1\n",
            "replacement\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Load(message)) => {
                assert!(
                    message.contains("invalid UTF-8"),
                    "expected an invalid UTF-8 error message, got: {message}"
                );
            }
            other => panic!("expected load error for invalid UTF-8, got {other:?}"),
        }

        assert_eq!(
            fs::read(&file).unwrap(),
            original,
            "rejecting invalid UTF-8 must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_replacement_in_empty_file_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-empty-file-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("empty.rs");
        let original: &[u8] = b"";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=empty-file\n",
            "CVB::testtoken123::FILE path=empty.rs\n",
            "CVB::testtoken123::OP replace lines=1-1\n",
            "CVB::testtoken123::OLD n=1\n",
            "placeholder\n",
            "CVB::testtoken123::NEW n=1\n",
            "replacement\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Plan(_)) => {}
            other => panic!("expected planning error for replacement in empty file, got {other:?}"),
        }

        assert_eq!(
            fs::read(&file).unwrap(),
            original,
            "a rejected operation must leave an empty file empty"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parse_load_plan_rejects_insertion_into_empty_file_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-empty-insert-{}",
            std::process::id()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(&root).unwrap();

        let file = root.join("empty.rs");
        let original: &[u8] = b"";
        fs::write(&file, original).unwrap();

        let input = concat!(
            "CVB::testtoken123::BEGIN v=1 id=empty-insert\n",
            "CVB::testtoken123::FILE path=empty.rs\n",
            "CVB::testtoken123::OP insert before=1-1\n",
            "CVB::testtoken123::ANCHOR n=1\n",
            "placeholder\n",
            "CVB::testtoken123::NEW n=1\n",
            "inserted line\n",
            "CVB::testtoken123::ENDOP\n",
            "CVB::testtoken123::END ops=1",
        );

        match parse_load_plan(&root, input) {
            Err(PipelineError::Plan(ref failure))
                if failure.kind == super::PlanError::InvalidFile => {}
            other => panic!("expected InvalidFile planning error for empty target, got {other:?}"),
        }

        assert_eq!(
            fs::read(&file).unwrap(),
            original,
            "rejecting insertion into an empty file must preserve its zero-byte contents"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verify_envelope_token_accepts_matching_session_token() {
        let input = "CVB::session123::BEGIN v=1 id=test\nCVB::session123::END ops=0";
        assert!(verify_envelope_token(input, "session123").is_ok());
    }

    #[test]
    fn verify_envelope_token_rejects_mismatched_token() {
        let input = "CVB::other456::BEGIN v=1 id=test\nCVB::other456::END ops=0";
        let err = verify_envelope_token(input, "session123").expect_err("should reject mismatched token");
        assert_eq!(
            err,
            "CVB: E-TOKEN: envelope token 'other456' does not match active session token 'session123'; run `cvb token` to print the current token."
        );
    }

    #[test]
    fn verify_envelope_token_rejects_before_parsing_even_if_malformed_syntax() {
        let malformed_input = "CVB::wrong_tok::BEGIN broken syntax\n!!!NOT VALID CSL!!!";
        let err = verify_envelope_token(malformed_input, "session123").expect_err("should reject token before syntax parsing");
        assert!(err.starts_with("CVB: E-TOKEN: envelope token 'wrong_tok' does not match active session token 'session123'"));
    }

    #[test]
    fn verify_envelope_token_ignores_input_without_begin() {
        let no_begin = "random text without begin line";
        assert!(verify_envelope_token(no_begin, "session123").is_ok());
    }
}

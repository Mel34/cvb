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

#[derive(Debug, PartialEq, Eq)]
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
            }) => {
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
                if let Some(side) = prefix_ended {
                    match side {
                        PrefixSide::ExpectedEnds => write!(f, "\n  note: expected ends here")?,
                        PrefixSide::FoundEnds => write!(f, "\n  note: found ends here")?,
                    }
                }
                if *only_trailing_whitespace {
                    write!(f, "\n  note: differs only in trailing whitespace")?;
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
    NoOp,
}

impl PlanError {
    pub(crate) fn protocol_code(&self) -> &'static str {
        match self {
            PlanError::Precondition => "E-PRECOND",
            PlanError::EndOfFile => "E-EOF",
            PlanError::Overlap => "E-OVERLAP",
            PlanError::Range => "E-RANGE",
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

/// Runs the surgery CLI pipeline.
///
pub(crate) fn spec_text(token: &str) -> String {
    r#"CVB Surgery Language (CSL), protocol version 1

USAGE
  cvb surgery --spec       Print this specification with a fresh token.
  cvb surgery              Read one envelope from the clipboard.
  cvb surgery --stdin      Read one envelope from standard input.
  cvb surgery --root PATH  Select the project root.
  cvb surgery --dry-run    Validate and render the diff without applying it.

TOKEN
  Replace {TOKEN} below with the token printed for this specification.
  Use that exact token on every control line in one envelope.

ENVELOPE GRAMMAR
  CVB::{TOKEN}::BEGIN v=1 id=UNIQUE_ID
  CVB::{TOKEN}::FILE path=RELATIVE_PATH
  CVB::{TOKEN}::OP replace lines=START-END
  CVB::{TOKEN}::OLD [n=COUNT]
  ...exact original lines...
  CVB::{TOKEN}::NEW [n=COUNT]
  ...replacement lines...
  CVB::{TOKEN}::ENDOP
  CVB::{TOKEN}::END ops=OPERATION_COUNT

  Each operation belongs to a FILE section. Repeat OP blocks as needed.
  Replace may replace one or more lines. OLD must match the target exactly.
  delete uses OLD [n=COUNT] and the original lines, then ENDOP.
  insert uses ANCHOR [n=COUNT] and the original anchor lines, then
  NEW [n=COUNT] and the inserted lines, then ENDOP.
  Every control line must use the same token.
  Counts describe payload lines; do not include control lines.
  Line counts (n=COUNT) on OLD, ANCHOR, and NEW are opt-in per block.
  When n= is omitted, the block extends to the next CVB::{TOKEN}:: control line.
  Always use n=COUNT on blocks that end with a trailing blank line, because
  markdown code fences often drop trailing blank lines.
  Ranges are 1-based, inclusive, and refer to the original file.
  Even a single-line range uses START-END, for example lines=12-12.
  Insert anchors also use ranges: after=20-20 or before=20-20.
  Payload lines are literal file content, not CSL control lines.

OPERATION FORMS
  CVB::{TOKEN}::OP replace lines=START-END
  CVB::{TOKEN}::OP delete lines=START-END
  CVB::{TOKEN}::OP insert after=START-END
  CVB::{TOKEN}::OP insert before=START-END

EXAMPLE 8.1 — REPLACE
  CVB::{TOKEN}::BEGIN v=1 id=example-replace
  CVB::{TOKEN}::FILE path=src/example.rs
  CVB::{TOKEN}::OP replace lines=2-2
  CVB::{TOKEN}::OLD n=1
  let answer = 41;
  CVB::{TOKEN}::NEW n=1
  let answer = 42;
  CVB::{TOKEN}::ENDOP
  CVB::{TOKEN}::END ops=1

EXAMPLE 8.2 — DELETE
  CVB::{TOKEN}::BEGIN v=1 id=example-delete
  CVB::{TOKEN}::FILE path=src/example.rs
  CVB::{TOKEN}::OP delete lines=4-4
  CVB::{TOKEN}::OLD n=1
  // remove this obsolete comment
  CVB::{TOKEN}::ENDOP
  CVB::{TOKEN}::END ops=1

EXAMPLE 8.3 — INSERT
  CVB::{TOKEN}::BEGIN v=1 id=example-insert
  CVB::{TOKEN}::FILE path=src/example.rs
  CVB::{TOKEN}::OP insert after=2-2
  CVB::{TOKEN}::ANCHOR n=1
  let answer = 42;
  CVB::{TOKEN}::NEW n=1
  println!("{answer}");
  CVB::{TOKEN}::ENDOP
  CVB::{TOKEN}::END ops=1

VALIDATION AND SAFETY
  Submit exactly ONE envelope per paste or --stdin invocation.
  Multiple envelopes in one input are rejected. For separate changes,
  submit separate invocations and use a new unique id for each envelope.
  The id must be 1–64 ASCII letters, digits, '.', '_' or '-'.
  Tokens are ASCII alphanumeric; the generated token is 16 characters.
  Paths are project-root-relative and must identify existing eligible files.
  Unsafe paths, symlink targets, and protected targets such as .git are rejected.
  OLD and ANCHOR payloads must match the original file exactly.
  All operation ranges refer to the original file, not intermediate edits.
  Overlapping or conflicting edits are rejected.
  No-op replacements and deletion of an entire file are rejected.
  Insertions before the first line or after the last line are rejected;
  use a replacement that includes the boundary line instead.
  Replacement content must be non-empty.
  Review the rendered diff and confirm before changes are applied.
  --dry-run validates and renders without applying changes.

LIMITS
  Maximum envelope input: 3 MiB.
  Maximum individual target file: 8 MiB.
  Maximum aggregate target-file size: 32 MiB.
  Maximum operations per envelope: 500.

ERRORS
  E-LIMIT   Input exceeds the size limit.
  E-LOCK    Surgery lock could not be acquired.
  E-NOTTY   Interactive confirmation requires /dev/tty.
  E-REPLAY  This envelope id has already been applied to the project.
  E-RECOVER An unfinished transaction requires recovery.
  E-STALE   A target changed since the plan was created.
  E-IO      An I/O error occurred while committing changes.

  Parse and planning failures also report diagnostics; do not assume every
  validation failure has a dedicated E- code. Fix the reported issue and
  submit a corrected envelope with a unique id.

WORKFLOW
  1. Inspect the relevant source and construct one small envelope.
  2. Use the fresh token consistently and verify every OLD/ANCHOR block.
  3. Run cvb surgery --dry-run to inspect the proposed diff.
  4. Apply only after reviewing the diff and confirming the prompt.
  5. Send each subsequent envelope separately.
"#.lines()
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .replace("{TOKEN}", token)
}

/// Planning and diff rendering happen before confirmation. Persistent project
/// state is initialized only after the user confirms the proposed changes.
pub(crate) fn run(
    dry_run: bool,
    stdin: bool,
    root: Option<std::path::PathBuf>,
    spec: bool,
) -> ! {
    if spec {
        let token = match token::generate_token() {
            Ok(token) => token,
            Err(error) => {
                eprintln!("CVB: unable to generate surgery specification token: {error}");
                std::process::exit(2);
            }
        };
        println!("{}", spec_text(&token));
        std::process::exit(0);
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
    print!("{diff}");

    use std::io::Write as _;
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("CVB: unable to flush surgery diff: {error}");
        std::process::exit(2);
    }

    let confirmation = match prompt_confirmation(dry_run) {
        Ok(result) => result,
        Err((message, code)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    };

    if dry_run {
        std::process::exit(0);
    }

    if confirmation == ConfirmResult::Declined {
        std::process::exit(SurgeryExitStatus::UserDeclined.code());
    }

    let state_base = match resolve_state_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(SurgeryExitStatus::RejectedBeforeCommit.code());
        }
    };

    let state = match init_project_state(&state_base, &root) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(SurgeryExitStatus::RejectedBeforeCommit.code());
        }
    };

    match apply_plan(&state, &plan) {
        Ok(_) => std::process::exit(SurgeryExitStatus::Applied.code()),
        Err(error) => {
            let status = error.exit_status().code();
            eprintln!("{error}");
            std::process::exit(status);
        }
    }
}

/// Parses an envelope, reads its target files, and builds an in-memory plan.
/// This function never writes to disk.
pub(crate) fn parse_load_plan(root: &Path, input: &str) -> Result<(Envelope, Plan), PipelineError> {
    let envelope = parse_envelope(input).map_err(PipelineError::Parse)?;
    let originals = load_originals(root, &envelope).map_err(PipelineError::Load)?;
    let plan = plan_envelope(&envelope, &originals).map_err(PipelineError::Plan)?;

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
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn spec_text_interpolates_token_and_documents_protocol_limits_and_errors() {
        let token = "AbCd1234EfGh5678";
        let spec = spec_text(token);

        assert!(spec.lines().any(|line| line == format!("CVB::{token}::BEGIN v=1 id=example-replace")));
        assert!(spec.lines().any(|line| line == format!("CVB::{token}::OP replace lines=2-2")));
        assert!(!spec.lines().any(|line| line.starts_with("  CVB::")));

        assert!(spec.contains("CVB Surgery Language (CSL), protocol version 1"));
        assert!(spec.contains(&format!("CVB::{token}::BEGIN")));
        assert!(spec.contains(&format!("CVB::{token}::OP replace lines=START-END")));
        assert!(spec.contains(&format!("CVB::{token}::OP insert after=START-END")));
        assert!(spec.contains(&format!("CVB::{token}::OP insert before=START-END")));

        assert!(spec.contains("Maximum envelope input: 3 MiB."));
        assert!(spec.contains("Maximum individual target file: 8 MiB."));
        assert!(spec.contains("Maximum aggregate target-file size: 32 MiB."));
        assert!(spec.contains("Maximum operations per envelope: 500."));

        assert!(spec.contains("lines=12-12"));
        assert!(spec.contains("after=20-20"));
        assert!(spec.contains("before=20-20"));
        assert!(spec.contains("E-LIMIT"));
        assert!(spec.contains("E-LOCK"));
        assert!(spec.contains("E-NOTTY"));
        assert!(spec.contains("E-REPLAY"));
        assert!(spec.contains("E-RECOVER"));
        assert!(spec.contains("E-STALE"));
        assert!(spec.contains("E-IO"));

        assert!(spec.contains("EXAMPLE 8.1 — REPLACE"));
        assert!(spec.contains("EXAMPLE 8.2 — DELETE"));
        assert!(spec.contains("EXAMPLE 8.3 — INSERT"));
        assert!(spec.contains("exactly ONE envelope per paste"));
        assert!(spec.contains("Multiple envelopes in one input are rejected."));
        assert!(spec.contains("--dry-run"));
        assert!(spec.contains("opt-in per block"));
        assert!(spec.contains("trailing blank line"));
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
}

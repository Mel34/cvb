#![allow(dead_code)]

use std::path::{Component, Path};

pub(crate) mod load;
pub(crate) mod parse;
pub(crate) mod plan;

pub(crate) use load::*;
pub(crate) use parse::*;
pub(crate) use plan::*;

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
        if let Some(detail) = &self.detail {
            write!(
                f,
                "CVB: line {}: parse error ({:?}): {}",
                self.line, self.kind, detail
            )
        } else {
            write!(f, "CVB: line {}: parse error ({:?})", self.line, self.kind)
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
                    "CVB: {}: operation {}: precondition mismatch at line {}, column {} (byte offset {}):\n  expected: \"{}\"\n     found: \"{}\"",
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
                    "CVB: {}: operation {}: {suggestion}",
                    self.path, self.op_index
                )
            }
            Some(PlanDetail::Overlap { other_op_index }) => {
                write!(
                    f,
                    "CVB: {}: operation {} overlaps with operation {}",
                    self.path, self.op_index, other_op_index
                )
            }
            Some(PlanDetail::Range { file_lines }) => {
                write!(
                    f,
                    "CVB: {}: operation {}: line range out of bounds (file has {file_lines} lines)",
                    self.path, self.op_index
                )
            }
            Some(PlanDetail::Other(msg)) => {
                write!(
                    f,
                    "CVB: {}: operation {}: {:?}: {msg}",
                    self.path, self.op_index, self.kind
                )
            }
            None => {
                write!(
                    f,
                    "CVB: {}: operation {}: {:?}",
                    self.path, self.op_index, self.kind
                )
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
    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::UnsafePath(path) => write!(f, "CVB: unsafe target path '{path}'"),
            LoadError::DuplicatePath(path) => write!(f, "CVB: duplicate target path '{path}'"),
            LoadError::Inspect { path, message } => {
                write!(f, "CVB: unable to inspect '{path}': {message}")
            }
            LoadError::SymlinkTarget(path) => {
                write!(f, "CVB: symlink target is not allowed: '{path}'")
            }
            LoadError::NotRegularFile(path) => {
                write!(f, "CVB: target is not a regular file: '{path}'")
            }
            LoadError::FileTooLarge { path, size, limit } => {
                write!(
                    f,
                    "CVB: target file '{path}' is too large ({size} bytes, limit is {limit} bytes)"
                )
            }
            LoadError::AggregateTooLarge { size, limit } => {
                write!(
                    f,
                    "CVB: aggregate target file size ({size} bytes) exceeds limit ({limit} bytes)"
                )
            }
            LoadError::Read { path, message } => {
                write!(f, "CVB: unable to read '{path}': {message}")
            }
            LoadError::InvalidUtf8(path) => write!(f, "CVB: invalid UTF-8 in '{path}'"),
            LoadError::NulByte(path) => write!(f, "CVB: NUL byte is not allowed: '{path}'"),
            LoadError::Bom(path) => write!(f, "CVB: UTF-8 BOM is not allowed: '{path}'"),
            LoadError::LoneCr(path) => write!(f, "CVB: lone CR is not allowed: '{path}'"),
            LoadError::MixedLineEndings(path) => {
                write!(f, "CVB: mixed LF/CRLF line endings: '{path}'")
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

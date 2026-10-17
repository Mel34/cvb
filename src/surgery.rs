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
pub(crate) enum PlanDetail {
    Precondition {
        line: usize,
        expected: String,
        found: String,
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
    truncated.replace('\t', "\\t")
}

impl std::fmt::Display for PlanFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.detail {
            Some(PlanDetail::Precondition {
                line,
                expected,
                found,
                only_trailing_whitespace,
            }) => {
                write!(
                    f,
                    "CVB: {}: operation {}: precondition mismatch at line {}:\n  expected: \"{}\"\n     found: \"{}\"",
                    self.path,
                    self.op_index,
                    line,
                    format_visible_line(expected),
                    format_visible_line(found)
                )?;
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

pub(crate) fn valid_token(token: &str) -> bool {
    (8..=32).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn parse_count(value: &str) -> Result<usize, ParseError> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(ParseError::Syntax);
    }

    value.parse().map_err(|_| ParseError::Limit)
}

pub(crate) fn parse_range(value: &str) -> Result<(usize, usize), ParseError> {
    let (start, end) = value.split_once('-').ok_or(ParseError::Syntax)?;

    if end.contains('-') {
        return Err(ParseError::Syntax);
    }

    let start = parse_count(start)?;
    let end = parse_count(end)?;

    if start == 0 || end == 0 || start > end {
        return Err(ParseError::Range);
    }

    Ok((start, end))
}

/// Validates relative paths: rejects empty, backslashes, absolute paths,
/// non-normal components (`.` / `..`), and `.git` components (case-insensitive).
pub(crate) fn validate_relative_path(path: &str) -> Result<(), PlanError> {
    use std::path::{Component, Path};

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
                if let Some(s) = c.to_str() {
                    if s.eq_ignore_ascii_case(".git") {
                        return Err(PlanError::InvalidPath);
                    }
                }
            }
            _ => return Err(PlanError::InvalidPath),
        }
    }

    Ok(())
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

fn is_begin_shaped(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("CVB::") else {
        return false;
    };

    let Some((token, _suffix)) = rest.split_once("::BEGIN ") else {
        return false;
    };

    valid_token(token)
}

fn parse_begin(line: &str) -> Result<(String, String), ParseError> {
    let rest = line.strip_prefix("CVB::").ok_or(ParseError::Syntax)?;
    let (token, fields) = rest.split_once("::BEGIN ").ok_or(ParseError::Syntax)?;

    if !valid_token(token) || fields.is_empty() || fields.contains("::") {
        return Err(ParseError::Syntax);
    }

    let mut fields = fields.split(' ');

    let version = fields.next().ok_or(ParseError::Syntax)?;
    let id_field = fields.next().ok_or(ParseError::Syntax)?;

    if fields.next().is_some() {
        return Err(ParseError::Syntax);
    }

    let version_value = version.strip_prefix("v=").ok_or(ParseError::Syntax)?;
    let id = id_field.strip_prefix("id=").ok_or(ParseError::Syntax)?;

    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(ParseError::Syntax);
    }

    if version_value != "1" {
        return Err(ParseError::Version);
    }

    Ok((token.to_string(), id.to_string()))
}

fn parse_file(line: &str, token: &str) -> Result<String, ParseError> {
    let prefix = format!("CVB::{token}::FILE path=");
    let path = line.strip_prefix(&prefix).ok_or(ParseError::Syntax)?;

    validate_relative_path(path).map_err(|_| ParseError::InvalidPath)?;

    if !path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        return Err(ParseError::InvalidPath);
    }

    Ok(path.to_string())
}

struct Parser<'a> {
    lines: &'a [&'a str],
    index: usize,
    token: &'a str,
    prefix: String,
}

impl<'a> Parser<'a> {
    fn new(lines: &'a [&'a str], token: &'a str) -> Self {
        Self {
            lines,
            index: 0,
            token,
            prefix: format!("CVB::{token}::"),
        }
    }

    fn peek(&self) -> Option<&'a str> {
        self.lines.get(self.index).copied()
    }

    fn next_line(&mut self) -> Result<&'a str, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.lines.get(self.index).ok_or_else(|| ParseFailure {
            kind: ParseError::Truncated,
            line: line_num,
            detail: None,
        })?;
        self.index += 1;
        Ok(*line)
    }

    fn expect_header(&mut self, name: &str) -> Result<usize, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.next_line()?;
        let header_prefix = format!("{}{name} n=", self.prefix);
        let count_text = line
            .strip_prefix(&header_prefix)
            .ok_or_else(|| ParseFailure {
                kind: ParseError::Syntax,
                line: line_num,
                detail: None,
            })?;
        parse_count(count_text).map_err(|k| ParseFailure {
            kind: k,
            line: line_num,
            detail: None,
        })
    }

    fn consume_endop(&mut self) -> Result<(), ParseFailure> {
        let is_last_line = self.index + 1 == self.lines.len();
        let line_num = self.index + 1;
        let line = self.next_line()?;
        let endop = format!("{}ENDOP", self.prefix);
        if line != endop {
            return Err(ParseFailure {
                kind: if is_last_line {
                    ParseError::Truncated
                } else {
                    ParseError::Syntax
                },
                line: line_num,
                detail: None,
            });
        }
        Ok(())
    }

    fn read_payload(&mut self, count: usize) -> Result<Vec<String>, ParseFailure> {
        if count > self.lines.len().saturating_sub(self.index) {
            return Err(ParseFailure {
                kind: ParseError::Truncated,
                line: self.lines.len(),
                detail: None,
            });
        }

        let mut payload = Vec::with_capacity(count);

        for _ in 0..count {
            let line_num = self.index + 1;
            let line = self.lines.get(self.index).ok_or_else(|| ParseFailure {
                kind: ParseError::Truncated,
                line: self.lines.len(),
                detail: None,
            })?;

            if line.starts_with(&self.prefix) {
                return Err(ParseFailure {
                    kind: ParseError::Collision,
                    line: line_num,
                    detail: None,
                });
            }

            if line.contains('\0') || line.contains('\r') {
                return Err(ParseFailure {
                    kind: ParseError::InvalidBytes,
                    line: line_num,
                    detail: None,
                });
            }

            payload.push((*line).to_owned());
            self.index += 1;
        }

        Ok(payload)
    }

    fn parse_operation(&mut self) -> Result<Operation, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.peek().ok_or_else(|| ParseFailure {
            kind: ParseError::Truncated,
            line: line_num,
            detail: None,
        })?;
        let op_prefix = format!("{}OP ", self.prefix);
        if !line.starts_with(&op_prefix) {
            return Err(ParseFailure {
                kind: ParseError::Syntax,
                line: line_num,
                detail: None,
            });
        }

        if line.starts_with(&format!("{}OP replace ", self.prefix)) {
            self.parse_replace()
        } else if line.starts_with(&format!("{}OP delete ", self.prefix)) {
            self.parse_delete()
        } else if line.starts_with(&format!("{}OP insert ", self.prefix)) {
            self.parse_insert()
        } else {
            Err(ParseFailure {
                kind: ParseError::Syntax,
                line: line_num,
                detail: None,
            })
        }
    }

    fn parse_replace(&mut self) -> Result<Operation, ParseFailure> {
        let prefix = format!("{}OP replace lines=", self.prefix);
        let op_line = self.index + 1;
        let line = self.next_line()?;
        let range_text = line.strip_prefix(&prefix).ok_or_else(|| ParseFailure {
            kind: ParseError::Syntax,
            line: op_line,
            detail: None,
        })?;
        let (start, end) = parse_range(range_text).map_err(|k| ParseFailure {
            kind: k,
            line: op_line,
            detail: None,
        })?;

        let old_line = self.index + 1;
        let old_count = self.expect_header("OLD")?;
        let expected_old = end - start + 1;
        if old_count != expected_old {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: old_line,
                detail: Some(format!(
                    "declared count {old_count}, expected {expected_old}"
                )),
            });
        }

        let old = self.read_payload(old_count)?;

        let new_line = self.index + 1;
        let new_count = self.expect_header("NEW")?;
        if new_count == 0 {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: new_line,
                detail: Some("declared count 0, expected at least 1".into()),
            });
        }

        let new = self.read_payload(new_count)?;
        self.consume_endop()?;

        Ok(Operation::Replace {
            start,
            end,
            old,
            new,
        })
    }

    fn parse_delete(&mut self) -> Result<Operation, ParseFailure> {
        let prefix = format!("{}OP delete lines=", self.prefix);
        let op_line = self.index + 1;
        let line = self.next_line()?;
        let range_text = line.strip_prefix(&prefix).ok_or_else(|| ParseFailure {
            kind: ParseError::Syntax,
            line: op_line,
            detail: None,
        })?;
        let (start, end) = parse_range(range_text).map_err(|k| ParseFailure {
            kind: k,
            line: op_line,
            detail: None,
        })?;

        let old_line = self.index + 1;
        let count = self.expect_header("OLD")?;
        let expected_count = end - start + 1;
        if count != expected_count {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: old_line,
                detail: Some(format!("declared count {count}, expected {expected_count}")),
            });
        }

        let old = self.read_payload(count)?;
        self.consume_endop()?;

        Ok(Operation::Delete { start, end, old })
    }

    fn parse_insert(&mut self) -> Result<Operation, ParseFailure> {
        let op_line = self.index + 1;
        let line = self.next_line()?;
        let after_prefix = format!("{}OP insert after=", self.prefix);
        let before_prefix = format!("{}OP insert before=", self.prefix);

        let (position, range_text) = if let Some(value) = line.strip_prefix(&after_prefix) {
            (InsertPosition::After, value)
        } else if let Some(value) = line.strip_prefix(&before_prefix) {
            (InsertPosition::Before, value)
        } else {
            return Err(ParseFailure {
                kind: ParseError::Syntax,
                line: op_line,
                detail: None,
            });
        };

        let (start, end) = parse_range(range_text).map_err(|k| ParseFailure {
            kind: k,
            line: op_line,
            detail: None,
        })?;

        let anchor_line = self.index + 1;
        let count = self.expect_header("ANCHOR")?;
        let expected_count = end - start + 1;
        if count != expected_count {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: anchor_line,
                detail: Some(format!("declared count {count}, expected {expected_count}")),
            });
        }

        let anchor = self.read_payload(count)?;

        let new_line = self.index + 1;
        let new_count = self.expect_header("NEW")?;
        if new_count == 0 {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: new_line,
                detail: Some("declared count 0, expected at least 1".into()),
            });
        }

        let new = self.read_payload(new_count)?;
        self.consume_endop()?;

        Ok(Operation::Insert {
            position,
            start,
            end,
            anchor,
            new,
        })
    }
}

#[cfg(test)]
fn read_payload(
    lines: &[&str],
    index: &mut usize,
    token: &str,
    count: usize,
) -> Result<Vec<String>, ParseError> {
    let mut parser = Parser::new(lines, token);
    parser.index = *index;
    let res = parser.read_payload(count);
    *index = parser.index;
    res.map_err(|f| f.kind)
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

pub(crate) fn parse_envelope(input: &str) -> Result<Envelope, ParseFailure> {
    const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
    const MAX_OPERATIONS: usize = 500;

    if input.len() > MAX_INPUT_BYTES {
        return Err(ParseFailure {
            kind: ParseError::Limit,
            line: 1,
            detail: Some("input exceeds maximum size".into()),
        });
    }
    let normalized = input.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').collect();

    if lines.last() == Some(&"") {
        lines.pop();
    }

    let begin_index = lines
        .iter()
        .position(|line| is_begin_shaped(line))
        .ok_or_else(|| ParseFailure {
            kind: ParseError::NoEnvelope,
            line: lines.len(),
            detail: None,
        })?;

    let (token, id) = parse_begin(lines[begin_index]).map_err(|k| ParseFailure {
        kind: k,
        line: begin_index + 1,
        detail: None,
    })?;
    let mut parser = Parser::new(&lines, &token);
    parser.index = begin_index + 1;

    let mut files = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let mut operation_count = 0usize;

    while let Some(line) = parser.peek() {
        let end_prefix = format!("{}END ", parser.prefix);
        if line.starts_with(&end_prefix) {
            let ops_prefix = format!("{}END ops=", parser.prefix);
            let is_last_line = parser.index + 1 == parser.lines.len();
            let value = match line.strip_prefix(&ops_prefix) {
                Some(value) => value,
                None => {
                    return Err(ParseFailure {
                        kind: if is_last_line {
                            ParseError::Truncated
                        } else {
                            ParseError::Syntax
                        },
                        line: parser.index + 1,
                        detail: None,
                    });
                }
            };
            let declared_count = match parse_count(value) {
                Ok(count) => count,
                Err(_) if is_last_line => {
                    return Err(ParseFailure {
                        kind: ParseError::Truncated,
                        line: parser.index + 1,
                        detail: None,
                    });
                }
                Err(error) => {
                    return Err(ParseFailure {
                        kind: error,
                        line: parser.index + 1,
                        detail: None,
                    });
                }
            };

            if declared_count != operation_count || operation_count == 0 {
                return Err(ParseFailure {
                    kind: ParseError::Syntax,
                    line: parser.index + 1,
                    detail: Some(format!(
                        "declared count {declared_count} does not match actual operations {operation_count}"
                    )),
                });
            }

            if files.is_empty()
                || files
                    .iter()
                    .any(|file: &FileSection| file.operations.is_empty())
            {
                return Err(ParseFailure {
                    kind: ParseError::Syntax,
                    line: parser.index + 1,
                    detail: None,
                });
            }

            if parser.lines[parser.index + 1..]
                .iter()
                .any(|line| is_begin_shaped(line))
            {
                return Err(ParseFailure {
                    kind: ParseError::MultipleEnvelopes,
                    line: parser.index + 2,
                    detail: None,
                });
            }

            return Ok(Envelope { token, id, files });
        }

        let op_prefix = format!("{}OP ", parser.prefix);
        if line.starts_with(&op_prefix) {
            let op_line = parser.index + 1;
            let operation = parser.parse_operation()?;
            let file = files.last_mut().ok_or_else(|| ParseFailure {
                kind: ParseError::Syntax,
                line: op_line,
                detail: None,
            })?;
            file.operations.push(operation);
            operation_count += 1;
            if operation_count > MAX_OPERATIONS {
                return Err(ParseFailure {
                    kind: ParseError::Limit,
                    line: op_line,
                    detail: Some("exceeded 500 operations".into()),
                });
            }
            continue;
        }

        if line.starts_with("CVB::") {
            let file_line = parser.index + 1;
            let path = parse_file(line, &token).map_err(|k| ParseFailure {
                kind: k,
                line: file_line,
                detail: None,
            })?;

            if !seen_paths.insert(path.clone()) {
                return Err(ParseFailure {
                    kind: ParseError::DuplicateFile,
                    line: file_line,
                    detail: None,
                });
            }

            files.push(FileSection {
                path,
                operations: Vec::new(),
            });
            parser.index += 1;
            continue;
        }

        return Err(ParseFailure {
            kind: ParseError::Stray,
            line: parser.index + 1,
            detail: None,
        });
    }

    Err(ParseFailure {
        kind: ParseError::Truncated,
        line: parser.lines.len(),
        detail: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_requires_ascii_alphanumeric_and_length_8_to_32() {
        assert!(valid_token("k7Q2xw9R"));
        assert!(valid_token("A1234567"));
        assert!(valid_token("12345678901234567890123456789012"));

        assert!(!valid_token("short"));
        assert!(!valid_token("123456789012345678901234567890123"));
        assert!(!valid_token("abcd_efg"));
        assert!(!valid_token("abcd efgh"));
        assert!(!valid_token("é1234567"));
    }

    #[test]
    fn count_rejects_signs_whitespace_and_leading_zeroes() {
        assert_eq!(parse_count("0"), Ok(0));
        assert_eq!(parse_count("12"), Ok(12));

        for invalid in ["", "-1", "+1", "01", " 1", "1 ", "1.0"] {
            assert_eq!(parse_count(invalid), Err(ParseError::Syntax), "{invalid:?}");
        }
    }

    #[test]
    fn range_requires_two_ordered_positive_numbers() {
        assert_eq!(parse_range("10-10"), Ok((10, 10)));
        assert_eq!(parse_range("2-7"), Ok((2, 7)));

        assert_eq!(parse_range("0-1"), Err(ParseError::Range));
        assert_eq!(parse_range("4-3"), Err(ParseError::Range));

        for invalid in ["", "1", "1-2-3", "01-2", "1-02", "1 -2", "1- 2"] {
            assert_eq!(parse_range(invalid), Err(ParseError::Syntax), "{invalid:?}");
        }
    }

    #[test]
    fn parses_begin_header() {
        assert_eq!(
            parse_begin("CVB::k7Q2xw9R::BEGIN v=1 id=demo-01"),
            Ok(("k7Q2xw9R".to_string(), "demo-01".to_string()))
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        assert_eq!(
            parse_begin("CVB::k7Q2xw9R::BEGIN v=2 id=demo-01"),
            Err(ParseError::Version)
        );
    }

    #[test]
    fn rejects_malformed_begin_header() {
        for input in [
            "CVB::short::BEGIN v=1 id=x",
            "CVB::k7Q2xw9R::BEGIN v=1",
            "CVB::k7Q2xw9R::BEGIN id=x v=1",
            "CVB::k7Q2xw9R::BEGIN v=1 id=bad/id",
            " CVB::k7Q2xw9R::BEGIN v=1 id=x",
        ] {
            assert_eq!(parse_begin(input), Err(ParseError::Syntax), "{input:?}");
        }
    }

    #[test]
    fn finds_begin_after_leading_prose_but_rejects_incomplete_envelope() {
        let input = concat!(
            "Here is the change:\n",
            "CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\n",
            "CVB::k7Q2xw9R::FILE path=src/main.rs\n",
        );

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn malformed_begin_header_is_syntax_not_no_envelope() {
        assert_eq!(
            kind(parse_envelope("CVB::k7Q2xw9R::BEGIN \n")),
            Err(ParseError::Syntax)
        );
    }

    #[test]
    fn rejects_lone_carriage_return_in_payload() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old\rtext
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::InvalidBytes));
    }

    #[test]
    fn rejects_huge_new_count_in_replace_without_panicking() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=18446744073709551615
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn rejects_huge_old_count_in_replace_without_panicking() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-18446744073709551615
CVB::k7Q2xw9R::OLD n=18446744073709551615
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn rejects_huge_old_count_in_delete_without_panicking() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP delete lines=1-18446744073709551615
CVB::k7Q2xw9R::OLD n=18446744073709551615
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn rejects_huge_anchor_count_in_insert_without_panicking() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP insert after=1-18446744073709551615
CVB::k7Q2xw9R::ANCHOR n=18446744073709551615
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn rejects_count_exceeding_remaining_lines_as_truncated() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=5
line 1
line 2";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn other_token_structural_prefix_is_ordinary_payload() {
        let lines = ["CVB::A1B2C3D4::FIELD"];
        let mut index = 0;

        assert_eq!(
            read_payload(&lines, &mut index, "k7Q2xw9R", 1),
            Ok(vec!["CVB::A1B2C3D4::FIELD".to_string()])
        );
        assert_eq!(index, 1);
    }

    #[test]
    fn payload_collision_precedes_invalid_bytes() {
        let token = "k7Q2xw9R";

        for payload_line in ["CVB::k7Q2xw9R::FIELD\0", "CVB::k7Q2xw9R::FIELD\r"] {
            let lines = [payload_line];
            let mut index = 0;

            assert_eq!(
                read_payload(&lines, &mut index, token, 1),
                Err(ParseError::Collision),
                "{payload_line:?}"
            );
        }
    }

    #[test]
    fn parses_crlf_envelope() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\r
CVB::k7Q2xw9R::FILE path=src/main.rs\r
CVB::k7Q2xw9R::OP replace lines=1-1\r
CVB::k7Q2xw9R::OLD n=1\r
old\r
CVB::k7Q2xw9R::NEW n=1\r
new\r
CVB::k7Q2xw9R::ENDOP\r
CVB::k7Q2xw9R::END ops=1";

        assert!(parse_envelope(input).is_ok());
    }

    #[test]
    fn rejects_duplicate_file_paths() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=2-2
CVB::k7Q2xw9R::OLD n=1
old2
CVB::k7Q2xw9R::NEW n=1
new2
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=2";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::DuplicateFile));
    }

    #[test]
    fn rejects_input_over_four_mebibytes() {
        let input = "x".repeat(4 * 1024 * 1024 + 1);
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::Limit));
    }

    #[test]
    fn input_at_limit_passes_size_check() {
        let mut input = "x".repeat(4 * 1024 * 1024);
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::NoEnvelope));
        input.push('x');
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::Limit));
    }

    #[test]
    fn rejects_more_than_five_hundred_operations() {
        let mut input = String::from(
            "CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\n\
             CVB::k7Q2xw9R::FILE path=src/main.rs\n",
        );

        for _ in 0..501 {
            input.push_str(
                "CVB::k7Q2xw9R::OP replace lines=1-1\n\
                 CVB::k7Q2xw9R::OLD n=1\n\
                 old\n\
                 CVB::k7Q2xw9R::NEW n=1\n\
                 new\n\
                 CVB::k7Q2xw9R::ENDOP\n",
            );
        }

        input.push_str("CVB::k7Q2xw9R::END ops=501");
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::Limit));
    }

    #[test]
    fn rejects_git_components_in_relative_file_paths() {
        for path in [
            ".git",
            ".git/hooks/pre-commit",
            ".git/config",
            "sub/.git/hooks/x",
            ".GIT/hooks/x",
        ] {
            let line = format!("CVB::k7Q2xw9R::FILE path={path}");
            assert_eq!(
                parse_file(&line, "k7Q2xw9R"),
                Err(ParseError::InvalidPath),
                "{path:?}"
            );
        }
    }

    #[test]
    fn accepts_git_lookalikes_in_relative_file_paths() {
        for path in [
            ".gitignore",
            ".gitattributes",
            ".github/workflows/ci.yml",
            "src/git.rs",
        ] {
            let line = format!("CVB::k7Q2xw9R::FILE path={path}");
            assert_eq!(
                parse_file(&line, "k7Q2xw9R"),
                Ok(path.to_string()),
                "{path:?}"
            );
        }
    }

    #[test]
    fn detect_newline_distinguishes_lf_crlf_mixed_and_lone_cr() {
        assert_eq!(detect_newline("hello\nworld\n"), Ok(Newline::Lf));
        assert_eq!(detect_newline("hello\r\nworld\r\n"), Ok(Newline::Crlf));
        assert_eq!(detect_newline("hello\nworld\r\n"), Err(NewlineError::Mixed));
        assert_eq!(detect_newline("hello\rworld\n"), Err(NewlineError::LoneCr));
        assert_eq!(detect_newline("no newlines"), Ok(Newline::Lf));
    }

    #[test]
    fn preserves_blank_lines_inside_payload() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-3
CVB::k7Q2xw9R::OLD n=3
first

third
CVB::k7Q2xw9R::NEW n=1
replacement
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        let parsed = parse_envelope(input).unwrap();
        assert_eq!(
            parsed.files[0].operations,
            vec![Operation::Replace {
                start: 1,
                end: 3,
                old: vec!["first".into(), "".into(), "third".into()],
                new: vec!["replacement".into()],
            }]
        );
    }

    #[test]
    fn ignores_extra_blank_lines_after_envelope() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1

";

        assert!(parse_envelope(input).is_ok());
    }

    #[test]
    fn preserves_blank_payload_with_trailing_envelope_newline() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-3
CVB::k7Q2xw9R::OLD n=3
first

third
CVB::k7Q2xw9R::NEW n=1
replacement
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1
";

        let parsed = parse_envelope(input).unwrap();
        assert_eq!(
            parsed.files[0].operations,
            vec![Operation::Replace {
                start: 1,
                end: 3,
                old: vec!["first".into(), "".into(), "third".into()],
                new: vec!["replacement".into()],
            }]
        );
    }

    #[test]
    fn parses_delete_operation() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP delete lines=2-3
CVB::k7Q2xw9R::OLD n=2
old one
old two
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        let parsed = parse_envelope(input).unwrap();
        assert_eq!(
            parsed.files[0].operations,
            vec![Operation::Delete {
                start: 2,
                end: 3,
                old: vec!["old one".into(), "old two".into()],
            }]
        );
    }

    #[test]
    fn parses_insert_before_operation() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP insert before=4-4
CVB::k7Q2xw9R::ANCHOR n=1
anchor
CVB::k7Q2xw9R::NEW n=2
new one
new two
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        let parsed = parse_envelope(input).unwrap();
        assert_eq!(
            parsed.files[0].operations,
            vec![Operation::Insert {
                position: InsertPosition::Before,
                start: 4,
                end: 4,
                anchor: vec!["anchor".into()],
                new: vec!["new one".into(), "new two".into()],
            }]
        );
    }

    #[test]
    fn parses_insert_after_operation() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP insert after=1-2
CVB::k7Q2xw9R::ANCHOR n=2
anchor one
anchor two
CVB::k7Q2xw9R::NEW n=1
inserted
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";

        let parsed = parse_envelope(input).unwrap();
        assert_eq!(
            parsed.files[0].operations,
            vec![Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 2,
                anchor: vec!["anchor one".into(), "anchor two".into()],
                new: vec!["inserted".into()],
            }]
        );
    }

    #[test]
    fn rejects_input_without_begin() {
        assert_eq!(
            kind(parse_envelope("nothing here\n")),
            Err(ParseError::NoEnvelope)
        );
    }

    #[test]
    fn rejects_malformed_second_envelope_as_multiple_envelopes() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1
CVB::A1B2C3D4::BEGIN nonsense";

        assert_eq!(
            kind(parse_envelope(input)),
            Err(ParseError::MultipleEnvelopes)
        );
    }

    #[test]
    fn malformed_final_end_line_is_truncated() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=
";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn malformed_end_line_before_more_input_is_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=
extra";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Syntax));
    }

    #[test]
    fn rejects_mismatched_declared_operation_count_as_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=2";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Syntax));
    }

    #[test]
    fn malformed_final_endop_is_truncated() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::NOT_ENDOP";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Truncated));
    }

    #[test]
    fn rejects_wrong_endop_as_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::NOT_ENDOP
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Syntax));
    }

    #[test]
    fn rejects_zero_declared_operations_as_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::END ops=0";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Syntax));
    }

    #[test]
    fn accepts_valid_relative_file_paths() {
        for path in ["src/main.rs", ".config/cvb.toml", "tests/unit-test.rs"] {
            let line = format!("CVB::k7Q2xw9R::FILE path={path}");
            assert_eq!(
                parse_file(&line, "k7Q2xw9R"),
                Ok(path.to_string()),
                "{path:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_relative_file_paths() {
        for path in [
            "",
            "/etc/passwd",
            "../secret",
            "src/../secret",
            "src/./main.rs",
            "src//main.rs",
            "src/",
            "./main.rs",
            "a\\b",
            "path with spaces",
            "é.rs",
        ] {
            let line = format!("CVB::k7Q2xw9R::FILE path={path}");
            assert_eq!(
                parse_file(&line, "k7Q2xw9R"),
                Err(ParseError::InvalidPath),
                "{path:?}"
            );
        }
    }
}

/// Reads the existing target files without modifying them.
///
/// Paths must be safe relative paths. Symlinks are rejected so a target cannot
/// redirect a read outside the supplied root.
pub(crate) fn load_originals(
    root: &std::path::Path,
    envelope: &Envelope,
) -> Result<std::collections::HashMap<String, String>, LoadError> {
    use std::path::{Component, Path};

    let mut originals = std::collections::HashMap::new();

    for section in &envelope.files {
        if validate_relative_path(&section.path).is_err() {
            return Err(LoadError::UnsafePath(section.path.clone()));
        }
        let relative = Path::new(&section.path);

        if originals.contains_key(&section.path) {
            return Err(LoadError::DuplicatePath(section.path.clone()));
        }

        let mut current = root.to_path_buf();
        for component in relative.components() {
            current.push(component.as_os_str());
            let metadata =
                std::fs::symlink_metadata(&current).map_err(|error| LoadError::Inspect {
                    path: current.display().to_string(),
                    message: error.to_string(),
                })?;

            if metadata.file_type().is_symlink() {
                return Err(LoadError::SymlinkTarget(current.display().to_string()));
            }
        }

        let metadata = std::fs::metadata(&current).map_err(|error| LoadError::Inspect {
            path: current.display().to_string(),
            message: error.to_string(),
        })?;
        if !metadata.is_file() {
            return Err(LoadError::NotRegularFile(current.display().to_string()));
        }

        let bytes = std::fs::read(&current).map_err(|error| LoadError::Read {
            path: current.display().to_string(),
            message: error.to_string(),
        })?;

        let contents = String::from_utf8(bytes)
            .map_err(|_| LoadError::InvalidUtf8(current.display().to_string()))?;

        if contents.contains('\0') {
            return Err(LoadError::NulByte(current.display().to_string()));
        }

        if contents.starts_with('\u{FEFF}') {
            return Err(LoadError::Bom(current.display().to_string()));
        }

        match detect_newline(&contents) {
            Err(NewlineError::LoneCr) => {
                return Err(LoadError::LoneCr(current.display().to_string()));
            }
            Err(NewlineError::Mixed) => {
                return Err(LoadError::MixedLineEndings(current.display().to_string()));
            }
            Ok(_) => {}
        }

        originals.insert(section.path.clone(), contents);
    }

    Ok(originals)
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
pub(crate) fn parse_load_plan(
    root: &std::path::Path,
    input: &str,
) -> Result<(Envelope, Plan), PipelineError> {
    let envelope = parse_envelope(input).map_err(PipelineError::Parse)?;
    let originals = load_originals(root, &envelope).map_err(PipelineError::Load)?;
    let plan = plan_envelope(&envelope, &originals).map_err(PipelineError::Plan)?;

    Ok((envelope, plan))
}

/// A fully validated proposal. Creating a plan never writes to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub token: String,
    pub id: String,
    pub files: Vec<PlannedFile>,
}

/// Original and proposed contents for one existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedFile {
    pub path: String,
    pub original: String,
    pub proposed: String,
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

/// Applies one operation to an original file represented as lines.
/// All operation ranges are one-based and refer to the original lines.
pub(crate) fn validate_operation(
    original: &[String],
    operation: &Operation,
) -> Result<(), PlanError> {
    validate_operation_detail(original, operation).map_err(|(err, _)| err)
}

pub(crate) fn validate_operation_detail(
    original: &[String],
    operation: &Operation,
) -> Result<(), (PlanError, PlanDetail)> {
    if original.is_empty() {
        return Err((
            PlanError::InvalidFile,
            PlanDetail::Other("target file is empty".into()),
        ));
    }

    match operation {
        Operation::Replace {
            start,
            end,
            old,
            new,
        } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err((
                    PlanError::Range,
                    PlanDetail::Range {
                        file_lines: original.len(),
                    },
                ));
            }
            if new.is_empty() {
                return Err((
                    PlanError::InvalidFile,
                    PlanDetail::Other("new payload cannot be empty".into()),
                ));
            }

            let first = start - 1;
            let last = *end;

            if original[first..last] != old[..] {
                let mut mismatch_line = *start;
                let mut exp = String::new();
                let mut fnd = String::new();
                for (k, (orig_l, old_l)) in original[first..last].iter().zip(old.iter()).enumerate()
                {
                    if orig_l != old_l {
                        mismatch_line = start + k;
                        exp = old_l.clone();
                        fnd = orig_l.clone();
                        break;
                    }
                }
                let only_ws = exp.trim_end() == fnd.trim_end();
                return Err((
                    PlanError::Precondition,
                    PlanDetail::Precondition {
                        line: mismatch_line,
                        expected: exp,
                        found: fnd,
                        only_trailing_whitespace: only_ws,
                    },
                ));
            }
            if old == new {
                return Err((
                    PlanError::NoOp,
                    PlanDetail::Other("replacement is identical to original".into()),
                ));
            }
        }

        Operation::Delete { start, end, old } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err((
                    PlanError::Range,
                    PlanDetail::Range {
                        file_lines: original.len(),
                    },
                ));
            }

            let first = start - 1;
            let last = *end;

            if original[first..last] != old[..] {
                let mut mismatch_line = *start;
                let mut exp = String::new();
                let mut fnd = String::new();
                for (k, (orig_l, old_l)) in original[first..last].iter().zip(old.iter()).enumerate()
                {
                    if orig_l != old_l {
                        mismatch_line = start + k;
                        exp = old_l.clone();
                        fnd = orig_l.clone();
                        break;
                    }
                }
                let only_ws = exp.trim_end() == fnd.trim_end();
                return Err((
                    PlanError::Precondition,
                    PlanDetail::Precondition {
                        line: mismatch_line,
                        expected: exp,
                        found: fnd,
                        only_trailing_whitespace: only_ws,
                    },
                ));
            }
            if first == 0 && last == original.len() {
                return Err((
                    PlanError::EndOfFile,
                    PlanDetail::EndOfFile {
                        suggestion:
                            "cannot delete the entire file; cvb surgery modifies existing files"
                                .into(),
                    },
                ));
            }
        }

        Operation::Insert {
            position,
            start,
            end,
            anchor,
            new,
        } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err((
                    PlanError::Range,
                    PlanDetail::Range {
                        file_lines: original.len(),
                    },
                ));
            }
            if new.is_empty() {
                return Err((
                    PlanError::InvalidFile,
                    PlanDetail::Other("inserted text cannot be empty".into()),
                ));
            }

            let first = start - 1;
            let last = *end;

            if (matches!(position, InsertPosition::Before) && first == 0)
                || (matches!(position, InsertPosition::After) && last == original.len())
            {
                return Err((PlanError::EndOfFile, PlanDetail::EndOfFile { suggestion: "cannot insert before first line or after last line; include the boundary line in a replace operation rather than inserting before/after it".into() }));
            }

            if original[first..last] != anchor[..] {
                let mut mismatch_line = *start;
                let mut exp = String::new();
                let mut fnd = String::new();
                for (k, (orig_l, anc_l)) in
                    original[first..last].iter().zip(anchor.iter()).enumerate()
                {
                    if orig_l != anc_l {
                        mismatch_line = start + k;
                        exp = anc_l.clone();
                        fnd = orig_l.clone();
                        break;
                    }
                }
                let only_ws = exp.trim_end() == fnd.trim_end();
                return Err((
                    PlanError::Precondition,
                    PlanDetail::Precondition {
                        line: mismatch_line,
                        expected: exp,
                        found: fnd,
                        only_trailing_whitespace: only_ws,
                    },
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod validate_operation_tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn make_test_envelope(operations: Vec<Operation>) -> Envelope {
        Envelope {
            token: "TOKEN123".to_owned(),
            id: "val-test".to_owned(),
            files: vec![FileSection {
                path: "example.txt".to_owned(),
                operations,
            }],
        }
    }

    fn make_test_originals(contents: &str) -> std::collections::HashMap<String, String> {
        [("example.txt".to_owned(), contents.to_owned())]
            .into_iter()
            .collect()
    }

    #[test]
    fn applies_replacement_in_memory() {
        let envelope = make_test_envelope(vec![Operation::Replace {
            start: 2,
            end: 2,
            old: vec!["old".to_owned()],
            new: vec!["new".to_owned()],
        }]);
        let plan = plan_envelope(&envelope, &make_test_originals("one\nold\nthree\n")).unwrap();
        assert_eq!(plan.files[0].proposed, "one\nnew\nthree\n");
    }

    #[test]
    fn applies_deletion_in_memory() {
        let envelope = make_test_envelope(vec![Operation::Delete {
            start: 2,
            end: 2,
            old: vec!["remove".to_owned()],
        }]);
        let plan = plan_envelope(&envelope, &make_test_originals("one\nremove\nthree\n")).unwrap();
        assert_eq!(plan.files[0].proposed, "one\nthree\n");
    }

    #[test]
    fn inserts_before_anchor_in_memory() {
        let envelope = make_test_envelope(vec![Operation::Insert {
            position: InsertPosition::Before,
            start: 2,
            end: 2,
            anchor: vec!["anchor".to_owned()],
            new: vec!["inserted".to_owned()],
        }]);
        let plan = plan_envelope(&envelope, &make_test_originals("one\nanchor\nthree\n")).unwrap();
        assert_eq!(plan.files[0].proposed, "one\ninserted\nanchor\nthree\n");
    }

    #[test]
    fn inserts_after_anchor_in_memory() {
        let envelope = make_test_envelope(vec![Operation::Insert {
            position: InsertPosition::After,
            start: 2,
            end: 2,
            anchor: vec!["anchor".to_owned()],
            new: vec!["inserted".to_owned()],
        }]);
        let plan = plan_envelope(&envelope, &make_test_originals("one\nanchor\nthree\n")).unwrap();
        assert_eq!(plan.files[0].proposed, "one\nanchor\ninserted\nthree\n");
    }

    #[test]
    fn rejects_replacement_when_original_text_does_not_match() {
        let original = lines(&["one", "actual", "three"]);
        let operation = Operation::Replace {
            start: 2,
            end: 2,
            old: lines(&["expected"]),
            new: lines(&["replacement"]),
        };

        assert_eq!(
            validate_operation(&original, &operation),
            Err(PlanError::Precondition)
        );
    }

    #[test]
    fn rejects_invalid_operation_ranges() {
        let original = lines(&["one", "two"]);

        let zero_start = Operation::Delete {
            start: 0,
            end: 1,
            old: lines(&["one"]),
        };
        assert_eq!(
            validate_operation(&original, &zero_start),
            Err(PlanError::Range)
        );

        let past_end = Operation::Delete {
            start: 2,
            end: 3,
            old: lines(&["two", "three"]),
        };
        assert_eq!(
            validate_operation(&original, &past_end),
            Err(PlanError::Range)
        );
    }

    #[test]
    fn rejects_no_op_replacement() {
        let original = lines(&["one", "unchanged", "three"]);
        let operation = Operation::Replace {
            start: 2,
            end: 2,
            old: lines(&["unchanged"]),
            new: lines(&["unchanged"]),
        };

        assert_eq!(
            validate_operation(&original, &operation),
            Err(PlanError::NoOp)
        );
    }

    #[test]
    fn rejects_deleting_the_entire_file() {
        let original = lines(&["one", "two"]);
        let operation = Operation::Delete {
            start: 1,
            end: 2,
            old: lines(&["one", "two"]),
        };

        assert_eq!(
            validate_operation(&original, &operation),
            Err(PlanError::EndOfFile)
        );
    }

    #[test]
    fn rejects_insertion_before_first_line_with_mismatched_anchor_as_end_of_file() {
        let original = lines(&["one", "two", "three"]);
        let operation = Operation::Insert {
            position: InsertPosition::Before,
            start: 1,
            end: 1,
            anchor: lines(&["mismatched"]),
            new: lines(&["inserted"]),
        };
        assert_eq!(
            validate_operation(&original, &operation),
            Err(PlanError::EndOfFile)
        );
    }
}

/// Builds a complete in-memory plan without writing to disk.
/// All operation ranges refer to the original contents of each file.
pub(crate) fn plan_envelope(
    envelope: &Envelope,
    originals: &std::collections::HashMap<String, String>,
) -> Result<Plan, PlanFailure> {
    use std::path::{Component, Path};

    let mut seen = std::collections::HashSet::new();
    let mut planned_files = Vec::with_capacity(envelope.files.len());

    for section in &envelope.files {
        if validate_relative_path(&section.path).is_err() {
            return Err(PlanFailure {
                kind: PlanError::InvalidPath,
                path: section.path.clone(),
                op_index: 0,
                detail: None,
            });
        }

        if !seen.insert(section.path.as_str()) {
            return Err(PlanFailure {
                kind: PlanError::DuplicateFile,
                path: section.path.clone(),
                op_index: 0,
                detail: None,
            });
        }

        let original = originals.get(&section.path).ok_or_else(|| PlanFailure {
            kind: PlanError::MissingFile,
            path: section.path.clone(),
            op_index: 0,
            detail: None,
        })?;

        if original.is_empty() {
            return Err(PlanFailure {
                kind: PlanError::InvalidFile,
                path: section.path.clone(),
                op_index: 0,
                detail: Some(PlanDetail::Other("target file is empty".into())),
            });
        }

        let newline_kind = detect_newline(original).map_err(|_| PlanFailure {
            kind: PlanError::InvalidFile,
            path: section.path.clone(),
            op_index: 0,
            detail: Some(PlanDetail::Other("invalid line endings".into())),
        })?;
        let newline = newline_kind.as_str();
        let has_final_newline = original.ends_with(newline);
        let content = if has_final_newline {
            original.strip_suffix(newline).unwrap()
        } else {
            original.as_str()
        };

        let original_lines: Vec<String> = content.split(newline).map(str::to_owned).collect();

        let mut edits: Vec<(usize, usize, &Operation)> = Vec::new();

        for (op_idx, operation) in section.operations.iter().enumerate() {
            let op_number = op_idx + 1;
            validate_operation_detail(&original_lines, operation).map_err(|(k, d)| {
                PlanFailure {
                    kind: k,
                    path: section.path.clone(),
                    op_index: op_number,
                    detail: Some(d),
                }
            })?;

            let span = match operation {
                Operation::Replace { start, end, .. } | Operation::Delete { start, end, .. } => {
                    (*start - 1, *end)
                }
                Operation::Insert {
                    position: InsertPosition::Before,
                    start,
                    ..
                } => (*start - 1, *start - 1),
                Operation::Insert {
                    position: InsertPosition::After,
                    end,
                    ..
                } => (*end, *end),
            };

            edits.push((span.0, span.1, operation));
        }

        for i in 0..edits.len() {
            for j in (i + 1)..edits.len() {
                let (a_start, a_end, a_operation) = edits[i];
                let (b_start, b_end, b_operation) = edits[j];

                let a_insert = a_start == a_end;
                let b_insert = b_start == b_end;

                let anchor_overlaps = |insert: &Operation, edit: &Operation| {
                    let (anchor_start, anchor_end) = match insert {
                        Operation::Insert { start, end, .. } => (*start - 1, *end),
                        _ => return false,
                    };

                    let (edit_start, edit_end) = match edit {
                        Operation::Replace { start, end, .. }
                        | Operation::Delete { start, end, .. } => (*start - 1, *end),
                        _ => return false,
                    };

                    anchor_start < edit_end && edit_start < anchor_end
                };

                if (a_insert && anchor_overlaps(a_operation, b_operation))
                    || (b_insert && anchor_overlaps(b_operation, a_operation))
                {
                    return Err(PlanFailure {
                        kind: PlanError::Overlap,
                        path: section.path.clone(),
                        op_index: i + 1,
                        detail: Some(PlanDetail::Overlap {
                            other_op_index: j + 1,
                        }),
                    });
                }

                let overlaps = match (a_insert, b_insert) {
                    (true, true) => a_start == b_start,
                    (true, false) => a_start > b_start && a_start < b_end,
                    (false, true) => b_start > a_start && b_start < a_end,
                    (false, false) => a_start < b_end && b_start < a_end,
                };

                if overlaps {
                    return Err(PlanFailure {
                        kind: PlanError::Overlap,
                        path: section.path.clone(),
                        op_index: i + 1,
                        detail: Some(PlanDetail::Overlap {
                            other_op_index: j + 1,
                        }),
                    });
                }
            }
        }

        edits.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| {
                let a_insert = a.0 == a.1;
                let b_insert = b.0 == b.1;
                a_insert.cmp(&b_insert)
            })
        });

        let mut proposed_lines = original_lines;
        for (_, _, operation) in edits {
            match operation {
                Operation::Replace {
                    start, end, new, ..
                } => {
                    proposed_lines.splice(start - 1..*end, new.iter().cloned());
                }
                Operation::Delete { start, end, .. } => {
                    proposed_lines.drain(start - 1..*end);
                }
                Operation::Insert {
                    position,
                    start,
                    end,
                    new,
                    ..
                } => {
                    let index = match position {
                        InsertPosition::Before => start - 1,
                        InsertPosition::After => *end,
                    };
                    proposed_lines.splice(index..index, new.iter().cloned());
                }
            }
        }

        let mut proposed = proposed_lines.join(newline);
        if has_final_newline {
            proposed.push_str(newline);
        }

        if proposed == *original {
            return Err(PlanFailure {
                kind: PlanError::NoOp,
                path: section.path.clone(),
                op_index: 0,
                detail: Some(PlanDetail::Other("net file change is a no-op".into())),
            });
        }

        planned_files.push(PlannedFile {
            path: section.path.clone(),
            original: original.clone(),
            proposed,
        });
    }

    Ok(Plan {
        token: envelope.token.clone(),
        id: envelope.id.clone(),
        files: planned_files,
    })
}

#[cfg(test)]
mod plan_envelope_tests {
    use super::*;

    fn make_envelope(operations: Vec<Operation>) -> Envelope {
        Envelope {
            token: "TOKEN123".to_owned(),
            id: "plan-test".to_owned(),
            files: vec![FileSection {
                path: "example.txt".to_owned(),
                operations,
            }],
        }
    }

    fn originals(contents: &str) -> std::collections::HashMap<String, String> {
        [("example.txt".to_owned(), contents.to_owned())]
            .into_iter()
            .collect()
    }

    #[test]
    fn plans_large_file_with_many_operations_quickly() {
        const TOTAL_LINES: usize = 50_000;
        const OP_COUNT: usize = 500;

        let mut lines = Vec::with_capacity(TOTAL_LINES);
        for i in 1..=TOTAL_LINES {
            lines.push(format!("line {i}"));
        }
        let original_content = lines.join("\n") + "\n";

        let mut operations = Vec::with_capacity(OP_COUNT);
        for i in 0..OP_COUNT {
            let line_num = i * 50 + 2;
            operations.push(Operation::Replace {
                start: line_num,
                end: line_num,
                old: vec![format!("line {line_num}")],
                new: vec![format!("replaced {line_num}")],
            });
        }

        let envelope = make_envelope(operations);
        let original = originals(&original_content);

        let plan = plan_envelope(&envelope, &original).unwrap();
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "example.txt");
    }

    #[test]
    fn precondition_error_message_shows_path_op_and_mismatch() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 2,
            end: 2,
            old: vec!["wrong".to_owned()],
            new: vec!["replacement".to_owned()],
        }]);
        let original = originals("line1\nactual\nline3\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(message.contains("example.txt"), "{message}");
        assert!(message.contains("operation 1"), "{message}");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("expected: \"wrong\""), "{message}");
        assert!(message.contains("found: \"actual\""), "{message}");
    }

    #[test]
    fn precondition_error_message_notes_trailing_whitespace_only() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["line   ".to_owned()],
            new: vec!["new".to_owned()],
        }]);
        let original = originals("line\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(
            message.contains("differs only in trailing whitespace"),
            "{message}"
        );
    }

    #[test]
    fn end_of_file_error_message_suggests_replace() {
        let envelope = make_envelope(vec![Operation::Insert {
            position: InsertPosition::Before,
            start: 1,
            end: 1,
            anchor: vec!["first".to_owned()],
            new: vec!["inserted".to_owned()],
        }]);
        let original = originals("first\nsecond\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::EndOfFile);
        assert!(message.contains("replace"), "{message}");
    }

    #[test]
    fn overlap_error_message_names_both_operations() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 1,
                end: 2,
                old: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["combo".to_owned()],
            },
            Operation::Delete {
                start: 2,
                end: 2,
                old: vec!["two".to_owned()],
            },
        ]);
        let original = originals("one\ntwo\nthree\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Overlap);
        assert!(message.contains("operation 1"), "{message}");
        assert!(message.contains("operation 2"), "{message}");
    }

    #[test]
    fn range_error_message_shows_file_line_count() {
        let envelope = make_envelope(vec![Operation::Delete {
            start: 1,
            end: 10,
            old: vec!["x".to_owned()],
        }]);
        let original = originals("one\ntwo\nthree\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Range);
        assert!(message.contains("3 lines"), "{message}");
    }

    #[test]
    fn precondition_error_message_truncates_long_lines() {
        let long_str = "x".repeat(300);
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec![long_str],
            new: vec!["new".to_owned()],
        }]);
        let original = originals("actual\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert!(message.contains("..."), "{message}");
    }

    #[test]
    fn count_error_message_shows_line_and_counts() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::END ops=0";
        let failure = parse_envelope(input).unwrap_err();
        let message = failure.to_string();
        assert_eq!(failure.kind, ParseError::Syntax);
        assert!(message.contains("line 3"), "{message}");
    }

    #[test]
    fn plans_multiple_operations_against_original_lines() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 1,
                end: 1,
                old: vec!["one".to_owned()],
                new: vec!["ONE".to_owned()],
            },
            Operation::Replace {
                start: 3,
                end: 3,
                old: vec!["three".to_owned()],
                new: vec!["THREE".to_owned()],
            },
        ]);
        let original = originals("one\ntwo\nthree\n");

        let plan = plan_envelope(&envelope, &original).unwrap();

        assert_eq!(plan.files[0].original, "one\ntwo\nthree\n");
        assert_eq!(plan.files[0].proposed, "ONE\ntwo\nTHREE\n");
        assert_eq!(original["example.txt"], "one\ntwo\nthree\n");
    }

    #[test]
    fn rejects_insert_anchor_overlapping_replaced_lines() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 2,
                end: 2,
                old: vec!["two".to_owned()],
                new: vec!["TWO".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 2,
                anchor: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(
                &envelope,
                &originals("one\ntwo\nthree\nfour\n")
            )),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn rejects_insertion_after_final_line() {
        let envelope = make_envelope(vec![Operation::Insert {
            position: InsertPosition::After,
            start: 3,
            end: 3,
            anchor: vec!["three".to_owned()],
            new: vec!["inserted".to_owned()],
        }]);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\ntwo\nthree\n"))),
            Err(PlanError::EndOfFile)
        );
    }

    #[test]
    fn rejects_insertion_before_first_line() {
        let envelope = make_envelope(vec![Operation::Insert {
            position: InsertPosition::Before,
            start: 1,
            end: 1,
            anchor: vec!["one".to_owned()],
            new: vec!["inserted".to_owned()],
        }]);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\ntwo\nthree\n"))),
            Err(PlanError::EndOfFile)
        );
    }

    #[test]
    fn rejects_insert_anchor_overlapping_deleted_lines() {
        let envelope = make_envelope(vec![
            Operation::Delete {
                start: 2,
                end: 2,
                old: vec!["two".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 2,
                anchor: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(
                &envelope,
                &originals("one\ntwo\nthree\nfour\n")
            )),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn allows_insertions_with_overlapping_anchors_at_distinct_gaps() {
        let envelope = make_envelope(vec![
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 2,
                anchor: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["after_two".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 2,
                end: 3,
                anchor: vec!["two".to_owned(), "three".to_owned()],
                new: vec!["after_three".to_owned()],
            },
        ]);

        let plan = plan_envelope(&envelope, &originals("one\ntwo\nthree\nfour\n")).unwrap();

        assert_eq!(
            plan.files[0].proposed,
            "one\ntwo\nafter_two\nthree\nafter_three\nfour\n"
        );
    }

    #[test]
    fn rejects_two_insertions_at_same_gap() {
        let envelope = make_envelope(vec![
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 1,
                anchor: vec!["one".to_owned()],
                new: vec!["first".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 1,
                anchor: vec!["one".to_owned()],
                new: vec!["second".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\ntwo\nthree\n"))),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn rejects_insertion_inside_deleted_range() {
        let envelope = make_envelope(vec![
            Operation::Delete {
                start: 2,
                end: 4,
                old: vec!["two".to_owned(), "three".to_owned(), "four".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 2,
                end: 3,
                anchor: vec!["two".to_owned(), "three".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(
                &envelope,
                &originals("one\ntwo\nthree\nfour\nfive\n"),
            )),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn rejects_insertion_inside_replaced_range() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 2,
                end: 4,
                old: vec!["two".to_owned(), "three".to_owned(), "four".to_owned()],
                new: vec!["combined".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::After,
                start: 2,
                end: 3,
                anchor: vec!["two".to_owned(), "three".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(
                &envelope,
                &originals("one\ntwo\nthree\nfour\nfive\n"),
            )),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn rejects_overlapping_operations() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 1,
                end: 2,
                old: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["combined".to_owned()],
            },
            Operation::Delete {
                start: 2,
                end: 2,
                old: vec!["two".to_owned()],
            },
        ]);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\ntwo\nthree\n"))),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn plan_envelope_rejects_git_path() {
        let envelope = Envelope {
            token: "TOKEN123".to_owned(),
            id: "git-test".to_owned(),
            files: vec![FileSection {
                path: ".git/config".to_owned(),
                operations: vec![],
            }],
        };
        let originals = std::collections::HashMap::new();
        assert_eq!(
            kind(plan_envelope(&envelope, &originals)),
            Err(PlanError::InvalidPath)
        );
    }

    #[test]
    fn rejects_missing_original_file() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        assert_eq!(
            kind(plan_envelope(&envelope, &std::collections::HashMap::new())),
            Err(PlanError::MissingFile)
        );
    }

    #[test]
    fn rejects_duplicate_file_sections() {
        let mut envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);
        let duplicate = FileSection {
            path: "example.txt".to_owned(),
            operations: vec![Operation::Replace {
                start: 1,
                end: 1,
                old: vec!["one".to_owned()],
                new: vec!["ONE".to_owned()],
            }],
        };
        envelope.files.push(duplicate);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\n"))),
            Err(PlanError::DuplicateFile)
        );
    }

    #[test]
    fn preserves_trailing_blank_lines() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        let plan = plan_envelope(&envelope, &originals("one\n\n")).unwrap();

        assert_eq!(plan.files[0].proposed, "ONE\n\n");
    }

    #[test]
    fn preserves_missing_final_newline() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        let plan = plan_envelope(&envelope, &originals("one\ntwo")).unwrap();

        assert_eq!(plan.files[0].proposed, "ONE\ntwo");
    }

    #[test]
    fn preserves_crlf_without_final_newline() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        let plan = plan_envelope(&envelope, &originals("one\r\ntwo")).unwrap();

        assert_eq!(plan.files[0].proposed, "ONE\r\ntwo");
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        let plan = plan_envelope(&envelope, &originals("one\r\ntwo\r\n")).unwrap();

        assert_eq!(plan.files[0].proposed, "ONE\r\ntwo\r\n");
    }

    #[test]
    fn rejects_mixed_line_endings() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        assert_eq!(
            kind(plan_envelope(&envelope, &originals("one\r\ntwo\n"))),
            Err(PlanError::InvalidFile)
        );
    }

    #[test]
    fn preserves_multiple_trailing_blank_lines() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec!["ONE".to_owned()],
        }]);

        let plan = plan_envelope(&envelope, &originals("one\n\n\n")).unwrap();

        assert_eq!(plan.files[0].proposed, "ONE\n\n\n");
    }

    #[test]
    fn allows_insertion_before_edit_boundary() {
        let envelope = make_envelope(vec![
            Operation::Insert {
                position: InsertPosition::After,
                start: 1,
                end: 2,
                anchor: vec!["one".to_owned(), "two".to_owned()],
                new: vec!["inserted".to_owned()],
            },
            Operation::Replace {
                start: 3,
                end: 3,
                old: vec!["three".to_owned()],
                new: vec!["THREE".to_owned()],
            },
        ]);

        let plan = plan_envelope(&envelope, &originals("one\ntwo\nthree\nfour\n")).unwrap();

        assert_eq!(plan.files[0].proposed, "one\ntwo\ninserted\nTHREE\nfour\n");
    }

    #[test]
    fn allows_insertion_at_edit_boundary() {
        let envelope = make_envelope(vec![
            Operation::Replace {
                start: 2,
                end: 3,
                old: vec!["two".to_owned(), "three".to_owned()],
                new: vec!["TWO".to_owned(), "THREE".to_owned()],
            },
            Operation::Insert {
                position: InsertPosition::Before,
                start: 4,
                end: 4,
                anchor: vec!["four".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        let plan = plan_envelope(&envelope, &originals("one\ntwo\nthree\nfour\nfive\n")).unwrap();

        assert_eq!(
            plan.files[0].proposed,
            "one\nTWO\nTHREE\ninserted\nfour\nfive\n"
        );
    }

    #[test]
    fn precondition_error_message_shows_escaped_tabs() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["\texpected".to_owned()],
            new: vec!["new".to_owned()],
        }]);
        let original = originals("\tactual\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(message.contains("\\texpected"), "{message}");
        assert!(message.contains("\\tactual"), "{message}");
    }
}

#[cfg(test)]
mod parse_load_plan_tests {
    use super::{ParseError, PipelineError, parse_load_plan};
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

#[cfg(test)]
mod load_originals_tests {
    use super::{Envelope, FileSection, load_originals};
    use std::fs;
    use std::path::PathBuf;

    fn test_directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("cvb-load-originals-{name}-{}", std::process::id()));

        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn envelope(path: &str) -> Envelope {
        Envelope {
            token: "TOKEN123".to_owned(),
            id: "load-test".to_owned(),
            files: vec![FileSection {
                path: path.to_owned(),
                operations: vec![],
            }],
        }
    }

    #[test]
    fn load_originals_rejects_git_path() {
        let root = test_directory("git-path");
        let result = load_originals(&root, &envelope(".git/config"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsafe target path"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_existing_relative_file() {
        let root = test_directory("reads");
        fs::write(root.join("example.rs"), "fn example() {}\n").unwrap();

        let result = load_originals(&root, &envelope("example.rs")).unwrap();

        assert_eq!(
            result.get("example.rs").map(String::as_str),
            Some("fn example() {}\n")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_missing_file() {
        let root = test_directory("missing");

        let result = load_originals(&root, &envelope("missing.rs"));

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unable to inspect"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_relative_path() {
        let root = test_directory("unsafe");

        let result = load_originals(&root, &envelope("../outside.rs"));

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsafe target path"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_target() {
        use std::os::unix::fs::symlink;

        let root = test_directory("symlink");
        let outside = root.with_extension("outside");
        fs::write(&outside, "outside contents").unwrap();
        symlink(&outside, root.join("target.rs")).unwrap();

        let result = load_originals(&root, &envelope("target.rs"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("symlink target is not allowed")
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside contents");

        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[test]
    fn rejects_utf8_bom() {
        let root = test_directory("bom");
        let original: &[u8] = b"\xEF\xBB\xBFfn example() {}\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "UTF-8 BOM must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting a BOM must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_nul_byte() {
        let root = test_directory("nul");
        let original: &[u8] = b"fn example() {}\0\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "NUL bytes must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting NUL bytes must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_crlf_file() {
        let root = test_directory("crlf");
        let original = "first line\r\nsecond line\r\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs")).unwrap();

        assert_eq!(result.get("example.rs").map(String::as_str), Some(original));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_mixed_line_endings() {
        let root = test_directory("mixed-line-endings");
        let original: &[u8] = b"first line\nsecond line\r\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "mixed LF/CRLF endings must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting mixed line endings must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_lone_carriage_return() {
        let root = test_directory("lone-carriage-return");
        let original: &[u8] = b"first line\rsecond line\n";
        fs::write(root.join("example.rs"), original).unwrap();

        let result = load_originals(&root, &envelope("example.rs"));

        assert!(result.is_err(), "a lone CR must be rejected");
        assert_eq!(
            fs::read(root.join("example.rs")).unwrap(),
            original,
            "rejecting a lone CR must preserve the original bytes"
        );

        fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn parse_error_message_shows_line_number() {
    let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::BEGIN ";
    let failure = parse_envelope(input).unwrap_err();
    let message = failure.to_string();
    assert_eq!(failure.kind, ParseError::Syntax);
    assert_eq!(failure.line, 3);
    assert!(message.contains("line 3"), "{message}");
}

#[test]
fn count_error_message_shows_line_and_counts() {
    let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-2
CVB::k7Q2xw9R::OLD n=5
line 1
line 2
CVB::k7Q2xw9R::NEW n=1
new line
CVB::k7Q2xw9R::ENDOP
CVB::k7Q2xw9R::END ops=1";
    let failure = parse_envelope(input).unwrap_err();
    let message = failure.to_string();
    assert_eq!(failure.kind, ParseError::Count);
    assert_eq!(failure.line, 4);
    assert!(message.contains("line 4"), "{message}");
    assert!(message.contains("5"), "{message}");
    assert!(message.contains("2"), "{message}");
}

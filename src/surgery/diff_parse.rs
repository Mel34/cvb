use std::fmt;

const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffFile {
    pub(crate) old_path: String,
    pub(crate) new_path: String,
    pub(crate) hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffHunk {
    pub(crate) old_start: usize,
    pub(crate) old_count: usize,
    pub(crate) new_start: usize,
    pub(crate) new_count: usize,
    pub(crate) label: String,
    pub(crate) lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffLine {
    pub(crate) kind: DiffLineKind,
    pub(crate) text: String,
    pub(crate) no_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffLineKind {
    Context,
    Remove,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffParseError {
    pub(crate) line: usize,
    pub(crate) message: String,
}

impl fmt::Display for DiffParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "diff parse error on line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for DiffParseError {}

fn error(line: usize, message: impl Into<String>) -> DiffParseError {
    DiffParseError {
        line,
        message: message.into(),
    }
}

fn parse_usize(value: &str, line: usize) -> Result<usize, DiffParseError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error(line, "invalid range number"));
    }

    value
        .parse::<usize>()
        .map_err(|_| error(line, "range number is too large"))
}

fn parse_range(value: &str, line: usize) -> Result<(usize, usize), DiffParseError> {
    let value = value
        .strip_prefix(['-', '+'])
        .ok_or_else(|| error(line, "range must start with '-' or '+'"))?;

    let (start_text, count_text) = match value.split_once(',') {
        Some((start, count)) => {
            if count.contains(',') {
                return Err(error(line, "invalid range count"));
            }
            (start, Some(count))
        }
        None => (value, None),
    };

    let start = parse_usize(start_text, line)?;
    let count = match count_text {
        Some(text) => parse_usize(text, line)?,
        None => 1,
    };

    // For an empty range, unified diff uses the line before the insertion
    // or deletion point; zero is therefore valid.
    if count > 0 && start == 0 {
        return Err(error(line, "non-empty range must start at line 1 or later"));
    }

    Ok((start, count))
}

fn parse_hunk_header(
    header: &str,
    line: usize,
) -> Result<(usize, usize, usize, usize, String), DiffParseError> {
    let rest = header
        .strip_prefix("@@ -")
        .ok_or_else(|| error(line, "invalid hunk header"))?;

    let (ranges, suffix) = rest
        .split_once(" @@")
        .ok_or_else(|| error(line, "unterminated hunk header"))?;

    let mut ranges = ranges.split_whitespace();
    let old_range = ranges
        .next()
        .ok_or_else(|| error(line, "missing old range"))?;
    let new_range = ranges
        .next()
        .ok_or_else(|| error(line, "missing new range"))?;

    if ranges.next().is_some() {
        return Err(error(line, "unexpected text in hunk ranges"));
    }

    let (old_start, old_count) = parse_range(&format!("-{old_range}"), line)?;
    let (new_start, new_count) = parse_range(new_range, line)?;

    let label = suffix.trim().to_owned();

    Ok((old_start, old_count, new_start, new_count, label))
}

fn validate_path(path: &str, line: usize) -> Result<(), DiffParseError> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(error(line, "empty path or path containing control characters"));
    }

    Ok(())
}

fn parse_file_header(
    line_text: &str,
    expected_prefix: &str,
    line: usize,
) -> Result<String, DiffParseError> {
    let path = line_text
        .strip_prefix(expected_prefix)
        .ok_or_else(|| error(line, "invalid file header"))?;

    validate_path(path, line)?;
    Ok(path.to_owned())
}

/// Parse the unified-diff portion of CVB's rendered output.
///
/// The banner and legend are ignored. If a notes section is present, parsing
/// stops at its header; note text is commentary and is never source content.
///
/// Hunk bodies are consumed by their declared old/new counts, not by guessing
/// whether a source line resembles a structural header.
pub(crate) fn parse_diff(input: &str) -> Result<Vec<DiffFile>, DiffParseError> {
    let input = input.strip_suffix('\n').unwrap_or(input);
    let lines: Vec<&str> = input.split('\n').collect();
    let mut index = 0;
    let mut files = Vec::new();

    while index < lines.len() {
        let line_text = lines[index];

        if line_text == "=== notes ===" {
            break;
        }

        if !line_text.starts_with("--- ") {
            index += 1;
            continue;
        }

        let old_header_line = index + 1;
        let old_path = parse_file_header(line_text, "--- ", old_header_line)?;
        index += 1;

        let new_header = lines
            .get(index)
            .copied()
            .ok_or_else(|| error(index + 1, "missing new file header"))?;
        let new_path = parse_file_header(new_header, "+++ ", index + 1)?;
        index += 1;

        let mut hunks = Vec::new();

        while index < lines.len() && lines[index].starts_with("@@ -") {
            let header_line = index + 1;
            let (old_start, old_count, new_start, new_count, label) =
                parse_hunk_header(lines[index], header_line)?;
            index += 1;

            let mut old_remaining = old_count;
            let mut new_remaining = new_count;
            let mut hunk_lines: Vec<DiffLine> = Vec::new();

            while old_remaining > 0 || new_remaining > 0 {
                let body_line = lines
                    .get(index)
                    .copied()
                    .ok_or_else(|| error(index + 1, "truncated hunk body"))?;
                let body_line_number = index + 1;

                if body_line == NO_NEWLINE_MARKER {
                    return Err(error(
                        body_line_number,
                        "no-newline marker must immediately follow a qualifying body line",
                    ));
                }

                let (kind, text) = match body_line.as_bytes().first().copied() {
                    Some(b' ') => (DiffLineKind::Context, &body_line[1..]),
                    Some(b'-') => (DiffLineKind::Remove, &body_line[1..]),
                    Some(b'+') => (DiffLineKind::Add, &body_line[1..]),
                    _ => {
                        return Err(error(
                            body_line_number,
                            "expected a context, removal, or addition line",
                        ));
                    }
                };

                match kind {
                    DiffLineKind::Context => {
                        if old_remaining == 0 || new_remaining == 0 {
                            return Err(error(
                                body_line_number,
                                "context line exceeds declared hunk counts",
                            ));
                        }
                        old_remaining -= 1;
                        new_remaining -= 1;
                    }
                    DiffLineKind::Remove => {
                        if old_remaining == 0 {
                            return Err(error(
                                body_line_number,
                                "removal exceeds declared old-line count",
                            ));
                        }
                        old_remaining -= 1;
                    }
                    DiffLineKind::Add => {
                        if new_remaining == 0 {
                            return Err(error(
                                body_line_number,
                                "addition exceeds declared new-line count",
                            ));
                        }
                        new_remaining -= 1;
                    }
                }

                let qualifies_for_marker = match kind {
                    DiffLineKind::Context => old_remaining == 0 && new_remaining == 0,
                    DiffLineKind::Remove => old_remaining == 0,
                    DiffLineKind::Add => new_remaining == 0,
                };

                index += 1;

                let mut no_newline = false;
                if lines.get(index).copied() == Some(NO_NEWLINE_MARKER) {
                    if !qualifies_for_marker {
                        return Err(error(
                            index + 1,
                            "no-newline marker follows a non-final line",
                        ));
                    }
                    no_newline = true;
                    index += 1;

                    if lines.get(index).copied() == Some(NO_NEWLINE_MARKER) {
                        return Err(error(index + 1, "duplicate no-newline marker"));
                    }
                }

                hunk_lines.push(DiffLine {
                    kind,
                    text: text.to_owned(),
                    no_newline,
                });
            }

            hunks.push(DiffHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                label,
                lines: hunk_lines,
            });
        }

        if hunks.is_empty() {
            return Err(error(index + 1, "file has no hunks"));
        }

        files.push(DiffFile {
            old_path,
            new_path,
            hunks,
        });
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_omitted_and_explicit_single_line_counts() {
        for header in ["@@ -5 +5 @@", "@@ -5,1 +5,1 @@"] {
            let (old_start, old_count, new_start, new_count, _) =
                parse_hunk_header(header, 1).unwrap();

            assert_eq!((old_start, old_count), (5, 1));
            assert_eq!((new_start, new_count), (5, 1));
        }
    }

    #[test]
    fn parses_zero_count_ranges() {
        let (old_start, old_count, new_start, new_count, _) =
            parse_hunk_header("@@ -0,0 +1 @@", 1).unwrap();

        assert_eq!((old_start, old_count), (0, 0));
        assert_eq!((new_start, new_count), (1, 1));
    }

    #[test]
    fn body_text_that_looks_structural_is_not_interpreted_as_structure() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "- -- foo\n",
            "+=== notes ===\n",
        );

        let files = parse_diff(input).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks[0].lines[0].text, " -- foo");
        assert_eq!(files[0].hunks[0].lines[1].text, "=== notes ===");
    }

    #[test]
    fn accepts_marker_after_final_removed_line() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +0,0 @@\n",
            "-old\n",
            "\\ No newline at end of file\n",
        );

        let files = parse_diff(input).unwrap();
        assert!(files[0].hunks[0].lines[0].no_newline);
    }

    #[test]
    fn rejects_marker_at_body_start() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "\\ No newline at end of file\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn rejects_marker_after_non_final_line() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -2,2 +2,2 @@\n",
            " first\n",
            "\\ No newline at end of file\n",
            " second\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn rejects_duplicate_marker() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "\\ No newline at end of file\n",
            "\\ No newline at end of file\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn rejects_control_characters_in_paths() {
        assert!(validate_path("bad\npath", 1).is_err());
        assert!(validate_path("bad\u{7f}path", 1).is_err());
    }
}

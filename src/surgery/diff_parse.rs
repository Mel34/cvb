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
fn unescape_line_content(input: &str) -> Result<String, String> {
    let mut chars = input.chars().peekable();
    let mut output = String::with_capacity(input.len());

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('\\') => output.push('\\'),
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err("expected '{' after '\\u'".into());
                }

                let mut digits = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(ch) if ch.is_ascii_hexdigit() => digits.push(ch),
                        Some(_) => {
                            return Err("invalid character in Unicode escape".into());
                        }
                        None => return Err("unterminated Unicode escape".into()),
                    }
                }

                if digits.is_empty() {
                    return Err("Unicode escape requires at least one hex digit".into());
                }

                let value = u32::from_str_radix(&digits, 16)
                    .map_err(|_| "Unicode escape value is out of range".to_string())?;
                let decoded = char::from_u32(value)
                    .ok_or_else(|| "Unicode escape is not a valid scalar value".to_string())?;
                output.push(decoded);
            }
            Some(_) => return Err("unsupported escape sequence".into()),
            None => return Err("trailing backslash in escaped line".into()),
        }
    }

    Ok(output)
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
    let mut escape_notes: Vec<(EscapeNote, usize)> = Vec::new();

    let notes_start = lines.iter().position(|line| *line == "=== notes ===");

    while index < lines.len() {
        let line_text = lines[index];

        if Some(index) == notes_start {
            for note_index in index + 1..lines.len() {
                if let Some(note) = parse_escape_note(lines[note_index], note_index + 1)? {
                    escape_notes.push((note, note_index + 1));
                }
            }
            break;
        }

        if !line_text.starts_with("--- ") {
            if files.is_empty() {
                index += 1;
                continue;
            }

            if line_text.is_empty()
                && lines.get(index + 1).copied() == Some("=== notes ===")
            {
                index += 1;
                continue;
            }

            return Err(error(index + 1, "unexpected line outside file diff"));
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

            if lines.get(index).copied() == Some(NO_NEWLINE_MARKER) {
                return Err(error(
                    index + 1,
                    "no-newline marker must immediately follow a qualifying body line",
                ));
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

        if hunks
            .iter()
            .take(hunks.len().saturating_sub(1))
            .any(|hunk| hunk.lines.iter().any(|line| line.no_newline))
        {
            return Err(error(
                index + 1,
                "no-newline marker cannot precede another hunk",
            ));
        }

        files.push(DiffFile {
            old_path,
            new_path,
            hunks,
        });
    }

    validate_escape_notes(&files, &escape_notes)?;
    Ok(files)
}

fn validate_escape_notes(
    files: &[DiffFile],
    notes: &[(EscapeNote, usize)],
) -> Result<(), DiffParseError> {
    let mut matched_targets = Vec::new();

    for (note, input_line) in notes {
        let mut targets = Vec::new();

        for (file_index, file) in files.iter().enumerate() {
            let file_path = match note.side {
                EscapeNoteSide::Remove => {
                    file.old_path.strip_prefix("a/").unwrap_or(&file.old_path)
                }
                EscapeNoteSide::Add | EscapeNoteSide::Context => {
                    file.new_path.strip_prefix("b/").unwrap_or(&file.new_path)
                }
            };

            if file_path != note.path {
                continue;
            }

            for (hunk_index, hunk) in file.hunks.iter().enumerate() {
                let mut old_line = hunk.old_start;
                let mut new_line = hunk.new_start;

                for (line_index, diff_line) in hunk.lines.iter().enumerate() {
                    let matches = match note.side {
                        EscapeNoteSide::Add => {
                            diff_line.kind == DiffLineKind::Add && new_line == note.line
                        }
                        EscapeNoteSide::Remove => {
                            diff_line.kind == DiffLineKind::Remove && old_line == note.line
                        }
                        EscapeNoteSide::Context => {
                            diff_line.kind == DiffLineKind::Context && new_line == note.line
                        }
                    };

                    if matches {
                        targets.push((file_index, hunk_index, line_index));
                    }

                    match diff_line.kind {
                        DiffLineKind::Context => {
                            old_line = old_line.checked_add(1).ok_or_else(|| {
                                error(*input_line, "old hunk line coordinate overflow")
                            })?;
                            new_line = new_line.checked_add(1).ok_or_else(|| {
                                error(*input_line, "new hunk line coordinate overflow")
                            })?;
                        }
                        DiffLineKind::Remove => {
                            old_line = old_line.checked_add(1).ok_or_else(|| {
                                error(*input_line, "old hunk line coordinate overflow")
                            })?;
                        }
                        DiffLineKind::Add => {
                            new_line = new_line.checked_add(1).ok_or_else(|| {
                                error(*input_line, "new hunk line coordinate overflow")
                            })?;
                        }
                    }
                }
            }
        }

        if targets.is_empty() {
            return Err(error(*input_line, "shown-escaped note does not match a diff line"));
        }
        if targets.len() != 1 {
            return Err(error(*input_line, "shown-escaped note matches multiple diff lines"));
        }

        let target = targets[0];
        if matched_targets.contains(&target) {
            return Err(error(*input_line, "duplicate shown-escaped note target"));
        }
        matched_targets.push(target);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeNoteSide {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EscapeNote {
    path: String,
    line: usize,
    side: EscapeNoteSide,
}

fn parse_escape_note(
    note: &str,
    input_line: usize,
) -> Result<Option<EscapeNote>, DiffParseError> {
    let Some(note) = note.strip_prefix("file ") else {
        return Ok(None);
    };
    let Some(note) = note.strip_suffix(": shown escaped") else {
        return Ok(None);
    };

    let (path, location) = note
        .rsplit_once(" L")
        .ok_or_else(|| error(input_line, "malformed shown-escaped note"))?;
    let (line_text, side_text) = location
        .split_once(' ')
        .ok_or_else(|| error(input_line, "malformed shown-escaped note"))?;

    validate_path(path, input_line)?;
    let line = parse_usize(line_text, input_line)?;
    if line == 0 {
        return Err(error(input_line, "shown-escaped line number must be positive"));
    }

    let side = match side_text {
        "+" => EscapeNoteSide::Add,
        "-" => EscapeNoteSide::Remove,
        "context" => EscapeNoteSide::Context,
        _ => return Err(error(input_line, "invalid shown-escaped note side")),
    };

    Ok(Some(EscapeNote {
        path: path.to_owned(),
        line,
        side,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_escape_notes_for_each_side() {
        assert_eq!(
            parse_escape_note("file src/lib.rs L12 +: shown escaped", 1).unwrap(),
            Some(EscapeNote {
                path: "src/lib.rs".into(),
                line: 12,
                side: EscapeNoteSide::Add,
            })
        );
        assert_eq!(
            parse_escape_note("file src/lib.rs L7 -: shown escaped", 2).unwrap(),
            Some(EscapeNote {
                path: "src/lib.rs".into(),
                line: 7,
                side: EscapeNoteSide::Remove,
            })
        );
        assert_eq!(
            parse_escape_note("file src/my file.rs L3 context: shown escaped", 3).unwrap(),
            Some(EscapeNote {
                path: "src/my file.rs".into(),
                line: 3,
                side: EscapeNoteSide::Context,
            })
        );
    }

    #[test]
    fn ignores_notes_that_do_not_describe_escaped_lines() {
        assert_eq!(
            parse_escape_note("file src/lib.rs: line endings: old LF, new CRLF", 1).unwrap(),
            None
        );
        assert_eq!(
            parse_escape_note("op 1 L12 +: trailing spaces", 2).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_malformed_escape_notes() {
        for note in [
            "file  L1 +: shown escaped",
            "file src/lib.rs L0 +: shown escaped",
            "file src/lib.rs Lbad +: shown escaped",
            "file src/lib.rs L1 bogus: shown escaped",
            "file src/lib.rs +: shown escaped",
        ] {
            assert!(
                parse_escape_note(note, 9).is_err(),
                "expected malformed note to be rejected: {note:?}"
            );
        }
    }

    #[test]
    fn unescapes_backslashes_unicode_and_adjacent_escapes() {
        assert_eq!(unescape_line_content(r"plain text").unwrap(), "plain text");
        assert_eq!(unescape_line_content(r"\\").unwrap(), "\\");
        assert_eq!(unescape_line_content(r"\u{41}").unwrap(), "A");
        assert_eq!(
            unescape_line_content(r"\\u{41}\u{200b}").unwrap(),
            "\\u{41}\u{200b}"
        );
        assert_eq!(
            unescape_line_content(r"\u{41}\u{42}").unwrap(),
            "AB"
        );
        assert_eq!(
            unescape_line_content(r"\u{0}\u{7f}\u{a0}\u{200d}\u{feff}\u{2028}\u{2029}")
                .unwrap(),
            "\0\u{7f}\u{a0}\u{200d}\u{feff}\u{2028}\u{2029}"
        );
    }

    #[test]
    fn unescape_does_not_rescan_decoded_characters() {
        assert_eq!(
            unescape_line_content(r"\u{5c}u{41}").unwrap(),
            r"\u{41}"
        );
        assert_eq!(
            unescape_line_content(r"\\\\u{41}").unwrap(),
            r"\\u{41}"
        );
    }

    #[test]
    fn rejects_malformed_line_escapes() {
        for input in [
            r"\",
            r"\x",
            r"\u",
            r"\u41}",
            r"\u{}",
            r"\u{g}",
            r"\u{41",
            r"\u{110000}",
            r"\u{d800}",
            r"\u{12345678901234567890}",
        ] {
            assert!(
                unescape_line_content(input).is_err(),
                "expected malformed escape to be rejected: {input:?}"
            );
        }
    }



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
    fn rejects_malformed_hunk_header_ranges() {
        for header in [
            "@@ x1 +1 @@",
            "@@ - +1 @@",
            "@@ -1 + @@",
            "@@ -1 +1",
            "@@ -1 @@",
            "@@ -1 +1 +2 @@",
            "@@ -1, +1 @@",
            "@@ -1,1,2 +1 @@",
            "@@ --1 +1 @@",
            "@@ -0 +1 @@",
            "@@ -1 +0 @@",
            "@@ -\u{0661} +1 @@",
        ] {
            assert!(
                parse_hunk_header(header, 7).is_err(),
                "expected malformed hunk header to be rejected: {header:?}"
            );
        }
    }

    #[test]
    fn rejects_oversized_hunk_range_numbers() {
        let too_large = format!("@@ -{} +1 @@", format!("{}0", usize::MAX));

        let error = parse_hunk_header(&too_large, 9).unwrap_err();
        assert_eq!(error.line, 9);
        assert_eq!(error.message, "range number is too large");
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
            "@@ -1,4 +1,4 @@\n",
            "- -- foo\n",
            "---- a/x\n",
            "-@@ -1 +1 @@\n",
            "-\\ No newline at end of file\n",
            "+=== notes ===\n",
            "++++ b/x\n",
            "+@@ -1 +1 @@\n",
            "+\\ No newline at end of file\n",
        );

        let files = parse_diff(input).unwrap();
        assert_eq!(files.len(), 1);
        let lines = &files[0].hunks[0].lines;
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[0].text, " -- foo");
        assert_eq!(lines[1].text, "--- a/x");
        assert_eq!(lines[2].text, "@@ -1 +1 @@");
        assert_eq!(lines[3].text, "\\ No newline at end of file");
        assert_eq!(lines[4].text, "=== notes ===");
        assert_eq!(lines[5].text, "+++ b/x");
        assert_eq!(lines[6].text, "@@ -1 +1 @@");
        assert_eq!(lines[7].text, "\\ No newline at end of file");
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
    fn accepts_marker_after_final_added_line() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -0,0 +1 @@\n",
            "+new\n",
            "\\ No newline at end of file\n",
        );

        let files = parse_diff(input).unwrap();
        let line = &files[0].hunks[0].lines[0];
        assert_eq!(line.kind, DiffLineKind::Add);
        assert!(line.no_newline);
    }

    #[test]
    fn accepts_marker_after_final_context_line() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            " same\n",
            "\\ No newline at end of file\n",
        );

        let files = parse_diff(input).unwrap();
        let line = &files[0].hunks[0].lines[0];
        assert_eq!(line.kind, DiffLineKind::Context);
        assert!(line.no_newline);
    }

    #[test]
    fn rejects_unexpected_text_after_completed_hunk() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "unexpected text\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn rejects_malformed_escape_note_in_diff() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "\n",
            "=== notes ===\n",
            "file example.txt Lbad +: shown escaped\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn accepts_line_ending_and_unrelated_notes_in_diff() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "\n",
            "=== notes ===\n",
            "file example.txt: line endings: old LF, new CRLF\n",
            "op 1 L2 +: trailing spaces\n",
        );

        assert_eq!(parse_diff(input).unwrap().len(), 1);
    }

    #[test]
    fn validates_escape_notes_against_diff_lines() {
        for (notes, expected_ok) in [
            ("file example.txt L1 +: shown escaped", true),
            ("file other.txt L1 +: shown escaped", false),
            ("file example.txt L2 +: shown escaped", false),
            ("file example.txt L2 -: shown escaped", false),
            (
                "file example.txt L1 +: shown escaped\nfile example.txt L1 +: shown escaped",
                false,
            ),
        ] {
            let input = format!(
                "--- a/example.txt\n\
                 +++ b/example.txt\n\
                 @@ -1 +1 @@\n\
                 -old\n\
                 +new\n\
                 \n\
                 === notes ===\n\
                 {notes}\n"
            );

            assert_eq!(
                parse_diff(&input).is_ok(),
                expected_ok,
                "unexpected result for notes: {notes:?}"
            );
        }
    }

    #[test]
    fn rejects_missing_new_file_header() {
        let input = "--- a/example.txt\n";

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.message, "missing new file header");
    }

    #[test]
    fn rejects_malformed_new_file_header() {
        let input = "--- a/example.txt\nnot a new-file header\n";

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 2);
        assert_eq!(error.message, "invalid file header");
    }

    #[test]
    fn rejects_file_diff_without_hunks() {
        let input = "--- a/example.txt\n+++ b/example.txt\n";

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 3);
        assert_eq!(error.message, "file has no hunks");
    }

    #[test]
    fn rejects_truncated_hunk_body() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1,2 +1,2 @@\n",
            "-old\n",
        );

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 5);
        assert_eq!(error.message, "truncated hunk body");
    }

    #[test]
    fn rejects_invalid_hunk_body_prefix() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "?invalid\n",
        );

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 4);
        assert_eq!(
            error.message,
            "expected a context, removal, or addition line"
        );
    }

    #[test]
    fn rejects_oversized_hunk_range_count() {
        let too_large = format!(
            "--- a/example.txt\n+++ b/example.txt\n@@ -1,{} +1 @@\n",
            format!("{}0", usize::MAX)
        );

        let error = parse_diff(&too_large).unwrap_err();
        assert_eq!(error.line, 3);
        assert_eq!(error.message, "range number is too large");
    }

    #[test]
    fn rejects_orphan_marker_after_empty_hunk() {
        let input = concat!(
            "cvb surgery (dry run): 1 op, 1 file\n",
            "Legend: test\n",
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -0,0 +0,0 @@\n",
            "\\ No newline at end of file\n",
        );

        assert!(parse_diff(input).is_err());
    }

    #[test]
    fn rejects_misplaced_no_newline_markers() {
        let cases = [
            concat!(
                "--- a/example.txt\n",
                "+++ b/example.txt\n",
                "@@ -1,2 +1 @@\n",
                "-old\n",
                "\\ No newline at end of file\n",
                "+new\n",
            ),
            concat!(
                "--- a/example.txt\n",
                "+++ b/example.txt\n",
                "@@ -1 +1,2 @@\n",
                "-old\n",
                "+new\n",
                "\\ No newline at end of file\n",
            ),
            concat!(
                "--- a/example.txt\n",
                "+++ b/example.txt\n",
                "@@ -1,2 +1,2 @@\n",
                " first\n",
                "\\ No newline at end of file\n",
                "-old\n",
                "+new\n",
            ),
            concat!(
                "--- a/example.txt\n",
                "+++ b/example.txt\n",
                "@@ -1 +0,0 @@\n",
                "-old\n",
                "\\ No newline at end of file\n",
                "\\ No newline at end of file\n",
            ),
            concat!(
                "--- a/example.txt\n",
                "+++ b/example.txt\n",
                "@@ -1 +1 @@\n",
                "\\ No newline at end of file\n",
            ),
        ];

        for input in cases {
            assert!(
                parse_diff(input).is_err(),
                "expected misplaced no-newline marker to be rejected:\n{input}"
            );
        }
    }

    #[test]
    fn rejects_context_line_when_declared_counts_are_inconsistent() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -0,0 +1 @@\n",
            " context\n",
        );

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 4);
        assert_eq!(error.message, "context line exceeds declared hunk counts");
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

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 6);
        assert_eq!(
            error.message,
            "no-newline marker must immediately follow a qualifying body line"
        );
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

        let error = parse_diff(input).unwrap_err();
        assert_eq!(error.line, 8);
        assert_eq!(error.message, "duplicate no-newline marker");
    }

    #[test]
    fn rejects_no_newline_marker_before_later_hunk() {
        let input = concat!(
            "--- a/example.txt\n",
            "+++ b/example.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "\\ No newline at end of file\n",
            "@@ -10 +10 @@\n",
            "-before\n",
            "+after\n",
        );

        let error = parse_diff(input).unwrap_err();
        assert_eq!(
            error.message,
            "no-newline marker cannot precede another hunk"
        );
    }

    #[test]
    fn rejects_control_characters_in_paths() {
        assert!(validate_path("bad\npath", 1).is_err());
        assert!(validate_path("bad\u{7f}path", 1).is_err());
    }
}

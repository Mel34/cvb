use std::fmt::Write as _;

use similar::{ChangeTag, TextDiff};

use crate::surgery::{detect_newline, InsertPosition, Newline, Operation, Plan};

pub(crate) const MARKER_TRAILING_WHITESPACE: &str = "[trailing-whitespace]";
pub(crate) const MARKER_TAB_INDENT: &str = "[tab-indent]";
pub(crate) const MARKER_ZERO_WIDTH: &str = "[zero-width]";
pub(crate) const MARKER_BIDI: &str = "[bidi]";
pub(crate) const MARKER_CONTROL: &str = "[control]";
pub(crate) const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RenderOptions {
    pub color: bool,
}

pub(crate) fn is_zero_width(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x200B..=0x200D | 0xFEFF | 0x200E | 0x200F | 0x2028 | 0x2029 | 0x2060..=0x2064
    )
}

pub(crate) fn is_bidi_override(c: char) -> bool {
    let code = c as u32;
    matches!(code, 0x202A..=0x202E | 0x2066..=0x2069)
}

fn is_format_char(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn is_space_separator(c: char) -> bool {
    matches!(
        c as u32,
        0x00A0 | 0x1680 | 0x2000..=0x200A | 0x202F | 0x205F | 0x3000
    )
}

pub(crate) fn is_control_char(c: char) -> bool {
    let code = c as u32;
    (code < 32 && c != '\t' && c != '\n')
        || code == 127
        || (0x80..=0x9F).contains(&code)
}

fn is_escape_char(c: char) -> bool {
    is_format_char(c) || is_space_separator(c) || is_zero_width(c) || is_control_char(c)
}

fn escape_line_content(s: &str) -> String {
    if !s.chars().any(is_escape_char) {
        return s.to_owned();
    }

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' {
            out.push_str("\\\\");
        } else if is_escape_char(c) {
            write!(out, "\\u{{{:x}}}", c as u32).unwrap();
        } else {
            out.push(c);
        }
    }
    out
}

fn compute_line_markers(content: &str) -> Vec<&'static str> {
    let mut markers = Vec::new();

    let mut leading_ws = "";
    for (i, c) in content.char_indices() {
        if c != ' ' && c != '\t' {
            leading_ws = &content[..i];
            break;
        }
        if i + c.len_utf8() == content.len() {
            leading_ws = content;
        }
    }

    if leading_ws.contains('\t') {
        markers.push(MARKER_TAB_INDENT);
    }

    if content.ends_with(' ') || content.ends_with('\t') {
        markers.push(MARKER_TRAILING_WHITESPACE);
    }

    if content.chars().any(is_zero_width) {
        markers.push(MARKER_ZERO_WIDTH);
    }
    if content.chars().any(is_bidi_override) {
        markers.push(MARKER_BIDI);
    }
    if content.chars().any(is_control_char) {
        markers.push(MARKER_CONTROL);
    }

    markers
}

fn detect_file_ending(bytes: &[u8]) -> &'static str {
    if let Ok(s) = std::str::from_utf8(bytes)
        && let Ok(nl) = detect_newline(s)
    {
        return match nl {
            Newline::Lf => "LF",
            Newline::Crlf => "CRLF",
        };
    }
    if bytes.contains(&b'\r') {
        "CRLF"
    } else {
        "LF"
    }
}

fn canonical_hunk_header(header: &str) -> String {
    let Some(rest) = header.strip_prefix("@@ ") else {
        return header.to_owned();
    };
    let Some((ranges, suffix)) = rest.split_once(" @@") else {
        return header.to_owned();
    };

    let normalized = ranges
        .split_whitespace()
        .map(|range| {
            let Some((sign, coordinates)) = range.split_at_checked(1) else {
                return range.to_owned();
            };
            let Some((start, count)) = coordinates.split_once(',') else {
                return range.to_owned();
            };
            if count == "1" {
                format!("{sign}{start}")
            } else {
                range.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    format!("@@ {normalized} @@{suffix}")
}

/// Renders a unified diff for one file purely from its original and planned bytes.
pub(crate) fn render_file_diff(
    path: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
    options: RenderOptions,
) -> String {
    render_file_diff_with_operations(path, old_bytes, new_bytes, options, &[])
}

fn render_file_diff_with_operations(
    path: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
    options: RenderOptions,
    operations: &[Operation],
) -> String {
    let old_str = String::from_utf8_lossy(old_bytes);
    let new_str = String::from_utf8_lossy(new_bytes);

    let diff = TextDiff::from_lines(&old_str, &new_str);
    let hunks: Vec<_> = diff.unified_diff().context_radius(3).iter_hunks().collect();

    if hunks.is_empty() {
        return String::new();
    }


    let mut out = String::new();

    if options.color {
        writeln!(out, "\x1b[1m--- a/{path}\x1b[0m").unwrap();
        writeln!(out, "\x1b[1m+++ b/{path}\x1b[0m").unwrap();
    } else {
        writeln!(out, "--- a/{path}").unwrap();
        writeln!(out, "+++ b/{path}").unwrap();
    }

    for hunk in hunks {
        let mut header_str = canonical_hunk_header(&hunk.header().to_string());
        let old_indices: Vec<usize> = hunk
            .iter_changes()
            .filter_map(|change| change.old_index())
            .collect();
        let old_start = old_indices.iter().copied().min().map(|line| line + 1).unwrap_or(1);
        let old_end = old_indices.iter().copied().max().map(|line| line + 1).unwrap_or(0);
        let labels: Vec<String> = operations
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| {
                let (kind, start, end) = match operation {
                    Operation::Replace { start, end, .. } => ("replace", *start, *end),
                    Operation::Delete { start, end, .. } => ("delete", *start, *end),
                    Operation::Insert {
                        position, start, end, ..
                    } => (
                        match position {
                            InsertPosition::Before => "insert before",
                            InsertPosition::After => "insert after",
                        },
                        *start,
                        *end,
                    ),
                };
                (start <= old_end && end >= old_start).then(|| {
                    format!("op {} {kind} [exact @ L{start}]", index + 1)
                })
            })
            .collect();
        if !labels.is_empty() {
            let header_end = header_str.trim_end_matches('\n').to_string();
            header_str = format!("{header_end} {}\n", labels.join("; "));
        }
        if options.color {
            write!(out, "\x1b[36m{header_str}\x1b[0m").unwrap();
        } else {
            write!(out, "{header_str}").unwrap();
        }
        if !header_str.ends_with('\n') {
            writeln!(out).unwrap();
        }

        for change in hunk.iter_changes() {
            let tag = change.tag();
            let value = change.value();
            let content = value.strip_suffix('\n').map_or(value, |line| {
                line.strip_suffix('\r').unwrap_or(line)
            });
            let sign = match tag {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };

            match tag {
                ChangeTag::Equal => {
                    let escaped = escape_line_content(content);
                    writeln!(out, " {escaped}").unwrap();
                }
                ChangeTag::Delete | ChangeTag::Insert => {
                    let escaped = escape_line_content(content);
                    if options.color {
                        let color_code = if tag == ChangeTag::Insert {
                            "\x1b[32m"
                        } else {
                            "\x1b[31m"
                        };
                        writeln!(out, "{color_code}{sign}{escaped}\x1b[0m").unwrap();
                    } else {
                        writeln!(out, "{sign}{escaped}").unwrap();
                    }
                }
            }

            let old_line_is_unterminated = !old_bytes.ends_with(b"\n")
                && change
                    .old_index()
                    .is_some_and(|index| index + 1 == old_str.lines().count());
            let new_line_is_unterminated = !new_bytes.ends_with(b"\n")
                && change
                    .new_index()
                    .is_some_and(|index| index + 1 == new_str.lines().count());

            if change.missing_newline()
                || old_line_is_unterminated
                || new_line_is_unterminated
            {
                writeln!(out, "{NO_NEWLINE_MARKER}").unwrap();
            }
        }
    }

    out
}

/// Renders a full unified diff for all files in a plan.
pub(crate) fn render_plan_diff(plan: &Plan, options: RenderOptions) -> String {
    let mut out = String::new();
    let mut notes = Vec::new();

    let operation_count: usize = plan.files.iter().map(|file| file.operations.len()).sum();
    let changed_file_count = plan
        .files
        .iter()
        .filter(|file| file.original != file.proposed)
        .count();

    writeln!(
        out,
        "cvb surgery (dry run): {operation_count} op{}, {changed_file_count} file{}",
        if operation_count == 1 { "" } else { "s" },
        if changed_file_count == 1 { "" } else { "s" }
    )
    .unwrap();

    writeln!(
        out,
        "Legend: column 1 is the gutter (' ' context, '-' removed, '+' added). Source text starts at column 2. Lines named in the notes as \"shown escaped\" are transformed (see notes). Text after \"=== notes ===\" is commentary, never source."
    )
    .unwrap();

    for file in &plan.files {
        let old_bytes = file.original.as_bytes();
        let new_bytes = file.proposed.as_bytes();
        let file_diff = render_file_diff_with_operations(
            &file.path,
            old_bytes,
            new_bytes,
            options,
            &file.operations,
        );

        if file_diff.is_empty() {
            continue;
        }

        out.push_str(&file_diff);

        let diff = TextDiff::from_lines(&file.original, &file.proposed);
        let mut lines_removed = 0usize;
        let mut lines_added = 0usize;
        for change in diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Delete => lines_removed += 1,
                ChangeTag::Insert => lines_added += 1,
                ChangeTag::Equal => {}
            }
        }

        notes.push(format!(
            "file {}: Summary: -{lines_removed} lines, +{lines_added} lines; bytes: {} -> {}",
            file.path,
            old_bytes.len(),
            new_bytes.len()
        ));

        let mut old_line = 1usize;
        let mut new_line = 1usize;
        for change in diff.iter_all_changes() {
            let content = change.value().trim_end_matches(['\r', '\n']);
            let (action, side, line) = match change.tag() {
                ChangeTag::Delete => {
                    let line = old_line;
                    old_line += 1;
                    ("removed", "-", line)
                }
                ChangeTag::Insert => {
                    let line = new_line;
                    new_line += 1;
                    ("added", "+", line)
                }
                ChangeTag::Equal => {
                    old_line += 1;
                    new_line += 1;
                    continue;
                }
            };

            if content.chars().any(is_escape_char) {
                notes.push(format!(
                    "file {} L{} {side}: shown escaped",
                    file.path, line
                ));
            }

            let markers = compute_line_markers(content);
            if !markers.is_empty() {
                notes.push(format!(
                    "file {}: {action} line has {}",
                    file.path,
                    markers.join(", ")
                ));
            }
        }

        for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
            for change in hunk.iter_changes() {
                if change.tag() != ChangeTag::Equal {
                    continue;
                }

                let content = change.value().trim_end_matches(['\r', '\n']);
                if content.chars().any(is_escape_char) {
                    let line = change.new_index().unwrap() + 1;
                    notes.push(format!(
                        "file {} L{line} context: shown escaped",
                        file.path
                    ));
                }
            }
        }

        let old_ending = detect_file_ending(old_bytes);
        let new_ending = detect_file_ending(new_bytes);
        if old_ending == "CRLF" || new_ending == "CRLF" {
            notes.push(format!(
                "file {}: line endings: old {old_ending}, new {new_ending}",
                file.path
            ));
        }
    }

    if !notes.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "=== notes ===").unwrap();
        for note in notes {
            writeln!(out, "{note}").unwrap();
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_authority_inputs_are_strictly_bytes() {
        let old = b"fn main() {\n    println!(\"hello\");\n}\n";
        let new = b"fn main() {\n    println!(\"world\");\n}\n";

        let diff = render_file_diff("src/main.rs", old, new, RenderOptions::default());

        assert!(diff.contains("--- a/src/main.rs"));
        assert!(diff.contains("+++ b/src/main.rs"));
        assert!(diff.contains("-    println!(\"hello\");"));
        assert!(diff.contains("+    println!(\"world\");"));
    }

    #[test]
    fn test_review_visibility_trailing_whitespace() {
        let old = b"let x = 1;\n";
        let new = b"let x = 1; \n";

        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "test.rs".to_owned(),
                original: String::from_utf8(old.to_vec()).unwrap(),
                original_digest: String::new(),
                proposed: String::from_utf8(new.to_vec()).unwrap(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };
        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(
            diff.contains(MARKER_TRAILING_WHITESPACE),
            "adding trailing whitespace must show the {MARKER_TRAILING_WHITESPACE} marker in notes, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_visibility_tab_vs_space_indent() {
        let old = b"\tprintln!(\"indent\");\n";
        let new = b"    println!(\"indent\");\n";

        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "test.rs".to_owned(),
                original: String::from_utf8(old.to_vec()).unwrap(),
                original_digest: String::new(),
                proposed: String::from_utf8(new.to_vec()).unwrap(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };
        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(
            diff.contains(MARKER_TAB_INDENT),
            "removing tab indent must show {MARKER_TAB_INDENT} in notes, got:\n{diff}"
        );
        assert!(
            !diff.contains("[space-indent]"),
            "ordinary space indentation must not produce a note, got:\n{diff}"
        );
    }

    #[test]
    fn test_escape_literal_sequence_alongside_zero_width() {
        assert_eq!(
            escape_line_content("literal \\u{41} hidden \u{200b}"),
            "literal \\\\u{41} hidden \\u{200b}"
        );
    }

    #[test]
    fn test_literal_backslashes_remain_raw_without_escape_characters() {
        let source = r"literal \u{41} and \\ and \anything";
        assert_eq!(escape_line_content(source), source);
    }

    #[test]
    fn test_review_visibility_zero_width() {
        let old = b"let msg = \"hello\";\n";
        let new = "let msg = \"hello\u{200b}\";\n".as_bytes();

        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "test.rs".to_owned(),
                original: String::from_utf8(old.to_vec()).unwrap(),
                original_digest: String::new(),
                proposed: String::from_utf8(new.to_vec()).unwrap(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };
        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(
            diff.contains(MARKER_ZERO_WIDTH),
            "inserting zero-width char must show {MARKER_ZERO_WIDTH} in notes, got:\n{diff}"
        );
        assert!(
            diff.contains("\\u{200b}"),
            "zero-width char must be displayed as an escape sequence, got:\n{diff}"
        );
        assert!(
            diff.contains("file test.rs L1 +: shown escaped"),
            "escaped added line must be identified in notes, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_visibility_escaped_context() {
        let old = "let hidden = \"a\u{200b}b\";\nlet value = 1;\nlet tail = 0;\n";
        let new = "let hidden = \"a\u{200b}b\";\nlet value = 2;\nlet tail = 0;\n";

        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "test.rs".to_owned(),
                original: old.to_owned(),
                original_digest: String::new(),
                proposed: new.to_owned(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };
        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(
            !diff.contains("a\u{200b}b"),
            "raw zero-width context must not appear in rendered source"
        );
        assert!(
            diff.contains("a\\u{200b}b"),
            "context zero-width char must be displayed as an escape sequence, got:\n{diff}"
        );
        assert!(
            diff.contains("file test.rs L1 context: shown escaped"),
            "escaped context line must be identified in notes, got:\n{diff}"
        );
    }

    #[test]
    fn test_escape_mixed_character_classes_together() {
        assert_eq!(
            escape_line_content("literal \\u{41} \u{202e}\u{1}\u{200b}"),
            "literal \\\\u{41} \\u{202e}\\u{1}\\u{200b}"
        );
    }

    #[test]
    fn test_review_visibility_bidi() {
        let old = b"let text = \"abc\";\n";
        let new = "let text = \"abc\u{202e}\";\n".as_bytes();

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains("\\u{202e}"),
            "bidi char must be displayed as an escape sequence, got:\n{diff}"
        );

        for (source, expected) in [
            ("\u{00ad}", "\\u{ad}"),
            ("\u{00a0}", "\\u{a0}"),
            ("\u{202f}", "\\u{202f}"),
            ("\u{3000}", "\\u{3000}"),
            ("\u{180e}", "\\u{180e}"),
            ("\u{0085}", "\\u{85}"),
            ("\u{2028}", "\\u{2028}"),
            ("\u{2029}", "\\u{2029}"),
            ("\r", "\\u{d}"),
        ] {
            assert_eq!(
                escape_line_content(source),
                expected,
                "unexpected escaping for {source:?}"
            );

            if source != "\r" {
                let new = format!("value = \"{source}\";\n");
                let diff = render_file_diff(
                    "test.rs",
                    b"value = \"safe\";\n",
                    new.as_bytes(),
                    RenderOptions::default(),
                );
                assert!(
                    diff.contains(expected),
                    "expected rendered diff to contain {expected:?} for {source:?}, got:\n{diff}"
                );
            }
        }
    }

    #[test]
    fn test_review_visibility_no_newline_at_eof() {
        let old = b"line 1\nline 2";
        let new = b"line 1\nline 2 modified";

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains(NO_NEWLINE_MARKER),
            "missing final newline must show '{NO_NEWLINE_MARKER}', got:\n{diff}"
        );
    }
    #[test]
    fn test_review_visibility_preserves_lone_cr_at_eof() {
        let old = b"safe\n";
        let new = b"unsafe\r";

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.lines().any(|line| line == "+unsafe\\u{d}"),
            "a trailing lone CR must be escaped, not discarded, got:\n{diff}"
        );
        assert!(
            diff.contains(NO_NEWLINE_MARKER),
            "unterminated final line must retain the no-newline marker, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_per_file_header_and_summary() {
        let old = "first\r\nsecond\r\n";
        let new = "first\r\nthird\r\nfourth\r\n";
        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "crlf.txt".to_string(),
                original: old.to_string(),
                original_digest: String::new(),
                proposed: new.to_string(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };

        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(diff.starts_with("cvb surgery (dry run): 0 ops, 1 file\n"));
        assert!(diff.contains("--- a/crlf.txt"));
        assert!(diff.contains("+++ b/crlf.txt"));
        assert!(
            diff.contains("file crlf.txt: Summary: -1 lines, +2 lines; bytes: 15 -> 22"),
            "diff summary mismatch, got:\n{diff}"
        );
        assert!(
            diff.contains("file crlf.txt: line endings: old CRLF, new CRLF"),
            "CRLF note missing, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_hunk_header_includes_operation_label() {
        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "example.rs".to_string(),
                original: "first();\nsecond();\nthird();\n".to_string(),
                original_digest: String::new(),
                proposed: "first();\nchanged();\nthird();\n".to_string(),
                proposed_digest: String::new(),
                operations: vec![Operation::Replace {
                    start: 2,
                    end: 2,
                    old: vec!["second();".to_string()],
                    new: vec!["changed();".to_string()],
                }],
            }],
        };

        let diff = render_plan_diff(&plan, RenderOptions::default());
        assert!(
            diff.lines().any(|line| {
                line.starts_with("@@ ")
                    && line.contains("op 1 replace [exact @ L2]")
            }),
            "operation label must appear on its hunk header, got:\n{diff}"
        );
    }

    #[test]
    fn test_changed_lines_have_no_separator_after_gutter() {
        let old = b"    old();\n";
        let new = b"    new();\n";
        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.lines().any(|line| line == "-    old();"),
            "removed source must start immediately after the gutter, got:\n{diff}"
        );
        assert!(
            diff.lines().any(|line| line == "+    new();"),
            "added source must start immediately after the gutter, got:\n{diff}"
        );
    }

    #[test]
    fn test_canonical_hunk_header_counts() {
        assert_eq!(
            canonical_hunk_header("@@ -5,1 +5,1 @@\n"),
            "@@ -5 +5 @@\n"
        );
        assert_eq!(
            canonical_hunk_header("@@ -0,0 +1,1 @@\n"),
            "@@ -0,0 +1 @@\n"
        );
        assert_eq!(
            canonical_hunk_header("@@ -5,3 +5,2 @@\n"),
            "@@ -5,3 +5,2 @@\n"
        );
        assert_eq!(
            canonical_hunk_header("@@ -5 +5 @@\n"),
            "@@ -5 +5 @@\n"
        );
    }

    #[test]
    fn test_rendered_diff_round_trips_escaped_crlf_and_unterminated_eof() {
        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "roundtrip.txt".to_string(),
                original: "before\r\nold\u{200b}\r\nlast".to_string(),
                original_digest: String::new(),
                proposed: "before\r\nnew\u{200b}\r\nlast changed".to_string(),
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };

        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(
            diff.contains("file roundtrip.txt L2 -: shown escaped"),
            "removed escaped line must be noted, got:\n{diff}"
        );
        assert!(
            diff.contains("file roundtrip.txt L2 +: shown escaped"),
            "added escaped line must be noted, got:\n{diff}"
        );
        assert!(
            diff.contains("line endings: old CRLF, new CRLF"),
            "CRLF line endings must be noted, got:\n{diff}"
        );
        assert!(
            diff.contains(NO_NEWLINE_MARKER),
            "unterminated EOF must be visible, got:\n{diff}"
        );

        let parsed = crate::surgery::diff_parse::parse_diff(&diff)
            .expect("rendered diff must parse successfully");
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed[0]
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.text.contains(r"\u{200b}")),
            "escaped Unicode must survive the render/parse round trip"
        );
    }

    #[test]
    fn test_render_large_inputs() {
        let mut old = String::with_capacity(5000 * 30);
        let mut new = String::with_capacity(5000 * 30);

        for i in 1..=5000 {
            old.push_str(&format!("line {i} original content\n"));
            if i % 500 == 0 {
                new.push_str(&format!("line {i} changed content\n"));
            } else {
                new.push_str(&format!("line {i} original content\n"));
            }
        }

        let plan = Plan {
            token: String::new(),
            id: String::new(),
            digest: String::new(),
            files: vec![crate::surgery::PlannedFile {
                path: "large.rs".to_string(),
                original: old,
                original_digest: String::new(),
                proposed: new,
                proposed_digest: String::new(),
                operations: vec![],
            }],
        };
        let diff = render_plan_diff(&plan, RenderOptions::default());

        assert!(diff.contains("file large.rs: Summary: -10 lines, +10 lines"));
        assert!(diff.contains("-line 500 original content"));
        assert!(diff.contains("+line 500 changed content"));
    }
}

use std::fmt::Write as _;

use similar::{ChangeTag, TextDiff};

use crate::surgery::{detect_newline, Newline, Plan};

pub(crate) const MARKER_TRAILING_WHITESPACE: &str = "[trailing-whitespace]";
pub(crate) const MARKER_TAB_INDENT: &str = "[tab-indent]";
pub(crate) const MARKER_SPACE_INDENT: &str = "[space-indent]";
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

pub(crate) fn is_control_char(c: char) -> bool {
    let code = c as u32;
    (code < 32 && c != '\t' && c != '\r' && c != '\n')
        || code == 127
        || (0x80..=0x9F).contains(&code)
}

fn escape_line_content(s: &str) -> String {
    let transformed = s.chars().any(|c| {
        is_bidi_override(c) || is_zero_width(c) || is_control_char(c)
    });
    if !transformed {
        return s.to_owned();
    }

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' {
            out.push_str("\\\\");
        } else if is_bidi_override(c) || is_zero_width(c) || is_control_char(c) {
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
    } else if leading_ws.contains(' ') {
        markers.push(MARKER_SPACE_INDENT);
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

/// Renders a unified diff for one file purely from its original and planned bytes.
pub(crate) fn render_file_diff(
    path: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
    options: RenderOptions,
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
        let header_str = hunk.header().to_string();
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
            let content = value.trim_end_matches(['\r', '\n']);
            let sign = match tag {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };

            match tag {
                ChangeTag::Equal => {
                    writeln!(out, " {content}").unwrap();
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

            if change.missing_newline() {
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

    writeln!(
        out,
        "Legend: column 1 is the gutter (' ' context, '-' removed, '+' added). Source text starts at column 2. Lines named in the notes as \"shown escaped\" are transformed (see notes). Text after \"=== notes ===\" is commentary, never source."
    )
    .unwrap();

    for file in &plan.files {
        let old_bytes = file.original.as_bytes();
        let new_bytes = file.proposed.as_bytes();
        let file_diff = render_file_diff(&file.path, old_bytes, new_bytes, options);

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

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains(MARKER_TRAILING_WHITESPACE),
            "adding trailing whitespace must show the {MARKER_TRAILING_WHITESPACE} marker, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_visibility_tab_vs_space_indent() {
        let old = b"\tprintln!(\"indent\");\n";
        let new = b"    println!(\"indent\");\n";

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains(MARKER_TAB_INDENT),
            "removing tab indent must show {MARKER_TAB_INDENT}, got:\n{diff}"
        );
        assert!(
            diff.contains(MARKER_SPACE_INDENT),
            "adding space indent must show {MARKER_SPACE_INDENT}, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_visibility_zero_width() {
        let old = b"let msg = \"hello\";\n";
        let new = "let msg = \"hello\u{200b}\";\n".as_bytes();

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains(MARKER_ZERO_WIDTH),
            "inserting zero-width char must show {MARKER_ZERO_WIDTH}, got:\n{diff}"
        );
        assert!(
            diff.contains("\\u{200b}"),
            "zero-width char must be displayed as an escape sequence, got:\n{diff}"
        );
    }

    #[test]
    fn test_review_visibility_bidi() {
        let old = b"let text = \"abc\";\n";
        let new = "let text = \"abc\u{202e}\";\n".as_bytes();

        let diff = render_file_diff("test.rs", old, new, RenderOptions::default());

        assert!(
            diff.contains(MARKER_BIDI),
            "inserting bidi char must show {MARKER_BIDI}, got:\n{diff}"
        );
        assert!(
            diff.contains("\\u{202e}"),
            "bidi char must be displayed as an escape sequence, got:\n{diff}"
        );
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
    fn test_review_per_file_header_and_summary() {
        let old = b"first\r\nsecond\r\n";
        let new = b"first\r\nthird\r\nfourth\r\n";

        let diff = render_file_diff("crlf.txt", old, new, RenderOptions::default());

        assert!(diff.contains("--- a/crlf.txt (CRLF)"));
        assert!(diff.contains("+++ b/crlf.txt (CRLF)"));
        assert!(
            diff.contains("Summary: -1 lines, +2 lines; bytes: 15 -> 22"),
            "diff summary mismatch, got:\n{diff}"
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

        let diff = render_file_diff(
            "large.rs",
            old.as_bytes(),
            new.as_bytes(),
            RenderOptions::default(),
        );

        assert!(diff.contains("Summary: -10 lines, +10 lines"));
        assert!(diff.contains("- line 500 original content"));
        assert!(diff.contains("+ line 500 changed content"));
    }
}

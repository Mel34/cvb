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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InsertPosition {
    Before,
    After,
}

#[derive(Debug, PartialEq, Eq)]
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

pub(crate) fn valid_token(token: &str) -> bool {
    (8..=32).contains(&token.len())
        && token.bytes().all(|b| b.is_ascii_alphanumeric())
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

    if path.is_empty()
        || path.starts_with('/')
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(ParseError::InvalidPath);
    }

    Ok(path.to_string())
}

fn read_payload<'a>(
    lines: &'a [&'a str],
    index: &mut usize,
    token: &str,
    count: usize,
) -> Result<Vec<String>, ParseError> {
    let mut payload = Vec::with_capacity(count);

    for _ in 0..count {
        let line = lines.get(*index).ok_or(ParseError::Truncated)?;

        if line.starts_with(&format!("CVB::{token}::")) {
            return Err(ParseError::Collision);
        }

        if line.contains('\0') || line.contains('\r') {
            return Err(ParseError::InvalidBytes);
        }

        payload.push((*line).to_owned());
        *index += 1;
    }

    Ok(payload)
}

fn parse_operation(
    lines: &[&str],
    index: &mut usize,
    token: &str,
) -> Result<Operation, ParseError> {
    let line = lines.get(*index).ok_or(ParseError::Truncated)?;

    if line.starts_with(&format!("CVB::{token}::OP replace ")) {
        parse_replace(lines, index, token)
    } else if line.starts_with(&format!("CVB::{token}::OP delete ")) {
        parse_delete(lines, index, token)
    } else if line.starts_with(&format!("CVB::{token}::OP insert ")) {
        parse_insert(lines, index, token)
    } else {
        Err(ParseError::Syntax)
    }
}

fn parse_replace(
    lines: &[&str],
    index: &mut usize,
    token: &str,
) -> Result<Operation, ParseError> {
    let prefix = format!("CVB::{token}::OP replace lines=");
    let line = lines.get(*index).ok_or(ParseError::Truncated)?;
    let range_text = line.strip_prefix(&prefix).ok_or(ParseError::Syntax)?;
    let (start, end) = parse_range(range_text)?;
    *index += 1;

    let old_header = lines.get(*index).ok_or(ParseError::Truncated)?;
    let old_prefix = format!("CVB::{token}::OLD n=");
    let old_count_text = old_header
        .strip_prefix(&old_prefix)
        .ok_or(ParseError::Syntax)?;
    let old_count = parse_count(old_count_text)?;
    *index += 1;

    if old_count != end - start + 1 {
        return Err(ParseError::Count);
    }

    let old = read_payload(lines, index, token, old_count)?;

    let new_header = lines.get(*index).ok_or(ParseError::Truncated)?;
    let new_prefix = format!("CVB::{token}::NEW n=");
    let new_count_text = new_header
        .strip_prefix(&new_prefix)
        .ok_or(ParseError::Syntax)?;
    let new_count = parse_count(new_count_text)?;
    *index += 1;

    if new_count == 0 {
        return Err(ParseError::Count);
    }

    let new = read_payload(lines, index, token, new_count)?;
    consume_endop(lines, index, token)?;

    Ok(Operation::Replace {
        start,
        end,
        old,
        new,
    })
}

fn parse_delete(
    lines: &[&str],
    index: &mut usize,
    token: &str,
) -> Result<Operation, ParseError> {
    let prefix = format!("CVB::{token}::OP delete lines=");
    let line = lines.get(*index).ok_or(ParseError::Truncated)?;
    let range_text = line.strip_prefix(&prefix).ok_or(ParseError::Syntax)?;
    let (start, end) = parse_range(range_text)?;
    *index += 1;

    let header = lines.get(*index).ok_or(ParseError::Truncated)?;
    let count_text = header
        .strip_prefix(&format!("CVB::{token}::OLD n="))
        .ok_or(ParseError::Syntax)?;
    let count = parse_count(count_text)?;
    *index += 1;

    if count != end - start + 1 {
        return Err(ParseError::Count);
    }

    let old = read_payload(lines, index, token, count)?;
    consume_endop(lines, index, token)?;

    Ok(Operation::Delete { start, end, old })
}

fn parse_insert(
    lines: &[&str],
    index: &mut usize,
    token: &str,
) -> Result<Operation, ParseError> {
    let line = lines.get(*index).ok_or(ParseError::Truncated)?;
    let (position, range_text) =
        if let Some(value) = line.strip_prefix(&format!("CVB::{token}::OP insert after=")) {
            (InsertPosition::After, value)
        } else if let Some(value) =
            line.strip_prefix(&format!("CVB::{token}::OP insert before="))
        {
            (InsertPosition::Before, value)
        } else {
            return Err(ParseError::Syntax);
        };

    let (start, end) = parse_range(range_text)?;
    *index += 1;

    let header = lines.get(*index).ok_or(ParseError::Truncated)?;
    let count_text = header
        .strip_prefix(&format!("CVB::{token}::ANCHOR n="))
        .ok_or(ParseError::Syntax)?;
    let count = parse_count(count_text)?;
    *index += 1;

    if count != end - start + 1 {
        return Err(ParseError::Count);
    }

    let anchor = read_payload(lines, index, token, count)?;

    let header = lines.get(*index).ok_or(ParseError::Truncated)?;
    let new_count_text = header
        .strip_prefix(&format!("CVB::{token}::NEW n="))
        .ok_or(ParseError::Syntax)?;
    let new_count = parse_count(new_count_text)?;
    *index += 1;

    if new_count == 0 {
        return Err(ParseError::Count);
    }

    let new = read_payload(lines, index, token, new_count)?;
    consume_endop(lines, index, token)?;

    Ok(Operation::Insert {
        position,
        start,
        end,
        anchor,
        new,
    })
}

fn consume_endop(
    lines: &[&str],
    index: &mut usize,
    token: &str,
) -> Result<(), ParseError> {
    let line = lines.get(*index).ok_or(ParseError::Truncated)?;
    if *line != format!("CVB::{token}::ENDOP") {
        return Err(if *index + 1 == lines.len() {
            ParseError::Truncated
        } else {
            ParseError::Syntax
        });
    }

    *index += 1;
    Ok(())
}

pub(crate) fn parse_envelope(input: &str) -> Result<Envelope, ParseError> {
    const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
    const MAX_OPERATIONS: usize = 500;

    if input.len() > MAX_INPUT_BYTES {
        return Err(ParseError::Limit);
    }
    let normalized = input.replace("\r\n", "\n");
    let mut lines: Vec<&str> = normalized.split('\n').collect();

    // Ignore the single empty split item caused by a final newline.
    if lines.last() == Some(&"") {
        lines.pop();
    }

    let begin_index = lines
        .iter()
        .position(|line| is_begin_shaped(line))
        .ok_or(ParseError::NoEnvelope)?;

    let (token, id) = parse_begin(lines[begin_index])?;
    let mut files = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();
    let mut index = begin_index + 1;
    let mut operation_count = 0usize;

    while index < lines.len() {
        let line = lines[index];

        if line.starts_with(&format!("CVB::{token}::END ")) {
            let value = match line.strip_prefix(&format!("CVB::{token}::END ops=")) {
                Some(value) => value,
                None => {
                    return Err(if index + 1 == lines.len() {
                        ParseError::Truncated
                    } else {
                        ParseError::Syntax
                    });
                }
            };
            let declared_count = match parse_count(value) {
                Ok(count) => count,
                Err(_) if index + 1 == lines.len() => {
                    return Err(ParseError::Truncated);
                }
                Err(error) => return Err(error),
            };

            if declared_count != operation_count || operation_count == 0 {
                return Err(ParseError::Syntax);
            }

            if files.is_empty()
                || files
                    .iter()
                    .any(|file: &FileSection| file.operations.is_empty())
            {
                return Err(ParseError::Syntax);
            }

            if lines[index + 1..]
                .iter()
                .any(|line| is_begin_shaped(line))
            {
                return Err(ParseError::MultipleEnvelopes);
            }

            return Ok(Envelope {
                token,
                id,
                files,
            });
        }

        if line.starts_with(&format!("CVB::{token}::OP ")) {
            let operation = parse_operation(&lines, &mut index, &token)?;
            let file = files.last_mut().ok_or(ParseError::Syntax)?;
            file.operations.push(operation);
            operation_count += 1;
            if operation_count > MAX_OPERATIONS {
                return Err(ParseError::Limit);
            }
            continue;
        }

        if line.starts_with("CVB::") {
            let path = parse_file(line, &token)?;

            if !seen_paths.insert(path.clone()) {
                return Err(ParseError::DuplicateFile);
            }

            files.push(FileSection {
                path,
                operations: Vec::new(),
            });
            index += 1;
            continue;
        }

        return Err(ParseError::Stray);
    }

    Err(ParseError::Truncated)
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
            assert_eq!(
                parse_count(invalid),
                Err(ParseError::Syntax),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn range_requires_two_ordered_positive_numbers() {
        assert_eq!(parse_range("10-10"), Ok((10, 10)));
        assert_eq!(parse_range("2-7"), Ok((2, 7)));

        assert_eq!(parse_range("0-1"), Err(ParseError::Range));
        assert_eq!(parse_range("4-3"), Err(ParseError::Range));

        for invalid in ["", "1", "1-2-3", "01-2", "1-02", "1 -2", "1- 2"] {
            assert_eq!(
                parse_range(invalid),
                Err(ParseError::Syntax),
                "{invalid:?}"
            );
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

        assert_eq!(parse_envelope(input), Err(ParseError::Truncated));
    }

    #[test]
    fn malformed_begin_header_is_syntax_not_no_envelope() {
        assert_eq!(
            parse_envelope("CVB::k7Q2xw9R::BEGIN \\n"),
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

        assert_eq!(parse_envelope(input), Err(ParseError::InvalidBytes));
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

        for payload_line in [
            "CVB::k7Q2xw9R::FIELD\0",
            "CVB::k7Q2xw9R::FIELD\r",
        ] {
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

        assert_eq!(parse_envelope(input), Err(ParseError::DuplicateFile));
    }


    #[test]
    fn rejects_input_over_four_mibibytes() {
        let input = "x".repeat(4 * 1024 * 1024 + 1);
        assert_eq!(parse_envelope(&input), Err(ParseError::Limit));
    }

    #[test]
    fn accepts_input_at_four_mibibytes_before_normalization() {
        let mut input = "x".repeat(4 * 1024 * 1024);
        assert_eq!(parse_envelope(&input), Err(ParseError::NoEnvelope));
        input.push('x');
        assert_eq!(parse_envelope(&input), Err(ParseError::Limit));
    }

    #[test]
    fn rejects_more_than_five_hundred_operations() {
        let mut input = String::from(
            "CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\n\
             CVB::k7Q2xw9R::FILE path=src/main.rs\n"
        );

        for _ in 0..501 {
            input.push_str(
                "CVB::k7Q2xw9R::OP replace lines=1-1\n\
                 CVB::k7Q2xw9R::OLD n=1\n\
                 old\n\
                 CVB::k7Q2xw9R::NEW n=1\n\
                 new\n\
                 CVB::k7Q2xw9R::ENDOP\n"
            );
        }

        input.push_str("CVB::k7Q2xw9R::END ops=501");
        assert_eq!(parse_envelope(&input), Err(ParseError::Limit));
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
        assert_eq!(parse_envelope("nothing here\n"), Err(ParseError::NoEnvelope));
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
            parse_envelope(input),
            Err(ParseError::MultipleEnvelopes)
        );
    }

    #[test]
    fn malformed_final_end_line_is_truncated() {
        let input = "\\
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

        assert_eq!(parse_envelope(input), Err(ParseError::Truncated));
    }

    #[test]
    fn malformed_end_line_before_more_input_is_syntax() {
        let input = "\\
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

        assert_eq!(parse_envelope(input), Err(ParseError::Syntax));
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

        assert_eq!(parse_envelope(input), Err(ParseError::Syntax));
    }


    #[test]
    fn malformed_final_endop_is_truncated() {
        let input = "\\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
CVB::k7Q2xw9R::NOT_ENDOP";

        assert_eq!(parse_envelope(input), Err(ParseError::Truncated));
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

        assert_eq!(parse_envelope(input), Err(ParseError::Syntax));
    }

    #[test]
    fn rejects_zero_declared_operations_as_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::END ops=0";

        assert_eq!(parse_envelope(input), Err(ParseError::Syntax));
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

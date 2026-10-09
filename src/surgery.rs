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

/// Reads the existing target files without modifying them.
///
/// Paths must be safe relative paths. Symlinks are rejected so a target cannot
/// redirect a read outside the supplied root.
pub(crate) fn load_originals(
    root: &std::path::Path,
    envelope: &Envelope,
) -> Result<std::collections::HashMap<String, String>, String> {
    use std::path::{Component, Path};

    let mut originals = std::collections::HashMap::new();

    for section in &envelope.files {
        let relative = Path::new(&section.path);

        if section.path.is_empty()
            || section.path.contains('\\')
            || relative.is_absolute()
            || !relative.components().all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(format!("CVB: unsafe target path '{}'", section.path));
        }

        if originals.contains_key(&section.path) {
            return Err(format!("CVB: duplicate target path '{}'", section.path));
        }

        let mut current = root.to_path_buf();
        for component in relative.components() {
            current.push(component.as_os_str());
            let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
                format!("CVB: unable to inspect '{}': {error}", current.display())
            })?;

            if metadata.file_type().is_symlink() {
                return Err(format!("CVB: symlink target is not allowed: '{}'", current.display()));
            }
        }

        let metadata = std::fs::metadata(&current)
            .map_err(|error| format!("CVB: unable to inspect '{}': {error}", current.display()))?;
        if !metadata.is_file() {
            return Err(format!("CVB: target is not a regular file: '{}'", current.display()));
        }

        let contents = std::fs::read_to_string(&current)
            .map_err(|error| format!("CVB: unable to read '{}': {error}", current.display()))?;

        originals.insert(section.path.clone(), contents);
    }

    Ok(originals)
}

/// Error from parsing, loading, or planning a surgery envelope.
#[derive(Debug)]
pub(crate) enum PipelineError {
    Parse(ParseError),
    Load(String),
    Plan(PlanError),
}

/// Parses an envelope, reads its target files, and builds an in-memory plan.
/// This function never writes to disk.
pub(crate) fn parse_load_plan(
    root: &std::path::Path,
    input: &str,
) -> Result<(Envelope, Plan), PipelineError> {
    let envelope = parse_envelope(input).map_err(PipelineError::Parse)?;
    let originals = load_originals(root, &envelope).map_err(PipelineError::Load)?;
    let plan =
        plan_envelope(&envelope, &originals).map_err(PipelineError::Plan)?;

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
#[derive(Debug, Clone, PartialEq, Eq)]
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
pub(crate) fn apply_operation(
    original: &[String],
    operation: &Operation,
) -> Result<Vec<String>, PlanError> {
    if original.is_empty() {
        return Err(PlanError::InvalidFile);
    }

    let mut proposed = original.to_vec();

    match operation {
        Operation::Replace { start, end, old, new } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err(PlanError::Range);
            }
            if new.is_empty() {
                return Err(PlanError::InvalidFile);
            }

            let first = start - 1;
            let last = *end;

            if original[first..last] != old[..] {
                return Err(PlanError::Precondition);
            }
            if old == new {
                return Err(PlanError::NoOp);
            }

            proposed.splice(first..last, new.iter().cloned());
        }

        Operation::Delete { start, end, old } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err(PlanError::Range);
            }

            let first = start - 1;
            let last = *end;

            if original[first..last] != old[..] {
                return Err(PlanError::Precondition);
            }
            if first == 0 && last == original.len() {
                return Err(PlanError::EndOfFile);
            }

            proposed.drain(first..last);
        }

        Operation::Insert { position, start, end, anchor, new } => {
            if *start == 0 || start > end || *end > original.len() {
                return Err(PlanError::Range);
            }
            if new.is_empty() {
                return Err(PlanError::InvalidFile);
            }

            let first = start - 1;
            let last = *end;

            if original[first..last] != anchor[..] {
                return Err(PlanError::Precondition);
            }

            if (matches!(position, InsertPosition::Before) && first == 0)
                || (matches!(position, InsertPosition::After) && last == original.len())
            {
                return Err(PlanError::EndOfFile);
            }

            let insertion_index = match position {
                InsertPosition::Before => first,
                InsertPosition::After => last,
            };

            proposed.splice(insertion_index..insertion_index, new.iter().cloned());
        }
    }

    Ok(proposed)
}

#[cfg(test)]
mod apply_operation_tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn applies_replacement_in_memory() {
        let original = lines(&["one", "old", "three"]);
        let operation = Operation::Replace {
            start: 2,
            end: 2,
            old: lines(&["old"]),
            new: lines(&["new"]),
        };

        assert_eq!(
            apply_operation(&original, &operation),
            Ok(lines(&["one", "new", "three"]))
        );
        assert_eq!(original, lines(&["one", "old", "three"]));
    }

    #[test]
    fn applies_deletion_in_memory() {
        let original = lines(&["one", "remove", "three"]);
        let operation = Operation::Delete {
            start: 2,
            end: 2,
            old: lines(&["remove"]),
        };

        assert_eq!(
            apply_operation(&original, &operation),
            Ok(lines(&["one", "three"]))
        );
    }

    #[test]
    fn inserts_before_anchor_in_memory() {
        let original = lines(&["one", "anchor", "three"]);
        let operation = Operation::Insert {
            position: InsertPosition::Before,
            start: 2,
            end: 2,
            anchor: lines(&["anchor"]),
            new: lines(&["inserted"]),
        };

        assert_eq!(
            apply_operation(&original, &operation),
            Ok(lines(&["one", "inserted", "anchor", "three"]))
        );
    }

    #[test]
    fn inserts_after_anchor_in_memory() {
        let original = lines(&["one", "anchor", "three"]);
        let operation = Operation::Insert {
            position: InsertPosition::After,
            start: 2,
            end: 2,
            anchor: lines(&["anchor"]),
            new: lines(&["inserted"]),
        };

        assert_eq!(
            apply_operation(&original, &operation),
            Ok(lines(&["one", "anchor", "inserted", "three"]))
        );
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
            apply_operation(&original, &operation),
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
            apply_operation(&original, &zero_start),
            Err(PlanError::Range)
        );

        let past_end = Operation::Delete {
            start: 2,
            end: 3,
            old: lines(&["two", "three"]),
        };
        assert_eq!(
            apply_operation(&original, &past_end),
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
            apply_operation(&original, &operation),
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
            apply_operation(&original, &operation),
            Err(PlanError::EndOfFile)
        );
    }
}


/// Builds a complete in-memory plan without writing to disk.
/// All operation ranges refer to the original contents of each file.
pub(crate) fn plan_envelope(
    envelope: &Envelope,
    originals: &std::collections::HashMap<String, String>,
) -> Result<Plan, PlanError> {
    use std::path::{Component, Path};

    let mut seen = std::collections::HashSet::new();
    let mut planned_files = Vec::with_capacity(envelope.files.len());

    for section in &envelope.files {
        let file_path = Path::new(&section.path);

        if section.path.is_empty()
            || section.path.contains('\\')
            || file_path.is_absolute()
            || !file_path.components().all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(PlanError::InvalidPath);
        }

        if !seen.insert(section.path.as_str()) {
            return Err(PlanError::DuplicateFile);
        }

        let original = originals
            .get(&section.path)
            .ok_or(PlanError::MissingFile)?;

        if original.is_empty() {
            return Err(PlanError::InvalidFile);
        }

        // Preserve the file's newline convention. Reject mixed line endings
        // and lone carriage returns instead of silently rewriting the file.
        let crlf_count = original.matches("\r\n").count();
        let remaining = original.replace("\r\n", "");

        if remaining.contains('\r')
            || (crlf_count > 0 && remaining.contains('\n'))
        {
            return Err(PlanError::InvalidFile);
        }

        let newline = if crlf_count > 0 { "\r\n" } else { "\n" };
        let has_final_newline = original.ends_with(newline);
        let content = if has_final_newline {
            original.strip_suffix(newline).unwrap()
        } else {
            original.as_str()
        };

        let original_lines: Vec<String> =
            content.split(newline).map(str::to_owned).collect();

        // Each edit span is represented as a half-open range of original
        // line positions. Insertions have an empty range at their insertion
        // point. Boundary-touching edits are conservatively considered
        // conflicting to avoid ambiguous ordering.
        let mut edits: Vec<(usize, usize, &Operation)> = Vec::new();

        for operation in &section.operations {
            let insertion_at_boundary = match operation {
                Operation::Insert {
                    position: InsertPosition::Before,
                    start,
                    ..
                } => *start == 1,
                Operation::Insert {
                    position: InsertPosition::After,
                    end,
                    ..
                } => *end == original_lines.len(),
                _ => false,
            };

            if insertion_at_boundary {
                return Err(PlanError::EndOfFile);
            }

            // Validate range, payload preconditions, and operation-specific
            // safety rules against the unchanged original file.
            apply_operation(&original_lines, operation)?;

            let span = match operation {
                Operation::Replace { start, end, .. }
                | Operation::Delete { start, end, .. } => (*start - 1, *end),
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
                    return Err(PlanError::Overlap);
                }

                let overlaps = match (a_insert, b_insert) {
                    (true, true) => a_start == b_start,
                    (true, false) => a_start > b_start && a_start < b_end,
                    (false, true) => b_start > a_start && b_start < a_end,
                    (false, false) => a_start < b_end && b_start < a_end,
                };

                if overlaps {
                    return Err(PlanError::Overlap);
                }
            }
        }

        // Work from the bottom upward so every edit retains its original
        // line coordinates.
        edits.sort_by(|a, b| b.0.cmp(&a.0));

        let mut proposed_lines = original_lines;
        for (_, _, operation) in edits {
            match operation {
                Operation::Replace { start, end, new, .. } => {
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
            return Err(PlanError::NoOp);
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
            plan_envelope(&envelope, &originals("one\ntwo\nthree\nfour\n")),
            Err(PlanError::Overlap)
        );
    }

    #[test]
    fn rejects_insertion_after_final_line() {
        let envelope = make_envelope(vec![
            Operation::Insert {
                position: InsertPosition::After,
                start: 3,
                end: 3,
                anchor: vec!["three".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            plan_envelope(&envelope, &originals("one\ntwo\nthree\n")),
            Err(PlanError::EndOfFile)
        );
    }

    #[test]
    fn rejects_insertion_before_first_line() {
        let envelope = make_envelope(vec![
            Operation::Insert {
                position: InsertPosition::Before,
                start: 1,
                end: 1,
                anchor: vec!["one".to_owned()],
                new: vec!["inserted".to_owned()],
            },
        ]);

        assert_eq!(
            plan_envelope(&envelope, &originals("one\\ntwo\\nthree\\n")),
            Err(PlanError::EndOfFile)
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
            plan_envelope(&envelope, &originals("one\ntwo\nthree\n")),
            Err(PlanError::Overlap)
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
            plan_envelope(&envelope, &std::collections::HashMap::new()),
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
            plan_envelope(&envelope, &originals("one\n")),
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
            plan_envelope(&envelope, &originals("one\r\ntwo\n")),
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

        let plan = plan_envelope(
            &envelope,
            &originals("one\ntwo\nthree\nfour\nfive\n"),
        )
        .unwrap();

        assert_eq!(
            plan.files[0].proposed,
            "one\nTWO\nTHREE\ninserted\nfour\nfive\n"
        );
    }

}



#[cfg(test)]
mod parse_load_plan_tests {
    use super::{parse_load_plan, ParseError, PipelineError};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn parse_load_plan_builds_plan_without_modifying_file() {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "cvb-parse-load-plan-{}",
            std::process::id()
        ));

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
            Err(PipelineError::Parse(ParseError::NoEnvelope)) => {}
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
            Err(PipelineError::Parse(ParseError::DuplicateFile)) => {}
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
            Err(PipelineError::Parse(ParseError::InvalidPath)) => {}
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
            other => panic!(
                "expected load error for symlinked parent directory, got {other:?}"
            ),
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
            Err(PipelineError::Load(_)) => {}
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
            Err(PipelineError::Plan(super::PlanError::InvalidFile)) => {}
            other => panic!(
                "expected InvalidFile planning error for empty target, got {other:?}"
            ),
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
    use super::{load_originals, Envelope, FileSection};
    use std::fs;
    use std::path::PathBuf;

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cvb-load-originals-{name}-{}",
            std::process::id()
        ));

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
        assert!(result.unwrap_err().contains("symlink target is not allowed"));
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside contents");

        fs::remove_dir_all(&root).unwrap();
        fs::remove_file(outside).unwrap();
    }
}

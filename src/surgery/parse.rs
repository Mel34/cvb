use super::*;
use std::collections::HashSet;

/// GNOME's Mutter silently truncates clipboard transfers over ~4 MB.
/// By setting `MAX_INPUT_BYTES` conservatively below this threshold (3 MiB),
/// pastes truncated by Mutter to ~4 MB arrive above `MAX_INPUT_BYTES` and are
/// rejected with `ParseError::Limit` (warning the user about clipboard truncation)
/// rather than reaching the parser. Incomplete envelopes below 3 MiB are caught
/// by the mandatory terminal `END ops=N` validation.
pub(crate) const CLIPBOARD_TRUNCATION_BYTES: usize = 4_000_000;
pub(crate) const MAX_INPUT_BYTES: usize = 3 * 1024 * 1024;

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

fn is_begin_shaped(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("CVB::") else {
        return false;
    };

    let Some((token, _suffix)) = rest.split_once("::BEGIN ") else {
        return false;
    };

    valid_token(token)
}

fn has_structural_shape(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("CVB::") else {
        return false;
    };
    let Some((token, _)) = rest.split_once("::") else {
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
        let line = self.lines.get(self.index).ok_or(ParseFailure {
            kind: ParseError::Truncated,
            line: line_num,
            detail: None,
        })?;
        self.index += 1;
        Ok(*line)
    }

    fn check_structural_line_after_block(
        &self,
        line: &str,
        line_num: usize,
    ) -> Result<(), ParseFailure> {
        if line.starts_with(&self.prefix) {
            return Ok(());
        }
        if has_structural_shape(line) {
            return Err(ParseFailure {
                kind: ParseError::Syntax,
                line: line_num,
                detail: Some("structural line has wrong token or indentation".into()),
            });
        }
        Err(ParseFailure {
            kind: ParseError::Count,
            line: line_num,
            detail: Some(
                "expected structural line directly after counted block; count may be too small"
                    .into(),
            ),
        })
    }

    fn parse_header_count(
        line: &str,
        prefix: &str,
        name: &str,
        line_num: usize,
    ) -> Result<Option<usize>, ParseFailure> {
        let exact = format!("{prefix}{name}");
        if line == exact {
            return Ok(None);
        }
        let counted_prefix = format!("{prefix}{name} n=");
        if let Some(count_text) = line.strip_prefix(&counted_prefix) {
            let count = parse_count(count_text).map_err(|kind| ParseFailure {
                kind,
                line: line_num,
                detail: None,
            })?;
            return Ok(Some(count));
        }
        Err(ParseFailure {
            kind: ParseError::Syntax,
            line: line_num,
            detail: None,
        })
    }

    fn expect_header_after_block(&mut self, name: &str) -> Result<Option<usize>, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.next_line()?;
        self.check_structural_line_after_block(line, line_num)?;
        Self::parse_header_count(line, &self.prefix, name, line_num)
    }

    fn check_structural_line_elsewhere(
        &self,
        line: &str,
        line_num: usize,
    ) -> Result<(), ParseFailure> {
        if line.starts_with(&self.prefix) {
            return Ok(());
        }
        if has_structural_shape(line) {
            return Err(ParseFailure {
                kind: ParseError::Syntax,
                line: line_num,
                detail: Some("structural line has wrong token or indentation".into()),
            });
        }
        Err(ParseFailure {
            kind: ParseError::Stray,
            line: line_num,
            detail: None,
        })
    }

    fn expect_header(&mut self, name: &str) -> Result<Option<usize>, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.next_line()?;
        self.check_structural_line_elsewhere(line, line_num)?;
        Self::parse_header_count(line, &self.prefix, name, line_num)
    }

    fn consume_endop_after_block(&mut self) -> Result<(), ParseFailure> {
        let is_last_line = self.index + 1 == self.lines.len();
        let line_num = self.index + 1;
        let line = self.next_line()?;
        self.check_structural_line_after_block(line, line_num)?;
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

    fn read_block(
        &mut self,
        count: Option<usize>,
        header_line: usize,
    ) -> Result<Vec<String>, ParseFailure> {
        match count {
            Some(count) => {
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
                    let line = self.lines.get(self.index).ok_or(ParseFailure {
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
            None => {
                let mut payload = Vec::new();

                loop {
                    let line_num = self.index + 1;
                    let Some(line) = self.peek() else {
                        return Err(ParseFailure {
                            kind: ParseError::Truncated,
                            line: self.lines.len(),
                            detail: None,
                        });
                    };

                    if line.starts_with(&self.prefix) {
                        break;
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

                if payload.is_empty() {
                    return Err(ParseFailure {
                        kind: ParseError::Count,
                        line: header_line,
                        detail: Some("block is empty; expected at least 1 line".into()),
                    });
                }

                Ok(payload)
            }
        }
    }

    fn parse_operation(&mut self) -> Result<Operation, ParseFailure> {
        let line_num = self.index + 1;
        let line = self.peek().ok_or(ParseFailure {
            kind: ParseError::Truncated,
            line: line_num,
            detail: None,
        })?;
        self.check_structural_line_elsewhere(line, line_num)?;
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
        let range_text = line.strip_prefix(&prefix).ok_or(ParseFailure {
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
        if let Some(count) = old_count {
            if count != expected_old {
                return Err(ParseFailure {
                    kind: ParseError::Count,
                    line: old_line,
                    detail: Some(format!(
                        "declared count {count}, expected {expected_old}"
                    )),
                });
            }
        }

        let old = self.read_block(old_count, old_line)?;

        let new_line = self.index + 1;
        let new_count = self.expect_header_after_block("NEW")?;
        if new_count == Some(0) {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: new_line,
                detail: Some("declared count 0, expected at least 1".into()),
            });
        }

        let new = self.read_block(new_count, new_line)?;
        self.consume_endop_after_block()?;

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
        let range_text = line.strip_prefix(&prefix).ok_or(ParseFailure {
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
        let expected_count = end - start + 1;
        if let Some(count) = old_count {
            if count != expected_count {
                return Err(ParseFailure {
                    kind: ParseError::Count,
                    line: old_line,
                    detail: Some(format!("declared count {count}, expected {expected_count}")),
                });
            }
        }

        let old = self.read_block(old_count, old_line)?;
        self.consume_endop_after_block()?;

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
        let anchor_count = self.expect_header("ANCHOR")?;
        let expected_count = end - start + 1;
        if let Some(count) = anchor_count {
            if count != expected_count {
                return Err(ParseFailure {
                    kind: ParseError::Count,
                    line: anchor_line,
                    detail: Some(format!("declared count {count}, expected {expected_count}")),
                });
            }
        }

        let anchor = self.read_block(anchor_count, anchor_line)?;

        let new_line = self.index + 1;
        let new_count = self.expect_header_after_block("NEW")?;
        if new_count == Some(0) {
            return Err(ParseFailure {
                kind: ParseError::Count,
                line: new_line,
                detail: Some("declared count 0, expected at least 1".into()),
            });
        }

        let new = self.read_block(new_count, new_line)?;
        self.consume_endop_after_block()?;

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
    let res = parser.read_block(Some(count), parser.index + 1);
    *index = parser.index;
    res.map_err(|f| f.kind)
}

pub(crate) fn parse_envelope(input: &str) -> Result<Envelope, ParseFailure> {
    const MAX_OPERATIONS: usize = 500;

    if input.len() > MAX_INPUT_BYTES {
        let note = if input.len() >= CLIPBOARD_TRUNCATION_BYTES.saturating_sub(100_000) {
            "; input is close to or exceeds 4 MB: the clipboard may have truncated the paste; split the change into several smaller envelopes"
        } else {
            "; split the change into several smaller envelopes"
        };

        return Err(ParseFailure {
            kind: ParseError::Limit,
            line: 1,
            detail: Some(format!(
                "input size ({} bytes) exceeds limit ({} bytes){note}",
                input.len(),
                MAX_INPUT_BYTES
            )),
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
        .ok_or(ParseFailure {
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
    let mut seen_paths = HashSet::new();
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
            let file = files.last_mut().ok_or(ParseFailure {
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

        if line.starts_with(&format!("{}FILE ", parser.prefix)) {
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

        if has_structural_shape(line) {
            return Err(ParseFailure {
                kind: ParseError::Syntax,
                line: parser.index + 1,
                detail: Some("structural line has wrong token or indentation".into()),
            });
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
    fn rejects_input_over_limit() {
        let input = "x".repeat(MAX_INPUT_BYTES + 1);
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::Limit));
    }

    #[test]
    fn input_at_limit_passes_size_check() {
        let mut input = "x".repeat(MAX_INPUT_BYTES);
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::NoEnvelope));
        input.push('x');
        assert_eq!(kind(parse_envelope(&input)), Err(ParseError::Limit));
    }

    #[test]
    fn limit_error_message_for_moderate_overflow_advises_splitting() {
        let input = "x".repeat(MAX_INPUT_BYTES + 1);
        let failure = parse_envelope(&input).unwrap_err();
        assert_eq!(failure.kind, ParseError::Limit);
        let message = failure.to_string();
        assert!(message.contains(&MAX_INPUT_BYTES.to_string()), "{message}");
        assert!(
            message.contains(&(MAX_INPUT_BYTES + 1).to_string()),
            "{message}"
        );
        assert!(
            message.contains("; split the change into several smaller envelopes"),
            "{message}"
        );
        assert!(
            !message.contains("clipboard may have truncated"),
            "moderate overflow should not mention clipboard truncation"
        );
    }

    #[test]
    fn limit_error_message_near_four_megabytes_warns_about_clipboard_truncation() {
        let input = "x".repeat(CLIPBOARD_TRUNCATION_BYTES);
        let failure = parse_envelope(&input).unwrap_err();
        assert_eq!(failure.kind, ParseError::Limit);
        let message = failure.to_string();
        assert!(
            message.contains("clipboard may have truncated the paste"),
            "{message}"
        );
    }

    #[test]
    fn truncation_never_parses_as_success() {
        let envelope = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=truncation-test\n",
            "CVB::k7Q2xw9R::FILE path=first.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD n=1\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW n=1\n",
            "new first\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::OP insert after=1-1\n",
            "CVB::k7Q2xw9R::ANCHOR n=1\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW n=1\n",
            "inserted\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::FILE path=second.rs\n",
            "CVB::k7Q2xw9R::OP delete lines=1-1\n",
            "CVB::k7Q2xw9R::OLD n=1\n",
            "second\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=3",
        );

        for cut in 0..envelope.len() {
            let prefix = &envelope[..cut];
            let result = parse_envelope(prefix);
            assert!(
                result.is_err(),
                "prefix up to byte {cut} must not parse successfully: {prefix:?}"
            );
        }

        assert!(parse_envelope(envelope).is_ok());
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

    #[test]
    fn plain_line_after_counted_new_payload_is_count_error() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::OP replace lines=1-1
CVB::k7Q2xw9R::OLD n=1
old
CVB::k7Q2xw9R::NEW n=1
new
unexpected text
CVB::k7Q2xw9R::END ops=1";

        let failure = parse_envelope(input).unwrap_err();
        assert_eq!(failure.kind, ParseError::Count);
        assert_eq!(failure.line, 8);
        assert!(
            failure
                .detail
                .as_deref()
                .unwrap()
                .contains("count may be too small")
        );
    }

    #[test]
    fn malformed_structural_line_after_counted_payload_is_syntax() {
        for boundary in [" CVB::k7Q2xw9R::ENDOP", "CVB::A1B2C3D4::ENDOP"] {
            let input = format!(
                "CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\n\
CVB::k7Q2xw9R::FILE path=src/main.rs\n\
CVB::k7Q2xw9R::OP replace lines=1-1\n\
CVB::k7Q2xw9R::OLD n=1\n\
old\n\
CVB::k7Q2xw9R::NEW n=1\n\
new\n\
{boundary}\n\
CVB::k7Q2xw9R::END ops=1"
            );

            let failure = parse_envelope(&input).unwrap_err();
            assert_eq!(failure.kind, ParseError::Syntax, "{boundary:?}");
            assert_eq!(failure.line, 8, "{boundary:?}");
        }
    }

    #[test]
    fn stray_text_between_file_and_operation_is_stray() {
        let input = "\\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
unexpected text
CVB::k7Q2xw9R::END ops=1";

        assert_eq!(kind(parse_envelope(input)), Err(ParseError::Stray));
    }

    #[test]
    fn wrong_token_file_boundary_is_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::A1B2C3D4::FILE path=src/main.rs
CVB::k7Q2xw9R::END ops=1";

        let failure = parse_envelope(input).unwrap_err();
        assert_eq!(failure.kind, ParseError::Syntax);
        assert_eq!(failure.line, 2);
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

    #[test]
    fn parse_error_display_includes_protocol_code_syntax() {
        let input = "\
CVB::k7Q2xw9R::BEGIN v=1 id=demo-01
CVB::k7Q2xw9R::FILE path=src/main.rs
CVB::k7Q2xw9R::BEGIN ";
        let failure = parse_envelope(input).unwrap_err();
        let message = failure.to_string();
        assert_eq!(failure.kind, ParseError::Syntax);
        assert!(
            message.starts_with("CVB: E-SYNTAX:"),
            "expected message to start with 'CVB: E-SYNTAX:', got: {message}"
        );
        assert!(
            !message.contains("Syntax"),
            "message should not contain raw debug variant: {message}"
        );
    }

    #[test]
    fn parse_error_display_includes_protocol_code_count() {
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
        assert!(
            message.starts_with("CVB: E-COUNT:"),
            "expected message to start with 'CVB: E-COUNT:', got: {message}"
        );
        assert!(
            !message.contains("Count"),
            "message should not contain raw debug variant: {message}"
        );
    }

    #[test]
    fn parse_error_display_includes_protocol_code_limit() {
        let input = "x".repeat(MAX_INPUT_BYTES + 1);
        let failure = parse_envelope(&input).unwrap_err();
        let message = failure.to_string();
        assert_eq!(failure.kind, ParseError::Limit);
        assert!(
            message.starts_with("CVB: E-LIMIT:"),
            "expected message to start with 'CVB: E-LIMIT:', got: {message}"
        );
        assert!(
            !message.contains("Limit"),
            "message should not contain raw debug variant: {message}"
        );
    }

    #[test]
    fn parse_error_display_includes_protocol_code_truncated() {
        let input = "CVB::k7Q2xw9R::BEGIN v=1 id=demo-01\nCVB::k7Q2xw9R::FILE path=src/main.rs";
        let failure = parse_envelope(input).unwrap_err();
        let message = failure.to_string();
        assert_eq!(failure.kind, ParseError::Truncated);
        assert!(
            message.starts_with("CVB: E-TRUNC:"),
            "expected message to start with 'CVB: E-TRUNC:', got: {message}"
        );
        assert!(
            !message.contains("Truncated"),
            "message should not contain raw debug variant: {message}"
        );
    }

    #[test]
    fn parse_error_display_includes_protocol_code_no_envelope() {
        let input = "not a valid envelope";
        let failure = parse_envelope(input).unwrap_err();
        let message = failure.to_string();
        assert_eq!(failure.kind, ParseError::NoEnvelope);
        assert!(
            message.starts_with("CVB: E-NOENV:"),
            "expected message to start with 'CVB: E-NOENV:', got: {message}"
        );
        assert!(
            !message.contains("NoEnvelope"),
            "message should not contain raw debug variant: {message}"
        );
    }
    #[test]
    fn parses_operations_without_line_counts() {
        let envelope = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=no-counts\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=2-2\n",
            "CVB::k7Q2xw9R::OLD\n",
            "let answer = 41;\n",
            "CVB::k7Q2xw9R::NEW\n",
            "let answer = 42;\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::OP delete lines=4-4\n",
            "CVB::k7Q2xw9R::OLD\n",
            "// remove this\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::OP insert after=2-2\n",
            "CVB::k7Q2xw9R::ANCHOR\n",
            "let answer = 42;\n",
            "CVB::k7Q2xw9R::NEW\n",
            "println!(\"{answer}\");\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=3\n",
        );

        let parsed = parse_envelope(envelope).expect("envelope without counts should parse successfully");
        assert_eq!(parsed.id, "no-counts");
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].operations.len(), 3);
    }

    #[test]
    fn rejects_empty_uncounted_blocks() {
        // 1. Empty uncounted OLD block in replace
        let empty_old = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=empty-old\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=2-2\n",
            "CVB::k7Q2xw9R::OLD\n",
            "CVB::k7Q2xw9R::NEW\n",
            "let answer = 42;\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let err = parse_envelope(empty_old).unwrap_err();
        assert_eq!(err.kind, ParseError::Count);
        assert_eq!(err.line, 4);

        // 2. Empty uncounted NEW block in replace
        let empty_new = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=empty-new\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=2-2\n",
            "CVB::k7Q2xw9R::OLD\n",
            "let answer = 41;\n",
            "CVB::k7Q2xw9R::NEW\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let err = parse_envelope(empty_new).unwrap_err();
        assert_eq!(err.kind, ParseError::Count);
        assert_eq!(err.line, 6);

        // 3. Empty uncounted ANCHOR block in insert
        let empty_anchor = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=empty-anchor\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP insert after=2-2\n",
            "CVB::k7Q2xw9R::ANCHOR\n",
            "CVB::k7Q2xw9R::NEW\n",
            "println!(\"hello\");\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let err = parse_envelope(empty_anchor).unwrap_err();
        assert_eq!(err.kind, ParseError::Count);
        assert_eq!(err.line, 4);
    }

    #[test]
    fn truncation_never_parses_as_success_for_countless_and_mixed_envelopes() {
        // 1. Entirely countless envelope
        let countless = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=trunc-countless\n",
            "CVB::k7Q2xw9R::FILE path=first.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW\n",
            "new first\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::OP insert after=1-1\n",
            "CVB::k7Q2xw9R::ANCHOR\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW\n",
            "inserted\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::FILE path=second.rs\n",
            "CVB::k7Q2xw9R::OP delete lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "second\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=3",
        );

        for cut in countless.char_indices().map(|(i, _)| i) {
            let prefix = &countless[..cut];
            assert!(
                parse_envelope(prefix).is_err(),
                "countless prefix up to byte {cut} must not parse successfully: {prefix:?}"
            );
        }
        assert!(parse_envelope(countless).is_ok());

        // 2. Mixed envelope (some counted, some countless)
        let mixed = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=trunc-mixed\n",
            "CVB::k7Q2xw9R::FILE path=first.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD n=1\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW\n",
            "new first\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::FILE path=second.rs\n",
            "CVB::k7Q2xw9R::OP delete lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "second\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=2",
        );

        for cut in mixed.char_indices().map(|(i, _)| i) {
            let prefix = &mixed[..cut];
            assert!(
                parse_envelope(prefix).is_err(),
                "mixed prefix up to byte {cut} must not parse successfully: {prefix:?}"
            );
        }
        assert!(parse_envelope(mixed).is_ok());
    }

    #[test]
    fn uncounted_blocks_preserve_trailing_blank_lines() {
        let envelope = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=trailing-blank\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "first\n",
            "\n",
            "CVB::k7Q2xw9R::NEW\n",
            "first replaced\n",
            "\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let parsed = parse_envelope(envelope).expect("should parse envelope with trailing blank lines");
        match &parsed.files[0].operations[0] {
            Operation::Replace { old, new, .. } => {
                assert_eq!(old, &["first".to_string(), "".to_string()]);
                assert_eq!(new, &["first replaced".to_string(), "".to_string()]);
            }
            _ => panic!("expected replace operation"),
        }
    }

    #[test]
    fn uncounted_payload_permits_foreign_token_and_treats_real_token_as_structural() {
        // 1. Foreign token control lines inside uncounted payload are preserved as payload lines
        let foreign_envelope = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=foreign-token\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "CVB::DIFFTOK::OLD\n",
            "CVB::DIFFTOK::NEW\n",
            "CVB::k7Q2xw9R::NEW\n",
            "replacement\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let parsed = parse_envelope(foreign_envelope).expect("foreign token inside uncounted block must be treated as payload");
        match &parsed.files[0].operations[0] {
            Operation::Replace { old, .. } => {
                assert_eq!(old, &["CVB::DIFFTOK::OLD".to_string(), "CVB::DIFFTOK::NEW".to_string()]);
            }
            _ => panic!("expected replace operation"),
        }

        // 2. Real token prefix on line 1 of payload terminates block immediately (0 lines -> ParseError::Count)
        let immediate_collision = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=collision-immediate\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "CVB::k7Q2xw9R::NOT_A_VALID_HEADER\n",
            "CVB::k7Q2xw9R::NEW\n",
            "replacement\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let err = parse_envelope(immediate_collision).unwrap_err();
        assert_eq!(err.kind, ParseError::Count);

        // 3. Real token prefix after payload terminates block, and subsequent line is checked as structural (-> ParseError::Syntax)
        let mid_payload_collision = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=collision-mid\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "valid old line\n",
            "CVB::k7Q2xw9R::NOT_A_VALID_HEADER\n",
            "CVB::k7Q2xw9R::NEW\n",
            "replacement\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );
        let err = parse_envelope(mid_payload_collision).unwrap_err();
        assert_eq!(err.kind, ParseError::Syntax);
    }

    #[test]
    fn parses_mixed_counted_and_uncounted_blocks() {
        let envelope = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=mixed-counts\n",
            "CVB::k7Q2xw9R::FILE path=src/example.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD n=1\n",
            "first\n",
            "CVB::k7Q2xw9R::NEW\n",
            "first replaced\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::OP insert after=1-1\n",
            "CVB::k7Q2xw9R::ANCHOR\n",
            "first replaced\n",
            "CVB::k7Q2xw9R::NEW n=2\n",
            "added 1\n",
            "added 2\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=2\n",
        );
        let parsed = parse_envelope(envelope).expect("mixed count envelope must parse successfully");
        assert_eq!(parsed.files[0].operations.len(), 2);
    }

    #[test]
    fn new_block_trailing_blank_line_vs_dropped_diff_differs() {
        use crate::surgery::plan::plan_envelope;
        use crate::surgery::render::{render_plan_diff, RenderOptions};
        use std::collections::HashMap;

        let envelope_with_blank = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=with-blank\n",
            "CVB::k7Q2xw9R::FILE path=test.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "original line\n",
            "CVB::k7Q2xw9R::NEW\n",
            "replacement\n",
            "\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );

        let envelope_without_blank = concat!(
            "CVB::k7Q2xw9R::BEGIN v=1 id=without-blank\n",
            "CVB::k7Q2xw9R::FILE path=test.rs\n",
            "CVB::k7Q2xw9R::OP replace lines=1-1\n",
            "CVB::k7Q2xw9R::OLD\n",
            "original line\n",
            "CVB::k7Q2xw9R::NEW\n",
            "replacement\n",
            "CVB::k7Q2xw9R::ENDOP\n",
            "CVB::k7Q2xw9R::END ops=1\n",
        );

        let mut originals = HashMap::new();
        originals.insert("test.rs".to_string(), "original line\n".to_string());

        let plan_with = plan_envelope(&parse_envelope(envelope_with_blank).unwrap(), &originals).unwrap();
        let plan_without = plan_envelope(&parse_envelope(envelope_without_blank).unwrap(), &originals).unwrap();

        let diff_with = render_plan_diff(&plan_with, RenderOptions { color: false });
        let diff_without = render_plan_diff(&plan_without, RenderOptions { color: false });

        assert_ne!(diff_with, diff_without);
        assert!(diff_with.contains("+\n") || diff_with.contains("+ \n"));
        assert!(!diff_without.contains("+\n") && !diff_without.contains("+ \n"));
    }
}

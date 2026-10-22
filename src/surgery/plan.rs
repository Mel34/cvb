use super::*;
use std::collections::{HashMap, HashSet};
use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Applies one operation to an original file represented as lines.
/// All operation ranges are one-based and refer to the original lines.
pub(crate) fn validate_operation(
    original: &[String],
    operation: &Operation,
) -> Result<(), PlanError> {
    validate_operation_detail(original, operation).map_err(|(err, _)| err)
}

fn find_differing_position(
    expected: &str,
    found: &str,
) -> (usize, usize, Option<PrefixSide>) {
    let mut col = 1;
    let mut byte_offset = 0;
    let mut exp_chars = expected.chars();
    let mut fnd_chars = found.chars();

    loop {
        match (exp_chars.next(), fnd_chars.next()) {
            (Some(e), Some(f)) => {
                if e != f {
                    return (col, byte_offset, None);
                }
                col += 1;
                byte_offset += e.len_utf8();
            }
            (None, Some(_)) => return (col, byte_offset, Some(PrefixSide::ExpectedEnds)),
            (Some(_), None) => return (col, byte_offset, Some(PrefixSide::FoundEnds)),
            (None, None) => return (col, byte_offset, None),
        }
    }
}

fn precondition_detail(
    start: usize,
    original_slice: &[String],
    declared_slice: &[String],
) -> PlanDetail {
    let mut mismatch_line = start;
    let mut exp = String::new();
    let mut fnd = String::new();
    let mut found_mismatch = false;

    for (k, (orig_l, decl_l)) in
        original_slice.iter().zip(declared_slice.iter()).enumerate()
    {
        if orig_l != decl_l {
            mismatch_line = start + k;
            exp = decl_l.clone();
            fnd = orig_l.clone();
            found_mismatch = true;
            break;
        }
    }

    if !found_mismatch {
        if declared_slice.len() < original_slice.len() {
            let k = declared_slice.len();
            mismatch_line = start + k;
            fnd = original_slice[k].clone();
        } else if declared_slice.len() > original_slice.len() {
            let k = original_slice.len();
            mismatch_line = start + k;
            exp = declared_slice[k].clone();
        }
    }

    let only_ws = !exp.is_empty() && !fnd.is_empty() && exp.trim_end() == fnd.trim_end();
    let (column, byte_offset, prefix_ended) =
        find_differing_position(&exp, &fnd);

    PlanDetail::Precondition {
        line: mismatch_line,
        column,
        byte_offset,
        expected: exp,
        found: fnd,
        prefix_ended,
        only_trailing_whitespace: only_ws,
    }
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
                return Err((
                    PlanError::Precondition,
                    precondition_detail(*start, &original[first..last], &old[..]),
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
                return Err((
                    PlanError::Precondition,
                    precondition_detail(*start, &original[first..last], &old[..]),
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
                return Err((
                    PlanError::Precondition,
                    precondition_detail(*start, &original[first..last], &anchor[..]),
                ));
            }
        }
    }

    Ok(())
}

/// Builds a complete in-memory plan without writing to disk.
/// All operation ranges refer to the original contents of each file.
pub(crate) fn plan_envelope(
    envelope: &Envelope,
    originals: &HashMap<String, String>,
) -> Result<Plan, PlanFailure> {
    let mut seen = HashSet::new();
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

        let original_digest = sha256_hex(original.as_bytes());
        let proposed_digest = sha256_hex(proposed.as_bytes());

        planned_files.push(PlannedFile {
            path: section.path.clone(),
            original: original.clone(),
            original_digest,
            proposed,
            proposed_digest,
            operations: section.operations.clone(),
        });
    }

    let mut plan_hasher = Sha256::new();
    plan_hasher.update(envelope.id.as_bytes());
    plan_hasher.update(b"\0");

    for file in &planned_files {
        plan_hasher.update(file.path.as_bytes());
        plan_hasher.update(b"\0");
        plan_hasher.update(file.original_digest.as_bytes());
        plan_hasher.update(b"\0");
        plan_hasher.update(file.proposed_digest.as_bytes());
        plan_hasher.update(b"\0");
    }

    let digest = format!("{:x}", plan_hasher.finalize());

    Ok(Plan {
        token: envelope.token.clone(),
        id: envelope.id.clone(),
        digest,
        files: planned_files,
    })
}

#[cfg(test)]
mod tests {
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

    fn make_test_originals(contents: &str) -> HashMap<String, String> {
        [("example.txt".to_owned(), contents.to_owned())]
            .into_iter()
            .collect()
    }

    #[test]
    fn plan_computes_sha256_digests_for_files_and_plan() {
        let original_content = "one\nold\nthree\n";
        let envelope = make_test_envelope(vec![Operation::Replace {
            start: 2,
            end: 2,
            old: lines(&["old"]),
            new: lines(&["new"]),
        }]);

        let plan =
            plan_envelope(&envelope, &make_test_originals(original_content)).unwrap();

        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.digest.len(), 64);

        let file = &plan.files[0];
        assert_eq!(file.original_digest.len(), 64);
        assert_eq!(file.proposed_digest.len(), 64);
        assert_ne!(file.original_digest, file.proposed_digest);

        assert_eq!(
            file.original_digest,
            sha256_hex(original_content.as_bytes())
        );
        assert_eq!(
            file.proposed_digest,
            sha256_hex("one\nnew\nthree\n".as_bytes())
        );
    }

    #[test]
    fn plan_digest_changes_when_proposed_content_changes() {
        let make_plan = |replacement: &str| {
            let envelope = make_test_envelope(vec![Operation::Replace {
                start: 2,
                end: 2,
                old: lines(&["old"]),
                new: lines(&[replacement]),
            }]);

            plan_envelope(
                &envelope,
                &make_test_originals("one\nold\nthree\n"),
            )
            .unwrap()
        };

        let first = make_plan("first");
        let second = make_plan("second");

        assert_ne!(first.digest, second.digest);
    }

    #[test]
    fn planned_file_preserves_operation_metadata() {
        let envelope = make_test_envelope(vec![
            Operation::Replace {
                start: 2,
                end: 2,
                old: lines(&["old"]),
                new: lines(&["new"]),
            },
            Operation::Delete {
                start: 4,
                end: 4,
                old: lines(&["five"]),
            },
        ]);

        let plan = plan_envelope(
            &envelope,
            &make_test_originals("one\nold\nthree\nfive\n"),
        )
        .unwrap();

        let operations = &plan.files[0].operations;
        assert_eq!(operations.len(), 2);
        assert!(matches!(
            &operations[0],
            Operation::Replace { start: 2, end: 2, .. }
        ));
        assert!(matches!(
            &operations[1],
            Operation::Delete { start: 4, end: 4, .. }
        ));
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

    fn originals(contents: &str) -> HashMap<String, String> {
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
        let originals = HashMap::new();
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
            kind(plan_envelope(&envelope, &HashMap::new())),
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

    #[test]
    fn precondition_error_message_includes_e_precond_code_and_unicode_column() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            // 'é' is 2 UTF-8 bytes (col 1, bytes 0..2)
            // ' ' is 1 byte (col 2, byte 2)
            // mismatch at col 3 (byte offset 3): 'a' vs 'b'
            old: vec!["é a".to_owned()],
            new: vec!["new".to_owned()],
        }]);
        let original = originals("é b\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(
            message.starts_with("CVB: E-PRECOND:"),
            "expected message to start with protocol code, got: {message}"
        );
        assert!(
            message.contains("column 3 (byte offset 3)"),
            "expected col 3 byte 3, got: {message}"
        );
        assert!(
            !message.contains("PlanFailure"),
            "message should not contain debug structure: {message}"
        );
    }

    #[test]
    fn end_of_file_error_message_includes_e_eof_code() {
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
        assert!(
            message.starts_with("CVB: E-EOF:"),
            "expected message to start with 'CVB: E-EOF:', got: {message}"
        );
    }

    #[test]
    fn overlap_error_message_includes_e_overlap_code() {
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
        assert!(
            message.starts_with("CVB: E-OVERLAP:"),
            "expected message to start with 'CVB: E-OVERLAP:', got: {message}"
        );
    }

    #[test]
    fn range_error_message_includes_e_range_code() {
        let envelope = make_envelope(vec![Operation::Delete {
            start: 1,
            end: 10,
            old: vec!["x".to_owned()],
        }]);
        let original = originals("one\ntwo\nthree\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Range);
        assert!(
            message.starts_with("CVB: E-RANGE:"),
            "expected message to start with 'CVB: E-RANGE:', got: {message}"
        );
    }

    #[test]
    fn noop_error_message_includes_e_noop_code() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["same".to_owned()],
            new: vec!["same".to_owned()],
        }]);
        let original = originals("same\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::NoOp);
        assert!(
            message.starts_with("CVB: E-NOOP:"),
            "expected message to start with 'CVB: E-NOOP:', got: {message}"
        );
        assert!(
            !message.contains("NoOp"),
            "message should not contain debug variant: {message}"
        );
    }

    #[test]
    fn file_error_message_includes_e_file_code() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["one".to_owned()],
            new: vec![],
        }]);
        let original = originals("one\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::InvalidFile);
        assert!(
            message.starts_with("CVB: E-FILE:"),
            "expected message to start with 'CVB: E-FILE:', got: {message}"
        );
        assert!(
            !message.contains("InvalidFile"),
            "message should not contain debug variant: {message}"
        );
    }

    #[test]
    fn precondition_error_message_notes_when_declared_old_is_shorter_than_range() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 2,
            old: vec!["line 1".to_owned()],
            new: vec!["new 1".to_owned(), "new 2".to_owned()],
        }]);
        let original = originals("line 1\nline 2\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(message.starts_with("CVB: E-PRECOND:"), "{message}");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("note: expected ends here"), "{message}");
    }

    #[test]
    fn precondition_error_message_notes_when_declared_old_is_longer_than_range() {
        let envelope = make_envelope(vec![Operation::Replace {
            start: 1,
            end: 1,
            old: vec!["line 1".to_owned(), "extra declared line".to_owned()],
            new: vec!["new 1".to_owned()],
        }]);
        let original = originals("line 1\nline 2\n");
        let failure = plan_envelope(&envelope, &original).unwrap_err();
        let message = failure.to_string();

        assert_eq!(failure.kind, PlanError::Precondition);
        assert!(message.starts_with("CVB: E-PRECOND:"), "{message}");
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("note: found ends here"), "{message}");
    }
}

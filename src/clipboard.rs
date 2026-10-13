use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DEFAULT_COPY_LIMIT: usize = 9_750;

fn configured_copy_limit() -> usize {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        });

    let Some(config_dir) = config_dir else {
        eprintln!(
            "CVB: unable to determine config directory; using default limit {DEFAULT_COPY_LIMIT}"
        );
        return DEFAULT_COPY_LIMIT;
    };

    load_copy_limit(&config_dir.join("cvb").join("limit"))
}

fn load_copy_limit(path: &Path) -> usize {
    if !path.exists() {
        let Some(parent) = path.parent() else {
            eprintln!("CVB: invalid limit config path; using default limit {DEFAULT_COPY_LIMIT}");
            return DEFAULT_COPY_LIMIT;
        };

        if let Err(error) = fs::create_dir_all(parent) {
            eprintln!(
                "CVB: unable to create limit config directory: {error}; using default limit {DEFAULT_COPY_LIMIT}"
            );
            return DEFAULT_COPY_LIMIT;
        }

        let initial_contents = format!("{}{}", DEFAULT_COPY_LIMIT, char::from(10));
        if let Err(error) = fs::write(path, initial_contents) {
            eprintln!(
                "CVB: unable to initialize limit config: {error}; using default limit {DEFAULT_COPY_LIMIT}"
            );
            return DEFAULT_COPY_LIMIT;
        }
    }

    match fs::read_to_string(path) {
        Ok(contents) => match contents.trim().parse::<usize>() {
            Ok(limit) if limit > 0 => limit,
            _ => {
                eprintln!(
                    "CVB: invalid limit in {}; using default limit {DEFAULT_COPY_LIMIT}",
                    path.display()
                );
                DEFAULT_COPY_LIMIT
            }
        },
        Err(error) => {
            eprintln!(
                "CVB: unable to read {}: {error}; using default limit {DEFAULT_COPY_LIMIT}",
                path.display()
            );
            DEFAULT_COPY_LIMIT
        }
    }
}
fn chunk_prefix(number: usize, total: usize, final_part: bool) -> String {
    if final_part {
        format!(
            "[Part {number}/{total} - FINAL PART]\nThis is the final part ({number}/{total}). All parts have now been transmitted. Please assemble all parts and process the entire request now.\n---\n"
        )
    } else {
        format!(
            "[Part {number}/{total} - DO NOT PROCESS YET]\nThis is part {number} of {total}. Do not analyze, answer, or summarize yet. Simply reply: \"Part {number}/{total} received, awaiting next part.\" and wait for subsequent parts.\n---\n"
        )
    }
}

fn split_chunk_payloads(
    text: &str,
    limit: usize,
    total_guess: usize,
) -> Result<Vec<String>, String> {
    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let number = chunks.len() + 1;
        let intermediate = chunk_prefix(number, total_guess, false);
        let final_prefix = chunk_prefix(number, total_guess, true);
        let overhead = intermediate
            .chars()
            .count()
            .max(final_prefix.chars().count());

        let available = limit
            .checked_sub(overhead)
            .ok_or_else(|| "CVB: chunk framing exceeds the configured limit".to_string())?;

        if available == 0 {
            return Err("CVB: chunk framing leaves no room for content".to_string());
        }

        let mut end = remaining
            .char_indices()
            .nth(available)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());

        if end == 0 {
            return Err("CVB: unable to make progress while splitting capture".to_string());
        }

        if end < remaining.len() {
            let candidate = &remaining[..end];

            if let Some(index) = candidate.rfind('\n') {
                end = index + 1;
            } else if let Some(index) = candidate.rfind(' ') {
                end = index + 1;
            }
        }

        if end == 0 {
            return Err("CVB: unable to make progress while splitting capture".to_string());
        }

        chunks.push(remaining[..end].to_string());
        remaining = &remaining[end..];
    }

    Ok(chunks)
}

fn chunk_messages(text: &str, limit: usize) -> Result<Vec<String>, String> {
    if text.chars().count() <= limit {
        return Ok(vec![text.to_string()]);
    }

    let mut total_guess = 2;

    loop {
        let chunks = split_chunk_payloads(text, limit, total_guess)?;
        let actual_total = chunks.len();

        if actual_total == total_guess {
            return Ok(chunks
                .into_iter()
                .enumerate()
                .map(|(index, chunk)| {
                    let number = index + 1;
                    let prefix = chunk_prefix(number, actual_total, number == actual_total);
                    format!("{prefix}{chunk}")
                })
                .collect());
        }

        total_guess = actual_total;
    }
}

pub(crate) fn process(buffer: &ClipboardBuffer) -> Result<(), String> {
    let limit = configured_copy_limit();

    let text = match std::str::from_utf8(buffer.as_bytes()) {
        Ok(text) => text,
        Err(_) if buffer.len() < limit => return copy_to_clipboard(buffer),
        Err(_) => {
            return Err(
                "CVB: oversized capture is not valid UTF-8; cannot split it safely".to_string(),
            );
        }
    };

    let chunks = chunk_messages(text, limit)?;

    if chunks.len() == 1 {
        return copy_to_clipboard(buffer);
    }

    copy_chunk_sequence(&chunks)
}

fn copy_chunk_sequence(chunks: &[String]) -> Result<(), String> {
    let mut stdin = std::io::stdin();

    copy_chunk_sequence_with(chunks, copy_to_clipboard, || {
        let mut key = [0u8; 1];
        stdin
            .read_exact(&mut key)
            .map_err(|error| format!("CVB: unable to read chunk control key: {error}"))?;
        Ok(key[0])
    })
}

fn copy_chunk_sequence_with<C, R>(
    chunks: &[String],
    mut copy: C,
    mut read_key: R,
) -> Result<(), String>
where
    C: FnMut(&ClipboardBuffer) -> Result<(), String>,
    R: FnMut() -> Result<u8, String>,
{
    let total = chunks.len();

    for (index, chunk) in chunks.iter().enumerate() {
        let number = index + 1;
        let buffer = ClipboardBuffer::new(chunk.as_bytes().to_vec());
        copy(&buffer)?;

        if number == total {
            eprint!("\r\x1b[2K");
            std::io::stderr()
                .flush()
                .map_err(|error| format!("CVB: unable to clear final chunk status: {error}"))?;
            break;
        }

        eprint!(
            "\r\x1b[2KPart {number}/{total} is available in clipboard. \
Press Space for next part; q or Ctrl+C to cancel."
        );
        std::io::stderr()
            .flush()
            .map_err(|error| format!("CVB: unable to update chunk status: {error}"))?;

        loop {
            match read_key()? {
                b' ' => break,
                b'q' | b'Q' | 0x03 => {
                    eprint!("\r\x1b[2K");
                    std::io::stderr().flush().map_err(|error| {
                        format!("CVB: unable to clear cancelled chunk status: {error}")
                    })?;
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    Ok(())
}

impl ClipboardBuffer {
    pub(crate) fn new(contents: impl Into<Vec<u8>>) -> Self {
        Self {
            contents: contents.into(),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.contents.len()
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.contents
    }
}

#[cfg(test)]
mod tests {
    use super::ClipboardBuffer;

    fn temporary_limit_path(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();

        std::env::temp_dir()
            .join(format!("cvb-limit-{label}-{}-{nonce}", std::process::id()))
            .join("cvb")
            .join("limit")
    }

    #[test]
    fn limit_config_initializes_default_with_newline() {
        let path = temporary_limit_path("init");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        assert_eq!(super::load_copy_limit(&path), 9_750);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "9750\n");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limit_config_reads_custom_value() {
        let path = temporary_limit_path("custom");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "4321\n").unwrap();

        assert_eq!(super::load_copy_limit(&path), 4_321);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limit_config_invalid_value_uses_default() {
        let path = temporary_limit_path("invalid");
        let root = path.parent().unwrap().parent().unwrap().to_path_buf();

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-a-number\n").unwrap();

        assert_eq!(super::load_copy_limit(&path), 9_750);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn chunk_messages_leave_small_input_unchanged() {
        let input = "hello 🦀\nworld";
        let chunks = super::chunk_messages(input, 100).unwrap();

        assert_eq!(chunks, vec![input]);
    }

    #[test]
    fn chunk_messages_reconstruct_original_text_exactly() {
        let input = "line one with spaces\né 🧑‍💻\n".repeat(200);
        let chunks = super::chunk_messages(&input, 300).unwrap();
        let total = chunks.len();

        assert!(total > 1);

        let mut reconstructed = String::new();

        for (index, chunk) in chunks.iter().enumerate() {
            let number = index + 1;
            let final_part = number == total;

            assert!(
                chunk.chars().count() <= 300,
                "part {number}/{total} exceeds the configured character limit"
            );

            let (header, payload) = chunk
                .split_once("---\n")
                .unwrap_or_else(|| panic!("part {number}/{total} has no framing separator"));

            assert!(
                header.starts_with(&format!("[Part {number}/{total} - ")),
                "part {number}/{total} has incorrect numbering"
            );

            if final_part {
                assert!(header.contains("FINAL PART"));
                assert!(!header.contains("DO NOT PROCESS YET"));
            } else {
                assert!(header.contains("DO NOT PROCESS YET"));
                assert!(!header.contains("FINAL PART"));
            }

            reconstructed.push_str(payload);
        }

        assert_eq!(
            reconstructed.as_bytes(),
            input.as_bytes(),
            "reassembling the payloads must reproduce the original bytes exactly"
        );
    }

    #[test]
    fn chunk_messages_preserve_edge_case_payloads_exactly() {
        let inputs = [
            "é🧑‍💻漢字".repeat(500),
            "\n\n\nline\n\n".repeat(100),
            "   leading and trailing spaces   \n".repeat(100),
            "x".repeat(2_000),
            "line one\nline two\n".repeat(200),
        ];

        for input in inputs {
            let chunks = super::chunk_messages(&input, 300).unwrap();
            assert!(chunks.len() > 1);

            let reconstructed = chunks
                .iter()
                .map(|chunk| {
                    chunk
                        .split_once("---\n")
                        .expect("chunk must contain framing separator")
                        .1
                })
                .collect::<String>();

            assert_eq!(
                reconstructed.as_bytes(),
                input.as_bytes(),
                "chunk reconstruction changed the original payload"
            );

            assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 300));
        }
    }

    #[test]
    fn chunk_messages_prefer_newline_boundaries() {
        let input = format!("{}\n{}", "a".repeat(40), "b".repeat(600));
        let chunks = super::chunk_messages(&input, 300).unwrap();

        assert!(chunks.len() > 1);
        let first_payload = chunks[0].split_once("---\n").unwrap().1;
        assert!(first_payload.ends_with("\n"));
    }

    #[test]
    fn chunk_messages_prefer_spaces_when_no_newline_fits() {
        let input = format!("{} {}", "a".repeat(40), "b".repeat(600));
        let chunks = super::chunk_messages(&input, 300).unwrap();

        assert!(chunks.len() > 1);
        let first_payload = chunks[0].split_once("---\n").unwrap().1;
        assert!(first_payload.ends_with(' '));
    }

    #[test]
    fn chunk_messages_reject_limits_too_small_for_framing() {
        let error = super::chunk_messages(&"x".repeat(100), 10).unwrap_err();

        assert!(error.contains("framing"));
    }

    #[test]
    fn buffer_owns_exact_bytes() {
        let contents = vec![0x00, 0x41, 0xFF];
        let buffer = ClipboardBuffer::new(contents.clone());

        assert_eq!(buffer.as_bytes(), contents.as_slice());
        assert_eq!(buffer.len(), contents.len());
    }

    #[test]
    fn buffer_length_counts_utf8_bytes() {
        let buffer = ClipboardBuffer::new("é".as_bytes().to_vec());

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.as_bytes(), "é".as_bytes());
    }

    #[test]
    fn chunk_sequence_copies_every_part_and_reconstructs_original() {
        let input = "Unicode 🦀 and whitespace\n\n".repeat(100);
        let chunks = super::chunk_messages(&input, 300).unwrap();
        let expected_chunks = chunks.clone();
        let mut copied = Vec::<Vec<u8>>::new();
        let mut keys = vec![b' '; chunks.len().saturating_sub(1)].into_iter();

        super::copy_chunk_sequence_with(
            &chunks,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            || {
                Ok(keys
                    .next()
                    .expect("a key should advance each non-final part"))
            },
        )
        .unwrap();

        assert_eq!(
            copied,
            expected_chunks
                .iter()
                .map(|chunk| chunk.as_bytes().to_vec())
                .collect::<Vec<_>>(),
            "the clipboard must receive every framed chunk in order"
        );

        let reconstructed = copied
            .iter()
            .map(|chunk| {
                std::str::from_utf8(chunk)
                    .unwrap()
                    .split_once("---\n")
                    .expect("each copied chunk must have a framing separator")
                    .1
            })
            .collect::<String>();

        assert_eq!(reconstructed.as_bytes(), input.as_bytes());
        assert_eq!(keys.next(), None);
    }

    #[test]
    fn chunk_sequence_cancellation_stops_after_current_part() {
        for cancel_key in [b'q', b'Q', 0x03] {
            let chunks = vec![
                "first framed chunk".to_string(),
                "second framed chunk".to_string(),
                "third framed chunk".to_string(),
            ];
            let mut copied = Vec::<Vec<u8>>::new();

            super::copy_chunk_sequence_with(
                &chunks,
                |buffer| {
                    copied.push(buffer.as_bytes().to_vec());
                    Ok(())
                },
                || Ok(cancel_key),
            )
            .unwrap();

            assert_eq!(
                copied,
                vec![chunks[0].as_bytes().to_vec()],
                "cancel key {cancel_key:?} must prevent later chunks from being copied"
            );
        }
    }

    #[test]
    fn chunk_sequence_ignores_unrecognized_keys() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
            "third framed chunk".to_string(),
        ];
        let mut copied = Vec::<Vec<u8>>::new();
        let mut keys = [b'x', b'\n', b' ', b'!', b' '].into_iter().map(|key| key);

        super::copy_chunk_sequence_with(
            &chunks,
            |buffer| {
                copied.push(buffer.as_bytes().to_vec());
                Ok(())
            },
            || Ok(keys.next().expect("expected scripted control key")),
        )
        .unwrap();

        assert_eq!(
            copied,
            chunks
                .iter()
                .map(|chunk| chunk.as_bytes().to_vec())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn chunk_sequence_propagates_key_read_failure_without_advancing() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
        ];
        let mut copy_attempts = 0;

        let result = super::copy_chunk_sequence_with(
            &chunks,
            |_| {
                copy_attempts += 1;
                Ok(())
            },
            || Err("simulated key read failure".to_string()),
        );

        assert_eq!(result.unwrap_err(), "simulated key read failure");
        assert_eq!(copy_attempts, 1);
    }

    #[test]
    fn chunk_sequence_propagates_copy_failure_without_advancing() {
        let chunks = vec![
            "first framed chunk".to_string(),
            "second framed chunk".to_string(),
        ];
        let mut copy_attempts = 0;
        let mut key_reads = 0;

        let result = super::copy_chunk_sequence_with(
            &chunks,
            |_| {
                copy_attempts += 1;
                Err("simulated clipboard failure".to_string())
            },
            || {
                key_reads += 1;
                Ok(b' ')
            },
        );

        assert_eq!(result.unwrap_err(), "simulated clipboard failure");
        assert_eq!(copy_attempts, 1);
        assert_eq!(key_reads, 0);
    }
}

pub(crate) fn copy_to_clipboard(buffer: &ClipboardBuffer) -> Result<(), String> {
    let mut child = std::process::Command::new("wl-copy")
        .arg("--type")
        .arg("text/plain")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("CVB: unable to start wl-copy: {error}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(buffer.as_bytes())
            .map_err(|error| format!("CVB: unable to write to wl-copy: {error}"))?;
    }

    let status = child
        .wait()
        .map_err(|error| format!("CVB: unable to wait for wl-copy: {error}"))?;

    if !status.success() {
        return Err(format!("CVB: wl-copy exited with status {status}"));
    }

    Ok(())
}

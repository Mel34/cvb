use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DIRECT_COPY_LIMIT: usize = 9_750;
const MAX_PASTE_BYTES: usize = 1_000_000;
const DEFAULT_PASTE_URL: &str = "https://nullpaste.org/api/v1/pastes";
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

static CAPTURE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_capture_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = CAPTURE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("{timestamp:x}-{:x}-{counter:x}", std::process::id())
}


const MULTIPART_MAX_PART_BYTES: usize = DIRECT_COPY_LIMIT - 1;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MultipartPart {
    pub(crate) number: usize,
    pub(crate) total: usize,
    header: String,
    payload: Vec<u8>,
}

#[allow(dead_code)]
impl MultipartPart {
    pub(crate) fn clipboard_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.header.len() + self.payload.len());
        bytes.extend_from_slice(self.header.as_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }
}

fn multipart_header(capture_id: &str, number: usize, total: usize) -> String {
    format!(
        "[CVB MULTIPART CAPTURE — ID: {capture_id}] Part {number}/{total}. Do not analyze or respond yet. Wait until all parts have arrived and I explicitly say DONE.\n"
    )
}

#[allow(dead_code)]
fn multipart_parts(
    contents: &[u8],
    capture_id: &str,
) -> Result<Vec<MultipartPart>, String> {
    let text = std::str::from_utf8(contents).map_err(|_| {
        "CVB: multipart capture contains invalid UTF-8; refusing to split it".to_string()
    })?;

    if capture_id.is_empty()
        || !capture_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
        })
    {
        return Err("CVB: multipart capture ID must contain only ASCII letters, digits, '-' or '_'".to_string());
    }

    let mut total_guess = 1usize;

    loop {
        let payloads = split_multipart_payloads(text, capture_id, total_guess)?;
        let actual_total = payloads.len();

        if actual_total == total_guess {
            return Ok(payloads
                .into_iter()
                .enumerate()
                .map(|(index, payload)| {
                    let number = index + 1;
                    MultipartPart {
                        number,
                        total: actual_total,
                        header: multipart_header(capture_id, number, actual_total),
                        payload,
                    }
                })
                .collect());
        }

        total_guess = actual_total;
    }
}

fn split_multipart_payloads(
    text: &str,
    capture_id: &str,
    total_guess: usize,
) -> Result<Vec<Vec<u8>>, String> {
    let mut payloads = Vec::new();
    let mut offset = 0usize;

    if text.is_empty() {
        return Ok(vec![Vec::new()]);
    }

    while offset < text.len() {
        let number = payloads.len() + 1;
        let header = multipart_header(capture_id, number, total_guess);
        let available = MULTIPART_MAX_PART_BYTES
            .checked_sub(header.len())
            .ok_or_else(|| "CVB: multipart header exceeds the part size limit".to_string())?;

        if available == 0 {
            return Err("CVB: multipart header leaves no room for content".to_string());
        }

        let mut end = offset.saturating_add(available).min(text.len());

        while end > offset && !text.is_char_boundary(end) {
            end -= 1;
        }

        if end == offset {
            return Err("CVB: unable to find a valid UTF-8 multipart boundary".to_string());
        }

        payloads.push(text.as_bytes()[offset..end].to_vec());
        offset = end;
    }

    Ok(payloads)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClipboardAction {
    CopyDirectly,
    UploadAndCopyUrl,
}

pub(crate) fn action_for(buffer: &ClipboardBuffer) -> ClipboardAction {
    if buffer.len() < DIRECT_COPY_LIMIT {
        ClipboardAction::CopyDirectly
    } else {
        ClipboardAction::UploadAndCopyUrl
    }
}

fn dispatch<T>(
    buffer: &ClipboardBuffer,
    copy_directly: impl FnOnce(&ClipboardBuffer) -> Result<T, String>,
    upload_and_copy_url: impl FnOnce(&ClipboardBuffer) -> Result<T, String>,
) -> Result<T, String> {
    match action_for(buffer) {
        ClipboardAction::CopyDirectly => copy_directly(buffer),
        ClipboardAction::UploadAndCopyUrl => upload_and_copy_url(buffer),
    }
}

pub(crate) fn process(buffer: &ClipboardBuffer) -> Result<(), String> {
    dispatch(buffer, copy_to_clipboard, upload_and_copy_url)
}

fn upload_and_copy_url(buffer: &ClipboardBuffer) -> Result<(), String> {
    if buffer.len() > MAX_PASTE_BYTES {
        return Err(format!(
            "CVB: captured text exceeds NullPaste's 1 MB limit ({} bytes)",
            buffer.len()
        ));
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| format!("CVB: unable to create NullPaste HTTP client: {error}"))?;

    let paste_url = std::env::var("CVB_PASTE_URL")
        .unwrap_or_else(|_| DEFAULT_PASTE_URL.to_string());
    let response = client
        .post(&paste_url)
        .header(reqwest::header::CONTENT_TYPE, "text/plain")
        .body(buffer.as_bytes().to_vec())
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "CVB: NullPaste request timed out".to_string()
            } else {
                format!("CVB: NullPaste request failed: {error}")
            }
        })?;

    let status = response.status();

    if status != reqwest::StatusCode::CREATED {
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);

            return Err(match retry_after {
                Some(value) => format!(
                    "CVB: NullPaste rate limit reached (HTTP 429; Retry-After: {value})"
                ),
                None => "CVB: NullPaste rate limit reached (HTTP 429)".to_string(),
            });
        }

        if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            return Err("CVB: NullPaste rejected the paste as too large".to_string());
        }

        return Err(format!(
            "CVB: NullPaste upload failed with HTTP status {status}"
        ));
    }

    let body = response
        .bytes()
        .map_err(|error| format!("CVB: unable to read NullPaste response: {error}"))?;

    let url = parse_raw_url(&body)?;
    copy_to_clipboard(&ClipboardBuffer::new(url.into_bytes()))
}

fn parse_raw_url(body: &[u8]) -> Result<String, String> {
    let response: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "CVB: NullPaste returned invalid JSON".to_string())?;

    let raw_url = response
        .get("raw_url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "CVB: NullPaste response is missing a valid 'raw_url' field".to_string()
        })?;

    let url = reqwest::Url::parse(raw_url)
        .map_err(|_| "CVB: NullPaste response contains an invalid raw URL".to_string())?;

    let path = url.path().strip_prefix("/raw/").unwrap_or("");

    if url.scheme() != "https"
        || url.host_str() != Some("nullpaste.org")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || path.len() != 12
        || !path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
        })
    {
        return Err("CVB: NullPaste returned an unexpected raw URL".to_string());
    }

    Ok(url.to_string())
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
    use super::{ClipboardAction, ClipboardBuffer};

    #[test]
    fn capture_ids_are_unique_and_valid() {
        let first = super::new_capture_id();
        let second = super::new_capture_id();

        assert_ne!(first, second);
        assert!(super::multipart_parts(b"content", &first).is_ok());
        assert!(super::multipart_parts(b"content", &second).is_ok());
    }

    #[test]
    fn multipart_parts_reconstruct_original_utf8_exactly() {
        let original = "line one\né 🧑‍💻\n".repeat(3_000);
        let parts = super::multipart_parts(original.as_bytes(), "test-123").unwrap();

        assert!(parts.len() > 1);

        let reconstructed: Vec<u8> = parts
            .iter()
            .flat_map(|part| part.payload.iter().copied())
            .collect();

        assert_eq!(reconstructed, original.as_bytes());
    }

    #[test]
    fn multipart_parts_stay_below_direct_copy_limit_including_headers() {
        let original = "Rust 🦀 source line\n".repeat(2_000);
        let parts = super::multipart_parts(original.as_bytes(), "test-123").unwrap();
        let total = parts.len();

        for (index, part) in parts.iter().enumerate() {
            assert_eq!(part.number, index + 1);
            assert_eq!(part.total, total);
            assert!(part.clipboard_bytes().len() < super::DIRECT_COPY_LIMIT);
            assert!(part.clipboard_bytes().starts_with(part.header.as_bytes()));
            assert!(std::str::from_utf8(&part.payload).is_ok());
            assert!(part.header.contains(&format!("Part {}/{total}.", index + 1)));
        }
    }

    #[test]
    fn multipart_parts_preserve_empty_input() {
        let parts = super::multipart_parts(b"", "empty").unwrap();

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].number, 1);
        assert_eq!(parts[0].total, 1);
        assert!(parts[0].payload.is_empty());
        assert!(parts[0].clipboard_bytes().len() < super::DIRECT_COPY_LIMIT);
    }

    #[test]
    fn multipart_parts_reject_invalid_utf8() {
        let error = super::multipart_parts(&[b'a', 0xff, b'b'], "test-123").unwrap_err();

        assert!(error.contains("invalid UTF-8"));
    }

    #[test]
    fn multipart_parts_reject_unsafe_capture_ids() {
        assert!(super::multipart_parts(b"content", "").is_err());
        assert!(super::multipart_parts(b"content", "bad id").is_err());
        assert!(super::multipart_parts(b"content", "id/part").is_err());
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
    fn action_policy_uses_byte_length_boundary() {
        let below = ClipboardBuffer::new(vec![0; 9_749]);
        let at_limit = ClipboardBuffer::new(vec![0; 9_750]);
        let above = ClipboardBuffer::new(vec![0; 9_751]);

        assert_eq!(super::action_for(&below), ClipboardAction::CopyDirectly);
        assert_eq!(super::action_for(&at_limit), ClipboardAction::UploadAndCopyUrl);
        assert_eq!(super::action_for(&above), ClipboardAction::UploadAndCopyUrl);
    }

    #[test]
    fn dispatch_calls_only_direct_copy_below_limit() {
        let buffer = ClipboardBuffer::new(vec![0; 9_749]);
        let mut direct_called = false;
        let mut upload_called = false;

        let result = super::dispatch(
            &buffer,
            |_| {
                direct_called = true;
                Ok(())
            },
            |_| {
                upload_called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(direct_called);
        assert!(!upload_called);
    }

    #[test]
    fn dispatch_calls_only_upload_at_limit() {
        let buffer = ClipboardBuffer::new(vec![0; 9_750]);
        let mut direct_called = false;
        let mut upload_called = false;

        let result = super::dispatch(
            &buffer,
            |_| {
                direct_called = true;
                Ok(())
            },
            |_| {
                upload_called = true;
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert!(!direct_called);
        assert!(upload_called);
    }

    #[test]
    fn dispatch_propagates_selected_action_error() {
        let buffer = ClipboardBuffer::new(vec![0; 9_750]);

        let result = super::dispatch(
            &buffer,
            |_| Ok(()),
            |_| Err("upload failed".to_string()),
        );

        assert_eq!(result, Err("upload failed".to_string()));
    }

    #[test]
    fn parses_valid_nullpaste_raw_url() {
        let body = br#"{"raw_url":"https://nullpaste.org/raw/AbC123xYz012","delete_token":"secret"}"#;

        assert_eq!(
            super::parse_raw_url(body).unwrap(),
            "https://nullpaste.org/raw/AbC123xYz012"
        );
    }

    #[test]
    fn rejects_invalid_nullpaste_json() {
        assert!(super::parse_raw_url(b"not JSON").is_err());
    }

    #[test]
    fn rejects_missing_raw_url() {
        assert!(super::parse_raw_url(br#"{"id":"AbC123xYz012"}"#).is_err());
    }

    #[test]
    fn rejects_raw_url_from_another_host() {
        let body = br#"{"raw_url":"https://example.com/raw/AbC123xYz012"}"#;

        assert!(super::parse_raw_url(body).is_err());
    }

    #[test]
    fn rejects_raw_url_with_wrong_id_length() {
        let body = br#"{"raw_url":"https://nullpaste.org/raw/AbC123xYz01"}"#;

        assert!(super::parse_raw_url(body).is_err());
    }

    #[test]
    fn rejects_raw_url_with_invalid_id_character() {
        let body = br#"{"raw_url":"https://nullpaste.org/raw/AbC123xYz0.2"}"#;

        assert!(super::parse_raw_url(body).is_err());
    }

    #[test]
    fn rejects_raw_url_with_query_or_fragment() {
        assert!(super::parse_raw_url(
            br#"{"raw_url":"https://nullpaste.org/raw/AbC123xYz012?next=example.com"}"#
        ).is_err());

        assert!(super::parse_raw_url(
            br#"{"raw_url":"https://nullpaste.org/raw/AbC123xYz012#fragment"}"#
        ).is_err());
    }

    #[test]
    fn rejects_oversized_paste_without_network_request() {
        let buffer = ClipboardBuffer::new(vec![b'x'; super::MAX_PASTE_BYTES + 1]);

        let error = super::upload_and_copy_url(&buffer).unwrap_err();

        assert!(error.contains("1 MB limit"));
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

#[cfg(test)]
mod live_nullpaste_integration_test {
    use super::{ClipboardBuffer, process};

    #[test]
    #[ignore = "LIVE: uploads a public test paste and replaces the Wayland clipboard"]
    fn live_nullpaste_upload_copies_raw_url_and_opens_share_page() {
        let story = concat!(
            "NULLPASTE JOURNEY LOG\n",
            "Chapter 1 — Capture: CVB gathered this harmless test story.\n",
            "Chapter 2 — Departure: the payload crossed the 9,750-byte threshold.\n",
            "Chapter 3 — Upload: CVB sent the text to NullPaste as text/plain.\n",
            "Chapter 4 — Receipt: NullPaste returned a share URL.\n",
            "Chapter 5 — Return: CVB placed that URL on the Wayland clipboard.\n",
            "Chapter 6 — Reunion: this test will open the URL and check the page.\n",
            "Journey marker: CVB-NULLPASTE-LIVE-JOURNEY-2026-10-10\n",
            "The following repeated log entries are harmless padding for the test.\n",
        );

        let mut payload = story.as_bytes().to_vec();
        let padding = b"Journey continues: CVB delivered this story to NullPaste. \n";
        while payload.len() < 9_750 {
            payload.extend_from_slice(padding);
        }

        assert!(payload.len() < 1_000_000, "test payload unexpectedly too large");
        let expected_payload = payload.clone();
        let buffer = ClipboardBuffer::new(payload);

        process(&buffer).expect("live NullPaste upload and clipboard copy failed");

        let output = std::process::Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .expect("could not run wl-paste to inspect the resulting clipboard");

        assert!(
            output.status.success(),
            "wl-paste failed while reading the resulting clipboard"
        );

        let raw_url = String::from_utf8(output.stdout)
            .expect("clipboard URL was not valid UTF-8");

        let parsed = reqwest::Url::parse(&raw_url)
            .expect("clipboard did not contain a valid URL");

        assert_eq!(parsed.scheme(), "https", "raw URL must use HTTPS");
        assert_eq!(parsed.host_str(), Some("nullpaste.org"));
        assert!(parsed.query().is_none(), "raw URL unexpectedly has a query");
        assert!(parsed.fragment().is_none(), "raw URL unexpectedly has a fragment");

        let paste_id = parsed.path().strip_prefix("/raw/")
            .expect("clipboard did not contain NullPaste's raw URL");
        assert!(
            paste_id.len() == 12
                && paste_id.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
                }),
            "raw URL did not contain a valid paste ID"
        );

        let share_url = format!("https://nullpaste.org/{paste_id}");

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("could not create HTTP client for paste verification");

        let raw_response = client
            .get(&raw_url)
            .send()
            .expect("could not retrieve the raw NullPaste payload");

        assert!(
            raw_response.status().is_success(),
            "NullPaste raw endpoint returned HTTP {}",
            raw_response.status()
        );

        let retrieved_payload = raw_response
            .bytes()
            .expect("could not read the retrieved NullPaste payload");

        assert_eq!(
            retrieved_payload.as_ref(),
            expected_payload.as_slice(),
            "retrieved NullPaste payload differs from the uploaded bytes"
        );

        println!("PAYLOAD VERIFICATION PASSED: exact byte-for-byte match");

        let response = client
            .get(share_url.as_str())
            .send()
            .expect("could not open the returned NullPaste share URL");

        assert!(
            response.status().is_success(),
            "NullPaste share URL returned HTTP {}",
            response.status()
        );

        println!("LIVE NULLPASTE TEST PASSED");
        println!("Payload bytes: {}", buffer.len());
        println!("Share URL: {share_url}");
        println!("The share page returned HTTP {}", response.status());
    }
}

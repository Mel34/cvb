use std::io::Write;

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DIRECT_COPY_LIMIT: usize = 9_750;
const MAX_PASTE_BYTES: usize = 1_000_000;
const NULLPASTE_CREATE_URL: &str = "https://nullpaste.org/api/v1/pastes";
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

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

    let response = client
        .post(NULLPASTE_CREATE_URL)
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

    let url = parse_share_url(&body)?;
    copy_to_clipboard(&ClipboardBuffer::new(url.into_bytes()))
}

fn parse_share_url(body: &[u8]) -> Result<String, String> {
    let response: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| "CVB: NullPaste returned invalid JSON".to_string())?;

    let raw_url = response
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            "CVB: NullPaste response is missing a valid 'url' field".to_string()
        })?;

    let url = reqwest::Url::parse(raw_url)
        .map_err(|_| "CVB: NullPaste response contains an invalid share URL".to_string())?;

    let path = url.path().strip_prefix('/').unwrap_or("");

    if url.scheme() != "https"
        || url.host_str() != Some("nullpaste.org")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || path.is_empty()
        || path.contains('/')
    {
        return Err("CVB: NullPaste returned an unexpected share URL".to_string());
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
    fn parses_valid_nullpaste_share_url() {
        let body = br#"{"url":"https://nullpaste.org/AbC123xYz012","delete_token":"secret"}"#;

        assert_eq!(
            super::parse_share_url(body).unwrap(),
            "https://nullpaste.org/AbC123xYz012"
        );
    }

    #[test]
    fn rejects_invalid_nullpaste_json() {
        assert!(super::parse_share_url(b"not JSON").is_err());
    }

    #[test]
    fn rejects_missing_share_url() {
        assert!(super::parse_share_url(br#"{"id":"AbC123xYz012"}"#).is_err());
    }

    #[test]
    fn rejects_share_url_from_another_host() {
        let body = br#"{"url":"https://example.com/AbC123xYz012"}"#;

        assert!(super::parse_share_url(body).is_err());
    }

    #[test]
    fn rejects_share_url_with_query_or_fragment() {
        assert!(super::parse_share_url(
            br#"{"url":"https://nullpaste.org/AbC123xYz012?next=example.com"}"#
        ).is_err());

        assert!(super::parse_share_url(
            br#"{"url":"https://nullpaste.org/AbC123xYz012#fragment"}"#
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
    fn live_nullpaste_upload_creates_and_opens_share_url() {
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

        let share_url = String::from_utf8(output.stdout)
            .expect("clipboard URL was not valid UTF-8");

        let parsed = reqwest::Url::parse(&share_url)
            .expect("clipboard did not contain a valid URL");

        assert_eq!(parsed.scheme(), "https", "share URL must use HTTPS");
        assert_eq!(parsed.host_str(), Some("nullpaste.org"));
        assert!(parsed.query().is_none(), "share URL unexpectedly has a query");
        assert!(parsed.fragment().is_none(), "share URL unexpectedly has a fragment");
        assert!(
            parsed.path().trim_matches('/').len() > 0
                && !parsed.path().trim_matches('/').contains('/'),
            "share URL did not contain a single paste path"
        );

        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("could not create HTTP client for paste verification");

        let paste_id = parsed.path().trim_matches('/');
        let raw_url = format!("https://nullpaste.org/raw/{paste_id}");

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

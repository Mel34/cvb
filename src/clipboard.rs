use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DIRECT_COPY_LIMIT: usize = 9_750;
const MAX_CAPTURE_BYTES: usize = 1_000_000;
const REGISTRY_REMOTE: &str = "git@github.com:Mel34/cvb-registry.git";
const REGISTRY_FILE_URL: &str = "https://github.com/Mel34/cvb-registry/blob/main/captures/latest.txt";

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
    std::str::from_utf8(buffer.as_bytes()).map_err(|_| {
        "CVB: captured text is not valid UTF-8; refusing to publish it".to_string()
    })?;

    if buffer.len() > MAX_CAPTURE_BYTES {
        return Err(format!(
            "CVB: captured text exceeds the 1 MB limit ({} bytes)",
            buffer.len()
        ));
    }

    let home = std::env::var_os("HOME")
        .ok_or_else(|| "CVB: HOME is not set; cannot locate the registry clone".to_string())?;
    let repo = Path::new(&home).join(".local/share/cvb/registry");

    if !repo.join(".git").exists() {
        return Err(format!(
            "CVB: registry clone not found at {}",
            repo.display()
        ));
    }

    let branch = git_command(&repo, &["branch", "--show-current"], "inspect registry branch")?;
    if String::from_utf8_lossy(&branch.stdout).trim() != "main" {
        return Err("CVB: registry clone must be on branch 'main'".to_string());
    }

    let remote = git_command(&repo, &["remote", "get-url", "origin"], "inspect registry remote")?;
    if String::from_utf8_lossy(&remote.stdout).trim() != REGISTRY_REMOTE {
        return Err(format!(
            "CVB: unexpected registry origin; expected {REGISTRY_REMOTE}"
        ));
    }

    let status = git_command(
        &repo,
        &["status", "--porcelain", "--untracked-files=all"],
        "check registry working tree",
    )?;
    if !status.stdout.is_empty() {
        return Err(
            "CVB: registry working tree is not clean; refusing to overwrite local changes"
                .to_string(),
        );
    }

    let readme = fs::read(repo.join("README.md"))
        .map_err(|error| format!("CVB: unable to read registry README: {error}"))?;

    git_command(&repo, &["fetch", "origin", "main"], "fetch registry main")?;

    let remote_head = git_command(
        &repo,
        &["rev-parse", "refs/remotes/origin/main"],
        "read fetched registry commit",
    )?;
    let remote_sha = String::from_utf8_lossy(&remote_head.stdout).trim().to_string();

    if remote_sha.is_empty()
        || !remote_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("CVB: fetched registry commit ID is invalid".to_string());
    }

    let temp_branch = format!("cvb-upload-{}", new_capture_id());
    let mut orphan_created = false;

    let prepare_and_push = (|| -> Result<(), String> {
        git_command(
            &repo,
            &["switch", "--orphan", temp_branch.as_str()],
            "create parentless registry branch",
        )?;
        orphan_created = true;

        fs::create_dir_all(repo.join("captures"))
            .map_err(|error| format!("CVB: unable to create registry captures directory: {error}"))?;
        fs::write(repo.join("README.md"), &readme)
            .map_err(|error| format!("CVB: unable to restore registry README: {error}"))?;
        fs::write(repo.join("captures/latest.txt"), buffer.as_bytes())
            .map_err(|error| format!("CVB: unable to write latest capture: {error}"))?;

        git_command(&repo, &["add", "-A"], "stage latest capture")?;
        git_command(
            &repo,
            &["commit", "-m", "Update latest capture"],
            "commit latest capture",
        )?;

        let commit = git_command(
            &repo,
            &["rev-list", "--parents", "-n", "1", "HEAD"],
            "verify parentless capture commit",
        )?;
        if String::from_utf8_lossy(&commit.stdout)
            .split_whitespace()
            .count()
            != 1
        {
            return Err("CVB: new registry commit unexpectedly has a parent".to_string());
        }

        let lease = format!("--force-with-lease=refs/heads/main:{remote_sha}");
        git_command(
            &repo,
            &[lease.as_str(), "origin", "HEAD:refs/heads/main"],
            "push latest capture to registry",
        )?;

        Ok(())
    })();

    if let Err(error) = prepare_and_push {
        if orphan_created {
            let _ = git_command(
                &repo,
                &["switch", "--discard-changes", "main"],
                "restore registry main after failure",
            );
            let _ = git_command(
                &repo,
                &["branch", "-D", temp_branch.as_str()],
                "remove temporary registry branch",
            );
        }
        return Err(error);
    }

    git_command(
        &repo,
        &["switch", "-C", "main", "HEAD"],
        "update local registry main",
    )?;
    let _ = git_command(
        &repo,
        &["branch", "-D", temp_branch.as_str()],
        "remove temporary registry branch",
    );

    let message = handoff_message();
    copy_to_clipboard(&ClipboardBuffer::new(message.into_bytes()))
}

fn git_command(repo: &Path, args: &[&str], operation: &str) -> Result<Output, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("CVB: unable to {operation}: {error}"))?;

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("CVB: unable to {operation} (git exited with {})", output.status)
        } else {
            format!("CVB: unable to {operation}: {detail}")
        });
    }

    Ok(output)
}

fn handoff_message() -> String {
    format!(
        "Hey Chadgpt, check our GitHub registry — `captures/latest.txt` has new data for you!\n\n{REGISTRY_FILE_URL}\n"
    )
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
    fn handoff_message_contains_stable_github_url() {
        let message = super::handoff_message();

        assert!(message.contains("Hey Chadgpt"));
        assert!(message.contains(
            "https://github.com/Mel34/cvb-registry/blob/main/captures/latest.txt"
        ));
    }

    #[test]
    fn rejects_invalid_utf8_before_accessing_registry() {
        let buffer = ClipboardBuffer::new(vec![b'a', 0xff, b'b']);

        let error = super::upload_and_copy_url(&buffer).unwrap_err();

        assert!(error.contains("valid UTF-8"));
    }

    #[test]
    fn rejects_oversized_capture_before_accessing_registry() {
        let buffer = ClipboardBuffer::new(vec![b'x'; super::MAX_CAPTURE_BYTES + 1]);

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

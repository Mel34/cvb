use std::io::Write;

pub(crate) struct ClipboardBuffer {
    contents: Vec<u8>,
}

const DIRECT_COPY_LIMIT: usize = 9_750;

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
    dispatch(
        buffer,
        copy_to_clipboard,
        |_| Err("CVB: upload is not implemented yet; clipboard unchanged".to_string()),
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

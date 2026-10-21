use std::os::fd::{AsFd, OwnedFd};

use nix::unistd::read;

const HEADER_SIZE: usize = 4;
const MAX_FRAME_SIZE: usize = 4 * 1024 * 1024;

const EXIT: u8 = 0x03;
const TARGET_REQUEST: u8 = 0x04;
const TARGET_RESPONSE: u8 = 0x05;
const SURGERY_BEGIN: u8 = 0x06;
const CLIPBOARD_WRITE: u8 = 0x07;
const SURGERY_END: u8 = 0x08;

#[derive(Debug, PartialEq, Eq)]
pub enum ControlMessage {
    Exit,
    TargetRequest {
        id: u64,
        cwd: String,
        args: Vec<String>,
    },
    TargetResponse {
        id: u64,
        status: i32,
        message: String,
    },
    SurgeryBegin {
        token: String,
    },
    ClipboardWrite {
        text: String,
    },
    SurgeryEnd {
        status: u8,
        outcome: String,
    },
}

pub struct ControlChannel {
    fd: OwnedFd,
    receive_buffer: Vec<u8>,
}

impl ControlChannel {
    pub fn new(fd: OwnedFd) -> Self {
        Self {
            fd,
            receive_buffer: Vec::new(),
        }
    }

    pub fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    pub fn is_control_fd_socket(raw_fd: std::os::fd::RawFd) -> bool {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let res = unsafe { libc::fstat(raw_fd, stat.as_mut_ptr()) };
        if res != 0 {
            return false;
        }
        let stat = unsafe { stat.assume_init() };
        (stat.st_mode & libc::S_IFMT) == libc::S_IFSOCK
    }

    pub fn client_on_fd(raw_fd: std::os::fd::RawFd) -> Result<Self, String> {
        use std::os::fd::FromRawFd;

        if !Self::is_control_fd_socket(raw_fd) {
            return Err(format!("CVB: descriptor {raw_fd} is not a valid control socket"));
        }

        let dup_fd = unsafe { libc::dup(raw_fd) };
        if dup_fd == -1 {
            return Err(format!(
                "CVB: unable to duplicate control descriptor {raw_fd}: {}",
                std::io::Error::last_os_error()
            ));
        }

        let owned = unsafe { OwnedFd::from_raw_fd(dup_fd) };
        Ok(Self::new(owned))
    }

    pub fn send(&self, message: &ControlMessage) -> Result<(), String> {
        let payload = encode(message)?;
        let length = (payload.len() as u32).to_be_bytes();

        let mut frame = Vec::with_capacity(HEADER_SIZE + payload.len());
        frame.extend_from_slice(&length);
        frame.extend_from_slice(&payload);

        let mut written = 0;

        while written < frame.len() {
            let count = match nix::unistd::write(self.fd.as_fd(), &frame[written..]) {
                Ok(count) => count,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(format!("CVB: unable to send control message: {error}")),
            };

            if count == 0 {
                return Err("CVB: control message write returned zero bytes".to_string());
            }

            written += count;
        }

        Ok(())
    }

    pub fn receive(&mut self) -> Result<Vec<ControlMessage>, String> {
        let mut buffer = [0u8; 4096];
        let mut messages = decode_frames(&mut self.receive_buffer)?;

        loop {
            match read(self.fd.as_fd(), &mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    self.receive_buffer.extend_from_slice(&buffer[..count]);

                    messages.append(&mut decode_frames(&mut self.receive_buffer)?);

                    if count < buffer.len() {
                        break;
                    }
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(error) => {
                    return Err(format!("CVB: unable to receive control message: {error}"));
                }
            }
        }

        messages.append(&mut decode_frames(&mut self.receive_buffer)?);

        Ok(messages)
    }

    pub fn receive_one(&mut self) -> Result<ControlMessage, String> {
        loop {
            if self.receive_buffer.len() >= HEADER_SIZE {
                let length = u32::from_be_bytes(
                    self.receive_buffer[..HEADER_SIZE]
                        .try_into()
                        .map_err(|_| "CVB: invalid control message header".to_string())?,
                ) as usize;

                if length == 0 {
                    return Err("CVB: control message has empty payload".to_string());
                }

                if length > MAX_FRAME_SIZE {
                    return Err("CVB: control message exceeds maximum frame size".to_string());
                }

                let frame_size = HEADER_SIZE + length;

                if self.receive_buffer.len() >= frame_size {
                    let payload = self.receive_buffer[HEADER_SIZE..frame_size].to_vec();
                    self.receive_buffer.drain(..frame_size);

                    return decode(&payload);
                }
            }

            let mut buffer = [0u8; 4096];

            match read(self.fd.as_fd(), &mut buffer) {
                Ok(0) => {
                    return Err(
                        "CVB: control channel closed while waiting for response".to_string()
                    );
                }
                Ok(count) => {
                    self.receive_buffer.extend_from_slice(&buffer[..count]);
                }
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => {
                    return Err(format!("CVB: unable to receive control message: {error}"));
                }
            }
        }
    }
}

fn encode(message: &ControlMessage) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();

    match message {
        ControlMessage::Exit => {
            payload.push(EXIT);
        }

        ControlMessage::TargetRequest { id, cwd, args } => {
            payload.push(TARGET_REQUEST);
            payload.extend_from_slice(&id.to_be_bytes());

            let cwd = cwd.as_bytes();
            payload.extend_from_slice(&(cwd.len() as u32).to_be_bytes());
            payload.extend_from_slice(cwd);
            payload.extend_from_slice(&(args.len() as u32).to_be_bytes());

            for arg in args {
                let arg = arg.as_bytes();
                payload.extend_from_slice(&(arg.len() as u32).to_be_bytes());
                payload.extend_from_slice(arg);
            }
        }

        ControlMessage::TargetResponse {
            id,
            status,
            message,
        } => {
            payload.push(TARGET_RESPONSE);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&status.to_be_bytes());

            let message = message.as_bytes();
            payload.extend_from_slice(&(message.len() as u32).to_be_bytes());
            payload.extend_from_slice(message);
        }

        ControlMessage::SurgeryBegin { token } => {
            payload.push(SURGERY_BEGIN);
            let token = token.as_bytes();
            payload.extend_from_slice(&(token.len() as u32).to_be_bytes());
            payload.extend_from_slice(token);
        }

        ControlMessage::ClipboardWrite { text } => {
            payload.push(CLIPBOARD_WRITE);
            let text = text.as_bytes();
            payload.extend_from_slice(&(text.len() as u32).to_be_bytes());
            payload.extend_from_slice(text);
        }

        ControlMessage::SurgeryEnd { status, outcome } => {
            payload.push(SURGERY_END);
            payload.push(*status);
            let outcome = outcome.as_bytes();
            payload.extend_from_slice(&(outcome.len() as u32).to_be_bytes());
            payload.extend_from_slice(outcome);
        }
    }

    if payload.len() > MAX_FRAME_SIZE {
        return Err("CVB: control message exceeds maximum frame size".to_string());
    }

    Ok(payload)
}

fn decode_frames(buffer: &mut Vec<u8>) -> Result<Vec<ControlMessage>, String> {
    let mut messages = Vec::new();

    loop {
        if buffer.len() < HEADER_SIZE {
            break;
        }

        let length = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;

        if length == 0 {
            return Err("CVB: control message has empty payload".to_string());
        }

        if length > MAX_FRAME_SIZE {
            return Err("CVB: control message exceeds maximum frame size".to_string());
        }

        if buffer.len() < HEADER_SIZE + length {
            break;
        }

        let payload = buffer[HEADER_SIZE..HEADER_SIZE + length].to_vec();

        buffer.drain(..HEADER_SIZE + length);

        messages.push(decode(&payload)?);
    }

    Ok(messages)
}

fn decode(payload: &[u8]) -> Result<ControlMessage, String> {
    let message_type = payload[0];

    match message_type {
        EXIT => {
            if payload.len() != 1 {
                return Err("CVB: malformed EXIT message".to_string());
            }

            Ok(ControlMessage::Exit)
        }

        TARGET_REQUEST => {
            if payload.len() < 17 {
                return Err("CVB: malformed TARGET_REQUEST message".to_string());
            }

            let id = u64::from_be_bytes(
                payload[1..9]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
            );

            let cwd_length = u32::from_be_bytes(
                payload[9..13]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
            ) as usize;

            let mut offset = 13;
            if payload.len() < offset + cwd_length + 4 {
                return Err("CVB: malformed TARGET_REQUEST message".to_string());
            }

            let cwd = std::str::from_utf8(&payload[offset..offset + cwd_length])
                .map_err(|_| {
                    "CVB: TARGET_REQUEST working directory is not valid UTF-8".to_string()
                })?
                .to_string();
            offset += cwd_length;

            let argc = u32::from_be_bytes(
                payload[offset..offset + 4]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
            ) as usize;
            offset += 4;

            let mut args = Vec::with_capacity(argc);

            for _ in 0..argc {
                if payload.len() < offset + 4 {
                    return Err("CVB: malformed TARGET_REQUEST message".to_string());
                }

                let length = u32::from_be_bytes(
                    payload[offset..offset + 4]
                        .try_into()
                        .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
                ) as usize;

                offset += 4;

                if payload.len() < offset + length {
                    return Err("CVB: malformed TARGET_REQUEST message".to_string());
                }

                let arg = std::str::from_utf8(&payload[offset..offset + length])
                    .map_err(|_| "CVB: TARGET_REQUEST argument is not valid UTF-8".to_string())?
                    .to_string();

                args.push(arg);
                offset += length;
            }

            if offset != payload.len() {
                return Err("CVB: malformed TARGET_REQUEST message".to_string());
            }

            Ok(ControlMessage::TargetRequest { id, cwd, args })
        }

        TARGET_RESPONSE => {
            if payload.len() < 17 {
                return Err("CVB: malformed TARGET_RESPONSE message".to_string());
            }

            let id = u64::from_be_bytes(
                payload[1..9]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_RESPONSE message".to_string())?,
            );

            let status = i32::from_be_bytes(
                payload[9..13]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_RESPONSE message".to_string())?,
            );

            let message_length = u32::from_be_bytes(
                payload[13..17]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_RESPONSE message".to_string())?,
            ) as usize;

            if payload.len() != 17 + message_length {
                return Err("CVB: malformed TARGET_RESPONSE message".to_string());
            }

            let message = std::str::from_utf8(&payload[17..])
                .map_err(|_| "CVB: TARGET_RESPONSE message is not valid UTF-8".to_string())?
                .to_string();

            Ok(ControlMessage::TargetResponse {
                id,
                status,
                message,
            })
        }

        SURGERY_BEGIN => {
            if payload.len() < 5 {
                return Err("CVB: malformed SURGERY_BEGIN message".to_string());
            }

            let token_length = u32::from_be_bytes(
                payload[1..5]
                    .try_into()
                    .map_err(|_| "CVB: malformed SURGERY_BEGIN message".to_string())?,
            ) as usize;

            if payload.len() != 5 + token_length {
                return Err("CVB: malformed SURGERY_BEGIN message".to_string());
            }

            let token = std::str::from_utf8(&payload[5..])
                .map_err(|_| "CVB: SURGERY_BEGIN token is not valid UTF-8".to_string())?
                .to_string();

            Ok(ControlMessage::SurgeryBegin { token })
        }

        CLIPBOARD_WRITE => {
            if payload.len() < 5 {
                return Err("CVB: malformed CLIPBOARD_WRITE message".to_string());
            }

            let text_length = u32::from_be_bytes(
                payload[1..5]
                    .try_into()
                    .map_err(|_| "CVB: malformed CLIPBOARD_WRITE message".to_string())?,
            ) as usize;

            if payload.len() != 5 + text_length {
                return Err("CVB: malformed CLIPBOARD_WRITE message".to_string());
            }

            let text = std::str::from_utf8(&payload[5..])
                .map_err(|_| "CVB: CLIPBOARD_WRITE text is not valid UTF-8".to_string())?
                .to_string();

            Ok(ControlMessage::ClipboardWrite { text })
        }

        SURGERY_END => {
            if payload.len() < 6 {
                return Err("CVB: malformed SURGERY_END message".to_string());
            }

            let status = payload[1];
            let outcome_length = u32::from_be_bytes(
                payload[2..6]
                    .try_into()
                    .map_err(|_| "CVB: malformed SURGERY_END message".to_string())?,
            ) as usize;

            if payload.len() != 6 + outcome_length {
                return Err("CVB: malformed SURGERY_END message".to_string());
            }

            let outcome = std::str::from_utf8(&payload[6..])
                .map_err(|_| "CVB: SURGERY_END outcome is not valid UTF-8".to_string())?
                .to_string();

            Ok(ControlMessage::SurgeryEnd { status, outcome })
        }

        _ => Err(format!(
            "CVB: unknown control message type: 0x{message_type:02x}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_exit() {
        let message = ControlMessage::Exit;

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_target_request() {
        let message = ControlMessage::TargetRequest {
            id: 42,
            cwd: "/tmp/cvb-test".to_string(),
            args: vec![
                "src/foo.rs".to_string(),
                "a path with spaces".to_string(),
                "--literal=$HOME".to_string(),
            ],
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_target_response() {
        let message = ControlMessage::TargetResponse {
            id: 42,
            status: 17,
            message: "hello".to_string(),
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn waits_for_partial_frame() {
        let payload = encode(&ControlMessage::Exit).unwrap();

        let mut buffer = Vec::new();

        buffer.extend_from_slice(&(payload.len() as u32).to_be_bytes());

        let messages = decode_frames(&mut buffer).unwrap();

        assert!(messages.is_empty());

        buffer.extend_from_slice(&payload);

        let messages = decode_frames(&mut buffer).unwrap();

        assert_eq!(messages, vec![ControlMessage::Exit]);
    }

    #[test]
    fn decodes_frame_at_permitted_4mib_limit() {
        const FOUR_MIB: usize = 4 * 1024 * 1024;
        let message_len = FOUR_MIB - 17; // TargetResponse header overhead is 17 bytes
        let message = ControlMessage::TargetResponse {
            id: 1,
            status: 0,
            message: "x".repeat(message_len),
        };
        let payload = encode(&message).unwrap();
        assert_eq!(payload.len(), FOUR_MIB);

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buffer.extend_from_slice(&payload);

        let messages = decode_frames(&mut buffer).unwrap();
        assert_eq!(messages, vec![message]);
    }

    #[test]
    fn rejects_frame_exceeding_4mib_limit() {
        let oversized_len = 4 * 1024 * 1024 + 1;
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&(oversized_len as u32).to_be_bytes());
        let err = decode_frames(&mut buffer).unwrap_err();
        assert_eq!(err, "CVB: control message exceeds maximum frame size");
    }

    #[test]
    fn encodes_and_decodes_surgery_begin() {
        let message = ControlMessage::SurgeryBegin {
            token: "0123456789abcdef".to_string(),
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_clipboard_write() {
        let message = ControlMessage::ClipboardWrite {
            text: "--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_surgery_end() {
        let message = ControlMessage::SurgeryEnd {
            status: 0,
            outcome: "CVB surgery APPLIED id=test files=1\n".to_string(),
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn verifies_socket_descriptor() {
        use std::os::fd::AsRawFd;

        let mut fds = [0; 2];
        let res = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(res, 0);

        assert!(ControlChannel::is_control_fd_socket(fds[0]));
        assert!(ControlChannel::is_control_fd_socket(fds[1]));

        let client = ControlChannel::client_on_fd(fds[0]).unwrap();
        assert!(ControlChannel::is_control_fd_socket(client.fd().as_raw_fd()));

        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }

        assert!(!ControlChannel::is_control_fd_socket(fds[0]));
        assert!(!ControlChannel::is_control_fd_socket(-1));
        assert!(ControlChannel::client_on_fd(-1).is_err());
    }

    #[test]
    fn rejects_non_socket_descriptor() {
        use nix::unistd::pipe;
        use std::os::fd::AsRawFd;

        let (read_end, write_end) = pipe().unwrap();
        assert!(!ControlChannel::is_control_fd_socket(read_end.as_raw_fd()));
        assert!(!ControlChannel::is_control_fd_socket(write_end.as_raw_fd()));
        assert!(ControlChannel::client_on_fd(read_end.as_raw_fd()).is_err());
    }
}

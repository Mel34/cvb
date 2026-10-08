use std::os::fd::{AsFd, OwnedFd};

use nix::unistd::read;

const HEADER_SIZE: usize = 4;
const MAX_FRAME_SIZE: usize = 1024 * 1024;

const EXIT: u8 = 0x03;
const TARGET_REQUEST: u8 = 0x04;
const TARGET_RESPONSE: u8 = 0x05;

#[derive(Debug, PartialEq, Eq)]
pub enum ControlMessage {
    Exit,
    TargetRequest { id: u64, args: Vec<String> },
    TargetResponse { id: u64, status: i32 },
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

    pub fn receive(&mut self) -> Result<Vec<ControlMessage>, String> {
        let mut buffer = [0u8; 4096];

        loop {
            match read(self.fd.as_fd(), &mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    self.receive_buffer.extend_from_slice(&buffer[..count]);

                    if self.receive_buffer.len() > HEADER_SIZE + MAX_FRAME_SIZE {
                        return Err("CVB: control message exceeds maximum frame size".to_string());
                    }

                    if count < buffer.len() {
                        break;
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => break,
                Err(error) => {
                    return Err(format!("CVB: unable to receive control message: {error}"));
                }
            }
        }

        decode_frames(&mut self.receive_buffer)
    }
}

#[cfg(test)]
fn encode(message: &ControlMessage) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();

    match message {
        ControlMessage::Exit => {
            payload.push(EXIT);
        }

        ControlMessage::TargetRequest { id, args } => {
            payload.push(TARGET_REQUEST);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&(args.len() as u32).to_be_bytes());

            for arg in args {
                let arg = arg.as_bytes();
                payload.extend_from_slice(&(arg.len() as u32).to_be_bytes());
                payload.extend_from_slice(arg);
            }
        }

        ControlMessage::TargetResponse { id, status } => {
            payload.push(TARGET_RESPONSE);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&status.to_be_bytes());
        }
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
            if payload.len() < 13 {
                return Err("CVB: malformed TARGET_REQUEST message".to_string());
            }

            let id = u64::from_be_bytes(
                payload[1..9]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
            );

            let argc = u32::from_be_bytes(
                payload[9..13]
                    .try_into()
                    .map_err(|_| "CVB: malformed TARGET_REQUEST message".to_string())?,
            ) as usize;

            let mut offset = 13;
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

            Ok(ControlMessage::TargetRequest { id, args })
        }

        TARGET_RESPONSE => {
            if payload.len() != 13 {
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

            Ok(ControlMessage::TargetResponse { id, status })
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
}

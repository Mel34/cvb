use std::os::fd::{AsFd, OwnedFd};

use nix::unistd::read;

const HEADER_SIZE: usize = 4;
const MAX_FRAME_SIZE: usize = 1024 * 1024;

const START: u8 = 0x01;
const END: u8 = 0x02;
const EXIT: u8 = 0x03;

#[derive(Debug, PartialEq, Eq)]
pub enum ControlMessage {
    Start {
        id: u64,
        command: String,
        copy: bool,
    },
    End { id: u64, status: i32 },
    Exit,
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
        ControlMessage::Start { id, command, copy } => {
            payload.push(START);
            payload.push(u8::from(*copy));
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(command.as_bytes());
        }

        ControlMessage::End { id, status } => {
            payload.push(END);
            payload.extend_from_slice(&id.to_be_bytes());
            payload.extend_from_slice(&status.to_be_bytes());
        }

        ControlMessage::Exit => {
            payload.push(EXIT);
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
        START => {
            if payload.len() < 10 {
                return Err("CVB: malformed START message".to_string());
            }

            let copy = match payload[1] {
                0 => false,
                1 => true,
                _ => return Err("CVB: malformed START message".to_string()),
            };

            let id = u64::from_be_bytes(
                payload[2..10]
                    .try_into()
                    .map_err(|_| "CVB: malformed START message".to_string())?,
            );

            let command = std::str::from_utf8(&payload[10..])
                .map_err(|_| "CVB: START command is not valid UTF-8".to_string())?
                .to_string();

            Ok(ControlMessage::Start { id, command, copy })
        }

        END => {
            if payload.len() != 13 {
                return Err("CVB: malformed END message".to_string());
            }

            let id = u64::from_be_bytes(
                payload[1..9]
                    .try_into()
                    .map_err(|_| "CVB: malformed END message".to_string())?,
            );

            let status = i32::from_be_bytes(
                payload[9..13]
                    .try_into()
                    .map_err(|_| "CVB: malformed END message".to_string())?,
            );

            Ok(ControlMessage::End { id, status })
        }

        EXIT => {
            if payload.len() != 1 {
                return Err("CVB: malformed EXIT message".to_string());
            }

            Ok(ControlMessage::Exit)
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
    fn encodes_and_decodes_start() {
        let message = ControlMessage::Start {
            id: 42,
            command: "cargo build --release".to_string(),
            copy: true,
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_start_without_copy() {
        let message = ControlMessage::Start {
            id: 42,
            command: " echo ignored".to_string(),
            copy: false,
        };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_end() {
        let message = ControlMessage::End { id: 42, status: 17 };

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn encodes_and_decodes_exit() {
        let message = ControlMessage::Exit;

        let payload = encode(&message).unwrap();

        assert_eq!(decode(&payload).unwrap(), message);
    }

    #[test]
    fn decodes_multiple_frames() {
        let first = encode(&ControlMessage::Start {
            id: 1,
            command: "echo one".to_string(),
            copy: true,
        })
        .unwrap();

        let second = encode(&ControlMessage::End { id: 1, status: 0 }).unwrap();

        let mut buffer = Vec::new();

        buffer.extend_from_slice(&(first.len() as u32).to_be_bytes());
        buffer.extend_from_slice(&first);
        buffer.extend_from_slice(&(second.len() as u32).to_be_bytes());
        buffer.extend_from_slice(&second);

        let messages = decode_frames(&mut buffer).unwrap();

        assert_eq!(
            messages,
            vec![
                ControlMessage::Start {
                    id: 1,
                    command: "echo one".to_string(),
                    copy: true,
                },
                ControlMessage::End { id: 1, status: 0 },
            ]
        );

        assert!(buffer.is_empty());
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
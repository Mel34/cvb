use evdev::{Device, EventType};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use std::os::fd::{AsFd, BorrowedFd};

pub struct HotkeyMonitor {
    device: Device,
    ctrl: bool,
    shift: bool,
}

impl HotkeyMonitor {
    pub fn new() -> Result<Self, String> {
        let device = Device::open("/dev/input/event2")
            .map_err(|error| format!("CVB: unable to open keyboard: {error}"))?;

        Ok(Self {
            device,
            ctrl: false,
            shift: false,
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.device.as_fd()
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        let borrowed_fd = self.device.as_fd();
        let mut poll_fd = [PollFd::new(borrowed_fd, PollFlags::POLLIN)];

        let ready = poll(&mut poll_fd, PollTimeout::ZERO)
            .map_err(|error| format!("CVB: unable to poll keyboard: {error}"))?;

        if ready == 0 {
            return Ok(false);
        }

        let mut detected = false;

        for event in self
            .device
            .fetch_events()
            .map_err(|error| format!("CVB: unable to read keyboard events: {error}"))?
        {
            if event.event_type() != EventType::KEY {
                continue;
            }

            let code = event.code();
            let value = event.value();

            match code {
                29 | 97 => {
                    self.ctrl = value != 0;
                }

                42 | 54 => {
                    self.shift = value != 0;
                }

                30 if value == 1 && self.ctrl && self.shift => {
                    detected = true;
                }

                _ => {}
            }
        }

        Ok(detected)
    }
}
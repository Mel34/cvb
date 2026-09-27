use ashpd::desktop::{
    PersistMode,
    remote_desktop::{DeviceType, RemoteDesktop},
};
use ashpd::register_host_app;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use reis::PendingRequestResult;
use reis::ei::{self, Context};
use std::os::fd::AsFd;
use xkbcommon::xkb;

pub struct Input {
    context: Context,
    device: Option<ei::Device>,
    keyboard: Option<ei::keyboard::Keyboard>,
    last_serial: u32,
    ctrl_keycode: u32,
    c_keycode: u32,
    escape_keycode: u32,
}

impl Input {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        register_host_app("io.github.mel34.cvb".parse()?).await?;

        let remote_desktop = RemoteDesktop::new().await?;

        let session = remote_desktop.create_session().await?;

        remote_desktop
            .select_devices(
                &session,
                DeviceType::Keyboard.into(),
                None,
                PersistMode::DoNot,
            )
            .await?
            .response()?;

        remote_desktop.start(&session, None).await?.response()?;

        let eis_fd = remote_desktop.connect_to_eis(&session).await?;
        let eis_socket = std::os::unix::net::UnixStream::from(eis_fd);
        let context = Context::new(eis_socket)?;

        let handshake = context.handshake();

        handshake.handshake_version(1);
        context.flush()?;

        let mut device_interfaces: Vec<reis::Object> = Vec::new();
        let mut device = None;
        let mut keyboard = None;
        let mut resumed = false;
        let mut last_serial = 0;
        let mut keymap = None;

        loop {
            let borrowed_fd = context.as_fd();
            let mut poll_fd = [PollFd::new(borrowed_fd, PollFlags::POLLIN)];

            poll(&mut poll_fd, PollTimeout::NONE)?;
            context.read()?;

            while let Some(result) = context.pending_event() {
                let event = match result {
                    PendingRequestResult::Request(event) => event,
                    PendingRequestResult::ParseError(error) => {
                        return Err(format!("EIS parse error: {error}").into());
                    }
                    PendingRequestResult::InvalidObject(object) => {
                        return Err(format!("EIS invalid object: {object}").into());
                    }
                };

                match event {
                    ei::Event::Handshake(handshake, event) => match event {
                        ei::handshake::Event::HandshakeVersion { version: _ } => {
                            handshake.handshake_version(1);
                            handshake.name("cvb");
                            handshake.context_type(ei::handshake::ContextType::Sender);

                            handshake.interface_version("ei_connection", 1);
                            handshake.interface_version("ei_callback", 1);
                            handshake.interface_version("ei_pingpong", 1);
                            handshake.interface_version("ei_seat", 1);
                            handshake.interface_version("ei_device", 1);
                            handshake.interface_version("ei_keyboard", 1);

                            handshake.finish();
                            context.flush()?;
                        }

                        ei::handshake::Event::Connection {
                            connection: _,
                            serial,
                        } => {
                            last_serial = serial;
                        }

                        _ => {}
                    },

                    ei::Event::Connection(_, event) => match event {
                        ei::connection::Event::Ping { ping } => {
                            ping.done(0);
                            context.flush()?;
                        }

                        _ => {}
                    },

                    ei::Event::Seat(seat, event) => match event {
                        ei::seat::Event::Capability { mask, interface } => {
                            if interface == "ei_keyboard" {
                                seat.bind(mask);
                                context.flush()?;
                            }
                        }

                        _ => {}
                    },

                    ei::Event::Device(device_event, event) => match event {
                        ei::device::Event::Interface { object } => {
                            device_interfaces.push(object);
                        }

                        ei::device::Event::Resumed { serial } => {
                            last_serial = serial;
                            resumed = true;
                        }

                        ei::device::Event::Done => {
                            device = Some(device_event);

                            for object in &device_interfaces {
                                if let Some(interface) =
                                    object.clone().downcast::<ei::keyboard::Keyboard>()
                                {
                                    keyboard = Some(interface);
                                    break;
                                }
                            }
                        }

                        _ => {}
                    },

                    ei::Event::Keyboard(_, event) => {
                        if let ei::keyboard::Event::Keymap {
                            keymap_type,
                            size,
                            keymap: keymap_fd,
                        } = event
                        {
                            if keymap_type != ei::keyboard::KeymapType::Xkb {
                                return Err("CVB: unsupported EIS keyboard keymap".into());
                            }

                            let xkb_context = xkb::Context::new(0);

                            let xkb_keymap = unsafe {
                                xkb::Keymap::new_from_fd(
                                    &xkb_context,
                                    keymap_fd,
                                    size as _,
                                    xkb::KEYMAP_FORMAT_TEXT_V1,
                                    0,
                                )
                            }?
                            .ok_or("CVB: EIS keyboard keymap is invalid")?;

                            let ctrl_keycode = find_keycode(
                                &xkb_keymap,
                                xkb::Keysym::Control_L,
                            )
                            .ok_or("CVB: unable to find Control_L in XKB keymap")?;

                            let c_keycode = find_keycode(&xkb_keymap, xkb::Keysym::C)
                                .ok_or("CVB: unable to find C in XKB keymap")?;

                            let escape_keycode = find_keycode(
                                &xkb_keymap,
                                xkb::Keysym::Escape,
                            )
                            .ok_or("CVB: unable to find Escape in XKB keymap")?;

                            keymap = Some((
                                xkb_keymap,
                                ctrl_keycode,
                                c_keycode,
                                escape_keycode,
                            ));
                        }
                    }

                    _ => {}
                }
            }

            if keyboard.is_some() && resumed && keymap.is_some() {
                break;
            }
        }

        device
            .as_ref()
            .ok_or("CVB: EIS device is not available")?
            .start_emulating(1, last_serial);

        context.flush()?;

        let (_, ctrl_keycode, c_keycode, escape_keycode) =
            keymap.ok_or("CVB: EIS keyboard keymap is not available")?;

        Ok(Self {
            context,
            device,
            keyboard,
            last_serial,
            ctrl_keycode,
            c_keycode,
            escape_keycode,
        })
    }

    pub fn inject_ctrl_c(&self) -> Result<(), Box<dyn std::error::Error>> {
        let device = self
            .device
            .as_ref()
            .ok_or("CVB: EIS device is not available")?;

        let keyboard = self
            .keyboard
            .as_ref()
            .ok_or("CVB: keyboard interface is not available")?;

        let timestamp = || {
            let time = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)?;
            Ok::<u64, nix::Error>(
                time.tv_sec() as u64 * 1_000_000 + time.tv_nsec() as u64 / 1_000,
            )
        };

        keyboard.key(self.ctrl_keycode, ei::keyboard::KeyState::Press);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        keyboard.key(self.c_keycode, ei::keyboard::KeyState::Press);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        keyboard.key(self.c_keycode, ei::keyboard::KeyState::Released);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        keyboard.key(self.ctrl_keycode, ei::keyboard::KeyState::Released);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        Ok(())
    }

    pub fn inject_escape(&self) -> Result<(), Box<dyn std::error::Error>> {
        let device = self
            .device
            .as_ref()
            .ok_or("CVB: EIS device is not available")?;

        let keyboard = self
            .keyboard
            .as_ref()
            .ok_or("CVB: keyboard interface is not available")?;

        let timestamp = || {
            let time = nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)?;
            Ok::<u64, nix::Error>(
                time.tv_sec() as u64 * 1_000_000 + time.tv_nsec() as u64 / 1_000,
            )
        };

        keyboard.key(self.escape_keycode, ei::keyboard::KeyState::Press);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        keyboard.key(self.escape_keycode, ei::keyboard::KeyState::Released);
        device.frame(self.last_serial, timestamp()?);
        self.context.flush()?;

        Ok(())
    }
}

fn find_keycode(keymap: &xkb::Keymap, keysym: xkb::Keysym) -> Option<u32> {
    let min = keymap.min_keycode().raw();
    let max = keymap.max_keycode().raw();

    for keycode in min..=max {
        let keycode = xkb::Keycode::new(keycode);

        for level in 0..=1 {
            if keymap
                .key_get_syms_by_level(keycode, 0, level)
                .contains(&keysym)
            {
                return Some(keycode.raw() - 8);
            }
        }
    }

    None
}
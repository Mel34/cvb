use std::os::fd::{AsFd, BorrowedFd};

use wayland_client::backend::ReadEventsGuard;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_registry,
    wl_seat::{self, WlSeat},
};
use wayland_client::{event_created_child, Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::{self, ExtDataControlManagerV1},
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
};

struct State {
    selection_changed: bool,
}

pub struct ClipboardWatcher {
    connection: Connection,
    event_queue: wayland_client::EventQueue<State>,
    state: State,
    device: ExtDataControlDeviceV1,
}

impl ClipboardWatcher {
    pub fn new() -> Result<Self, String> {
        let connection = Connection::connect_to_env()
            .map_err(|error| format!("CVB: unable to connect to Wayland: {error}"))?;

        let (globals, mut event_queue) = registry_queue_init::<State>(&connection)
            .map_err(|error| format!("CVB: unable to initialize Wayland registry: {error}"))?;

        let qh = event_queue.handle();

        let seat = globals
            .bind::<WlSeat, _, _>(&qh, 1..=9, ())
            .map_err(|error| format!("CVB: unable to bind Wayland seat: {error}"))?;

        let manager = globals
            .bind::<ExtDataControlManagerV1, _, _>(&qh, 1..=1, ())
            .map_err(|error| {
                format!(
                    "CVB: compositor does not provide ext-data-control-v1: {error}"
                )
            })?;

        let device = manager.get_data_device(&seat, &qh, ());

        let mut state = State {
            selection_changed: false,
        };

        event_queue
            .roundtrip(&mut state)
            .map_err(|error| format!("CVB: unable to initialize clipboard watcher: {error}"))?;

        state.selection_changed = false;

        Ok(Self {
            connection,
            event_queue,
            state,
            device,
        })
    }

    pub fn clear_selection_event(&mut self) {
        self.state.selection_changed = false;
    }

    pub fn selection_changed(&self) -> bool {
        self.state.selection_changed
    }

    pub fn dispatch_pending(&mut self) -> Result<(), String> {
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("CVB: clipboard event dispatch failed: {error}"))?;

        self.connection
            .flush()
            .map_err(|error| format!("CVB: unable to flush Wayland connection: {error}"))?;

        Ok(())
    }

    pub fn prepare_read(&self) -> Option<ReadEventsGuard> {
        self.event_queue.prepare_read()
    }

    pub fn read_events(
        &mut self,
        guard: ReadEventsGuard,
    ) -> Result<(), String> {
        guard
            .read()
            .map_err(|error| format!("CVB: unable to read Wayland events: {error}"))?;

        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("CVB: clipboard event dispatch failed: {error}"))?;

        Ok(())
    }

    pub fn flush(&self) -> Result<(), String> {
        self.event_queue
            .flush()
            .map_err(|error| format!("CVB: unable to flush Wayland connection: {error}"))
    }

    pub fn wayland_fd(&self) -> BorrowedFd<'_> {
        self.event_queue.as_fd()
    }

    pub fn wait_for_selection(&mut self) -> Result<(), String> {
        self.state.selection_changed = false;

        loop {
            self.event_queue
                .blocking_dispatch(&mut self.state)
                .map_err(|error| {
                    format!("CVB: clipboard event dispatch failed: {error}")
                })?;

            if self.state.selection_changed {
                return Ok(());
            }
        }
    }

    pub fn device(&self) -> &ExtDataControlDeviceV1 {
        &self.device
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _seat: &WlSeat,
        _event: wl_seat::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _manager: &ExtDataControlManagerV1,
        _event: ext_data_control_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _device: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_data_control_device_v1::Event::Selection { id: Some(_) } = event {
            state.selection_changed = true;
        }
    }

    event_created_child!(State, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE =>
            (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(
        _state: &mut Self,
        _offer: &ExtDataControlOfferV1,
        _event: ext_data_control_offer_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
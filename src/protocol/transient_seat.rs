//! ext-transient-seat-v1: a privileged client (wayvnc, remote desktop tools) asks for a seat of
//! its own, to hang virtual keyboards and pointers on.
//!
//! jff has one seat, and its virtual keyboard and pointer protocols ignore the seat they are
//! given, so what a transient seat gives the client is a `wl_seat` to pass along: a real global
//! with no capabilities, removed again when the client destroys the transient seat (or goes
//! away). Input made through it lands on jff's one seat, like all other synthetic input.
//!
//! The protocol's `ready` event carries the *registry name* of the new `wl_seat` global, and
//! neither Smithay nor wayland-backend tells a compositor what name a global got. So jff asks
//! itself: the first transient seat starts a small client of its own on a socket pair. It
//! listens to the registry, binds every `wl_seat` it is told about and reads the seat's
//! `name` event. A transient seat is called `transient-N`, so the name event says which
//! request the registry name belongs to, however the events interleave.
//!
//! The global is hidden from sandboxed clients, see `JffState::new`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use smithay::input::Seat;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode, PostAction};
use smithay::reexports::wayland_protocols::ext::transient_seat::v1::server::{
    ext_transient_seat_manager_v1::{self, ExtTransientSeatManagerV1},
    ext_transient_seat_v1::{self, ExtTransientSeatV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, WlSeat as ClientSeat};
use wayland_client::{Connection, Dispatch as ClientDispatch, EventQueue, Proxy, QueueHandle};

use crate::state::JffState;

/// How many transient seats may exist at once. More are denied.
const MAX_TRANSIENT_SEATS: usize = 16;

pub struct TransientSeats {
    _global: GlobalId,
    /// Number of the next seat, for its name.
    next: u32,
    /// Seats that exist (granted, not destroyed yet).
    live: usize,
    /// Requests waiting for their registry name, by seat name.
    pending: HashMap<String, ExtTransientSeatV1>,
    /// The client that finds registry names. Started by the first request.
    probe: Option<Probe>,
}

impl TransientSeats {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, ExtTransientSeatManagerV1, ()>(1, ()),
            next: 0,
            live: 0,
            pending: HashMap::new(),
            probe: None,
        }
    }
}

/// jff's own client of itself.
struct Probe {
    connection: Connection,
    queue: EventQueue<ProbeState>,
    state: ProbeState,
}

#[derive(Default)]
struct ProbeState {
    /// (seat name, registry name) of every `wl_seat` that told its name since the last look.
    found: Vec<(String, u32)>,
}

impl ClientDispatch<WlRegistry, ()> for ProbeState {
    fn event(
        _state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        // The `name` event came with wl_seat version 2, `release` with 5.
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_seat" && version >= 2 {
                registry.bind::<ClientSeat, _, _>(name, version.min(5), queue, name);
            }
        }
    }
}

impl ClientDispatch<ClientSeat, u32> for ProbeState {
    fn event(
        state: &mut Self,
        seat: &ClientSeat,
        event: wl_seat::Event,
        registry_name: &u32,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Name { name } = event {
            state.found.push((name, *registry_name));
            if seat.version() >= 5 {
                seat.release();
            }
        }
    }
}

impl JffState {
    /// Start the client that finds registry names, unless it runs already. `false` when that
    /// didn't work.
    fn ensure_seat_probe(&mut self) -> bool {
        if self.transient_seats.probe.is_some() {
            return true;
        }
        match self.start_seat_probe() {
            Ok(probe) => {
                self.transient_seats.probe = Some(probe);
                true
            }
            Err(err) => {
                tracing::warn!("transient seats: could not start the registry probe: {err:#}");
                false
            }
        }
    }

    fn start_seat_probe(&mut self) -> anyhow::Result<Probe> {
        let (ours, theirs) = UnixStream::pair()?;
        // Both ends share one file description, so this also makes the probe's reads not block.
        theirs.set_nonblocking(true)?;
        let wake = theirs.try_clone()?;
        let connection = Connection::from_socket(theirs)?;
        let queue = connection.new_event_queue::<ProbeState>();
        connection.display().get_registry(&queue.handle(), ());
        connection.flush()?;
        self.insert_client(ours);
        self.loop_handle
            .insert_source(
                Generic::new(wake, Interest::READ, Mode::Level),
                |_, _, state: &mut JffState| Ok(state.run_seat_probe()),
            )
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        Ok(Probe {
            connection,
            queue,
            state: ProbeState::default(),
        })
    }

    /// The probe's socket has something: read it, and answer the requests it found names for.
    fn run_seat_probe(&mut self) -> PostAction {
        let Some(probe) = self.transient_seats.probe.as_mut() else {
            return PostAction::Remove;
        };
        let mut broken = None;
        if let Some(guard) = probe.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(err)) if err.kind() == ErrorKind::WouldBlock => {}
                Err(err) => broken = Some(err.to_string()),
            }
        }
        if broken.is_none() {
            if let Err(err) = probe.queue.dispatch_pending(&mut probe.state) {
                broken = Some(err.to_string());
            }
        }
        if broken.is_none() {
            if let Err(err) = probe.connection.flush() {
                broken = Some(err.to_string());
            }
        }
        let found = std::mem::take(&mut probe.state.found);

        for (seat_name, registry_name) in found {
            // Not one of ours (the main seat, or a request that is gone already).
            if let Some(resource) = self.transient_seats.pending.remove(&seat_name) {
                resource.ready(registry_name);
            }
        }

        if let Some(err) = broken {
            tracing::warn!("transient seats: registry probe stopped: {err}");
            self.transient_seats.probe = None;
            // Whoever waits for a name can't get one now.
            let waiting: Vec<_> = self.transient_seats.pending.drain().collect();
            for (_, resource) in waiting {
                self.deny_transient_seat(&resource);
            }
            return PostAction::Remove;
        }
        PostAction::Continue
    }

    /// Tell the client no, and take the seat that was made for it away again.
    fn deny_transient_seat(&mut self, resource: &ExtTransientSeatV1) {
        if let Some(data) = resource.data::<TransientSeatData>() {
            self.drop_transient_seat(data);
        }
        resource.denied();
    }

    fn drop_transient_seat(&mut self, data: &TransientSeatData) {
        self.transient_seats.pending.remove(&data.name);
        let Some(seat) = data.seat.lock().unwrap().take() else {
            return;
        };
        self.transient_seats.live = self.transient_seats.live.saturating_sub(1);
        self.revoke_hotkeys_on_seat(&seat);
        if let Some(global) = seat.global() {
            self.display_handle.remove_global::<JffState>(global);
        }
    }
}

/// What a transient seat object knows. `seat` is `None` for a denied one, and after it is gone.
pub struct TransientSeatData {
    name: String,
    seat: Mutex<Option<Seat<JffState>>>,
}

impl GlobalDispatch<ExtTransientSeatManagerV1, ()> for JffState {
    // A client that can type and click for the user can do anything the user can.
    fn can_view(client: Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtTransientSeatManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ExtTransientSeatManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &ExtTransientSeatManagerV1,
        request: ext_transient_seat_manager_v1::Request,
        _data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let ext_transient_seat_manager_v1::Request::Create { seat: id } = request else {
            return;
        };

        if state.transient_seats.live >= MAX_TRANSIENT_SEATS || !state.ensure_seat_probe() {
            let resource = data_init.init(
                id,
                TransientSeatData {
                    name: String::new(),
                    seat: Mutex::new(None),
                },
            );
            resource.denied();
            return;
        }

        let number = state.transient_seats.next;
        state.transient_seats.next += 1;
        let name = format!("transient-{number}");
        // No keyboard, pointer or touch: the seat only has to exist to be passed along.
        let seat = state.seat_state.new_wl_seat(display, name.clone());
        let resource = data_init.init(
            id,
            TransientSeatData {
                name: name.clone(),
                seat: Mutex::new(Some(seat)),
            },
        );
        state.transient_seats.live += 1;
        // `ready` goes out when the probe has seen the new global and read its name.
        state.transient_seats.pending.insert(name, resource);
    }
}

impl Dispatch<ExtTransientSeatV1, TransientSeatData> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ExtTransientSeatV1,
        _request: ext_transient_seat_v1::Request,
        _data: &TransientSeatData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // Only `destroy`, which `destroyed` below answers.
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        _resource: &ExtTransientSeatV1,
        data: &TransientSeatData,
    ) {
        state.drop_transient_seat(data);
    }
}

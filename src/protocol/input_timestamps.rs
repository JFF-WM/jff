//! zwp_input_timestamps_v1: a client asks for the hardware time of the events it gets on one
//! `wl_keyboard` / `wl_pointer` / `wl_touch`, in nanoseconds instead of the milliseconds the
//! core events carry. Games and input tools use it for latency and for sub-millisecond ordering.
//!
//! The protocol only promises that the `timestamp` event comes before the input event it
//! belongs to, so the compositor sends it right before it hands the event to the client.
//! `process_input_event` calls `stamp_keyboard` / `stamp_pointer` ahead of the dispatch for that.

use smithay::backend::input::InputTime;
use smithay::reexports::wayland_protocols::wp::input_timestamps::zv1::server::{
    zwp_input_timestamps_manager_v1::{self, ZwpInputTimestampsManagerV1},
    zwp_input_timestamps_v1::ZwpInputTimestampsV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_keyboard::WlKeyboard;
use smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::protocol::wl_touch::WlTouch;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::JffState;

/// Which kind of device a timestamp object was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Pointer,
    Touch,
}

/// The device resource a timestamp object is tied to.
enum Device {
    Keyboard(WlKeyboard),
    Pointer(WlPointer),
    Touch(WlTouch),
}

impl Device {
    fn kind(&self) -> DeviceKind {
        match self {
            Device::Keyboard(_) => DeviceKind::Keyboard,
            Device::Pointer(_) => DeviceKind::Pointer,
            Device::Touch(_) => DeviceKind::Touch,
        }
    }

    fn is_alive(&self) -> bool {
        match self {
            Device::Keyboard(d) => d.is_alive(),
            Device::Pointer(d) => d.is_alive(),
            Device::Touch(d) => d.is_alive(),
        }
    }

    fn client_id(&self) -> Option<ClientId> {
        match self {
            Device::Keyboard(d) => d.client(),
            Device::Pointer(d) => d.client(),
            Device::Touch(d) => d.client(),
        }
        .map(|c| c.id())
    }
}

struct Stamp {
    resource: ZwpInputTimestampsV1,
    device: Device,
}

pub struct InputTimestamps {
    _global: GlobalId,
    stamps: Vec<Stamp>,
}

impl InputTimestamps {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<JffState, ZwpInputTimestampsManagerV1, ()>(1, ()),
            stamps: Vec::new(),
        }
    }

    /// No client asked for timestamps: the input path can skip all of this.
    pub fn is_empty(&self) -> bool {
        self.stamps.is_empty()
    }

    fn add(&mut self, resource: ZwpInputTimestampsV1, device: Device) {
        self.stamps
            .retain(|s| s.resource.is_alive() && s.device.is_alive());
        self.stamps.push(Stamp { resource, device });
    }

    /// Send `nsec` (CLOCK_MONOTONIC) to every timestamp object of `kind` that belongs to the
    /// client owning `focus`: that client is the one about to get the input event.
    pub fn stamp(&mut self, kind: DeviceKind, focus: &WlSurface, nsec: u64) {
        let Some(client) = focus.client().map(|c| c.id()) else {
            return;
        };
        self.stamps
            .retain(|s| s.resource.is_alive() && s.device.is_alive());
        let (hi, lo, ns) = split_timestamp(nsec);
        for stamp in &self.stamps {
            if stamp.device.kind() == kind && stamp.device.client_id().as_ref() == Some(&client) {
                stamp.resource.timestamp(hi, lo, ns);
            }
        }
    }
}

/// `nsec` as the protocol's three words: seconds split into high and low 32 bits, then the
/// nanoseconds inside that second.
pub fn split_timestamp(nsec: u64) -> (u32, u32, u32) {
    let secs = nsec / 1_000_000_000;
    let rest = (nsec % 1_000_000_000) as u32;
    ((secs >> 32) as u32, secs as u32, rest)
}

/// The one place that reads an `InputTime` (a type from the smithay fork) as nanoseconds on
/// CLOCK_MONOTONIC, which is the clock libinput and `wp_presentation` use. If `InputTime`
/// spells it differently, this is the only line to change.
fn input_time_nsec(time: InputTime) -> u64 {
    time.micros().saturating_mul(1000)
}

impl JffState {
    /// Before a `wl_keyboard` event: tell the keyboard focus client when it happened.
    pub(crate) fn stamp_keyboard(&mut self, time: InputTime) {
        if self.input_timestamps.is_empty() {
            return;
        }
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        self.input_timestamps
            .stamp(DeviceKind::Keyboard, &focus, input_time_nsec(time));
    }

    /// Before a `wl_pointer` event: tell the pointer focus client when it happened.
    pub(crate) fn stamp_pointer(&mut self, time: InputTime) {
        if self.input_timestamps.is_empty() {
            return;
        }
        let Some(focus) = self.seat.get_pointer().and_then(|p| p.current_focus()) else {
            return;
        };
        self.input_timestamps
            .stamp(DeviceKind::Pointer, &focus, input_time_nsec(time));
    }
}

impl GlobalDispatch<ZwpInputTimestampsManagerV1, ()> for JffState {
    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputTimestampsManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpInputTimestampsManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &ZwpInputTimestampsManagerV1,
        request: zwp_input_timestamps_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        use zwp_input_timestamps_manager_v1::Request;
        // An object is always created, even for a device jff never sends events on: the
        // protocol has no way to refuse (touch: jff has no touch support yet).
        let (id, device) = match request {
            Request::GetKeyboardTimestamps { id, keyboard } => (id, Device::Keyboard(keyboard)),
            Request::GetPointerTimestamps { id, pointer } => (id, Device::Pointer(pointer)),
            Request::GetTouchTimestamps { id, touch } => (id, Device::Touch(touch)),
            Request::Destroy => return,
            _ => return,
        };
        let resource = data_init.init(id, ());
        state.input_timestamps.add(resource, device);
    }
}

impl Dispatch<ZwpInputTimestampsV1, ()> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        _resource: &ZwpInputTimestampsV1,
        _request: smithay::reexports::wayland_protocols::wp::input_timestamps::zv1::server::zwp_input_timestamps_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        // Only `destroy`. The dead resource is dropped from the list on the next `add` / `stamp`.
    }
}

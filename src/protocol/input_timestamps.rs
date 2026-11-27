//! zwp_input_timestamps_v1: a client asks for the hardware time of the events it gets on one
//! `wl_keyboard` / `wl_pointer` / `wl_touch`, in nanoseconds instead of the milliseconds the
//! core events carry. Games and input tools use it for latency and for sub-millisecond ordering.
//!
//! The protocol ties a `timestamp` event to the next input event of that device the client gets,
//! so a timestamp must only be sent for an event that really goes to the client. It is sent
//! where the event is handed over, not where it comes in: a key a binding keeps, a scroll the
//! zoom or the overview uses, a click that starts a window drag, or pointer motion that goes to
//! a compositor grab or only enters a surface (`enter` carries no time) would all leave a stray
//! timestamp behind, and the client would pair it with its next event. `stamp_keyboard`,
//! `stamp_pointer` and `stamp_pointer_motion` are called right before the event is delivered.

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

/// Which surface gets a `wl_pointer.motion` when the pointer moves, so the one a timestamp can be
/// sent to. `focus` is the pointer focus before the move, `target` the surface under the pointer
/// after it, `grabbed` whether any pointer grab is active.
///
/// Without a grab the focus follows the pointer: a move inside the focused surface is a
/// `motion`, one onto another surface is a `leave` and an `enter`, and neither carries a time
/// (a move out of every surface sends nothing). A grab that keeps the focus (a click, a
/// popup) forwards the motion to that focus wherever the pointer is. A compositor grab (a
/// window move or resize, the overview, a selection) clears the focus and the client sees no
/// motion at all.
pub fn motion_stamp_target<'a, T: PartialEq>(
    grabbed: bool,
    focus: Option<&'a T>,
    target: Option<&'a T>,
) -> Option<&'a T> {
    let focus = focus?;
    if grabbed || target == Some(focus) {
        Some(focus)
    } else {
        None
    }
}

/// The one place that reads an `InputTime` (a type from the smithay fork) as nanoseconds on
/// CLOCK_MONOTONIC, which is the clock libinput and `wp_presentation` use. If `InputTime`
/// spells it differently, this is the only line to change.
fn input_time_nsec(time: InputTime) -> u64 {
    time.micros().saturating_mul(1000)
}

impl JffState {
    /// Right before a `wl_keyboard.key` goes to the keyboard focus: tell the client when the key
    /// happened. Does nothing unless a device event is being handled (`input_hw_time`), so a key
    /// a virtual keyboard or `jff msg` makes carries no timestamp.
    pub(crate) fn stamp_keyboard(&mut self) {
        let Some(time) = self.input_hw_time.take() else {
            return;
        };
        if self.input_timestamps.is_empty() {
            return;
        }
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        self.input_timestamps
            .stamp(DeviceKind::Keyboard, &focus, input_time_nsec(time));
    }

    /// Before a `wl_touch` event: tell the client the finger's events go to when it happened.
    /// `surface` is where the finger is sent, not the pointer focus (a touch has none).
    pub(crate) fn stamp_touch(&mut self, surface: &WlSurface, time: InputTime) {
        if self.input_timestamps.is_empty() {
            return;
        }
        self.input_timestamps
            .stamp(DeviceKind::Touch, surface, input_time_nsec(time));
    }

    /// Right before `pointer.motion`: tell the client that is about to get a `wl_pointer.motion`
    /// when the move happened. `target` is the surface under the pointer after the move. See
    /// `motion_stamp_target` for who really gets the motion. Does nothing unless a device
    /// event is being handled (`motion_hw_time`), so synthetic motion carries no timestamp.
    pub(crate) fn stamp_pointer_motion(&mut self, target: Option<&WlSurface>) {
        let Some(time) = self.motion_hw_time.take() else {
            return;
        };
        if self.input_timestamps.is_empty() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let focus = pointer.current_focus();
        if let Some(surface) = motion_stamp_target(pointer.is_grabbed(), focus.as_ref(), target) {
            self.input_timestamps
                .stamp(DeviceKind::Pointer, surface, input_time_nsec(time));
        }
    }

    /// Right before a `wl_pointer.button` or `wl_pointer.axis` goes to the pointer focus: tell
    /// the client when it happened. Does nothing unless a device event is being handled
    /// (`input_hw_time`). With a compositor grab the pointer has no focus, so nothing is sent.
    pub(crate) fn stamp_pointer(&mut self) {
        let Some(time) = self.input_hw_time.take() else {
            return;
        };
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
        // protocol has no way to refuse.
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

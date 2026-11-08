//! Pens and graphics tablets (zwp_tablet_manager_v2).
//!
//! libinput reports a stylus as tablet-tool events, never as pointer events, so without
//! this, a tablet just does nothing. Here we map them onto the monitor the pointer is on,
//! and send them to the surface under the pen as pressure, tilt, distance, buttons, etc.
//!
//! The seat pointer follows the pen but has no focus: that moves the cursor and drives the
//! compositor's own stuff (hot corners, window drags, overview, screenshot selection)
//! without clients getting both wl_pointer and tablet events for the same stroke.
//!
//! Not done yet: tablet pads (rings, strips, pad buttons), and pointer emulation for
//! clients that don't know zwp_tablet_v2.

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Device, DeviceCapability, Event, InputBackend, InputTime,
    ProximityState, TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent,
    TabletToolTipEvent, TabletToolTipState,
};
use smithay::input::pointer::MotionEvent as PointerMotionEvent;
use smithay::input::tablet::tool::{
    AxisFrame, ButtonEvent, DownEvent, MotionEvent, ProximityInEvent, ProximityOutEvent, UpEvent,
};
use smithay::input::tablet::{TabletDescriptor, TabletSeatTrait};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Size, SERIAL_COUNTER};

use super::BTN_LEFT;
use crate::state::JffState;

impl JffState {
    /// A tablet got plugged in, tell clients it's there.
    pub(crate) fn tablet_device_added<D: Device>(&mut self, device: &D) {
        if !device.has_capability(DeviceCapability::TabletTool) {
            return;
        }
        let desc = TabletDescriptor::from(device);
        self.seat.tablet_seat().add_tablet(&desc);
    }

    pub(crate) fn tablet_device_removed<D: Device>(&mut self, device: &D) {
        if !device.has_capability(DeviceCapability::TabletTool) {
            return;
        }
        let tablet_seat = self.seat.tablet_seat();
        tablet_seat.remove_tablet(&TabletDescriptor::from(device));
        // Tools don't belong to any particular tablet. Last tablet gone, drop them too.
        if tablet_seat.count_tablets() == 0 {
            tablet_seat.clear_tools();
        }
    }

    /// Where the pen is in shared space. The tablet covers whichever monitor the pointer is on.
    fn tablet_position<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        event: &E,
    ) -> Point<f64, Logical> {
        let r = self.rect_at_pointer();
        let size: Size<i32, Logical> = (r.2, r.3).into();
        let pos = event.position_transformed(size) + Point::from((r.0 as f64, r.1 as f64));
        crate::primitives::layout::clamp_to_rects((pos.x, pos.y), &self.pointer_bounds()).into()
    }

    /// Move the seat pointer to `pos` without giving any client pointer focus.
    fn tablet_move_pointer(&mut self, pos: Point<f64, Logical>, time: InputTime) {
        self.check_hot_corner(pos);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            None,
            &PointerMotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        self.update_toplevel_drag();
    }

    /// The surface under the pen, unless the overview or screenshot selection has the input.
    fn tablet_focus(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.overview_active() || self.screenshot.is_some() {
            return None;
        }
        self.pointer_focus_under(pos)
    }

    /// The axes of `event` that actually changed, as one frame (None if nothing did).
    fn tablet_axes<B: InputBackend, E: TabletToolEvent<B>>(event: &E) -> Option<AxisFrame> {
        let mut frame = AxisFrame::new();
        let mut any = false;
        if event.pressure_has_changed() {
            frame = frame.pressure(event.pressure());
            any = true;
        }
        if event.distance_has_changed() {
            frame = frame.distance(event.distance());
            any = true;
        }
        if event.tilt_has_changed() {
            let (x, y) = event.tilt();
            frame = frame.tilt(x, y);
            any = true;
        }
        if event.slider_has_changed() {
            frame = frame.slider(event.slider_position());
            any = true;
        }
        if event.rotation_has_changed() {
            frame = frame.rotation(event.rotation());
            any = true;
        }
        if event.wheel_has_changed() {
            frame = frame.wheel(event.wheel_delta(), event.wheel_delta_discrete());
            any = true;
        }
        any.then_some(frame)
    }

    pub(crate) fn tablet_tool_axis<B: InputBackend>(&mut self, event: &B::TabletToolAxisEvent) {
        let tablet_seat = self.seat.tablet_seat();
        let pos = self.tablet_position::<B, _>(event);
        let time = event.time();
        self.tablet_move_pointer(pos, time);

        let Some(tool) = tablet_seat.get_tool(&event.tool()) else {
            return;
        };
        let under = self.tablet_focus(pos);
        tool.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        if let Some(frame) = Self::tablet_axes::<B, _>(event) {
            tool.axis(self, frame);
        }
        tool.frame(self, time);
    }

    pub(crate) fn tablet_tool_proximity<B: InputBackend>(
        &mut self,
        event: &B::TabletToolProximityEvent,
    ) {
        let tablet_seat = self.seat.tablet_seat();
        let pos = self.tablet_position::<B, _>(event);
        let time = event.time();
        self.tablet_move_pointer(pos, time);

        // A tool gets announced to clients the first time it comes near.
        let tool = tablet_seat.add_tool(&event.tool());
        let Some(tablet) = tablet_seat.get_tablet(&TabletDescriptor::from(&event.device())) else {
            return;
        };
        match event.state() {
            ProximityState::In => {
                let under = self.tablet_focus(pos);
                tool.proximity_in(
                    self,
                    under,
                    tablet,
                    &ProximityInEvent {
                        location: pos,
                        axis: Self::tablet_axes::<B, _>(event),
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
            ProximityState::Out => tool.proximity_out(
                self,
                &ProximityOutEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            ),
        }
        tool.frame(self, time);
    }

    pub(crate) fn tablet_tool_tip<B: InputBackend>(&mut self, event: &B::TabletToolTipEvent) {
        let tablet_seat = self.seat.tablet_seat();
        let time = event.time();
        let state = match event.tip_state() {
            TabletToolTipState::Down => ButtonState::Pressed,
            TabletToolTipState::Up => ButtonState::Released,
        };
        // Touching the surface counts as a left click for the compositor: click-to-focus and
        // raise, window drags, the overview, screenshot selection. Those never reach the client.
        let modal = self.overview_active() || self.screenshot.is_some();
        self.pointer_button(BTN_LEFT, state, time);
        if modal {
            return;
        }
        let Some(tool) = tablet_seat.get_tool(&event.tool()) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        match event.tip_state() {
            TabletToolTipState::Down => tool.down(self, &DownEvent { serial, time }),
            TabletToolTipState::Up => tool.up(self, &UpEvent { serial, time }),
        }
        tool.frame(self, time);
    }

    pub(crate) fn tablet_tool_button<B: InputBackend>(&mut self, event: &B::TabletToolButtonEvent) {
        if self.overview_active() || self.screenshot.is_some() {
            return;
        }
        let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) else {
            return;
        };
        let time = event.time();
        tool.button(
            self,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                button: event.button(),
                state: event.button_state(),
                time,
            },
        );
        tool.frame(self, time);
    }
}

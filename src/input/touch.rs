//! Touchscreens (wl_touch).
//!
//! Every finger goes to the client under it as `wl_touch` events. The compositor's own
//! behaviour (click-to-focus and raise, the overview, screenshot selection, panels taking the
//! keyboard, Alt+drag) is written for a pointer, so the *primary* finger, the first one down,
//! also drives the seat pointer: it follows the finger and "presses" the left button for as
//! long as the finger is down. Like for the pen (`tablet.rs`), that pointer has no focus, so
//! clients get touch events and never wl_pointer events for the same finger.
//!
//! A client that answers a touch with `xdg_toplevel.move` / `resize` (a titlebar drag) gets
//! its window moved by the same pointer grabs a mouse drag uses. The press carries the serial
//! of the `wl_touch.down`, which is how the request is recognised. The client's touch
//! sequence is then cancelled, as the protocol describes, and the finger belongs to the grab.
//!
//! Not done yet: mapping a touchscreen to a chosen monitor (it covers the built-in panel, or
//! the first monitor), output rotation, compositor gestures (multi-finger swipes), pointer
//! emulation for clients that never bind `wl_touch`, and Alt+drag keeping its first press
//! from the client.

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event, InputBackend, InputTime, TouchEvent, TouchSlot,
};
use smithay::input::pointer::{GrabStartData, MotionEvent as PointerMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point, Serial, Size, SERIAL_COUNTER};

use super::BTN_LEFT;
use crate::primitives::layout;
use crate::primitives::touch::{touch_output, Finger};
use crate::state::JffState;

impl JffState {
    /// Where a finger is in shared space. A touchscreen covers one monitor edge to edge.
    fn touch_position<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        event: &E,
    ) -> Option<Point<f64, Logical>> {
        let names: Vec<String> = self.outputs.iter().map(|output| output.name()).collect();
        let rects = self.output_rects();
        let rect = rects.get(touch_output(&names)?)?;
        let size: Size<i32, Logical> = (rect.2, rect.3).into();
        let pos = event.position_transformed(size) + Point::from((rect.0 as f64, rect.1 as f64));
        Some(layout::clamp_to_rects((pos.x, pos.y), &self.pointer_bounds()).into())
    }

    /// Move the seat pointer to the primary finger, without giving any client pointer focus.
    /// No hot corners: the corner of a touchscreen is where the close button of a maximized
    /// window is.
    fn touch_move_pointer(&mut self, pos: Point<f64, Logical>, time: InputTime) {
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

    /// The overview or the screenshot selection has the input: clients don't get the touch.
    fn touch_modal(&self) -> bool {
        self.overview_active() || self.screenshot.is_some()
    }

    /// `pos` is on a surface of the client whose menu holds the pointer. A tap there belongs
    /// to the menu: as a pointer press with no pointer focus it would read as a click outside
    /// the menu and close it before the menu saw the touch.
    fn touch_in_popup_grab(&self, pos: Point<f64, Logical>) -> bool {
        let Some(grab) = self.popup_grab.as_ref() else {
            return false;
        };
        self.pointer_focus_under(pos)
            .is_some_and(|(surface, _)| surface.id().same_client_as(&grab.root.id()))
    }

    pub(crate) fn touch_down<B: InputBackend>(&mut self, event: &B::TouchDownEvent) {
        let Some(pos) = self.touch_position::<B, _>(event) else {
            return;
        };
        let (slot, time) = (event.slot(), event.time());
        // One serial for the pointer press and the client's `touch.down`.
        let serial = SERIAL_COUNTER.next_serial();
        let modal = self.touch_modal();

        // A finger whose lift never came (a lost event): let it go first, so nothing stays
        // pressed.
        if let Some(stale) = self.fingers.up(slot) {
            self.touch_settle(slot, stale, time);
        }
        let primary = self.fingers.down(slot);
        if primary {
            self.touch_move_pointer(pos, time);
            // The overview and the screenshot selection act on the press themselves. An open
            // menu keeps a tap on itself to the menu alone (see `touch_in_popup_grab`).
            if modal || !self.touch_in_popup_grab(pos) {
                self.pointer_button_with(BTN_LEFT, ButtonState::Pressed, time, serial);
                if let Some(finger) = self.fingers.get_mut(slot) {
                    finger.pressed = true;
                }
            }
        }
        if modal {
            return;
        }

        // Asked after the press: it may have raised a window or closed a menu.
        let under = self.pointer_focus_under(pos);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        if let Some(finger) = self.fingers.get_mut(slot) {
            finger.forwarded = true;
            finger.target = under.as_ref().map(|(surface, _)| surface.clone());
        }
        if let Some((surface, _)) = &under {
            self.stamp_touch(surface, time);
        }
        touch.down(
            self,
            under,
            &DownEvent {
                slot,
                location: pos,
                serial,
                time,
            },
        );
    }

    pub(crate) fn touch_motion<B: InputBackend>(&mut self, event: &B::TouchMotionEvent) {
        let (slot, time) = (event.slot(), event.time());
        let Some(finger) = self.fingers.get(slot) else {
            return;
        };
        let (forwarded, target) = (finger.forwarded, finger.target.clone());
        let Some(pos) = self.touch_position::<B, _>(event) else {
            return;
        };
        if self.fingers.is_primary(slot) {
            self.touch_move_pointer(pos, time);
        }
        if !forwarded {
            return;
        }
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        if let Some(surface) = &target {
            self.stamp_touch(surface, time);
        }
        let under = self.pointer_focus_under(pos);
        touch.motion(
            self,
            under,
            &MotionEvent {
                slot,
                location: pos,
                time,
            },
        );
    }

    pub(crate) fn touch_up<B: InputBackend>(&mut self, event: &B::TouchUpEvent) {
        let (slot, time) = (event.slot(), event.time());
        let Some(finger) = self.fingers.up(slot) else {
            return;
        };
        self.touch_settle(slot, finger, time);
    }

    /// Give back what a finger that is gone was owed: the client its `touch.up`, the pointer
    /// side the release of the press.
    fn touch_settle(
        &mut self,
        slot: TouchSlot,
        finger: Finger<TouchSlot, WlSurface>,
        time: InputTime,
    ) {
        if finger.forwarded {
            if let Some(touch) = self.seat.get_touch() {
                if let Some(surface) = &finger.target {
                    self.stamp_touch(surface, time);
                }
                touch.up(
                    self,
                    &UpEvent {
                        slot,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
        }
        if finger.pressed {
            self.pointer_button_with(
                BTN_LEFT,
                ButtonState::Released,
                time,
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    /// The end of a batch of finger events that belong together.
    pub(crate) fn touch_frame(&mut self) {
        if let Some(touch) = self.seat.get_touch() {
            touch.frame(self);
        }
    }

    /// The device dropped every touch (a palm, a suspend): clients get a cancel, and a press
    /// the compositor still holds for the primary finger is let go.
    pub(crate) fn touch_cancel(&mut self, time: InputTime) {
        let fingers = self.fingers.clear();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
        for finger in fingers {
            if finger.pressed {
                self.pointer_button_with(
                    BTN_LEFT,
                    ButtonState::Released,
                    time,
                    SERIAL_COUNTER.next_serial(),
                );
            }
        }
    }

    /// The pointer grab a touch counts as, for a client's `xdg_toplevel.move` / `resize` that
    /// answers a touch of its own surface: the press the client reacts to has `serial` as
    /// its `wl_touch.down`. Start data like the other grabs the compositor starts itself
    /// (no focus: the client must not see the drag).
    pub(crate) fn touch_interactive_start(
        &self,
        client_surface: &WlSurface,
        serial: Serial,
    ) -> Option<GrabStartData<JffState>> {
        let touch = self.seat.get_touch()?;
        if !touch.has_grab(serial) {
            return None;
        }
        let start = touch.grab_start_data()?;
        let same_client = start
            .focus
            .as_ref()
            .is_some_and(|(focus, _)| focus.id().same_client_as(&client_surface.id()));
        same_client.then(|| GrabStartData {
            focus: None,
            button: BTN_LEFT,
            location: start.location,
        })
    }

    /// A move or resize started from the touch `serial` belongs to: from now on the finger
    /// drives a compositor grab, and the client's sequence is cancelled (`wl_touch.cancel`).
    /// Does nothing for a request that answers a mouse press.
    pub(crate) fn touch_taken_by_grab(&mut self, serial: Serial) {
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        if !touch.has_grab(serial) {
            return;
        }
        self.fingers.detach_all();
        touch.cancel(self);
    }
}

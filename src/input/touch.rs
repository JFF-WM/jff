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
//! A touchscreen covers the monitor `input { touchscreen { map-to-output … } }` names for it,
//! else the built-in panel, else the first monitor. A turned or scaled monitor is handled by
//! `absolute_on_output`, which undoes the turn on the position the panel reports.
//!
//! Three fingers down are the compositor's own gesture (`primitives::touch::TouchGesture`),
//! like the three-finger touchpad swipe: sideways switches workspace, up opens the overview,
//! down closes it. When the third finger lands, the client gets `wl_touch.cancel`, a press
//! the first finger made on the pointer side is let go, and every finger belongs to the
//! gesture until all are up. That also means a client never sees a three-finger pinch.
//!
//! A client that never bound `wl_touch` would be deaf to all of this, so the primary finger
//! over such a client is passed on as `wl_pointer` events: the pointer gets focus on the
//! client's surface, follows the finger and presses the left button. Other fingers are not
//! emulated, and the pointer leaves the client when the finger lifts.
//!
//! Not done yet: Alt+drag keeping its first press from the client.

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Device, Event, InputBackend, InputTime, TouchEvent,
    TouchSlot,
};
use smithay::input::pointer::{GrabStartData, MotionEvent as PointerMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent, UpEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Point, Serial, SERIAL_COUNTER};

use super::BTN_LEFT;
use crate::primitives::layout;
use crate::primitives::touch::{touch_output_mapped, Finger};
use crate::state::JffState;

impl JffState {
    /// Where a finger is in shared space. A touchscreen covers one monitor edge to edge.
    fn touch_position<B: InputBackend, E: AbsolutePositionEvent<B>>(
        &self,
        event: &E,
    ) -> Option<Point<f64, Logical>> {
        let names: Vec<String> = self.outputs.iter().map(|output| output.name()).collect();
        let rects = self.output_rects();
        let wanted = self.config.input().touch_output_for(&event.device().name());
        let index = touch_output_mapped(&names, wanted)?;
        let rect = rects.get(index)?;
        let pos = self.absolute_on_output::<B, _>(self.outputs.get(index)?, event)
            + Point::from((rect.0 as f64, rect.1 as f64));
        Some(layout::clamp_to_rects((pos.x, pos.y), &self.pointer_bounds()).into())
    }

    /// Move the seat pointer to the primary finger, without giving any client pointer focus.
    /// No hot corners: the corner of a touchscreen is where the close button of a maximized
    /// window is.
    ///
    /// `focus` is `None` except for pointer emulation, where the client under the finger is
    /// the pointer's focus.
    fn touch_move_pointer(
        &mut self,
        pos: Point<f64, Logical>,
        time: InputTime,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            focus,
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

    /// Whether the client that owns `surface` has a `wl_touch` object. Asked of the protocol
    /// objects themselves, as nothing else says whether a client bound one. A client that
    /// can't be looked at counts as having one: it gets touches as before.
    fn client_has_touch(&self, surface: &WlSurface) -> bool {
        let Some(client) = surface.client() else {
            return true;
        };
        let mut found = false;
        let walked =
            self.display_handle
                .backend_handle()
                .with_all_objects_for(client.id(), |object| {
                    if object.interface().name == "wl_touch" {
                        found = true;
                    }
                });
        walked.is_err() || found
    }

    /// The compositor takes every finger down for its own gesture: the client's touch is
    /// cancelled, and presses held on the pointer side are let go.
    fn touch_begin_gesture(&mut self, time: InputTime) {
        let owed = self.fingers.settle_all();
        if owed.iter().any(|finger| finger.forwarded) {
            if let Some(touch) = self.seat.get_touch() {
                touch.cancel(self);
            }
        }
        for finger in owed {
            self.touch_release_pointer(&finger, time);
        }
    }

    /// Let go of what the pointer side of `finger` holds: its press, and the focus a pointer
    /// emulation gave a client.
    fn touch_release_pointer(&mut self, finger: &Finger<TouchSlot, WlSurface>, time: InputTime) {
        if finger.pressed {
            self.pointer_button_with(
                BTN_LEFT,
                ButtonState::Released,
                time,
                SERIAL_COUNTER.next_serial(),
            );
        }
        if finger.emulated {
            self.touch_move_pointer(Point::from(finger.at), time, None);
        }
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
        if let Some(finger) = self.fingers.get_mut(slot) {
            finger.at = (pos.x, pos.y);
        }

        // Three fingers are the compositor's gesture. Once it is going, fingers that join
        // it belong to it too.
        if self.touch_gesture.is_active() {
            self.touch_gesture.add(slot, (pos.x, pos.y));
            return;
        }
        let allowed = !self.session_locked() && self.screenshot.is_none();
        if self.touch_gesture.begin(self.fingers.positions(), allowed) {
            self.touch_begin_gesture(time);
            return;
        }

        // A client with no `wl_touch` gets the primary finger as a pointer.
        let emulated = if primary && !modal {
            self.pointer_focus_under(pos)
                .filter(|(surface, _)| !self.client_has_touch(surface))
        } else {
            None
        };
        if primary {
            self.touch_move_pointer(pos, time, emulated.clone());
            // The overview and the screenshot selection act on the press themselves. An open
            // menu keeps a tap on itself to the menu alone (see `touch_in_popup_grab`),
            // unless the client is getting a pointer, which sees the tap as it is.
            if modal || emulated.is_some() || !self.touch_in_popup_grab(pos) {
                self.pointer_button_with(BTN_LEFT, ButtonState::Pressed, time, serial);
                if let Some(finger) = self.fingers.get_mut(slot) {
                    finger.pressed = true;
                }
            }
        }
        if emulated.is_some() {
            if let Some(finger) = self.fingers.get_mut(slot) {
                finger.emulated = true;
            }
            return;
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
        self.input_serials
            .note(serial, under.as_ref().map(|(surface, _)| surface));
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
        let (forwarded, target, emulated) =
            (finger.forwarded, finger.target.clone(), finger.emulated);
        let Some(pos) = self.touch_position::<B, _>(event) else {
            return;
        };
        if let Some(finger) = self.fingers.get_mut(slot) {
            finger.at = (pos.x, pos.y);
        }
        if self.touch_gesture.is_active() {
            if let Some(swipe) = self.touch_gesture.moved(slot, (pos.x, pos.y)) {
                self.run_swipe(swipe);
            }
            return;
        }
        if self.fingers.is_primary(slot) {
            let focus = if emulated {
                self.pointer_focus_under(pos)
            } else {
                None
            };
            self.touch_move_pointer(pos, time, focus);
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
        if self.touch_gesture.is_active() {
            self.touch_gesture.lift(slot);
            if self.fingers.is_empty() {
                self.touch_gesture.end();
            }
        }
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
                let serial = SERIAL_COUNTER.next_serial();
                self.input_serials.note(serial, finger.target.as_ref());
                touch.up(self, &UpEvent { slot, serial, time });
            }
        }
        self.touch_release_pointer(&finger, time);
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
        self.touch_gesture.end();
        let fingers = self.fingers.clear();
        if let Some(touch) = self.seat.get_touch() {
            touch.cancel(self);
        }
        for finger in fingers {
            self.touch_release_pointer(&finger, time);
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

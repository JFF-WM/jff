//! zwp-virtual-keyboard-v1: lets a client (wtype, wayvnc, input method helpers) type for the
//! user. Smithay turns each request into an `InputEvent` and hands it over here, so synthetic
//! keys take the same road as a real keyboard: key bindings, the overview and screenshot key
//! handling, and idle detection (see `keyboard_key` in `input/mod.rs`).
//!
//! A virtual keyboard's keycodes are numbers in its own keymap (wtype builds one for the text
//! it types), so that keymap is put on the seat before its keys are processed. Clients see a
//! new keymap, and the configured one comes back when the virtual keyboard goes away or when a
//! real key is pressed ([`JffState::restore_seat_keymap`]).
//!
//! The global is hidden from sandboxed clients, see `JffState::new`.

use std::sync::Arc;

use smithay::backend::input::{Event, InputEvent, KeyboardKeyEvent};
use smithay::wayland::virtual_keyboard::{VirtualKeyboardBackend, VirtualKeyboardHandler};

use crate::state::JffState;

impl VirtualKeyboardHandler for JffState {
    fn process_virtual_keyboard_event(&mut self, event: InputEvent<VirtualKeyboardBackend>) {
        match event {
            InputEvent::Keyboard { event } => {
                // Idle tools count this as input, like a real keyboard.
                self.idle_notifier_state.notify_activity(&self.seat);
                let device = Event::device(&event);
                self.activate_virtual_keymap(device.keymap());
                self.keyboard_key(
                    device.source(),
                    event.key_code(),
                    event.state(),
                    Event::time(&event),
                );
            }
            // The keyboard is gone, so is its keymap.
            InputEvent::DeviceRemoved { device } => {
                self.restore_seat_keymap();
                self.process_input_event::<VirtualKeyboardBackend>(InputEvent::DeviceRemoved {
                    device,
                });
            }
            other => self.process_input_event(other),
        }
    }
}

impl JffState {
    /// Make the seat read keycodes with `keymap`, a virtual keyboard's own. `None` is a keyboard
    /// that uses the seat's keymap, so the configured one is put back.
    fn activate_virtual_keymap(&mut self, keymap: Option<Arc<str>>) {
        let Some(keymap) = keymap else {
            self.restore_seat_keymap();
            return;
        };
        if self.active_virtual_keymap.as_deref() == Some(&*keymap) {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        match keyboard.set_keymap_from_string(self, keymap.to_string()) {
            Ok(()) => self.active_virtual_keymap = Some(keymap),
            Err(err) => tracing::warn!("virtual keyboard keymap not applied: {err:?}"),
        }
    }

    /// Put the `keyboard` settings of the config back on the seat after a virtual keyboard had
    /// its own keymap there. Does nothing when none did.
    pub(crate) fn restore_seat_keymap(&mut self) {
        if self.active_virtual_keymap.take().is_none() {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let config = self.config.keyboard().clone();
        if let Err(err) = keyboard.set_xkb_config(self, config.xkb_config()) {
            tracing::warn!("keyboard layout not restored: {err:?}");
        }
        // A new keymap starts with every lock off.
        self.apply_numlock();
    }
}

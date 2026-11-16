//! The scale of the output a frame is being built for, for code deep in the call tree.
//!
//! Elements are placed in logical coordinates and the output's damage tracker scales them, but
//! a few things are measured in frame pixels (a corner radius, a border width). They read the
//! scale from here instead of every builder passing it down. Outside a frame it is 1.

use std::cell::Cell;

thread_local! {
    static SCALE: Cell<f64> = const { Cell::new(1.0) };
}

/// The scale of the frame being built, 1 outside one.
pub fn get() -> f64 {
    SCALE.with(Cell::get)
}

/// A length in logical pixels as frame pixels.
pub fn px(value: i32) -> i32 {
    (f64::from(value) * get()).round() as i32
}

/// Puts the previous scale back when dropped.
pub struct Scope(f64);

impl Drop for Scope {
    fn drop(&mut self) {
        SCALE.with(|scale| scale.set(self.0));
    }
}

/// Build for an output of `scale` until the scope is dropped.
pub fn enter(scale: f64) -> Scope {
    Scope(SCALE.with(|cell| cell.replace(scale)))
}

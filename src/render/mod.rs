//! How a frame becomes pixels.
//!
//! One path: a fullscreen window that covers the monitor goes straight to the primary plane,
//! anything else is composited, with blur and window animations when the config turns them on.

pub mod cursor;
pub mod effects;
pub mod elements;

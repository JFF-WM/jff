//! How a frame becomes pixels.
//!
//! Today there is exactly one path: the fast one (no blur, no fades, no slides, straight to the
//! primary plane). `path/` is where the `NoAnimations` / `Animations` split lands, see `path/mod.rs`.

pub mod cursor;
pub mod effects;
pub mod elements;
pub mod path;

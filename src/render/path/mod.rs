//! Render paths, chosen once at startup.
//!
//! * `NoAnimations` (default): the fast path. No blur, no fade, no slide; every frame goes straight
//!   to the primary plane via `DrmCompositor`. This is what the 39-cube benchmark measures. A
//!   feature that touches this path is rejected.
//! * `Animations` (opt-in, `animations true` or a keybind): blur, open/close fades, workspace
//!   slide. Measured separately.
//!
//! The split is two implementations of one trait held as a function pointer, not a runtime `if`
//! in the fast path. Rule: gaming performance is never traded for a visual feature.
//!
//! STATUS: not wired up yet. All current drawing code (in `app/tty`, `app/winit`,
//! `feature/overview_mode`, `protocol/layer_shell`) is the fast path and moves into
//! `no_animations.rs` in the next step; `animations.rs` starts empty.

pub mod animations;
pub mod no_animations;

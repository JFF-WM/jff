//! Background blur: the compositor blurs whatever sits behind a surface that asked for it.
//!
//! * `blur`: the dual Kawase blur (texture chain + two small GL programs).
//! * `framebuffer_effect`: a render element that copies the pixels already drawn behind it,
//!   blurs them, and paints them back.
//! * `background_effect`: per-surface state that decides where the element goes.
//! * `border`: a rounded outline around windows (the `border` block), with a colour fade when
//!   focus moves. Also opt-in.
//! * `rounded`: rounded window corners (`radius` setting) - a texture shader masking the
//!   window's surface. Also opt-in: nothing runs with `radius 0`.
//!
//! * `yuv`: which surfaces need their YUV colors fixed (`color-representation { yuv }`), applied
//!   by the same shader as `rounded`.
//!
//! Blur itself is opt-in: nothing runs unless the config has a `blur { … }` block - the
//! renderer hands these functions `None` otherwise (see `render_for_surface`).
//!
//! * `surface_alpha`: the opacity a client set with wp_alpha_modifier_v1. Blur and border fade
//!   with it, and a faded window is never scanned out directly.
//!
//! Needs Smithay's framebuffer-effect support (`RenderElement::capture_framebuffer`).

pub mod background_effect;
pub mod blur;
pub mod border;
pub mod framebuffer_effect;
pub mod rounded;
pub mod surface_alpha;
pub mod yuv;

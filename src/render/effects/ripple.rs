//! The ripples at the top-left corner of a monitor when the overview opens, like the ones
//! GNOME Shell draws when its hot corner goes off.
//!
//! * [`ripple_element`]: one frame of the ripples, a square at the monitor's corner drawn
//!   through a small pixel shader (`shaders/ripple.frag`) that works out the three discs from
//!   the time since the overview opened.
//!
//! Draw it in front of everything on the monitor. The element only lives for the length of the
//! animation, see [`DURATION`]: nothing is drawn, and nothing is compiled, before the first
//! overview opens.

use std::time::Duration;

use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType,
};
use smithay::utils::{Logical, Rectangle};

const RIPPLE_SHADER: &str = include_str!("shaders/ripple.frag");

/// How long the ripples take from the first one starting to the last one fading out. The
/// shader's own timings end at this.
pub const DURATION: Duration = Duration::from_millis(1350);

/// Side (logical px) of the square the ripples are drawn in. The biggest ripple reaches 72 px
/// from the corner.
const REACH: i32 = 96;

/// Radius (logical px) of a ripple at scale 1.0.
const BASE_RADIUS: i32 = 48;

/// EGL-context user data for the ripples: the program, or `None` if compiling failed.
struct CachedRippleProgram(Option<GlesPixelProgram>);

/// Program of this renderer's GL context, compiled on first use. `None` if the driver rejected
/// it - there are then no ripples.
fn ripple_program(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    if let Some(cached) = renderer
        .egl_context()
        .user_data()
        .get::<CachedRippleProgram>()
    {
        return cached.0.clone();
    }
    let compiled = renderer
        .compile_custom_pixel_shader(
            RIPPLE_SHADER,
            &[
                UniformName::new("time", UniformType::_1f),
                UniformName::new("base", UniformType::_1f),
                UniformName::new("color", UniformType::_4f),
            ],
        )
        .map_err(|err| tracing::warn!("error compiling the ripple shader: {err:#}"))
        .ok();
    let stored = compiled.clone();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CachedRippleProgram(stored));
    compiled
}

/// The ripples `elapsed` after the overview opened, in `color` (straight alpha), at the top-left
/// corner of the monitor. Positions are relative to the monitor, like the rest of the overview's
/// elements.
///
/// `None` if the shader is unavailable, or the ripples are over.
pub fn ripple_element(
    renderer: &mut GlesRenderer,
    elapsed: Duration,
    color: [f32; 4],
) -> Option<PixelShaderElement> {
    if elapsed >= DURATION {
        return None;
    }
    let program = ripple_program(renderer)?;
    // the uniforms are in frame pixels, the area in logical ones
    let scale = super::frame_scale::get() as f32;
    // colors are sRGB in the config, an HDR output wants them in its working space
    let color = super::gamut::tint(color);
    let area = Rectangle::<i32, Logical>::new((0, 0).into(), (REACH, REACH).into());
    Some(PixelShaderElement::new(
        program,
        area,
        None,
        1.0,
        vec![
            Uniform::new("time", elapsed.as_secs_f32() * 1000.0),
            Uniform::new("base", BASE_RADIUS as f32 * scale),
            Uniform::new("color", color),
        ],
        Kind::Unspecified,
    ))
}

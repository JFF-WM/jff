//! Rounded window corners.
//!
//! A window is drawn through a small texture shader that fades its alpha to zero outside a
//! rounded rectangle (the window's geometry). The shader is swapped in for the duration of the
//! window's own `draw`, so the surface is drawn exactly as before, just through the mask.
//!
//! * [`Rounding`]: what one window needs (compiled program, geometry, radius).
//! * [`RoundedElement`]: any window element (a surface, or an overview thumbnail made of one)
//!   that draws through the shader.
//! * [`ring_element`]: the outline around the selected thumbnail in the overview - a rounded
//!   ring that follows the thumbnail's corners.
//!
//! Opt-in: with `radius 0` (the default) or for a fullscreen window nothing here is used, so
//! the plain path and direct scanout stay intact. A rounded surface is never scanned out
//! directly, since the shader has to run on it. The same goes for a surface that needs its
//! colors fixed or converted: only those are drawn through the shader.
//!
//! The mask works in framebuffer pixels (`gl_FragCoord`), not texture coordinates, so it
//! doesn't care how the client buffer is cropped, scaled or damaged. That's also why the
//! overview works: a thumbnail is the same element shrunk and moved, and the mask only needs
//! its rectangle on screen and a radius shrunk with it.

use std::cell::RefCell;
use std::collections::HashMap;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{RelocateRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName,
    UniformType,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::backend::renderer::Frame;
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};

use crate::primitives::gamut::Conversion;
use crate::primitives::yuv::Correction;
use crate::render::elements::JffElement;

const SHADER: &str = include_str!("shaders/rounded.frag");
const RING_SHADER: &str = include_str!("shaders/ring.frag");

/// EGL-context user data: the program, or `None` if compiling failed (so a broken driver isn't
/// asked again every frame).
struct CachedProgram(Option<GlesTexProgram>);

/// Program of this renderer's GL context, compiled on first use. `None` if the driver rejected
/// it - windows are then drawn square.
fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(cached) = renderer.egl_context().user_data().get::<CachedProgram>() {
        return cached.0.clone();
    }
    let compiled = renderer
        .compile_custom_texture_shader(
            SHADER,
            &[
                UniformName::new("geo_min", UniformType::_2f),
                UniformName::new("geo_max", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("ycc_r", UniformType::_3f),
                UniformName::new("ycc_g", UniformType::_3f),
                UniformName::new("ycc_b", UniformType::_3f),
                UniformName::new("ycc_off", UniformType::_3f),
                UniformName::new("ycc_on", UniformType::_1f),
                UniformName::new("cm_r", UniformType::_3f),
                UniformName::new("cm_g", UniformType::_3f),
                UniformName::new("cm_b", UniformType::_3f),
                UniformName::new("cm_decode", UniformType::_2f),
                UniformName::new("cm_src_luma", UniformType::_3f),
                UniformName::new("cm_encode", UniformType::_2f),
                UniformName::new("cm_map", UniformType::_1f),
                UniformName::new("cm_luma", UniformType::_3f),
                UniformName::new("cm_scale", UniformType::_1f),
                UniformName::new("cm_working", UniformType::_1f),
                UniformName::new("cm_on", UniformType::_1f),
            ],
        )
        .map_err(|err| tracing::warn!("error compiling the rounded corner shader: {err:#}"))
        .ok();
    let stored = compiled.clone();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CachedProgram(stored));
    compiled
}

thread_local! {
    /// Element id of a rounded surface, keyed by the plain surface element's id and the radius
    /// it was made for. A new radius means a new id: Smithay's damage tracking then sees a new
    /// element and repaints the window, which it wouldn't for a change nothing in the surface
    /// itself announces. Rendering is single-threaded, so a per-thread table is enough.
    static IDS: RefCell<HashMap<(Id, bool), (i32, Id)>> = RefCell::new(HashMap::new());
}

/// An overview thumbnail and the live window share the id of the surface under them, so they
/// are told apart by `thumb`: each keeps its own radius and id instead of fighting over one.
///
/// Most entries kept. A closed window leaves its entry behind, the table is cleared when it
/// grows past this (everything then repaints once).
const MAX_IDS: usize = 512;

fn element_id(inner: &Id, thumb: bool, radius: i32) -> Id {
    IDS.with(|ids| {
        let mut ids = ids.borrow_mut();
        if ids.len() > MAX_IDS {
            ids.clear();
        }
        let entry = ids
            .entry((inner.clone(), thumb))
            .or_insert_with(|| (radius, Id::new()));
        if entry.0 != radius {
            *entry = (radius, Id::new());
        }
        entry.1.clone()
    })
}

/// What drawing one window with rounded corners needs.
#[derive(Clone, Debug)]
pub struct Rounding {
    program: GlesTexProgram,
    /// The window's geometry (not the whole surface - a client shadow lies outside it), in
    /// frame pixels.
    geo: Rectangle<i32, Physical>,
    radius: i32,
    /// Fixes the colors of a YUV buffer the driver converted with another matrix or range.
    correction: Option<Correction>,
    /// Converts the surface's colors to those of the output (wp-color-management).
    conversion: Option<Conversion>,
}

impl Rounding {
    /// A mask that lets everything through, for a surface that only needs its colors fixed
    /// (a video on a square window). `None` if the shader is unavailable.
    pub fn passthrough(renderer: &mut GlesRenderer) -> Option<Self> {
        Some(Self {
            program: program(renderer)?,
            geo: Rectangle::new((-100_000, -100_000).into(), (200_000, 200_000).into()),
            radius: 0,
            correction: None,
            conversion: None,
        })
    }

    /// The same, and also fix the colors of a YUV buffer with `correction`.
    pub fn with_correction(mut self, correction: Correction) -> Self {
        self.correction = Some(correction);
        self
    }

    /// The same, and also convert the surface's colors to the output's with `conversion`.
    pub fn with_conversion(mut self, conversion: Conversion) -> Self {
        self.conversion = Some(conversion);
        self
    }

    /// `geo` is the window geometry in frame coordinates, `radius` in pixels. `None` when
    /// there's nothing to round (no radius, empty window) or the shader is unavailable.
    pub fn new(
        renderer: &mut GlesRenderer,
        geo: Rectangle<i32, Logical>,
        radius: i32,
    ) -> Option<Self> {
        if radius <= 0 || geo.size.w <= 0 || geo.size.h <= 0 {
            return None;
        }
        let program = program(renderer)?;
        // jff draws at scale 1: logical and physical pixels coincide.
        let geo = Rectangle::new(
            (geo.loc.x, geo.loc.y).into(),
            (geo.size.w, geo.size.h).into(),
        );
        Some(Self {
            program,
            geo,
            radius,
            correction: None,
            conversion: None,
        })
    }

    fn element<E>(&self, element: E, thumb: bool) -> RoundedElement<E>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        RoundedElement {
            // A new correction or conversion is a new id, like a new radius: the window is
            // repainted.
            id: element_id(
                element.id(),
                thumb,
                self.radius
                    .wrapping_add(self.correction.map_or(0, |fix| fix.tag()))
                    .wrapping_add(self.conversion.map_or(0, |conversion| conversion.tag())),
            ),
            inner: element,
            rounding: self.clone(),
        }
    }

    /// Make the next texture draw on `frame` go through the mask. Pair with
    /// [`Rounding::clear`] once that draw is done.
    pub fn apply(&self, frame: &mut GlesFrame<'_, '_>) {
        // The geometry as the shader sees the frame (`gl_FragCoord`): the framebuffer may be
        // flipped or rotated relative to what the user sees.
        let output_size = frame.output_size();
        let geo = frame
            .transformation()
            .transform_rect_in(self.geo, &output_size);
        let mut uniforms = vec![
            Uniform::new("geo_min", [geo.loc.x as f32, geo.loc.y as f32]),
            Uniform::new(
                "geo_max",
                [
                    (geo.loc.x + geo.size.w) as f32,
                    (geo.loc.y + geo.size.h) as f32,
                ],
            ),
            Uniform::new("radius", self.radius as f32),
        ];
        match self.correction {
            Some(fix) => uniforms.extend([
                Uniform::new("ycc_r", fix.rows[0]),
                Uniform::new("ycc_g", fix.rows[1]),
                Uniform::new("ycc_b", fix.rows[2]),
                Uniform::new("ycc_off", fix.offset),
                Uniform::new("ycc_on", 1.0f32),
            ]),
            None => uniforms.push(Uniform::new("ycc_on", 0.0f32)),
        }
        match self.conversion {
            Some(conversion) => uniforms.extend([
                Uniform::new("cm_r", conversion.rows[0]),
                Uniform::new("cm_g", conversion.rows[1]),
                Uniform::new("cm_b", conversion.rows[2]),
                Uniform::new("cm_decode", conversion.decode.shader_params()),
                Uniform::new("cm_src_luma", conversion.decode.shader_luma()),
                Uniform::new("cm_encode", conversion.encode.shader_params()),
                Uniform::new("cm_map", conversion.map.shader_param()),
                Uniform::new("cm_luma", conversion.luma),
                Uniform::new("cm_scale", conversion.scale),
                Uniform::new("cm_working", f32::from(u8::from(conversion.working))),
                Uniform::new("cm_on", 1.0f32),
            ]),
            None => uniforms.push(Uniform::new("cm_on", 0.0f32)),
        }
        frame.override_default_tex_program(self.program.clone(), uniforms);
    }

    /// Undo [`Rounding::apply`].
    pub fn clear(frame: &mut GlesFrame<'_, '_>) {
        frame.clear_tex_program_override();
    }

    /// A surface of a live window, shaped by the mask.
    pub fn wrap(&self, element: WaylandSurfaceRenderElement<GlesRenderer>) -> JffElement {
        JffElement::from(self.element(element, false))
    }

    /// A surface of an overview thumbnail, shaped by the mask. `self` must have been made for
    /// the thumbnail - its rectangle on screen and a radius shrunk with it.
    pub fn wrap_thumb(&self, element: ThumbSurface) -> JffElement {
        JffElement::from(self.element(element, true))
    }

    /// A painted picture (the cursor, a toast, an app icon) drawn through the shader.
    pub fn wrap_memory(
        &self,
        element: MemoryRenderBufferRenderElement<GlesRenderer>,
    ) -> JffElement {
        JffElement::from(self.element(element, false))
    }

    /// A painted picture drawn at another size, through the shader.
    pub fn wrap_scaled_memory(
        &self,
        element: RescaleRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>,
    ) -> JffElement {
        JffElement::from(self.element(element, false))
    }
}

/// Whether `window` is fullscreen: those keep square corners (and stay eligible for direct
/// scanout).
pub fn is_fullscreen(window: &Window) -> bool {
    window.toplevel().is_some_and(|t| {
        t.with_pending_state(|s| s.states.contains(xdg_toplevel::State::Fullscreen))
    })
}

/// A window surface shrunk and moved into its slot in the overview.
pub type ThumbSurface =
    RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>;

/// A window surface drawn through the rounded-corner shader.
pub type RoundedSurfaceElement = RoundedElement<WaylandSurfaceRenderElement<GlesRenderer>>;

/// An overview thumbnail drawn through the rounded-corner shader.
pub type RoundedThumbElement = RoundedElement<ThumbSurface>;

/// A painted picture drawn through the shader.
pub type RoundedMemoryElement = RoundedElement<MemoryRenderBufferRenderElement<GlesRenderer>>;

/// A painted picture drawn at another size, through the shader.
pub type RoundedScaledMemoryElement =
    RoundedElement<RescaleRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>>;

/// An element drawn through the rounded-corner shader.
#[derive(Debug)]
pub struct RoundedElement<E> {
    inner: E,
    id: Id,
    rounding: Rounding,
}

impl<E: Element> Element for RoundedElement<E> {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }

    // Corners are see-through, so nothing of the surface may count as opaque: the compositor
    // would otherwise skip drawing what's behind it.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    // Forwarded, never reset: the surface's alpha (the window's open fade times the client's
    // wp_alpha_modifier_v1 multiplier) reaches the shader's `alpha` uniform through the draw
    // call, so a faded window stays faded behind the mask.
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for RoundedElement<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        // The surface draws itself with the frame's default texture shader - swap ours in for
        // that one call.
        self.rounding.apply(frame);
        let result = RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        );
        Rounding::clear(frame);
        result
    }
}

/// EGL-context user data for the ring: the program, or `None` if compiling failed.
struct CachedRingProgram(Option<GlesPixelProgram>);

fn ring_program(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    if let Some(cached) = renderer
        .egl_context()
        .user_data()
        .get::<CachedRingProgram>()
    {
        return cached.0.clone();
    }
    let compiled = renderer
        .compile_custom_pixel_shader(
            RING_SHADER,
            &[
                UniformName::new("border", UniformType::_1f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("color", UniformType::_4f),
                UniformName::new("color2", UniformType::_4f),
                UniformName::new("phase", UniformType::_1f),
                UniformName::new("animated", UniformType::_1f),
                UniformName::new("sweep", UniformType::_1f),
            ],
        )
        .map_err(|err| tracing::warn!("error compiling the outline shader: {err:#}"))
        .ok();
    let stored = compiled.clone();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CachedRingProgram(stored));
    compiled
}

/// What a ring element was made from.
#[derive(PartialEq)]
struct RingKey {
    area: Rectangle<i32, Logical>,
    border: i32,
    radius: i32,
    color: [f32; 4],
    /// Drawn for an HDR output, whose working space needs the colors converted.
    working: bool,
}

thread_local! {
    /// Last ring, handed out again while nothing about it changed. That keeps its id stable,
    /// so Smithay's damage tracking sees the same element and repaints nothing - a fresh
    /// element every frame would repaint every frame. Only one thumbnail is selected at a time.
    static RING: RefCell<Option<(RingKey, PixelShaderElement)>> = const { RefCell::new(None) };
}

/// Outline of the selected overview thumbnail: a rounded ring `border` px wide around `thumb`,
/// in `color` (straight alpha, so a translucent colour is a translucent outline). `radius` is
/// the thumbnail's own corner radius, the outline's outer corners are that plus `border` so
/// the two curves are concentric. Draw it behind the thumbnail.
///
/// `None` if the shader is unavailable - the caller then falls back to a plain rectangle.
pub fn ring_element(
    renderer: &mut GlesRenderer,
    thumb: Rectangle<i32, Logical>,
    border: i32,
    radius: i32,
    color: [f32; 4],
) -> Option<PixelShaderElement> {
    let area = ring_area(thumb, border);
    let key = RingKey {
        area,
        border,
        radius,
        color,
        working: super::gamut::working().is_some(),
    };
    if let Some(element) = RING.with(|ring| {
        ring.borrow()
            .as_ref()
            .filter(|(last, _)| *last == key)
            .map(|(_, element)| element.clone())
    }) {
        return Some(element);
    }

    let element = make_ring(renderer, area, border, radius, color, RingMotion::default())?;
    RING.with(|ring| *ring.borrow_mut() = Some((key, element.clone())));
    Some(element)
}

/// The area a ring around `inner` covers: `inner` grown by `border` on every side.
pub fn ring_area(inner: Rectangle<i32, Logical>, border: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (inner.loc.x - border, inner.loc.y - border).into(),
        (inner.size.w + 2 * border, inner.size.h + 2 * border).into(),
    )
}

/// The turning part of a ring (window borders only). The default is a ring that stands still,
/// which is what the overview's outline is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RingMotion {
    /// Second colour (straight RGBA).
    pub color2: [f32; 4],
    /// Position in the turn, 0.0 to 1.0.
    pub phase: f32,
    /// How much of the motion shows, 0.0 (none) to 1.0.
    pub amount: f32,
    /// A travelling highlight instead of a turning gradient.
    pub sweep: bool,
}

/// A new ring element, with its own id: `area` as [`ring_area`] gave it, the rest as in
/// [`ring_element`]. The caller keeps it for as long as nothing about it changes (see the cache
/// in `ring_element`), a fresh one every frame would be repainted every frame. `None` if the
/// shader is unavailable.
pub fn make_ring(
    renderer: &mut GlesRenderer,
    area: Rectangle<i32, Logical>,
    border: i32,
    radius: i32,
    color: [f32; 4],
    motion: RingMotion,
) -> Option<PixelShaderElement> {
    let program = ring_program(renderer)?;
    // colors are sRGB in the config, an HDR output wants them in its working space
    let color = super::gamut::tint(color);
    let motion = RingMotion {
        color2: super::gamut::tint(motion.color2),
        ..motion
    };
    Some(PixelShaderElement::new(
        program,
        area,
        None,
        1.0,
        vec![
            Uniform::new("border", border as f32),
            Uniform::new("radius", radius as f32),
            Uniform::new("color", color),
            Uniform::new("color2", motion.color2),
            Uniform::new("phase", motion.phase),
            Uniform::new("animated", motion.amount),
            Uniform::new("sweep", if motion.sweep { 1.0f32 } else { 0.0 }),
        ],
        Kind::Unspecified,
    ))
}

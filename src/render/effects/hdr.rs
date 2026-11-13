//! HDR output: the frame is composited into a linear fp16 texture and a final pass turns it into the PQ (or HLG) signal.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::{Error as DamageError, OutputDamageTracker};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{
    Element, Id, Kind, RenderElement, RenderElementPresentationState, RenderElementStates,
};
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType,
};
use smithay::backend::renderer::utils::{
    CommitCounter, DamageBag, DamageSet, DamageSnapshot, OpaqueRegions,
};
use smithay::backend::renderer::{Bind, Offscreen};
use smithay::output::Output;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Size, Transform};

use crate::primitives::color::{ContentLight, ImageDescription};
use crate::primitives::gamut::Conversion;
use crate::primitives::hdr::OutputEotf;
use crate::primitives::hdr_output::{frame_content_peak, FinalPass};
use crate::protocol::color_management::WorkingSpace;
use crate::render::effects::rounded::Rounding;
use crate::render::elements::JffElement;

const SHADER: &str = include_str!("shaders/hdr_final.frag");

/// The texture format of the working space, and of what blur copies out of it.
pub const WORKING_FORMAT: Fourcc = Fourcc::Abgr16161616f;

/// EGL-context user data: the final pass program, or `None` if compiling failed.
struct CachedProgram(Option<GlesTexProgram>);

fn program(renderer: &mut GlesRenderer) -> Option<GlesTexProgram> {
    if let Some(cached) = renderer.egl_context().user_data().get::<CachedProgram>() {
        return cached.0.clone();
    }
    let vec3 = |name| UniformName::new(name, UniformType::_3f);
    let compiled = renderer
        .compile_custom_texture_shader(
            SHADER,
            &[
                UniformName::new("sdr_white", UniformType::_1f),
                UniformName::new("peak", UniformType::_1f),
                vec3("to_r"),
                vec3("to_g"),
                vec3("to_b"),
                vec3("from_r"),
                vec3("from_g"),
                vec3("from_b"),
                vec3("luma_2020"),
                vec3("luma_panel"),
                UniformName::new("map_on", UniformType::_1f),
                UniformName::new("tone_kind", UniformType::_1f),
                UniformName::new("content_peak", UniformType::_1f),
                UniformName::new("out_hlg", UniformType::_1f),
                UniformName::new("dither_levels", UniformType::_1f),
            ],
        )
        .map_err(|err| tracing::warn!("error compiling the HDR output shader: {err:#}"))
        .ok();
    let stored = compiled.clone();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CachedProgram(stored));
    compiled
}

/// Whether this renderer can do an HDR frame: a float render target and both shaders.
pub fn supported(renderer: &mut GlesRenderer) -> bool {
    program(renderer).is_some()
        && Rounding::passthrough(renderer).is_some()
        && Offscreen::<GlesTexture>::create_buffer(renderer, WORKING_FORMAT, (16, 16).into())
            .is_ok()
}

/// What an sRGB color (straight alpha last) is in the working space.
pub fn clear_color(space: WorkingSpace, color: [f32; 4]) -> [f32; 4] {
    let conversion = Conversion::to_working(&ImageDescription::srgb(), space.sdr_white as f32);
    let [r, g, b] = conversion.apply([color[0], color[1], color[2]]);
    [r, g, b, color[3]]
}

/// Bring the frame's elements that are drawn as plain sRGB into the working space: client cursors, painted pictures and solid rects. Surfaces that were tagged already are, and so are the rings and borders (their colors) and blur (it only moves pixels).
pub fn linearize(
    renderer: &mut GlesRenderer,
    elements: Vec<JffElement>,
    space: WorkingSpace,
) -> Vec<JffElement> {
    let conversion = Conversion::to_working(&ImageDescription::srgb(), space.sdr_white as f32);
    let Some(rounding) = Rounding::passthrough(renderer).map(|r| r.with_conversion(conversion))
    else {
        return elements;
    };
    elements
        .into_iter()
        .map(|element| match element {
            JffElement::Surface(inner) => rounding.wrap(inner),
            JffElement::Thumb(inner) => rounding.wrap_thumb(inner),
            JffElement::Memory(inner) => rounding.wrap_memory(inner),
            JffElement::ScaledMemory(inner) => rounding.wrap_scaled_memory(inner),
            JffElement::Solid(inner) => JffElement::Solid(solid(space, &inner)),
            other => other,
        })
        .collect()
}

/// The same rect with its color (premultiplied) in the working space.
fn solid(space: WorkingSpace, element: &SolidColorRenderElement) -> SolidColorRenderElement {
    let [r, g, b, a] = element.color().components();
    let straight = if a > 0.0 {
        [r / a, g / a, b / a]
    } else {
        [0.0; 3]
    };
    let [r, g, b] = super::gamut::srgb_to_working(space, straight).map(|v| v * a);
    SolidColorRenderElement::new(
        element.id().clone(),
        element.geometry(Scale::from(1.0)),
        element.current_commit(),
        [r, g, b, a],
        element.kind(),
    )
}

/// The size of the working texture: the mode in the output's own orientation, width and height swapped for a quarter turn. The output's compositor turns the frame onto the panel, so the texture is never turned itself.
fn oriented_size(output: &Output) -> Option<Size<i32, Physical>> {
    let mode = output.current_mode()?;
    Some(
        output
            .current_transform()
            .transform_size(Size::from((mode.size.w, mode.size.h))),
    )
}

/// Whether the damage tracker drew (any of) the element `id` into the last frame: it is not hidden under something opaque, and not off the output.
fn drawn(states: &RenderElementStates, id: &Id) -> bool {
    states.states.get(id).is_some_and(|state| {
        !matches!(
            state.presentation_state,
            RenderElementPresentationState::Skipped
        )
    })
}

/// The frame of one HDR output: the texture it is composited into, and what is needed to repaint only what changed.
pub struct HdrFrame {
    texture: GlesTexture,
    tracker: OutputDamageTracker,
    damage: DamageBag<i32, Physical>,
    id: Id,
    size: Size<i32, Physical>,
    /// 0 until the texture holds a whole frame.
    age: usize,
    /// The settings of the output. The content peak of `pass` is always `None`: it is `content_peak` below that goes to the element.
    pass: FinalPass,
    /// The content peak the last frame was drawn through, `None` when no visible surface stated one.
    content_peak: Option<f32>,
    program: GlesTexProgram,
}

impl HdrFrame {
    /// A frame for `output` at its current mode and orientation, drawn out through `pass`. `Err` says why it could not be made.
    pub fn new(
        renderer: &mut GlesRenderer,
        output: &Output,
        pass: FinalPass,
    ) -> Result<Self, String> {
        let size = oriented_size(output).ok_or("the output has no mode")?;
        let program = program(renderer).ok_or("the final pass shader did not compile")?;
        let texture = renderer
            .create_buffer(WORKING_FORMAT, (size.w, size.h).into())
            .map_err(|err| format!("no float render target: {err:?}"))?;
        Ok(Self {
            texture,
            // The tracker does not follow the output's transform: the frame stays in the output's orientation and the compositor that shows it applies the transform.
            tracker: OutputDamageTracker::new(size, Scale::from(1.0), Transform::Normal),
            damage: DamageBag::default(),
            id: Id::new(),
            size,
            age: 0,
            pass,
            content_peak: None,
            program,
        })
    }

    /// The content peak the last frame went out through, in cd/m².
    pub fn content_peak(&self) -> Option<f32> {
        self.content_peak
    }

    /// Whether this frame still fits the output's mode, its orientation (a quarter turn swaps the size) and the final pass settings.
    pub fn fits(&self, output: &Output, pass: &FinalPass) -> bool {
        self.pass == *pass && oriented_size(output) == Some(self.size)
    }

    /// Composite `elements` (already in the working space) and give back the element that puts the result on the output, with the states of the elements for presentation feedback.
    ///
    /// `lights` is the content light of the surfaces among `elements`, by the id of the element they were drawn as (see `gamut::take_content_light`), empty to leave the tone curve to the config. Only those the damage tracker really drew count, so a surface under an opaque window, or a thumbnail, does not. If the content peak they give differs from the last frame's, the whole output is damaged: the final pass is a full-screen effect that reads it, but nothing in the texture changed, so the element would otherwise look unchanged to the output's compositor.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[JffElement],
        clear: [f32; 4],
        lights: &[(Id, ContentLight)],
    ) -> Result<(HdrFrameElement, RenderElementStates), DamageError<GlesError>> {
        let _drawing = super::gamut::begin_hdr_draw();
        let (damage, states) = {
            let mut target = renderer
                .bind(&mut self.texture)
                .map_err(DamageError::Rendering)?;
            let result =
                self.tracker
                    .render_output(renderer, &mut target, self.age, elements, clear)?;
            // The next pass draws from this texture on the same context, so it is ordered after this one.
            (result.damage.cloned(), result.states)
        };
        self.age = 1;
        if let Some(damage) = damage.filter(|damage| !damage.is_empty()) {
            self.damage.add(damage);
        }
        let content_peak = frame_content_peak(
            self.pass.peak,
            lights
                .iter()
                .filter(|(id, _)| drawn(&states, id))
                .map(|(_, light)| *light),
        );
        if content_peak != self.content_peak {
            self.content_peak = content_peak;
            self.damage.add([Rectangle::from_size(self.size)]);
        }
        let element = HdrFrameElement {
            texture: self.texture.clone(),
            id: self.id.clone(),
            size: self.size,
            damage: self.damage.snapshot(),
            program: self.program.clone(),
            pass: self.pass.with_content_peak(content_peak),
        };
        Ok((element, states))
    }
}

/// The working-space texture drawn through the final pass: the one element the output's own compositor sees.
#[derive(Debug)]
pub struct HdrFrameElement {
    texture: GlesTexture,
    id: Id,
    size: Size<i32, Physical>,
    damage: DamageSnapshot<i32, Physical>,
    program: GlesTexProgram,
    pass: FinalPass,
}

impl Element for HdrFrameElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.damage.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size((f64::from(self.size.w), f64::from(self.size.h)).into())
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        Rectangle::from_size(self.size)
    }

    fn transform(&self) -> Transform {
        Transform::Normal
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.damage
            .damage_since(commit)
            .unwrap_or_else(|| DamageSet::from_slice(&[Rectangle::from_size(self.size)]))
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::from_slice(&[Rectangle::from_size(self.size)])
    }

    fn alpha(&self) -> f32 {
        1.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for HdrFrameElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let pass = &self.pass;
        let uniforms = vec![
            Uniform::new("sdr_white", pass.sdr_white),
            Uniform::new("peak", pass.peak),
            Uniform::new("to_r", pass.to_panel[0]),
            Uniform::new("to_g", pass.to_panel[1]),
            Uniform::new("to_b", pass.to_panel[2]),
            Uniform::new("from_r", pass.from_panel[0]),
            Uniform::new("from_g", pass.from_panel[1]),
            Uniform::new("from_b", pass.from_panel[2]),
            Uniform::new("luma_2020", pass.luma_2020),
            Uniform::new("luma_panel", pass.luma_panel),
            Uniform::new("map_on", f32::from(u8::from(pass.map_gamut))),
            Uniform::new("tone_kind", pass.tone.shader_param()),
            Uniform::new("content_peak", pass.content_peak.unwrap_or(0.0)),
            Uniform::new("out_hlg", f32::from(u8::from(pass.eotf == OutputEotf::Hlg))),
            Uniform::new("dither_levels", pass.dither_levels),
        ];
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            Some(&self.program),
            &uniforms,
        )
    }
}

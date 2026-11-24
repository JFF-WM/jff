//! The screen magnifier: the frame of a monitor is composited into a texture of its own, and the
//! part of it around the pointer is drawn enlarged over the whole monitor. Blur, rounded
//! corners and everything else are drawn at their normal size first, so they are only enlarged
//! with the rest. An HDR monitor does not need this, its frame is a texture already (see
//! `HdrFrame::set_view`).

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::{Error as DamageError, OutputDamageTracker};
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{
    CommitCounter, DamageBag, DamageSet, DamageSnapshot, OpaqueRegions,
};
use smithay::backend::renderer::{Bind, Offscreen};
use smithay::output::Output;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

use super::hdr::oriented_size;
use crate::render::elements::JffElement;

/// The texture format of the frame the monitor is composited into.
const FORMAT: Fourcc = Fourcc::Abgr8888;

/// The frame of one monitor while it is zoomed: the texture the monitor is composited into,
/// and what is needed to repaint only what changed.
pub struct MagnifierFrame {
    texture: GlesTexture,
    tracker: OutputDamageTracker,
    damage: DamageBag<i32, Physical>,
    id: Id,
    size: Size<i32, Physical>,
    /// The scale of the output the tracker draws for.
    scale: f64,
    /// 0 until the texture holds a whole frame.
    age: usize,
    /// The part of the texture the last frame showed.
    view: Option<Rectangle<f64, Buffer>>,
}

impl MagnifierFrame {
    /// A frame for `output` at its current mode and orientation. `Err` says why it could not be
    /// made.
    pub fn new(renderer: &mut GlesRenderer, output: &Output) -> Result<Self, String> {
        let size = oriented_size(output).ok_or("the output has no mode")?;
        let scale = output.current_scale().fractional_scale();
        let texture = renderer
            .create_buffer(FORMAT, (size.w, size.h).into())
            .map_err(|err| format!("no render target: {err:?}"))?;
        Ok(Self {
            texture,
            // The frame stays in the output's orientation, the compositor that shows it applies
            // the transform.
            tracker: OutputDamageTracker::new(size, Scale::from(scale), Transform::Normal),
            damage: DamageBag::default(),
            id: Id::new(),
            size,
            scale,
            age: 0,
            view: None,
        })
    }

    /// Whether this frame still fits the output's mode, orientation and scale.
    pub fn fits(&self, output: &Output) -> bool {
        oriented_size(output) == Some(self.size)
            && output.current_scale().fractional_scale() == self.scale
    }

    /// Composite `elements` and give back the element that draws `view` (in logical pixels of
    /// the output) enlarged over all of it, with the states of the elements for presentation
    /// feedback.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[JffElement],
        clear: [f32; 4],
        view: Rectangle<f64, Logical>,
    ) -> Result<(MagnifiedElement, RenderElementStates), DamageError<GlesError>> {
        let (damage, states) = {
            let mut target = renderer
                .bind(&mut self.texture)
                .map_err(DamageError::Rendering)?;
            let result =
                self.tracker
                    .render_output(renderer, &mut target, self.age, elements, clear)?;
            (result.damage.cloned(), result.states)
        };
        self.age = 1;
        // The output is drawn from part of the texture, so any change in it or in what part is
        // shown moves every pixel.
        let src = Rectangle::new(
            (view.loc.x * self.scale, view.loc.y * self.scale).into(),
            (view.size.w * self.scale, view.size.h * self.scale).into(),
        );
        if damage.is_some_and(|damage| !damage.is_empty()) || self.view != Some(src) {
            self.damage.add([Rectangle::from_size(self.size)]);
        }
        self.view = Some(src);
        let element = MagnifiedElement {
            texture: self.texture.clone(),
            id: self.id.clone(),
            size: self.size,
            damage: self.damage.snapshot(),
            src,
        };
        Ok((element, states))
    }
}

/// Part of the monitor's own texture, drawn enlarged over the whole monitor.
#[derive(Debug)]
pub struct MagnifiedElement {
    texture: GlesTexture,
    id: Id,
    size: Size<i32, Physical>,
    damage: DamageSnapshot<i32, Physical>,
    src: Rectangle<f64, Buffer>,
}

impl Element for MagnifiedElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.damage.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
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

impl RenderElement<GlesRenderer> for MagnifiedElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            1.0,
            None,
            &[],
        )
    }
}

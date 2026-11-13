//! A render element that blurs whatever is already on screen behind it.
//!
//! Normal elements draw content (a window buffer, a colour), this one draws a treatment of
//! the frame drawn so far. Smithay walks the element list back-to-front and, for a
//! framebuffer effect, does two steps:
//!
//! 1. `capture_framebuffer`: everything behind is in the framebuffer by now. Copy the pixels
//!    under the element into a texture and blur it.
//! 2. `draw`: paint the blurred texture back over the same area.
//!
//! Elements in front are drawn afterwards, on top - that's what makes a translucent window
//! look frosted: the window is drawn over the blur.
//!
//! Only the parts of the area inside `subregion` are painted. `draw` receives a list of
//! damage rectangles and paints nothing outside them, so narrowing the damage to the
//! subregion is all the "shaping" needed.
//!
//! The two steps share a texture through `cache`, a `UserDataMap` Smithay keeps per element.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use smithay::backend::renderer::element::{Element, Id, RenderElement};
use smithay::backend::renderer::gles::{
    ffi, Capability, GlesError, GlesFrame, GlesRenderer, GlesTexture,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Frame, FrameContext, Offscreen as _, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Size, Transform};

use super::background_effect::RenderParams;
use super::blur::{Blur, BlurOptions};
use super::rounded::Rounding;
use crate::primitives::region::TransformedRegion;

/// The long-lived half: identity and change counter used for Smithay's damage tracking
/// ("same element as last frame" / "it changed").
#[derive(Debug)]
pub struct FramebufferEffect {
    id: Id,
    commit: CommitCounter,
}

/// A copy of the frame as it looks once only the wallpaper (background layers) is drawn.
///
/// One per output. A [`snapshot element`](Self::element) placed right in front of the
/// background layers in the element list fills it. Blur elements using it as source blur this
/// copy instead of the live frame, so windows behind them never show up in (or through) the
/// blur. The texture survives between frames: a frame where nothing under the wallpaper
/// changed doesn't re-copy it.
#[derive(Clone, Debug)]
pub struct WallpaperSnapshot(Rc<RefCell<SnapshotState>>);

#[derive(Debug)]
struct SnapshotState {
    id: Id,
    commit: CommitCounter,
    /// Framebuffer-oriented pixels of the whole output.
    texture: Option<GlesTexture>,
}

thread_local! {
    static SNAPSHOTS: RefCell<HashMap<String, WallpaperSnapshot>> = RefCell::new(HashMap::new());
}

impl WallpaperSnapshot {
    /// Snapshot of the output called `name`, created on first use. Rendering is single-
    /// threaded, so a per-thread table is enough.
    pub fn for_output(name: &str) -> Self {
        SNAPSHOTS.with(|all| {
            all.borrow_mut()
                .entry(name.to_owned())
                .or_insert_with(|| {
                    Self(Rc::new(RefCell::new(SnapshotState {
                        id: Id::new(),
                        commit: CommitCounter::default(),
                        texture: None,
                    })))
                })
                .clone()
        })
    }

    /// The element that fills the snapshot when drawn. Place it in front of the background
    /// layers so it sees only them. `size` is the output size.
    pub fn element(&self, size: Size<i32, Logical>) -> FramebufferEffectElement {
        let state = self.0.borrow();
        FramebufferEffectElement {
            id: state.id.clone(),
            commit: state.commit,
            geometry: Rectangle::from_size(size.to_f64()),
            subregion: TransformedRegion {
                rects: Arc::new(Vec::new()),
                scale: Scale::from(1.),
                offset: (0., 0.).into(),
            },
            blur_options: BlurOptions::default(),
            captures: Some(self.clone()),
            source: None,
            mask: None,
            alpha: 1.0,
        }
    }
}

/// One frame's element. Cheap, the textures live in `cache`.
#[derive(Debug)]
pub struct FramebufferEffectElement {
    id: Id,
    commit: CommitCounter,
    /// `Some`: paints nothing, only copies the frame into the snapshot.
    captures: Option<WallpaperSnapshot>,
    /// `Some`: blur this wallpaper copy instead of the live frame.
    source: Option<WallpaperSnapshot>,
    geometry: Rectangle<f64, Logical>,
    /// Only these parts of `geometry` are painted.
    subregion: TransformedRegion,
    blur_options: BlurOptions,
    /// The window's rounded corners: the blurred picture is painted through this mask.
    mask: Option<Rounding>,
    /// How opaque the blurred picture is painted, 1.0 unless the window is faded.
    alpha: f32,
}

/// What `capture_framebuffer` leaves for `draw`.
#[derive(Debug)]
struct Inner {
    /// Receives the pixels copied out of the framebuffer.
    framebuffer: Option<GlesTexture>,
    blur: Option<Blur>,
    /// Painted by `draw` this frame. Cleared at the start of every capture.
    result: Option<GlesTexture>,
    /// Scratch list for filtered damage, reused to keep its allocation.
    filtered_damage: Vec<Rectangle<i32, Physical>>,
}

impl FramebufferEffect {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
        }
    }

    /// Force a repaint next frame even if nothing behind it moved.
    pub fn damage(&mut self) {
        self.commit.increment();
    }

    pub fn render(
        &self,
        params: RenderParams,
        blur_options: BlurOptions,
    ) -> FramebufferEffectElement {
        FramebufferEffectElement {
            id: self.id.clone(),
            commit: self.commit,
            geometry: params.geometry,
            subregion: params.subregion,
            blur_options,
            captures: None,
            source: None,
            mask: params.mask,
            alpha: params.alpha,
        }
    }
}

impl Default for FramebufferEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl FramebufferEffectElement {
    /// Blur the wallpaper copy in `snapshot` instead of the live frame.
    pub fn with_wallpaper_source(mut self, snapshot: Option<&WallpaperSnapshot>) -> Self {
        self.source = snapshot.cloned();
        self
    }

    /// Copy the whole frame into the snapshot. Only the snapshot element does this.
    fn capture_snapshot(
        &self,
        snapshot: &WallpaperSnapshot,
        frame: &mut GlesFrame<'_, '_>,
    ) -> Result<(), GlesError> {
        if !frame.capabilities().contains(&Capability::Blit) {
            return Ok(());
        }
        let output_size = frame.output_size();
        let transform = frame.transformation();
        let fb_size = transform.transform_size(output_size);
        let size: Size<i32, Buffer> = (fb_size.w, fb_size.h).into();

        {
            let mut state = snapshot.0.borrow_mut();
            let format = super::gamut::framebuffer_format();
            if state
                .texture
                .as_ref()
                .is_some_and(|t| t.size() != size || t.format() != Some(format))
            {
                state.texture = None;
            }
            if state.texture.is_none() {
                let mut guard = frame.renderer();
                let texture: GlesTexture = guard.as_mut().create_buffer(format, size)?;
                state.texture = Some(texture);
            }
        }
        let texture = snapshot.0.borrow().texture.clone().expect("created above");

        frame.with_profiled_context(
            gpu_span_location!("FramebufferEffectElement::capture_snapshot"),
            |gl| unsafe {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut draw_fbo = 0;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut draw_fbo);
                let scissor = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;
                gl.Disable(ffi::SCISSOR_TEST);

                let mut fbo = 0;
                gl.GenFramebuffers(1, &mut fbo);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    texture.tex_id(),
                    0,
                );
                gl.BlitFramebuffer(
                    0,
                    0,
                    size.w,
                    size.h,
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::NEAREST,
                );

                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, draw_fbo as u32);
                if scissor {
                    gl.Enable(ffi::SCISSOR_TEST);
                }
                gl.DeleteFramebuffers(1, &fbo);

                if gl.GetError() == ffi::NO_ERROR {
                    Ok(())
                } else {
                    Err(GlesError::BlitError)
                }
            },
        )??;
        Ok(())
    }
}

impl Element for FramebufferEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    // No buffer. `src` is in logical units and tells `draw` how much of the element is
    // visible (less than all of it if cropped).
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.geometry.size.to_buffer(1., Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry.to_physical_precise_round(scale)
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl RenderElement<GlesRenderer> for FramebufferEffectElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        if let Some(snapshot) = &self.captures {
            return self.capture_snapshot(snapshot, frame);
        }

        let output_rect = Rectangle::from_size(frame.output_size());
        let transform = frame.transformation();
        // glBlitFramebuffer is what copies the pixels, without it there is nothing to do.
        let can_blit = frame.capabilities().contains(&Capability::Blit);

        let mut guard = frame.renderer();
        let renderer = guard.as_mut();

        let inner = cache.get_or_insert::<RefCell<Inner>, _>(|| {
            RefCell::new(Inner {
                framebuffer: None,
                blur: Blur::new(renderer),
                result: None,
                filtered_damage: Vec::new(),
            })
        });
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;

        // Last frame's picture is stale. Dropping it also releases the reference that would
        // stop the blur from reusing its textures.
        inner.result = None;

        if !can_blit {
            tracing::warn!("background blur disabled: the renderer cannot blit the framebuffer");
            return Ok(());
        }
        // Blur programs failed to compile, already logged.
        if inner.blur.is_none() {
            return Ok(());
        }

        // Pixels outside the framebuffer can't be blitted - the GPU skips them instead of
        // repeating the edge, leaving a hole. Copy only what's on screen, `draw` clamps the
        // same way so the texture lines up.
        let Some(clamped) = dst.intersection(output_rect) else {
            return Ok(());
        };

        // Same rectangle in framebuffer pixels: `dst` is in the picture the user sees, the
        // framebuffer may be flipped relative to it.
        let source_rect = transform.transform_rect_in(clamped, &output_rect.size);
        let size: Size<i32, Buffer> = (source_rect.size.w, source_rect.size.h).into();

        let format = super::gamut::framebuffer_format();
        if inner
            .framebuffer
            .as_ref()
            .is_some_and(|t| t.size() != size || t.format() != Some(format))
        {
            inner.framebuffer = None;
        }
        if inner.framebuffer.is_none() {
            let texture: GlesTexture = renderer.create_buffer(format, size)?;
            inner.framebuffer = Some(texture);
        }
        let framebuffer = inner.framebuffer.as_ref().expect("created above");
        let blur = inner.blur.as_mut().expect("checked above");

        if let Err(err) = blur.prepare_textures(
            |format, size| renderer.create_buffer(format, size),
            framebuffer,
            self.blur_options,
        ) {
            tracing::warn!("error preparing the blur textures: {err:#}");
            return Ok(());
        }

        // Release the frame before using its GL access below.
        drop(guard);

        // Wallpaper copy to read instead of the live frame, when one is usable: one made in
        // another format (the output switched between HDR and SDR) is left alone.
        let fb_size = transform.transform_size(output_rect.size);
        let wallpaper = self
            .source
            .as_ref()
            .and_then(|s| s.0.borrow().texture.clone())
            .filter(|t| {
                t.format() == Some(format)
                    && t.size().w >= fb_size.w
                    && t.size().h >= fb_size.h
                    && source_rect.loc.x >= 0
                    && source_rect.loc.y >= 0
                    && source_rect.loc.x + source_rect.size.w <= t.size().w
                    && source_rect.loc.y + source_rect.size.h <= t.size().h
            });

        // Must go through the frame's GL access: the renderer's would rebind the framebuffer
        // we want to read from.
        frame.with_profiled_context(
            gpu_span_location!("FramebufferEffectElement::capture_framebuffer"),
            |gl| unsafe {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut draw_fbo = 0;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut draw_fbo);
                let mut read_fbo_before = 0;
                gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut read_fbo_before);
                let scissor = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;

                // The blit obeys the scissor test, we want the whole rectangle.
                gl.Disable(ffi::SCISSOR_TEST);

                // Read side: the wallpaper copy when present, otherwise the live frame.
                let mut read_fbo = 0;
                if let Some(wallpaper) = &wallpaper {
                    gl.GenFramebuffers(1, &mut read_fbo);
                    gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, read_fbo);
                    gl.FramebufferTexture2D(
                        ffi::READ_FRAMEBUFFER,
                        ffi::COLOR_ATTACHMENT0,
                        ffi::TEXTURE_2D,
                        wallpaper.tex_id(),
                        0,
                    );
                }

                // Draw side: our texture. Read side stays on the frame's framebuffer.
                let mut fbo = 0;
                gl.GenFramebuffers(1, &mut fbo);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    framebuffer.tex_id(),
                    0,
                );

                gl.BlitFramebuffer(
                    source_rect.loc.x,
                    source_rect.loc.y,
                    source_rect.loc.x + source_rect.size.w,
                    source_rect.loc.y + source_rect.size.h,
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::LINEAR,
                );

                // Restore what the frame expects.
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, draw_fbo as u32);
                if read_fbo != 0 {
                    gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, read_fbo_before as u32);
                    gl.DeleteFramebuffers(1, &read_fbo);
                }
                if scissor {
                    gl.Enable(ffi::SCISSOR_TEST);
                }
                gl.DeleteFramebuffers(1, &fbo);

                if gl.GetError() == ffi::NO_ERROR {
                    Ok(())
                } else {
                    Err(GlesError::BlitError)
                }
            },
        )??;

        let mut guard = frame.renderer();
        match blur.render(guard.as_mut(), framebuffer, self.blur_options) {
            Ok(blurred) => inner.result = Some(blurred),
            Err(err) => tracing::warn!("error rendering the blur: {err:#}"),
        }
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        // The snapshot element only copies, it paints nothing.
        if self.captures.is_some() {
            return Ok(());
        }

        // Nothing captured: blit unsupported, or the area was off-screen.
        let Some(inner) = cache.and_then(|cache| cache.get::<RefCell<Inner>>()) else {
            return Ok(());
        };
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;
        let Some(texture) = inner.result.as_ref() else {
            return Ok(());
        };

        // Clamp to the screen exactly as the capture did.
        let output_rect = Rectangle::from_size(frame.output_size());
        let Some(clamped) = dst.intersection(output_rect) else {
            return Ok(());
        };
        let clamp_offset = clamped.loc - dst.loc;

        // Keep only the damage inside the subregion. `src` is in logical units (see `src()`).
        let shown = Rectangle::<f64, Logical>::new(
            (
                src.loc.x + self.geometry.loc.x,
                src.loc.y + self.geometry.loc.y,
            )
                .into(),
            (src.size.w, src.size.h).into(),
        );
        let filtered = &mut inner.filtered_damage;
        filtered.clear();
        self.subregion.filter_damage(shown, dst, damage, filtered);

        // Re-express the damage relative to the clamped rectangle.
        if clamped != dst {
            let visible = Rectangle::new(clamp_offset, clamped.size);
            filtered.retain_mut(|d| match d.intersection(visible) {
                Some(mut inside) => {
                    inside.loc -= clamp_offset;
                    *d = inside;
                    true
                }
                None => false,
            });
        }
        if filtered.is_empty() {
            return Ok(());
        }

        // The texture holds framebuffer-oriented pixels: draw it with the inverse of the
        // frame's transform to land the right way up (a no-op on unrotated outputs, and
        // flipping is its own inverse).
        let tex_src = Rectangle::from_size(texture.size().to_f64());
        let texture_transform = frame.transformation().invert();
        // Rounded window: paint through the window's mask. It works on gl_FragCoord, so it
        // doesn't care how this texture is oriented.
        if let Some(mask) = &self.mask {
            mask.apply(frame);
        }
        let result = Frame::render_texture_from_to(
            frame,
            texture,
            tex_src,
            clamped,
            &filtered[..],
            &[],
            texture_transform,
            self.alpha,
        );
        if self.mask.is_some() {
            Rounding::clear(frame);
        }
        result
    }
}

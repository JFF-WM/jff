//! The cross-GPU frame path: the primary draws a monitor's frame, the secondary shows it.
//!
//! Per frame the primary renders the output into a dmabuf, and the secondary imports it and its
//! DrmCompositor shows it as one full-screen texture. The cost is a whole-frame copy on the
//! primary's GPU (into the dmabuf), one more on the secondary's (texture into its scanout
//! buffer), and a CPU wait for each GPU to finish. Fine for a second monitor, too much to be
//! the way every monitor works.
//!
//! An HDR monitor crosses as the signal itself: the primary composites the frame in the working
//! space, codes it as PQ with the final pass (the same one a monitor on the primary gets) and
//! draws that into a 10-bit buffer, which the secondary shows as it is.

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBuffer, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Fourcc, Modifier, Slot, Swapchain};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::texture::{TextureRenderBuffer, TextureRenderElement};
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::ImportDma;
use smithay::output::Output;
use smithay::reexports::drm::control::crtc;
use smithay::utils::{DeviceFd, Scale, Transform};

use super::{secondary_renderer, DeviceId, Tty};
use crate::app::Backend;
use crate::feature::outputs::frame_size;
use crate::feature::screenshot::capture::CrossHdr;
use crate::primitives::hdr_output::FinalPass;
use crate::protocol::color_management::WorkingSpace;
use crate::render::effects::hdr::HdrFrame;
use crate::state::JffState;

/// Formats a frame may cross in, best first: the picture is opaque, so no alpha is the better one.
const CROSS_FORMATS: [Fourcc; 2] = [Fourcc::Xrgb8888, Fourcc::Argb8888];

/// The same for an HDR monitor, whose frame is PQ coded and needs the 10 bits.
const CROSS_FORMATS_HDR: [Fourcc; 4] = [
    Fourcc::Xrgb2101010,
    Fourcc::Xbgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr2101010,
];

render_elements! {
    /// What a monitor on a secondary GPU is given to show.
    pub CrossElement<=GlesRenderer>;
    Frame = TextureRenderElement<GlesTexture>,
    Placeholder = SolidColorRenderElement,
}

/// The format and modifiers of the buffers a frame crosses in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Negotiated {
    pub code: Fourcc,
    pub modifiers: Vec<Modifier>,
}

/// What the primary can draw into (`render`) and the secondary can sample from (`texture`), in
/// the best of `wanted`.
pub(super) fn negotiate(
    render: &FormatSet,
    texture: &FormatSet,
    wanted: &[Fourcc],
) -> Option<Negotiated> {
    for &code in wanted {
        let modifiers: Vec<Modifier> = render
            .intersection(texture)
            .filter(|format| format.code == code)
            .map(|format| format.modifier)
            .collect();
        if !modifiers.is_empty() {
            return Some(Negotiated { code, modifiers });
        }
    }
    // Nothing in common: LINEAR is the modifier two GPUs most often agree on, if the primary draws it.
    for &code in wanted {
        let linear = smithay::backend::allocator::Format {
            code,
            modifier: Modifier::Linear,
        };
        if render.contains(&linear) {
            return Some(Negotiated {
                code,
                modifiers: vec![Modifier::Linear],
            });
        }
    }
    None
}

/// Whether a frame can cross between these two GPUs in a 10-bit format, which a monitor needs to be HDR.
pub(super) fn hdr_crossable(primary: &GlesRenderer, secondary: &GlesRenderer) -> bool {
    let render = primary
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<FormatSet>();
    negotiate(&render, &secondary.dmabuf_formats(), &CROSS_FORMATS_HDR).is_some()
}

/// The buffers the primary draws into and the damage tracking that goes with them.
pub(super) struct Chain {
    swapchain: Swapchain<GbmAllocator<DeviceFd>>,
    tracker: OutputDamageTracker,
    /// The buffer of the last frame, held so the next one is drawn into another.
    last: Option<Slot<GbmBuffer>>,
}

impl Chain {
    fn new(gbm: &GbmDevice<DeviceFd>, size: (i32, i32), scale: f64, format: Negotiated) -> Self {
        let mut flags = GbmBufferFlags::RENDERING;
        if format.modifiers == [Modifier::Linear] {
            flags |= GbmBufferFlags::LINEAR;
        }
        let allocator = GbmAllocator::new(gbm.clone(), flags);
        Chain {
            swapchain: Swapchain::new(
                allocator,
                size.0 as u32,
                size.1 as u32,
                format.code,
                format.modifiers,
            ),
            tracker: OutputDamageTracker::new(size, Scale::from(scale), Transform::Normal),
            last: None,
        }
    }

    /// The next buffer to draw into and its dmabuf, exported once and kept so imports are cached.
    fn acquire(&mut self) -> Result<(Slot<GbmBuffer>, Dmabuf), String> {
        let slot = self
            .swapchain
            .acquire()
            .map_err(|err| format!("could not allocate a buffer: {err}"))?
            .ok_or_else(|| "every buffer is still in use".to_string())?;
        if slot.userdata().get::<Dmabuf>().is_none() {
            let dmabuf = slot
                .export()
                .map_err(|err| format!("could not export a buffer: {err}"))?;
            slot.userdata().insert_if_missing(|| dmabuf);
        }
        let dmabuf = slot
            .userdata()
            .get::<Dmabuf>()
            .cloned()
            .ok_or_else(|| "the buffer lost its dmabuf".to_string())?;
        Ok((slot, dmabuf))
    }
}

/// How a monitor on a secondary GPU gets its picture.
pub(super) struct Cross {
    size: (i32, i32),
    scale: f64,
    /// The frame crosses as HDR, see the top of this file.
    hdr: bool,
    /// `None` when the setup failed: not tried again until the monitor changes size.
    chain: Option<Chain>,
    /// The working-space texture of an HDR monitor, which is composited into before the final pass.
    hdr_frame: Option<HdrFrame>,
    /// What the secondary shows: the last frame that made it across.
    pub shown: Option<TextureRenderBuffer<GlesTexture>>,
    /// What the primary drew in that frame, for presentation feedback.
    pub states: Option<RenderElementStates>,
    /// The failure of a frame is in the log, the next ones stay quiet until one works.
    warned: bool,
}

impl Cross {
    fn fits(&self, size: (i32, i32), scale: f64, hdr: bool) -> bool {
        self.size == size && self.scale == scale && self.hdr == hdr
    }
}

/// The buffers for a monitor of `size`, in a format both GPUs take.
fn open_chain(
    primary: &GlesRenderer,
    gbm: &GbmDevice<DeviceFd>,
    secondary: Option<&GlesRenderer>,
    size: (i32, i32),
    scale: f64,
    hdr: bool,
    name: &str,
) -> Result<Chain, String> {
    let secondary = secondary.ok_or_else(|| "its GPU has no renderer".to_string())?;
    let render = primary
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect::<FormatSet>();
    let wanted: &[Fourcc] = if hdr {
        &CROSS_FORMATS_HDR
    } else {
        &CROSS_FORMATS
    };
    let format = negotiate(&render, &secondary.dmabuf_formats(), wanted)
        .ok_or_else(|| "the two GPUs share no format a frame can cross in".to_string())?;
    tracing::info!(
        "monitor {name}: frames cross GPUs as {:?}, {} modifier(s)",
        format.code,
        format.modifiers.len()
    );
    Ok(Chain::new(gbm, size, scale, format))
}

impl JffState {
    /// Draw the frame of a monitor on a secondary GPU on the primary and bring it across.
    pub(super) fn update_cross_frame(
        &mut self,
        dev: DeviceId,
        crtc: crtc::Handle,
        output: &Output,
    ) {
        let Some(size) = frame_size(output) else {
            return;
        };
        let scale = output.current_scale().fractional_scale();
        let hdr = match &self.backend {
            Backend::Tty(tty) => tty.hdr_settings(output),
            _ => None,
        };
        let hdr_on = hdr.is_some();
        let Some(mut cross) = self.take_cross(dev, crtc, size, scale, hdr_on) else {
            return;
        };
        match self.cross_draw(output, dev, &mut cross, hdr) {
            Ok(()) => cross.warned = false,
            Err(err) => {
                // the monitor keeps the frame it has
                if !cross.warned {
                    cross.warned = true;
                    tracing::warn!("monitor {}: frame did not cross GPUs: {err}", output.name());
                }
            }
        }
        let peak = cross.hdr_frame.as_ref().and_then(HdrFrame::content_peak);
        if let Backend::Tty(tty) = &mut self.backend {
            if hdr_on {
                tty.set_hdr_content_peak(output, peak);
            }
            if let Some(lit) = tty.outputs.get_mut(&(dev, crtc)) {
                lit.cross = Some(cross);
            }
        }
    }

    /// The monitor's cross-GPU state, made on the first frame and again when its size changes.
    fn take_cross(
        &mut self,
        dev: DeviceId,
        crtc: crtc::Handle,
        size: (i32, i32),
        scale: f64,
        hdr: bool,
    ) -> Option<Cross> {
        let Backend::Tty(tty) = &mut self.backend else {
            return None;
        };
        let Tty {
            renderer,
            primary,
            secondary,
            outputs,
            ..
        } = &mut *tty;
        let lit = outputs.get_mut(&(dev, crtc))?;
        if let Some(cross) = lit.cross.take() {
            if cross.fits(size, scale, hdr) {
                return Some(cross);
            }
        }
        let name = lit.output.name();
        let chain = open_chain(
            renderer,
            &primary.gbm,
            secondary_renderer(secondary, dev).map(|r| &*r),
            size,
            scale,
            hdr,
            &name,
        );
        if let Err(err) = &chain {
            tracing::warn!("monitor {name}: cannot bring frames across GPUs: {err}");
        }
        Some(Cross {
            size,
            scale,
            hdr,
            chain: chain.ok(),
            hdr_frame: None,
            shown: None,
            states: None,
            warned: false,
        })
    }

    fn cross_draw(
        &mut self,
        output: &Output,
        dev: DeviceId,
        cross: &mut Cross,
        hdr: Option<(WorkingSpace, FinalPass, bool)>,
    ) -> Result<(), String> {
        let Some(chain) = cross.chain.as_mut() else {
            return Ok(());
        };
        let (slot, mut dmabuf) = chain.acquire()?;
        let hdr = hdr.map(|(space, pass, content_metadata)| CrossHdr {
            space,
            pass,
            content_metadata,
            frame: &mut cross.hdr_frame,
        });
        let (changed, states) = self.draw_output_for_scanout(
            output,
            &mut dmabuf,
            &mut chain.tracker,
            usize::from(slot.age()),
            hdr,
        )?;
        cross.states = Some(states);
        if !changed {
            return Ok(());
        }
        chain.swapchain.submitted(&slot);
        // the next frame goes to another buffer, this one is being read until the secondary is done
        chain.last = Some(slot);

        let Backend::Tty(tty) = &mut self.backend else {
            return Err("not on the TTY backend".to_string());
        };
        let renderer = secondary_renderer(&mut tty.secondary, dev)
            .ok_or_else(|| "its GPU has no renderer".to_string())?;
        let texture = renderer
            .import_dmabuf(&dmabuf, None)
            .map_err(|err| format!("the secondary GPU cannot import the frame: {err:?}"))?;
        match cross.shown.as_mut() {
            Some(buffer) => {
                buffer.update_from_texture(&*renderer, texture, 1, Transform::Normal, None)
            }
            None => {
                cross.shown = Some(TextureRenderBuffer::from_texture(
                    &*renderer,
                    texture,
                    1,
                    Transform::Normal,
                    None,
                ))
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::Format;

    fn set(formats: &[(Fourcc, Modifier)]) -> FormatSet {
        formats
            .iter()
            .map(|&(code, modifier)| Format { code, modifier })
            .collect()
    }

    #[test]
    fn picks_the_shared_modifiers_of_the_best_format() {
        let tiled = Modifier::from(0x0100_0000_0000_0001u64);
        let render = set(&[
            (Fourcc::Argb8888, Modifier::Linear),
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Xrgb8888, tiled),
        ]);
        let texture = set(&[
            (Fourcc::Xrgb8888, tiled),
            (Fourcc::Xrgb8888, Modifier::Invalid),
        ]);
        assert_eq!(
            negotiate(&render, &texture, &CROSS_FORMATS),
            Some(Negotiated {
                code: Fourcc::Xrgb8888,
                modifiers: vec![tiled]
            })
        );
    }

    #[test]
    fn falls_back_to_the_next_format_when_the_first_has_nothing_shared() {
        let render = set(&[
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Argb8888, Modifier::Linear),
        ]);
        let texture = set(&[(Fourcc::Argb8888, Modifier::Linear)]);
        assert_eq!(
            negotiate(&render, &texture, &CROSS_FORMATS),
            Some(Negotiated {
                code: Fourcc::Argb8888,
                modifiers: vec![Modifier::Linear]
            })
        );
    }

    #[test]
    fn falls_back_to_linear_when_no_modifier_is_shared() {
        let tiled = Modifier::from(0x0100_0000_0000_0001u64);
        let render = set(&[
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Xrgb8888, tiled),
        ]);
        let texture = set(&[(Fourcc::Xrgb8888, Modifier::Invalid)]);
        assert_eq!(
            negotiate(&render, &texture, &CROSS_FORMATS),
            Some(Negotiated {
                code: Fourcc::Xrgb8888,
                modifiers: vec![Modifier::Linear]
            })
        );
    }

    #[test]
    fn an_hdr_frame_crosses_in_ten_bits() {
        let render = set(&[
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Argb2101010, Modifier::Linear),
            (Fourcc::Abgr2101010, Modifier::Linear),
        ]);
        let texture = set(&[
            (Fourcc::Xrgb8888, Modifier::Linear),
            (Fourcc::Abgr2101010, Modifier::Linear),
        ]);
        assert_eq!(
            negotiate(&render, &texture, &CROSS_FORMATS_HDR),
            Some(Negotiated {
                code: Fourcc::Abgr2101010,
                modifiers: vec![Modifier::Linear]
            })
        );
        // 8 bits would band under PQ, so a pair that only shares those has no HDR crossing
        let eight = set(&[(Fourcc::Xrgb8888, Modifier::Linear)]);
        assert_eq!(negotiate(&eight, &eight, &CROSS_FORMATS_HDR), None);
    }

    #[test]
    fn gives_up_without_linear_or_a_shared_format() {
        let tiled = Modifier::from(0x0100_0000_0000_0001u64);
        let render = set(&[(Fourcc::Xrgb8888, tiled)]);
        let texture = set(&[(Fourcc::Xrgb8888, Modifier::Invalid)]);
        assert_eq!(negotiate(&render, &texture, &CROSS_FORMATS), None);
        // only formats that are not a picture's
        let render = set(&[(Fourcc::Abgr16161616f, Modifier::Linear)]);
        let texture = set(&[(Fourcc::Abgr16161616f, Modifier::Linear)]);
        assert_eq!(negotiate(&render, &texture, &CROSS_FORMATS), None);
    }
}

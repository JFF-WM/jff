//! The element type everything is drawn with: cursor, windows, overview thumbnails, solid rects
//! and blurred backgrounds. GLES only, because the blur element needs `GlesRenderer`.

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{RelocateRenderElement, RescaleRenderElement};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::GlesRenderer;

use crate::render::effects::border::BorderElement;
use crate::render::effects::framebuffer_effect::FramebufferEffectElement;
use crate::render::effects::hdr::HdrFrameElement;
use crate::render::effects::rounded::{
    RoundedMemoryElement, RoundedScaledMemoryElement, RoundedSurfaceElement, RoundedThumbElement,
};

render_elements! {
    /// Everything drawn (TTY and winit): the cursor, then windows.
    pub JffElement<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Memory = MemoryRenderBufferRenderElement<GlesRenderer>,
    /// A painted picture (an overview app icon) drawn at another size than it was painted at.
    ScaledMemory = RescaleRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>,
    /// A window shrunk and moved into its slot in the overview.
    Thumb = RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    /// A plain colored rectangle (the overview's highlight).
    Solid = SolidColorRenderElement,
    /// Blurred background behind a surface (GLES only, see `render/effects`).
    Effect = FramebufferEffectElement,
    /// A window surface with rounded corners (the `radius` setting).
    Rounded = RoundedSurfaceElement,
    /// An overview thumbnail with rounded corners.
    RoundedThumb = RoundedThumbElement,
    /// The rounded outline around the selected overview thumbnail.
    Ring = PixelShaderElement,
    /// A window's border: a ring that keeps its id while it changes colour or turns.
    Border = BorderElement,
    LinearMemory = RoundedMemoryElement,
    LinearScaledMemory = RoundedScaledMemoryElement,
    /// The working-space texture of an HDR output, drawn through the final pass.
    Hdr = HdrFrameElement,
}

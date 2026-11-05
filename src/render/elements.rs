//! The element type everything is drawn with (cursor, windows, overview thumbnails, solid rects).

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::render_elements;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{RelocateRenderElement, RescaleRenderElement};
use smithay::backend::renderer::{ImportAll, ImportMem};

render_elements! {
    /// Everything drawn (TTY and winit): the cursor, then windows.
    pub JffElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
    /// A window shrunk and moved into its slot in the overview.
    Thumb = RelocateRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>,
    /// A plain colored rectangle (the overview's highlight).
    Solid = SolidColorRenderElement,
}

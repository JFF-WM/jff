//! `ext-image-copy-capture-v1` (with `ext-image-capture-source-v1` for outputs): the standard
//! way for screen recorders, screen sharers, and screenshot tools (wayvnc, grim, OBS,
//! xdg-desktop-portal-wlr successors, ...) to read a monitor's picture.
//!
//! Smithay does the protocol, this file says what a capture is.
//!
//! Sources (`ext_image_capture_source_v1`): a client can capture a monitor, or a single window
//! it got from `ext-foreign-toplevel-list` (see `ext_foreign_toplevel.rs`). A window source
//! stands for the window whose identifier is `jff-<window id>`. Its picture is the window alone on
//! a transparent background, same as `JffState::capture_window` in `screenshot.rs`, wherever
//! the window is (other workspace, covered by another window). The session ends when the
//! window closes.
//!
//! Pictures: a frame is a render of the monitor (or window), the same as
//! `JffState::capture_output` in `screenshot.rs`, so a capture shows exactly what a screenshot
//! shows (no overview). Buffers offered: `Argb8888` and `Xrgb8888`, and for a monitor in HDR mode
//! also the 10-bit formats (`HDR_FORMATS`), listed after the 8-bit ones. Into a `wl_shm` buffer
//! the render is read back to the CPU and written. Monitors on the TTY backend also offer dmabuf
//! (`Argb8888` and `Xrgb8888` in the modifiers the renderer can draw into, and for a monitor in
//! HDR mode `Argb2101010` and `Abgr2101010` as well, where it can): a client that
//! allocates its buffer that way gets the monitor drawn straight into it, with no readback or
//! copy. That picture is always whole: the damage sent with it is all of it, and a frame is
//! still only drawn when the monitor drew since the last one. Windows and the winit backend stay
//! on `wl_shm`.
//!
//! HDR: the 8-bit formats always get the sRGB picture a screenshot file has, so a client that
//! knows nothing about HDR is never handed a washed-out one. A client that asks for a 10-bit
//! format on a monitor in HDR mode gets what the monitor is sent and what an HDR screenshot
//! file holds: PQ codes in BT.2020, with the tone curve and gamut mapping of the screen. The
//! protocol has no way to say so, a client picking these formats has to know. Windows are
//! always 8-bit: their picture has an alpha channel that 2 bits don't carry.
//!
//! Damage: the protocol wants a frame only when something changed, or a client asking for the
//! next frame right away would spin. The first frame of a session goes out at once. A later
//! frame stays pending, without drawing anything, until the source may have changed: a monitor
//! when a frame was sent to it (`ImageCopy::output_drawn`), a window when any of its surfaces
//! (or its menus) committed. Only then is the picture drawn and compared with the last one: the
//! same picture keeps the frame pending, a different one goes out with the box around the
//! pixels that changed as its damage.
//!
//! Cursor: `paint_cursors` is honored for monitors on the TTY backend (where jff draws the
//! cursor), a window picture never has one. A client that wants the cursor apart from the
//! picture (a portal's "metadata" cursor mode: OBS and browsers draw or hide it themselves)
//! uses a cursor capture session (`create_pointer_cursor_session`) on the same source:
//!
//! - `enter` / `leave` / `position` / `hotspot` follow the pointer. The cursor is on the source
//!   while its picture touches it, positions are in the pixels of the source's picture. Hidden
//!   (typing, resting, a client that hides it) and a locked session count as `leave`.
//! - Its capture session hands out the cursor's own picture (`Argb8888`, `wl_shm` only,
//!   premultiplied), at the scale of the monitor the source is on. A frame is only answered
//!   when the picture is a different one (another icon, an animation frame, a commit of the
//!   client's cursor surface). A different size is new buffer constraints, like a mode change.
//!   While there is no picture (hidden) a frame stays pending.
//! - Only the seat's pointer can be captured. Animated cursors only advance when the loop wakes
//!   for something else (a frame sent to the monitor, input), as nothing schedules a wake-up.
//!
//! Sessions end when the monitor goes away or is switched off. A mode change (or a window
//! resize) sends new buffer constraints and fails the frame in flight with `buffer_constraints`.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::backend::drm::DrmNode;
use smithay::backend::renderer::utils::{with_renderer_surface_state, CommitCounter};
use smithay::desktop::{PopupManager, Window};
use smithay::input::pointer::{CursorIcon, PointerHandle};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Buffer as BufferCoords, IsAlive, Rectangle, Size, Transform};
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
    OutputCaptureSourceHandler, OutputCaptureSourceState, ToplevelCaptureSourceHandler,
    ToplevelCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, CursorSession, CursorSessionRef, DmabufConstraints,
    Frame, FrameRef, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut};

use std::sync::Arc;

use crate::app::Backend;
use crate::feature::outputs::{frame_size, output_rect};
use crate::feature::screenshot::{Captured, HdrCaptured};
use crate::ipc::WindowId;
use crate::primitives::capture_color::frame_description_fits;
use crate::primitives::cursor_capture::hotspot_in_source;
use crate::primitives::hdr_capture::{pack_row_2101010, pq_picture};
use crate::protocol::color_management::Described;
use crate::protocol::presentation::monotonic_now;
use crate::render::cursor::{CursorLook, RenderCursor};
use crate::state::{is_sandboxed, JffState};

/// One capture session and what jff keeps for it.
pub struct CaptureSession {
    /// Dropping it tells the client the session is over.
    session: Session,
    /// The last frame sent, to tell whether the screen changed.
    last: Option<Last>,
    /// The frame the client is waiting for.
    pending: Option<Frame>,
    /// What the source was at when `last` was made: while it is the same, nothing is drawn.
    seen: Option<Seen>,
}

/// One cursor capture session and what jff keeps for it.
pub struct CursorCapture {
    /// Dropping it tells the client the session is over.
    session: CursorSession,
    /// The frame the client is waiting for.
    pending: Option<Frame>,
    /// What the cursor picture was like when the last frame was sent.
    last: Option<CursorLook>,
}

/// A cheap stand-in for "the source may look different now", compared for equality.
#[derive(PartialEq)]
enum Seen {
    /// How many frames were sent to the monitor.
    Output(u64),
    /// The commit counters of the window's surfaces and those of its menus.
    Window(Vec<CommitCounter>),
}

/// The pixels of a picture that differ from the last one, as a box.
enum Damage {
    Same,
    Part(Rectangle<i32, BufferCoords>),
}

/// The box around the pixels (four values each, `width` pixels a row) where `now` and `last`
/// differ. A `last` of another size counts as all of it.
fn damage_between<T: PartialEq>(now: &[T], last: &[T], width: usize) -> Damage {
    let row = width * 4;
    if row == 0 || now.len() != last.len() || now.len() % row != 0 {
        let size: Size<i32, BufferCoords> =
            ((width as i32).max(1), (now.len() / row.max(4)) as i32).into();
        return Damage::Part(Rectangle::from_size(size));
    }
    let (mut top, mut bottom, mut left, mut right) = (None, 0, width, 0);
    for (y, (a, b)) in now
        .chunks_exact(row)
        .zip(last.chunks_exact(row))
        .enumerate()
    {
        if a == b {
            continue;
        }
        top.get_or_insert(y);
        bottom = y;
        let pairs = || a.chunks_exact(4).zip(b.chunks_exact(4));
        if let Some(first) = pairs().position(|(p, q)| p != q) {
            left = left.min(first);
        }
        if let Some(end) = pairs().rposition(|(p, q)| p != q) {
            right = right.max(end);
        }
    }
    match top {
        None => Damage::Same,
        Some(top) => Damage::Part(Rectangle::new(
            (left as i32, top as i32).into(),
            ((right - left + 1) as i32, (bottom - top + 1) as i32).into(),
        )),
    }
}

/// The commit counters of `window`'s surfaces and those of its menus.
fn window_signature(window: &Window) -> Vec<CommitCounter> {
    let Some(toplevel) = window.toplevel() else {
        return Vec::new();
    };
    let mut roots = vec![toplevel.wl_surface().clone()];
    roots.extend(
        PopupManager::popups_for_surface(toplevel.wl_surface())
            .map(|(popup, _)| popup.wl_surface().clone()),
    );
    // `with_surface_tree_downward` holds the surface's data lock while it calls back, and
    // `with_renderer_surface_state` takes that same lock: asking for the counter from inside
    // the callback deadlocks the whole compositor. So only collect the surfaces there, and
    // read the counters after the traversal is over.
    let mut surfaces = Vec::new();
    for root in &roots {
        with_surface_tree_downward(
            root,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, _, _| surfaces.push(surface.clone()),
            |_, _, _| true,
        );
    }
    surfaces
        .iter()
        .map(|surface| {
            with_renderer_surface_state(surface, |state| state.current_commit()).unwrap_or_default()
        })
        .collect()
}

/// What the last frame of a session looked like.
enum Last {
    /// Pixels (RGBA) of an 8-bit picture.
    Sdr(Vec<u8>),
    /// The half floats of the working space of an HDR picture.
    Hdr(Vec<u16>),
    /// A picture drawn straight into a client's dmabuf: there are no pixels to compare with.
    Dmabuf,
}

/// A picture on its way into a client's buffer.
enum Picture {
    Sdr(Captured),
    Hdr {
        picture: HdrCaptured,
        /// Red is in the high bits of the buffer's pixels (`Xrgb2101010`), not blue.
        red_high: bool,
    },
}

/// The formats a client can pick for a monitor in HDR mode, besides the 8-bit ones. 10 bits a
/// color, PQ coded. The alpha of the `A` forms is always opaque.
const HDR_FORMATS: [wl_shm::Format; 4] = [
    wl_shm::Format::Xrgb2101010,
    wl_shm::Format::Xbgr2101010,
    wl_shm::Format::Argb2101010,
    wl_shm::Format::Abgr2101010,
];

/// The dmabuf formats a client can pick for a monitor in HDR mode, besides the 8-bit ones: the
/// same PQ coded 10 bits as `HDR_FORMATS`. Only the two with alpha are drawable by the GPU.
const HDR_DMA_CODES: [Fourcc; 2] = [Fourcc::Argb2101010, Fourcc::Abgr2101010];

/// Whether `format` is one of `HDR_FORMATS`, and if so whether it has red in the high bits.
fn hdr_layout(format: wl_shm::Format) -> Option<bool> {
    match format {
        wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => Some(true),
        wl_shm::Format::Xbgr2101010 | wl_shm::Format::Abgr2101010 => Some(false),
        _ => None,
    }
}

/// The format of an shm buffer, `None` for another kind of buffer.
fn buffer_format(buffer: &WlBuffer) -> Option<wl_shm::Format> {
    with_buffer_contents(buffer, |_, _, data| data.format).ok()
}

/// Everything the protocols need to keep between requests.
pub struct ImageCopy {
    /// Only has to stay alive: it handles the `ext_image_capture_source_v1` objects.
    #[allow(dead_code)]
    image_capture_source: ImageCaptureSourceState,
    output_capture_source: OutputCaptureSourceState,
    toplevel_capture_source: ToplevelCaptureSourceState,
    image_copy_capture: ImageCopyCaptureState,
    sessions: Vec<CaptureSession>,
    cursor_sessions: Vec<CursorCapture>,
    /// How many frames were sent to each monitor (by name).
    drawn: Vec<(String, u64)>,
    /// The `HDR_FORMATS` that `wl_shm` offers: a client can't make a buffer in any other.
    hdr_formats: Vec<wl_shm::Format>,
    /// The render node of the GPU, and the formats (with their modifiers) a client can allocate
    /// a capture buffer in. `None` and empty while dmabuf is not offered.
    dma_node: Option<DrmNode>,
    dma_formats: Vec<(Fourcc, Vec<Modifier>)>,
    /// The 10-bit dmabuf formats, offered on top of `dma_formats` for a monitor in HDR mode.
    hdr_dma_formats: Vec<(Fourcc, Vec<Modifier>)>,
}

impl ImageCopy {
    /// Create the protocol globals. All three are hidden from sandboxed clients: together they
    /// read the picture of any monitor or window.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            image_capture_source: ImageCaptureSourceState::new(),
            output_capture_source: OutputCaptureSourceState::new_with_filter::<JffState, _>(
                display,
                |client| !is_sandboxed(client),
            ),
            toplevel_capture_source: ToplevelCaptureSourceState::new_with_filter::<JffState, _>(
                display,
                |client| !is_sandboxed(client),
            ),
            image_copy_capture: ImageCopyCaptureState::new_with_filter::<JffState, _>(
                display,
                |client| !is_sandboxed(client),
            ),
            sessions: Vec::new(),
            cursor_sessions: Vec::new(),
            drawn: Vec::new(),
            hdr_formats: Vec::new(),
            dma_node: None,
            dma_formats: Vec::new(),
            hdr_dma_formats: Vec::new(),
        }
    }

    /// The fourcc codes of the dmabuf formats offered for monitor captures, empty when dmabuf
    /// is not offered (`wlr_screencopy.rs` announces them too).
    pub(crate) fn dmabuf_codes(&self) -> Vec<u32> {
        if self.dma_node.is_none() {
            return Vec::new();
        }
        self.dma_formats
            .iter()
            .map(|(code, _)| *code as u32)
            .collect()
    }

    /// A frame was sent to `output`: what a capture of it shows may have changed.
    pub fn output_drawn(&mut self, output: &Output) {
        if self.sessions.is_empty() {
            return;
        }
        let name = output.name();
        match self.drawn.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => self.drawn.push((name, 1)),
        }
    }

    /// Frames sent to the monitor called `name` so far.
    fn drawn_count(&self, name: &str) -> u64 {
        self.drawn
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, count)| *count)
    }

    /// Tell which formats `wl_shm` offers, to pick what a monitor in HDR mode can be captured
    /// in from them. Without any of the 10-bit ones, HDR monitors are captured in 8 bits.
    pub fn offer_hdr_formats(&mut self, offered: &[wl_shm::Format]) {
        self.hdr_formats = HDR_FORMATS
            .into_iter()
            .filter(|format| offered.contains(format))
            .collect();
    }
}

/// What a capture session is of.
enum Target {
    Output(Output),
    Window(Window),
}

/// Where a capture source is in the shared space and how big its picture is.
struct SourceSpace {
    /// Pixels per logical pixel.
    scale: f64,
    /// Top-left corner, in logical pixels.
    origin: (i32, i32),
    /// Size of the picture, in pixels.
    size: (i32, i32),
}

/// Set on a source made for a window: the window id (see `WindowId`).
struct ToplevelSourceId(u64);

/// What a client has to allocate for pictures of this `size`: the two 8-bit formats, then
/// `extra`, in `wl_shm`, and in a dmabuf when `dma` says so.
fn buffer_constraints(
    size: (i32, i32),
    extra: &[wl_shm::Format],
    dma: Option<DmabufConstraints>,
) -> BufferConstraints {
    let mut shm = vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888];
    shm.extend_from_slice(extra);
    BufferConstraints {
        size: size.into(),
        shm,
        dma,
    }
}

/// Fill the rows of a `width` by `height` picture in shm buffer `buffer`: `fill` gets the
/// number of each row and its four bytes a pixel to write.
fn write_rows(
    buffer: &WlBuffer,
    width: usize,
    height: usize,
    mut fill: impl FnMut(usize, &mut [u8]),
) -> Result<(), String> {
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        if data.width < width as i32 || data.height < height as i32 {
            return Err("the buffer is smaller than the picture".to_string());
        }
        let (offset, stride) = (data.offset.max(0) as usize, data.stride.max(0) as usize);
        if stride < width * 4 || offset + stride * (height - 1) + width * 4 > len {
            return Err("the buffer is too small for its own stride".to_string());
        }
        // SAFETY: smithay guarantees `ptr` points at `len` bytes of the pool for the duration
        // of the closure.
        let pool = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
        for y in 0..height {
            fill(
                y,
                &mut pool[offset + y * stride..offset + y * stride + width * 4],
            );
        }
        Ok(())
    })
    .map_err(|err| format!("could not access the buffer: {err:?}"))?
}

/// Write `picture` (RGBA) into shm buffer `buffer`, as `Argb8888`/`Xrgb8888` (which in memory
/// are B, G, R, A on the little-endian machines jff runs on).
pub(crate) fn write_shm(buffer: &WlBuffer, picture: &Captured) -> Result<(), String> {
    let (width, height) = (picture.width as usize, picture.height as usize);
    write_rows(buffer, width, height, |y, row| {
        let source = &picture.rgba[y * width * 4..(y + 1) * width * 4];
        for (out, px) in row.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
            out[0] = px[2];
            out[1] = px[1];
            out[2] = px[0];
            out[3] = px[3];
        }
    })
}

/// Write an HDR `picture` into shm buffer `buffer` as PQ codes, 10 bits a color. `red_high`
/// says which of `Xrgb2101010` and `Xbgr2101010` (or their alpha forms) the buffer is.
fn write_hdr_shm(buffer: &WlBuffer, picture: &HdrCaptured, red_high: bool) -> Result<(), String> {
    let (width, height) = (picture.width as usize, picture.height as usize);
    // The slow part, done before the client's memory is touched.
    let coded = pq_picture(&picture.pass, &picture.half_rgba);
    write_rows(buffer, width, height, |y, row| {
        pack_row_2101010(
            &coded.codes[y * width * 3..(y + 1) * width * 3],
            red_high,
            row,
        );
    })
}

/// Whether a picture that is HDR (`hdr_picture`) or not can be said to be what `wanted` describes
/// (`xx-image-capture-color-management-v1`). A frame nobody described always can.
fn description_fits(wanted: Option<&Described>, hdr_picture: bool) -> bool {
    wanted.map_or(true, |wanted| {
        frame_description_fits(&wanted.description, hdr_picture)
    })
}

impl JffState {
    /// The preferred description of a capture of `source` (`xx-image-capture-color-management-v1`),
    /// `None` when the monitor or window is gone.
    pub(crate) fn capture_source_preferred(
        &self,
        source: &ImageCaptureSource,
    ) -> Option<Arc<Described>> {
        let name = match self.source_target(source)? {
            Target::Output(output) => Some(output.name()),
            Target::Window(_) => None,
        };
        Some(self.color_management.capture_preferred(name.as_deref()))
    }

    /// The monitor or window a capture source stands for, if it's still there.
    fn source_target(&self, source: &ImageCaptureSource) -> Option<Target> {
        if let Some(weak) = source.user_data().get::<WeakOutput>() {
            let output = weak.upgrade()?;
            return self
                .outputs
                .iter()
                .find(|o| **o == output)
                .cloned()
                .map(Target::Output);
        }
        let id = source.user_data().get::<ToplevelSourceId>()?.0;
        self.desk
            .find_by(|w| w.user_data().get::<WindowId>() == Some(&WindowId(id)))
            .map(|entry| Target::Window(entry.window.clone()))
    }

    /// Find out which GPU a capture buffer can be allocated for and in which formats, so that
    /// monitor captures can offer dmabuf. Only the TTY backend has a render node. The formats
    /// are the 8-bit ones the renderer can draw into, and the 10-bit ones for monitors in HDR mode.
    pub fn offer_capture_dmabuf(&mut self) {
        let Backend::Tty(tty) = &self.backend else {
            return;
        };
        let Some(node) = tty.render_node() else {
            return;
        };
        let render_formats = tty.renderer.egl_context().dmabuf_render_formats();
        let drawable = |codes: &[Fourcc]| {
            let mut formats: Vec<(Fourcc, Vec<Modifier>)> = Vec::new();
            for &code in codes {
                let modifiers: Vec<Modifier> = render_formats
                    .iter()
                    .filter(|format| format.code == code)
                    .map(|format| format.modifier)
                    .collect();
                if !modifiers.is_empty() {
                    formats.push((code, modifiers));
                }
            }
            formats
        };
        let formats = drawable(&[Fourcc::Argb8888, Fourcc::Xrgb8888]);
        let hdr_formats = drawable(&HDR_DMA_CODES);
        if formats.is_empty() {
            tracing::info!("image-copy-capture: the renderer can draw into no dmabuf format, capture stays on wl_shm");
            return;
        }
        tracing::info!(
            "image-copy-capture: dmabuf offered for monitors, {} format(s), {} more in HDR mode",
            formats.len(),
            hdr_formats.len()
        );
        self.image_copy.dma_node = Some(node);
        self.image_copy.dma_formats = formats;
        self.image_copy.hdr_dma_formats = hdr_formats;
    }

    /// The dmabuf part of the constraints of a monitor capture, `None` when it is not offered.
    /// `hdr` is whether the monitor is shown as HDR, which adds the 10-bit formats.
    fn dmabuf_constraints(&self, hdr: bool) -> Option<DmabufConstraints> {
        let node = self.image_copy.dma_node?;
        if self.image_copy.dma_formats.is_empty() {
            return None;
        }
        let mut formats = self.image_copy.dma_formats.clone();
        if hdr {
            formats.extend(self.image_copy.hdr_dma_formats.iter().cloned());
        }
        Some(DmabufConstraints { node, formats })
    }

    /// Answer `frame` with a picture of the monitor drawn into the client's `dmabuf`.
    fn service_dmabuf_frame(
        &mut self,
        entry: &mut CaptureSession,
        target: &Target,
        frame: Frame,
        wanted: Option<Arc<Described>>,
        mut dmabuf: Dmabuf,
    ) -> bool {
        let Target::Output(output) = target else {
            frame.fail(CaptureFailureReason::BufferConstraints);
            return true;
        };
        let code = dmabuf.format().code;
        let ten_bit = HDR_DMA_CODES.contains(&code);
        // The 10-bit formats are only on offer while the monitor is shown as HDR: otherwise
        // there is no PQ picture to give.
        let allowed = matches!(code, Fourcc::Argb8888 | Fourcc::Xrgb8888)
            || (ten_bit && self.hdr_capture(output).is_some());
        if !allowed {
            frame.fail(CaptureFailureReason::BufferConstraints);
            return true;
        }
        if !description_fits(wanted.as_deref(), ten_bit) {
            tracing::warn!(
                "image-copy-capture: the frame's image description is not what it is drawn as"
            );
            frame.fail(CaptureFailureReason::Unknown);
            return true;
        }
        let seen = Seen::Output(self.image_copy.drawn_count(&output.name()));
        // Nothing is drawn while the monitor can't have changed since the last picture.
        if matches!(entry.last, Some(Last::Dmabuf)) && entry.seen.as_ref() == Some(&seen) {
            entry.pending = Some(frame);
            return true;
        }
        let cursor = entry.session.draw_cursor();
        let drawn = if ten_bit {
            self.capture_output_hdr_into(output, cursor, &mut dmabuf)
        } else {
            self.capture_output_into(output, cursor, &mut dmabuf)
        };
        match drawn {
            Ok(size) => {
                entry.seen = Some(seen);
                entry.last = Some(Last::Dmabuf);
                frame.success(
                    Transform::Normal,
                    vec![Rectangle::from_size(size)],
                    monotonic_now(),
                );
            }
            Err(why) => {
                tracing::warn!("image-copy-capture: {why}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
        true
    }

    /// Buffer constraints for `target`, or `None` when there is nothing to show (a monitor
    /// that is off, a window with no size yet).
    fn target_constraints(&self, target: &Target) -> Option<BufferConstraints> {
        match target {
            Target::Output(output) => {
                // The monitor's picture is drawn upright, so a turned monitor is as tall as it is
                // wide the other way round.
                let size = frame_size(output)?;
                // The 10-bit formats only while the monitor is shown as HDR: otherwise there
                // is no PQ picture to give.
                let hdr = self.hdr_capture(output).is_some();
                let extra = if hdr {
                    &self.image_copy.hdr_formats[..]
                } else {
                    &[]
                };
                Some(buffer_constraints(
                    size,
                    extra,
                    self.dmabuf_constraints(hdr),
                ))
            }
            Target::Window(window) => {
                let (_, size) = self.window_capture_size(window).ok()?;
                Some(buffer_constraints((size.w, size.h), &[], None))
            }
        }
    }

    /// Answer the frames clients wait for. Runs when a frame is asked for and after every
    /// event-loop iteration, does nothing while there are no sessions.
    pub fn service_image_copy(&mut self) {
        if !self.image_copy.sessions.is_empty() {
            let mut sessions = std::mem::take(&mut self.image_copy.sessions);
            sessions.retain_mut(|entry| self.service_session(entry));
            self.image_copy.sessions = sessions;
        }
        if !self.image_copy.cursor_sessions.is_empty() {
            let mut sessions = std::mem::take(&mut self.image_copy.cursor_sessions);
            sessions.retain_mut(|entry| self.service_cursor_session(entry));
            self.image_copy.cursor_sessions = sessions;
        }
    }

    /// Where `target` is, and how big its picture is: scale, top-left corner in the shared
    /// space (the pointer's space) and size in pixels. `None` when there is no picture.
    fn source_space(&self, target: &Target) -> Option<SourceSpace> {
        match target {
            Target::Output(output) => {
                let rect = output_rect(output);
                Some(SourceSpace {
                    scale: output.current_scale().fractional_scale(),
                    origin: (rect.0, rect.1),
                    size: frame_size(output)?,
                })
            }
            Target::Window(window) => {
                let (scale, size) = self.window_capture_size(window).ok()?;
                // `pos` is the corner of the window's geometry, which is what a picture of it
                // starts at.
                let pos = self.desk.pos_of(|w| w == window)?;
                Some(SourceSpace {
                    scale,
                    origin: (pos.0, pos.1),
                    size: (size.w, size.h),
                })
            }
        }
    }

    /// What the cursor is now: the picture to show (`None`: nothing, the session is locked or the
    /// cursor hidden) and its look on a source of `scale`.
    fn cursor_now(&mut self, scale: f64) -> (RenderCursor, Option<CursorLook>) {
        let cursor = if self.session_locked() {
            RenderCursor::Hidden
        } else {
            self.cursor.render_cursor()
        };
        let look = cursor.look(self.start_time.elapsed().as_millis() as u32, scale);
        (cursor, look)
    }

    /// The constraints of a cursor session on a source of `scale`: one `Argb8888` picture, as big
    /// as the cursor. While there is no picture to size it by, the size it has (`current`) or
    /// the theme's default arrow.
    fn cursor_constraints(
        &mut self,
        scale: f64,
        look: Option<&CursorLook>,
        current: Option<(i32, i32)>,
    ) -> BufferConstraints {
        let size = look.map(|look| look.size).or(current).unwrap_or_else(|| {
            let millis = self.start_time.elapsed().as_millis() as u32;
            RenderCursor::Named(self.cursor.themed(CursorIcon::Default))
                .look(millis, scale)
                .map_or((24, 24), |look| look.size)
        });
        BufferConstraints {
            size: size.into(),
            // Alpha is the point of a cursor picture.
            shm: vec![wl_shm::Format::Argb8888],
            dma: None,
        }
    }

    /// Look after one cursor session: tell the client where the cursor is, keep the constraints
    /// the size of its picture, answer the frame it waits for. Returns false when the session is
    /// over.
    fn service_cursor_session(&mut self, entry: &mut CursorCapture) -> bool {
        if !entry.session.alive() {
            return false;
        }
        let Some(target) = self.source_target(&entry.session.source()) else {
            return false;
        };
        let Some(space) = self.source_space(&target) else {
            return false;
        };
        let (cursor, look) = self.cursor_now(space.scale);

        // Where the cursor is. The hotspot first: `enter` carries it.
        let position = look.as_ref().and_then(|look| {
            let pointer = self.seat.get_pointer()?.current_location();
            hotspot_in_source(
                (
                    pointer.x - f64::from(space.origin.0),
                    pointer.y - f64::from(space.origin.1),
                ),
                space.scale,
                look.hotspot,
                look.size,
                space.size,
            )
        });
        if let Some(look) = &look {
            entry.session.set_cursor_hotspot(look.hotspot);
        }
        entry.session.set_cursor_pos(position.map(Into::into));

        // The picture changed size: clients have to allocate new buffers.
        let current = entry
            .session
            .current_constraints()
            .map(|c| (c.size.w, c.size.h));
        let wanted = self.cursor_constraints(space.scale, look.as_ref(), current);
        let constraints_ok = entry
            .session
            .current_constraints()
            .is_some_and(|c| c.size == wanted.size && c.shm == wanted.shm);
        if !constraints_ok {
            entry.session.update_constraints(wanted);
            entry.last = None;
            if let Some(frame) = entry.pending.take() {
                frame.fail(CaptureFailureReason::BufferConstraints);
            }
            return true;
        }

        // No picture to show: the frame stays pending until there is one.
        let Some(look) = look else {
            return true;
        };
        let Some(frame) = entry.pending.take() else {
            return true;
        };
        // Nothing changed since the last frame: the client keeps waiting.
        if entry.last.as_ref() == Some(&look) {
            entry.pending = Some(frame);
            return true;
        }
        let buffer = frame.buffer();
        // Only `Argb8888` in `wl_shm` was offered.
        if get_dmabuf(&buffer).is_ok() || buffer_format(&buffer) != Some(wl_shm::Format::Argb8888) {
            frame.fail(CaptureFailureReason::BufferConstraints);
            return true;
        }
        // Nothing converts a picture to another description, and a cursor is sRGB.
        let wanted = self.xx_capture_color.description_of(&frame);
        if !description_fits(wanted.as_deref(), false) {
            tracing::warn!(
                "image-copy-capture: the cursor frame's image description is not what it is drawn as"
            );
            frame.fail(CaptureFailureReason::Unknown);
            return true;
        }
        let millis = self.start_time.elapsed().as_millis() as u32;
        let written = self
            .capture_cursor_image(cursor, &look, space.scale, millis)
            .and_then(|picture| write_shm(&buffer, &picture));
        match written {
            Ok(()) => {
                let size: Size<i32, BufferCoords> = look.size.into();
                frame.success(
                    Transform::Normal,
                    vec![Rectangle::from_size(size)],
                    monotonic_now(),
                );
                entry.last = Some(look);
            }
            Err(why) => {
                tracing::warn!("image-copy-capture: cursor: {why}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
        true
    }

    /// Look after one session. Returns false when the session is over (it's then dropped, which
    /// tells the client).
    fn service_session(&mut self, entry: &mut CaptureSession) -> bool {
        if !entry.session.alive() {
            return false;
        }
        let Some(target) = self.source_target(&entry.session.source()) else {
            return false;
        };
        let Some(wanted) = self.target_constraints(&target) else {
            return false;
        };

        // The monitor changed mode or the window its size, or the monitor went into or out of HDR
        // mode (which adds or takes away formats): clients have to allocate new buffers.
        let constraints_ok = entry.session.current_constraints().is_some_and(|c| {
            c.size == wanted.size
                && c.shm == wanted.shm
                && c.dma.as_ref().map(|dma| &dma.formats)
                    == wanted.dma.as_ref().map(|dma| &dma.formats)
        });
        if !constraints_ok {
            entry.session.update_constraints(wanted);
            entry.last = None;
            if let Some(frame) = entry.pending.take() {
                frame.fail(CaptureFailureReason::BufferConstraints);
            }
            return true;
        }

        let Some(frame) = entry.pending.take() else {
            return true;
        };
        let buffer = frame.buffer();
        // What the client said its frame is (`xx-image-capture-color-management-v1`).
        let wanted = self.xx_capture_color.description_of(&frame);
        if let Some(dmabuf) = get_dmabuf(&buffer).ok().cloned() {
            return self.service_dmabuf_frame(entry, &target, frame, wanted, dmabuf);
        }
        let format = buffer_format(&buffer);
        let ten_bit = format.and_then(hdr_layout);
        let cursor = entry.session.draw_cursor();
        // Nothing is drawn while the source can't have changed since the last picture.
        let seen = match &target {
            Target::Output(output) => Seen::Output(self.image_copy.drawn_count(&output.name())),
            Target::Window(window) => Seen::Window(window_signature(window)),
        };
        let bad_format = match (&target, ten_bit) {
            (Target::Output(output), Some(_)) => self.hdr_capture(output).is_none(),
            (_, Some(_)) => true,
            (_, None) => format
                .is_some_and(|f| !matches!(f, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)),
        };
        // Nothing converts a picture to another description: the one the client asked for has to
        // be what the buffer's format gets drawn as.
        if !bad_format && !description_fits(wanted.as_deref(), ten_bit.is_some()) {
            tracing::warn!(
                "image-copy-capture: the frame's image description is not what it is drawn as"
            );
            frame.fail(CaptureFailureReason::Unknown);
            return true;
        }
        let shm_last = matches!(entry.last, Some(Last::Sdr(_) | Last::Hdr(_)));
        if !bad_format && shm_last && entry.seen.as_ref() == Some(&seen) {
            entry.pending = Some(frame);
            return true;
        }
        let drawn = match (&target, ten_bit) {
            // The client asked for a 10-bit format: only a monitor in HDR mode has such a picture.
            (Target::Output(output), Some(red_high)) if self.hdr_capture(output).is_some() => self
                .capture_output_hdr(output, cursor)
                .map(|picture| Picture::Hdr { picture, red_high }),
            (_, Some(_)) => {
                frame.fail(CaptureFailureReason::BufferConstraints);
                return true;
            }
            // Any other format than the 8-bit ones is not what the session offered, the
            // picture would come out as noise.
            (_, None)
                if format.is_some_and(|f| {
                    !matches!(f, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)
                }) =>
            {
                frame.fail(CaptureFailureReason::BufferConstraints);
                return true;
            }
            (Target::Output(output), None) => self.capture_output(output, cursor).map(Picture::Sdr),
            (Target::Window(window), None) => self.capture_window(window).map(Picture::Sdr),
        };
        let picture = match drawn {
            Ok(picture) => picture,
            Err(why) => {
                tracing::warn!("image-copy-capture: {why}");
                frame.fail(CaptureFailureReason::Unknown);
                return true;
            }
        };

        // Looked at: until the source changes again there is nothing to draw.
        entry.seen = Some(seen);
        // The pixels that changed since the last frame, all of it for the first one.
        let damage = match (&picture, &entry.last) {
            (Picture::Sdr(now), Some(Last::Sdr(last))) => {
                damage_between(&now.rgba, last, now.width as usize)
            }
            (Picture::Hdr { picture: now, .. }, Some(Last::Hdr(last))) => {
                damage_between(&now.half_rgba, last, now.width as usize)
            }
            _ => {
                let (w, h) = match &picture {
                    Picture::Sdr(now) => (now.width, now.height),
                    Picture::Hdr { picture: now, .. } => (now.width, now.height),
                };
                Damage::Part(Rectangle::from_size((w as i32, h as i32).into()))
            }
        };
        // Nothing changed since the last frame: the client keeps waiting.
        let Damage::Part(damage) = damage else {
            entry.pending = Some(frame);
            return true;
        };

        let written = match &picture {
            Picture::Sdr(now) => write_shm(&buffer, now).map(|()| (now.width, now.height)),
            Picture::Hdr {
                picture: now,
                red_high,
            } => write_hdr_shm(&buffer, now, *red_high).map(|()| (now.width, now.height)),
        };
        match written {
            Ok(_) => {
                frame.success(Transform::Normal, vec![damage], monotonic_now());
                entry.last = Some(match picture {
                    Picture::Sdr(now) => Last::Sdr(now.rgba),
                    Picture::Hdr { picture: now, .. } => Last::Hdr(now.half_rgba),
                });
            }
            Err(why) => {
                tracing::warn!("image-copy-capture: {why}");
                frame.fail(CaptureFailureReason::BufferConstraints);
            }
        }
        true
    }
}

impl ImageCaptureSourceHandler for JffState {}

impl OutputCaptureSourceHandler for JffState {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.image_copy.output_capture_source
    }

    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

impl ToplevelCaptureSourceHandler for JffState {
    fn toplevel_capture_source_state(&mut self) -> &mut ToplevelCaptureSourceState {
        &mut self.image_copy.toplevel_capture_source
    }

    // The identifier is `jff-<window id>`, see `ext_foreign_toplevel.rs`.
    fn toplevel_source_created(
        &mut self,
        source: ImageCaptureSource,
        toplevel: ForeignToplevelHandle,
    ) {
        let id = toplevel
            .identifier()
            .strip_prefix("jff-")
            .and_then(|id| id.parse::<u64>().ok());
        if let Some(id) = id {
            source
                .user_data()
                .insert_if_missing(|| ToplevelSourceId(id));
        }
    }
}

impl ImageCopyCaptureHandler for JffState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy.image_copy_capture
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        self.target_constraints(&self.source_target(source)?)
    }

    // Only the seat's pointer is captured: the cursor jff draws is that one's.
    fn cursor_capture_constraints(
        &mut self,
        source: &ImageCaptureSource,
        pointer: &WlPointer,
    ) -> Option<BufferConstraints> {
        let ours = self.seat.get_pointer()?;
        if PointerHandle::<JffState>::from_resource(pointer)? != ours {
            return None;
        }
        let space = self.source_space(&self.source_target(source)?)?;
        let (_, look) = self.cursor_now(space.scale);
        Some(self.cursor_constraints(space.scale, look.as_ref(), None))
    }

    fn new_cursor_session(&mut self, session: CursorSession) {
        self.image_copy.cursor_sessions.push(CursorCapture {
            session,
            pending: None,
            last: None,
        });
        // The next pass of the loop tells it where the cursor is.
    }

    fn cursor_frame(&mut self, session: &CursorSessionRef, frame: Frame) {
        // From now on the frame's image description can't be set any more.
        self.xx_capture_color.frame_captured(&frame);
        let Some(entry) = self
            .image_copy
            .cursor_sessions
            .iter_mut()
            .find(|entry| entry.session == *session)
        else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        };
        entry.pending = Some(frame);
        self.service_image_copy();
    }

    fn cursor_session_destroyed(&mut self, session: CursorSessionRef) {
        self.image_copy
            .cursor_sessions
            .retain(|entry| entry.session != session);
    }

    fn new_session(&mut self, session: Session) {
        self.image_copy.sessions.push(CaptureSession {
            session,
            last: None,
            pending: None,
            seen: None,
        });
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        // From now on the frame's image description can't be set any more.
        self.xx_capture_color.frame_captured(&frame);
        let Some(entry) = self
            .image_copy
            .sessions
            .iter_mut()
            .find(|entry| entry.session == *session)
        else {
            frame.fail(CaptureFailureReason::Stopped);
            return;
        };
        entry.pending = Some(frame);
        self.service_image_copy();
    }

    // The client gave up on a frame (destroyed it, or went away).
    fn frame_aborted(&mut self, frame: FrameRef) {
        for entry in &mut self.image_copy.sessions {
            if entry
                .pending
                .as_ref()
                .is_some_and(|pending| pending.eq(&frame))
            {
                entry.pending = None;
            }
        }
        for entry in &mut self.image_copy.cursor_sessions {
            if entry
                .pending
                .as_ref()
                .is_some_and(|pending| pending.eq(&frame))
            {
                entry.pending = None;
            }
        }
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.image_copy
            .sessions
            .retain(|entry| entry.session != session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: usize, height: usize) -> Vec<u8> {
        vec![0; width * height * 4]
    }

    #[test]
    fn same_picture_has_no_damage() {
        let a = picture(5, 4);
        assert!(matches!(damage_between(&a, &a.clone(), 5), Damage::Same));
    }

    #[test]
    fn damage_is_the_box_around_the_changed_pixels() {
        let last = picture(5, 4);
        let mut now = last.clone();
        // pixel (1, 1) and pixel (3, 2)
        now[(5 + 1) * 4] = 9;
        now[(2 * 5 + 3) * 4 + 2] = 9;
        let Damage::Part(rect) = damage_between(&now, &last, 5) else {
            panic!("expected damage");
        };
        assert_eq!(
            (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
            (1, 1, 3, 2)
        );
    }

    #[test]
    fn a_picture_of_another_size_is_all_damage() {
        let Damage::Part(rect) = damage_between(&picture(5, 4), &picture(5, 3), 5) else {
            panic!("expected damage");
        };
        assert_eq!((rect.size.w, rect.size.h), (5, 4));
    }
}

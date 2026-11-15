//! `ext-image-copy-capture-v1` (with `ext-image-capture-source-v1` for outputs): the standard
//! way for screen recorders, screen sharers, and screenshot tools (wayvnc, grim, OBS,
//! xdg-desktop-portal-wlr successors, ...) to read a monitor's picture.
//!
//! Smithay does the protocol, this file says what a capture is.
//!
//! Sources (`ext_image_capture_source_v1`): a client can capture a monitor, or a single window
//! it got from `ext-foreign-toplevel-list` (see `ext_foreign_toplevel.rs`). A window source
//! stands for the view whose identifier is `jff-<view id>`. Its picture is the window alone on
//! a transparent background, same as `JffState::capture_window` in `screenshot.rs`, wherever
//! the window is (other workspace, covered by another window). The session ends when the
//! window closes.
//!
//! Pictures: a frame is a render of the monitor (or window) into an offscreen texture, read back and written
//! into the client's `wl_shm` buffer. Same as `JffState::capture_output` in `screenshot.rs`, so a
//! capture shows exactly what a screenshot shows (no overview). Buffers offered: `Argb8888` and
//! `Xrgb8888`, and for a monitor in HDR mode also the 10-bit formats (`HDR_FORMATS`), listed
//! after the 8-bit ones. No dmabuf.
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
//! frame that would look the same as the last one stays pending and is looked at again after
//! every event-loop iteration (`JffState::service_image_copy`), so a still screen costs nothing
//! and a change arrives with the next event (a commit, a vblank, input). The whole picture is
//! reported as damaged.
//!
//! Cursor: `paint_cursors` is honored for monitors on the TTY backend (where jff draws the
//! cursor), a window picture never has one. Cursor
//! capture sessions (`create_pointer_cursor_session`) aren't supported: clients get a session
//! that never produces anything and fall back to painting the cursor.
//!
//! Sessions end when the monitor goes away or is switched off. A mode change (or a window
//! resize) sends new buffer constraints and fails the frame in flight with `buffer_constraints`.

use smithay::desktop::Window;
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Buffer as BufferCoords, IsAlive, Rectangle, Size, Transform};
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
    OutputCaptureSourceHandler, OutputCaptureSourceState, ToplevelCaptureSourceHandler,
    ToplevelCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, Frame, FrameRef, ImageCopyCaptureHandler,
    ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut};

use crate::feature::outputs::frame_size;
use crate::feature::screenshot::{Captured, HdrCaptured};
use crate::ipc::ViewId;
use crate::primitives::hdr_capture::{pack_row_2101010, pq_picture};
use crate::protocol::presentation::monotonic_now;
use crate::state::JffState;

/// One capture session and what jff keeps for it.
pub struct CaptureSession {
    /// Dropping it tells the client the session is over.
    session: Session,
    /// The last frame sent, to tell whether the screen changed.
    last: Option<Last>,
    /// The frame the client is waiting for.
    pending: Option<Frame>,
}

/// What the last frame of a session looked like.
enum Last {
    /// Pixels (RGBA) of an 8-bit picture.
    Sdr(Vec<u8>),
    /// The half floats of the working space of an HDR picture.
    Hdr(Vec<u16>),
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
    /// The `HDR_FORMATS` that `wl_shm` offers: a client can't make a buffer in any other.
    hdr_formats: Vec<wl_shm::Format>,
}

impl ImageCopy {
    /// Create the protocol globals.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            image_capture_source: ImageCaptureSourceState::new(),
            output_capture_source: OutputCaptureSourceState::new::<JffState>(display),
            toplevel_capture_source: ToplevelCaptureSourceState::new::<JffState>(display),
            image_copy_capture: ImageCopyCaptureState::new::<JffState>(display),
            sessions: Vec::new(),
            hdr_formats: Vec::new(),
        }
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

/// Set on a source made for a window: the view id (see `ViewId`).
struct ToplevelSourceId(u64);

/// What a client has to allocate for pictures of this `size`: the two 8-bit formats, then
/// `extra`. No dmabuf: the picture is read back to memory anyway.
fn shm_constraints(size: (i32, i32), extra: &[wl_shm::Format]) -> BufferConstraints {
    let mut shm = vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888];
    shm.extend_from_slice(extra);
    BufferConstraints {
        size: size.into(),
        shm,
        dma: None,
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
fn write_shm(buffer: &WlBuffer, picture: &Captured) -> Result<(), String> {
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

impl JffState {
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
            .find_by(|w| w.user_data().get::<ViewId>() == Some(&ViewId(id)))
            .map(|entry| Target::Window(entry.window.clone()))
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
                let extra = if self.hdr_capture(output).is_some() {
                    &self.image_copy.hdr_formats[..]
                } else {
                    &[]
                };
                Some(shm_constraints(size, extra))
            }
            Target::Window(window) => {
                let (_, size) = self.window_capture_size(window).ok()?;
                Some(shm_constraints((size.w, size.h), &[]))
            }
        }
    }

    /// Answer the frames clients wait for. Runs when a frame is asked for and after every
    /// event-loop iteration, does nothing while there are no sessions.
    pub fn service_image_copy(&mut self) {
        if self.image_copy.sessions.is_empty() {
            return;
        }
        let mut sessions = std::mem::take(&mut self.image_copy.sessions);
        sessions.retain_mut(|entry| self.service_session(entry));
        self.image_copy.sessions = sessions;
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

        let Some(frame) = entry.pending.take() else {
            return true;
        };
        let buffer = frame.buffer();
        let format = buffer_format(&buffer);
        let ten_bit = format.and_then(hdr_layout);
        let cursor = entry.session.draw_cursor();
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

        // Nothing changed since the last frame: the client keeps waiting.
        let unchanged = match (&picture, &entry.last) {
            (Picture::Sdr(now), Some(Last::Sdr(last))) => now.rgba == *last,
            (Picture::Hdr { picture: now, .. }, Some(Last::Hdr(last))) => now.half_rgba == *last,
            _ => false,
        };
        if unchanged {
            entry.pending = Some(frame);
            return true;
        }

        let written = match &picture {
            Picture::Sdr(now) => write_shm(&buffer, now).map(|()| (now.width, now.height)),
            Picture::Hdr {
                picture: now,
                red_high,
            } => write_hdr_shm(&buffer, now, *red_high).map(|()| (now.width, now.height)),
        };
        match written {
            Ok((width, height)) => {
                let size: Size<i32, BufferCoords> = (width as i32, height as i32).into();
                frame.success(
                    Transform::Normal,
                    vec![Rectangle::from_size(size)],
                    monotonic_now(),
                );
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

    // The identifier is `jff-<view id>`, see `ext_foreign_toplevel.rs`.
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

    fn new_session(&mut self, session: Session) {
        self.image_copy.sessions.push(CaptureSession {
            session,
            last: None,
            pending: None,
        });
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
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
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.image_copy
            .sessions
            .retain(|entry| entry.session != session);
    }
}

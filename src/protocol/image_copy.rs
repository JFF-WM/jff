//! `ext-image-copy-capture-v1` (with `ext-image-capture-source-v1` for outputs): the standard
//! way for screen recorders, screen sharers, and screenshot tools (wayvnc, grim, OBS,
//! xdg-desktop-portal-wlr successors, ...) to read a monitor's picture.
//!
//! Smithay does the protocol, this file says what a capture is.
//!
//! Sources: a client can capture a monitor. Capturing a single window needs
//! `ext-foreign-toplevel-list`, which jff doesn't have, so the toplevel source manager isn't
//! advertised.
//!
//! Pictures: a frame is a render of the monitor into an offscreen texture, read back and written
//! into the client's `wl_shm` buffer. Same as `JffState::capture_output` in `screenshot.rs`, so a
//! capture shows exactly what a screenshot shows (no overview). Buffers offered: `Argb8888` and
//! `Xrgb8888`. No dmabuf.
//!
//! Damage: the protocol wants a frame only when something changed, or a client asking for the
//! next frame right away would spin. The first frame of a session goes out at once. A later
//! frame that would look the same as the last one stays pending and is looked at again after
//! every event-loop iteration (`JffState::service_image_copy`), so a still screen costs nothing
//! and a change arrives with the next event (a commit, a vblank, input). The whole picture is
//! reported as damaged.
//!
//! Cursor: `paint_cursors` is honored on the TTY backend (where jff draws the cursor). Cursor
//! capture sessions (`create_pointer_cursor_session`) aren't supported: clients get a session
//! that never produces anything and fall back to painting the cursor.
//!
//! Sessions end when the monitor goes away or is switched off. A mode change sends new buffer
//! constraints and fails the frame in flight with `buffer_constraints`.

use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Buffer as BufferCoords, IsAlive, Rectangle, Size, Transform};
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, ImageCaptureSourceState,
    OutputCaptureSourceHandler, OutputCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, Frame, FrameRef, ImageCopyCaptureHandler,
    ImageCopyCaptureState, Session, SessionRef,
};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::feature::screenshot::Captured;
use crate::protocol::presentation::monotonic_now;
use crate::state::JffState;

/// One capture session and what jff keeps for it.
pub struct CaptureSession {
    /// Dropping it tells the client the session is over.
    session: Session,
    /// Pixels (RGBA) of the last frame sent, to tell whether the screen changed.
    last: Option<Vec<u8>>,
    /// The frame the client is waiting for.
    pending: Option<Frame>,
}

/// Everything the protocols need to keep between requests.
pub struct ImageCopy {
    /// Only has to stay alive: it handles the `ext_image_capture_source_v1` objects.
    #[allow(dead_code)]
    image_capture_source: ImageCaptureSourceState,
    output_capture_source: OutputCaptureSourceState,
    image_copy_capture: ImageCopyCaptureState,
    sessions: Vec<CaptureSession>,
}

impl ImageCopy {
    /// Create the protocol globals.
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            image_capture_source: ImageCaptureSourceState::new(),
            output_capture_source: OutputCaptureSourceState::new::<JffState>(display),
            image_copy_capture: ImageCopyCaptureState::new::<JffState>(display),
            sessions: Vec::new(),
        }
    }
}

/// What a client has to allocate to capture `output`, or `None` when it has no mode (is off).
fn output_constraints(output: &Output) -> Option<BufferConstraints> {
    let mode = output.current_mode()?;
    Some(BufferConstraints {
        size: (mode.size.w, mode.size.h).into(),
        shm: vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888],
        // No dmabuf: the picture is read back to memory anyway.
        dma: None,
    })
}

/// Write `picture` (RGBA) into shm buffer `buffer`, as `Argb8888`/`Xrgb8888` (which in memory
/// are B, G, R, A on the little-endian machines jff runs on).
fn write_shm(buffer: &WlBuffer, picture: &Captured) -> Result<(), String> {
    let (width, height) = (picture.width as usize, picture.height as usize);
    with_buffer_contents_mut(buffer, |ptr, len, data| {
        if data.width < picture.width as i32 || data.height < picture.height as i32 {
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
            let row = &mut pool[offset + y * stride..offset + y * stride + width * 4];
            let source = &picture.rgba[y * width * 4..(y + 1) * width * 4];
            for (out, px) in row.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
                out[0] = px[2];
                out[1] = px[1];
                out[2] = px[0];
                out[3] = px[3];
            }
        }
        Ok(())
    })
    .map_err(|err| format!("could not access the buffer: {err:?}"))?
}

impl JffState {
    /// The monitor a capture source stands for, if it's still there.
    fn source_output(&self, source: &ImageCaptureSource) -> Option<Output> {
        let output = source.user_data().get::<WeakOutput>()?.upgrade()?;
        self.outputs.iter().find(|o| **o == output).cloned()
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
        let Some(output) = self.source_output(&entry.session.source()) else {
            return false;
        };
        let Some(wanted) = output_constraints(&output) else {
            return false;
        };

        // The monitor changed mode: clients have to allocate new buffers.
        let size_ok = entry
            .session
            .current_constraints()
            .is_some_and(|c| c.size == wanted.size);
        if !size_ok {
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
        let picture = match self.capture_output(&output, entry.session.draw_cursor()) {
            Ok(picture) => picture,
            Err(why) => {
                tracing::warn!("image-copy-capture: {why}");
                frame.fail(CaptureFailureReason::Unknown);
                return true;
            }
        };

        // Nothing changed since the last frame: the client keeps waiting.
        if entry.last.as_ref() == Some(&picture.rgba) {
            entry.pending = Some(frame);
            return true;
        }

        match write_shm(&frame.buffer(), &picture) {
            Ok(()) => {
                let size: Size<i32, BufferCoords> =
                    (picture.width as i32, picture.height as i32).into();
                frame.success(
                    Transform::Normal,
                    vec![Rectangle::from_size(size)],
                    monotonic_now(),
                );
                entry.last = Some(picture.rgba);
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

impl ImageCopyCaptureHandler for JffState {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy.image_copy_capture
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        output_constraints(&self.source_output(source)?)
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

//! `wlr-screencopy-unstable-v1` (version 3): the older capture protocol that grim, wf-recorder,
//! wayvnc, slurp-based tools and xdg-desktop-portal-wlr speak. `ext-image-copy-capture-v1` (see
//! `image_copy.rs`) is the newer one, both are offered.
//!
//! Smithay has no module for this one, so the requests are handled here, on bindings generated
//! from `protocols/wlr-screencopy-unstable-v1.xml`.
//!
//! What is offered: monitors only (the protocol has no windows), whole or a region of one. A
//! picture is what `JffState::capture_output` makes, the same as a screenshot and as an
//! `ext-image-copy-capture` frame: sRGB, no overview, the cursor when `overlay_cursor` is set
//! (TTY backend only). Buffers: `wl_shm` `Xrgb8888` (`Argb8888` is accepted too), and on the
//! TTY backend a dmabuf in the formats `ImageCopy` found (`offer_capture_dmabuf`), for a capture
//! of a whole monitor. A region always goes through `wl_shm`.
//!
//! `copy` is answered at the next turn of the event loop. `copy_with_damage` waits until a
//! frame was sent to the monitor since the last picture handed to this client, and then
//! reports what changed since that picture, as boxes worked out from the render elements (a
//! `DamageTrack` for each client and monitor, see `capture.rs`). When nothing changed in the
//! part the client asked for (all of the monitor, or its region), it keeps waiting and nothing
//! is drawn or read back.
//!
//! Hidden from sandboxed clients, like the other capture protocols.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Buffer as BufferCoords, Rectangle};
use smithay::wayland::dmabuf::get_dmabuf;
use smithay::wayland::shm::with_buffer_contents;

use super::wlr_screencopy_bindings::server::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use super::wlr_screencopy_bindings::server::zwlr_screencopy_manager_v1::{
    self, ZwlrScreencopyManagerV1,
};
use crate::feature::outputs::frame_size;
use crate::feature::screenshot::{Captured, DamageTrack};
use crate::protocol::image_copy::write_shm;
use crate::protocol::presentation::monotonic_now;
use crate::state::{is_sandboxed, JffState};

/// What a frame object stands for, fixed when it is created.
pub struct FrameData {
    /// `None` for a frame that failed when it was made.
    output: Option<WeakOutput>,
    /// The whole picture of the monitor, in pixels.
    full: (i32, i32),
    /// The part wanted: x, y, width, height in pixels of the picture. Empty means the request
    /// had nothing of the monitor in it.
    region: (i32, i32, i32, i32),
    cursor: bool,
    /// A `copy` or `copy_with_damage` was sent.
    used: AtomicBool,
}

impl FrameData {
    /// A dmabuf can only hold a picture of the whole monitor.
    fn whole(&self) -> bool {
        self.region == (0, 0, self.full.0, self.full.1)
    }

    fn size(&self) -> (i32, i32) {
        (self.region.2, self.region.3)
    }
}

/// A frame the client asked a copy of, waiting for the event loop.
struct PendingFrame {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    with_damage: bool,
}

/// Everything the protocol keeps between requests.
pub struct WlrScreencopy {
    #[allow(dead_code)]
    global: smithay::reexports::wayland_server::backend::GlobalId,
    pending: Vec<PendingFrame>,
    /// How many frames were sent to each monitor (by name).
    drawn: Vec<(String, u64)>,
    /// What each client's `copy_with_damage` last got, to tell it what changed since. The
    /// oldest are dropped, a client that comes back starts with a whole picture.
    tracks: Vec<WlrTrack>,
}

/// How many clients' (and monitors') damage is kept at once.
const MAX_TRACKS: usize = 16;

/// What one client was last given of one monitor.
struct WlrTrack {
    client: ClientId,
    output: String,
    /// Whether the picture had the cursor in it: a different picture, so its own damage.
    cursor: bool,
    /// The `drawn` count of the monitor at that picture.
    drawn: u64,
    track: DamageTrack,
}

impl WlrScreencopy {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            global: display.create_global::<JffState, ZwlrScreencopyManagerV1, ()>(3, ()),
            pending: Vec::new(),
            drawn: Vec::new(),
            tracks: Vec::new(),
        }
    }

    /// A frame was sent to `output`: what a capture of it shows may have changed.
    pub fn output_drawn(&mut self, output: &Output) {
        let name = output.name();
        match self.drawn.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => self.drawn.push((name, 1)),
        }
    }

    fn count(list: &[(String, u64)], name: &str) -> Option<u64> {
        list.iter().find(|(n, _)| n == name).map(|(_, c)| *c)
    }

    /// The `drawn` count of `output` at the last picture `client` got, if it got one.
    fn track_drawn(&self, client: &ClientId, output: &str, cursor: bool) -> Option<u64> {
        self.tracks
            .iter()
            .find(|t| t.client == *client && t.output == output && t.cursor == cursor)
            .map(|t| t.drawn)
    }

    /// Take the damage of `client` out, a new one (whole picture) when it has none. Put it
    /// back with `put_track`, one that is not put back starts over, which is what a picture
    /// that did not get through needs.
    fn take_track(&mut self, client: &ClientId, output: &str, cursor: bool) -> DamageTrack {
        let found = self
            .tracks
            .iter()
            .position(|t| t.client == *client && t.output == output && t.cursor == cursor);
        match found {
            Some(at) => self.tracks.remove(at).track,
            None => DamageTrack::default(),
        }
    }

    fn put_track(
        &mut self,
        client: ClientId,
        output: String,
        cursor: bool,
        drawn: u64,
        track: DamageTrack,
    ) {
        self.tracks.push(WlrTrack {
            client,
            output,
            cursor,
            drawn,
            track,
        });
        if self.tracks.len() > MAX_TRACKS {
            self.tracks.remove(0);
        }
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for JffState {
    fn can_view(client: Client, _: &()) -> bool {
        !is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        let (frame, cursor, output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor != 0, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                overlay_cursor != 0,
                output,
                Some((x, y, width, height)),
            ),
            zwlr_screencopy_manager_v1::Request::Destroy => return,
        };

        let output = Output::from_resource(&output)
            .filter(|output| state.outputs.iter().any(|o| o == output));
        let full = output.as_ref().and_then(frame_size);
        let (Some(output), Some(full)) = (output, full) else {
            // A monitor that is gone, or off: a frame that fails at once. Its data says so.
            let frame = data_init.init(
                frame,
                FrameData {
                    output: None,
                    full: (0, 0),
                    region: (0, 0, 0, 0),
                    cursor,
                    used: AtomicBool::new(false),
                },
            );
            frame.failed();
            return;
        };

        // The region is in logical coordinates of the monitor, the picture in pixels.
        let region = match region {
            None => (0, 0, full.0, full.1),
            Some((x, y, w, h)) => {
                let scale = output.current_scale().fractional_scale();
                let x0 = ((x as f64 * scale).round() as i32).max(0);
                let y0 = ((y as f64 * scale).round() as i32).max(0);
                let x1 = (((x as i64 + w as i64) as f64 * scale).round() as i32).min(full.0);
                let y1 = (((y as i64 + h as i64) as f64 * scale).round() as i32).min(full.1);
                if w <= 0 || h <= 0 || x1 <= x0 || y1 <= y0 {
                    (0, 0, 0, 0)
                } else {
                    (x0, y0, x1 - x0, y1 - y0)
                }
            }
        };

        let data = FrameData {
            output: Some(output.downgrade()),
            full,
            region,
            cursor,
            used: AtomicBool::new(false),
        };
        let whole = data.whole();
        let (w, h) = data.size();
        let frame = data_init.init(frame, data);
        if w <= 0 || h <= 0 {
            frame.failed();
            return;
        }

        frame.buffer(wl_shm::Format::Xrgb8888, w as u32, h as u32, w as u32 * 4);
        if frame.version() >= 3 {
            if whole {
                for code in state.image_copy.dmabuf_codes() {
                    frame.linux_dmabuf(code, w as u32, h as u32);
                }
            }
            frame.buffer_done();
        }
    }
}

/// Whether `buffer` can take the picture `data` stands for. `Err` is the text of the protocol
/// error to post.
fn check_buffer(data: &FrameData, buffer: &WlBuffer) -> Result<(), &'static str> {
    let (w, h) = data.size();
    if let Ok(dmabuf) = get_dmabuf(buffer) {
        let size = dmabuf.size();
        if !data.whole() {
            return Err("a region can only be copied into a wl_shm buffer");
        }
        if (size.w, size.h) != (w, h) {
            return Err("the buffer is not the size of the frame");
        }
        if !matches!(dmabuf.format().code, Fourcc::Argb8888 | Fourcc::Xrgb8888) {
            return Err("the buffer is not in a format the frame offered");
        }
        return Ok(());
    }
    with_buffer_contents(buffer, |_, _, shm| {
        if shm.width != w || shm.height != h {
            Err("the buffer is not the size of the frame")
        } else if shm.stride < w * 4 {
            Err("the stride of the buffer is too small")
        } else if !matches!(
            shm.format,
            wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888
        ) {
            Err("the buffer is not in a format the frame offered")
        } else {
            Ok(())
        }
    })
    .unwrap_or(Err("the buffer is neither wl_shm nor dmabuf"))
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
        };
        if data.used.swap(true, Ordering::SeqCst) {
            resource.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "the frame was already used to copy a buffer",
            );
            return;
        }
        // A frame that failed when it was made has nothing to copy: it already said so.
        let (w, h) = data.size();
        if w <= 0 || h <= 0 {
            return;
        }
        if let Err(why) = check_buffer(data, &buffer) {
            resource.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, why);
            return;
        }
        state.wlr_screencopy.pending.push(PendingFrame {
            frame: resource.clone(),
            buffer,
            with_damage,
        });
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &ZwlrScreencopyFrameV1,
        _data: &FrameData,
    ) {
        state
            .wlr_screencopy
            .pending
            .retain(|p| p.frame != *resource);
    }
}

/// The `region` box (x, y, width, height) of `picture` as a picture of its own.
fn crop(picture: Captured, region: (i32, i32, i32, i32)) -> Captured {
    let (x, y, w, h) = (
        region.0 as usize,
        region.1 as usize,
        region.2 as usize,
        region.3 as usize,
    );
    let stride = picture.width as usize * 4;
    if x == 0 && y == 0 && w == picture.width as usize && h == picture.height as usize {
        return picture;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = row * stride + x * 4;
        rgba.extend_from_slice(&picture.rgba[start..start + w * 4]);
    }
    Captured {
        rgba,
        width: w as u32,
        height: h as u32,
    }
}

/// The boxes of `rects` (pixels of the whole picture) that fall in `region` (x, y, width,
/// height), as boxes of the region's own picture. Empty when none does.
fn clip_damage(
    rects: &[Rectangle<i32, BufferCoords>],
    region: (i32, i32, i32, i32),
) -> Vec<(i32, i32, i32, i32)> {
    let (rx, ry, rw, rh) = region;
    rects
        .iter()
        .filter_map(|rect| {
            let x0 = rect.loc.x.max(rx);
            let y0 = rect.loc.y.max(ry);
            let x1 = (rect.loc.x + rect.size.w).min(rx + rw);
            let y1 = (rect.loc.y + rect.size.h).min(ry + rh);
            (x1 > x0 && y1 > y0).then_some((x0 - rx, y0 - ry, x1 - x0, y1 - y0))
        })
        .collect()
}

impl JffState {
    /// Answer the copies clients wait for. Runs after every event-loop iteration, does nothing
    /// while no copy is waiting.
    pub fn service_wlr_screencopy(&mut self) {
        if self.wlr_screencopy.pending.is_empty() {
            return;
        }
        for pending in std::mem::take(&mut self.wlr_screencopy.pending) {
            if let Some(waiting) = self.serve_wlr_frame(pending) {
                self.wlr_screencopy.pending.push(waiting);
            }
        }
    }

    /// Answer one frame. Gives it back when it has to wait for damage.
    fn serve_wlr_frame(&mut self, pending: PendingFrame) -> Option<PendingFrame> {
        if !pending.frame.is_alive() {
            return None;
        }
        let Some(data) = pending.frame.data::<FrameData>() else {
            return None;
        };
        let (region, cursor, full) = (data.region, data.cursor, data.full);
        let whole = data.whole();
        let output = data
            .output
            .as_ref()
            .and_then(WeakOutput::upgrade)
            .filter(|output| self.outputs.iter().any(|o| o == output));
        // A monitor that went away or changed mode since the frame was made: the buffer
        // the client has is the wrong size now.
        let Some(output) = output.filter(|output| frame_size(output) == Some(full)) else {
            pending.frame.failed();
            return None;
        };
        let name = output.name();
        let drawn = WlrScreencopy::count(&self.wlr_screencopy.drawn, &name).unwrap_or(0);
        // `copy_with_damage` is told what changed since the last picture this client got of
        // the monitor, so what it got is kept for it. A plain `copy` says nothing of damage.
        let client = if pending.with_damage {
            pending.frame.client().map(|client| client.id())
        } else {
            None
        };
        if let Some(client) = &client {
            if self.wlr_screencopy.track_drawn(client, &name, cursor) == Some(drawn) {
                return Some(pending);
            }
        }
        let mut track = match &client {
            Some(client) => self.wlr_screencopy.take_track(client, &name, cursor),
            None => DamageTrack::default(),
        };

        // `Ok(Some(boxes))`: the buffer holds the picture, `boxes` (x, y, width, height in
        // pixels of the buffer) changed. `Ok(None)`: nothing changed, nothing was drawn.
        let whole_box = vec![(0, 0, region.2, region.3)];
        let result: Result<Option<Vec<(i32, i32, i32, i32)>>, String> =
            match get_dmabuf(&pending.buffer).ok().cloned() {
                Some(mut dmabuf) if whole && client.is_some() => self
                    .capture_output_into_tracked(&output, cursor, &mut dmabuf, &mut track)
                    .map(|changed| {
                        changed.map(|rects| {
                            rects
                                .iter()
                                .map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h))
                                .collect()
                        })
                    }),
                Some(mut dmabuf) if whole => self
                    .copy_wlr_dmabuf(&output, cursor, &mut dmabuf)
                    .map(|()| Some(whole_box.clone())),
                Some(_) => Err("a region can only be copied into a wl_shm buffer".to_string()),
                None if client.is_some() => self
                    .capture_output_tracked(&output, cursor, &mut track)
                    .and_then(|changed| {
                        let Some((picture, rects)) = changed else {
                            return Ok(None);
                        };
                        // Only what changed inside the part the client asked for counts.
                        let clipped = clip_damage(&rects, region);
                        if clipped.is_empty() {
                            return Ok(None);
                        }
                        write_shm(&pending.buffer, &crop(picture, region)).map(|()| Some(clipped))
                    }),
                None => self
                    .capture_output(&output, cursor)
                    .map(|picture| crop(picture, region))
                    .and_then(|picture| write_shm(&pending.buffer, &picture))
                    .map(|()| Some(whole_box.clone())),
            };
        let boxes = match result {
            // the picture did not get through: the next one is whole
            Err(why) => {
                tracing::warn!("wlr-screencopy: {why}");
                pending.frame.failed();
                return None;
            }
            // nothing changed for this client: it keeps waiting for the next frame
            Ok(None) => {
                if let Some(client) = client {
                    self.wlr_screencopy
                        .put_track(client, name, cursor, drawn, track);
                }
                return Some(pending);
            }
            Ok(Some(boxes)) => boxes,
        };
        if let Some(client) = client {
            self.wlr_screencopy
                .put_track(client, name, cursor, drawn, track);
        }

        let frame = &pending.frame;
        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        if pending.with_damage && frame.version() >= 2 {
            for (x, y, w, h) in boxes {
                frame.damage(x as u32, y as u32, w as u32, h as u32);
            }
        }
        let now = monotonic_now();
        let secs = now.as_secs();
        frame.ready((secs >> 32) as u32, secs as u32, now.subsec_nanos());
        None
    }

    fn copy_wlr_dmabuf(
        &mut self,
        output: &Output,
        cursor: bool,
        dmabuf: &mut Dmabuf,
    ) -> Result<(), String> {
        self.capture_output_into(output, cursor, dmabuf).map(|_| ())
    }
}

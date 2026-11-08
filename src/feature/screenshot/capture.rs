//! Screenshots: three ways to take one, plus the area selection that sits on top of the
//! compositor while it's going on.
//!
//! Capture: `JffState::capture_output` and `JffState::capture_window` build the same element
//! list the frame loop draws, render it into an offscreen texture, and read the pixels back
//! (RGBA, top row first). The overview is never in a picture.
//!
//! Saving: `save.rs` turns the pixels into a file, the clipboard, and a notification on a
//! worker thread. Capturing is synchronous, nothing else is.
//!
//! Area mode: the monitor under the pointer freezes into a picture first (so an open menu is
//! in it), then `ScreenshotUi` takes the pointer (`ScreenshotGrab`) and the keyboard
//! (`KeyboardFocus::Screenshot`). Clients see no motion, click, or key meanwhile. The selection
//! is cut out of the frozen pixels, no second render. Clicks come in through `input.rs`
//! (`JffState::screenshot_button`), keys through the `input_intercept` closure there
//! (`screenshot_key`).
//!
//! Drawing: `screenshot_elements` replaces everything but the cursor on the frozen monitor,
//! shared by the TTY and winit backends.

use std::path::PathBuf;

use smithay::backend::allocator::Fourcc;
use smithay::backend::input::InputTime;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::{AsRenderElements, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen, TextureMapping};
use smithay::desktop::Window;
use smithay::input::keyboard::Keysym;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, Focus, GestureHoldBeginEvent,
    GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData,
    MotionEvent, PointerGrab, PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{
    Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Serial, Size, Transform,
    SERIAL_COUNTER,
};

use crate::app::BACKGROUND;
use crate::feature::outputs::output_rect;
use crate::feature::screenshot::area::{
    crop_rgba, flip_rows, physical_rect, screenshot_dir, Selector,
};
use crate::feature::screenshot::save::{notify, spawn_save, SaveJob};
use crate::input::BTN_LEFT;
use crate::primitives::layout::{self, Rect};
use crate::protocol::layer_shell::{
    layer_body_elements, layer_elements, layer_popup_elements, layers_above, BELOW_WINDOWS,
};
use crate::render::cursor::RenderCursor;
use crate::render::elements::JffElement;
use crate::state::JffState;

/// White frame around the selection, in pixels of the picture.
const BORDER: i32 = 2;
const BORDER_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Dims everything outside the selection (the whole monitor when there is none).
const DIM_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.5];
/// How far outside a monitor a cursor can still be partly visible (same as `tty.rs`).
const CURSOR_MARGIN: i32 = 256;

/// What a key does while the area selection is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotKey {
    /// Leave, don't save.
    Cancel,
    /// Save the selection (ignored when there is none).
    Confirm,
}

/// The key for `sym`, if it's one. Modifiers ignored.
pub fn screenshot_key(sym: Keysym) -> Option<ScreenshotKey> {
    if sym == Keysym::Escape {
        Some(ScreenshotKey::Cancel)
    } else if sym == Keysym::Return || sym == Keysym::KP_Enter || sym == Keysym::space {
        Some(ScreenshotKey::Confirm)
    } else {
        None
    }
}

/// Four rectangles that cover `full` except for `hole` (all `(x, y, w, h)`, `hole` inside
/// `full`): above, below, left, right. Some may be empty. Without a hole, the first is all of
/// `full`.
pub fn around(full: Rect, hole: Option<Rect>) -> [Rect; 4] {
    let (fx, fy, fw, fh) = full;
    let Some((hx, hy, hw, hh)) = hole else {
        return [full, (fx, fy, 0, 0), (fx, fy, 0, 0), (fx, fy, 0, 0)];
    };
    [
        (fx, fy, fw, hy - fy),
        (fx, hy + hh, fw, fy + fh - (hy + hh)),
        (fx, hy, hx - fx, hh),
        (hx + hw, hy, fx + fw - (hx + hw), hh),
    ]
}

/// Four bars of a frame `width` thick, drawn just outside `rect`: top, bottom, left, right.
pub fn frame_bars(rect: Rect, width: i32) -> [Rect; 4] {
    let (x, y, w, h) = rect;
    [
        (x - width, y - width, w + 2 * width, width),
        (x - width, y + h, w + 2 * width, width),
        (x - width, y, width, h),
        (x + w, y, width, h),
    ]
}

/// `rect` cut to what lies inside `bounds`. `None` if nothing is left.
pub fn clip(rect: Rect, bounds: Rect) -> Option<Rect> {
    let left = rect.0.max(bounds.0);
    let top = rect.1.max(bounds.1);
    let right = (rect.0 + rect.2).min(bounds.0 + bounds.2);
    let bottom = (rect.1 + rect.3).min(bounds.1 + bounds.3);
    (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
}

/// The area selection: a frozen picture of one monitor plus the rectangle drawn on it so far.
pub struct ScreenshotUi {
    output: Output,
    /// Where the monitor was and its scale, at freeze time.
    rect: Rect,
    scale: f64,
    /// Frozen frame, RGBA (`Abgr8888`), top row first, `width * height * 4` bytes.
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    buffer: MemoryRenderBuffer,
    selector: Selector,
    /// Serial of our pointer grab, so we can recognise it when ending.
    grab_serial: Serial,
    /// Stable ids of the eight quads (4 frame bars, 4 dim areas), so damage tracking sees them
    /// move instead of reappear.
    ids: [Id; 8],
}

impl ScreenshotUi {
    /// Whether the selection is on `output`.
    pub fn shows(&self, output: &Output) -> bool {
        self.output == *output
    }
}

/// A picture read back from the GPU.
pub struct Captured {
    /// Tightly packed RGBA, top row first.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Render `elements` (front to back, in picture pixels) into an offscreen texture of `size` and
/// read it back.
fn render_to_rgba<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    size: (i32, i32),
    scale: f64,
    elements: &[E],
    clear: [f32; 4],
) -> Result<Captured, String> {
    if size.0 <= 0 || size.1 <= 0 {
        return Err("nothing to capture: the picture would be empty".to_string());
    }
    let buffer_size: Size<i32, BufferCoords> = size.into();
    let physical: Size<i32, Physical> = size.into();

    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .map_err(|err| format!("could not create an offscreen buffer: {err:?}"))?;
    let mut target = renderer
        .bind(&mut texture)
        .map_err(|err| format!("could not draw into the offscreen buffer: {err:?}"))?;

    let mut tracker = OutputDamageTracker::new(physical, scale, Transform::Normal);
    let result = tracker
        .render_output(renderer, &mut target, 0, elements, clear)
        .map_err(|err| format!("could not render the picture: {err:?}"))?;
    // We read the pixels right away, so wait for the GPU first.
    let _ = result.sync.wait();

    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .map_err(|err| format!("could not read the picture back: {err:?}"))?;
    let flipped = mapping.flipped();
    let data = renderer
        .map_texture(&mapping)
        .map_err(|err| format!("could not map the picture: {err:?}"))?;

    let (width, height) = (size.0 as usize, size.1 as usize);
    if data.len() != width * height * 4 {
        return Err(format!(
            "the GPU gave {} bytes for a {width}x{height} picture",
            data.len()
        ));
    }
    let mut rgba = data.to_vec();
    // Seen on a real run: rows come out upside down exactly when flipped() says false, so the
    // check is inverted. If a picture ever comes out upside down again, this is the line.
    if !flipped {
        flip_rows(&mut rgba, width * 4);
    }
    Ok(Captured {
        rgba,
        width: size.0 as u32,
        height: size.1 as u32,
    })
}

/// The cursor as render elements. `pointer` and `origin` in the shared space. Same as the frame
/// loop in `tty.rs`.
fn cursor_elements(
    renderer: &mut GlesRenderer,
    cursor: RenderCursor,
    pointer: Point<f64, Logical>,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
    millis: u32,
) -> Vec<JffElement> {
    let mut elements: Vec<JffElement> = Vec::new();
    match cursor {
        RenderCursor::Hidden => {}
        RenderCursor::Surface { hotspot, surface } => {
            let loc: Point<i32, Physical> =
                (pointer.to_i32_round() - hotspot - origin).to_physical_precise_round(scale);
            let surfaces: Vec<JffElement> = render_elements_from_surface_tree(
                &mut *renderer,
                &surface,
                loc,
                scale,
                1.0,
                Kind::Cursor,
            );
            elements.extend(surfaces);
        }
        RenderCursor::Named(cursor) => {
            let frame = cursor.frame(millis);
            let loc: Point<f64, Physical> =
                (pointer - origin.to_f64() - frame.hotspot.to_f64()).to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                &frame.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => elements.push(JffElement::Memory(element)),
                Err(err) => tracing::warn!("error importing the cursor image: {err:?}"),
            }
        }
    }
    elements
}

impl JffState {
    /// A picture of `output` as it is on screen, without the overview. The cursor is only in it
    /// on the TTY backend (winit leaves it to the host).
    pub(crate) fn capture_output(
        &mut self,
        output: &Output,
        with_cursor: bool,
    ) -> Result<Captured, String> {
        if self.session_locked() {
            return Err("the session is locked".to_string());
        }
        let mode = output
            .current_mode()
            .ok_or("the monitor has no mode (it is off)")?;
        let scale_factor = output.current_scale().fractional_scale();
        let scale = Scale::from(scale_factor);
        let rect = output_rect(output);
        let origin: Point<i32, Logical> = (rect.0, rect.1).into();
        let pointer: Point<f64, Logical> = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        let millis = self.start_time.elapsed().as_millis() as u32;
        let fullscreen = self.fullscreen_rects();

        // Same order as `render_crtc`: cursor first, when asked for and drawn by jff at all.
        let cursor_near =
            layout::contains(layout::grow(rect, CURSOR_MARGIN), (pointer.x, pointer.y));
        let cursor = (with_cursor && self.backend.is_tty() && cursor_near)
            .then(|| self.cursor.render_cursor());

        let renderer = self.backend.renderer();
        let mut elements: Vec<JffElement> = Vec::new();
        if let Some(cursor) = cursor {
            elements.extend(cursor_elements(
                &mut *renderer,
                cursor,
                pointer,
                origin,
                scale,
                millis,
            ));
        }
        let above = layers_above(&fullscreen, rect, false);
        elements.extend(layer_elements(&mut *renderer, output, above, None, &[]));
        elements.extend(layer_popup_elements(
            &mut *renderer,
            output,
            &BELOW_WINDOWS,
            None,
            &[],
        ));
        for entry in self.desk.top_down() {
            let geo = entry.window.geometry();
            let window_origin: Point<i32, Logical> = Point::from(entry.pos) - geo.loc;
            let bbox = entry.window.bbox();
            let area = (
                window_origin.x + bbox.loc.x,
                window_origin.y + bbox.loc.y,
                bbox.size.w,
                bbox.size.h,
            );
            // Windows on other monitors don't belong here.
            if !layout::intersects(area, rect) {
                continue;
            }
            let loc: Point<i32, Physical> =
                (window_origin - origin).to_physical_precise_round(scale);
            let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = entry
                .window
                .render_elements(&mut *renderer, loc, scale, 1.0);
            elements.extend(surfaces.into_iter().map(JffElement::from));
        }
        elements.extend(layer_body_elements(
            &mut *renderer,
            output,
            &BELOW_WINDOWS,
            None,
            &[],
        ));

        render_to_rgba(
            renderer,
            (mode.size.w, mode.size.h),
            scale_factor,
            &elements,
            BACKGROUND,
        )
    }

    /// Scale of the monitor `window` is on and the pixel size of a picture of it.
    pub(crate) fn window_capture_size(
        &self,
        window: &Window,
    ) -> Result<(f64, Size<i32, Physical>), String> {
        let geo = window.geometry();
        if geo.size.w <= 0 || geo.size.h <= 0 {
            return Err("the window has nothing on screen yet".to_string());
        }
        // Scale of the monitor the window is on.
        let pos = self
            .desk
            .pos_of(|w| w == window)
            .ok_or("the window is gone")?;
        let on = self.rect_for_window(pos, (geo.size.w, geo.size.h));
        let scale_factor = self
            .outputs
            .iter()
            .find(|output| output_rect(output) == on)
            .map_or(1.0, |output| output.current_scale().fractional_scale());
        let size: Size<i32, Physical> = geo
            .size
            .to_f64()
            .to_physical(Scale::from(scale_factor))
            .to_i32_round();
        Ok((scale_factor, size))
    }

    /// A picture of `window` alone (its geometry, with its menus) on a transparent background.
    /// Pixels are premultiplied.
    pub(crate) fn capture_window(&mut self, window: &Window) -> Result<Captured, String> {
        let geo = window.geometry();
        let (scale_factor, size) = self.window_capture_size(window)?;
        let scale = Scale::from(scale_factor);
        // Draw the window with its geometry corner at (0, 0).
        let loc: Point<i32, Physical> =
            Point::<i32, Logical>::from((-geo.loc.x, -geo.loc.y)).to_physical_precise_round(scale);
        let renderer = self.backend.renderer();
        // A client's fade (wp_alpha_modifier_v1) is applied per surface on top of the 1.0, so
        // the picture shows the window as it looks on screen, translucent where it is faded.
        // The closing fade starts from this picture, so it carries on from what was shown.
        let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            window.render_elements(&mut *renderer, loc, scale, 1.0);
        render_to_rgba(
            renderer,
            (size.w, size.h),
            scale_factor,
            &elements,
            [0.0; 4],
        )
    }

    /// Hand a finished picture to the worker thread that writes it.
    fn queue_save(&self, picture: Captured, unpremultiply: bool) {
        let config = self.config.screenshot();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let pictures = std::env::var("XDG_PICTURES_DIR").ok();
        let dir = screenshot_dir(config.path.as_deref(), pictures.as_deref(), home.as_deref());
        spawn_save(SaveJob {
            rgba: picture.rgba,
            width: picture.width,
            height: picture.height,
            unpremultiply,
            dir,
            copy_to_clipboard: config.copy_to_clipboard,
        });
    }

    /// Save a picture of the whole monitor under the pointer.
    pub fn screenshot_fullscreen(&mut self) -> Result<(), String> {
        let output = self.output_at_pointer().ok_or("there is no monitor")?;
        let include_cursor = self.config.screenshot().include_cursor;
        let picture = self.capture_output(&output, include_cursor)?;
        self.queue_save(picture, false);
        Ok(())
    }

    /// Save a picture of the window the user works in.
    pub fn screenshot_window(&mut self) -> Result<(), String> {
        let window = self
            .focused_window
            .clone()
            .or_else(|| {
                self.desk
                    .top_down()
                    .next()
                    .map(|entry| entry.window.clone())
            })
            .ok_or("there is no window to take a picture of")?;
        let picture = self.capture_window(&window)?;
        self.queue_save(picture, true);
        Ok(())
    }

    /// Freeze the monitor under the pointer and let the user draw a rectangle on it.
    pub fn start_screenshot_area(&mut self) -> Result<(), String> {
        if self.screenshot.is_some() || self.resize.is_some() {
            return Ok(());
        }
        // Overview and selection never open together.
        if self.overview.is_some() {
            self.abort_overview();
        }
        let output = self.output_at_pointer().ok_or("there is no monitor")?;
        let rect = output_rect(&output);
        let scale = output.current_scale().fractional_scale();

        // Freeze first, so an open menu is in the picture, and no cursor.
        let picture = self.capture_output(&output, false)?;
        // Then dismiss popups: with a grab they'd hold the pointer.
        self.dismiss_popups();

        let pointer = self.seat.get_pointer().ok_or("there is no pointer")?;
        if pointer.is_grabbed() {
            return Err("a move or resize has the pointer".to_string());
        }

        let buffer = MemoryRenderBuffer::from_slice(
            &picture.rgba,
            Fourcc::Abgr8888,
            (picture.width as i32, picture.height as i32),
            1,
            Transform::Normal,
            None,
        );
        let grab_serial = SERIAL_COUNTER.next_serial();
        self.screenshot = Some(ScreenshotUi {
            output,
            rect,
            scale,
            pixels: picture.rgba,
            width: picture.width,
            height: picture.height,
            buffer,
            selector: Selector::new(rect),
            grab_serial,
            ids: std::array::from_fn(|_| Id::new()),
        });

        // Tell the client under the pointer the pointer left, then take it.
        let at = pointer.current_location();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: at,
        };
        pointer.set_grab(
            self,
            ScreenshotGrab { start_data },
            grab_serial,
            Focus::Clear,
        );
        self.cursor
            .set_status(CursorImageStatus::Named(CursorIcon::Crosshair));
        // Nobody has the keyboard while the selection is open.
        self.update_keyboard_focus();
        Ok(())
    }

    /// Leave the area selection without saving. Safe when there is none.
    ///
    /// Must not be called from inside a method of `ScreenshotGrab` (the pointer is locked there).
    pub fn cancel_screenshot(&mut self) {
        let Some(ui) = self.screenshot.take() else {
            return;
        };
        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.has_grab(ui.grab_serial) {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }
        self.cursor.set_status(CursorImageStatus::default_named());
        // Keyboard focus is derived, it goes back where it belongs by itself.
        self.update_keyboard_focus();
        self.refresh_pointer_focus();
    }

    /// A key of the area selection (see `screenshot_key`).
    pub fn handle_screenshot_key(&mut self, key: ScreenshotKey) {
        match key {
            ScreenshotKey::Cancel => self.cancel_screenshot(),
            ScreenshotKey::Confirm => self.confirm_screenshot(),
        }
    }

    /// Save the selected part of the frozen picture and leave. No selection, nothing happens.
    fn confirm_screenshot(&mut self) {
        let Some(ui) = self.screenshot.as_ref() else {
            return;
        };
        let Some(selection) = ui.selector.rect() else {
            return;
        };
        let wanted = physical_rect(selection, (ui.rect.0, ui.rect.1), ui.scale);
        let cropped = crop_rgba(&ui.pixels, ui.width as i32, ui.height as i32, wanted);
        self.cancel_screenshot();
        match cropped {
            Some((rgba, width, height)) => self.queue_save(
                Captured {
                    rgba,
                    width: width as u32,
                    height: height as u32,
                },
                false,
            ),
            None => report_failure("the selection is empty"),
        }
    }

    /// Mouse button while the area selection is open. Only the left one draws.
    pub fn screenshot_button(&mut self, button: u32, state: smithay::backend::input::ButtonState) {
        if button != BTN_LEFT {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let at = pointer.current_location();
        let Some(ui) = self.screenshot.as_mut() else {
            return;
        };
        match state {
            smithay::backend::input::ButtonState::Pressed => ui.selector.press((at.x, at.y)),
            smithay::backend::input::ButtonState::Released => ui.selector.release((at.x, at.y)),
        }
    }

    /// Pointer moved while the area selection is open.
    pub fn screenshot_motion(&mut self, at: Point<f64, Logical>) {
        if let Some(ui) = self.screenshot.as_mut() {
            ui.selector.motion((at.x, at.y));
        }
    }
}

/// Report that a screenshot did not work, in the log and as a notification.
pub fn report_failure(why: &str) {
    tracing::warn!("screenshot failed: {why}");
    notify("Screenshot failed", why);
}

/// The area selection as render elements, front to back: frame around the selection, dimmed
/// rest, frozen monitor. They stand in for everything but the cursor on the frozen monitor.
pub fn screenshot_elements(renderer: &mut GlesRenderer, ui: &ScreenshotUi) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();
    let origin = (ui.rect.0, ui.rect.1);
    let full: Rect = (0, 0, ui.width as i32, ui.height as i32);
    // Selection in picture pixels, kept inside the picture.
    let selection = ui
        .selector
        .rect()
        .map(|rect| physical_rect(rect, origin, ui.scale))
        .and_then(|rect| clip(rect, full));

    let mut push = |id: &Id, (x, y, w, h): Rect, color: [f32; 4]| {
        if w > 0 && h > 0 {
            out.push(JffElement::Solid(SolidColorRenderElement::new(
                id.clone(),
                Rectangle::<i32, Physical>::new((x, y).into(), (w, h).into()),
                CommitCounter::default(),
                color,
                Kind::Unspecified,
            )));
        }
    };
    if let Some(selection) = selection {
        for (id, bar) in ui.ids[..4].iter().zip(frame_bars(selection, BORDER)) {
            push(id, bar, BORDER_COLOR);
        }
    }
    for (id, area) in ui.ids[4..].iter().zip(around(full, selection)) {
        push(id, area, DIM_COLOR);
    }

    let logical: Size<i32, Logical> = (ui.rect.2, ui.rect.3).into();
    match MemoryRenderBufferRenderElement::from_buffer(
        &mut *renderer,
        (0.0, 0.0),
        &ui.buffer,
        None,
        None,
        Some(logical),
        Kind::Unspecified,
    ) {
        Ok(element) => out.push(JffElement::Memory(element)),
        Err(err) => tracing::warn!("error importing the frozen picture: {err:?}"),
    }
    out
}

/// Owns the pointer while the area selection is open: nobody but the selection sees it.
pub struct ScreenshotGrab {
    start_data: GrabStartData<JffState>,
}

impl PointerGrab<JffState> for ScreenshotGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);
        data.screenshot_motion(event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    // Buttons are handled in `input.rs` before they reach the pointer. Anything that still gets
    // here has no focus to go to.
    fn button(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
    }

    fn axis(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut JffState, handle: &mut PointerInnerHandle<'_, JffState>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }
    fn gesture_swipe_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }
    fn gesture_swipe_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }
    fn gesture_pinch_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }
    fn gesture_pinch_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }
    fn gesture_pinch_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }
    fn gesture_hold_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }
    fn gesture_hold_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<JffState> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut JffState) {}
}

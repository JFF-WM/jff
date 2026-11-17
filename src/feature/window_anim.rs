//! Window open and close fades (`window-animations`, on by default).
//!
//! * Open: starting from when the window first had something to show (`Entry::shown`) its
//!   alpha goes 0 -> 1, and for some styles it also scales or moves, see [`open_frame`].
//! * Close: a destroyed window is gone immediately, so just before removing it we take a
//!   picture (`JffState::capture_window`, the same one the window screenshot uses) and keep
//!   drawing that picture, fading out, as a [`Ghost`]. If the picture can't be taken (the
//!   surface is already dead) the window just disappears, like before.
//!
//! * Minimize and restore: a window that a taskbar minimized, after the taskbar told where its
//!   button is (`wlr-foreign-toplevel` `set_rectangle`), is kept as a [`Ghost`] too, and that one
//!   flies into the button while it fades. Restoring it flies the same picture back out of the
//!   button to the window's place, see [`GhostMotion`].
//!
//! Nothing here schedules frames: while a fade runs, `JffState::animations_ongoing` keeps the
//! monitor drawing.

use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::Window;
use smithay::utils::{Logical, Physical, Point, Scale, Size, Transform};

use crate::config::OpenStyle;
use crate::feature::screenshot::capture::Captured;
use crate::primitives::layout::Rect;
use crate::render::effects::rounded::is_fullscreen;
use crate::render::elements::JffElement;
use crate::state::JffState;

fn progress(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn ease_out_back(t: f32) -> f32 {
    const C: f32 = 1.4;
    1.0 + (C + 1.0) * (t - 1.0).powi(3) + C * (t - 1.0).powi(2)
}

/// How far a sliding window starts below its place, in pixels.
const SLIDE_PX: f32 = 40.0;

/// How a window looks at one moment of its opening.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenFrame {
    pub alpha: f32,
    /// Size relative to the real one, around the window's center.
    pub scale: f64,
    /// Pixels below its place.
    pub dy: i32,
    /// How opaque the window is drawn all the time (a `window-rule`'s `opacity`), on top of
    /// `alpha`. It has nothing to do with opening, so it does not keep a window from being
    /// [`settled`](OpenFrame::settled).
    pub opacity: f32,
}

impl OpenFrame {
    /// Fully open.
    pub const DONE: Self = Self {
        alpha: 1.0,
        scale: 1.0,
        dy: 0,
        opacity: 1.0,
    };

    pub fn settled(&self) -> bool {
        self.alpha == Self::DONE.alpha && self.scale == Self::DONE.scale && self.dy == Self::DONE.dy
    }

    /// The same frame for a window drawn with `opacity`.
    pub fn with_opacity(self, opacity: f32) -> Self {
        Self { opacity, ..self }
    }
}

/// The look of a window that first showed at `shown`. Fully open for a window without a
/// `shown` time and once `open` has passed.
pub fn open_frame(
    style: OpenStyle,
    shown: Option<Instant>,
    now: Instant,
    open: Duration,
) -> OpenFrame {
    let Some(shown) = shown else {
        return OpenFrame::DONE;
    };
    let t = progress(now.saturating_duration_since(shown), open);
    if t >= 1.0 || style == OpenStyle::None {
        return OpenFrame::DONE;
    }
    let eased = ease_out_cubic(t);
    let mut frame = OpenFrame {
        alpha: eased,
        ..OpenFrame::DONE
    };
    match style {
        OpenStyle::Fade | OpenStyle::None => {}
        OpenStyle::Zoom => frame.scale = f64::from(0.85 + 0.15 * eased),
        OpenStyle::Slide => frame.dy = (SLIDE_PX * (1.0 - eased)).round() as i32,
        OpenStyle::Pop => {
            frame.alpha = (t * 3.0).min(1.0);
            frame.scale = f64::from(0.6 + 0.4 * ease_out_back(t));
        }
    }
    frame
}

/// Opacity alone, for a plain fade.
pub fn open_alpha(shown: Option<Instant>, now: Instant, open: Duration) -> f32 {
    open_frame(OpenStyle::Fade, shown, now, open).alpha
}

/// What a [`Ghost`] does while it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostMotion {
    /// A closed window: fades out where it was.
    Stay,
    /// A minimized window: shrinks and moves into this rectangle (a taskbar button, in the
    /// shared space) while it fades.
    Minimize(Rect),
    /// A restored window: grows out of this rectangle to the place of the window.
    Restore(Rect),
}

fn ease_in_out(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

fn lerp_rect(from: Rect, to: Rect, t: f64) -> (f64, f64, f64, f64) {
    (
        lerp(f64::from(from.0), f64::from(to.0), t),
        lerp(f64::from(from.1), f64::from(to.1), t),
        lerp(f64::from(from.2), f64::from(to.2), t),
        lerp(f64::from(from.3), f64::from(to.3), t),
    )
}

/// How a ghost of the window at `window` (x, y, width, height) looks `t` of the way through its
/// animation (0 to 1): its opacity and its rectangle, in the shared space.
pub fn ghost_frame(motion: GhostMotion, window: Rect, t: f32) -> (f32, (f64, f64, f64, f64)) {
    let t = t.clamp(0.0, 1.0);
    let here = (
        f64::from(window.0),
        f64::from(window.1),
        f64::from(window.2),
        f64::from(window.3),
    );
    match motion {
        GhostMotion::Stay => (1.0 - ease_out_cubic(t), here),
        // Stays visible until it is nearly in the button.
        GhostMotion::Minimize(target) => (
            1.0 - t * t,
            lerp_rect(window, target, f64::from(ease_in_out(t))),
        ),
        GhostMotion::Restore(from) => (
            (t * 2.0).min(1.0),
            lerp_rect(from, window, f64::from(ease_in_out(t))),
        ),
    }
}

/// The picture of a window that went away, minimized or came back, fading and moving.
pub struct Ghost {
    buffer: MemoryRenderBuffer,
    /// What the picture shows, in logical pixels: it has more pixels on a scaled monitor.
    size: Size<i32, Logical>,
    /// Top-left corner of the window geometry, in the shared space.
    pos: Point<i32, Logical>,
    /// Workspace it was on: only drawn while that one is shown.
    workspace: usize,
    started: Instant,
    motion: GhostMotion,
}

impl Ghost {
    /// `picture` is premultiplied RGBA, as `capture_window` returns it.
    pub fn new(
        picture: &Captured,
        pos: (i32, i32),
        size: Size<i32, Logical>,
        workspace: usize,
    ) -> Self {
        Self::moving(picture, pos, size, workspace, GhostMotion::Stay)
    }

    /// A ghost that does `motion` (`pos` and `size` are the window's).
    pub fn moving(
        picture: &Captured,
        pos: (i32, i32),
        size: Size<i32, Logical>,
        workspace: usize,
        motion: GhostMotion,
    ) -> Self {
        Self {
            size,
            buffer: MemoryRenderBuffer::from_slice(
                &picture.rgba,
                Fourcc::Abgr8888,
                (picture.width as i32, picture.height as i32),
                1,
                Transform::Normal,
                None,
            ),
            pos: pos.into(),
            workspace,
            started: Instant::now(),
            motion,
        }
    }
}

/// Drop ghosts that have finished fading (`close` long) and draw the rest (those on
/// `workspace`) with the frame's top-left at `origin`, as they look at `now`. Push the result
/// before the windows' elements: ghosts go on top.
pub fn ghost_elements(
    renderer: &mut GlesRenderer,
    ghosts: &mut Vec<Ghost>,
    workspace: usize,
    origin: Point<i32, Logical>,
    close: Duration,
    now: Instant,
    out: &mut Vec<JffElement>,
) {
    ghosts.retain(|ghost| now.saturating_duration_since(ghost.started) < close);
    for ghost in ghosts.iter().filter(|g| g.workspace == workspace) {
        let t = progress(now.saturating_duration_since(ghost.started), close);
        let window = (ghost.pos.x, ghost.pos.y, ghost.size.w, ghost.size.h);
        let (alpha, (x, y, w, h)) = ghost_frame(ghost.motion, window, t);
        let size: Size<i32, Logical> =
            (w.round().max(1.0) as i32, h.round().max(1.0) as i32).into();
        let loc: Point<f64, Physical> =
            Point::<f64, Logical>::from((x - f64::from(origin.x), y - f64::from(origin.y)))
                .to_physical(Scale::from(crate::render::effects::frame_scale::get()));
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            loc,
            &ghost.buffer,
            Some(alpha),
            None,
            Some(size),
            Kind::Unspecified,
        ) {
            Ok(element) => out.push(JffElement::Memory(element)),
            Err(err) => tracing::warn!("error drawing a closing window: {err:?}"),
        }
    }
}

impl JffState {
    /// The ghost of `window` that flies into `target` (a taskbar button) when it is minimized,
    /// taken now, while it is still shown. `None` when there is nothing to show, for the same
    /// reasons as [`JffState::start_close_fade`].
    pub fn minimize_ghost(&mut self, window: &Window, target: Rect) -> Option<Ghost> {
        let entry = self.desk.find_by(|w| w == window)?;
        let (pos, workspace) = (entry.pos, entry.workspace);
        if !self.can_animate_window(window) {
            return None;
        }
        let color = self.window_output_color(window);
        match self.capture_window_in(window, color) {
            Ok(picture) => Some(Ghost::moving(
                &picture,
                pos,
                window.geometry().size,
                workspace,
                GhostMotion::Minimize(target),
            )),
            Err(why) => {
                tracing::debug!("no minimize animation: {why}");
                None
            }
        }
    }

    /// Fly `window`, which was just brought back from a minimize, out of `from` (the taskbar
    /// button it went into) to its place, and let the window itself fade in under the picture.
    pub fn start_restore_fly(&mut self, window: &Window, from: Rect) {
        let Some(entry) = self.desk.find_by(|w| w == window) else {
            return;
        };
        let (pos, workspace) = (entry.pos, entry.workspace);
        if !self.can_animate_window(window) {
            return;
        }
        let color = self.window_output_color(window);
        match self.capture_window_in(window, color) {
            Ok(picture) => {
                self.closing.push(Ghost::moving(
                    &picture,
                    pos,
                    window.geometry().size,
                    workspace,
                    GhostMotion::Restore(from),
                ));
                self.desk.mark_shown_where(|w| w == window);
            }
            Err(why) => tracing::debug!("no restore animation: {why}"),
        }
    }

    /// Whether a minimize or restore picture of `window` can be drawn: animations are on, the
    /// overview is closed, the window is not fullscreen and is shown (or about to be) on a
    /// workspace that is shown.
    fn can_animate_window(&self, window: &Window) -> bool {
        self.config.window_animations()
            && self.overview.is_none()
            && !is_fullscreen(window)
            && self
                .desk
                .find_by(|w| w == window)
                .is_some_and(|entry| entry.workspace == self.desk.current_on(&entry.output))
    }

    /// Keep a fading picture of `window` (at `pos`, on `workspace`) for when it's removed.
    /// No-op when there's nothing to show: a fullscreen window, the overview open, a window on
    /// a workspace that isn't shown, or a surface that's already gone.
    pub fn start_close_fade(&mut self, window: &Window, pos: (i32, i32), workspace: usize) {
        if !self.config.window_animations()
            || self.overview.is_some()
            || !self
                .desk
                .find_by(|w| w == window)
                .is_some_and(|entry| self.desk.is_visible(entry))
            || is_fullscreen(window)
        {
            return;
        }
        // The picture is made in the colors of the monitor it fades on, which draws it as it is.
        let color = self.window_output_color(window);
        match self.capture_window_in(window, color) {
            Ok(picture) => {
                self.closing
                    .push(Ghost::new(&picture, pos, window.geometry().size, workspace))
            }
            Err(why) => tracing::debug!("no close animation: {why}"),
        }
    }
}

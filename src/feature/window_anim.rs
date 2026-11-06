//! Window open and close fades (`window-animations`, on by default).
//!
//! * Open: the window's alpha goes 0 -> 1 starting from when it first had something to show
//!   (`Entry::shown`), see [`open_alpha`].
//! * Close: a destroyed window is gone immediately, so just before removing it we take a
//!   picture (`JffState::capture_window`, the same one the window screenshot uses) and keep
//!   drawing that picture, fading out, as a [`Ghost`]. If the picture can't be taken (the
//!   surface is already dead) the window just disappears, like before.
//!
//! Frame loops already redraw every refresh, so nothing here schedules frames.

use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::Window;
use smithay::utils::{Logical, Physical, Point, Scale, Transform};

use crate::feature::screenshot::capture::Captured;
use crate::render::effects::rounded::is_fullscreen;
use crate::render::elements::JffElement;
use crate::state::JffState;

/// How long a window takes to fade in.
pub const OPEN: Duration = Duration::from_millis(100);
/// How long a closed window's picture takes to fade out.
pub const CLOSE: Duration = Duration::from_millis(80);

fn progress(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Opacity of a window that first showed at `shown`. 1.0 for a window without a `shown` time
/// (nothing to fade) and once the fade is done.
pub fn open_alpha(shown: Option<Instant>, now: Instant) -> f32 {
    match shown {
        Some(shown) => ease_out_cubic(progress(now.saturating_duration_since(shown), OPEN)),
        None => 1.0,
    }
}

/// The last picture of a closed window, fading out where it was.
pub struct Ghost {
    buffer: MemoryRenderBuffer,
    /// Top-left corner of the window geometry, in the shared space.
    pos: Point<i32, Logical>,
    /// Workspace it was on: only drawn while that one is shown.
    workspace: usize,
    closed: Instant,
}

impl Ghost {
    /// `picture` is premultiplied RGBA, as `capture_window` returns it.
    pub fn new(picture: &Captured, pos: (i32, i32), workspace: usize) -> Self {
        Self {
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
            closed: Instant::now(),
        }
    }
}

/// Drop ghosts that have finished fading and draw the rest (those on `workspace`) with the
/// frame's top-left at `origin`. Push the result before the windows' elements: ghosts go on
/// top. Drawn at scale 1, like the capture they came from.
pub fn ghost_elements(
    renderer: &mut GlesRenderer,
    ghosts: &mut Vec<Ghost>,
    workspace: usize,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let now = Instant::now();
    ghosts.retain(|ghost| now.saturating_duration_since(ghost.closed) < CLOSE);
    for ghost in ghosts.iter().filter(|g| g.workspace == workspace) {
        let alpha =
            1.0 - ease_out_cubic(progress(now.saturating_duration_since(ghost.closed), CLOSE));
        let loc: Point<f64, Physical> = (ghost.pos - origin).to_f64().to_physical(Scale::from(1.0));
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            loc,
            &ghost.buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            Ok(element) => out.push(JffElement::Memory(element)),
            Err(err) => tracing::warn!("error drawing a closing window: {err:?}"),
        }
    }
}

impl JffState {
    /// Keep a fading picture of `window` (at `pos`, on `workspace`) for when it's removed.
    /// No-op when there's nothing to show: a fullscreen window, the overview open, a window on
    /// a workspace that isn't shown, or a surface that's already gone.
    pub fn start_close_fade(&mut self, window: &Window, pos: (i32, i32), workspace: usize) {
        if !self.config.window_animations()
            || self.overview.is_some()
            || workspace != self.desk.current()
            || is_fullscreen(window)
        {
            return;
        }
        match self.capture_window(window) {
            Ok(picture) => self.closing.push(Ghost::new(&picture, pos, workspace)),
            Err(why) => tracing::debug!("no close animation: {why}"),
        }
    }
}

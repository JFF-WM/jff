//! The screen magnifier: zooms the screen in around the pointer, for people who need the screen
//! larger than it is.
//!
//! The `zoom-in`, `zoom-out` and `zoom-reset` actions change the zoom by `zoom { step }` (up to
//! `zoom { max }`), scrolling the wheel with `zoom { scroll-modifier }` held does the same, and
//! `zoom-pan-*` moves the part that is shown. The zoom is of the monitor the pointer is on. How
//! the part shown follows the pointer is `zoom { tracking }`, and what the pointer looks like
//! is `zoom { pointer }`. The math is in `primitives/zoom.rs`, the drawing in
//! `render/effects/magnifier.rs`.
//!
//! Only what is drawn is zoomed: clicks go to what is under the pointer on the real desktop,
//! which is what the zoomed screen shows under it, so nothing needs translating. A touchscreen
//! reports where the finger is on the zoomed screen, so it does not line up while zoomed.
//!
//! While the screen is zoomed the frame is composited (no direct scanout), and screenshots and
//! screen captures are of the real, unzoomed screen.

use std::collections::HashMap;
use std::time::Instant;

use smithay::backend::input::AxisSource;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::utils::{Buffer, Logical, Rectangle};

use crate::feature::outputs::output_rect;
use crate::primitives::layout;
use crate::primitives::scroll_steps::ScrollSteps;
use crate::primitives::zoom::{Pan, PointerStyle, Tracking, View, Zoom, MIN_LEVEL};
use crate::render::effects::magnifier::{MagnifiedElement, MagnifierFrame};
use crate::render::elements::JffElement;
use crate::state::JffState;

/// Levels this close to 1 are not drawn as a zoom.
const NEARLY_ONE: f64 = 1e-3;
/// How much of a touchpad's scroll counts as one wheel notch, in pixels.
const NOTCH_PX: f64 = 15.0;

/// The zoom of the screen and what is kept to draw it.
pub struct ZoomState {
    zoom: Zoom,
    /// What has been scrolled and not yet turned into a step.
    scroll: ScrollSteps,
    /// The frame of every monitor that is zoomed right now, by output name.
    frames: HashMap<String, MagnifierFrame>,
    /// Whether a failure to draw was told already, so a frame that keeps failing says so once.
    told: bool,
}

impl ZoomState {
    pub fn new() -> Self {
        Self {
            zoom: Zoom::new(Instant::now()),
            scroll: ScrollSteps::default(),
            frames: HashMap::new(),
            told: false,
        }
    }

    /// Composite `scene` into the frame of `output` and give back the element that draws the
    /// zoomed part of it over the whole monitor, with the states of what was composited. `None`
    /// when it can't be done, the caller draws the scene as it is then.
    pub fn magnify(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        scene: &[JffElement],
        clear: [f32; 4],
        plan: &ZoomPlan,
    ) -> Option<(MagnifiedElement, RenderElementStates)> {
        let name = output.name();
        if !self.frames.get(&name).is_some_and(|f| f.fits(output)) {
            match MagnifierFrame::new(renderer, output) {
                Ok(frame) => {
                    self.frames.insert(name.clone(), frame);
                }
                Err(err) => {
                    self.frames.remove(&name);
                    if !std::mem::replace(&mut self.told, true) {
                        tracing::warn!("monitor {name}: cannot make the zoom frame: {err}");
                    }
                    return None;
                }
            }
        }
        let frame = self.frames.get_mut(&name)?;
        match frame.render(renderer, scene, clear, plan.rect()) {
            Ok(drawn) => {
                self.told = false;
                Some(drawn)
            }
            Err(err) => {
                self.frames.remove(&name);
                if !std::mem::replace(&mut self.told, true) {
                    tracing::warn!("monitor {name}: error drawing the zoomed frame: {err}");
                }
                None
            }
        }
    }
}

/// What one monitor shows this frame: which part of it, and what the pointer looks like.
#[derive(Clone, Copy, Debug)]
pub struct ZoomPlan {
    pub view: View,
    pub pointer: PointerStyle,
}

impl ZoomPlan {
    /// The part of the monitor that is shown, in its logical pixels.
    pub fn rect(&self) -> Rectangle<f64, Logical> {
        let (x, y, w, h) = self.view.rect();
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// The same part in the pixels of the monitor's frame, at `scale`.
    pub fn src(&self, scale: f64) -> Rectangle<f64, Buffer> {
        let (x, y, w, h) = self.view.rect();
        Rectangle::new((x * scale, y * scale).into(), (w * scale, h * scale).into())
    }
}

impl JffState {
    /// The monitor the pointer is on, as `(x, y, width, height)`.
    fn zoom_output_rect(&self) -> Option<layout::Rect> {
        let at = self.seat.get_pointer()?.current_location();
        self.outputs
            .iter()
            .map(output_rect)
            .find(|rect| layout::contains(*rect, (at.x, at.y)))
    }

    /// Zoom in by one step.
    pub fn zoom_in(&mut self) {
        let settings = self.config.zoom();
        self.zoom.zoom.zoom_in(
            Instant::now(),
            settings.step,
            settings.max,
            settings.animation,
        );
        self.queue_redraw_all();
    }

    /// Zoom out by one step.
    pub fn zoom_out(&mut self) {
        let settings = self.config.zoom();
        self.zoom
            .zoom
            .zoom_out(Instant::now(), settings.step, settings.animation);
        self.queue_redraw_all();
    }

    /// Back to the screen as it is.
    pub fn zoom_reset(&mut self) {
        let settings = self.config.zoom();
        self.zoom.zoom.reset(Instant::now(), settings.animation);
        self.queue_redraw_all();
    }

    /// Move the part that is shown.
    pub fn zoom_pan(&mut self, way: Pan) {
        let Some(rect) = self.zoom_output_rect() else {
            return;
        };
        let size = (f64::from(rect.2), f64::from(rect.3));
        self.zoom.zoom.pan(Instant::now(), way, size);
        self.queue_redraw_all();
    }

    /// The wheel (or a touchpad) turned while the zoom modifier may be held: zooms by a step per
    /// notch, scrolling up zooms in. True when the scroll was used up here, false when it
    /// belongs to what is under the pointer.
    pub fn zoom_scroll(
        &mut self,
        source: AxisSource,
        amount: f64,
        v120: Option<f64>,
        stop: bool,
    ) -> bool {
        let held = self
            .seat
            .get_keyboard()
            .is_some_and(|k| self.config.zoom().scroll_modifier_held(&k.modifier_state()));
        if !held || self.session_locked() {
            self.zoom.scroll.reset();
            return false;
        }
        if stop {
            self.zoom.scroll.reset();
            return true;
        }
        let notches = match (source, v120) {
            (AxisSource::Wheel | AxisSource::WheelTilt, Some(v120)) => v120,
            _ => amount * 120.0 / NOTCH_PX,
        };
        let steps = self.zoom.scroll.wheel(notches);
        for _ in 0..steps.unsigned_abs() {
            // scrolling down is positive
            if steps < 0 {
                self.zoom_in();
            } else {
                self.zoom_out();
            }
        }
        true
    }

    /// What `output` shows this frame at `now`, `None` when it is not zoomed: the zoom is of the
    /// monitor the pointer is on. Call it once per frame, it moves the part shown along with the
    /// pointer.
    pub fn zoom_plan(&mut self, output: &Output, now: Instant) -> Option<ZoomPlan> {
        let rect = output_rect(output);
        let at = self.seat.get_pointer()?.current_location();
        let zoomed = self.zoom.zoom.active(now) && layout::contains(rect, (at.x, at.y));
        let settings = self.config.zoom();
        // The host window's own pointer is where the real one is, so a view that puts the pointer
        // elsewhere would be wrong there.
        let tracking = if self.backend.is_tty() {
            settings.tracking
        } else {
            Tracking::Proportional
        };
        let view = zoomed.then(|| {
            self.zoom.zoom.view(
                now,
                tracking,
                (at.x - f64::from(rect.0), at.y - f64::from(rect.1)),
                (f64::from(rect.2), f64::from(rect.3)),
            )
        });
        match view {
            Some(view) if view.level > MIN_LEVEL + NEARLY_ONE => Some(ZoomPlan {
                view,
                pointer: settings.pointer,
            }),
            _ => {
                self.zoom.frames.remove(&output.name());
                None
            }
        }
    }

    /// Whether the zoom is changing, so frames have to keep coming.
    pub fn zoom_animating(&self) -> bool {
        self.zoom.zoom.animating(Instant::now())
    }
}

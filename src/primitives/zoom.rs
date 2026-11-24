//! The screen magnifier's math: how far the screen is zoomed, which part of it shows, and how
//! that part follows the pointer. No Smithay types, so it can be tested on its own.
//!
//! Positions are in the logical pixels of one monitor, from its top left. The view is the part
//! of the monitor that fills the whole screen: `origin` is its top left corner and the size it
//! has is the monitor's size divided by the zoom level.

use std::time::{Duration, Instant};

/// No zoom: the screen as it is.
pub const MIN_LEVEL: f64 = 1.0;
/// The most the screen can be zoomed in, whatever `max` the config asks for.
pub const MAX_LEVEL: f64 = 64.0;
/// How much one zoom in (or out) step multiplies the level by.
pub const DEFAULT_STEP: f64 = 1.2;
/// The default of `max`: the level `zoom-in` stops at.
pub const DEFAULT_MAX: f64 = 20.0;
/// How long a change of the level takes by default.
pub const DEFAULT_ANIMATION: Duration = Duration::from_millis(200);

/// How close to the edge of the shown part the pointer gets, as a fraction of it, before
/// `push` tracking moves the view along.
const PUSH_MARGIN: f64 = 0.05;
/// The fraction of the shown part a pan step moves it by.
const PAN_FRACTION: f64 = 0.1;
/// Levels this close to 1 are no zoom at all.
const EPSILON: f64 = 1e-3;

/// How the shown part follows the pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tracking {
    /// The pointer stays over what it points at, the part shown slides with it from one edge of
    /// the screen to the other. The default.
    #[default]
    Proportional,
    /// The pointer is in the middle of the shown part, as far as the edges of the screen allow.
    Centered,
    /// The part shown stands still until the pointer reaches its edge, then it is pushed along.
    Push,
    /// The part shown does not follow the pointer, only a pan moves it.
    Disabled,
}

/// What the pointer looks like while zoomed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerStyle {
    /// Magnified with the rest of the screen. The default.
    #[default]
    Scale,
    /// At its normal size, on top of the magnified screen.
    Keep,
    /// Not drawn.
    Hide,
}

/// Which way a pan step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pan {
    Left,
    Right,
    Up,
    Down,
}

/// The part of a monitor that is shown, enlarged to fill it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// How many times the screen is enlarged, 1 or more.
    pub level: f64,
    /// Top left of the part shown.
    pub origin: (f64, f64),
    /// The whole monitor.
    pub size: (f64, f64),
}

impl View {
    /// The size of the part shown.
    pub fn shown(&self) -> (f64, f64) {
        (self.size.0 / self.level, self.size.1 / self.level)
    }

    /// The part shown as `(x, y, width, height)`.
    pub fn rect(&self) -> (f64, f64, f64, f64) {
        let (w, h) = self.shown();
        (self.origin.0, self.origin.1, w, h)
    }

    /// Where a point of the monitor ends up on the zoomed screen.
    pub fn to_screen(&self, p: (f64, f64)) -> (f64, f64) {
        (
            (p.0 - self.origin.0) * self.level,
            (p.1 - self.origin.1) * self.level,
        )
    }

    /// The point of the monitor that is shown at `p` of the zoomed screen.
    pub fn from_screen(&self, p: (f64, f64)) -> (f64, f64) {
        (
            self.origin.0 + p.0 / self.level,
            self.origin.1 + p.1 / self.level,
        )
    }
}

/// The zoom level of a screen, how it is changing, and where the part shown is.
#[derive(Clone, Copy, Debug)]
pub struct Zoom {
    /// The level the current change started from.
    from: f64,
    /// The level it is going to, the one that counts for the next step.
    to: f64,
    started: Instant,
    duration: Duration,
    /// Top left of the part shown, kept for the tracking modes that don't work it out again
    /// from the pointer every time.
    origin: (f64, f64),
    /// The level `origin` was last worked out for.
    seen: f64,
}

impl Zoom {
    /// No zoom.
    pub fn new(now: Instant) -> Self {
        Self {
            from: MIN_LEVEL,
            to: MIN_LEVEL,
            started: now,
            duration: Duration::ZERO,
            origin: (0.0, 0.0),
            seen: MIN_LEVEL,
        }
    }

    /// The level at `now`. A change runs on a logarithmic scale, so going from 1 to 2 looks as
    /// fast as going from 2 to 4, and it slows down towards the end.
    pub fn level(&self, now: Instant) -> f64 {
        if self.from == self.to {
            return self.to;
        }
        let spent = now.saturating_duration_since(self.started);
        if self.duration.is_zero() || spent >= self.duration {
            return self.to;
        }
        let t = spent.as_secs_f64() / self.duration.as_secs_f64();
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from * (self.to / self.from).powf(eased)
    }

    /// The level the screen is going to.
    pub fn target(&self) -> f64 {
        self.to
    }

    /// Whether the level is still changing.
    pub fn animating(&self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.started) < self.duration
    }

    /// Whether the screen is zoomed, or on its way back to normal.
    pub fn active(&self, now: Instant) -> bool {
        self.level(now) > MIN_LEVEL + EPSILON || self.to > MIN_LEVEL + EPSILON
    }

    /// Go to `level` (between 1 and `MAX_LEVEL`), taking `duration` from wherever it is now.
    pub fn set_target(&mut self, now: Instant, level: f64, duration: Duration) {
        let level = level.clamp(MIN_LEVEL, MAX_LEVEL);
        let level = if level < MIN_LEVEL + EPSILON {
            MIN_LEVEL
        } else {
            level
        };
        if (level - self.to).abs() < f64::EPSILON {
            return;
        }
        self.from = self.level(now);
        // Starting from no zoom, the view starts from the corner too, not from where an earlier
        // zoom left it.
        if self.from <= MIN_LEVEL + EPSILON {
            self.origin = (0.0, 0.0);
            self.seen = MIN_LEVEL;
        }
        self.to = level;
        self.started = now;
        self.duration = duration;
    }

    /// One step closer, `step` times as much, up to `max`.
    pub fn zoom_in(&mut self, now: Instant, step: f64, max: f64, duration: Duration) {
        self.set_target(now, (self.to * step).min(max.min(MAX_LEVEL)), duration);
    }

    /// One step further away, down to no zoom.
    pub fn zoom_out(&mut self, now: Instant, step: f64, duration: Duration) {
        self.set_target(now, self.to / step, duration);
    }

    /// Back to the screen as it is.
    pub fn reset(&mut self, now: Instant, duration: Duration) {
        self.set_target(now, MIN_LEVEL, duration);
    }

    /// Move the part shown by a tenth of its size, on a monitor of `size`. Tracking that
    /// follows the pointer works it out again on the next frame, so this is for `push` and
    /// `disabled`.
    pub fn pan(&mut self, now: Instant, way: Pan, size: (f64, f64)) {
        let level = self.level(now).max(MIN_LEVEL);
        let (w, h) = (size.0 / level, size.1 / level);
        match way {
            Pan::Left => self.origin.0 -= w * PAN_FRACTION,
            Pan::Right => self.origin.0 += w * PAN_FRACTION,
            Pan::Up => self.origin.1 -= h * PAN_FRACTION,
            Pan::Down => self.origin.1 += h * PAN_FRACTION,
        }
        self.origin = clamp_origin(self.origin, level, size);
    }

    /// What to show at `now` on a monitor of `size` with the pointer at `pointer`. Call it once
    /// per frame: the tracking modes that keep the view in between depend on it.
    pub fn view(
        &mut self,
        now: Instant,
        tracking: Tracking,
        pointer: (f64, f64),
        size: (f64, f64),
    ) -> View {
        let level = self.level(now).max(MIN_LEVEL);
        // A change of the level leaves what is under the pointer where it is on the screen.
        if (level - self.seen).abs() > f64::EPSILON {
            let k = self.seen / level;
            self.origin = (
                pointer.0 - (pointer.0 - self.origin.0) * k,
                pointer.1 - (pointer.1 - self.origin.1) * k,
            );
            self.seen = level;
        }
        let shown = (size.0 / level, size.1 / level);
        self.origin = match tracking {
            Tracking::Proportional => (
                pointer.0 * (1.0 - 1.0 / level),
                pointer.1 * (1.0 - 1.0 / level),
            ),
            Tracking::Centered => (pointer.0 - shown.0 / 2.0, pointer.1 - shown.1 / 2.0),
            Tracking::Push => (
                pushed(self.origin.0, pointer.0, shown.0),
                pushed(self.origin.1, pointer.1, shown.1),
            ),
            Tracking::Disabled => self.origin,
        };
        self.origin = clamp_origin(self.origin, level, size);
        View {
            level,
            origin: self.origin,
            size,
        }
    }
}

/// `origin` moved along an axis just far enough that `pointer` is not within the margin of the
/// edge of the `shown` part that starts there.
fn pushed(origin: f64, pointer: f64, shown: f64) -> f64 {
    let margin = shown * PUSH_MARGIN;
    if pointer < origin + margin {
        pointer - margin
    } else if pointer > origin + shown - margin {
        pointer + margin - shown
    } else {
        origin
    }
}

/// `origin` moved back inside the monitor: the part shown never reaches past its edges.
fn clamp_origin(origin: (f64, f64), level: f64, size: (f64, f64)) -> (f64, f64) {
    let room = (size.0 - size.0 / level, size.1 - size.1 / level);
    (origin.0.clamp(0.0, room.0), origin.1.clamp(0.0, room.1))
}

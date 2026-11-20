//! Turning scroll events into whole steps: one per wheel notch, one per touchpad gesture.

use std::time::{Duration, Instant};

/// Touchpad distance, in pixels, that makes a gesture count as a step.
const TOUCH_THRESHOLD: f64 = 40.0;
/// One wheel notch in the `v120` unit.
const NOTCH: f64 = 120.0;

/// What has been scrolled but not yet turned into a step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollSteps {
    acc: f64,
    /// Touchpad only: false after a step until the fingers lift, so one gesture is one step.
    armed: bool,
    /// Wheel only, for `wheel_paced`: when the last step was made.
    last_step: Option<Instant>,
}

impl Default for ScrollSteps {
    fn default() -> Self {
        Self {
            acc: 0.0,
            armed: true,
            last_step: None,
        }
    }
}

impl ScrollSteps {
    /// Forget everything pending.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A wheel moved by `v120` (120 per notch, positive is down): the steps it completes.
    pub fn wheel(&mut self, v120: f64) -> i32 {
        if v120 * self.acc < 0.0 {
            self.acc = 0.0; // turned around: the half notch the other way doesn't count
        }
        self.acc += v120;
        let steps = (self.acc / NOTCH).trunc();
        self.acc -= steps * NOTCH;
        steps as i32
    }

    /// Like `wheel`, for a slide that takes `cooldown`: at most one step, and none while the
    /// last one is still sliding. A fast spin or a run of notches is one step, not one per
    /// notch, and what is scrolled during the slide is dropped, not saved up for after it.
    pub fn wheel_paced(&mut self, v120: f64, now: Instant, cooldown: Duration) -> i32 {
        if let Some(last) = self.last_step {
            if now.saturating_duration_since(last) < cooldown {
                self.acc = 0.0;
                return 0;
            }
        }
        let step = self.wheel(v120).signum();
        if step != 0 {
            self.acc = 0.0;
            self.last_step = Some(now);
        }
        step
    }

    /// A touchpad moved by `px` (positive is down), or `stop` when the fingers lifted: the
    /// step it completes, at most one per gesture.
    pub fn touch(&mut self, px: f64, stop: bool) -> i32 {
        if stop {
            self.reset();
            return 0;
        }
        if !self.armed {
            return 0;
        }
        self.acc += px;
        if self.acc.abs() < TOUCH_THRESHOLD {
            return 0;
        }
        let step = self.acc.signum() as i32;
        self.acc = 0.0;
        self.armed = false;
        step
    }
}

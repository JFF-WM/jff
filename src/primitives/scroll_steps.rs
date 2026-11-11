//! Turning scroll events into whole steps: one per wheel notch, one per touchpad gesture.

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
}

impl Default for ScrollSteps {
    fn default() -> Self {
        Self {
            acc: 0.0,
            armed: true,
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

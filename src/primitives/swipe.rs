//! The touchpad swipes jff keeps for itself: three fingers switch workspace or open and close the overview.

/// Fingers of the swipe jff keeps, any other swipe goes to the client under the pointer.
pub const FINGERS: u32 = 3;
/// Touchpad distance, in pixels, along the main direction that makes a swipe count.
const THRESHOLD: f64 = 80.0;

/// Where the fingers went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swipe {
    Left,
    Right,
    Up,
    Down,
}

/// One touchpad swipe, from the first finger down to the last one up.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SwipeTracker {
    /// jff keeps this swipe, the client hears nothing about it.
    kept: bool,
    /// The swipe already counted, it counts once.
    fired: bool,
    dx: f64,
    dy: f64,
}

impl SwipeTracker {
    /// A swipe with `fingers` began. `allowed` is false while something else owns the input
    /// (the session is locked, a screenshot is being selected). Returns whether jff keeps it.
    pub fn begin(&mut self, fingers: u32, allowed: bool) -> bool {
        *self = Self {
            kept: allowed && fingers == FINGERS,
            ..Self::default()
        };
        self.kept
    }

    /// Whether the swipe going on is jff's.
    pub fn is_kept(&self) -> bool {
        self.kept
    }

    /// The fingers moved by `dx`, `dy`. Gives the direction once the swipe has gone far enough,
    /// then nothing more until the next swipe. Fingers moving left is `Left`.
    pub fn update(&mut self, dx: f64, dy: f64) -> Option<Swipe> {
        if !self.kept || self.fired {
            return None;
        }
        self.dx += dx;
        self.dy += dy;
        let (across, along) = (self.dx.abs(), self.dy.abs());
        if across.max(along) < THRESHOLD {
            return None;
        }
        self.fired = true;
        Some(match (across >= along, self.dx < 0.0, self.dy < 0.0) {
            (true, true, _) => Swipe::Left,
            (true, false, _) => Swipe::Right,
            (false, _, true) => Swipe::Up,
            (false, _, false) => Swipe::Down,
        })
    }

    /// The swipe ended. Returns whether it was jff's.
    pub fn end(&mut self) -> bool {
        let kept = self.kept;
        *self = Self::default();
        kept
    }
}

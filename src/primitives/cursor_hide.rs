//! When the cursor hides itself: while typing, after the pointer rests, and after it rests while
//! the focused window is fullscreen.

use std::time::{Duration, Instant};

/// The cursor's hiding state. Time comes in as an argument.
#[derive(Clone, Debug)]
pub struct CursorHide {
    when_typing: bool,
    after: Option<Duration>,
    /// Rest time that hides the cursor only while the caller says a fullscreen window has the
    /// focus (see `hidden_in`).
    fullscreen_after: Option<Duration>,
    typing: bool,
    idle: bool,
    fullscreen_idle: bool,
    last_pointer: Instant,
}

impl CursorHide {
    /// Nothing hides the cursor yet, the pointer counts as moved at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            when_typing: false,
            after: None,
            fullscreen_after: None,
            typing: false,
            idle: false,
            fullscreen_idle: false,
            last_pointer: now,
        }
    }

    /// Take new settings. What a setting that went away had hidden shows again.
    pub fn configure(&mut self, when_typing: bool, after: Option<Duration>) {
        self.when_typing = when_typing;
        self.after = after;
        if !when_typing {
            self.typing = false;
        }
        if after.is_none() {
            self.idle = false;
        }
    }

    /// Take the rest time that hides the cursor only under a fullscreen window, `None` for never.
    /// What a time that went away had hidden shows again.
    pub fn set_fullscreen_after(&mut self, after: Option<Duration>) {
        self.fullscreen_after = after;
        if after.is_none() {
            self.fullscreen_idle = false;
        }
    }

    /// The rest time that hides the cursor only under a fullscreen window.
    pub fn fullscreen_after(&self) -> Option<Duration> {
        self.fullscreen_after
    }

    /// How long the pointer rests before the cursor hides, `None` when it never does. With both
    /// kinds of rest time set, the shorter one: the later one is found out by `remaining`.
    pub fn after(&self) -> Option<Duration> {
        match (self.after, self.fullscreen_after) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A key went down.
    pub fn key_pressed(&mut self) {
        if self.when_typing {
            self.typing = true;
        }
    }

    /// The pointer moved, clicked or scrolled at `now`: the cursor is back.
    pub fn pointer_activity(&mut self, now: Instant) {
        self.typing = false;
        self.idle = false;
        self.fullscreen_idle = false;
        self.last_pointer = now;
    }

    /// How long until the next rest time is up, marking each one that is up by now. `None` when
    /// every rest time that is set has been reached (or none is set).
    pub fn remaining(&mut self, now: Instant) -> Option<Duration> {
        let rested = now.saturating_duration_since(self.last_pointer);
        let mut left: Option<Duration> = None;
        for (after, reached) in [
            (self.after, &mut self.idle),
            (self.fullscreen_after, &mut self.fullscreen_idle),
        ] {
            let Some(after) = after else { continue };
            if *reached {
                continue;
            }
            if rested >= after {
                *reached = true;
                continue;
            }
            let wait = after - rested;
            left = Some(left.map_or(wait, |l| l.min(wait)));
        }
        left
    }

    /// The cursor should not be drawn, whatever has the focus.
    pub fn hidden(&self) -> bool {
        self.typing || self.idle
    }

    /// The cursor should not be drawn, given whether it may hide for a fullscreen window now.
    pub fn hidden_in(&self, fullscreen: bool) -> bool {
        self.hidden() || (fullscreen && self.fullscreen_idle)
    }
}

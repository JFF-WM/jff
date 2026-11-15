//! When the cursor hides itself: while typing, and after the pointer rests.

use std::time::{Duration, Instant};

/// The cursor's hiding state. Time comes in as an argument.
#[derive(Clone, Debug)]
pub struct CursorHide {
    when_typing: bool,
    after: Option<Duration>,
    typing: bool,
    idle: bool,
    last_pointer: Instant,
}

impl CursorHide {
    /// Nothing hides the cursor yet, the pointer counts as moved at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            when_typing: false,
            after: None,
            typing: false,
            idle: false,
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

    /// How long the pointer rests before the cursor hides, `None` when it never does.
    pub fn after(&self) -> Option<Duration> {
        self.after
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
        self.last_pointer = now;
    }

    /// How long until the cursor hides for resting, hiding it when that time has come. `None`
    /// when it is hidden now or never hides for resting.
    pub fn remaining(&mut self, now: Instant) -> Option<Duration> {
        let after = self.after?;
        if self.idle {
            return None;
        }
        let rested = now.saturating_duration_since(self.last_pointer);
        if rested >= after {
            self.idle = true;
            return None;
        }
        Some(after - rested)
    }

    /// The cursor should not be drawn.
    pub fn hidden(&self) -> bool {
        self.typing || self.idle
    }
}

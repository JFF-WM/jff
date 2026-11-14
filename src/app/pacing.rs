//! What decides when a frame is drawn and when clients hear about it: the frame callback
//! cycle of each monitor and the test for animations that need frames without any event.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use smithay::output::Output;

use crate::state::JffState;

/// Number of the output's current frame cycle, stored on the output. It moves on when a frame
/// is queued on the display, or when the timer that stands in for a vblank goes off.
#[derive(Default)]
struct FrameSequence(AtomicU32);

/// The last cycle a surface got its frame callback in, with the monitor it was for.
#[derive(Default)]
pub struct LastCallback(Mutex<Option<(String, u32)>>);

impl LastCallback {
    /// True the first time it is asked in a cycle of `output`, false after that: a client
    /// that commits without damage must not be answered again within the same cycle, or it
    /// would spin.
    pub fn first_in_cycle(&self, output: &str, sequence: u32) -> bool {
        let mut last = self.0.lock().unwrap();
        if matches!(&*last, Some((name, seq)) if name == output && *seq == sequence) {
            return false;
        }
        *last = Some((output.to_string(), sequence));
        true
    }
}

/// The current frame cycle of `output`.
pub fn frame_sequence(output: &Output) -> u32 {
    output
        .user_data()
        .get_or_insert_threadsafe(FrameSequence::default)
        .0
        .load(Ordering::Relaxed)
}

/// Start the next frame cycle of `output`: every surface may be sent a callback again.
pub fn advance_frame_sequence(output: &Output) {
    output
        .user_data()
        .get_or_insert_threadsafe(FrameSequence::default)
        .0
        .fetch_add(1, Ordering::Relaxed);
}

impl JffState {
    /// Something on screen changes by itself, without any event to ask for the frame: the
    /// monitor keeps redrawing at the refresh rate while this is true.
    ///
    /// Deliberately generous: a false positive costs frames, a false negative freezes an
    /// animation halfway.
    pub fn animations_ongoing(&mut self) -> bool {
        // slides, the carried thumbnail and the selection ring run on the wall clock
        if self.overview.is_some() {
            return true;
        }
        // fading windows that were closed, toasts (they fade and expire while drawn)
        if !self.closing.is_empty() || !self.osd.is_empty() {
            return true;
        }
        if self.cursor.is_animated() {
            return true;
        }
        // the borders of a window being carried run on their own clock
        if self.tile_carry.as_ref().is_some_and(|carry| carry.shown) {
            return true;
        }
        let now = Instant::now();
        if crate::render::effects::border::animating(now) {
            return true;
        }
        if self.config.window_animations() {
            let (open, _) = self.config.window_animation_times();
            if self.desk.top_down().any(|entry| {
                entry
                    .shown
                    .is_some_and(|at| now.saturating_duration_since(at) < open)
            }) {
                return true;
            }
        }
        self.commit_timers_in_use()
    }
}

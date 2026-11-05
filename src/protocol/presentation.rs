//! wp_presentation: tells clients when their frames actually reached the screen, so video players
//! and games can keep audio and video in sync and pace themselves.
//!
//! The flow, for both backends: a client asks for feedback with `wp_presentation.feedback`
//! before a `wl_surface.commit`. When jff draws a frame it collects the feedback of every
//! surface shown in it (`take_feedback`). Once the frame is on the screen, the feedback is
//! answered with `presented`: on the TTY backend at the vblank that showed it, with the kernel's
//! timestamp, on winit right after submitting, since the host window system gives no timestamps.
//!
//! wp_fifo_v1 rides on the same moments: a client's `set_barrier` clears once the frame it was
//! committed with is on screen (`JffState::release_fifo_barriers`), which lets its next
//! `wait_barrier` commit apply. One new frame per refresh, no busy loop.
//!
//! wp_commit_timing_v1 is the other direction: a client can ask for a commit to show up no
//! earlier than some time. Before each frame is drawn, the commits due by the time that frame
//! will be on screen are let through (`JffState::release_commit_timers`).
//!
//! A feedback dropped without an answer (the client left, the frame was never shown) is reported
//! as discarded by Smithay.

use std::collections::HashMap;
use std::time::Duration;

use smithay::backend::renderer::element::RenderElementStates;
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::{
    surface_presentation_feedback_flags_from_states, OutputPresentationFeedback,
};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{Clock, Monotonic, Time};
use smithay::wayland::commit_timing::CommitTimerBarrierStateUserData;
use smithay::wayland::compositor::{CompositorHandler, SurfaceData};
use smithay::wayland::fifo::FifoBarrierCachedState;

use crate::desk::Desk;
use crate::feature::outputs::output_rect;
use crate::primitives::layout;

/// The presentation clock (`CLOCK_MONOTONIC`) right now. Timestamps given to clients are on this
/// clock.
pub fn monotonic_now() -> Duration {
    Duration::from(Clock::<Monotonic>::new().now())
}

/// Time between two refreshes of `output`, or `None` when it has no mode.
pub fn refresh_interval(output: &Output) -> Option<Duration> {
    let mhz = output.current_mode()?.refresh;
    (mhz > 0).then(|| Duration::from_nanos(1_000_000_000_000 / mhz as u64))
}

/// Collect the feedback requests of everything shown on `output` in the frame that was just
/// rendered (`states` is what the renderer reports for it). Windows on other monitors and on
/// hidden workspaces keep their requests until they're shown.
pub fn take_feedback(
    desk: &Desk<Window>,
    output: &Output,
    states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    let rect = output_rect(output);

    for entry in desk.top_down() {
        let size = entry.window.geometry().size;
        let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
        if !layout::intersects(area, rect) {
            continue;
        }
        entry.window.take_presentation_feedback(
            &mut feedback,
            |_, _| Some(output.clone()),
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }

    for layer in layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(
            &mut feedback,
            |_, _| Some(output.clone()),
            |surface, _| surface_presentation_feedback_flags_from_states(surface, None, states),
        );
    }

    feedback
}

impl crate::state::JffState {
    /// Call `f` for every surface shown on `output` (windows with their subsurfaces, and the
    /// monitor's layer-shell surfaces). Windows on other monitors and on hidden workspaces are
    /// skipped: they keep their barriers and timers until they're shown.
    fn for_each_shown_surface(&self, output: &Output, mut f: impl FnMut(&WlSurface, &SurfaceData)) {
        let rect = output_rect(output);
        for entry in self.desk.top_down() {
            let size = entry.window.geometry().size;
            let area = (entry.pos.0, entry.pos.1, size.w.max(1), size.h.max(1));
            if !layout::intersects(area, rect) {
                continue;
            }
            entry
                .window
                .with_surfaces(|surface, states| f(surface, states));
        }
        for layer in layer_map_for_output(output).layers() {
            layer.with_surfaces(|surface, states| f(surface, states));
        }
    }

    /// Smithay only re-checks a client's blocked commits when told a blocker was cleared, so
    /// after signaling barriers or timers, every affected client has to be told.
    fn blockers_cleared(&mut self, clients: HashMap<ClientId, Client>) {
        let dh = self.display_handle.clone();
        for client in clients.into_values() {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }

    /// Clear the wp_fifo_v1 barriers of everything shown on `output`, so the commits waiting on
    /// them (`wait_barrier`) can apply. Call it when a frame reaches the screen: at the vblank
    /// on the TTY backend, right after submitting with winit, and on TTY also when a redraw
    /// found nothing new to show (the committed content is already on screen).
    pub fn release_fifo_barriers(&mut self, output: &Output) {
        let mut clients: HashMap<ClientId, Client> = HashMap::new();
        self.for_each_shown_surface(output, |surface, states| {
            if signal_fifo_barrier(states) {
                if let Some(client) = surface.client() {
                    clients.insert(client.id(), client);
                }
            }
        });
        self.blockers_cleared(clients);
    }

    /// wp_commit_timing_v1: let the commits whose target time has come apply. A client can ask
    /// for a commit to show up no earlier than some time, `frame_target` is when the frame about
    /// to be drawn on `output` will reach the screen, so every commit due by then applies now
    /// and is part of that frame. Call before drawing.
    pub fn release_commit_timers(&mut self, output: &Output, frame_target: Duration) {
        let mut clients: HashMap<ClientId, Client> = HashMap::new();
        self.for_each_shown_surface(output, |surface, states| {
            let Some(timer) = states.data_map.get::<CommitTimerBarrierStateUserData>() else {
                return; // this client never asked for a timed commit
            };
            timer
                .lock()
                .unwrap()
                .signal_until(Time::<Monotonic>::from(frame_target));
            if let Some(client) = surface.client() {
                clients.insert(client.id(), client);
            }
        });
        self.blockers_cleared(clients);
    }
}

/// Signal the barrier a surface set with its last commit. True when there was one.
fn signal_fifo_barrier(states: &SurfaceData) -> bool {
    let mut cached = states.cached_state.get::<FifoBarrierCachedState>();
    match cached.current().barrier.take() {
        Some(barrier) => {
            tracing::trace!("fifo barrier released");
            barrier.signal();
            true
        }
        None => false,
    }
}

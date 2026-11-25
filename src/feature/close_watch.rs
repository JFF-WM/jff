//! `close-kill`: kill the process of a window that does not close when it is asked to.
//!
//! Every way jff asks a window to close (the `close` bind, the overview's close button and
//! middle click, a taskbar through wlr-foreign-toplevel, `jff msg close`) goes through
//! [`JffState::request_close`]. With `close-kill { enable }` that also starts a timer. When it
//! runs out and the window is still there, the pid of the window's Wayland client gets a
//! SIGKILL and a toast says why. A window that closes in time (its toplevel is destroyed, see
//! [`JffState::close_watch_gone`]) or a config reload that turns the feature off cancels it.
//!
//! The pid is the one of the client, so every window of that process goes with it. Two
//! processes are never killed: jff itself, and xwayland-satellite, whose one client owns every
//! X11 window.

use std::collections::HashSet;
use std::time::Duration;

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::shell::xdg::ToplevelSurface;

use crate::feature::osd::toplevel_names;
use crate::state::JffState;

#[derive(Default)]
pub struct CloseWatch {
    /// Windows that were asked to close and are still open, with a timer running for each.
    pending: HashSet<WlSurface>,
    /// Processes that are never killed (xwayland-satellite).
    protected: HashSet<u32>,
}

impl CloseWatch {
    /// Never kill this process, whatever window of it is slow to close.
    pub fn protect(&mut self, pid: u32) {
        self.protected.insert(pid);
    }

    /// Whether `pid` is a process jff never kills (xwayland-satellite): it owns windows that
    /// are not its own.
    pub fn is_protected(&self, pid: u32) -> bool {
        self.protected.contains(&pid)
    }

    /// Whether a kill of `pid` is allowed: not jff, not a protected process, not pid 0 or 1.
    fn may_kill(&self, pid: u32) -> bool {
        pid > 1 && pid != std::process::id() && !self.protected.contains(&pid)
    }
}

/// "1 second", "5 seconds", "2.5 seconds".
pub fn seconds_text(timeout: Duration) -> String {
    let ms = timeout.as_millis();
    let number = if ms % 1000 == 0 {
        (ms / 1000).to_string()
    } else {
        // "2.500" -> "2.5"
        format!("{:.3}", ms as f64 / 1000.0)
            .trim_end_matches('0')
            .to_owned()
    };
    let unit = if ms == 1000 { "second" } else { "seconds" };
    format!("{number} {unit}")
}

impl JffState {
    /// Ask a window to close. With `close-kill` on, also kill its process if the window is
    /// still open when the timeout runs out.
    pub fn request_close(&mut self, toplevel: &ToplevelSurface) {
        toplevel.send_close();
        let config = self.config.close_kill();
        if !config.enable {
            return;
        }
        let timeout = config.timeout;
        let surface = toplevel.wl_surface().clone();
        // Asked twice: the first request keeps the clock.
        if !self.close_watch.pending.insert(surface.clone()) {
            return;
        }
        let timer_surface = surface.clone();
        let armed =
            self.loop_handle
                .insert_source(Timer::from_duration(timeout), move |_, _, state| {
                    state.close_watch_expired(&timer_surface, timeout);
                    TimeoutAction::Drop
                });
        if let Err(err) = armed {
            tracing::warn!("close-kill: could not start the timer: {}", err.error);
            self.close_watch.pending.remove(&surface);
        }
    }

    /// The window's toplevel is gone: it closed in time, nothing to kill.
    pub fn close_watch_gone(&mut self, surface: &WlSurface) {
        self.close_watch.pending.remove(surface);
    }

    fn close_watch_expired(&mut self, surface: &WlSurface, timeout: Duration) {
        if !self.close_watch.pending.remove(surface) {
            return; // it closed while we waited
        }
        if !self.config.close_kill().enable {
            return; // a reload turned the feature off while we waited
        }
        let still_open = surface.is_alive()
            && self
                .desk
                .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                .is_some();
        if !still_open {
            return;
        }

        let (app_id, title) = toplevel_names(surface);
        let name = [app_id, title]
            .into_iter()
            .find(|n| !n.is_empty())
            .unwrap_or_else(|| "a window".to_owned());

        let pid = surface
            .client()
            .and_then(|client| client.get_credentials(&self.display_handle).ok())
            .and_then(|creds| u32::try_from(creds.pid).ok());
        let Some(pid) = pid else {
            tracing::warn!("close-kill: {name} did not close, but its pid is not known");
            self.show_error(
                &format!("Could not kill {name}"),
                "The pid of its process is not known",
            );
            return;
        };
        if !self.close_watch.may_kill(pid) {
            tracing::warn!("close-kill: {name} did not close, its process {pid} is never killed");
            self.show_error(
                &format!("{name} did not close"),
                "It shares its process with other windows (X11 apps do), so it was not killed",
            );
            return;
        }

        tracing::info!(
            "close-kill: {name} (pid {pid}) did not close within {}, killing it",
            seconds_text(timeout)
        );
        // SAFETY: `kill(2)` has no memory effects. The pid is the one the socket of a client that
        // is still connected reported, so it still belongs to that process.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!("close-kill: kill({pid}) failed: {err}");
            self.show_error(&format!("Could not kill {name}"), &err.to_string());
            return;
        }
        self.show_error(
            &format!("Killed {name}"),
            &format!(
                "It did not close within {} of the close request (pid {pid})",
                seconds_text(timeout)
            ),
        );
        self.queue_redraw_all();
    }
}

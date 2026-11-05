//! X11 support, via xwayland-satellite.
//!
//! The satellite is a normal Wayland client that runs Xwayland for us and turns X11 windows
//! into xdg-toplevels. So we don't need any X11 code, just launch it and set `DISPLAY`.

use std::path::Path;
use std::process::{Child, Command, Stdio};

/// First X display number that looks free: no socket, no lock file.
pub fn free_display() -> Option<u32> {
    (1..=32).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })
}

/// Launch `xwayland-satellite` on a free display and export `DISPLAY` for anything we start
/// later. Has to run after `WAYLAND_DISPLAY` is set, the satellite connects back to us.
/// Returns the child so the caller can stop it on exit, `None` means no X11.
pub fn spawn() -> Option<Child> {
    let Some(n) = free_display() else {
        tracing::warn!("no free X display number; X11 apps disabled");
        return None;
    };
    let x_display = format!(":{n}");

    match Command::new("xwayland-satellite")
        .arg(&x_display)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            tracing::info!("xwayland-satellite started on DISPLAY={}", x_display);
            std::env::set_var("DISPLAY", &x_display);
            Some(child)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("xwayland-satellite not installed; X11 apps disabled");
            None
        }
        Err(err) => {
            tracing::warn!("could not start xwayland-satellite: {err}");
            None
        }
    }
}

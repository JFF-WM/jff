//! X11 support, via xwayland-satellite.
//!
//! The satellite is a normal Wayland client that runs Xwayland for us and turns X11 windows
//! into xdg-toplevels. So we don't need any X11 code, just launch it and set `DISPLAY`.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

/// How long the satellite gets to stop by itself before it is killed.
const STOP_GRACE: Duration = Duration::from_millis(1500);

/// Stop the satellite: ask it to quit (so it stops its Xwayland and removes the X socket and
/// lock file), and kill it if it takes too long. Call it while we still serve our display.
pub fn stop(child: &mut Child) {
    // SAFETY: `kill` with the pid of a child we have not waited for yet.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
    }
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

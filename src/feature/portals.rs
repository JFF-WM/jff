//! xdg-desktop-portal support: file chooser dialogs and the other portals.
//!
//! Portals are D-Bus services started on demand, not children of the compositor. For one to
//! open a window on jff it needs to know the Wayland socket (`WAYLAND_DISPLAY`) and which
//! desktop it runs on (`XDG_CURRENT_DESKTOP`), D-Bus-activated services get neither from us
//! unless we push them into the activation environment. It also needs to know which backend
//! answers for jff: with no `jff-portals.conf`, the portal frontend doesn't know that
//! `xdg-desktop-portal-gtk` should handle the file chooser.
//!
//! Dialogs also need the `xdg-foreign` protocol, so a chooser can be a child of the window
//! that asked for it (see `state.rs`).

use std::path::PathBuf;
use std::process::Command;

/// The name portals see in `XDG_CURRENT_DESKTOP`, and the prefix of `<name>-portals.conf`.
pub const DESKTOP_NAME: &str = "jff";

const PORTALS_CONF: &str = "\
# Which xdg-desktop-portal backends to use under jff.
# FileChooser, AppChooser, Settings, Print and friends come from xdg-desktop-portal-gtk.
[preferred]
default=gtk
";

/// Call after `WAYLAND_DISPLAY` is set (and `DISPLAY`, if X11 apps are on).
///
/// `export_to_session` is false for the nested winit backend: there, the D-Bus session belongs
/// to the host desktop, and changing its activation environment would point the host's portals
/// at our window.
pub fn setup(export_to_session: bool) {
    std::env::set_var("XDG_CURRENT_DESKTOP", DESKTOP_NAME);
    std::env::set_var("XDG_SESSION_DESKTOP", DESKTOP_NAME);
    std::env::set_var("XDG_SESSION_TYPE", "wayland");

    if !export_to_session {
        tracing::info!("nested session: not exporting the environment to D-Bus/systemd");
        return;
    }

    write_portals_conf();

    // Only variables that are actually set. `--systemd` also imports them into the user manager.
    let vars: Vec<&str> = [
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "XDG_SESSION_TYPE",
        "XCURSOR_THEME",
        "XCURSOR_SIZE",
    ]
    .into_iter()
    .filter(|name| std::env::var_os(name).is_some())
    .collect();

    match Command::new("dbus-update-activation-environment")
        .arg("--systemd")
        .args(&vars)
        .status()
    {
        Ok(status) if status.success() => {
            tracing::info!(
                "exported {} to D-Bus and systemd for portals",
                vars.join(", ")
            );
        }
        Ok(status) => tracing::warn!("dbus-update-activation-environment failed: {status}"),
        Err(err) => tracing::warn!(
            "could not run dbus-update-activation-environment ({err}); portals will not find \
             jff, install dbus or export WAYLAND_DISPLAY to your user session yourself"
        ),
    }
}

/// Create `~/.config/xdg-desktop-portal/jff-portals.conf` unless the user already has one.
fn write_portals_conf() {
    let Some(dir) = config_home().map(|dir| dir.join("xdg-desktop-portal")) else {
        return;
    };
    let path = dir.join(format!("{DESKTOP_NAME}-portals.conf"));
    if path.exists() {
        return;
    }
    let result = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, PORTALS_CONF));
    match result {
        Ok(()) => tracing::info!("wrote {}", path.display()),
        Err(err) => tracing::warn!("could not write {}: {err}", path.display()),
    }
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

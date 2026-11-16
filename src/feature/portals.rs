//! xdg-desktop-portal support: file chooser dialogs and the other portals.
//!
//! Portals are D-Bus services started on demand, not children of the compositor. For one to
//! open a window on jff it needs to know the Wayland socket (`WAYLAND_DISPLAY`) and which
//! desktop it runs on (`XDG_CURRENT_DESKTOP`), D-Bus-activated services get neither from us
//! unless we push them into the activation environment. It also needs to know which backend
//! answers for jff: with no `jff-portals.conf`, the portal frontend doesn't know that
//! `xdg-desktop-portal-gtk` should handle the file chooser, or that `xdg-desktop-portal-wlr`
//! (which reads the screen through wlr-screencopy) should handle ScreenCast and Screenshot.
//!
//! Dialogs also need the `xdg-foreign` protocol, so a chooser can be a child of the window
//! that asked for it (see `state.rs`).

use std::path::PathBuf;
use std::process::Command;

/// The name portals see in `XDG_CURRENT_DESKTOP`, and the prefix of `<name>-portals.conf`.
pub const DESKTOP_NAME: &str = "jff";

/// A file whose first line starts with this was written by jff and is jff's to keep current,
/// anything else is the user's and is left alone. The first line of `PORTALS_CONF` carries a
/// version after it: bump it when the text changes.
const MANAGED_MARK: &str = "# managed by jff (portals-conf";

const PORTALS_CONF: &str = "\
# managed by jff (portals-conf v2)
# Which xdg-desktop-portal backends to use under jff. jff rewrites this file when it changes
# the defaults, as long as this first line stays. Remove the line to make the file yours.
[preferred]
default=gtk
# Screen sharing and recording read the picture through wlr-screencopy.
org.freedesktop.impl.portal.ScreenCast=wlr
# wlr first, gtk if xdg-desktop-portal-wlr does not do screenshots.
org.freedesktop.impl.portal.Screenshot=wlr;gtk
";

/// What jff wrote before the marker existed, byte for byte: still ours to replace.
const PORTALS_CONF_V1: &str = "\
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

/// What to do with `jff-portals.conf` given what is in it now (`None`: no file).
#[derive(Debug, PartialEq, Eq)]
enum ConfAction {
    /// Write it: there is none, or an older one of ours.
    Write,
    /// Already current.
    Current,
    /// The user's own file, not touched.
    Keep,
}

fn conf_action(existing: Option<&str>) -> ConfAction {
    match existing {
        None => ConfAction::Write,
        Some(text) if text == PORTALS_CONF => ConfAction::Current,
        Some(text)
            if text
                .lines()
                .next()
                .is_some_and(|l| l.starts_with(MANAGED_MARK))
                || text == PORTALS_CONF_V1 =>
        {
            ConfAction::Write
        }
        Some(_) => ConfAction::Keep,
    }
}

/// Keep `~/.config/xdg-desktop-portal/jff-portals.conf` current: create it, or replace one jff
/// wrote earlier. A file the user wrote or edited (no marker line) is left alone.
fn write_portals_conf() {
    let Some(dir) = config_home().map(|dir| dir.join("xdg-desktop-portal")) else {
        return;
    };
    let path = dir.join(format!("{DESKTOP_NAME}-portals.conf"));
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        // Unreadable (or not UTF-8): not ours to judge.
        Err(err) => {
            tracing::warn!("could not read {}: {err}", path.display());
            return;
        }
    };
    match conf_action(existing.as_deref()) {
        ConfAction::Current => return,
        ConfAction::Keep => {
            tracing::info!(
                "{} is not managed by jff, leaving it; ScreenCast needs `org.freedesktop.impl.portal.ScreenCast=wlr` in it",
                path.display()
            );
            return;
        }
        ConfAction::Write => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_written() {
        assert_eq!(conf_action(None), ConfAction::Write);
    }

    #[test]
    fn current_file_is_left_as_is() {
        assert_eq!(conf_action(Some(PORTALS_CONF)), ConfAction::Current);
    }

    #[test]
    fn the_old_default_is_upgraded() {
        assert_eq!(conf_action(Some(PORTALS_CONF_V1)), ConfAction::Write);
    }

    #[test]
    fn an_older_managed_file_is_upgraded() {
        let old = "# managed by jff (portals-conf v1)\n[preferred]\ndefault=gtk\n";
        assert_eq!(conf_action(Some(old)), ConfAction::Write);
    }

    #[test]
    fn a_file_without_the_marker_line_is_kept() {
        let edited = PORTALS_CONF.replacen(MANAGED_MARK, "# mine", 1);
        assert_eq!(conf_action(Some(&edited)), ConfAction::Keep);
    }

    #[test]
    fn a_users_file_is_kept() {
        assert_eq!(
            conf_action(Some("[preferred]\ndefault=kde\n")),
            ConfAction::Keep
        );
    }

    #[test]
    fn the_conf_routes_screen_capture_to_wlr() {
        assert!(PORTALS_CONF.contains("ScreenCast=wlr"));
        assert!(PORTALS_CONF.contains("Screenshot=wlr;gtk"));
        assert!(PORTALS_CONF.starts_with(MANAGED_MARK));
    }
}

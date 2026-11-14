//! `restore-apps`: start again the applications that were open when the last session ended, each
//! window on the monitor and the workspace it was on. Off by default, while it is off nothing in
//! here reads or writes anything.
//!
//! While it is on, a line for every open window (dialogs and windows without an app id left
//! out): its app id, workspace and monitor, kept in `$XDG_STATE_HOME/jff-wm/open-apps`,
//! rewritten whenever that changes and once more when jff stops. At the next start, after
//! `autostart` has run, every app on that list is found in the installed `.desktop` files (the
//! same way the overview finds the app of a window, `primitives::app_search::entry_for_app_id`)
//! and started once, however many windows it had.
//!
//! The places are handed out as the windows open: the first window an app opens takes the
//! first place saved for its app id, the next one the next, and so on, for [`GRACE`] after the
//! start (this takes in the apps that `autostart` starts, and apps that open more than one
//! window). The window is moved there before anything else is decided for it, as a
//! `window-rule` with `open-on-output` and `open-workspace` would (`fit_to_output_once`), and it
//! wins over such a rule's monitor and workspace, which are the same for every window of an
//! app. A monitor that is not connected any more leaves the window where it opens by default.
//! Windows an app puts back itself (xdg-session-management) keep the place that gives them.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::config::Action;
use crate::feature::app_index::AppIndex;
use crate::feature::osd::toplevel_names;
use crate::primitives::app_restore::{
    app_ids, format_apps, is_autostarted, parse_apps, shell_line_command, sorted, Placement,
};
use crate::primitives::app_search::entry_for_app_id;
use crate::state::JffState;

/// How often the restore looks whether the list of installed apps is ready.
pub const POLL: Duration = Duration::from_millis(100);
/// How long it waits for that before giving up.
pub const GIVE_UP: Duration = Duration::from_secs(10);
/// How long after the start the saved places are handed to new windows, and how long an app that
/// was started by the restore stays on the list without a window.
const GRACE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct AppRestore {
    /// What the file says now (what was last written, or read at startup).
    saved: Vec<Placement>,
    /// What the last session left, as read at startup.
    loaded: Vec<Placement>,
    /// Apps to start once the list of installed apps is ready. `None`: nothing to restore, or
    /// done.
    pending: Option<Vec<String>>,
    /// When the restore started waiting for the list of installed apps.
    waiting_since: Option<Instant>,
    /// The places not given to a window yet, by app id, in the order they are given out.
    places: HashMap<String, VecDeque<Placement>>,
    /// Until when `places` are given out.
    places_until: Option<Instant>,
    /// Apps the restore started that have not shown a window yet, and when they were started.
    starting: Vec<(String, Instant)>,
    /// The session is ending: the file is not touched any more.
    frozen: bool,
}

fn state_path() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let state =
        absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|h| h.join(".local/state")))?;
    Some(state.join("jff-wm").join("open-apps"))
}

fn load() -> Vec<Placement> {
    state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_apps(&text))
        .unwrap_or_default()
}

/// Best effort, like the launch counts: a list that can't be saved isn't worth an error.
fn save(placements: &[Placement]) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, format_apps(placements)).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

impl JffState {
    /// Read the list the last session left and get ready to start its apps. Called once at
    /// startup, after `autostart`. Does nothing without `restore-apps`.
    pub fn app_restore_begin(&mut self) {
        if !self.config.restore_apps() {
            return;
        }
        let loaded = load();
        let apps = app_ids(&loaded);
        tracing::info!(
            "restore-apps: {} window(s) of {} app(s) from the last session",
            loaded.len(),
            apps.len()
        );
        self.app_restore.saved = loaded.clone();
        if loaded.is_empty() {
            return;
        }
        for place in &loaded {
            if place.workspace.is_some() || !place.output.is_empty() {
                self.app_restore
                    .places
                    .entry(place.app_id.clone())
                    .or_default()
                    .push_back(place.clone());
            }
        }
        self.app_restore.places_until = Some(Instant::now() + GRACE);
        self.app_restore.loaded = loaded;
        self.app_restore.pending = Some(apps);
        self.app_restore.waiting_since = Some(Instant::now());
        // Starts the scan of the desktop files.
        self.app_index.refresh();
    }

    /// The place saved for the next window of `root`'s app, when the restore has one. Takes it:
    /// the next window of the app gets the next place. Called once for a window, when it first
    /// shows.
    pub(crate) fn app_restore_place(&mut self, root: &WlSurface) -> Option<Placement> {
        if !self.config.restore_apps() {
            return None;
        }
        match self.app_restore.places_until {
            Some(until) if Instant::now() < until => {}
            Some(_) => {
                // Too late: a window the user opens now is not one of the last session's.
                self.app_restore.places.clear();
                self.app_restore.places_until = None;
                return None;
            }
            None => return None,
        }
        let app_id = toplevel_names(root).0;
        let key = app_id.trim();
        let places = self.app_restore.places.get_mut(key)?;
        let place = places.pop_front();
        if places.is_empty() {
            self.app_restore.places.remove(key);
        }
        place
    }

    /// Whether [`JffState::app_restore_poll`] still has something to do. Lets `app::run` skip
    /// the timer when there is nothing to restore.
    pub fn app_restore_pending(&self) -> bool {
        self.app_restore.pending.is_some()
    }

    /// Start the apps once the list of installed apps is there. Returns `true` when it is done
    /// (or gave up) and doesn't need to be called again.
    pub fn app_restore_poll(&mut self) -> bool {
        let Some(wanted) = self.app_restore.pending.take() else {
            return true;
        };
        let (entries, version) = self.app_index.snapshot();
        if version == 0 {
            let waited = self
                .app_restore
                .waiting_since
                .map_or(Duration::ZERO, |since| since.elapsed());
            if waited >= GIVE_UP {
                tracing::warn!("restore-apps: the list of installed apps never came, giving up");
                return true;
            }
            self.app_restore.pending = Some(wanted);
            return false;
        }

        let autostarted: Vec<Vec<String>> = self
            .config
            .autostart()
            .iter()
            .filter_map(|action| match action {
                Action::Spawn(argv) => Some(argv.clone()),
                Action::SpawnShell(line) => Some(shell_line_command(line)),
                _ => None,
            })
            .collect();

        let mut started_entries: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for app_id in wanted {
            let Some(index) = entry_for_app_id(&entries, &app_id) else {
                tracing::info!("restore-apps: no desktop file for {app_id:?}, not started");
                continue;
            };
            let entry = &entries[index];
            // Two app ids can lead to the same app, which is started once.
            if started_entries.contains(&entry.id) {
                continue;
            }
            started_entries.push(entry.id.clone());
            // Counts as open for a while, whoever starts it.
            self.app_restore
                .starting
                .push((app_id.clone(), Instant::now()));
            if AppIndex::command_of(entry).is_some_and(|argv| is_autostarted(&argv, &autostarted)) {
                tracing::info!("restore-apps: {} is in autostart already", entry.name);
                continue;
            }
            match AppIndex::launch_uncounted(entry) {
                Ok(()) => tracing::info!("restore-apps: started {}", entry.name),
                Err(why) => {
                    tracing::warn!("restore-apps: {why}");
                    failures.push(why);
                }
            }
        }
        if !failures.is_empty() {
            self.show_error("Could not restore some apps", &failures.join("\n"));
        }
        true
    }

    /// The place of every window that is open now and has shown something (before that its
    /// place is a guess).
    fn open_placements(&self) -> Vec<Placement> {
        let windows: Vec<WlSurface> = self
            .desk
            .all_top_down()
            .filter_map(|entry| {
                let toplevel = entry.window.toplevel()?;
                let surface = toplevel.wl_surface();
                // Dialogs belong to the window they are over.
                if toplevel.parent().is_some() || self.dialog_toplevels.contains(surface) {
                    return None;
                }
                Some(surface.clone())
            })
            .collect();
        sorted(windows.iter().filter_map(|surface| {
            let saved = self.session_snapshot(surface)?;
            Some(Placement {
                app_id: toplevel_names(surface).0,
                workspace: Some(saved.workspace),
                output: saved.output,
            })
        }))
    }

    /// Keep the file in step with the open windows. Runs before every frame, and is cheap when
    /// nothing changed. Does nothing without `restore-apps`, while the apps of the last session
    /// have not been started yet, and once the session is ending.
    pub fn app_restore_flush(&mut self) {
        if !self.config.restore_apps()
            || self.app_restore.frozen
            || self.app_restore.pending.is_some()
        {
            return;
        }
        let mut apps = self.open_placements();
        if !self.app_restore.starting.is_empty() {
            let open = app_ids(&apps);
            // Seen, or tried for long enough.
            self.app_restore
                .starting
                .retain(|(id, since)| since.elapsed() < GRACE && !open.contains(id));
            // Still on its way: it stays, where it was.
            let waiting: Vec<Placement> = self
                .app_restore
                .loaded
                .iter()
                .filter(|place| {
                    self.app_restore
                        .starting
                        .iter()
                        .any(|(id, _)| *id == place.app_id)
                })
                .cloned()
                .collect();
            apps = sorted(apps.into_iter().chain(waiting));
        }
        if apps != self.app_restore.saved {
            save(&apps);
            self.app_restore.saved = apps;
        }
    }

    /// The session is ending (a signal, `quit`, the host window closed): write the list as it
    /// is, and don't touch it after this, because the clients are about to go and take their
    /// windows with them.
    pub fn app_restore_finish(&mut self) {
        self.app_restore_flush();
        self.app_restore.frozen = true;
    }
}

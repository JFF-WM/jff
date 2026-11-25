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
//! Each line also keeps the pid and the pidfs id of the window's process (pidfd, Linux 6.9+).
//! At the start an app whose process is still running (a fresh pidfd with the same id that
//! `poll` says has not exited) is not started again, one whose process exited, or whose pid was
//! recycled, is. No `/proc`.
//!
//! The places are handed out as the windows open: the first window an app opens takes the
//! first place saved for its app id, the next one the next, and so on, for [`GRACE`] after the
//! start (this takes in the apps that `autostart` starts, and apps that open more than one
//! window). The window is moved there before anything else is decided for it, as a
//! `window-rule` with `open-on-output` and `open-workspace` would (`fit_to_output_once`), and it
//! wins over such a rule's monitor and workspace, which are the same for every window of an
//! app. A monitor that is not connected any more leaves the window where it opens by default.
//! Windows an app puts back itself (xdg-session-management) keep the place that gives them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::IsAlive;

use crate::config::Action;
use crate::desk::WindowId;
use crate::feature::app_index::AppIndex;
use crate::feature::osd::toplevel_names;
use crate::primitives::app_restore::{
    app_ids, app_survived, format_apps, is_autostarted, parse_apps, pick_by_title,
    shell_line_command, sorted, without_stale_identity, Placement, ProcessState,
};
use crate::primitives::app_search::entry_for_app_id;
use crate::primitives::layout_template::Template;
use crate::protocol::ext_foreign_toplevel_pidfd::process_state;
use crate::state::JffState;

/// How often the restore looks whether the list of installed apps is ready.
pub const POLL: Duration = Duration::from_millis(100);
/// How long it waits for that before giving up.
pub const GIVE_UP: Duration = Duration::from_secs(10);
/// How long after the start the saved places are handed to new windows, and how long an app that
/// was started by the restore stays on the list without a window.
const GRACE: Duration = Duration::from_secs(60);
/// How long after a window got its saved place it is kept there: browsers can publish their
/// real window title well after the first buffer, so keep matching and correcting placements
/// for the full restore grace period.
const ENFORCE: Duration = GRACE;

/// A window the restore gave a saved place to.
struct Assigned {
    surface: WlSurface,
    place: Placement,
    since: Instant,
    /// Where the window was when the restore last looked at it. A window found somewhere else
    /// than here and than its place was moved by the user, not left behind by a late title.
    seen: Option<(usize, String)>,
    /// The user moved the window: the restore leaves its workspace and monitor alone.
    released: bool,
    /// Its place changed after it was put somewhere (a late title swapped places): its container
    /// has to be put right once, not only its monitor and workspace.
    refit: bool,
}

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
    places: HashMap<String, Vec<Placement>>,
    /// Until when `places` are given out.
    places_until: Option<Instant>,
    /// Apps the restore started that have not shown a window yet, and when they were started.
    starting: Vec<(String, Instant)>,
    /// The session is ending: the file is not touched any more.
    frozen: bool,
    /// Windows that were put in a saved place, so `container-template` can tell a window the
    /// restore brought back from one the user opened, and so they can be kept there.
    placed: Vec<Assigned>,
    /// The windows that joined a container the restore is rebuilding, by container name, with
    /// the place each had in it.
    joined: HashMap<String, Vec<(WlSurface, usize)>>,
    /// The arrangement saved for each container, by container name.
    layouts: HashMap<String, String>,
}

fn container_key(name: &str, workspace: Option<usize>, output: &str) -> String {
    format!(
        "{name}\0{}\0{output}",
        workspace.map_or_else(|| "?".to_owned(), |w| w.to_string())
    )
}

fn workspace_path() -> Option<PathBuf> {
    state_path().map(|path| path.with_file_name("last-workspace"))
}

fn boot_path() -> Option<PathBuf> {
    state_path().map(|path| path.with_file_name("boot-id"))
}

/// The kernel's id of this boot. `None` when it can't be read.
fn current_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// Whether the saved list was written in this boot, and note this boot for the next start.
/// Unknown counts as another boot, so a stale pid never keeps an app from starting.
fn same_boot_as_saved() -> bool {
    let now = current_boot_id();
    let path = boot_path();
    let before = path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_owned());
    if let (Some(path), Some(now)) = (path.as_ref(), now.as_ref()) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, format!("{now}\n"));
    }
    matches!((before, now), (Some(before), Some(now)) if before == now)
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
        if let Some(path) = workspace_path() {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Some((output, workspace)) = text.trim_end().split_once('\t') {
                    if let Ok(workspace) = workspace.parse::<usize>() {
                        if !output.is_empty() && self.outputs.iter().any(|o| o.name() == output) {
                            self.switch_workspace_on(output, workspace);
                        } else {
                            self.switch_workspace(workspace);
                        }
                    }
                }
            }
        }
        let loaded = without_stale_identity(load(), same_boot_as_saved());
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
                    .push(place.clone());
            }
        }
        for place in &loaded {
            if !place.container.is_empty() && !place.layout.is_empty() {
                self.app_restore.layouts.insert(
                    container_key(&place.container, place.workspace, &place.output),
                    place.layout.clone(),
                );
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
        let (app_id, title) = toplevel_names(root);
        let key = app_id.trim();
        let places = self.app_restore.places.get_mut(key)?;
        let index = pick_by_title(places.iter().map(|p| p.title.as_str()), &title)?;
        let place = places.remove(index);
        if places.is_empty() {
            self.app_restore.places.remove(key);
        }
        self.app_restore.placed.push(Assigned {
            surface: root.clone(),
            place: place.clone(),
            since: Instant::now(),
            seen: None,
            released: false,
            refit: false,
        });
        Some(place)
    }

    /// The window `root` has joined container `name`, which the restore is rebuilding: lay the
    /// container out as it was, with the windows that are in it in the places they had.
    pub(crate) fn app_restore_container_joined(&mut self, root: &WlSurface, name: &str) {
        let Some(place) = self
            .app_restore
            .placed
            .iter()
            .find(|a| a.surface == *root)
            .map(|a| a.place.clone())
        else {
            return;
        };
        let Some(slot) = place.slot else {
            return;
        };
        let key = container_key(name, place.workspace, &place.output);
        // It may have been counted in another container before its place changed.
        for list in self.app_restore.joined.values_mut() {
            list.retain(|(surface, _)| surface != root);
        }
        self.app_restore
            .joined
            .entry(key.clone())
            .or_default()
            .push((root.clone(), slot));
        let Some(template) = self
            .app_restore
            .layouts
            .get(&key)
            .and_then(|text| Template::parse(text).ok())
        else {
            return;
        };
        let Some(id) = self
            .desk
            .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == root))
            .and_then(|e| e.container)
        else {
            return;
        };
        let mut joined = self
            .app_restore
            .joined
            .get(&key)
            .cloned()
            .unwrap_or_default();
        joined.retain(|(surface, _)| surface.alive());
        joined.sort_by_key(|(_, slot)| *slot);
        let prefer: Vec<WindowId> = joined
            .iter()
            .filter_map(|(surface, _)| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                    .map(|e| e.id)
            })
            .collect();
        let gap = self.config.snap_gap();
        match self
            .desk
            .apply_container_layout_ordered(id, &template, |_| None, &prefer, gap)
        {
            Ok(_) => self.retile_container(id),
            Err(why) => tracing::warn!("restore-apps: container {name:?} not laid out: {why:?}"),
        }
    }

    /// The windows `restore-apps` put back that are still open, oldest first.
    pub(crate) fn app_restore_placed(&mut self) -> Vec<WlSurface> {
        self.app_restore.placed.retain(|a| a.surface.alive());
        self.app_restore
            .placed
            .iter()
            .map(|a| a.surface.clone())
            .collect()
    }

    /// Keep the windows the restore placed where their saved place says, for [`ENFORCE`] after
    /// they were placed, until the user moves one (a window found somewhere other than its
    /// place and its last known spot is let go). Runs before every frame, cheap once the time
    /// is over. A window whose title turns out to be the one saved for another place of its app (Firefox sets it after
    /// the first buffer) swaps places with the window that holds that place, or takes it from
    /// the places not given out. A window that is somewhere else is moved there.
    pub fn app_restore_enforce(&mut self) {
        if !self.config.restore_apps() {
            return;
        }
        self.app_restore.placed.retain(|a| a.surface.alive());
        let active: Vec<usize> = (0..self.app_restore.placed.len())
            .filter(|&i| self.app_restore.placed[i].since.elapsed() < ENFORCE)
            .collect();
        if active.is_empty() {
            return;
        }
        // The title the window has now, against the one its place was saved with.
        let titles: Vec<String> = active
            .iter()
            .map(|&i| toplevel_names(&self.app_restore.placed[i].surface).1)
            .collect();
        for (n, &i) in active.iter().enumerate() {
            let title = titles[n].trim();
            let held = self.app_restore.placed[i].place.title.trim().to_owned();
            if title.is_empty() || title == held {
                continue;
            }
            let app_id = self.app_restore.placed[i].place.app_id.clone();
            // A place nobody has, saved with this title.
            let free = self
                .app_restore
                .places
                .get(&app_id)
                .and_then(|places| places.iter().position(|p| p.title.trim() == title));
            if let Some(j) = free {
                let new = self.app_restore.places.get_mut(&app_id).unwrap().remove(j);
                let old = std::mem::replace(&mut self.app_restore.placed[i].place, new);
                self.app_restore
                    .places
                    .entry(app_id)
                    .or_default()
                    .insert(0, old);
                self.app_restore.placed[i].refit = true;
                continue;
            }
            // A place a window of the app holds that does not fit that window.
            let swap = active.iter().enumerate().find(|&(m, &k)| {
                k != i
                    && self.app_restore.placed[k].place.app_id == app_id
                    && self.app_restore.placed[k].place.title.trim() == title
                    && titles[m].trim() != title
            });
            if let Some((_, &k)) = swap {
                let (a, b) = (
                    self.app_restore.placed[i].place.clone(),
                    self.app_restore.placed[k].place.clone(),
                );
                self.app_restore.placed[i].place = b;
                self.app_restore.placed[k].place = a;
                self.app_restore.placed[i].refit = true;
                self.app_restore.placed[k].refit = true;
            }
        }
        // Move every window that is not where its place says, unless the user moved it.
        for &i in &active {
            if self.app_restore.placed[i].released {
                continue;
            }
            let (root, place) = {
                let a = &self.app_restore.placed[i];
                (a.surface.clone(), a.place.clone())
            };
            if self.zones_is_member(&root) {
                continue;
            }
            // Its place changed: put its container right as well, once.
            if self.app_restore.placed[i].refit {
                self.app_restore.placed[i].refit = false;
                self.app_restore_refit(&root, &place);
                if let Some(here) = self.session_place(&root) {
                    self.app_restore.placed[i].seen = Some(here);
                }
                continue;
            }
            // Before its first fit the first commit places it.
            let Some((now_workspace, now_output)) = self.session_place(&root) else {
                continue;
            };
            let output_known =
                !place.output.is_empty() && self.outputs.iter().any(|o| o.name() == place.output);
            let wrong_output = output_known && now_output != place.output;
            let wrong_workspace = place.workspace.is_some_and(|w| w != now_workspace);
            let here = (now_workspace, now_output.clone());
            if !wrong_output && !wrong_workspace {
                self.app_restore.placed[i].seen = Some(here);
                continue;
            }
            // Somewhere new that is not where its place says and not where it was last
            // seen: the user moved it. Stop keeping it.
            if self.app_restore.placed[i]
                .seen
                .as_ref()
                .is_some_and(|seen| *seen != here)
            {
                tracing::info!(
                    "restore-apps: {:?} was moved by the user, left there",
                    place.app_id
                );
                self.app_restore.placed[i].released = true;
                continue;
            }
            self.app_restore.placed[i].seen = Some(here);
            tracing::info!(
                "restore-apps: {:?} was on workspace {} of {:?}, moving to {:?} of {:?}",
                place.app_id,
                now_workspace,
                now_output,
                place.workspace,
                place.output
            );
            let target = crate::config::WindowRule {
                output: output_known
                    .then(|| crate::config::RuleOutput::Named(place.output.clone())),
                workspace: place.workspace,
                ..Default::default()
            };
            self.apply_rule_monitor_and_workspace(&root, &target);
        }
    }

    /// A window got another place than the one it joined a container for (its title came late and
    /// swapped places): take it out of the container it is in when that is not the one its place
    /// names, then bring it to the monitor and workspace of its place and into that container,
    /// laid out the way it was saved.
    fn app_restore_refit(&mut self, root: &WlSurface, place: &Placement) {
        let Some((window, current)) = self
            .desk
            .find_by(|w| w.toplevel().is_some_and(|t| t.wl_surface() == root))
            .map(|e| (e.window.clone(), e.container))
        else {
            return;
        };
        let in_name = current
            .and_then(|id| self.desk.container(id))
            .and_then(|c| c.name().map(str::to_owned));
        if current.is_some()
            && !place.container.is_empty()
            && in_name.as_deref() == Some(place.container.as_str())
        {
            return;
        }
        let output_known =
            !place.output.is_empty() && self.outputs.iter().any(|o| o.name() == place.output);
        let target = crate::config::WindowRule {
            output: output_known.then(|| crate::config::RuleOutput::Named(place.output.clone())),
            workspace: place.workspace,
            ..Default::default()
        };
        if let Some(from) = current {
            if self.desk.leave_container(|w| w == &window) {
                tracing::info!(
                    "restore-apps: {:?} left container {in_name:?}, its place is elsewhere",
                    place.app_id
                );
                self.retile_container(from);
            }
        }
        if !place.container.is_empty()
            && self.open_in_named_container(&window, &target, &place.container)
        {
            self.app_restore_container_joined(root, &place.container);
        } else {
            self.apply_rule_monitor_and_workspace(root, &target);
        }
        if place.maximized {
            if let Some(toplevel) = window.toplevel() {
                self.set_mode(toplevel, crate::desk::WindowMode::Maximized);
            }
        }
    }

    /// Whether the restore is starting the app with this app id and its window has not shown
    /// yet: waiting for the list of installed apps, or started and not seen. An app that
    /// `autostart` starts counts, the restore notes those too. Compared without regard to case.
    pub(crate) fn app_restore_expects(&self, app_id: &str) -> bool {
        if !self.config.restore_apps() {
            return false;
        }
        let same = |id: &str| id.trim().eq_ignore_ascii_case(app_id.trim());
        self.app_restore
            .pending
            .as_ref()
            .is_some_and(|apps| apps.iter().any(|id| same(id)))
            || self.app_restore.starting.iter().any(|(id, _)| same(id))
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
            // Counts as open for a while, whoever starts it. This holds for a survivor too: one
            // that never opens a window keeps its saved places until `GRACE` runs out, so they
            // do not go to another app.
            self.app_restore
                .starting
                .push((app_id.clone(), Instant::now()));
            // Its process outlived the last compositor: starting it again would run it twice.
            if self.app_restore_survived(&app_id) {
                tracing::info!("restore-apps: {} is still running, not started", entry.name);
                continue;
            }
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

    /// Whether a process of the app is still running, by `poll` on a pidfd (see
    /// `process_state`). Dead, recycled or unknown all lead to a restart.
    fn app_restore_survived(&self, app_id: &str) -> bool {
        app_survived(
            self.app_restore
                .loaded
                .iter()
                .filter(|place| place.app_id == app_id)
                .map(|place| match place.pid {
                    Some(pid) => process_state(pid, place.proc_id),
                    None => ProcessState::Unknown,
                }),
        )
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
        // The container of each, with its place in it and its arrangement.
        let mut container_facts: HashMap<crate::desk::ContainerId, (String, String)> =
            HashMap::new();
        let taken: Vec<String> = self
            .desk
            .containers()
            .filter_map(|c| c.name().map(str::to_owned))
            .collect();
        for container in self.desk.containers() {
            let name = match container.name() {
                Some(name) => name.to_owned(),
                None => {
                    let mut name = format!("restore-{}", container.id().get());
                    while taken.contains(&name) {
                        name.push('\'');
                    }
                    name
                }
            };
            let layout = Template::from_tree(container.tree(), |_| None)
                .map(|t| t.to_text())
                .unwrap_or_default();
            container_facts.insert(container.id(), (name, layout));
        }
        let starting = !self.app_restore.starting.is_empty();
        sorted(windows.iter().filter_map(|surface| {
            let (workspace, output) = self.session_place(surface)?;
            let (pid, proc_id) = self.window_process_identity(surface).unzip();
            let (mut container, mut slot, mut layout) = (String::new(), None, String::new());
            let mut maximized = self
                .desk
                .mode_of(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
                == crate::desk::WindowMode::Maximized;
            let entry = self.desk.all_top_down().find(|e| {
                e.window
                    .toplevel()
                    .is_some_and(|t| t.wl_surface() == surface)
            });
            if let Some(entry) = entry {
                if let Some(id) = entry.container {
                    if let Some((name, text)) = container_facts.get(&id) {
                        container = name.clone();
                        layout = text.clone();
                        let wid = Some(entry.id);
                        slot = self.desk.container(id).and_then(|c| {
                            c.tree().leaves().iter().position(|leaf| Some(*leaf) == wid)
                        });
                    }
                }
            }
            // While the container is still being rebuilt it is not all there: what the restore
            // put the window in keeps being written, so the windows that have not come yet
            // are not lost from the arrangement.
            if starting {
                if let Some(a) = self
                    .app_restore
                    .placed
                    .iter()
                    .find(|a| a.surface == *surface && !a.place.container.is_empty())
                {
                    container = a.place.container.clone();
                    slot = a.place.slot;
                    layout = a.place.layout.clone();
                    maximized = a.place.maximized;
                }
            }
            Some(Placement {
                app_id: toplevel_names(surface).0,
                workspace: Some(workspace),
                output,
                pid,
                proc_id,
                title: toplevel_names(surface).1.trim().chars().take(200).collect(),
                container,
                slot,
                layout,
                maximized,
            })
        }))
    }

    /// Keep the file in step with the open windows. Runs before every frame, and is cheap when
    /// nothing changed. Does nothing without `restore-apps`, while the apps of the last session
    /// have not been started yet, and once the session is ending.
    pub fn app_restore_flush(&mut self) {
        if !self.config.restore_apps() || self.app_restore.frozen {
            return;
        }
        // Save the active workspace even while apps are still being restored. Otherwise a
        // session that ends before the restore timer finishes keeps the previous workspace.
        if let Some(path) = workspace_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(
                path,
                format!(
                    "{}\t{}\n",
                    self.active_output_name(),
                    self.current_workspace()
                ),
            );
        }
        // Do not rewrite the app list until the startup restore has finished: windows that
        // have not appeared yet still need their saved placements.
        if self.app_restore.pending.is_some() {
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

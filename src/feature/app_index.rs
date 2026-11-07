//! The list of installed applications the overview search draws from.
//!
//! Matching and `Exec` handling live in `primitives/app_search.rs` (pure). This file is the
//! file-system side:
//!
//! - Scanning `$XDG_DATA_HOME/applications` and `$XDG_DATA_DIRS/*/applications` for `.desktop`
//!   files on a helper thread, so opening the overview never waits on the disk.
//! - Keeping the list fresh: every time the overview opens the directories are `stat`ed, and a
//!   changed modification time starts a new scan. The old list stays in use until the new one
//!   is ready.
//! - Counting launches per app (stored in `$XDG_STATE_HOME/jff-wm/app-launches`) so the apps
//!   you use most come first among equal matches.
//! - Starting an app: detached, in its own process group, output discarded.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::primitives::app_search::{launch_plan, parse_desktop_entry, DesktopEntry, Env};

/// Files bigger than this aren't desktop files.
const MAX_FILE: u64 = 256 * 1024;
/// How deep below `applications/` to look.
const MAX_DEPTH: usize = 4;

/// Modification times of the directories the list was made from.
type Signature = Vec<(PathBuf, Option<SystemTime>)>;

struct Snapshot {
    entries: Arc<Vec<DesktopEntry>>,
    /// Bumped on every finished scan, so users of the list can tell it changed.
    version: u64,
    /// Directory state when the scan that produced `entries` started. `None` before the first
    /// scan finishes.
    signature: Option<Signature>,
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    scanning: AtomicBool,
}

pub struct AppIndex {
    shared: Arc<Shared>,
    launches: HashMap<String, u32>,
    launches_loaded: bool,
}

impl Default for AppIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl AppIndex {
    /// Empty, nothing is read until [`AppIndex::refresh`].
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                snapshot: Mutex::new(Snapshot {
                    entries: Arc::new(Vec::new()),
                    version: 0,
                    signature: None,
                }),
                scanning: AtomicBool::new(false),
            }),
            launches: HashMap::new(),
            launches_loaded: false,
        }
    }

    /// Start a scan on a helper thread when there's no list yet or a directory changed since
    /// the last one. Cheap otherwise (a few `stat` calls). Call when the overview opens.
    pub fn refresh(&mut self) {
        if !self.launches_loaded {
            self.launches_loaded = true;
            self.launches = read_launches();
        }
        if self.shared.scanning.load(Ordering::Acquire) {
            return;
        }
        let dirs = application_dirs();
        let now = signature_of(&dirs);
        let stale = {
            let snapshot = self
                .shared
                .snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot.signature.as_ref() != Some(&now)
        };
        if !stale {
            return;
        }
        if self.shared.scanning.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("jff-app-index".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let entries = scan(&dirs);
                    {
                        let mut snapshot = shared
                            .snapshot
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        snapshot.entries = Arc::new(entries);
                        snapshot.version += 1;
                        snapshot.signature = Some(now);
                    }
                    // Icons may have been installed along with the apps.
                    crate::feature::app_icons::forget_all();
                    shared.scanning.store(false, Ordering::Release);
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("app search: cannot start the scan thread: {err}");
            shared.scanning.store(false, Ordering::Release);
        }
    }

    /// The list as of the last finished scan, and its version.
    pub fn snapshot(&self) -> (Arc<Vec<DesktopEntry>>, u64) {
        let snapshot = self
            .shared
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (Arc::clone(&snapshot.entries), snapshot.version)
    }

    /// Version of the list (see [`AppIndex::snapshot`]) without taking the list.
    pub fn version(&self) -> u64 {
        self.shared
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .version
    }

    /// How many times each app (by entry ID) was started from the overview.
    pub fn launches(&self) -> &HashMap<String, u32> {
        &self.launches
    }

    /// Start `entry` and count it. `Err` says why it didn't start, ready to show.
    pub fn launch(&mut self, entry: &DesktopEntry) -> Result<(), String> {
        let plan = launch_plan(entry, &terminal_program())
            .ok_or_else(|| format!("{} has no command to run", entry.name))?;
        spawn_detached(&plan.argv, plan.cwd.as_deref())
            .map_err(|err| format!("{}: {err}", plan.argv[0]))?;
        let count = self.launches.entry(entry.id.clone()).or_insert(0);
        *count = count.saturating_add(1);
        write_launches(&self.launches);
        Ok(())
    }
}

/// The terminal for `Terminal=true` entries, chosen like the `terminal` action does.
fn terminal_program() -> String {
    std::env::var("JFF_TERMINAL")
        .or_else(|_| std::env::var("TERMINAL"))
        .unwrap_or_else(|_| "alacritty".to_string())
}

/// Run `argv` so it outlives us and shares neither our terminal nor our process group.
fn spawn_detached(argv: &[String], cwd: Option<&str>) -> std::io::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(dir) = cwd.filter(|dir| Path::new(dir).is_dir()) {
        command.current_dir(dir);
    }
    let mut child = command.spawn()?;
    // Nobody else waits for it, a child that exits unwatched becomes a zombie until the
    // compositor does.
    let reaper = std::thread::Builder::new()
        .name("jff-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(err) = reaper {
        tracing::warn!("app search: cannot start a reaper thread: {err}");
    }
    Ok(())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// An absolute path from an environment variable, or `None` when unset, empty or relative (the
/// XDG rules say to ignore those).
fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// `applications` directories, the user's first: a file there hides one of the same name
/// further down.
fn application_dirs() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Some(data_home) = absolute_env("XDG_DATA_HOME") {
        bases.push(data_home);
    } else if let Some(home) = home() {
        bases.push(home.join(".local/share"));
    }
    match std::env::var("XDG_DATA_DIRS") {
        Ok(dirs) if !dirs.trim().is_empty() => {
            bases.extend(
                dirs.split(':')
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute()),
            );
        }
        _ => {
            bases.push(PathBuf::from("/usr/local/share"));
            bases.push(PathBuf::from("/usr/share"));
        }
    }
    // Flatpak exports apps here. A session that didn't run the login scripts that add this to
    // XDG_DATA_DIRS would otherwise never see them.
    if let Some(home) = home() {
        bases.push(home.join(".local/share/flatpak/exports/share"));
    }
    bases.push(PathBuf::from("/var/lib/flatpak/exports/share"));

    let mut seen: HashSet<PathBuf> = HashSet::new();
    bases
        .into_iter()
        .map(|base| base.join("applications"))
        .filter(|dir| seen.insert(dir.clone()))
        .collect()
}

/// Modification time of every directory in `dirs` and below them.
fn signature_of(dirs: &[PathBuf]) -> Signature {
    fn walk(dir: &Path, depth: usize, out: &mut Signature) {
        let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok();
        out.push((dir.to_owned(), modified));
        if modified.is_none() || depth >= MAX_DEPTH {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<PathBuf> = read
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        children.sort();
        for child in children {
            walk(&child, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        walk(dir, 0, &mut out);
    }
    out
}

/// Locales to look translated names up in, best first: `pt_BR.UTF-8` gives `pt_BR`, `pt`.
fn locales() -> Vec<String> {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX");
    let Some(raw) = raw else {
        return Vec::new();
    };
    // lang_COUNTRY.ENCODING@MODIFIER
    let without_modifier = raw.split('@').next().unwrap_or(&raw);
    let without_encoding = without_modifier
        .split('.')
        .next()
        .unwrap_or(without_modifier);
    let mut out = vec![without_encoding.to_owned()];
    if let Some((lang, _country)) = without_encoding.split_once('_') {
        out.push(lang.to_owned());
    }
    out.retain(|l| !l.is_empty());
    out
}

fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `program` can be run: an existing file when it contains a `/`, else something in
/// `PATH`.
fn program_exists(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Every application in `dirs`, ordered by name. A desktop file ID found in an earlier
/// directory hides the same ID in a later one, also when the earlier one is hidden or doesn't
/// apply here, which is how a user turns an app off.
fn scan(dirs: &[PathBuf]) -> Vec<DesktopEntry> {
    let desktops = current_desktops();
    let locales = locales();
    let env = Env {
        desktops: &desktops,
        locales: &locales,
    };
    let mut taken: HashSet<String> = HashSet::new();
    let mut entries: Vec<DesktopEntry> = Vec::new();
    for dir in dirs {
        let mut files: Vec<(String, PathBuf)> = Vec::new();
        collect_files(dir, dir, 0, &mut files);
        files.sort();
        for (id, path) in files {
            if !taken.insert(id.clone()) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.len() > MAX_FILE {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(entry) = parse_desktop_entry(&id, &path.to_string_lossy(), &text, &env) else {
                continue;
            };
            if !entry.try_exec.is_empty() && !program_exists(&entry.try_exec) {
                continue;
            }
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    tracing::debug!("app search: {} applications", entries.len());
    entries
}

/// `.desktop` files below `dir`, with their desktop file IDs.
fn collect_files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        // `metadata` follows symlinks - Flatpak and Nix export their desktop files as links.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                collect_files(root, &path, depth + 1, out);
            }
        } else if path.extension() == Some(OsStr::new("desktop")) {
            if let Some(id) = desktop_file_id(root, &path) {
                out.push((id, path));
            }
        }
    }
}

/// `root/kde/foo.desktop` is `kde-foo`.
fn desktop_file_id(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?.with_extension("");
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| parts.join("-"))
}

fn launches_path() -> Option<PathBuf> {
    let state =
        absolute_env("XDG_STATE_HOME").or_else(|| home().map(|h| h.join(".local/state")))?;
    Some(state.join("jff-wm").join("app-launches"))
}

/// One line per app: the count, a space, the entry ID.
fn read_launches() -> HashMap<String, u32> {
    let Some(path) = launches_path() else {
        return HashMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    parse_launches(&text)
}

pub fn parse_launches(text: &str) -> HashMap<String, u32> {
    text.lines()
        .filter_map(|line| {
            let (count, id) = line.split_once(' ')?;
            let count: u32 = count.trim().parse().ok()?;
            let id = id.trim();
            (!id.is_empty()).then(|| (id.to_owned(), count))
        })
        .collect()
}

pub fn format_launches(launches: &HashMap<String, u32>) -> String {
    let mut lines: Vec<(&String, &u32)> = launches.iter().collect();
    lines.sort();
    lines
        .into_iter()
        .map(|(id, count)| format!("{count} {id}\n"))
        .collect()
}

/// Best effort: a launch count that can't be saved isn't worth an error.
fn write_launches(launches: &HashMap<String, u32>) {
    let Some(path) = launches_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, format_launches(launches)).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

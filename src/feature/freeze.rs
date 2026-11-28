//! `toggle-freeze`: freeze the process behind the focused window, or let it run again.
//!
//! The process is always found through a pidfd, never a bare pid, so nothing can land on a
//! process that inherited a recycled pid. Wayland apps use the pidfd from `SO_PEERPIDFD`
//! (Linux 6.5+, no pidfd means no freeze). X11 windows all belong to xwayland-satellite as far
//! as Wayland knows, so the X server is asked who owns the focused window, and the answer is
//! checked again once a pidfd is held (`feature/x11_pid.rs`).
//!
//! The preferred way is the cgroup v2 freezer: the whole app stops, nothing can `SIGCONT` it,
//! and `cgroup.events` confirms it. That only happens when the cgroup holds nothing but this
//! app and not jff, which per-app scopes like systemd's `app-*.scope` give you. Otherwise the
//! process and its descendants get `SIGSTOP` through their pidfds and are checked in `/proc`.
//! That is weaker, since anyone of the same user can resume it, and the toast says which one
//! was used.
//!
//! jff itself, xwayland-satellite and pid 1 are never frozen. What is frozen is written to
//! `$XDG_RUNTIME_DIR/jff-frozen`, thawed on a clean exit, and thawed by the next jff if this
//! one crashed (a frozen cgroup stays frozen when its freezer dies).

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::desktop::Window;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;

use crate::feature::osd::toplevel_names;
use crate::feature::x11_pid;
use crate::protocol::ext_foreign_toplevel_pidfd::{open_pidfd, pidfd_inode, pidfs_supported};
use crate::state::{ClientState, JffState};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// How long a freeze or a thaw gets to show in `cgroup.events` or `/proc` before it is
/// reported as not done. The compositor waits for it, so it is short.
const SETTLE: Duration = Duration::from_millis(300);
const POLL_STEP: Duration = Duration::from_millis(5);
/// Rounds of "stop the children found, look again" before a process that keeps forking is
/// given up on.
const MAX_ROUNDS: usize = 8;
const MAX_MEMBERS: usize = 4096;
/// Use `SIGSTOP` on the process tree when the cgroup freezer can't be used. `false` makes
/// `toggle-freeze` refuse instead.
const ALLOW_SIGSTOP_FALLBACK: bool = true;

/// What `toggle-freeze` did.
pub struct Outcome {
    /// `true`: it is frozen now. `false`: it runs again.
    pub frozen: bool,
    /// `cgroup` or `sigstop`.
    pub method: &'static str,
    /// For a toast.
    pub title: String,
    pub detail: String,
}

/// The processes `toggle-freeze` froze and that are still frozen.
#[derive(Default)]
pub struct Freezer {
    frozen: Vec<Frozen>,
}

struct Frozen {
    /// The process the window belongs to, as the pidfd said when it was frozen.
    pid: u32,
    /// The window it was frozen from. When it goes away the connection is dead, and so the
    /// process is thawed to notice that.
    surface: WlSurface,
    how: How,
}

enum How {
    /// The cgroup that was frozen and a pidfd of the app's process to see when it is gone.
    Cgroup { dir: PathBuf, target: OwnedFd },
    /// The processes that were sent `SIGSTOP`.
    Stop { pids: Vec<Stopped> },
}

struct Stopped {
    pid: u32,
    fd: OwnedFd,
}

impl How {
    fn label(&self) -> &'static str {
        match self {
            How::Cgroup { .. } => "cgroup",
            How::Stop { .. } => "sigstop",
        }
    }

    /// Let it run again. `false` when it did not show as thawed in time.
    fn thaw(&self) -> bool {
        match self {
            How::Cgroup { dir, .. } => {
                if !dir.exists() {
                    return true; // the cgroup is gone, with everything frozen in it
                }
                set_frozen(dir, false).unwrap_or(false)
            }
            How::Stop { pids } => {
                for stopped in pids {
                    let _ = send_signal(&stopped.fd, libc::SIGCONT);
                }
                true
            }
        }
    }
}

impl Freezer {
    fn state_file() -> Option<PathBuf> {
        std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("jff-frozen"))
    }

    /// Write down what is frozen, for the next jff to thaw if this one does not get to.
    fn save(&self) {
        let Some(path) = Self::state_file() else {
            return;
        };
        let mut text = String::new();
        for entry in &self.frozen {
            match &entry.how {
                How::Cgroup { dir, .. } => {
                    if let Ok(rel) = dir.strip_prefix(CGROUP_ROOT) {
                        text.push_str(&format!("cgroup /{}\n", rel.display()));
                    }
                }
                // A pid alone could be another process by then: only with the pidfs id.
                How::Stop { pids } if pidfs_supported() => {
                    for stopped in pids {
                        if let Some(id) = pidfd_inode(&stopped.fd) {
                            text.push_str(&format!("stop {} {id}\n", stopped.pid));
                        }
                    }
                }
                How::Stop { .. } => {}
            }
        }
        if text.is_empty() {
            let _ = fs::remove_file(&path);
        } else if let Err(err) = fs::write(&path, text) {
            tracing::warn!("freeze: could not write {}: {err}", path.display());
        }
    }
}

/// Thaw what an earlier jff left frozen. Call it once at startup, before apps are started.
pub fn thaw_stale() {
    let Some(path) = Freezer::state_file() else {
        return;
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    let _ = fs::remove_file(&path);
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("cgroup"), Some(rel), None)
                if rel.starts_with('/') && !rel.split('/').any(|part| part == "..") =>
            {
                let knob = format!("{CGROUP_ROOT}{rel}/cgroup.freeze");
                if fs::write(&knob, "0").is_ok() {
                    tracing::info!("freeze: thawed {rel}, left frozen by an earlier jff");
                }
            }
            (Some("stop"), Some(pid), Some(id)) => {
                let (Ok(pid), Ok(id)) = (pid.parse::<u32>(), id.parse::<u64>()) else {
                    continue;
                };
                if let Some(fd) = open_pidfd(pid) {
                    if pidfd_inode(&fd) == Some(id) {
                        let _ = send_signal(&fd, libc::SIGCONT);
                        tracing::info!(
                            "freeze: resumed process {pid}, left stopped by an earlier jff"
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

// ---- kernel helpers -------------------------------------------------------------------------

/// `pidfd_send_signal(2)`: the signal goes to the process the pidfd was made for, or fails.
fn send_signal(fd: &OwnedFd, signal: libc::c_int) -> std::io::Result<()> {
    // SAFETY: the syscall only reads its integer arguments, a null `siginfo` is allowed.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Whether the process has exited (a pidfd is readable then), a zombie included.
fn exited(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd`, timeout 0 so it never blocks.
    let rc = unsafe { libc::poll(&mut poll, 1, 0) };
    rc > 0 && poll.revents & libc::POLLIN != 0
}

/// The pid the pidfd was made for, from its `fdinfo`. `None` when the process is gone and
/// reaped, or lives in a pid namespace jff can't see into.
fn pid_of(fd: &OwnedFd) -> Option<u32> {
    let text = fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).ok()?;
    let pid: i64 = text
        .lines()
        .find_map(|line| line.strip_prefix("Pid:"))?
        .trim()
        .parse()
        .ok()?;
    u32::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// State letter and parent of a process, from `/proc/PID/stat`.
fn stat_fields(pid: u32) -> Option<(char, u32)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name in parentheses can hold anything, even `)`: go from the last one.
    let rest = &text[text.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

/// The cgroup (v2) of a process, as a path from the cgroup root: `/user.slice/…/app-x.scope`.
fn cgroup_path(pid: u32) -> Option<String> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim();
    let sane = path.starts_with('/')
        && !path.ends_with("(deleted)")
        && !path.split('/').any(|part| part == "..");
    sane.then(|| path.to_owned())
}

fn writable(path: &Path) -> bool {
    CString::new(path.as_os_str().as_bytes())
        // SAFETY: `access` reads a NUL-terminated path that lives through the call.
        .map(|path| unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0)
        .unwrap_or(false)
}

// ---- cgroup freezer -------------------------------------------------------------------------

/// Every process in the cgroup `dir` and below it.
fn subtree_members(dir: &Path, depth: u32, out: &mut Vec<u32>) -> Result<(), String> {
    if depth > 16 {
        return Err("its cgroup tree is too deep".to_string());
    }
    let procs = fs::read_to_string(dir.join("cgroup.procs"))
        .map_err(|e| format!("cannot read {}: {e}", dir.join("cgroup.procs").display()))?;
    out.extend(
        procs
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok()),
    );
    if out.len() > MAX_MEMBERS {
        return Err("its cgroup holds too many processes".to_string());
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            subtree_members(&entry.path(), depth + 1, out)?;
        }
    }
    Ok(())
}

/// Whether `pid` is `root` or one of its descendants.
fn descends_from(mut pid: u32, root: u32) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        match stat_fields(pid) {
            Some((_, parent)) if parent > 1 => pid = parent,
            _ => return false,
        }
    }
    false
}

/// The cgroup to freeze for the app whose process is `pid` (held by `target`), or why it can't
/// be done that way.
fn cgroup_plan(pid: u32, target: &OwnedFd) -> Result<PathBuf, String> {
    let path = cgroup_path(pid).ok_or("its cgroup is not known (no cgroup v2?)")?;
    // The pid was read from the pidfd, but a pid can be recycled after the process is reaped:
    // if the process is still there now, it was when its cgroup was read.
    send_signal(target, 0).map_err(|_| "the process is gone".to_string())?;
    if path == "/" {
        return Err("it is in the root cgroup".to_string());
    }
    let mine = cgroup_path(std::process::id()).ok_or("the cgroup of jff is not known")?;
    if mine == path || mine.starts_with(&format!("{path}/")) {
        return Err("it shares its cgroup with jff (start apps in their own scope)".to_string());
    }
    if !Path::new(CGROUP_ROOT).join("cgroup.controllers").exists() {
        return Err("cgroup v2 is not mounted".to_string());
    }
    let dir = PathBuf::from(format!("{CGROUP_ROOT}{path}"));
    if !writable(&dir.join("cgroup.freeze")) {
        return Err("its cgroup.freeze is not writable by jff".to_string());
    }
    let events = fs::read_to_string(dir.join("cgroup.events")).unwrap_or_default();
    if events.lines().any(|line| line.trim() == "frozen 1") {
        return Err("its cgroup is already frozen by something else".to_string());
    }
    let mut members = Vec::new();
    subtree_members(&dir, 0, &mut members)?;
    for member in members {
        if stat_fields(member).is_none() {
            continue; // gone while we looked
        }
        if !descends_from(member, pid) {
            return Err(format!(
                "its cgroup also holds process {member}, which is not part of this app"
            ));
        }
    }
    Ok(dir)
}

/// Write `cgroup.freeze` and wait for `cgroup.events` to say it happened. `Ok(false)`: written,
/// but not confirmed in time (a task in uninterruptible sleep delays it, it still completes).
fn set_frozen(dir: &Path, on: bool) -> Result<bool, String> {
    fs::write(dir.join("cgroup.freeze"), if on { "1" } else { "0" })
        .map_err(|e| format!("cannot write {}: {e}", dir.join("cgroup.freeze").display()))?;
    let want = if on { "frozen 1" } else { "frozen 0" };
    let deadline = Instant::now() + SETTLE;
    loop {
        let events = fs::read_to_string(dir.join("cgroup.events")).unwrap_or_default();
        if events.lines().any(|line| line.trim() == want) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL_STEP);
    }
}

// ---- SIGSTOP of the process tree ------------------------------------------------------------

/// Parent pid to child pids, for every process.
fn scan_children() -> HashMap<u32, Vec<u32>> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(dir) = fs::read_dir("/proc") {
        for entry in dir.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some((_, parent)) = stat_fields(pid) {
                children.entry(parent).or_default().push(pid);
            }
        }
    }
    children
}

fn resume_all(stopped: &[Stopped]) {
    for one in stopped {
        let _ = send_signal(&one.fd, libc::SIGCONT);
    }
}

/// Stop `root` and everything below it, and check that they stopped. On failure everything is
/// resumed.
fn stop_tree(
    root_pid: u32,
    root: &OwnedFd,
    protected: &dyn Fn(u32) -> bool,
) -> Result<Vec<Stopped>, String> {
    let mut stopped = vec![Stopped {
        pid: root_pid,
        fd: root.try_clone().map_err(|e| e.to_string())?,
    }];
    send_signal(root, libc::SIGSTOP).map_err(|e| format!("cannot stop process {root_pid}: {e}"))?;

    let me = std::process::id();
    let mut settled = false;
    for _ in 0..MAX_ROUNDS {
        let children = scan_children();
        let mut fresh = Vec::new();
        let mut seen = HashSet::from([root_pid]);
        let mut queue = vec![root_pid];
        while let Some(parent) = queue.pop() {
            for &child in children.get(&parent).into_iter().flatten() {
                if child <= 1 || child == me || protected(child) || !seen.insert(child) {
                    continue;
                }
                queue.push(child);
                if stopped.iter().any(|one| one.pid == child) {
                    continue;
                }
                // Hold the process first, then see that it is still the child the scan saw.
                let Some(fd) = open_pidfd(child) else {
                    continue;
                };
                if stat_fields(child).map(|(_, parent)| parent) != Some(parent) {
                    continue;
                }
                fresh.push(Stopped { pid: child, fd });
            }
        }
        if fresh.is_empty() {
            settled = true;
            break;
        }
        for one in &fresh {
            let _ = send_signal(&one.fd, libc::SIGSTOP);
        }
        stopped.extend(fresh);
    }
    if !settled {
        resume_all(&stopped);
        return Err("it keeps starting new processes".to_string());
    }

    let deadline = Instant::now() + SETTLE;
    loop {
        stopped.retain(|one| !exited(&one.fd));
        if stopped.is_empty() {
            return Err("the process exited".to_string());
        }
        let all_stopped = stopped
            .iter()
            .all(|one| matches!(stat_fields(one.pid), Some(('T' | 't', _)) | None));
        if all_stopped {
            return Ok(stopped);
        }
        if Instant::now() >= deadline {
            resume_all(&stopped);
            return Err("some of its processes did not stop".to_string());
        }
        std::thread::sleep(POLL_STEP);
    }
}

// ---- the toggle -----------------------------------------------------------------------------

fn window_name(surface: &WlSurface) -> String {
    let (app_id, title) = toplevel_names(surface);
    [app_id, title]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "a window".to_owned())
}

/// Whether the title the X server shows for the focused window is the title of the Wayland
/// window the keyboard is on. A stale X focus (the satellite has not caught up) would
/// otherwise freeze the app that had the keyboard before.
fn titles_match(wayland: &str, x11: &str) -> bool {
    if wayland.is_empty() || x11.is_empty() {
        return wayland.is_empty() && x11.is_empty();
    }
    wayland == x11 || wayland.starts_with(x11) || x11.starts_with(wayland)
}

impl JffState {
    /// The `toggle-freeze` action: freeze the focused window's process or thaw it, with a toast.
    pub fn toggle_freeze_focused(&mut self) {
        let Some(window) = self.focused_window() else {
            self.show_error("Nothing to freeze", "No window has the keyboard.");
            return;
        };
        let name = window
            .toplevel()
            .map_or_else(|| "a window".to_owned(), |t| window_name(t.wl_surface()));
        match self.toggle_freeze(&window) {
            Ok(outcome) => {
                tracing::info!("freeze: {} ({})", outcome.title, outcome.method);
                self.show_info(&outcome.title, &outcome.detail);
            }
            Err(why) => {
                tracing::warn!("freeze: {name}: {why}");
                self.show_error(&format!("Could not freeze {name}"), &why);
            }
        }
    }

    /// Freeze the process behind `window`, or thaw it when it is frozen already.
    pub fn toggle_freeze(&mut self, window: &Window) -> Result<Outcome, String> {
        self.freeze_gc();
        let surface = window
            .toplevel()
            .ok_or("the window has no toplevel")?
            .wl_surface()
            .clone();
        let name = window_name(&surface);
        let (pid, target) = self.freeze_target(window, &surface)?;

        // Frozen already (another window of the same process counts): thaw.
        if let Some(at) = self.freezer.frozen.iter().position(|f| f.pid == pid) {
            let entry = self.freezer.frozen.remove(at);
            let method = entry.how.label();
            let settled = entry.how.thaw();
            self.freezer.save();
            return Ok(Outcome {
                frozen: false,
                method,
                title: format!("Thawed {name}"),
                detail: if settled {
                    "It runs again.".to_owned()
                } else {
                    "It is waking up, a task was slow to leave the freezer.".to_owned()
                },
            });
        }

        let protected = |pid: u32| self.close_watch.is_protected(pid);
        let (how, detail) = match cgroup_plan(pid, &target) {
            Ok(dir) => {
                let settled = set_frozen(&dir, true)?;
                let target = target.try_clone().map_err(|e| e.to_string())?;
                let detail = if settled {
                    "The whole app is frozen (cgroup).".to_owned()
                } else {
                    "Freezing: a task is in uninterruptible sleep, it stops when that ends."
                        .to_owned()
                };
                (How::Cgroup { dir, target }, detail)
            }
            Err(why) if ALLOW_SIGSTOP_FALLBACK => {
                let pids = stop_tree(pid, &target, &protected)
                    .map_err(|err| format!("{why}; SIGSTOP did not work either: {err}"))?;
                let detail = format!(
                    "SIGSTOP on {} process(es), weaker than a cgroup freeze: {why}.",
                    pids.len()
                );
                (How::Stop { pids }, detail)
            }
            Err(why) => return Err(format!("it cannot be frozen with a cgroup: {why}")),
        };
        let method = how.label();
        self.freezer.frozen.push(Frozen { pid, surface, how });
        self.freezer.save();
        Ok(Outcome {
            frozen: true,
            method,
            title: format!("Froze {name}"),
            detail,
        })
    }

    /// The process to freeze for `window`, as a pid and a pidfd that is that process.
    fn freeze_target(
        &self,
        window: &Window,
        surface: &WlSurface,
    ) -> Result<(u32, OwnedFd), String> {
        let client = surface.client().ok_or("the window has no client")?;
        let client_pid = client
            .get_credentials(&self.display_handle)
            .ok()
            .and_then(|creds| u32::try_from(creds.pid).ok());
        let (pid, fd) = if client_pid.is_some_and(|pid| self.close_watch.is_protected(pid)) {
            self.x11_target(window, surface)?
        } else {
            let fd = client
                .get_data::<ClientState>()
                .and_then(|state| state.pidfd.as_ref())
                .ok_or("the kernel gave no pidfd for this app (Linux 6.5 or newer is needed)")?
                .try_clone()
                .map_err(|e| e.to_string())?;
            let pid = pid_of(&fd).ok_or("the process is gone")?;
            (pid, fd)
        };
        if pid <= 1 || pid == std::process::id() || self.close_watch.is_protected(pid) {
            return Err("jff never freezes itself, init or the X11 bridge".to_string());
        }
        Ok((pid, fd))
    }

    /// The process of an X11 window: the owner of the window the X server gives the keyboard,
    /// when that is `window`.
    fn x11_target(&self, window: &Window, surface: &WlSurface) -> Result<(u32, OwnedFd), String> {
        if self.focused_window().as_ref() != Some(window) {
            return Err("an X11 window can only be frozen while it has the keyboard".to_string());
        }
        let display = std::env::var("DISPLAY").map_err(|_| "DISPLAY is not set".to_string())?;
        let (_, title) = toplevel_names(surface);

        let first = x11_pid::focused_window(&display)?;
        if !titles_match(&title, &first.title) {
            return Err(format!(
                "the X server has the keyboard on {:?}, not on this window (yet), try again",
                first.title
            ));
        }
        let fd = open_pidfd(first.pid).ok_or("the app's process is gone")?;
        // Now that the process is held: the same window must still have the same owner.
        let second = x11_pid::focused_window(&display)?;
        if second.window != first.window || second.pid != first.pid {
            return Err(
                "the X11 focus moved while the process was looked up, try again".to_string(),
            );
        }
        Ok((first.pid, fd))
    }

    /// Forget what was frozen and has exited, and let go of its cgroup.
    fn freeze_gc(&mut self) {
        let before = self.freezer.frozen.len();
        self.freezer
            .frozen
            .retain_mut(|entry| match &mut entry.how {
                How::Cgroup { dir, target } => {
                    if !exited(target) {
                        return true;
                    }
                    // A frozen, empty cgroup that a service reuses would freeze its next run.
                    if dir.exists() {
                        let _ = fs::write(dir.join("cgroup.freeze"), "0");
                    }
                    false
                }
                How::Stop { pids } => {
                    pids.retain(|stopped| !exited(&stopped.fd));
                    !pids.is_empty()
                }
            });
        if self.freezer.frozen.len() != before {
            self.freezer.save();
        }
    }

    /// A toplevel is gone. If it was frozen its connection is dead, thaw the process so it
    /// notices and ends instead of staying frozen out of sight.
    pub fn freeze_window_gone(&mut self, surface: &WlSurface) {
        let before = self.freezer.frozen.len();
        self.freezer.frozen.retain(|entry| {
            if entry.surface == *surface {
                entry.how.thaw();
                return false;
            }
            true
        });
        if self.freezer.frozen.len() != before {
            self.freezer.save();
        }
    }

    /// Thaw everything `toggle-freeze` froze. Runs when jff ends in order.
    pub fn thaw_all_frozen(&mut self) {
        for entry in std::mem::take(&mut self.freezer.frozen) {
            entry.how.thaw();
        }
        self.freezer.save();
    }

    /// Whether a window's process is frozen (for code that should not feed it input).
    pub fn is_frozen(&self, surface: &WlSurface) -> bool {
        self.freezer
            .frozen
            .iter()
            .any(|entry| entry.surface == *surface)
    }
}

#[cfg(test)]
mod tests {
    use super::titles_match;

    #[test]
    fn titles_must_agree_before_an_x11_window_is_frozen() {
        assert!(titles_match("", ""));
        assert!(titles_match("Calculator", "Calculator"));
        // jff keeps a title as the client set it, the X side can be cut or longer
        assert!(titles_match("Calculator - extra", "Calculator"));
        assert!(!titles_match("Calculator", "Firefox"));
        assert!(!titles_match("", "Firefox"));
        assert!(!titles_match("Firefox", ""));
    }
}

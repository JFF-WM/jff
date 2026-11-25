//! The list kept for `restore-apps`, and the decisions made from it. Pure: no file system, no
//! Wayland.
//!
//! The list has a line for every open window: the app id, the workspace (0-based) and the name
//! of the monitor it is on, then (when known) the pid and the pidfs id of its process,
//! separated by tabs. The id tells the same process from a recycled pid on the next start. An app with windows on two workspaces has two
//! lines. Lines are sorted, so the file only changes when a window comes, goes or moves. Text
//! the app chooses (its id) and the monitor name are escaped (`\\`, `\t`, `\n`, `\r`). A line
//! with only an app id is fine (no place known), and a line that doesn't parse is skipped.

use std::collections::BTreeSet;
use std::path::Path;

/// Where one window was.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Placement {
    pub app_id: String,
    /// 0-based. `None`: not known.
    pub workspace: Option<usize>,
    /// The monitor's name. Empty: not known.
    pub output: String,
    /// The process behind the window, with `proc_id` its identity. Written only when the
    /// kernel gives pidfds unique ids (pidfs, Linux 6.9+), so that after a restart a pid that
    /// was recycled is told from the process that really was there. `None`: not known.
    pub pid: Option<u32>,
    /// The pidfs inode of the process's pidfd: never reused, unlike the pid.
    pub proc_id: Option<u64>,
    /// The window's title when last seen. Tells apart the windows of one app (Firefox) when
    /// they are placed again. Empty: not known.
    pub title: String,
    /// The tiling container the window was in, by name. Empty: not in one.
    pub container: String,
    /// Where the window was among the leaves of that container, in tree order.
    pub slot: Option<usize>,
    /// The arrangement of that container as `Template::to_text` writes it, the same on every
    /// line of the container. Empty: not known.
    pub layout: String,
    /// Whether the window was maximized (including a maximized container member).
    pub maximized: bool,
}

/// Whether the process a window belonged to is still there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Alive,
    Dead,
    /// No identity was saved, or the kernel can't say.
    Unknown,
}

/// Whether an app is still running, from the state of the processes its saved windows had: one
/// that is alive is enough. Dead and unknown both mean the app has to be started again.
pub fn app_survived(states: impl IntoIterator<Item = ProcessState>) -> bool {
    states.into_iter().any(|s| s == ProcessState::Alive)
}

/// What to remember out of the places of the open windows: windows without an app id dropped
/// (there is nothing to find their app by), the rest sorted. A window that is where another
/// of its app is stays: each of them takes one place when the app is started again.
pub fn sorted(placements: impl IntoIterator<Item = Placement>) -> Vec<Placement> {
    let mut list: Vec<Placement> = placements
        .into_iter()
        .filter_map(|mut placement| {
            let id = placement.app_id.trim();
            if id.is_empty() {
                return None;
            }
            placement.app_id = id.to_owned();
            Some(placement)
        })
        .collect();
    list.sort();
    list
}

/// What to trust of a saved list after a restart: when the machine was rebooted since it was
/// written (or that cannot be told), the pids and pidfs ids in it belong to another boot. Both
/// counters start over at boot and move in step, so an old pid and id can match an unrelated
/// live process and make a dead app look alive. They are dropped, the rest of the line stays.
pub fn without_stale_identity(mut list: Vec<Placement>, same_boot: bool) -> Vec<Placement> {
    if !same_boot {
        for place in &mut list {
            place.pid = None;
            place.proc_id = None;
        }
    }
    list
}

/// Which of the saved `titles` (in the order they are given out) belongs to a window titled
/// `title`: the first one that is the same, else the first one. `None` when there are none.
pub fn pick_by_title<'a>(titles: impl IntoIterator<Item = &'a str>, title: &str) -> Option<usize> {
    let title = title.trim();
    let titles: Vec<&str> = titles.into_iter().collect();
    if titles.is_empty() {
        return None;
    }
    if !title.is_empty() {
        if let Some(i) = titles.iter().position(|t| t.trim() == title) {
            return Some(i);
        }
    }
    Some(0)
}

/// The apps in the list, each once, sorted: what is started.
pub fn app_ids(placements: &[Placement]) -> Vec<String> {
    let set: BTreeSet<&str> = placements.iter().map(|p| p.app_id.as_str()).collect();
    set.into_iter().map(str::to_owned).collect()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The list as the text of the file.
pub fn format_apps(placements: &[Placement]) -> String {
    let mut out = String::new();
    for p in placements {
        out.push_str(&escape(&p.app_id));
        out.push('\t');
        if let Some(workspace) = p.workspace {
            out.push_str(&workspace.to_string());
        }
        out.push('\t');
        out.push_str(&escape(&p.output));
        out.push('\t');
        if let Some(pid) = p.pid {
            out.push_str(&pid.to_string());
        }
        out.push('\t');
        if let Some(id) = p.proc_id {
            out.push_str(&id.to_string());
        }
        if !p.title.is_empty() || !p.container.is_empty() {
            out.push('\t');
            out.push_str(&escape(&p.title));
        }
        if !p.container.is_empty() {
            out.push('\t');
            out.push_str(&escape(&p.container));
            out.push('\t');
            if let Some(slot) = p.slot {
                out.push_str(&slot.to_string());
            }
            out.push('\t');
            out.push_str(&escape(&p.layout));
        }
        if p.maximized {
            let fields = 5
                + usize::from(!p.title.is_empty() || !p.container.is_empty())
                + if p.container.is_empty() { 0 } else { 3 };
            for _ in fields..10 {
                out.push('\t');
            }
            out.push_str("true");
        }
        out.push('\n');
    }
    out
}

/// The list from the text of the file, sorted whatever the file says.
pub fn parse_apps(text: &str) -> Vec<Placement> {
    let parsed = text.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        // Up to ten fields: app id, workspace, monitor, pid, process id, title, container, slot,
        // layout, maximized. A maximized window writes all ten, so ten has to parse.
        if fields.len() > 10 {
            return None;
        }
        let workspace = match fields.get(1).map(|f| f.trim()) {
            None | Some("") => None,
            Some(number) => Some(number.parse::<usize>().ok()?),
        };
        Some(Placement {
            app_id: unescape(fields[0]),
            workspace,
            output: fields.get(2).map(|f| unescape(f)).unwrap_or_default(),
            pid: fields.get(3).and_then(|f| f.trim().parse().ok()),
            proc_id: fields.get(4).and_then(|f| f.trim().parse().ok()),
            title: fields.get(5).map(|f| unescape(f)).unwrap_or_default(),
            container: fields.get(6).map(|f| unescape(f)).unwrap_or_default(),
            slot: fields.get(7).and_then(|f| f.trim().parse().ok()),
            layout: fields.get(8).map(|f| unescape(f)).unwrap_or_default(),
            maximized: fields.get(9).is_some_and(|f| *f == "true" || *f == "1"),
        })
    });
    sorted(parsed)
}

/// Whether the command `candidate` is one the config already starts with `autostart` or
/// `sh-autostart`, so restoring it would open the app twice. Same program (by file name) is
/// the same command, except for the programs that only wrap another one (`flatpak run X`,
/// `env FOO=1 X`, `sh -c ...`), where the whole command has to match.
pub fn is_autostarted(candidate: &[String], autostarted: &[Vec<String>]) -> bool {
    const WRAPPERS: &[&str] = &["flatpak", "snap", "env", "sh", "bash", "gtk-launch"];
    let program = |argv: &[String]| -> Option<String> {
        let first = argv.first()?;
        Path::new(first)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    let Some(wanted) = program(candidate) else {
        return false;
    };
    autostarted.iter().any(|other| {
        program(other).is_some_and(|name| {
            name == wanted && (!WRAPPERS.contains(&name.as_str()) || other == candidate)
        })
    })
}

/// The command an `sh-autostart` line starts, as far as it can be told without a shell: its
/// words up to the first thing the shell would treat specially (`&&`, `,`, a pipe, `&`), with
/// no attempt at quoting. Only used to tell whether an app is already started that way.
pub fn shell_line_command(line: &str) -> Vec<String> {
    line.split_whitespace()
        .take_while(|word| !matches!(*word, "&&" | "||" | ";" | "|" | "&"))
        .map(str::to_owned)
        .collect()
}

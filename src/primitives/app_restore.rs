//! The list kept for `restore-apps`, and the decisions made from it. Pure: no file system, no
//! Wayland.
//!
//! The list has a line for every open window: the app id, the workspace (0-based) and the name
//! of the monitor it is on, separated by tabs. An app with windows on two workspaces has two
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
        out.push('\n');
    }
    out
}

/// The list from the text of the file, sorted whatever the file says.
pub fn parse_apps(text: &str) -> Vec<Placement> {
    let parsed = text.lines().filter_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() > 3 {
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

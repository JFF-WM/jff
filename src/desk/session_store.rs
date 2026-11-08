//! The saved window state of xdg-session-management, and its file format. No Smithay or Wayland
//! types in here, so it can be tested on its own.
//!
//! One line per window: `session id`, `window name`, `mode`, `x`, `y`, `width`, `height`,
//! `workspace` and `output`, separated by tabs. Ids, names and output names are escaped (`\\`,
//! `\t`, `\n`) because the app chooses them. Lines that don't parse are skipped.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use super::{Pos, Side, WindowMode};

/// What is remembered of one window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedWindow {
    pub mode: WindowMode,
    /// Where the window floats: its own position and size, or for a maximized or fullscreen
    /// window the ones it floats back to.
    pub pos: Pos,
    pub size: (i32, i32),
    /// 0-based.
    pub workspace: usize,
    /// The monitor, by name. Empty: unknown.
    pub output: String,
}

/// Every saved window, by session id and then by the name the app gave the window.
pub type Store = HashMap<String, BTreeMap<String, SavedWindow>>;

fn mode_name(mode: WindowMode) -> &'static str {
    match mode {
        WindowMode::Floating => "floating",
        WindowMode::Maximized => "maximized",
        WindowMode::Fullscreen => "fullscreen",
        WindowMode::Snapped(Side::Left) => "snap-left",
        WindowMode::Snapped(Side::Right) => "snap-right",
        WindowMode::Snapped(Side::Top) => "snap-top",
        WindowMode::Snapped(Side::Bottom) => "snap-bottom",
    }
}

fn mode_from_name(name: &str) -> Option<WindowMode> {
    Some(match name {
        "floating" => WindowMode::Floating,
        "maximized" => WindowMode::Maximized,
        "fullscreen" => WindowMode::Fullscreen,
        "snap-left" => WindowMode::Snapped(Side::Left),
        "snap-right" => WindowMode::Snapped(Side::Right),
        "snap-top" => WindowMode::Snapped(Side::Top),
        "snap-bottom" => WindowMode::Snapped(Side::Bottom),
        _ => return None,
    })
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

/// The store as the text of the file. Sorted, so the file only changes when the state does.
pub fn format_store(store: &Store) -> String {
    let mut sessions: Vec<&String> = store.keys().collect();
    sessions.sort();
    let mut out = String::new();
    for id in sessions {
        for (name, w) in &store[id] {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape(id),
                escape(name),
                mode_name(w.mode),
                w.pos.0,
                w.pos.1,
                w.size.0,
                w.size.1,
                w.workspace,
                escape(&w.output),
            ));
        }
    }
    out
}

pub fn parse_store(text: &str) -> Store {
    let mut store = Store::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 9 {
            continue;
        }
        let parsed = (|| {
            Some(SavedWindow {
                mode: mode_from_name(f[2])?,
                pos: (f[3].parse().ok()?, f[4].parse().ok()?),
                size: (f[5].parse().ok()?, f[6].parse().ok()?),
                workspace: f[7].parse().ok()?,
                output: unescape(f[8]),
            })
        })();
        let Some(window) = parsed else {
            continue;
        };
        if f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        store
            .entry(unescape(f[0]))
            .or_default()
            .insert(unescape(f[1]), window);
    }
    store
}

/// `$XDG_STATE_HOME/jff-wm/sessions`, next to the other state jff keeps.
pub fn store_path() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let state =
        absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|h| h.join(".local/state")))?;
    Some(state.join("jff-wm").join("sessions"))
}

pub fn load_store() -> Store {
    store_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_store(&text))
        .unwrap_or_default()
}

/// Best effort, like the app launch counts: state that can't be saved isn't worth an error.
pub fn save_store(store: &Store) {
    let Some(path) = store_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let temporary = path.with_extension("tmp");
    if std::fs::write(&temporary, format_store(store)).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

/// A new session id: 128 random bits as hex.
pub fn new_session_id() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if random.is_err() {
        // No random device: the clock and the address of a local still differ between runs.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let local = 0u8;
        let mixed = nanos ^ ((&local as *const u8 as u128) << 64);
        bytes = mixed.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

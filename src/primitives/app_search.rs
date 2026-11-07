//! Pure code for the overview's search: no Smithay types, no file access, so it can be
//! unit-tested on its own (like `overview_grid.rs`).
//!
//! - [`parse_desktop_entry`] - one `.desktop` file to a [`DesktopEntry`], or `None` when the
//!   file must not be listed (`NoDisplay`, `Hidden`, an `OnlyShowIn` that doesn't apply, not
//!   an application).
//! - [`launch_plan`] - the program and arguments to run for an entry: field codes stripped,
//!   `Terminal=true` wrapped in a terminal, `Path=` as working directory.
//! - [`search_apps`] / [`window_matches`] - matching and ranking.
//! - [`Query`] - the text typed so far.
//! - [`Pick`] and its helpers - moving the selection over "filtered windows, then apps".

use std::collections::HashMap;

/// Most app results shown (and reachable with the keyboard).
pub const MAX_RESULTS: usize = 8;
/// Longest query, in characters.
pub const MAX_QUERY: usize = 64;

/// One application from a `.desktop` file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DesktopEntry {
    /// Desktop file ID: the file name without `.desktop`, with the directories of a nested file
    /// joined by `-` (`kde/foo.desktop` is `kde-foo`).
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub keywords: Vec<String>,
    pub exec: String,
    /// `Icon=`, a theme name or an absolute path.
    pub icon: String,
    /// `Path=`: the working directory.
    pub path: String,
    pub terminal: bool,
    /// `TryExec=`: the entry is only listed when this program exists. Checked by the caller -
    /// this module doesn't touch the file system.
    pub try_exec: String,
    /// Where the file was read from (`%k`).
    pub source: String,
}

/// What decides whether an entry applies here.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env<'a> {
    /// `XDG_CURRENT_DESKTOP`, split on `:`. Empty: `OnlyShowIn` isn't held against entries.
    pub desktops: &'a [String],
    /// Locales to look `Name[xx]` up in, best first: `["pt_BR", "pt"]`.
    pub locales: &'a [String],
}

/// Undo the escapes of a desktop file string value: `\s \n \t \r \\`.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            // `\,` only means something inside a list, anywhere else it's just the `,`.
            Some(';') => out.push(';'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split a `,`-separated list. `\,` is a `,` inside an item, empty items are dropped.
fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&';') => {
                current.push(';');
                chars.next();
            }
            // Keep the other escapes for `unescape`.
            '\\' => {
                current.push('\\');
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ';' => {
                items.push(unescape(current.trim()));
                current.clear();
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        items.push(unescape(current.trim()));
    }
    items.retain(|item| !item.is_empty());
    items
}

fn is_true(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn lists_overlap(list: &[String], desktops: &[String]) -> bool {
    list.iter()
        .any(|a| desktops.iter().any(|b| a.eq_ignore_ascii_case(b)))
}

/// Read one `.desktop` file. `id` and `source` are copied into the entry.
pub fn parse_desktop_entry(id: &str, source: &str, text: &str, env: &Env) -> Option<DesktopEntry> {
    let mut values: HashMap<&str, &str> = HashMap::new();
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // `[Desktop Entry]` is the only group that matters. Actions have groups of their
            // own (`[Desktop Action new-window]`) and must not overwrite its keys.
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            // The first one wins if a key is repeated.
            values.entry(key.trim()).or_insert_with(|| value.trim());
        }
    }

    let plain = |key: &str| values.get(key).copied();
    // `Name[pt_BR]` before `Name[pt]` before `Name`.
    let localized = |key: &str| -> Option<String> {
        for locale in env.locales {
            if let Some(value) = values.get(format!("{key}[{locale}]").as_str()) {
                return Some(unescape(value));
            }
        }
        plain(key).map(unescape)
    };

    if let Some(kind) = plain("Type") {
        if kind != "Application" {
            return None;
        }
    }
    if plain("NoDisplay").is_some_and(is_true) || plain("Hidden").is_some_and(is_true) {
        return None;
    }
    if !env.desktops.is_empty() {
        if let Some(only) = plain("OnlyShowIn") {
            let only = split_list(only);
            if !only.is_empty() && !lists_overlap(&only, env.desktops) {
                return None;
            }
        }
        if let Some(not) = plain("NotShowIn") {
            if lists_overlap(&split_list(not), env.desktops) {
                return None;
            }
        }
    }

    let name = localized("Name").filter(|n| !n.trim().is_empty())?;
    let exec = plain("Exec").map(str::trim).filter(|e| !e.is_empty())?;

    let mut keywords: Vec<String> = Vec::new();
    for locale in env.locales {
        if let Some(value) = values.get(format!("Keywords[{locale}]").as_str()) {
            keywords.extend(split_list(value));
            break;
        }
    }
    if let Some(value) = plain("Keywords") {
        keywords.extend(split_list(value));
    }

    Some(DesktopEntry {
        id: id.to_owned(),
        name,
        generic_name: localized("GenericName").unwrap_or_default(),
        keywords,
        exec: exec.to_owned(),
        icon: plain("Icon").map(unescape).unwrap_or_default(),
        path: plain("Path").map(unescape).unwrap_or_default(),
        terminal: plain("Terminal").is_some_and(is_true),
        try_exec: plain("TryExec").map(unescape).unwrap_or_default(),
        source: source.to_owned(),
    })
}

/// Split an `Exec` line into arguments: whitespace separates them, double quotes group, and
/// inside quotes a backslash escapes `"`, `` ` ``, `$` and `\`.
fn split_exec(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    // A token has started (an empty `""` is an argument too).
    let mut started = false;
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' => quoted = false,
                '\\' => match chars.peek() {
                    Some(&next) if matches!(next, '"' | '`' | '$' | '\\') => {
                        current.push(next);
                        chars.next();
                    }
                    _ => current.push('\\'),
                },
                _ => current.push(c),
            }
        } else if c == '"' {
            quoted = true;
            started = true;
        } else if c.is_whitespace() {
            if started {
                args.push(std::mem::take(&mut current));
                started = false;
            }
        } else {
            current.push(c);
            started = true;
        }
    }
    if started {
        args.push(current);
    }
    args
}

/// One argument with its field codes expanded. Codes that stand for files or URLs (`%f %F %u %U
/// %d %D %n %N`) and the deprecated ones (`%v %m`) become nothing, there is nothing to open.
fn expand_arg(arg: &str, entry: &DesktopEntry) -> Vec<String> {
    if arg == "%i" {
        return if entry.icon.is_empty() {
            Vec::new()
        } else {
            vec!["--icon".to_owned(), entry.icon.clone()]
        };
    }
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('c') => out.push_str(&entry.name),
            Some('k') => out.push_str(&entry.source),
            // Every other code (known or not) is dropped.
            Some(_) => {}
            None => out.push('%'),
        }
    }
    // An argument that was only a field code leaves no empty argument behind.
    if out.is_empty() && !arg.is_empty() {
        Vec::new()
    } else {
        vec![out]
    }
}

/// The program and arguments of `entry`'s `Exec` line, with field codes dealt with. Empty when
/// there's nothing to run.
pub fn exec_argv(entry: &DesktopEntry) -> Vec<String> {
    split_exec(&entry.exec)
        .iter()
        .flat_map(|arg| expand_arg(arg, entry))
        .collect()
}

/// What to put between a terminal and the command it runs: `-e` for most, and not the same for
/// all of them.
pub fn terminal_exec_args(terminal: &str) -> Vec<String> {
    let base = terminal.rsplit('/').next().unwrap_or(terminal);
    let args: &[&str] = match base {
        "gnome-terminal" | "kgx" | "ptyxis" | "xfce4-terminal" | "mate-terminal" => &["--"],
        "wezterm" => &["start", "--"],
        // These take the command as it is.
        "foot" | "kitty" => &[],
        _ => &["-e"],
    };
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// What to run for an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// Program first.
    pub argv: Vec<String>,
    /// Working directory (`Path=`), if the entry has one.
    pub cwd: Option<String>,
}

/// The command for `entry`, wrapped in `terminal` (a program name or path) when the entry wants
/// one. `None` when the `Exec` line is empty.
pub fn launch_plan(entry: &DesktopEntry, terminal: &str) -> Option<Launch> {
    let command = exec_argv(entry);
    if command.is_empty() {
        return None;
    }
    let argv = if entry.terminal {
        let mut argv = vec![terminal.to_owned()];
        argv.extend(terminal_exec_args(terminal));
        argv.extend(command);
        argv
    } else {
        command
    };
    Some(Launch {
        argv,
        cwd: (!entry.path.is_empty()).then(|| entry.path.clone()),
    })
}

/// Lowercase, accents removed: "Configurações" and "configuracoes" are the same word.
fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => c,
    }
}

fn fold(text: &str) -> Vec<char> {
    text.chars().map(fold_char).collect()
}

/// Start of a word: first character, or after something that isn't a letter or digit.
fn word_starts(hay: &[char]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous_alnum = false;
    for (i, c) in hay.iter().enumerate() {
        if c.is_alphanumeric() && !previous_alnum {
            starts.push(i);
        }
        previous_alnum = c.is_alphanumeric();
    }
    starts
}

fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// How well one word `needle` matches `text`: higher is better, `None` for no match. Both are
/// compared folded. With `subsequence`, also accept the letters of `needle` in order with gaps
/// ("ffx" in "Firefox"), at a low score.
fn word_score(needle: &[char], text: &str, subsequence: bool) -> Option<u32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay = fold(text);
    if hay.is_empty() {
        return None;
    }
    // Shorter texts are the better answer to the same query.
    let brevity = 50u32.saturating_sub(hay.len() as u32) / 2;

    if hay == needle {
        return Some(1000 + brevity);
    }
    if hay.starts_with(needle) {
        return Some(800 + brevity);
    }
    let starts = word_starts(&hay);
    let mut from = 0;
    while let Some(at) = find_sub(&hay, needle, from) {
        if starts.contains(&at) {
            return Some(600 + brevity);
        }
        from = at + 1;
    }
    if find_sub(&hay, needle, 0).is_some() {
        return Some(400 + brevity);
    }
    if !subsequence || needle.len() < 2 {
        return None;
    }
    // Letters in order, the first one at the start of a word ("ffx" finds Firefox, "fire" does
    // not find Configurações). Letters that follow each other, and ones that start a word, count
    // more.
    let mut score = 100u32;
    let mut at = 0;
    let mut last: Option<usize> = None;
    for &want in needle {
        let found =
            (at..hay.len()).find(|&i| hay[i] == want && (last.is_some() || starts.contains(&i)))?;
        if last.is_some_and(|l| l + 1 == found) {
            score += 12;
        }
        if starts.contains(&found) {
            score += 8;
        }
        last = Some(found);
        at = found + 1;
    }
    Some(score.min(399))
}

fn terms_of(query: &str) -> Vec<Vec<char>> {
    query.split_whitespace().map(fold).collect()
}

/// Score of an entry for `query`: `None` when it doesn't match. Every word of the query is
/// required, each is scored on its best field - name first, then generic name and keywords,
/// the desktop file ID last.
fn entry_score(terms: &[Vec<char>], entry: &DesktopEntry) -> Option<u32> {
    let mut total = 0u32;
    for term in terms {
        let mut best: Option<u32> = None;
        let mut offer = |score: Option<u32>, weight: u32| {
            if let Some(score) = score {
                best = Some(best.map_or(score * weight, |b| b.max(score * weight)));
            }
        };
        offer(word_score(term, &entry.name, true), 10);
        offer(word_score(term, &entry.generic_name, false), 5);
        for keyword in &entry.keywords {
            offer(word_score(term, keyword, false), 5);
        }
        offer(word_score(term, &entry.id, false), 3);
        total += best?;
    }
    Some(total)
}

/// Indices into `entries` of the apps matching `query`, best first, at most `limit`. Ties are
/// broken by launch count (`launches`, by entry ID), then by name. An empty query matches
/// nothing.
pub fn search_apps(
    entries: &[DesktopEntry],
    query: &str,
    launches: &HashMap<String, u32>,
    limit: usize,
) -> Vec<usize> {
    let terms = terms_of(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u32, u32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let score = entry_score(&terms, entry)?;
            Some((score, launches.get(&entry.id).copied().unwrap_or(0), i))
        })
        .collect();
    hits.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then_with(|| fold(&entries[a.2].name).cmp(&fold(&entries[b.2].name)))
            .then(a.2.cmp(&b.2))
    });
    hits.into_iter().take(limit).map(|(_, _, i)| i).collect()
}

/// Whether a window with `title` and `app_id` is a result for `query`. Every word of the query
/// has to be somewhere in one of the two. Windows are matched on what's written in them, with
/// no gaps: typing "fx" shouldn't find half the windows on the screen.
pub fn window_matches(query: &str, title: &str, app_id: &str) -> bool {
    terms_of(query).iter().all(|term| {
        word_score(term, title, false).is_some() || word_score(term, app_id, false).is_some()
    })
}

/// The text typed into the overview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
}

impl Query {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Add `c`. Control characters, spaces at the start, and anything past [`MAX_QUERY`] are
    /// ignored. Returns whether the query changed.
    pub fn push(&mut self, c: char) -> bool {
        if c.is_control() || (c.is_whitespace() && self.text.is_empty()) {
            return false;
        }
        if self.text.chars().count() >= MAX_QUERY {
            return false;
        }
        self.text.push(if c.is_whitespace() { ' ' } else { c });
        true
    }

    /// Remove the last character. Returns whether the query changed.
    pub fn backspace(&mut self) -> bool {
        self.text.pop().is_some()
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }
}

/// A selected result: a window among the filtered ones, or an app among the app results. Both
/// are indices into the lists as shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Window(usize),
    App(usize),
}

/// The result Enter should act on before any key is pressed: the first window, else the first
/// app.
pub fn first_pick(windows: usize, apps: usize) -> Option<Pick> {
    if windows > 0 {
        Some(Pick::Window(0))
    } else if apps > 0 {
        Some(Pick::App(0))
    } else {
        None
    }
}

/// `pick` if it still points at something, else the first result.
pub fn valid_pick(pick: Option<Pick>, windows: usize, apps: usize) -> Option<Pick> {
    match pick {
        Some(Pick::Window(i)) if i < windows => pick,
        Some(Pick::App(i)) if i < apps => pick,
        _ => first_pick(windows, apps),
    }
}

/// One step through every window, then every app, wrapping around. With nothing selected, the
/// first result (or the last, going backwards).
pub fn step_pick(pick: Option<Pick>, windows: usize, apps: usize, forward: bool) -> Option<Pick> {
    let total = windows + apps;
    if total == 0 {
        return None;
    }
    let at = |i: usize| {
        if i < windows {
            Pick::Window(i)
        } else {
            Pick::App(i - windows)
        }
    };
    let current = match pick {
        Some(Pick::Window(i)) if i < windows => Some(i),
        Some(Pick::App(i)) if i < apps => Some(windows + i),
        _ => None,
    };
    let next = match (current, forward) {
        (None, true) => 0,
        (None, false) => total - 1,
        (Some(i), true) => (i + 1) % total,
        (Some(i), false) => (i + total - 1) % total,
    };
    Some(at(next))
}

/// Up or down while an app is selected. Up from the first app goes to the last window, down
/// from the last app stays put. `None`: nothing to move to.
pub fn step_in_apps(app: usize, windows: usize, apps: usize, up: bool) -> Option<Pick> {
    if up {
        if app > 0 && app - 1 < apps {
            Some(Pick::App(app - 1))
        } else if windows > 0 {
            Some(Pick::Window(windows - 1))
        } else {
            None
        }
    } else if app + 1 < apps {
        Some(Pick::App(app + 1))
    } else {
        None
    }
}

/// An arrow key while an app is selected in a grid with `columns` tiles per row. Left and right
/// go to the previous and next tile, up and down go a row. Up from the first row goes to the
/// last window, down from the last row stays put (a short last row is reached from the column
/// above it by going to its last tile). `None`: nothing to move to.
pub fn step_in_app_grid(
    app: usize,
    windows: usize,
    apps: usize,
    columns: usize,
    dir: crate::primitives::overview_grid::Dir,
) -> Option<Pick> {
    use crate::primitives::overview_grid::Dir;
    let columns = columns.max(1);
    match dir {
        Dir::Left => (app > 0 && app - 1 < apps).then(|| Pick::App(app - 1)),
        Dir::Right => (app + 1 < apps).then(|| Pick::App(app + 1)),
        Dir::Up => {
            if app >= columns && app - columns < apps {
                Some(Pick::App(app - columns))
            } else if windows > 0 {
                Some(Pick::Window(windows - 1))
            } else {
                None
            }
        }
        Dir::Down => {
            if app + columns < apps {
                Some(Pick::App(app + columns))
            } else if apps > 0 && app / columns < (apps - 1) / columns {
                Some(Pick::App(apps - 1))
            } else {
                None
            }
        }
    }
}

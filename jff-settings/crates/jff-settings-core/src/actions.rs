//! What a bind can do, plus the little text helpers around it.

use crate::Arg;

/// What comes after the action name in a bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// nothing, like `maximize`
    None,
    /// program and arguments typed as one line: spawn "foot" "-e" "htop"
    Words,
    /// a single string taken as typed (spawn-sh, forward-key)
    Line,
    /// workspace number, from 1
    Number,
}

/// All the actions jff knows, written the way they go inside bind { }.
pub const ACTIONS: &[(&str, ArgKind)] = &[
    ("terminal", ArgKind::None),
    ("spawn", ArgKind::Words),
    ("spawn-sh", ArgKind::Line),
    ("maximize", ArgKind::None),
    ("fullscreen", ArgKind::None),
    ("snap-left", ArgKind::None),
    ("snap-right", ArgKind::None),
    ("snap-top", ArgKind::None),
    ("snap-bottom", ArgKind::None),
    ("close", ArgKind::None),
    ("overview", ArgKind::None),
    ("switch-window", ArgKind::None),
    ("switch-window-back", ArgKind::None),
    ("workspace", ArgKind::Number),
    ("move-to-workspace", ArgKind::Number),
    ("workspace-next", ArgKind::None),
    ("workspace-prev", ArgKind::None),
    ("reload-config", ArgKind::None),
    ("screenshot-area", ArgKind::None),
    ("screenshot-fullscreen", ArgKind::None),
    ("screenshot-window", ArgKind::None),
    ("toggle-shortcuts-inhibit", ArgKind::None),
    ("forward-key", ArgKind::Line),
    ("quit", ArgKind::None),
];

pub fn kind_of(action: &str) -> ArgKind {
    ACTIONS
        .iter()
        .find(|(name, _)| *name == action)
        .map_or(ArgKind::None, |(_, kind)| *kind)
}

/// Turns what the user typed into the action's arguments.
pub fn action_args(action: &str, text: &str) -> Result<Vec<Arg>, String> {
    match kind_of(action) {
        ArgKind::None => Ok(Vec::new()),
        ArgKind::Words => {
            let words = split_words(text);
            if words.is_empty() {
                Err(format!("{action} needs a program"))
            } else {
                Ok(words.into_iter().map(Arg::Str).collect())
            }
        }
        ArgKind::Line => match text.trim() {
            "" => Err(format!("{action} needs a value")),
            line => Ok(vec![Arg::Str(line.to_owned())]),
        },
        ArgKind::Number => match text.trim().parse::<i64>() {
            Ok(n) if n >= 1 => Ok(vec![Arg::Int(n)]),
            _ => Err(format!("{action} takes a workspace number from 1")),
        },
    }
}

/// Other direction: the text you'd type to get these args.
pub fn action_text(action: &str, args: &[Arg]) -> String {
    let strings: Vec<String> = args
        .iter()
        .map(|a| match a {
            Arg::Str(s) => s.clone(),
            Arg::Int(i) => i.to_string(),
            Arg::Float(f) => f.to_string(),
            Arg::Bool(b) => b.to_string(),
        })
        .collect();
    match kind_of(action) {
        ArgKind::Words => join_words(&strings),
        _ => strings.join(" "),
    }
}

/// Splits a line into words. Double quotes keep spaces together and a backslash takes the next
/// char literally.
pub fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut in_word, mut quoted) = (false, false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                    in_word = true;
                }
            }
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Reverse of split_words.
pub fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.is_empty()
                || w.chars()
                    .any(|c| c.is_whitespace() || c == '"' || c == '\\')
            {
                format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Combos are bare KDL names, so only letters, digits and + _ - . are allowed and the first
/// char has to be a letter. Symbols use their keysym name (comma, minus, Return). Whether the
/// keys and modifiers really exist is up to jff.
pub fn valid_combo(combo: &str) -> Result<(), String> {
    let first = combo.chars().next().ok_or("The key combination is empty")?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err("A key combination starts with a letter, like Alt+Return".into());
    }
    if !combo
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '-' | '.'))
    {
        return Err("Use letters, digits and + only; write symbols as names (comma, minus)".into());
    }
    if combo.ends_with('+') || combo.contains("++") {
        return Err("The key combination is missing its key, like Alt+Return".into());
    }
    Ok(())
}

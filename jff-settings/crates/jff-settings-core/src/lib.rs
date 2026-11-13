//! Editing jff's config.kdl without a GUI around it.
//!
//! The file stays a kdl::KdlDocument the whole time, so comments and formatting in the parts
//! we never touch come out the same after a save.
//!
//! Finding the file works like in jff: $JFF_CONFIG, then $XDG_CONFIG_HOME/jff/config.kdl, then
//! ~/.config/jff/config.kdl. No file yet means we start from DEFAULT_CONFIG and the first save
//! creates it.
//!
//! save() writes, runs `jff check-config` and puts the old file back if jff doesn't like it.
//! jff watches the file anyway, so a good save shows up right away.
//!
//! Nodes are addressed by path, like ["keyboard", "xkb", "layout"]. "window-rule#2" picks the
//! third window-rule and "*#3" the fourth child no matter its name (binds are named after their
//! key combo, so that's how you get at them). Careful: an unknown key inside a block jff knows
//! is an error for jff, only write keys it has.

pub mod actions;
pub mod modes;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Used when there's no file yet. Has to match DEFAULT_CONFIG in src/config/mod.rs of the
/// compositor.
pub const DEFAULT_CONFIG: &str = r#"keyboard {
    repeat-delay 200
    repeat-rate 25
}
binds {
    Alt+Return { terminal; }
    Alt+M { maximize; }
    Alt+F { fullscreen; }
    Alt+Q { close; }
    Alt+O { overview; }
    Alt+Tab { switch-window; }
    Alt+Shift+Tab { switch-window-back; }
    Alt+Shift+R { reload-config; }
    Ctrl+Alt+BackSpace { quit; }
    Super+Escape { toggle-shortcuts-inhibit; }
    Alt+1 { workspace 1; }
    Alt+2 { workspace 2; }
    Alt+3 { workspace 3; }
    Alt+4 { workspace 4; }
    Alt+5 { workspace 5; }
    Alt+6 { workspace 6; }
    Alt+7 { workspace 7; }
    Alt+8 { workspace 8; }
    Alt+9 { workspace 9; }
    Alt+Shift+1 { move-to-workspace 1; }
    Alt+Shift+2 { move-to-workspace 2; }
    Alt+Shift+3 { move-to-workspace 3; }
    Alt+Shift+4 { move-to-workspace 4; }
    Alt+Shift+5 { move-to-workspace 5; }
    Alt+Shift+6 { move-to-workspace 6; }
    Alt+Shift+7 { move-to-workspace 7; }
    Alt+Shift+8 { move-to-workspace 8; }
    Alt+Shift+9 { move-to-workspace 9; }
    Ctrl+Alt+Right { workspace-next; }
    Ctrl+Alt+Left { workspace-prev; }
    Print { screenshot-area; }
    Ctrl+Print { screenshot-fullscreen; }
    Alt+Print { screenshot-window; }
}
"#;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// Not valid KDL, or jff check-config explains what's wrong with it.
    Parse(String),
    /// jff check-config said no and the old file is back.
    Invalid(String),
    /// JFF_CONFIG, XDG_CONFIG_HOME and HOME are all unset.
    NoPath,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Parse(e) | Error::Invalid(e) => write!(f, "{}", e.trim()),
            Error::NoPath => write!(f, "no config path: JFF_CONFIG, XDG_CONFIG_HOME and HOME are unset"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Argument or property value, only the types jff's config actually uses.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Arg {
    fn to_kdl(&self) -> KdlValue {
        match self {
            Arg::Str(s) => KdlValue::String(s.clone()),
            Arg::Int(i) => KdlValue::Integer(i128::from(*i)),
            Arg::Float(f) => KdlValue::Float(*f),
            Arg::Bool(b) => KdlValue::Bool(*b),
        }
    }

    fn from_kdl(value: &KdlValue) -> Option<Arg> {
        match value {
            KdlValue::String(s) => Some(Arg::Str(s.clone())),
            KdlValue::Integer(i) => i64::try_from(*i).ok().map(Arg::Int),
            KdlValue::Float(f) => Some(Arg::Float(*f)),
            KdlValue::Bool(b) => Some(Arg::Bool(*b)),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub struct JffConfig {
    path: PathBuf,
    doc: KdlDocument,
    /// No file on disk yet, doc is DEFAULT_CONFIG.
    is_new: bool,
    dirty: bool,
    /// Set once the first save has copied the old file to <file>.bak.
    backed_up: bool,
}

impl JffConfig {
    /// None if we can't tell where home is.
    pub fn default_path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("JFF_CONFIG") {
            return Some(path.into());
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("jff").join("config.kdl"))
    }

    /// A missing file gives the defaults, it only gets created on save.
    pub fn load() -> Result<Self> {
        Self::load_from(Self::default_path().ok_or(Error::NoPath)?)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        let (text, is_new) = match fs::read_to_string(&path) {
            Ok(text) => (text, false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (DEFAULT_CONFIG.to_string(), true),
            Err(e) => return Err(e.into()),
        };
        let doc = match text.parse::<KdlDocument>() {
            Ok(doc) => doc,
            // check-config points at the actual line, the parser error alone is a lot less useful
            Err(e) => {
                let why = match check_with_jff(&path) {
                    Err(Error::Invalid(why)) => why,
                    _ => e.to_string(),
                };
                return Err(Error::Parse(why));
            }
        };
        Ok(Self { path, doc, is_new, dirty: false, backed_up: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True until the first save creates the file.
    pub fn is_new(&self) -> bool {
        self.is_new
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    // reads

    pub fn has(&self, path: &[&str]) -> bool {
        self.find(path).is_some()
    }

    pub fn get_int(&self, path: &[&str]) -> Option<i64> {
        let value = self.find(path).and_then(first_arg)?;
        i64::try_from(value.as_integer()?).ok()
    }

    /// Takes 3 and 3.0 alike.
    pub fn get_float(&self, path: &[&str]) -> Option<f64> {
        match self.find(path).and_then(first_arg)? {
            KdlValue::Float(f) => Some(*f),
            KdlValue::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn get_str(&self, path: &[&str]) -> Option<String> {
        self.find(path).and_then(first_arg)?.as_string().map(str::to_owned)
    }

    /// jff takes some settings as `parent N` and also as `parent { child N }` (workspace count,
    /// hot corner size). The child wins, then the parent's own number.
    pub fn get_int_or_positional(&self, path: &[&str]) -> Option<i64> {
        self.get_int(path).or_else(|| {
            let parent = path.get(..path.len().checked_sub(1)?)?;
            self.get_int(parent)
        })
    }

    /// Flags the way jff reads them: bare name is on, #false is off, "yes"/"no" and "on"/"off"
    /// strings work too. Falls back to `default` when the node is missing.
    pub fn flag(&self, path: &[&str], default: bool) -> bool {
        let Some(node) = self.find(path) else { return default };
        match first_arg(node) {
            None => true,
            Some(value) => value
                .as_bool()
                .or_else(|| match value.as_string() {
                    Some("true" | "yes" | "on") => Some(true),
                    Some("false" | "no" | "off") => Some(false),
                    _ => None,
                })
                .unwrap_or(default),
        }
    }

    pub fn args(&self, path: &[&str]) -> Vec<Arg> {
        self.find(path).map(node_args).unwrap_or_default()
    }

    /// One key=value property.
    pub fn get_prop(&self, path: &[&str], key: &str) -> Option<Arg> {
        let node = self.find(path)?;
        let entry = node.entries().iter().find(|e| e.name().map(|n| n.value()) == Some(key))?;
        Arg::from_kdl(entry.value())
    }

    /// Counts the children of `parent` (&[] is the top level) with this name, "*" matches any.
    pub fn count(&self, parent: &[&str], name: &str) -> usize {
        self.children_of(parent)
            .map_or(0, |doc| doc.nodes().iter().filter(|n| name_matches(n, name)).count())
    }

    /// Child names in file order.
    pub fn child_names(&self, parent: &[&str]) -> Vec<String> {
        self.children_of(parent)
            .map(|doc| doc.nodes().iter().map(|n| n.name().value().to_owned()).collect())
            .unwrap_or_default()
    }

    /// First child plus its args, which is the action of a bind.
    pub fn first_child(&self, path: &[&str]) -> Option<(String, Vec<Arg>)> {
        let child = self.find(path)?.children()?.nodes().first()?;
        Some((child.name().value().to_owned(), node_args(child)))
    }

    // writes

    pub fn set_int(&mut self, path: &[&str], value: i64) {
        self.set_args(path, &[Arg::Int(value)]);
    }

    pub fn set_float(&mut self, path: &[&str], value: f64) {
        self.set_args(path, &[Arg::Float(value)]);
    }

    pub fn set_str(&mut self, path: &[&str], value: &str) {
        self.set_args(path, &[Arg::Str(value.to_owned())]);
    }

    /// Mirror of get_int_or_positional. Writes wherever the value already lives, or as a child if
    /// it isn't there yet.
    pub fn set_int_or_positional(&mut self, path: &[&str], value: i64) {
        if self.get_int(path).is_none() {
            if let Some(parent) = path.get(..path.len().saturating_sub(1)).filter(|p| !p.is_empty()) {
                if self.get_int(parent).is_some() {
                    self.set_int(parent, value);
                    return;
                }
            }
        }
        self.set_int(path, value);
    }

    /// Bare name for on, #false for off.
    pub fn set_flag(&mut self, path: &[&str], on: bool) {
        let node = ensure_node(&mut self.doc, path);
        node.entries_mut().retain(|e| e.name().is_some());
        if !on {
            node.entries_mut().insert(0, KdlEntry::new(KdlValue::Bool(false)));
        }
        self.dirty = true;
    }

    /// For switches that only exist when written (off, content-type-hint). On is the bare name,
    /// off just removes the node.
    pub fn set_presence(&mut self, path: &[&str], on: bool) {
        if on {
            let node = ensure_node(&mut self.doc, path);
            node.entries_mut().retain(|e| e.name().is_some());
            self.dirty = true;
        } else {
            self.remove(path);
        }
    }

    /// Swaps the positional args, properties stay.
    pub fn set_args(&mut self, path: &[&str], values: &[Arg]) {
        let node = ensure_node(&mut self.doc, path);
        let props: Vec<KdlEntry> = node.entries().iter().filter(|e| e.name().is_some()).cloned().collect();
        let entries = node.entries_mut();
        entries.clear();
        entries.extend(values.iter().map(|v| KdlEntry::new(v.to_kdl())));
        entries.extend(props);
        self.dirty = true;
    }

    /// Sets one key=value, makes the node if it's missing.
    pub fn set_prop(&mut self, path: &[&str], key: &str, value: Arg) {
        let node = ensure_node(&mut self.doc, path);
        let entry = KdlEntry::new_prop(key, value.to_kdl());
        match node.entries_mut().iter_mut().find(|e| e.name().map(|n| n.value()) == Some(key)) {
            Some(existing) => *existing = entry,
            None => node.entries_mut().push(entry),
        }
        self.dirty = true;
    }

    /// Removes a property. If the node ends up empty it goes too, jff rejects a bare match line.
    pub fn remove_prop(&mut self, path: &[&str], key: &str) {
        let Some(node) = find_node_mut(&mut self.doc, path) else { return };
        node.entries_mut().retain(|e| e.name().map(|n| n.value()) != Some(key));
        let empty = node.entries().is_empty() && node.children().is_none();
        self.dirty = true;
        if empty {
            self.remove(path);
        }
    }

    /// Removing a node puts jff's default back in charge.
    pub fn remove(&mut self, path: &[&str]) {
        if remove_node(&mut self.doc, path) {
            self.dirty = true;
        }
    }

    /// Adds a child to `parent` (&[] is the top level), making parent if needed. Gives back its
    /// index among the children with the same name.
    pub fn add(&mut self, parent: &[&str], name: &str, args: &[Arg]) -> usize {
        let doc = doc_at(&mut self.doc, parent);
        let mut node = KdlNode::new(name);
        node.entries_mut().extend(args.iter().map(|a| KdlEntry::new(a.to_kdl())));
        doc.nodes_mut().push(node);
        self.dirty = true;
        doc.nodes().iter().filter(|n| n.name().value() == name).count() - 1
    }

    /// Order matters for window rules, hence the swap.
    pub fn swap(&mut self, parent: &[&str], name: &str, a: usize, b: usize) {
        let Some(doc) = existing_doc_at(&mut self.doc, parent) else { return };
        let at: Vec<usize> = doc
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| name_matches(n, name))
            .map(|(i, _)| i)
            .collect();
        if let (Some(&x), Some(&y)) = (at.get(a), at.get(b)) {
            doc.nodes_mut().swap(x, y);
            self.dirty = true;
        }
    }

    /// Gives a bind another key combo. The caller checks the name first, see actions::valid_combo.
    pub fn rename(&mut self, path: &[&str], name: &str) {
        if let Some(node) = find_node_mut(&mut self.doc, path) {
            node.set_name(name);
            self.dirty = true;
        }
    }

    /// Replaces the first child or adds one. That's the action of a bind.
    pub fn set_first_child(&mut self, path: &[&str], name: &str, args: &[Arg]) {
        let children = ensure_node(&mut self.doc, path).ensure_children();
        let mut child = KdlNode::new(name);
        child.entries_mut().extend(args.iter().map(|a| KdlEntry::new(a.to_kdl())));
        match children.nodes_mut().first_mut() {
            Some(first) => *first = child,
            None => children.nodes_mut().push(child),
        }
        self.dirty = true;
    }

    fn find(&self, path: &[&str]) -> Option<&KdlNode> {
        find_node(&self.doc, path)
    }

    fn children_of(&self, parent: &[&str]) -> Option<&KdlDocument> {
        if parent.is_empty() {
            Some(&self.doc)
        } else {
            find_node(&self.doc, parent)?.children()
        }
    }

    // save

    /// Writes the file, then runs jff check-config on it. If jff refuses, the old file comes back
    /// (or the new one is deleted when there was none) and Error::Invalid has the reason. No jff on
    /// PATH means no check.
    ///
    /// The first save of a session also copies the old file to <file>.bak.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let previous = match fs::read_to_string(&self.path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if let (Some(text), false) = (&previous, self.backed_up) {
            let mut name = self.path.file_name().unwrap_or_default().to_os_string();
            name.push(".bak");
            fs::write(self.path.with_file_name(name), text)?;
            self.backed_up = true;
        }
        write_atomic(&self.path, &self.doc.to_string())?;
        if let Err(e) = check_with_jff(&self.path) {
            match previous {
                Some(text) => {
                    let _ = write_atomic(&self.path, &text);
                }
                None => {
                    let _ = fs::remove_file(&self.path);
                }
            }
            return Err(e);
        }
        self.is_new = false;
        self.dirty = false;
        Ok(())
    }
}

fn first_arg(node: &KdlNode) -> Option<&KdlValue> {
    node.entries().iter().find(|e| e.name().is_none()).map(|e| e.value())
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let tmp = path.with_file_name(format!(".{name}.settings-tmp"));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// `jff check-config FILE` exits 0 if it loads and 1 if not.
fn check_with_jff(file: &Path) -> Result<()> {
    match Command::new("jff").arg("check-config").arg(file).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(Error::Invalid(String::from_utf8_lossy(&out.stderr).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn node_args(node: &KdlNode) -> Vec<Arg> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| Arg::from_kdl(e.value()))
        .collect()
}

/// "window-rule#2" is the third window-rule, no # means the first. "*" matches any name.
fn segment(seg: &str) -> (&str, usize) {
    if let Some((name, index)) = seg.rsplit_once('#') {
        if let (false, Ok(index)) = (name.is_empty(), index.parse()) {
            return (name, index);
        }
    }
    (seg, 0)
}

fn name_matches(node: &KdlNode, name: &str) -> bool {
    name == "*" || node.name().value() == name
}

fn find_node<'a>(doc: &'a KdlDocument, path: &[&str]) -> Option<&'a KdlNode> {
    let (first, rest) = path.split_first()?;
    let (name, index) = segment(first);
    let node = doc.nodes().iter().filter(|n| name_matches(n, name)).nth(index)?;
    if rest.is_empty() {
        Some(node)
    } else {
        find_node(node.children()?, rest)
    }
}

fn find_node_mut<'a>(doc: &'a mut KdlDocument, path: &[&str]) -> Option<&'a mut KdlNode> {
    let (first, rest) = path.split_first()?;
    let (name, index) = segment(first);
    let node = doc.nodes_mut().iter_mut().filter(|n| name_matches(n, name)).nth(index)?;
    if rest.is_empty() {
        Some(node)
    } else {
        find_node_mut(node.children_mut().as_mut()?, rest)
    }
}

fn ensure_node<'a>(doc: &'a mut KdlDocument, path: &[&str]) -> &'a mut KdlNode {
    let (first, rest) = path.split_first().expect("empty path");
    let (name, index) = segment(first);
    while doc.nodes().iter().filter(|n| name_matches(n, name)).count() <= index {
        doc.nodes_mut().push(KdlNode::new(if name == "*" { "node" } else { name }));
    }
    let node = doc
        .nodes_mut()
        .iter_mut()
        .filter(|n| name_matches(n, name))
        .nth(index)
        .expect("just inserted");
    if rest.is_empty() {
        node
    } else {
        ensure_node(node.ensure_children(), rest)
    }
}

fn doc_at<'a>(doc: &'a mut KdlDocument, parent: &[&str]) -> &'a mut KdlDocument {
    if parent.is_empty() {
        doc
    } else {
        ensure_node(doc, parent).ensure_children()
    }
}

fn existing_doc_at<'a>(doc: &'a mut KdlDocument, parent: &[&str]) -> Option<&'a mut KdlDocument> {
    if parent.is_empty() {
        Some(doc)
    } else {
        find_node_mut(doc, parent)?.children_mut().as_mut()
    }
}

fn remove_node(doc: &mut KdlDocument, path: &[&str]) -> bool {
    let Some((first, rest)) = path.split_first() else { return false };
    let (name, index) = segment(first);
    if rest.is_empty() {
        let at = doc
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| name_matches(n, name))
            .nth(index)
            .map(|(i, _)| i);
        match at {
            Some(i) => {
                doc.nodes_mut().remove(i);
                true
            }
            None => false,
        }
    } else {
        let node = doc.nodes_mut().iter_mut().filter(|n| name_matches(n, name)).nth(index);
        match node.and_then(|n| n.children_mut().as_mut()) {
            Some(children) => remove_node(children, rest),
            None => false,
        }
    }
}

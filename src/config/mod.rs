//! User configuration, read from a KDL file.
//!
//! Looked up at `$JFF_CONFIG`, then `$XDG_CONFIG_HOME/jff/config.kdl`, then
//! `~/.config/jff/config.kdl`. When none exists, the built-in [`DEFAULT_CONFIG`] is used.
//!
//! The format is one KDL document. Top-level nodes: `workspaces`, `maximize-new-windows`,
//! `no-client-decorations`, `drag-modifier`, `screenshot { … }`, `keyboard { … }`,
//! `autostart` / `sh-autostart` lines, `outputs { output "NAME" { … } }`, and
//! `binds { "Combo" { action, } }`. Unknown top-level nodes are silently ignored, an
//! unknown key *inside* a known block is an error.
//!
//! Two things that bite when editing the file by hand:
//! - `adaptive-sync` takes a quoted string (`"off"` / `"fullscreen"` / `"always"`), not a
//!   bare flag.
//! - `maximize-new-windows`, `no-client-decorations`, `numlock`, `include-cursor` and
//!   `copy-to-clipboard` accept a bare name (`numlock`), `true` / `false`, `"true"` /
//!   `"false"`, `"yes"` / `"no"`, or `"on"` / `"off"`.
//!
//! `DEFAULT_CONFIG` is what runs when the user has no file. It has to parse: `Config::default`
//! goes through `parse()` and logs an error if it fails.

use std::path::PathBuf;

use anyhow::{bail, Context};
use kdl::{KdlDocument, KdlNode, KdlValue};
use smithay::input::keyboard::{xkb, Keysym, ModifiersState, XkbConfig};

use crate::desk::Goto;

/// Default number of workspaces.
pub const DEFAULT_WORKSPACES: usize = 9;
/// Upper bound for `workspaces`.
pub const MAX_WORKSPACES: usize = 64;

/// Default key repeat delay (ms) and rate (per second).
pub const DEFAULT_REPEAT_DELAY: i32 = 200;
pub const DEFAULT_REPEAT_RATE: i32 = 25;
/// Upper bounds for the repeat settings.
const MAX_REPEAT_DELAY: i32 = 10_000;
const MAX_REPEAT_RATE: i32 = 1_000;

/// The config used when the user has no file. See the module doc for the two
/// gotchas.
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

/// Something a key binding can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Run a program with arguments, without a shell.
    Spawn(Vec<String>),
    /// Run a command line through `sh -c`.
    SpawnShell(String),
    /// Open `$JFF_TERMINAL`, `$TERMINAL`, or alacritty.
    Terminal,
    /// Toggle maximize on the focused window.
    Maximize,
    /// Toggle fullscreen on the focused window.
    Fullscreen,
    /// Ask the focused window to close.
    Close,
    /// Show every window of the current monitor as a thumbnail.
    Overview,
    /// Switch to a workspace.
    Workspace(Goto),
    /// Send the focused window to workspace `n` (0-based).
    MoveToWorkspace(usize),
    /// Re-read the config file.
    ReloadConfig,
    /// Screenshot: frozen picture of the monitor under the pointer, drag a rectangle.
    ScreenshotArea,
    /// Screenshot: the whole monitor under the pointer.
    ScreenshotFullscreen,
    /// Screenshot: the window the user works in.
    ScreenshotWindow,
    /// Toggle the keyboard-shortcuts inhibitor of the focused window.
    ToggleShortcutsInhibit,
    /// Stop the compositor.
    Quit,
}

/// The modifier keys a binding requires, compared exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
}

impl Mods {
    fn from_state(mods: &ModifiersState) -> Self {
        Self {
            ctrl: mods.ctrl,
            alt: mods.alt,
            shift: mods.shift,
            logo: mods.logo,
        }
    }
}

/// A mouse button a binding can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Back,
    Forward,
}

impl MouseButton {
    /// Mice send BTN_SIDE / BTN_EXTRA, some devices (and winit) send
    /// BTN_BACK / BTN_FORWARD. Accept both.
    pub fn from_code(code: u32) -> Option<Self> {
        match code {
            0x113 | 0x116 => Some(Self::Back),    // BTN_SIDE, BTN_BACK
            0x114 | 0x115 => Some(Self::Forward), // BTN_EXTRA, BTN_FORWARD
            _ => None,
        }
    }
}

/// What fires a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Key(Keysym),
    Button(MouseButton),
}

/// One key or button combination and the action it triggers.
#[derive(Clone, Debug)]
struct Binding {
    mods: Mods,
    trigger: Trigger,
    action: Action,
}

/// `WIDTHxHEIGHT`, optionally `@REFRESH` in Hz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeRequest {
    pub width: i32,
    pub height: i32,
    pub refresh: Option<i32>,
}

/// When a monitor runs with variable refresh rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Vrr {
    Off,
    /// Only while a fullscreen window is on top on that monitor.
    #[default]
    Fullscreen,
    Always,
}

/// What the user asked for one monitor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputConfig {
    /// Connector name, like `DP-1`.
    pub name: String,
    /// Leave the monitor dark.
    pub off: bool,
    /// Top-left corner in the shared desktop space.
    pub position: Option<(i32, i32)>,
    pub mode: Option<ModeRequest>,
    /// ICC profile whose calibration curves (`vcgt`) are applied to the monitor.
    pub icc_profile: Option<std::path::PathBuf>,
    /// Variable refresh rate.
    pub vrr: Vrr,
    /// Set the connector's `content type` to Game while a fullscreen window says it is a game.
    pub content_type_hint: bool,
}

/// Keyboard layout and key repeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardConfig {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: Option<String>,
    /// Milliseconds before a held key starts repeating.
    pub repeat_delay: i32,
    /// Repeats per second. 0 = no repeat.
    pub repeat_rate: i32,
    /// Num Lock state at startup and after a keymap change.
    pub numlock: bool,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            rules: String::new(),
            model: String::new(),
            layout: String::new(),
            variant: String::new(),
            options: None,
            repeat_delay: DEFAULT_REPEAT_DELAY,
            repeat_rate: DEFAULT_REPEAT_RATE,
            numlock: true,
        }
    }
}

impl KeyboardConfig {
    /// The keymap settings in the form smithay wants.
    pub fn xkb_config(&self) -> XkbConfig<'_> {
        XkbConfig {
            rules: &self.rules,
            model: &self.model,
            layout: &self.layout,
            variant: &self.variant,
            options: self.options.clone(),
        }
    }

    /// Whether the keymap settings differ. Repeat settings are not part of this.
    pub fn keymap_differs(&self, other: &Self) -> bool {
        (
            &self.rules,
            &self.model,
            &self.layout,
            &self.variant,
            &self.options,
        ) != (
            &other.rules,
            &other.model,
            &other.layout,
            &other.variant,
            &other.options,
        )
    }

    /// Ask xkbcommon to build a keymap. Catches typos in `layout` / `variant` /
    /// `options` before they reach the seat.
    fn validate(&self) -> anyhow::Result<()> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &context,
            &self.rules,
            &self.model,
            &self.layout,
            &self.variant,
            self.options.clone(),
            xkb::COMPILE_NO_FLAGS,
        )
        .map(|_| ())
        .context("xkb cannot build a keymap from these names (check layout, variant, options)")
    }
}

/// What the `screenshot` block of the config says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotConfig {
    /// Folder the PNG goes in. `~` is the home directory. `None` is
    /// `$XDG_PICTURES_DIR/Screenshots`, else `~/Pictures/Screenshots`.
    pub path: Option<String>,
    /// Draw the cursor in the picture. TTY backend only.
    pub include_cursor: bool,
    /// Also hand the PNG to `wl-copy`. The file is written either way.
    pub copy_to_clipboard: bool,
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            path: None,
            include_cursor: false,
            copy_to_clipboard: true,
        }
    }
}

/// The parsed configuration.
#[derive(Clone, Debug)]
pub struct Config {
    keyboard: KeyboardConfig,
    binds: Vec<Binding>,
    outputs: Vec<OutputConfig>,
    workspaces: usize,
    maximize_new_windows: bool,
    no_client_decorations: bool,
    drag_modifier: DragModifier,
    screenshot: ScreenshotConfig,
    autostart: Vec<Action>,
}

/// Held while dragging with the mouse, moves or resizes a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DragModifier {
    #[default]
    Alt,
    Super,
    Ctrl,
    Shift,
}

impl Default for Config {
    fn default() -> Self {
        Self::parse(DEFAULT_CONFIG).unwrap_or_else(|err| {
            // DEFAULT_CONFIG is supposed to parse. If it doesn't, start anyway
            // with a config that has no binds, no outputs, no autostart.
            tracing::error!("built-in config is invalid: {err:#}");
            Self {
                keyboard: KeyboardConfig::default(),
                binds: Vec::new(),
                outputs: Vec::new(),
                workspaces: DEFAULT_WORKSPACES,
                maximize_new_windows: true,
                no_client_decorations: false,
                drag_modifier: DragModifier::Alt,
                screenshot: ScreenshotConfig::default(),
                autostart: Vec::new(),
            }
        })
    }
}

impl Config {
    /// Load the file, falling back to defaults when it is missing.
    ///
    /// # Errors
    ///
    /// File exists but can't be read or parsed.
    pub fn load() -> anyhow::Result<Self> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                tracing::info!("loading config from {}", path.display());
                Self::parse(&text).with_context(|| format!("in {}", path.display()))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Where the file is looked up. `None` if no home directory is known.
    pub fn path() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("JFF_CONFIG") {
            return Some(path.into());
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("jff").join("config.kdl"))
    }

    /// Parse config text.
    ///
    /// # Errors
    ///
    /// Malformed KDL, unknown keys inside known blocks, unknown modifiers,
    /// unknown or malformed actions.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let doc: KdlDocument = text.parse()?;
        let mut binds = Vec::new();

        if let Some(children) = doc.get("binds").and_then(KdlNode::children) {
            for node in children.nodes() {
                let combo = node.name().value();
                let (mods, trigger) =
                    parse_combo(combo).with_context(|| format!("bind {combo:?}"))?;
                let action_node = node
                    .children()
                    .and_then(|c| c.nodes().first())
                    .with_context(|| format!("bind {combo:?} has no action"))?;
                let action =
                    parse_action(action_node).with_context(|| format!("bind {combo:?}"))?;
                binds.push(Binding {
                    mods,
                    trigger,
                    action,
                });
            }
        }

        let mut outputs = Vec::new();
        if let Some(children) = doc.get("outputs").and_then(KdlNode::children) {
            for node in children.nodes() {
                if node.name().value() != "output" {
                    bail!("unknown entry {:?} in outputs", node.name().value());
                }
                outputs.push(parse_output(node).context("in outputs")?);
            }
        }

        let workspaces = match doc.get("workspaces") {
            None => DEFAULT_WORKSPACES,
            Some(node) => positional(node)
                .first()
                .and_then(|value| value.as_integer())
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| (1..=MAX_WORKSPACES).contains(n))
                .with_context(|| format!("workspaces takes a number from 1 to {MAX_WORKSPACES}"))?,
        };

        let maximize_new_windows = parse_flag(&doc, "maximize-new-windows", true)?;
        let no_client_decorations = parse_flag(&doc, "no-client-decorations", false)?;
        let drag_modifier = match doc.get("drag-modifier") {
            None => DragModifier::Alt,
            Some(node) => match positional(node).as_slice() {
                [value] => match value.as_string().map(str::to_ascii_lowercase).as_deref() {
                    Some("alt") => DragModifier::Alt,
                    Some("super" | "logo" | "mod4") => DragModifier::Super,
                    Some("ctrl" | "control") => DragModifier::Ctrl,
                    Some("shift") => DragModifier::Shift,
                    _ => bail!("drag-modifier takes Alt, Super, Ctrl or Shift"),
                },
                _ => bail!("drag-modifier takes one value: Alt, Super, Ctrl or Shift"),
            },
        };

        let keyboard = match doc.get("keyboard") {
            None => KeyboardConfig::default(),
            Some(node) => parse_keyboard(node).context("in keyboard")?,
        };

        let screenshot = match doc.get("screenshot") {
            None => ScreenshotConfig::default(),
            Some(node) => parse_screenshot(node).context("in screenshot")?,
        };

        let mut autostart = Vec::new();
        for node in doc.nodes() {
            let name = node.name().value();
            if matches!(name, "autostart" | "sh-autostart") {
                autostart.push(parse_startup(node).with_context(|| format!("in {name}"))?);
            }
        }

        Ok(Self {
            keyboard,
            binds,
            outputs,
            workspaces,
            maximize_new_windows,
            no_client_decorations,
            drag_modifier,
            screenshot,
            autostart,
        })
    }

    /// `autostart` + `sh-autostart`, in file order. Only [`Action::Spawn`] and
    /// [`Action::SpawnShell`] appear here.
    pub fn autostart(&self) -> &[Action] {
        &self.autostart
    }

    /// The screenshot block.
    pub fn screenshot(&self) -> &ScreenshotConfig {
        &self.screenshot
    }

    /// Keyboard layout and key repeat.
    pub fn keyboard(&self) -> &KeyboardConfig {
        &self.keyboard
    }

    /// Whether new windows start maximized.
    pub fn maximize_new_windows(&self) -> bool {
        self.maximize_new_windows
    }

    /// Whether apps are asked to drop their own titlebar and borders.
    pub fn no_client_decorations(&self) -> bool {
        self.no_client_decorations
    }

    /// Whether the drag modifier (Alt by default) is held.
    pub fn drag_modifier_held(&self, mods: &ModifiersState) -> bool {
        match self.drag_modifier {
            DragModifier::Alt => mods.alt,
            DragModifier::Super => mods.logo,
            DragModifier::Ctrl => mods.ctrl,
            DragModifier::Shift => mods.shift,
        }
    }

    /// How many workspaces there are.
    pub fn workspaces(&self) -> usize {
        self.workspaces
    }

    /// What the config says about `name`, if anything.
    pub fn output(&self, name: &str) -> Option<&OutputConfig> {
        self.outputs.iter().find(|o| o.name == name)
    }

    /// Edit (or add) the entry for `name`. Used by `wlr-output-management`,
    /// lasts until `reload-config`.
    pub fn update_output(&mut self, name: &str, change: impl FnOnce(&mut OutputConfig)) {
        let index = match self.outputs.iter().position(|o| o.name == name) {
            Some(index) => index,
            None => {
                self.outputs.push(OutputConfig {
                    name: name.to_string(),
                    ..Default::default()
                });
                self.outputs.len() - 1
            }
        };
        change(&mut self.outputs[index]);
    }

    /// The action bound to `sym` with exactly `mods`, if any.
    pub fn action_for(&self, mods: &ModifiersState, sym: Keysym) -> Option<&Action> {
        let mods = Mods::from_state(mods);
        self.binds
            .iter()
            .find(|b| b.mods == mods && b.trigger == Trigger::Key(sym))
            .map(|b| &b.action)
    }

    /// The action bound to the bindable mouse button with evdev code `code`,
    /// with exactly `mods`, if any.
    pub fn button_action_for(&self, mods: &ModifiersState, code: u32) -> Option<&Action> {
        let button = MouseButton::from_code(code)?;
        let mods = Mods::from_state(mods);
        self.binds
            .iter()
            .find(|b| b.mods == mods && b.trigger == Trigger::Button(button))
            .map(|b| &b.action)
    }
}

/// A boolean setting: absent → `default`, bare name → on, otherwise
/// `true` / `false` (or `"true"` / `"false"`, `"yes"` / `"no"`, `"on"` / `"off"`).
fn parse_flag(doc: &KdlDocument, name: &str, default: bool) -> anyhow::Result<bool> {
    match doc.get(name) {
        None => Ok(default),
        Some(node) => flag_value(node),
    }
}

fn flag_value(node: &KdlNode) -> anyhow::Result<bool> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [] => Ok(true),
        [value] => value
            .as_bool()
            .or_else(|| match value.as_string() {
                Some("true" | "yes" | "on") => Some(true),
                Some("false" | "no" | "off") => Some(false),
                _ => None,
            })
            .with_context(|| format!("{name} takes true or false")),
        _ => bail!("{name} takes one value: true or false"),
    }
}

/// Positional arguments of `node`, excluding `key=value` properties.
fn positional(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| entry.value())
        .collect()
}

/// One string argument, for settings like `layout "us"`.
fn string_arg(node: &KdlNode) -> anyhow::Result<String> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_string()
            .map(str::to_owned)
            .with_context(|| format!("{name} takes a string")),
        _ => bail!("{name} takes one string"),
    }
}

/// One integer argument in `0..=max`.
fn int_arg(node: &KdlNode, max: i32) -> anyhow::Result<i32> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_integer()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (0..=max).contains(n))
            .with_context(|| format!("{name} takes a number from 0 to {max}")),
        _ => bail!("{name} takes one number from 0 to {max}"),
    }
}

/// The `keyboard { … }` block.
fn parse_keyboard(node: &KdlNode) -> anyhow::Result<KeyboardConfig> {
    let mut config = KeyboardConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "xkb" => {
                for setting in child.children().map(KdlDocument::nodes).unwrap_or_default() {
                    let value = string_arg(setting).context("in xkb")?;
                    match setting.name().value() {
                        "rules" => config.rules = value,
                        "model" => config.model = value,
                        "layout" => config.layout = value,
                        "variant" => config.variant = value,
                        "options" => config.options = Some(value).filter(|v| !v.is_empty()),
                        other => bail!("unknown xkb setting {other:?}"),
                    }
                }
            }
            "repeat-delay" => config.repeat_delay = int_arg(child, MAX_REPEAT_DELAY)?,
            "repeat-rate" => config.repeat_rate = int_arg(child, MAX_REPEAT_RATE)?,
            "numlock" => config.numlock = flag_value(child)?,
            other => bail!("unknown keyboard setting {other:?}"),
        }
    }
    config.validate()?;
    Ok(config)
}

/// The `screenshot { … }` block.
fn parse_screenshot(node: &KdlNode) -> anyhow::Result<ScreenshotConfig> {
    let mut config = ScreenshotConfig::default();
    let Some(children) = node.children() else {
        return Ok(config);
    };
    for child in children.nodes() {
        match child.name().value() {
            "path" => config.path = Some(string_arg(child)?).filter(|p| !p.trim().is_empty()),
            "include-cursor" | "copy-to-clipboard" => {}
            other => bail!("unknown screenshot setting {other:?}"),
        }
    }
    config.include_cursor = parse_flag(children, "include-cursor", config.include_cursor)?;
    config.copy_to_clipboard = parse_flag(children, "copy-to-clipboard", config.copy_to_clipboard)?;
    Ok(config)
}

/// One `output "NAME" { … }` node.
fn parse_output(node: &KdlNode) -> anyhow::Result<OutputConfig> {
    let name = positional(node)
        .first()
        .and_then(|value| value.as_string())
        .context("output needs a connector name, like \"DP-1\"")?
        .to_string();
    let mut config = OutputConfig {
        name,
        ..Default::default()
    };

    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        let args = positional(child);
        match child.name().value() {
            "off" => config.off = true,
            "content-type-hint" => {
                if !args.is_empty() {
                    bail!("content-type-hint takes no value");
                }
                config.content_type_hint = true;
            }
            "position" => {
                let coords = args
                    .iter()
                    .map(|value| value.as_integer().and_then(|i| i32::try_from(i).ok()))
                    .collect::<Option<Vec<_>>>();
                match coords.as_deref() {
                    Some(&[x, y]) => config.position = Some((x, y)),
                    _ => bail!("position takes two integers: position X Y"),
                }
            }
            "mode" => {
                let text = match args.as_slice() {
                    [value] => value
                        .as_string()
                        .context("mode takes a string like \"1920x1080@60\"")?,
                    _ => bail!("mode takes one string like \"1920x1080@60\""),
                };
                config.mode = Some(parse_mode(text)?);
            }
            "adaptive-sync" => {
                config.vrr = match args.as_slice() {
                    [value] => match value.as_string() {
                        Some("off") => Vrr::Off,
                        Some("fullscreen") => Vrr::Fullscreen,
                        Some("always") => Vrr::Always,
                        _ => bail!("adaptive-sync takes \"off\", \"fullscreen\" or \"always\""),
                    },
                    _ => bail!("adaptive-sync takes one value: adaptive-sync \"fullscreen\""),
                };
            }
            "icc-profile" => {
                let path = match args.as_slice() {
                    [value] => value
                        .as_string()
                        .context("icc-profile takes a path in quotes")?,
                    _ => bail!("icc-profile takes one path: icc-profile \"/path/to/file.icc\""),
                };
                config.icc_profile = Some(expand_home(path));
            }
            other => bail!("unknown output setting {other:?}"),
        }
    }
    Ok(config)
}

/// `~/file` → `file` in the home directory.
fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

/// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@REFRESH`.
fn parse_mode(text: &str) -> anyhow::Result<ModeRequest> {
    let (size, refresh) = match text.split_once('@') {
        Some((size, refresh)) => (size, Some(refresh)),
        None => (text, None),
    };
    let (width, height) = size.split_once('x').context("expected WIDTHxHEIGHT")?;
    Ok(ModeRequest {
        width: width.trim().parse()?,
        height: height.trim().parse()?,
        refresh: refresh
            .map(|hz| hz.trim().parse::<f64>().map(|hz| hz.round() as i32))
            .transpose()?,
    })
}

/// `Mod+Mod+Key`. The last component is the trigger (a keysym or a mouse
/// button), the rest are modifiers.
fn parse_combo(combo: &str) -> anyhow::Result<(Mods, Trigger)> {
    let mut parts = combo.split('+').collect::<Vec<_>>();
    let key = parts
        .pop()
        .filter(|k| !k.is_empty())
        .context("missing key")?;

    let mut mods = Mods::default();
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "super" | "logo" | "mod4" => mods.logo = true,
            _ => bail!("unknown modifier {part:?}"),
        }
    }

    let trigger = match key.to_ascii_lowercase().as_str() {
        "mouseback" => Trigger::Button(MouseButton::Back),
        "mouseforward" => Trigger::Button(MouseButton::Forward),
        _ => {
            let sym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
            if sym.raw() == 0 {
                bail!("unknown key {key:?}");
            }
            Trigger::Key(sym)
        }
    };
    Ok((mods, trigger))
}

/// `autostart "prog" "arg"` / `sh-autostart "line"` → a spawn action.
fn parse_startup(node: &KdlNode) -> anyhow::Result<Action> {
    let args = positional(node)
        .into_iter()
        .map(|value| {
            value
                .as_string()
                .map(str::to_owned)
                .context("arguments must be strings")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    match node.name().value() {
        "autostart" if args.is_empty() => bail!("autostart needs a program"),
        "autostart" => Ok(Action::Spawn(args)),
        _ => match <[String; 1]>::try_from(args) {
            Ok([line]) => Ok(Action::SpawnShell(line)),
            Err(_) => bail!("sh-autostart takes exactly one command line"),
        },
    }
}

/// An action node, e.g. `spawn "foot" "-e" "htop"` or `workspace 3`.
fn parse_action(node: &KdlNode) -> anyhow::Result<Action> {
    // The only actions that take a number, not strings. Workspaces count from 1 in the config.
    if let name @ ("workspace" | "move-to-workspace") = node.name().value() {
        let number = match positional(node).as_slice() {
            [value] => value
                .as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 1),
            _ => None,
        }
        .with_context(|| format!("{name} takes one workspace number, like {name} 2"))?;
        return Ok(if name == "workspace" {
            Action::Workspace(Goto::To(number - 1))
        } else {
            Action::MoveToWorkspace(number - 1)
        });
    }

    let args = node
        .entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| {
            entry
                .value()
                .as_string()
                .map(str::to_owned)
                .context("arguments must be strings")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let name = node.name().value();
    let no_args = |action: Action| {
        if args.is_empty() {
            Ok(action)
        } else {
            Err(anyhow::anyhow!("{name} takes no arguments"))
        }
    };

    match name {
        "spawn" if args.is_empty() => bail!("spawn needs a program"),
        "spawn" => Ok(Action::Spawn(args)),
        "spawn-sh" => match <[String; 1]>::try_from(args) {
            Ok([command]) => Ok(Action::SpawnShell(command)),
            Err(_) => bail!("spawn-sh takes exactly one command line"),
        },
        "terminal" => no_args(Action::Terminal),
        "maximize" => no_args(Action::Maximize),
        "fullscreen" => no_args(Action::Fullscreen),
        "close" => no_args(Action::Close),
        "overview" => no_args(Action::Overview),
        "workspace-next" => no_args(Action::Workspace(Goto::Next)),
        "workspace-prev" => no_args(Action::Workspace(Goto::Prev)),
        "reload-config" => no_args(Action::ReloadConfig),
        "screenshot-area" => no_args(Action::ScreenshotArea),
        "screenshot-fullscreen" => no_args(Action::ScreenshotFullscreen),
        "screenshot-window" => no_args(Action::ScreenshotWindow),
        "toggle-shortcuts-inhibit" => no_args(Action::ToggleShortcutsInhibit),
        "quit" => no_args(Action::Quit),
        other => bail!("unknown action {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool, logo: bool) -> ModifiersState {
        ModifiersState {
            ctrl,
            alt,
            shift,
            logo,
            ..Default::default()
        }
    }

    #[test]
    fn new_windows_maximize_unless_disabled() {
        assert!(Config::default().maximize_new_windows());
        assert!(!Config::parse("maximize-new-windows false")
            .unwrap()
            .maximize_new_windows());
        assert!(Config::parse("maximize-new-windows true")
            .unwrap()
            .maximize_new_windows());
        assert!(Config::parse("maximize-new-windows 3").is_err());
    }

    #[test]
    fn csd_is_kept_unless_no_csd_is_preferred() {
        assert!(!Config::default().no_client_decorations());
        assert!(Config::parse("no-client-decorations")
            .unwrap()
            .no_client_decorations());
        assert!(Config::parse("no-client-decorations true")
            .unwrap()
            .no_client_decorations());
        assert!(!Config::parse("no-client-decorations false")
            .unwrap()
            .no_client_decorations());
        assert!(Config::parse("no-client-decorations 3").is_err());
    }

    #[test]
    fn drag_modifier_is_alt_unless_set() {
        let alt = mods(false, true, false, false);
        let logo = mods(false, false, false, true);
        assert!(Config::default().drag_modifier_held(&alt));
        assert!(!Config::default().drag_modifier_held(&logo));
        let config = Config::parse("drag-modifier \"Super\"").unwrap();
        assert!(config.drag_modifier_held(&logo));
        assert!(!config.drag_modifier_held(&alt));
        assert!(Config::parse("drag-modifier \"Hyper\"").is_err());
    }

    #[test]
    fn keyboard_defaults_match_builtin_behaviour() {
        let keyboard = Config::default().keyboard().clone();
        assert_eq!(keyboard, KeyboardConfig::default());
        assert_eq!(keyboard.repeat_delay, DEFAULT_REPEAT_DELAY);
        assert_eq!(keyboard.repeat_rate, DEFAULT_REPEAT_RATE);
        assert!(Config::parse("").unwrap().keyboard().layout.is_empty());
        assert!(keyboard.numlock);
    }

    #[test]
    fn numlock_can_be_disabled() {
        assert!(
            Config::parse("keyboard { numlock true; }")
                .unwrap()
                .keyboard()
                .numlock
        );
        assert!(
            !Config::parse("keyboard { numlock false; }")
                .unwrap()
                .keyboard()
                .numlock
        );
        assert!(
            Config::parse("keyboard { numlock; }")
                .unwrap()
                .keyboard()
                .numlock
        );
        assert!(Config::parse(r#"keyboard { numlock "maybe"; }"#).is_err());
    }

    #[test]
    fn keyboard_is_parsed() {
        let config = Config::parse(
            r#"keyboard {
                xkb { layout "us"; variant "intl"; options "caps:escape"; }
                repeat-delay 300
                repeat-rate 40
            }"#,
        )
        .unwrap();
        let keyboard = config.keyboard();
        assert_eq!(keyboard.layout, "us");
        assert_eq!(keyboard.variant, "intl");
        assert_eq!(keyboard.options.as_deref(), Some("caps:escape"));
        assert_eq!((keyboard.repeat_delay, keyboard.repeat_rate), (300, 40));
        assert!(keyboard.keymap_differs(&KeyboardConfig::default()));
    }

    #[test]
    fn rejects_bad_keyboard() {
        assert!(Config::parse("keyboard { repeat-delay -1; }").is_err());
        assert!(Config::parse("keyboard { repeat-delay 20000; }").is_err());
        assert!(Config::parse("keyboard { repeat-rate; }").is_err());
        assert!(Config::parse(r#"keyboard { repeat-rate "fast"; }"#).is_err());
        assert!(Config::parse("keyboard { speed 3; }").is_err());
        assert!(Config::parse("keyboard { xkb { colour \"x\"; } }").is_err());
        assert!(Config::parse("keyboard { xkb { layout 3; } }").is_err());
        assert!(Config::parse(r#"keyboard { xkb { layout "nosuchlayout"; } }"#).is_err());
    }

    #[test]
    fn default_config_parses() {
        let config = Config::default();
        assert_eq!(
            config.action_for(&mods(false, true, false, false), Keysym::m),
            Some(&Action::Maximize)
        );
    }

    #[test]
    fn screenshot_actions_are_parsed_from_their_names() {
        let config = Config::parse(
            r#"binds {
                Print { screenshot-area; }
                Ctrl+Print { screenshot-fullscreen; }
                Alt+Print { screenshot-window; }
            }"#,
        )
        .unwrap();
        assert_eq!(
            config.action_for(&mods(false, false, false, false), Keysym::Print),
            Some(&Action::ScreenshotArea)
        );
        assert_eq!(
            config.action_for(&mods(true, false, false, false), Keysym::Print),
            Some(&Action::ScreenshotFullscreen)
        );
        assert_eq!(
            config.action_for(&mods(false, true, false, false), Keysym::Print),
            Some(&Action::ScreenshotWindow)
        );
    }

    #[test]
    fn screenshot_actions_take_no_arguments_and_have_no_other_spelling() {
        assert!(Config::parse(r#"binds { Print { screenshot-area "x"; } }"#).is_err());
        assert!(Config::parse("binds { Print { screenshot-fullscreen 1; } }").is_err());
        assert!(Config::parse("binds { Print { screenshot_window; } }").is_err());
        assert!(Config::parse("binds { Print { screenshot; } }").is_err());
    }

    #[test]
    fn toggle_shortcuts_inhibit_is_bound_by_default_and_takes_no_arguments() {
        assert!(Config::parse("binds { Super+Escape { toggle-shortcuts-inhibit; } }").is_ok());
        assert!(Config::parse("binds { Super+Escape { toggle-shortcuts-inhibit 1; } }").is_err());
        assert!(DEFAULT_CONFIG.contains("toggle-shortcuts-inhibit"));
    }

    #[test]
    fn screenshots_are_bound_by_default() {
        let config = Config::default();
        assert_eq!(
            config.action_for(&mods(false, false, false, false), Keysym::Print),
            Some(&Action::ScreenshotArea)
        );
        assert_eq!(
            config.action_for(&mods(true, false, false, false), Keysym::Print),
            Some(&Action::ScreenshotFullscreen)
        );
        assert_eq!(
            config.action_for(&mods(false, true, false, false), Keysym::Print),
            Some(&Action::ScreenshotWindow)
        );
    }

    #[test]
    fn screenshot_block_defaults_and_settings() {
        let config = Config::default();
        assert_eq!(config.screenshot(), &ScreenshotConfig::default());
        assert!(!config.screenshot().include_cursor);
        assert!(config.screenshot().copy_to_clipboard);
        assert_eq!(config.screenshot().path, None);

        let config = Config::parse(
            r#"screenshot {
                path "~/Pictures/Shots"
                include-cursor true
                copy-to-clipboard false
            }"#,
        )
        .unwrap();
        assert_eq!(
            config.screenshot().path.as_deref(),
            Some("~/Pictures/Shots")
        );
        assert!(config.screenshot().include_cursor);
        assert!(!config.screenshot().copy_to_clipboard);

        // A bare flag means on, an empty block changes nothing.
        let config = Config::parse("screenshot { include-cursor; }").unwrap();
        assert!(config.screenshot().include_cursor);
        assert_eq!(
            Config::parse("screenshot {}").unwrap().screenshot(),
            &ScreenshotConfig::default()
        );
    }

    #[test]
    fn rejects_bad_screenshot_block() {
        assert!(Config::parse("screenshot { path 3; }").is_err());
        assert!(Config::parse("screenshot { path; }").is_err());
        assert!(Config::parse("screenshot { include-cursor 3; }").is_err());
        assert!(Config::parse("screenshot { copy-to-clipboard maybe; }").is_err());
        assert!(Config::parse("screenshot { format \"jpg\"; }").is_err());
    }

    #[test]
    fn overview_is_bound_by_default() {
        let config = Config::default();
        assert_eq!(
            config.action_for(&mods(false, true, false, false), Keysym::o),
            Some(&Action::Overview)
        );
        assert!(Config::parse("binds { Alt+O { overview 1; } }").is_err());
    }

    fn key(name: &str) -> Keysym {
        xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE)
    }

    #[test]
    fn workspaces_are_bound_by_default() {
        let config = Config::default();
        let alt = mods(false, true, false, false);
        let alt_shift = mods(false, true, true, false);
        assert_eq!(config.workspaces(), DEFAULT_WORKSPACES);
        assert_eq!(
            config.action_for(&alt, key("3")),
            Some(&Action::Workspace(Goto::To(2)))
        );
        assert_eq!(
            config.action_for(&alt_shift, key("9")),
            Some(&Action::MoveToWorkspace(8))
        );
        assert_eq!(
            config.action_for(&mods(true, true, false, false), key("Right")),
            Some(&Action::Workspace(Goto::Next))
        );
    }

    #[test]
    fn workspace_actions_and_count_are_parsed() {
        let config = Config::parse(
            "workspaces 3\nbinds { Super+1 { workspace 1; } Super+P { workspace-prev; } }",
        )
        .unwrap();
        let logo = mods(false, false, false, true);
        assert_eq!(config.workspaces(), 3);
        assert_eq!(
            config.action_for(&logo, key("1")),
            Some(&Action::Workspace(Goto::To(0)))
        );
        assert_eq!(
            config.action_for(&logo, key("p")),
            Some(&Action::Workspace(Goto::Prev))
        );
    }

    #[test]
    fn rejects_bad_workspaces() {
        assert!(Config::parse("workspaces 0").is_err());
        assert!(Config::parse("workspaces 65").is_err());
        assert!(Config::parse("workspaces").is_err());
        assert!(Config::parse(r#"workspaces "four""#).is_err());
        assert!(Config::parse("binds { Alt+1 { workspace; } }").is_err());
        assert!(Config::parse("binds { Alt+1 { workspace 0; } }").is_err());
        assert!(Config::parse(r#"binds { Alt+1 { workspace "1"; } }"#).is_err());
        assert!(Config::parse("binds { Alt+1 { move-to-workspace 1 2; } }").is_err());
        assert!(Config::parse("binds { Alt+1 { workspace-next 1; } }").is_err());
    }

    #[test]
    fn mouse_buttons_can_be_bound() {
        let config =
            Config::parse("binds { MouseBack { overview; } Alt+MouseForward { maximize; } }")
                .unwrap();
        let none = mods(false, false, false, false);
        let alt = mods(false, true, false, false);
        assert_eq!(
            config.button_action_for(&none, 0x113),
            Some(&Action::Overview)
        );
        assert_eq!(
            config.button_action_for(&none, 0x116),
            Some(&Action::Overview)
        );
        assert_eq!(config.button_action_for(&none, 0x114), None);
        assert_eq!(
            config.button_action_for(&alt, 0x115),
            Some(&Action::Maximize)
        );
        // A button bind is not a key bind, and the left button is not bindable.
        assert_eq!(config.action_for(&none, Keysym::o), None);
        assert_eq!(config.button_action_for(&none, 0x110), None);
    }

    #[test]
    fn autostart_is_parsed_in_order() {
        let config = Config::parse(
            r#"autostart "waybar"
autostart "swaybg" "-i" "/tmp/w.png"
sh-autostart "a && b""#,
        )
        .unwrap();
        assert_eq!(
            config.autostart(),
            &[
                Action::Spawn(vec!["waybar".into()]),
                Action::Spawn(vec!["swaybg".into(), "-i".into(), "/tmp/w.png".into()]),
                Action::SpawnShell("a && b".into()),
            ]
        );
        assert!(Config::default().autostart().is_empty());
    }

    #[test]
    fn bad_autostart_is_an_error() {
        assert!(Config::parse("autostart").is_err());
        assert!(Config::parse("autostart 5").is_err());
        assert!(Config::parse("sh-autostart").is_err());
        assert!(Config::parse("sh-autostart \"a\" \"b\"").is_err());
    }

    #[test]
    fn spawn_keeps_arguments() {
        let config = Config::parse(r#"binds { Super+T { spawn "foot" "-e" "htop"; } }"#).unwrap();
        assert_eq!(
            config.action_for(&mods(false, false, false, true), Keysym::t),
            Some(&Action::Spawn(vec![
                "foot".into(),
                "-e".into(),
                "htop".into()
            ]))
        );
    }

    #[test]
    fn modifiers_match_exactly() {
        let config = Config::parse("binds { Alt+M { maximize; } }").unwrap();
        assert!(config
            .action_for(&mods(false, true, true, false), Keysym::m)
            .is_none());
    }

    #[test]
    fn outputs_are_parsed() {
        let config = Config::parse(
            r#"outputs {
                output "DP-1" { position 1920 0; mode "2560x1440@143.97"; }
                output "eDP-1" { off; }
            }"#,
        )
        .unwrap();
        let dp = config.output("DP-1").unwrap();
        assert_eq!(dp.position, Some((1920, 0)));
        assert_eq!(
            dp.mode,
            Some(ModeRequest {
                width: 2560,
                height: 1440,
                refresh: Some(144)
            })
        );
        assert!(config.output("eDP-1").unwrap().off);
        assert!(config.output("HDMI-A-1").is_none());
    }

    #[test]
    fn update_output_edits_or_adds() {
        let mut config = Config::parse(r#"outputs { output "DP-1" { position 5 5; } }"#).unwrap();
        config.update_output("DP-1", |c| c.off = true);
        config.update_output("HDMI-A-1", |c| c.position = Some((1, 2)));
        assert!(config.output("DP-1").unwrap().off);
        assert_eq!(config.output("DP-1").unwrap().position, Some((5, 5)));
        assert_eq!(config.output("HDMI-A-1").unwrap().position, Some((1, 2)));
    }

    #[test]
    fn rejects_bad_outputs() {
        assert!(Config::parse(r#"outputs { monitor "DP-1" { off; } }"#).is_err());
        assert!(Config::parse(r#"outputs { output "DP-1" { position 1; } }"#).is_err());
        let config =
            Config::parse(r#"outputs { output "DP-1" { icc-profile "/tmp/a.icc"; } }"#).unwrap();
        assert_eq!(
            config.output("DP-1").unwrap().icc_profile.as_deref(),
            Some(std::path::Path::new("/tmp/a.icc"))
        );
        assert!(Config::parse(r#"outputs { output "DP-1" { icc-profile; } }"#).is_err());
        let config =
            Config::parse(r#"outputs { output "DP-1" { adaptive-sync "always"; } }"#).unwrap();
        assert_eq!(config.output("DP-1").unwrap().vrr, Vrr::Always);
        let config = Config::parse(r#"outputs { output "DP-1" { content-type-hint; } }"#).unwrap();
        assert!(config.output("DP-1").unwrap().content_type_hint);
        assert!(!config.output("DP-2").map_or(false, |c| c.content_type_hint));
        assert!(Config::parse(r#"outputs { output "DP-1" { content-type-hint true; } }"#).is_err());
        assert!(Config::parse(r#"outputs { output "DP-1" { adaptive-sync "maybe"; } }"#).is_err());
        assert!(Config::parse(r#"outputs { output "DP-1" { mode "big"; } }"#).is_err());
        assert!(Config::parse(r#"outputs { output { off; } }"#).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Config::parse("binds { Hyper+M { maximize; } }").is_err());
        assert!(Config::parse("binds { Alt+NoSuchKey { maximize; } }").is_err());
        assert!(Config::parse("binds { Alt+M { fly; } }").is_err());
        assert!(Config::parse("binds { Alt+M { spawn; } }").is_err());
        assert!(Config::parse("binds { Alt+M { maximize 1; } }").is_err());
    }
}

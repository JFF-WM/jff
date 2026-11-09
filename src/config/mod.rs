//! User configuration, read from a KDL file.
//!
//! Looked up at `$JFF_CONFIG`, then `$XDG_CONFIG_HOME/jff/config.kdl`, then
//! `~/.config/jff/config.kdl`. When none exists, the built-in [`DEFAULT_CONFIG`] is used.
//!
//! The format is one KDL document. Top-level nodes: `workspaces` (`workspaces 9`, or `workspaces { count 9, per-output }` for a workspace of its own on every monitor), `maximize-new-windows`,
//! `global-hotkeys` (xx-hotkey-v1: clients bind their own hotkeys, off by default), `window-animations { … }` (`open \"fade\"|\"zoom\"|\"slide\"|\"pop\"|\"none\"`, `open-ms`, `close-ms`), `no-client-decorations`, `drag-modifier`, `gaps`, `radius`, `popup-radius`, `border { … }`, `screenshot { … }`,
//! `keyboard { … }`, `blur { … }` (`passes`, `offset`, `all-windows`, `all-layers`, `behind-windows`, `ignore-layer \"NS\"`, `ignore-window \"APP-ID\"`: no blur for that app, `only-window \"APP-ID\"`: blur only those apps, whole window), `osd { … }`, `switcher { … }`, `overview { … }` (`selection-color "#RRGGBBAA"`, `selection-width N`: the outline of the selected thumbnail in the overview and the switcher, `selection-width 0` for none), `workspace-wall` (`workspace-wall`, or a block with `height N`: dragging a thumbnail in the overview shows a square per workspace to drop it on), `hot-corner { … }` (`size`, `dwell-ms`, `monitor \"NAME\"`, `hot-corner 0` or no block: off, while on, the pointer can't change monitor through the corner's `size` px of edge), `autostart` / `sh-autostart` lines, `outputs { output "NAME" { … } }`,
//! `window-rule { … }` (any number, `match app-id="…"`, `title="…"` (contains), `tag="…"` (xdg-toplevel-tag), `dialog=#true`, `restored=#true`, all of which have to hold, then `open-on-output "NAME"|"pointer"`, `open-workspace N`, `open-maximized`, `open-size W H`, `floating`, the first rule that fits a window applies), and
//! `binds { "Combo" { action, } }` (`forward-key "APP-ID"` hands the key to that app's window without focusing it, for its own global hotkeys). Unknown top-level nodes are silently ignored, an
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
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context};
use kdl::{KdlDocument, KdlNode, KdlValue};
use smithay::input::keyboard::{xkb, Keysym, ModifiersState, XkbConfig};

use crate::desk::{Goto, Side};

/// Default number of workspaces.
pub const DEFAULT_WORKSPACES: usize = 9;
/// Upper bound for `workspaces`.
pub const MAX_WORKSPACES: usize = 64;

/// Default gap (px) around maximized windows: none.
pub const DEFAULT_GAPS: i32 = 0;
/// Upper bound for `gaps`.
pub const MAX_GAPS: i32 = 512;

/// Default corner radius (px) of windows: square corners.
pub const DEFAULT_RADIUS: i32 = 0;
/// Upper bound for `radius`.
pub const MAX_RADIUS: i32 = 256;

/// Border around windows: width used when a `border { }` block gives none, its upper bound,
/// and the focus fade.
pub const DEFAULT_BORDER_WIDTH: i32 = 2;
pub const MAX_BORDER_WIDTH: i32 = 64;
pub const DEFAULT_BORDER_FADE_MS: i32 = 150;
pub const MAX_BORDER_FADE_MS: i32 = 5000;
/// Time one turn of an animated border takes, and its upper bound (10 minutes).
pub const DEFAULT_BORDER_PERIOD_MS: i32 = 3000;
pub const MAX_BORDER_PERIOD_MS: i32 = 600_000;

/// Upper bound for the window fade times (ms).
pub const MAX_WINDOW_ANIM_MS: i32 = 10_000;

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

/// All the top-level settings jff knows. Anything else in the file is silently ignored, and
/// `jff check-config` warns about it.
pub const TOP_LEVEL_NODES: &[&str] = &[
    "workspaces",
    "maximize-new-windows",
    "allow-tearing",
    "global-hotkeys",
    "window-animations",
    "no-client-decorations",
    "drag-modifier",
    "gaps",
    "radius",
    "popup-radius",
    "border",
    "screenshot",
    "keyboard",
    "blur",
    "osd",
    "switcher",
    "overview",
    "workspace-wall",
    "hot-corner",
    "autostart",
    "sh-autostart",
    "outputs",
    "window-rule",
    "color-representation",
    "binds",
];

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
    /// Fill one half of the monitor with the focused window.
    Snap(Side),
    /// Ask the focused window to close.
    Close,
    /// Show every window of the current monitor as a thumbnail.
    Overview,
    /// Window switcher: a row of thumbnails, most recently used first. Pressing the key again
    /// moves on, letting go of the modifier picks the window.
    SwitchWindow { backwards: bool },
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
    /// Hand the key (press and release) to the window with this app id, wherever it is and
    /// whatever has the keyboard, so an app's own global hotkey works (`forward-key "com.obsproject.Studio"`).
    ForwardKey(String),
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

    /// The keymap these names build, `None` when xkbcommon rejects them.
    pub fn keymap(&self) -> Option<xkb::Keymap> {
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

/// The `blur { … }` block: background blur for surfaces that ask for it (see
/// `protocol/background_effect.rs`).
///
/// Blur is opt-in: with no `blur` block it is `off`. Writing the block turns it on, and a
/// child `off` turns it back off without deleting the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blur {
    pub off: bool,
    /// Down/upsample rounds, 1 to 31. Each one doubles the blur radius.
    pub passes: u8,
    /// Stretch of the sample pattern, above 0. Bigger is blurrier, and blockier.
    pub offset: f64,
    /// Also blur behind every window that sent no blur region of its own, over the whole
    /// window. A window that sent an empty region still gets none.
    pub all_windows: bool,
    /// Also blur behind panels and launchers (layer surfaces on the `top` and `overlay`
    /// layers) that sent no blur region, over their whole area. Wallpapers (`background`)
    /// and `bottom` widgets are left alone.
    pub all_layers: bool,
    /// What window blur samples. By default only the wallpaper (the `background` and `bottom`
    /// layers): windows behind a blurred window stay sharp. With this set the blur also
    /// takes in the windows behind, so they are blurred too.
    pub behind_windows: bool,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            off: true,
            passes: 3,
            offset: 3.0,
            all_windows: false,
            all_layers: false,
            behind_windows: false,
        }
    }
}

/// Which windows get blur, by app id: `ignore-window` entries never do, `only-window` entries are the only ones that do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlurWindows {
    pub ignore: Vec<String>,
    pub only: Vec<String>,
}

impl BlurWindows {
    /// Whether a window with this app id may be blurred: not ignored, and on the `only` list when there is one.
    pub fn allows(&self, app_id: &str) -> bool {
        !self.ignore.iter().any(|id| id == app_id)
            && (self.only.is_empty() || self.only.iter().any(|id| id == app_id))
    }

    /// `blur` for this app id: `None` when filtered out, and whole-window for an `only-window` app, which needs no blur region.
    pub fn apply(&self, blur: Option<Blur>, app_id: &str) -> Option<Blur> {
        let blur = blur.filter(|_| self.allows(app_id))?;
        let listed = self.only.iter().any(|id| id == app_id);
        Some(Blur {
            all_windows: blur.all_windows || listed,
            ..blur
        })
    }
}

/// The `osd { … }` block, the toasts shown for the system bell. On unless `off`. `font` is only
/// read at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsdConfig {
    pub off: bool,
    pub timeout_ms: u32,
    pub font: String,
}

impl Default for OsdConfig {
    fn default() -> Self {
        Self {
            off: false,
            timeout_ms: 1800,
            font: "sans-serif".to_owned(),
        }
    }
}

/// Default time (ms) the window switcher's row takes to slide to the selected window.
pub const DEFAULT_SWITCHER_SLIDE_MS: u32 = 120;
/// Bounds for the switcher's `thumbnail-height`, in pixels.
pub const MIN_THUMBNAIL_HEIGHT: i32 = 32;
pub const MAX_THUMBNAIL_HEIGHT: i32 = 4096;
/// Upper bound for the switcher's `slide-ms`.
pub const MAX_SWITCHER_SLIDE_MS: i32 = 2000;

/// The `switcher { … }` block, the Alt+Tab window switcher. `thumbnail-height` is the height in
/// pixels of every thumbnail (a window smaller than that keeps its real size), without it, a
/// third of the monitor's height. `slide-ms` is how long the row takes to slide to the selected
/// window, 0 to jump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitcherConfig {
    pub thumbnail_height: Option<i32>,
    pub slide_ms: u32,
}

impl Default for SwitcherConfig {
    fn default() -> Self {
        Self {
            thumbnail_height: None,
            slide_ms: DEFAULT_SWITCHER_SLIDE_MS,
        }
    }
}

/// Default width (px) and colour of the outline around the selected thumbnail of the overview.
/// The colour is straight (not premultiplied) alpha: it is translucent.
pub const DEFAULT_SELECTION_WIDTH: i32 = 4;
pub const DEFAULT_SELECTION_COLOR: [f32; 4] = [0.35, 0.55, 0.95, 0.6];

/// The `overview { … }` block: how the overview and the switcher mark the selected thumbnail.
/// `selection-width` is the outline's width in pixels (0 draws none), `selection-color` its
/// colour (`#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`). The square of the workspace wall the
/// pointer is over uses the same outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverviewConfig {
    pub selection_width: i32,
    pub selection_color: [f32; 4],
}

impl Default for OverviewConfig {
    fn default() -> Self {
        Self {
            selection_width: DEFAULT_SELECTION_WIDTH,
            selection_color: DEFAULT_SELECTION_COLOR,
        }
    }
}

/// Default height (px) of a square of the workspace wall, and its bounds.
pub const DEFAULT_WALL_HEIGHT: i32 = 96;
pub const MIN_WALL_HEIGHT: i32 = 32;
pub const MAX_WALL_HEIGHT: i32 = 400;

/// The `workspace-wall` setting: while a thumbnail is dragged in the overview, a row of squares,
/// one per workspace, appears, dropping the thumbnail on one sends the window to that workspace.
/// `height` is the height in pixels of a square (it has the shape of the monitor). Off unless
/// the setting is in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceWallConfig {
    pub height: i32,
}

/// Default size (px) of a hot corner whose block gives none, and its upper bound.
pub const DEFAULT_HOT_CORNER_SIZE: i32 = 4;
pub const MAX_HOT_CORNER_SIZE: i32 = 64;
/// Default time (ms) the pointer must rest in a hot corner, and its upper bound.
pub const DEFAULT_HOT_CORNER_DWELL_MS: i32 = 250;
pub const MAX_HOT_CORNER_DWELL_MS: i32 = 5000;

/// The `hot-corner { … }` block: resting the pointer in a `size` px square at the top-left of a
/// monitor for `dwell` opens the overview. `monitors` are the output names it is limited to,
/// empty for every monitor. Only built for a `size` above 0, see [`Config::hot_corner`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotCorner {
    pub size: i32,
    pub dwell: Duration,
    pub monitors: Vec<String>,
}

/// How a new window appears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenStyle {
    #[default]
    Fade,
    /// Grows from a bit smaller while fading in.
    Zoom,
    /// Rises into place while fading in.
    Slide,
    /// Grows from small and overshoots a little.
    Pop,
    /// Appears at once.
    None,
}

/// `window-animations`: how windows open and close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowAnimations {
    pub on: bool,
    pub open: OpenStyle,
    pub open_ms: u32,
    pub close_ms: u32,
}

impl Default for WindowAnimations {
    fn default() -> Self {
        Self {
            on: true,
            open: OpenStyle::Fade,
            open_ms: 100,
            close_ms: 80,
        }
    }
}

/// How the border of the focused window moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderAnimation {
    /// Standing still.
    #[default]
    None,
    /// A two-colour gradient that turns around the window.
    Spin,
    /// A short highlight of the second colour that travels around the window.
    Sweep,
}

/// The `border { … }` block: an outline drawn just outside every window that isn't fullscreen.
///
/// Opt-in: with no block `width` is 0 and nothing is drawn. Colours are straight (not
/// premultiplied) RGBA, 0.0 to 1.0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Border {
    /// Width in pixels, 0 = no border.
    pub width: i32,
    /// Colour of a window that doesn't have the keyboard.
    pub color: [f32; 4],
    /// Colour of the window that has it.
    pub focus_color: [f32; 4],
    /// How long the colour takes to change when focus moves, in milliseconds (0 = at once).
    pub fade_ms: i32,
    /// Motion of the focused window's border.
    pub animate: BorderAnimation,
    /// Time of one turn of the animation, in milliseconds (0 = standing still).
    pub period_ms: i32,
    /// Second colour of the animation, `None` = the same as `focus_color` (nothing to see).
    pub focus_color_2: Option<[f32; 4]>,
}

impl Border {
    pub fn on(&self) -> bool {
        self.width > 0
    }

    /// Second colour of the animation.
    pub fn focus_color_2(&self) -> [f32; 4] {
        self.focus_color_2.unwrap_or(self.focus_color)
    }

    /// Whether the focused window's border moves at all. It doesn't with `animate none`, a
    /// period of 0, or a second colour equal to the first (the motion would be invisible), so
    /// no frames are spent on it then.
    pub fn animated(&self) -> bool {
        self.on()
            && self.animate != BorderAnimation::None
            && self.period_ms > 0
            && self.focus_color_2() != self.focus_color
    }
}

impl Default for Border {
    fn default() -> Self {
        Self {
            width: 0,
            color: [0.35, 0.35, 0.40, 1.0],
            focus_color: [0.35, 0.55, 0.95, 1.0],
            fade_ms: DEFAULT_BORDER_FADE_MS,
            animate: BorderAnimation::None,
            period_ms: DEFAULT_BORDER_PERIOD_MS,
            focus_color_2: None,
        }
    }
}

/// How often the config file is looked at for changes.
pub const CONFIG_WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Modification time and size of the config file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

/// Decides when the config file has been saved and should be read again.
///
/// Fed what the file looks like (`None` = missing) at every poll. A change is not acted on
/// at once: editors write a file in several steps, or replace it by renaming another over it,
/// and reading in the middle would see half a file. [`ConfigWatch::poll`] reports it once the
/// file looked the same on two polls in a row. A file that is saved again while that wait
/// goes on restarts the wait.
#[derive(Debug)]
pub struct ConfigWatch {
    /// What the config in use was read from.
    loaded: Option<FileStamp>,
    /// A change seen on the last poll, not yet acted on.
    pending: Option<Option<FileStamp>>,
}

impl ConfigWatch {
    /// `loaded` is the file as the running config was read from it.
    pub fn new(loaded: Option<FileStamp>) -> Self {
        Self {
            loaded,
            pending: None,
        }
    }

    /// `true` when the file has changed and then held still: read the config again now.
    pub fn poll(&mut self, now: Option<FileStamp>) -> bool {
        if now == self.loaded {
            // Back to what was loaded (or never left it).
            self.pending = None;
            return false;
        }
        if self.pending == Some(now) {
            self.loaded = now;
            self.pending = None;
            return true;
        }
        self.pending = Some(now);
        false
    }
}

/// Where a window rule opens a window: on the monitor with this name, or on the monitor the
/// pointer is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleOutput {
    Pointer,
    Named(String),
}

/// One `match` condition of a `window-rule`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowMatch {
    /// The app id is exactly this.
    AppId(String),
    /// The title contains this text.
    Title(String),
    /// The tag the app set with xdg-toplevel-tag is exactly this.
    Tag(String),
    /// The window is a dialog (it has a parent, or said so with xdg-dialog) or is not.
    Dialog(bool),
    /// The app's session put the window back (xdg-session-management) or did not.
    Restored(bool),
}

/// What a window rule is asked about a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowFacts<'a> {
    pub app_id: &'a str,
    pub title: &'a str,
    pub tag: Option<&'a str>,
    pub dialog: bool,
    pub restored: bool,
}

/// A `window-rule { match …, action … }` block. A window gets the first rule whose `match`
/// lines all hold. A rule without `match` lines holds for every window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowRule {
    pub matches: Vec<WindowMatch>,
    /// `open-on-output`.
    pub output: Option<RuleOutput>,
    /// `open-workspace`, 0-based here (1-based in the file).
    pub workspace: Option<usize>,
    /// `open-maximized`: `Some(false)` keeps the window floating even though new windows are
    /// maximized.
    pub maximized: Option<bool>,
    /// `open-size`: width and height.
    pub size: Option<(i32, i32)>,
    /// `floating #true`: the window stays floating, whatever `maximize-new-windows` says.
    pub floating: bool,
}

impl WindowRule {
    /// Whether every `match` line holds for `window`.
    pub fn matches(&self, window: &WindowFacts<'_>) -> bool {
        self.matches.iter().all(|condition| match condition {
            WindowMatch::AppId(id) => window.app_id == id.as_str(),
            WindowMatch::Title(text) => window.title.contains(text.as_str()),
            WindowMatch::Tag(tag) => window.tag == Some(tag.as_str()),
            WindowMatch::Dialog(want) => window.dialog == *want,
            WindowMatch::Restored(want) => window.restored == *want,
        })
    }

    /// Whether the rule says how the window's mode or size starts: then the window is not
    /// maximized like a new window is by default.
    pub fn places_window(&self) -> bool {
        self.floating || self.maximized.is_some() || self.size.is_some()
    }

    /// Whether the rule has any action at all.
    pub fn has_actions(&self) -> bool {
        self.output.is_some() || self.workspace.is_some() || self.places_window()
    }
}

/// The largest side `open-size` takes.
pub const MAX_RULE_SIZE: i32 = 16384;

/// The parsed configuration.
#[derive(Clone, Debug)]
pub struct Config {
    keyboard: KeyboardConfig,
    binds: Vec<Binding>,
    outputs: Vec<OutputConfig>,
    workspaces: usize,
    /// Every monitor has its own shown workspace, `workspaces { per-output }`.
    workspaces_per_output: bool,
    maximize_new_windows: bool,
    /// `allow-tearing`: a fullscreen window that asked for it (wp_tearing_control_v1) is shown
    /// with async page flips. Off by default.
    allow_tearing: bool,
    /// `global-hotkeys`: clients may bind their own global hotkeys (xx-hotkey-v1). Off by default.
    global_hotkeys: bool,
    window_animations: WindowAnimations,
    no_client_decorations: bool,
    drag_modifier: DragModifier,
    /// Pixels kept free on each side of a maximized window.
    gaps: i32,
    /// Corner radius in pixels of windows that aren't fullscreen. 0 = square.
    radius: i32,
    /// Corner radius of popups (menus, popovers) for the blur behind them. `None`: same as
    /// `radius`.
    popup_radius: Option<i32>,
    border: Border,
    screenshot: ScreenshotConfig,
    blur: Blur,
    /// Layer-surface namespaces the blur leaves alone (`blur { ignore-layer "…" }`).
    blur_ignore_layers: Vec<String>,
    /// Which windows get blur (`blur { ignore-window "…", only-window "…" }`).
    blur_windows: BlurWindows,
    osd: OsdConfig,
    switcher: SwitcherConfig,
    overview: OverviewConfig,
    workspace_wall: Option<WorkspaceWallConfig>,
    hot_corner: Option<HotCorner>,
    autostart: Vec<Action>,
    /// `window-rule` blocks, in file order.
    window_rules: Vec<WindowRule>,
    /// `color-representation { yuv }`: draw YUV buffers with the matrix and range the client
    /// declared (wp-color-representation).
    color_representation_yuv: bool,
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
                workspaces_per_output: false,
                maximize_new_windows: true,
                allow_tearing: false,
                global_hotkeys: false,
                window_animations: WindowAnimations::default(),
                no_client_decorations: false,
                drag_modifier: DragModifier::Alt,
                gaps: DEFAULT_GAPS,
                radius: DEFAULT_RADIUS,
                popup_radius: None,
                border: Border::default(),
                screenshot: ScreenshotConfig::default(),
                blur: Blur::default(),
                blur_ignore_layers: Vec::new(),
                blur_windows: BlurWindows::default(),
                osd: OsdConfig::default(),
                switcher: SwitcherConfig::default(),
                overview: OverviewConfig::default(),
                workspace_wall: None,
                hot_corner: None,
                autostart: Vec::new(),
                window_rules: Vec::new(),
                color_representation_yuv: false,
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

    /// What the config file looks like on disk right now, for [`ConfigWatch`]. `None` when
    /// there is no file (or no path). Follows symlinks, so a config kept elsewhere and linked
    /// in is noticed when the real file changes.
    pub fn stamp() -> Option<FileStamp> {
        let meta = std::fs::metadata(Self::path()?).ok()?;
        Some(FileStamp {
            modified: meta.modified().ok(),
            len: meta.len(),
        })
    }

    /// What `jff check-config` runs. Fails with the same errors as `parse`, and on top of that
    /// returns warnings for things that load fine but probably aren't what you meant.
    pub fn check_text(text: &str) -> anyhow::Result<Vec<String>> {
        let config = Self::parse(text)?;
        let doc: KdlDocument = text.parse()?;
        let mut warnings = Vec::new();
        for node in doc.nodes() {
            let name = node.name().value();
            if !TOP_LEVEL_NODES.contains(&name) {
                let hint = closest(name, TOP_LEVEL_NODES)
                    .map(|near| format!(", did you mean {near:?}?"))
                    .unwrap_or_default();
                warnings.push(format!("unknown setting {name:?} is ignored{hint}"));
            }
        }
        let border = config.border();
        if border.animate != BorderAnimation::None && !border.animated() {
            let why = if !border.on() {
                "the border has width 0"
            } else if border.period_ms == 0 {
                "period-ms is 0"
            } else {
                "focus-color-2 is missing or the same as focus-color"
            };
            warnings.push(format!(
                "border animate is set but nothing will move: {why}"
            ));
        }
        for (i, rule) in config.window_rules().iter().enumerate() {
            let n = i + 1;
            if !rule.has_actions() {
                warnings.push(format!("window-rule {n} has no actions, it does nothing"));
            }
            // A restored window keeps the monitor, size and mode its session saved, only
            // `open-workspace` applies to it.
            let restored = rule.matches.contains(&WindowMatch::Restored(true));
            if restored && (rule.output.is_some() || rule.places_window()) {
                warnings.push(format!(
                    "window-rule {n} matches restored windows, which keep their saved monitor, \
                     size and mode: only open-workspace applies"
                ));
            }
        }
        Ok(warnings)
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

        let (workspaces, workspaces_per_output) = match doc.get("workspaces") {
            None => (DEFAULT_WORKSPACES, false),
            Some(node) => parse_workspaces(node)?,
        };

        let maximize_new_windows = parse_flag(&doc, "maximize-new-windows", true)?;
        let allow_tearing = parse_flag(&doc, "allow-tearing", false)?;
        let global_hotkeys = parse_flag(&doc, "global-hotkeys", false)?;
        let window_animations = match doc.get("window-animations") {
            None => WindowAnimations::default(),
            Some(node) => parse_window_animations(node).context("in window-animations")?,
        };
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

        let gaps = match doc.get("gaps") {
            None => DEFAULT_GAPS,
            Some(node) => int_arg(node, MAX_GAPS)?,
        };
        let radius = match doc.get("radius") {
            None => DEFAULT_RADIUS,
            Some(node) => int_arg(node, MAX_RADIUS)?,
        };
        let popup_radius = match doc.get("popup-radius") {
            None => None,
            Some(node) => Some(int_arg(node, MAX_RADIUS)?),
        };

        let border = match doc.get("border") {
            None => Border::default(),
            Some(node) => parse_border(node).context("in border")?,
        };

        let keyboard = match doc.get("keyboard") {
            None => KeyboardConfig::default(),
            Some(node) => parse_keyboard(node).context("in keyboard")?,
        };

        let screenshot = match doc.get("screenshot") {
            None => ScreenshotConfig::default(),
            Some(node) => parse_screenshot(node).context("in screenshot")?,
        };

        let (blur, blur_ignore_layers, blur_windows) = match doc.get("blur") {
            None => (Blur::default(), Vec::new(), BlurWindows::default()),
            Some(node) => parse_blur(node).context("in blur")?,
        };

        let osd = match doc.get("osd") {
            None => OsdConfig::default(),
            Some(node) => parse_osd(node).context("in osd")?,
        };

        let workspace_wall = match doc.get("workspace-wall") {
            None => None,
            Some(node) => parse_workspace_wall(node).context("in workspace-wall")?,
        };

        let switcher = match doc.get("switcher") {
            None => SwitcherConfig::default(),
            Some(node) => parse_switcher(node).context("in switcher")?,
        };

        let overview = match doc.get("overview") {
            None => OverviewConfig::default(),
            Some(node) => parse_overview(node).context("in overview")?,
        };

        let hot_corner = match doc.get("hot-corner") {
            None => None,
            Some(node) => parse_hot_corner(node).context("in hot-corner")?,
        };

        let mut autostart = Vec::new();
        for node in doc.nodes() {
            let name = node.name().value();
            if matches!(name, "autostart" | "sh-autostart") {
                autostart.push(parse_startup(node).with_context(|| format!("in {name}"))?);
            }
        }

        let mut color_representation_yuv = false;
        if let Some(node) = doc.get("color-representation") {
            for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
                match child.name().value() {
                    "yuv" => color_representation_yuv = flag_value(child)?,
                    other => bail!("unknown color-representation setting {other:?}"),
                }
            }
        }
        let mut window_rules = Vec::new();
        for node in doc.nodes() {
            if node.name().value() == "window-rule" {
                let n = window_rules.len() + 1;
                window_rules
                    .push(parse_window_rule(node).with_context(|| format!("in window-rule {n}"))?);
            }
        }

        Ok(Self {
            keyboard,
            binds,
            outputs,
            workspaces,
            workspaces_per_output,
            maximize_new_windows,
            allow_tearing,
            global_hotkeys,
            window_animations,
            no_client_decorations,
            drag_modifier,
            gaps,
            radius,
            popup_radius,
            border,
            screenshot,
            blur,
            blur_ignore_layers,
            blur_windows,
            osd,
            switcher,
            overview,
            workspace_wall,
            hot_corner,
            autostart,
            window_rules,
            color_representation_yuv,
        })
    }

    /// Corner radius in pixels of windows that aren't fullscreen (0 by default: square).
    pub fn radius(&self) -> i32 {
        self.radius
    }

    /// The border block (`width` 0 when there is none).
    pub fn border(&self) -> Border {
        self.border
    }

    /// Pixels kept free on each side of a maximized window: `gaps`, but at least the border
    /// width, which is drawn outside the window and would otherwise be cut off by the screen.
    pub fn maximize_inset(&self) -> i32 {
        self.gaps.max(self.border.width)
    }

    /// Pixels between two snapped windows: `gaps`, but at least twice the border width, since
    /// each window's border is drawn outside it and the two rings must not touch.
    pub fn snap_gap(&self) -> i32 {
        self.gaps.max(2 * self.border.width)
    }

    /// Corner radius in pixels the blur behind popups (menus, popovers) is shaped with: the
    /// `popup-radius` setting, `radius` when it isn't set. GTK popovers are rounded by the
    /// client, which the compositor can't see, so this says how much.
    pub fn popup_radius(&self) -> i32 {
        self.popup_radius.unwrap_or(self.radius)
    }

    /// Pixels kept free on each side of a maximized window (0 by default). Fullscreen windows
    /// ignore it.
    pub fn gaps(&self) -> i32 {
        self.gaps
    }

    /// `autostart` + `sh-autostart`, in file order. Only [`Action::Spawn`] and
    /// [`Action::SpawnShell`] appear here.
    pub fn autostart(&self) -> &[Action] {
        &self.autostart
    }

    /// The blur block.
    pub fn blur(&self) -> Blur {
        self.blur
    }

    /// Namespaces of layer surfaces (see `layers` in `jff msg`) that never get blur, neither
    /// their own nor behind their menus, whatever they ask for and whatever `all-layers` says.
    pub fn blur_ignore_layers(&self) -> &[String] {
        &self.blur_ignore_layers
    }

    /// The window filter of the blur block.
    pub fn blur_windows(&self) -> &BlurWindows {
        &self.blur_windows
    }

    /// `blur` for the window with this app id: `None` when the window's blur (and its popups') is filtered out.
    pub fn blur_for_window(&self, blur: Option<Blur>, app_id: &str) -> Option<Blur> {
        self.blur_windows.apply(blur, app_id)
    }

    pub fn osd(&self) -> &OsdConfig {
        &self.osd
    }

    pub fn switcher(&self) -> &SwitcherConfig {
        &self.switcher
    }

    pub fn overview(&self) -> &OverviewConfig {
        &self.overview
    }

    /// The workspace wall of the overview's drag, `None` when it is off (the default).
    pub fn workspace_wall(&self) -> Option<WorkspaceWallConfig> {
        self.workspace_wall
    }

    /// The hot corner, `None` when it is off (no block, `hot-corner 0`, or `size 0`).
    pub fn hot_corner(&self) -> Option<HotCorner> {
        self.hot_corner.clone()
    }

    /// Whether `color-representation { yuv }` is on.
    pub fn color_representation_yuv(&self) -> bool {
        self.color_representation_yuv
    }

    /// The `window-rule` blocks, in file order.
    pub fn window_rules(&self) -> &[WindowRule] {
        &self.window_rules
    }

    /// The rule a window gets: the first one whose `match` lines all hold.
    pub fn window_rule_for(&self, window: &WindowFacts<'_>) -> Option<&WindowRule> {
        self.window_rules.iter().find(|rule| rule.matches(window))
    }

    /// The screenshot block.
    pub fn screenshot(&self) -> &ScreenshotConfig {
        &self.screenshot
    }

    /// Keyboard layout and key repeat.
    pub fn keyboard(&self) -> &KeyboardConfig {
        &self.keyboard
    }

    /// Whether a fullscreen window may be shown with tearing when it asks for it.
    pub fn allow_tearing(&self) -> bool {
        self.allow_tearing
    }

    /// Whether clients may bind their own global hotkeys (xx-hotkey-v1).
    pub fn global_hotkeys(&self) -> bool {
        self.global_hotkeys
    }

    /// Whether new windows start maximized.
    pub fn maximize_new_windows(&self) -> bool {
        self.maximize_new_windows
    }

    /// Whether windows fade in and out (`window-animations`, on by default).
    pub fn window_animations(&self) -> bool {
        self.window_animations.on
    }

    /// How new windows appear.
    pub fn window_open_style(&self) -> OpenStyle {
        self.window_animations.open
    }

    /// How long the open and close fades take.
    pub fn window_animation_times(&self) -> (Duration, Duration) {
        let a = self.window_animations;
        (
            Duration::from_millis(a.open_ms.into()),
            Duration::from_millis(a.close_ms.into()),
        )
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

    /// Whether every monitor shows its own workspace (`workspaces { per-output }`), instead of
    /// all of them showing the same one. Off by default.
    pub fn workspaces_per_output(&self) -> bool {
        self.workspaces_per_output
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

/// How many single-letter changes it takes to turn `a` into `b`.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The known name that `name` looks like a typo of, if there is one.
pub fn closest<'a>(name: &str, options: &[&'a str]) -> Option<&'a str> {
    options
        .iter()
        .map(|option| (edit_distance(name, option), *option))
        .filter(|(distance, _)| *distance <= 2 && *distance < name.chars().count())
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, option)| option)
}

/// `period-ms`: a number of milliseconds, or `"none"` for 0 (standing still).
fn period_arg(node: &KdlNode) -> anyhow::Result<i32> {
    if let [value] = positional(node).as_slice() {
        if value.as_string() == Some("none") {
            return Ok(0);
        }
    }
    int_arg(node, MAX_BORDER_PERIOD_MS)
}

/// A colour written as `"#rgb"`, `"#rgba"`, `"#rrggbb"` or `"#rrggbbaa"`, as straight RGBA.
pub fn parse_color(text: &str) -> anyhow::Result<[f32; 4]> {
    let hex = text
        .strip_prefix('#')
        .with_context(|| format!("colour {text:?} must start with #"))?;
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<_>>()
        .with_context(|| format!("colour {text:?} has a character that isn't a hex digit"))?;
    let [r, g, b, a] = match digits.as_slice() {
        [r, g, b] => [r * 17, g * 17, b * 17, 255],
        [r, g, b, a] => [r * 17, g * 17, b * 17, a * 17],
        [r1, r2, g1, g2, b1, b2] => [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2, 255],
        [r1, r2, g1, g2, b1, b2, a1, a2] => {
            [r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2, a1 * 16 + a2]
        }
        _ => bail!("colour {text:?} must have 3, 4, 6 or 8 hex digits"),
    };
    Ok([r, g, b, a].map(|v| f32::from(v) / 255.0))
}

/// The `border { … }` block. Having the block at all turns borders on (2 px unless `width`
/// says otherwise, `width 0` turns them off again).
fn parse_border(node: &KdlNode) -> anyhow::Result<Border> {
    let mut config = Border {
        width: DEFAULT_BORDER_WIDTH,
        ..Border::default()
    };
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "width" => config.width = int_arg(child, MAX_BORDER_WIDTH)?,
            "color" => config.color = parse_color(&string_arg(child)?)?,
            "focus-color" => config.focus_color = parse_color(&string_arg(child)?)?,
            "fade-ms" => config.fade_ms = int_arg(child, MAX_BORDER_FADE_MS)?,
            "focus-color-2" => config.focus_color_2 = Some(parse_color(&string_arg(child)?)?),
            "animate" => {
                config.animate = match string_arg(child)?.as_str() {
                    "none" => BorderAnimation::None,
                    "spin" => BorderAnimation::Spin,
                    "sweep" => BorderAnimation::Sweep,
                    other => bail!("animate takes \"none\", \"spin\" or \"sweep\", not {other:?}"),
                }
            }
            "period-ms" => config.period_ms = period_arg(child)?,
            other => bail!("unknown border setting {other:?}"),
        }
    }
    Ok(config)
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

/// One number argument, as a float. Whole numbers are fine (`offset 3`).
fn float_arg(node: &KdlNode) -> anyhow::Result<f64> {
    let name = node.name().value();
    match positional(node).as_slice() {
        [value] => value
            .as_float()
            .or_else(|| value.as_integer().map(|n| n as f64))
            .with_context(|| format!("{name} takes a number")),
        _ => bail!("{name} takes one number"),
    }
}

/// The `blur { … }` block. Having the block at all turns blur on.
fn parse_blur(node: &KdlNode) -> anyhow::Result<(Blur, Vec<String>, BlurWindows)> {
    let mut config = Blur {
        off: false,
        ..Blur::default()
    };
    let mut ignore_layers: Vec<String> = Vec::new();
    let mut windows = BlurWindows::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.off = flag_value(child)?,
            "all-windows" => config.all_windows = flag_value(child)?,
            "all-layers" => config.all_layers = flag_value(child)?,
            "behind-windows" => config.behind_windows = flag_value(child)?,
            // The namespace a panel reports in `jff msg` (`layers`), e.g. `ignore-layer "Nbar-Top"`.
            "ignore-layer" => ignore_layers.push(string_arg(child)?),
            // App ids (`jff msg` windows), e.g. `ignore-window "firefox"`: never blurred.
            "ignore-window" => windows.ignore.push(string_arg(child)?),
            // With any `only-window`, only those app ids are blurred.
            "only-window" => windows.only.push(string_arg(child)?),
            "passes" => {
                config.passes = u8::try_from(int_arg(child, 31)?)
                    .ok()
                    .filter(|n| *n >= 1)
                    .context("passes takes a number from 1 to 31")?;
            }
            "offset" => {
                config.offset = Some(float_arg(child)?)
                    .filter(|x| x.is_finite() && *x > 0.)
                    .context("offset takes a number above 0")?;
            }
            other => bail!("unknown blur setting {other:?}"),
        }
    }
    Ok((config, ignore_layers, windows))
}

/// `window-animations` as a flag, or as a block with `off`, `open-ms` and `close-ms`.
fn parse_window_animations(node: &KdlNode) -> anyhow::Result<WindowAnimations> {
    let mut config = WindowAnimations::default();
    if !positional(node).is_empty() {
        config.on = flag_value(node)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.on = !flag_value(child)?,
            "open" => {
                config.open = match string_arg(child)?.as_str() {
                    "fade" => OpenStyle::Fade,
                    "zoom" => OpenStyle::Zoom,
                    "slide" => OpenStyle::Slide,
                    "pop" => OpenStyle::Pop,
                    "none" => OpenStyle::None,
                    other => bail!(
                    "open takes \"fade\", \"zoom\", \"slide\", \"pop\" or \"none\", not {other:?}"
                ),
                }
            }
            "open-ms" => config.open_ms = u32::try_from(int_arg(child, MAX_WINDOW_ANIM_MS)?)?,
            "close-ms" => config.close_ms = u32::try_from(int_arg(child, MAX_WINDOW_ANIM_MS)?)?,
            other => bail!("unknown window-animations setting {other:?}"),
        }
    }
    Ok(config)
}

fn parse_osd(node: &KdlNode) -> anyhow::Result<OsdConfig> {
    let mut config = OsdConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "off" => config.off = flag_value(child)?,
            "timeout-ms" => {
                let ms = int_arg(child, 60_000)?;
                if ms < 100 {
                    bail!("timeout-ms takes a number from 100 to 60000");
                }
                config.timeout_ms = u32::try_from(ms)?;
            }
            "font" => {
                config.font = Some(string_arg(child)?)
                    .filter(|f| !f.trim().is_empty())
                    .context("font takes a family name, like \"Inter\"")?;
            }
            other => bail!("unknown osd setting {other:?}"),
        }
    }
    Ok(config)
}

/// `workspace-wall`, `workspace-wall #false`, or a block with `height N` and `off`. `None` when
/// it is off.
fn parse_workspace_wall(node: &KdlNode) -> anyhow::Result<Option<WorkspaceWallConfig>> {
    let mut on = true;
    let mut height = DEFAULT_WALL_HEIGHT;
    if !positional(node).is_empty() {
        on = flag_value(node)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "height" => {
                height = int_arg(child, MAX_WALL_HEIGHT)?;
                if height < MIN_WALL_HEIGHT {
                    bail!("height takes a number from {MIN_WALL_HEIGHT} to {MAX_WALL_HEIGHT}");
                }
            }
            "off" => on = !flag_value(child)?,
            other => bail!("unknown workspace-wall setting {other:?}"),
        }
    }
    Ok(on.then_some(WorkspaceWallConfig { height }))
}

/// A `window-rule` block: `match` lines, then actions.
fn parse_window_rule(node: &KdlNode) -> anyhow::Result<WindowRule> {
    if !positional(node).is_empty() {
        bail!("window-rule takes a block, not arguments");
    }
    let mut rule = WindowRule::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "match" => parse_window_match(child, &mut rule.matches)?,
            "open-on-output" => {
                let name = string_arg(child)?;
                if name.is_empty() {
                    bail!("open-on-output takes a monitor name or \"pointer\"");
                }
                rule.output = Some(if name == "pointer" {
                    RuleOutput::Pointer
                } else {
                    RuleOutput::Named(name)
                });
            }
            "open-workspace" => {
                let max = i32::try_from(MAX_WORKSPACES).unwrap_or(i32::MAX);
                let number = int_arg(child, max)?;
                if number < 1 {
                    bail!("open-workspace takes a number from 1 to {MAX_WORKSPACES}");
                }
                rule.workspace = Some(usize::try_from(number - 1)?);
            }
            "open-maximized" => rule.maximized = Some(flag_value(child)?),
            "open-size" => {
                let sides = positional(child)
                    .iter()
                    .map(|value| {
                        value
                            .as_integer()
                            .and_then(|n| i32::try_from(n).ok())
                            .filter(|n| (1..=MAX_RULE_SIZE).contains(n))
                    })
                    .collect::<Option<Vec<i32>>>();
                match sides.as_deref() {
                    Some(&[width, height]) => rule.size = Some((width, height)),
                    _ => bail!("open-size takes width and height, each from 1 to {MAX_RULE_SIZE}"),
                }
            }
            "floating" => rule.floating = flag_value(child)?,
            other => bail!("unknown window-rule setting {other:?}"),
        }
    }
    if rule.floating && rule.maximized == Some(true) {
        bail!("floating and open-maximized #true ask for opposite things");
    }
    Ok(rule)
}

/// One `match` line: `key=value` pairs, all of which have to hold.
fn parse_window_match(node: &KdlNode, out: &mut Vec<WindowMatch>) -> anyhow::Result<()> {
    if !positional(node).is_empty() {
        bail!("match takes name=value pairs, like match app-id=\"firefox\"");
    }
    let mut any = false;
    for entry in node.entries() {
        let Some(key) = entry.name().map(|key| key.value()) else {
            continue;
        };
        any = true;
        let value = entry.value();
        let text = || {
            value
                .as_string()
                .map(str::to_owned)
                .with_context(|| format!("match {key} takes a string"))
        };
        let flag = || {
            value
                .as_bool()
                .or_else(|| match value.as_string() {
                    Some("true" | "yes") => Some(true),
                    Some("false" | "no") => Some(false),
                    _ => None,
                })
                .with_context(|| format!("match {key} takes true or false"))
        };
        out.push(match key {
            "app-id" => WindowMatch::AppId(text()?),
            "title" => WindowMatch::Title(text()?),
            "tag" => WindowMatch::Tag(text()?),
            "dialog" => WindowMatch::Dialog(flag()?),
            "restored" => WindowMatch::Restored(flag()?),
            other => bail!("unknown match {other:?}: use app-id, title, tag, dialog or restored"),
        });
    }
    if !any {
        bail!("match needs a name=value pair, like match tag=\"browser\"");
    }
    Ok(())
}

/// The `switcher { … }` block.
fn parse_switcher(node: &KdlNode) -> anyhow::Result<SwitcherConfig> {
    let mut config = SwitcherConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "thumbnail-height" => {
                let height = int_arg(child, MAX_THUMBNAIL_HEIGHT)?;
                if height < MIN_THUMBNAIL_HEIGHT {
                    bail!(
                        "thumbnail-height takes a number from {MIN_THUMBNAIL_HEIGHT} to {MAX_THUMBNAIL_HEIGHT}"
                    );
                }
                config.thumbnail_height = Some(height);
            }
            "slide-ms" => config.slide_ms = u32::try_from(int_arg(child, MAX_SWITCHER_SLIDE_MS)?)?,
            other => bail!("unknown switcher setting {other:?}"),
        }
    }
    Ok(config)
}

/// The `overview { … }` block.
fn parse_overview(node: &KdlNode) -> anyhow::Result<OverviewConfig> {
    let mut config = OverviewConfig::default();
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "selection-width" => config.selection_width = int_arg(child, MAX_BORDER_WIDTH)?,
            "selection-color" => config.selection_color = parse_color(&string_arg(child)?)?,
            other => bail!("unknown overview setting {other:?}"),
        }
    }
    Ok(config)
}

/// A workspace count: one number from 1 to [`MAX_WORKSPACES`].
fn workspace_count(node: &KdlNode) -> anyhow::Result<usize> {
    positional(node)
        .first()
        .and_then(|value| value.as_integer())
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (1..=MAX_WORKSPACES).contains(n))
        .with_context(|| {
            format!(
                "{} takes a number from 1 to {MAX_WORKSPACES}",
                node.name().value()
            )
        })
}

/// `workspaces N`, or a block with `count N` and `per-output`. Returns the count and whether
/// every monitor gets its own workspace.
fn parse_workspaces(node: &KdlNode) -> anyhow::Result<(usize, bool)> {
    let mut count = DEFAULT_WORKSPACES;
    let mut per_output = false;
    let children = node.children().map(KdlDocument::nodes).unwrap_or_default();
    if positional(node).is_empty() && children.is_empty() {
        bail!("workspaces takes a number from 1 to {MAX_WORKSPACES}, or a block");
    }
    if !positional(node).is_empty() {
        count = workspace_count(node)?;
    }
    for child in children {
        match child.name().value() {
            "count" => count = workspace_count(child).context("in workspaces")?,
            "per-output" => per_output = flag_value(child).context("in workspaces")?,
            other => bail!("unknown workspaces setting {other:?}"),
        }
    }
    Ok((count, per_output))
}

/// The `hot-corner { … }` block, or `hot-corner N` for just the size. `None` when `size` is 0.
/// A block that doesn't say otherwise is on, with [`DEFAULT_HOT_CORNER_SIZE`].
fn parse_hot_corner(node: &KdlNode) -> anyhow::Result<Option<HotCorner>> {
    let mut size = DEFAULT_HOT_CORNER_SIZE;
    let mut dwell_ms = DEFAULT_HOT_CORNER_DWELL_MS;
    let mut monitors: Vec<String> = Vec::new();
    if !positional(node).is_empty() {
        size = int_arg(node, MAX_HOT_CORNER_SIZE)?;
    }
    for child in node.children().map(KdlDocument::nodes).unwrap_or_default() {
        match child.name().value() {
            "size" => size = int_arg(child, MAX_HOT_CORNER_SIZE)?,
            "dwell-ms" => dwell_ms = int_arg(child, MAX_HOT_CORNER_DWELL_MS)?,
            "monitor" => monitors.push(string_arg(child)?),
            other => bail!("unknown hot-corner setting {other:?}"),
        }
    }
    if size == 0 {
        return Ok(None);
    }
    Ok(Some(HotCorner {
        size,
        dwell: Duration::from_millis(u64::try_from(dwell_ms)?),
        monitors,
    }))
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
        "snap-left" => no_args(Action::Snap(Side::Left)),
        "snap-right" => no_args(Action::Snap(Side::Right)),
        "snap-top" => no_args(Action::Snap(Side::Top)),
        "snap-bottom" => no_args(Action::Snap(Side::Bottom)),
        "close" => no_args(Action::Close),
        "overview" => no_args(Action::Overview),
        "switch-window" => no_args(Action::SwitchWindow { backwards: false }),
        "switch-window-back" => no_args(Action::SwitchWindow { backwards: true }),
        "workspace-next" => no_args(Action::Workspace(Goto::Next)),
        "workspace-prev" => no_args(Action::Workspace(Goto::Prev)),
        "reload-config" => no_args(Action::ReloadConfig),
        "screenshot-area" => no_args(Action::ScreenshotArea),
        "screenshot-fullscreen" => no_args(Action::ScreenshotFullscreen),
        "screenshot-window" => no_args(Action::ScreenshotWindow),
        "toggle-shortcuts-inhibit" => no_args(Action::ToggleShortcutsInhibit),
        "forward-key" => match <[String; 1]>::try_from(args) {
            Ok([app_id]) if !app_id.is_empty() => Ok(Action::ForwardKey(app_id)),
            _ => bail!("forward-key takes one app id, like forward-key \"com.obsproject.Studio\""),
        },
        "quit" => no_args(Action::Quit),
        other => bail!("unknown action {other:?}"),
    }
}

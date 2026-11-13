//! Everything the window shows, as plain data. widgets.rs and lists.rs turn it into rows.
//!
//! Paths, ranges and defaults were checked against src/config/mod.rs in the compositor.
//! New setting: add a Field here. New page: add a Page in build() and an icon in main.rs.
//!
//! Capitalization follows the GNOME HIG: Title Case for pages, groups and row titles,
//! sentence case (no final period) for subtitles.

pub enum Kind {
    /// Whole number in min..=max. `default` is used when the key is missing, `block_default`
    /// when the parent block exists but the key doesn't (`border { }` alone is a 2px border,
    /// no border block at all is none). `optional` adds a reset button, `positional` is for
    /// `parent N` written as `parent { child N }`.
    Int {
        min: i64,
        max: i64,
        default: i64,
        block_default: Option<i64>,
        optional: bool,
        positional: bool,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
        default: f64,
    },
    /// Bare name is on, `#false` is off.
    Flag { default: bool },
    /// Switch for things that only exist by being written (`off`, `content-type-hint`).
    Presence,
    /// Unset, yes or no. `unset` is what the first entry says ("Any", "Default").
    Tri { unset: &'static str },
    /// `path` is a block that gets turned off by an `off` line inside it. `needs_block` is for
    /// blocks that only exist when they're on (blur).
    Enable { needs_block: bool },
    /// Pairs of (value in the file, label in the list).
    Choice {
        options: &'static [(&'static str, &'static str)],
        default: &'static str,
    },
    /// Free text, empty removes the key. Applied with Enter or the check button.
    Text,
    /// Written as "#rrggbbaa".
    Color {
        default: &'static str,
        optional: bool,
    },
    /// Two numbers on one line, like `position X Y` or `open-size W H`. Can be reset.
    Pair {
        min: i64,
        max: i64,
        default: (i64, i64),
    },
}

pub struct Field {
    /// Relative to the root the page builds under (empty for the plain pages).
    pub path: &'static [&'static str],
    /// If set, the value is this property of the node at `path`.
    pub prop: Option<&'static str>,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub kind: Kind,
}

/// A setting that's a list of lines, like `ignore-window "firefox"` or `autostart "foot"`.
#[derive(Clone)]
pub struct TextList {
    pub parent: &'static [&'static str],
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Title of each entry row.
    pub item: &'static str,
    /// Title of the row that adds an entry.
    pub add: &'static str,
    /// One line is several words (a program plus arguments) instead of a single string.
    pub words: bool,
}

pub struct Group {
    pub title: &'static str,
    pub description: &'static str,
    pub fields: Vec<Field>,
    pub lists: Vec<TextList>,
    /// First field decides if the others can be edited (blur on or off, border width 0).
    pub gated: bool,
}

pub enum Body {
    Groups(Vec<Group>),
    /// The monitor list, followed by these groups.
    Outputs(Vec<Group>),
    Rules,
    Binds,
}

pub struct Page {
    pub id: &'static str,
    pub title: &'static str,
    /// Extra words the sidebar search matches, on top of the titles in the page.
    pub keywords: &'static str,
    pub body: Body,
}

impl Page {
    /// Lowercase text the sidebar search looks through.
    pub fn search_text(&self) -> String {
        let mut text = format!("{} {}", self.title, self.keywords);
        let mut add_group = |g: &Group| {
            text.push(' ');
            text.push_str(g.title);
            for f in &g.fields {
                text.push(' ');
                text.push_str(f.title);
            }
            for l in &g.lists {
                text.push(' ');
                text.push_str(l.title);
            }
        };
        match &self.body {
            Body::Groups(groups) | Body::Outputs(groups) => groups.iter().for_each(&mut add_group),
            Body::Rules | Body::Binds => {}
        }
        text.to_lowercase()
    }
}

pub fn build() -> Vec<Page> {
    vec![
        Page {
            id: "appearance",
            title: "Appearance",
            keywords: "gaps corners radius border color decorations title bar osd font",
            body: Body::Groups(appearance()),
        },
        Page {
            id: "effects",
            title: "Effects",
            keywords: "blur animation fade zoom slide",
            body: Body::Groups(effects()),
        },
        Page {
            id: "multitasking",
            title: "Multitasking",
            keywords: "workspaces hot corner overview switcher alt tab wall drag maximize",
            body: Body::Groups(multitasking()),
        },
        Page {
            id: "displays",
            title: "Displays",
            keywords: "monitor output resolution mode refresh rate hdr tone map bt2390 knee sdr white peak icc profile position vrr tearing color management",
            body: Body::Outputs(displays()),
        },
        Page {
            id: "keyboard",
            title: "Keyboard",
            keywords: "layout xkb variant repeat delay num lock hotkeys",
            body: Body::Groups(keyboard()),
        },
        Page {
            id: "shortcuts",
            title: "Keyboard Shortcuts",
            keywords: "key bindings binds hotkey action spawn",
            body: Body::Binds,
        },
        Page {
            id: "rules",
            title: "Window Rules",
            keywords: "match app id title tag dialog floating size workspace open",
            body: Body::Rules,
        },
        Page {
            id: "startup",
            title: "Startup Applications",
            keywords: "autostart programs commands shell login",
            body: Body::Groups(startup()),
        },
        Page {
            id: "system",
            title: "System",
            keywords: "screenshots log file level folder clipboard cursor",
            body: Body::Groups(system()),
        },
    ]
}

// ---------------------------------------------------------------- builders

fn field(
    path: &'static [&'static str],
    title: &'static str,
    subtitle: &'static str,
    kind: Kind,
) -> Field {
    Field {
        path,
        prop: None,
        title,
        subtitle,
        kind,
    }
}

fn int(
    path: &'static [&'static str],
    title: &'static str,
    subtitle: &'static str,
    min: i64,
    max: i64,
    default: i64,
) -> Field {
    field(
        path,
        title,
        subtitle,
        Kind::Int {
            min,
            max,
            default,
            block_default: None,
            optional: false,
            positional: false,
        },
    )
}

fn flag(
    path: &'static [&'static str],
    title: &'static str,
    subtitle: &'static str,
    default: bool,
) -> Field {
    field(path, title, subtitle, Kind::Flag { default })
}

fn text(path: &'static [&'static str], title: &'static str) -> Field {
    field(path, title, "", Kind::Text)
}

fn color(path: &'static [&'static str], title: &'static str, default: &'static str) -> Field {
    field(
        path,
        title,
        "",
        Kind::Color {
            default,
            optional: false,
        },
    )
}

fn enable(path: &'static [&'static str], title: &'static str, needs_block: bool) -> Field {
    field(path, title, "", Kind::Enable { needs_block })
}

fn choice(
    path: &'static [&'static str],
    title: &'static str,
    subtitle: &'static str,
    options: &'static [(&'static str, &'static str)],
    default: &'static str,
) -> Field {
    field(path, title, subtitle, Kind::Choice { options, default })
}

impl Field {
    fn block_default(mut self, value: i64) -> Self {
        if let Kind::Int { block_default, .. } = &mut self.kind {
            *block_default = Some(value);
        }
        self
    }

    fn optional(mut self) -> Self {
        match &mut self.kind {
            Kind::Int { optional, .. } | Kind::Color { optional, .. } => *optional = true,
            _ => {}
        }
        self
    }

    fn positional(mut self) -> Self {
        if let Kind::Int { positional, .. } = &mut self.kind {
            *positional = true;
        }
        self
    }

    fn prop(mut self, prop: &'static str) -> Self {
        self.prop = Some(prop);
        self
    }
}

fn group(title: &'static str, fields: Vec<Field>) -> Group {
    Group {
        title,
        description: "",
        fields,
        lists: Vec::new(),
        gated: false,
    }
}

impl Group {
    fn gated(mut self) -> Self {
        self.gated = true;
        self
    }

    fn described(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    fn lists(mut self, lists: Vec<TextList>) -> Self {
        self.lists = lists;
        self
    }
}

fn list(
    parent: &'static [&'static str],
    name: &'static str,
    title: &'static str,
    item: &'static str,
    add: &'static str,
) -> TextList {
    TextList {
        parent,
        name,
        title,
        description: "",
        item,
        add,
        words: false,
    }
}

// ------------------------------------------------------------------- pages

fn appearance() -> Vec<Group> {
    const ANIMATE: &[(&str, &str)] = &[("none", "None"), ("spin", "Spin"), ("sweep", "Sweep")];
    vec![
        group(
            "Windows",
            vec![
                int(
                    &["gaps"],
                    "Gaps",
                    "Space around maximized windows, in pixels",
                    0,
                    512,
                    0,
                ),
                int(
                    &["radius"],
                    "Corner Radius",
                    "In pixels, 0 for square corners",
                    0,
                    256,
                    0,
                ),
                int(
                    &["popup-radius"],
                    "Popup Corner Radius",
                    "Follows the corner radius when not set",
                    0,
                    256,
                    0,
                )
                .optional(),
                flag(
                    &["no-client-decorations"],
                    "Server-Side Decorations Only",
                    "Ask windows not to draw their own title bar",
                    false,
                ),
            ],
        ),
        group(
            "Border",
            vec![
                int(
                    &["border", "width"],
                    "Border Width",
                    "In pixels, 0 turns the border off",
                    0,
                    64,
                    0,
                )
                .block_default(2),
                int(
                    &["border", "fade-ms"],
                    "Focus Fade",
                    "How long the color takes to change, in milliseconds",
                    0,
                    5_000,
                    150,
                ),
                color(&["border", "color"], "Color", "#595966ff"),
                color(&["border", "focus-color"], "Focused Color", "#598cf2ff"),
                field(
                    &["border", "focus-color-2"],
                    "Second Focused Color",
                    "Needed for an animated border",
                    Kind::Color {
                        default: "#598cf2ff",
                        optional: true,
                    },
                ),
                choice(&["border", "animate"], "Animation", "", ANIMATE, "none"),
                int(
                    &["border", "period-ms"],
                    "Animation Period",
                    "Time for one turn, in milliseconds",
                    0,
                    600_000,
                    3_000,
                ),
            ],
        )
        .gated(),
        group(
            "On-Screen Display",
            vec![
                enable(&["osd"], "On-Screen Display", false),
                int(
                    &["osd", "timeout-ms"],
                    "Timeout",
                    "In milliseconds, from 100 to 60000",
                    100,
                    60_000,
                    1_800,
                ),
                text(&["osd", "font"], "Font Family"),
            ],
        )
        .gated(),
    ]
}

fn effects() -> Vec<Group> {
    const OPEN: &[(&str, &str)] = &[
        ("fade", "Fade"),
        ("zoom", "Zoom"),
        ("slide", "Slide"),
        ("pop", "Pop"),
        ("none", "None"),
    ];
    vec![
        group(
            "Blur",
            vec![
                enable(&["blur"], "Blur", true),
                int(
                    &["blur", "passes"],
                    "Passes",
                    "More passes make a softer blur",
                    1,
                    31,
                    3,
                ),
                field(
                    &["blur", "offset"],
                    "Offset",
                    "",
                    Kind::Float {
                        min: 0.1,
                        max: 64.0,
                        step: 0.5,
                        default: 3.0,
                    },
                ),
                flag(&["blur", "all-windows"], "Blur All Windows", "", false),
                flag(
                    &["blur", "all-layers"],
                    "Blur All Panels and Overlays",
                    "",
                    false,
                ),
                flag(
                    &["blur", "behind-windows"],
                    "Blur Behind Windows",
                    "",
                    false,
                ),
            ],
        )
        .gated()
        .lists(vec![
            list(
                &["blur"],
                "ignore-layer",
                "Panels Never Blurred",
                "Panel Namespace",
                "Add a Panel Namespace",
            ),
            list(
                &["blur"],
                "ignore-window",
                "Apps Never Blurred",
                "App ID",
                "Add an App ID",
            ),
            list(
                &["blur"],
                "only-window",
                "Only These Apps Are Blurred",
                "App ID",
                "Add an App ID",
            ),
        ]),
        group(
            "Window Animations",
            vec![
                enable(&["window-animations"], "Window Animations", false),
                choice(
                    &["window-animations", "open"],
                    "Open Animation",
                    "",
                    OPEN,
                    "fade",
                ),
                int(
                    &["window-animations", "open-ms"],
                    "Open Duration",
                    "In milliseconds",
                    0,
                    10_000,
                    100,
                ),
                int(
                    &["window-animations", "close-ms"],
                    "Close Duration",
                    "In milliseconds",
                    0,
                    10_000,
                    80,
                ),
            ],
        )
        .gated(),
    ]
}

fn multitasking() -> Vec<Group> {
    const MODIFIERS: &[(&str, &str)] = &[
        ("Alt", "Alt"),
        ("Super", "Super"),
        ("Ctrl", "Ctrl"),
        ("Shift", "Shift"),
    ];
    vec![
        group(
            "Workspaces",
            vec![
                int(
                    &["workspaces", "count"],
                    "Number of Workspaces",
                    "",
                    1,
                    64,
                    9,
                )
                .positional(),
                flag(
                    &["workspaces", "per-output"],
                    "Workspaces on Each Display",
                    "Every monitor gets its own set",
                    false,
                ),
            ],
        ),
        group(
            "Windows",
            vec![
                flag(&["maximize-new-windows"], "Maximize New Windows", "", true),
                choice(
                    &["drag-modifier"],
                    "Drag Modifier",
                    "Modifier key for dragging windows",
                    MODIFIERS,
                    "Alt",
                ),
            ],
        ),
        group(
            "Hot Corner",
            vec![
                int(
                    &["hot-corner", "size"],
                    "Size",
                    "In pixels, 0 turns the hot corner off",
                    0,
                    64,
                    0,
                )
                .block_default(4)
                .positional(),
                int(
                    &["hot-corner", "dwell-ms"],
                    "Dwell Time",
                    "How long the pointer waits in the corner, in milliseconds",
                    0,
                    5_000,
                    250,
                ),
            ],
        )
        .gated()
        .lists(vec![list(
            &["hot-corner"],
            "monitor",
            "Displays with a Hot Corner",
            "Display",
            "Add a Display Name",
        )]),
        group(
            "Window Switcher",
            vec![
                int(
                    &["switcher", "thumbnail-height"],
                    "Thumbnail Height",
                    "In pixels, jff picks a size when not set",
                    32,
                    4_096,
                    120,
                )
                .optional(),
                int(
                    &["switcher", "slide-ms"],
                    "Slide Duration",
                    "In milliseconds",
                    0,
                    2_000,
                    120,
                ),
            ],
        ),
        group(
            "Overview",
            vec![
                int(
                    &["overview", "selection-width"],
                    "Selection Outline",
                    "In pixels, 0 for none",
                    0,
                    64,
                    4,
                ),
                color(
                    &["overview", "selection-color"],
                    "Selection Color",
                    "#598cf299",
                ),
            ],
        ),
        group(
            "Workspace Wall",
            vec![
                enable(&["workspace-wall"], "Workspace Wall", true),
                int(
                    &["workspace-wall", "height"],
                    "Height",
                    "In pixels",
                    32,
                    400,
                    96,
                )
                .block_default(96),
                flag(
                    &["workspace-wall", "thumbnails"],
                    "Show Thumbnails",
                    "",
                    true,
                ),
            ],
        )
        .gated(),
    ]
}

/// Goes below the monitor list on the Displays page.
fn displays() -> Vec<Group> {
    vec![
        group(
            "Color",
            vec![
                flag(
                    &["color-management", "off"],
                    "Disable Color Management",
                    "Takes effect after a restart",
                    false,
                ),
                flag(
                    &["color-management", "convert-srgb"],
                    "Convert sRGB Windows Too",
                    "Stops oversaturation on wide-gamut monitors",
                    false,
                ),
                flag(
                    &["color-management", "fullscreen-passthrough"],
                    "Never Convert Fullscreen Windows",
                    "Keeps direct scanout working",
                    false,
                ),
                flag(
                    &["color-representation", "yuv"],
                    "YUV Color Representation",
                    "",
                    false,
                ),
            ],
        ),
        group(
            "Gaming",
            vec![flag(
                &["allow-tearing"],
                "Allow Tearing",
                "Lower latency in fullscreen games, at the cost of tearing",
                false,
            )],
        ),
    ]
}

fn keyboard() -> Vec<Group> {
    vec![
        group(
            "Layout",
            vec![
                text(&["keyboard", "xkb", "layout"], "Layout"),
                text(&["keyboard", "xkb", "variant"], "Variant"),
                text(&["keyboard", "xkb", "options"], "Options"),
            ],
        )
        .described("XKB names, like us, de or br for the layout, intl for the variant, caps:escape for options"),
        group(
            "Typing",
            vec![
                int(
                    &["keyboard", "repeat-delay"],
                    "Repeat Delay",
                    "Time before a held key repeats, in milliseconds",
                    0,
                    10_000,
                    200,
                ),
                int(
                    &["keyboard", "repeat-rate"],
                    "Repeat Rate",
                    "Repeats per second, 0 for no repeat",
                    0,
                    1_000,
                    25,
                ),
                flag(&["keyboard", "numlock"], "Num Lock on at Startup", "", true),
            ],
        ),
        group(
            "Application Hotkeys",
            vec![flag(
                &["global-hotkeys"],
                "Global Hotkeys for Apps",
                "Let apps bind their own hotkeys (xx-hotkey)",
                false,
            )],
        ),
        group(
            "Advanced",
            vec![
                text(&["keyboard", "xkb", "rules"], "Rules"),
                text(&["keyboard", "xkb", "model"], "Model"),
            ],
        ),
    ]
}

fn startup() -> Vec<Group> {
    vec![
        group("", Vec::new()).lists(vec![TextList {
            parent: &[],
            name: "autostart",
            title: "Programs",
            description: "Started with jff. Write the program first, then its arguments",
            item: "Program",
            add: "Add a Program and Its Arguments",
            words: true,
        }]),
        group("", Vec::new()).lists(vec![TextList {
            description: "Run through the shell, so pipes and variables work",
            ..list(
                &[],
                "sh-autostart",
                "Shell Commands",
                "Command",
                "Add a Command Line",
            )
        }]),
    ]
}

fn system() -> Vec<Group> {
    const LEVELS: &[(&str, &str)] = &[
        ("error", "Error"),
        ("warn", "Warning"),
        ("info", "Info"),
        ("debug", "Debug"),
        ("trace", "Trace"),
    ];
    vec![
        group(
            "Screenshots",
            vec![
                text(&["screenshot", "path"], "Screenshot Folder"),
                flag(
                    &["screenshot", "include-cursor"],
                    "Include the Pointer",
                    "",
                    false,
                ),
                flag(
                    &["screenshot", "copy-to-clipboard"],
                    "Copy to the Clipboard",
                    "",
                    true,
                ),
            ],
        ),
        group(
            "Logging",
            vec![
                flag(&["log", "enable"], "Write a Log File", "", false),
                text(&["log", "path"], "File or Folder"),
                choice(&["log", "level"], "Level", "", LEVELS, "info"),
            ],
        )
        .described("A path that ends with / is a folder")
        .gated(),
    ]
}

/// Rows for one `output "NAME" { }`, relative to that node.
pub fn output_fields() -> Vec<Field> {
    const SYNC: &[(&str, &str)] = &[
        ("off", "Off"),
        ("fullscreen", "Fullscreen Only"),
        ("always", "Always"),
    ];
    const TONE_MAP: &[(&str, &str)] = &[("knee", "Knee"), ("bt2390", "BT.2390")];
    vec![
        field(&["off"], "Turn Off This Display", "", Kind::Presence),
        field(
            &["position"],
            "Position",
            "Top-left corner on the shared desktop",
            Kind::Pair {
                min: -100_000,
                max: 100_000,
                default: (0, 0),
            },
        ),
        text(&["mode"], "Mode, for example 1920x1080@60"),
        choice(
            &["adaptive-sync"],
            "Variable Refresh Rate",
            "",
            SYNC,
            "fullscreen",
        ),
        flag(
            &["hdr"],
            "HDR",
            "Signals HDR10 when the monitor and connector can do it, and stays SDR when they can't",
            false,
        ),
        int(
            &["sdr-white"],
            "SDR White",
            "Brightness of SDR white on an HDR display, in cd/m². Lower it for more room for highlights",
            10,
            1000,
            203,
        )
        .optional(),
        int(
            &["hdr-peak"],
            "HDR Peak",
            "Peak brightness in cd/m², used instead of the one the display reports",
            100,
            10_000,
            400,
        )
        .optional(),
        choice(
            &["hdr-tone-map"],
            "HDR Tone Mapping",
            "How highlights brighter than the display can show are rolled off. Only used with HDR on",
            TONE_MAP,
            "knee",
        ),
        text(&["icc-profile"], "ICC Profile Path"),
        field(
            &["content-type-hint"],
            "Game Content Type",
            "Tells the monitor it's showing a game in fullscreen games",
            Kind::Presence,
        ),
        flag(&["color-management"], "Color Managed", "", true),
    ]
}

/// Rows for one `window-rule { }`, relative to that node. Only the first `match` line gets
/// edited, a rule with several keeps the rest untouched.
pub fn rule_fields() -> Vec<Field> {
    vec![
        field(&["match"], "Match App ID", "", Kind::Text).prop("app-id"),
        field(&["match"], "Match Title", "", Kind::Text).prop("title"),
        field(&["match"], "Match Tag", "", Kind::Text).prop("tag"),
        field(&["match"], "Match Dialogs", "", Kind::Tri { unset: "Any" }).prop("dialog"),
        field(
            &["match"],
            "Match Restored Windows",
            "",
            Kind::Tri { unset: "Any" },
        )
        .prop("restored"),
        text(&["open-on-output"], "Open on Display, a name or pointer"),
        int(&["open-workspace"], "Open on Workspace", "", 1, 64, 1).optional(),
        field(
            &["open-maximized"],
            "Open Maximized",
            "",
            Kind::Tri { unset: "Default" },
        ),
        field(
            &["open-size"],
            "Open Size",
            "Width and height",
            Kind::Pair {
                min: 1,
                max: 16_384,
                default: (800, 600),
            },
        ),
        flag(
            &["floating"],
            "Floating",
            "Keep the window at its own size",
            false,
        ),
    ]
}

//! The window overview: every window of one monitor shown as a thumbnail, to pick one from.
//!
//! Geometry lives in `overview_grid.rs` (pure, unit-tested). This file is the Smithay side:
//!
//! - [`Overview`] - state in [`JffState::overview`]: the monitor shown, a stable window order,
//!   the selection, and the focus to restore on cancel.
//! - [`OverviewGrab`] - owns the pointer while open. Clients see no pointer. Motion moves the
//!   selection, clicks are handled in `input.rs` ([`JffState::overview_button`]) because acting
//!   on them ends the grab.
//! - Keys come through the `input_intercept` closure in `input.rs` ([`overview_key`]).
//! - [`overview_elements`] - turns an [`OverviewPlan`] into render elements for both backends.
//!
//! Search: typing while the overview is open filters the windows by title and app id and lists
//! matching installed apps under them (see `primitives/app_search.rs`, `feature/app_index.rs`,
//! `feature/overview_search.rs`). Enter focuses the selected window or starts the selected app.
//! The Tab, arrow and Enter keys work on thumbnails only while nothing is typed. Not in the
//! window switcher: letting go of Alt there picks a window.
//!
//! Opening, by keybind or hot corner, also plays ripples at the top-left corner of the monitor
//! (see `render/effects/ripple.rs`). Not in the switcher.
//!
//! Closing runs the opening animation backwards. Input and focus release at once, only the
//! picture lingers (`closing` is set), and the state is dropped when the animation is over.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::input::{AxisSource, ButtonState, InputTime};
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::utils::{
    Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{AsRenderElements, Element, Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::desktop::{layer_map_for_output, Window};
use smithay::input::keyboard::Keysym;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorImageStatus, Focus, GestureHoldBeginEvent, GestureHoldEndEvent,
    GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent,
    GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{
    Logical, Physical, Point, Rectangle, Scale, Serial, Transform, SERIAL_COUNTER,
};
use smithay::wayland::shell::wlr_layer::Layer;

use crate::config;
use crate::desk::{ContainerId, WindowMode};
use crate::feature::osd::{
    draw_text, half_line, over, sd_round_box, text_width, toplevel_names, Fonts, Picture, Pixel,
    SHADOW,
};
use crate::feature::outputs::output_rect;
use crate::feature::overview_search::{self as search_ui, Row};
use crate::input::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::ipc::window_is_urgent;
use crate::primitives::app_search::{self as apps, DesktopEntry, Pick, Query};
use crate::primitives::container_view;
use crate::primitives::geom::Size;
use crate::primitives::layout::{self, Rect};
use crate::primitives::overview_grid::{self as overview, Dir, Held, Thumb};
use crate::protocol::layer_shell::{layer_body_elements, ABOVE_WINDOWS, BELOW_WINDOWS};
use crate::protocol::toplevel_icon;
use crate::render::effects::background_effect::render_for_surface_zoomed;
use crate::render::effects::blur::BlurOptions;
use crate::render::effects::framebuffer_effect::{FramebufferEffectElement, WallpaperSnapshot};
use crate::render::effects::gamut;
use crate::render::effects::ripple::ripple_element;
use crate::render::effects::rounded::{is_flush, ring_element, Rounding};
use crate::render::elements::JffElement;
use crate::state::JffState;

mod elements;
mod input;
mod lifecycle;
mod plan;

pub(crate) use self::elements::thumb_elements;
pub use self::elements::{overview_drag_elements, overview_elements, overview_ripple_elements};

/// Free space around the thumbnails so docks and panels stay visible.
const MARGIN: i32 = 150;
/// Space between thumbnails.
const GAP: i32 = 24;
/// Share of the monitor's width the shown workspace takes, at most: the neighbours peek out
/// on both sides of what is left.
const STAGE_PERCENT: i32 = 82;
/// Least space above and below the shown workspace.
const STAGE_MARGIN: i32 = 110;
/// Space between the shown workspace's frame and its windows.
const STAGE_PAD: i32 = 24;
/// Space between the shown workspace and its neighbours.
const STAGE_GAP: i32 = 32;
/// Corner radius of the workspaces' frames.
const STAGE_RADIUS: i32 = 28;
/// Opacity of the black laid over the wallpaper of the workspaces of the overview, when fully
/// open.
const STAGE_DIM_ALPHA: f32 = 0.45;
/// How long the slide to a neighbouring workspace takes. Also the wheel's pause after a step
/// (`wheel_paced`): notches during it are dropped, so a long slide made scrolling feel stuck.
const PAN_DURATION: Duration = Duration::from_millis(150);
/// Colour (straight alpha) of the ripples at the top-left corner when the overview opens.
const RIPPLE_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 0.9];
/// How far the pointer moves with the left button held before it's a drag, not a click.
const DRAG_THRESHOLD: f64 = 8.0;

/// What a key does while the overview is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewKey {
    /// Leave and restore the previous focus.
    Cancel,
    /// Leave and focus the selected window.
    Select,
    Move(Dir),
    /// Slide to the workspace on the left (`-1`) or right (`1`).
    Workspace(i32),
    Cycle {
        backwards: bool,
    },
    /// A character typed into the search.
    Type(char),
    /// Delete the last character of the search.
    Backspace,
}

/// The overview key for `sym`, if it's one. Modifiers are ignored - Shift+Tab arrives as
/// `ISO_Left_Tab`.
pub fn overview_key(sym: Keysym) -> Option<OverviewKey> {
    if sym == Keysym::Escape {
        Some(OverviewKey::Cancel)
    } else if sym == Keysym::Return || sym == Keysym::KP_Enter {
        Some(OverviewKey::Select)
    } else if sym == Keysym::Tab {
        Some(OverviewKey::Cycle { backwards: false })
    } else if sym == Keysym::ISO_Left_Tab {
        Some(OverviewKey::Cycle { backwards: true })
    } else if sym == Keysym::Left {
        Some(OverviewKey::Move(Dir::Left))
    } else if sym == Keysym::Right {
        Some(OverviewKey::Move(Dir::Right))
    } else if sym == Keysym::Up {
        Some(OverviewKey::Move(Dir::Up))
    } else if sym == Keysym::Down {
        Some(OverviewKey::Move(Dir::Down))
    } else if sym == Keysym::Page_Up {
        Some(OverviewKey::Workspace(-1))
    } else if sym == Keysym::Page_Down {
        Some(OverviewKey::Workspace(1))
    } else if sym == Keysym::BackSpace {
        Some(OverviewKey::Backspace)
    } else {
        None
    }
}

/// The character `sym` types into the search, if it types one. `sym` should be the keysym with
/// the layout and Shift applied, so "ç" or "A" come out as the keyboard shows them. Ctrl, Alt
/// and Super make a shortcut, not text.
pub fn typed_char(sym: Keysym, ctrl: bool, alt: bool, logo: bool) -> Option<char> {
    if ctrl || alt || logo {
        return None;
    }
    sym.key_char().filter(|c| !c.is_control())
}

/// The sideways slide of the switcher's row: where it was, where it is headed, and since when.
#[derive(Clone, Copy)]
struct Slide {
    from: f64,
    to: i32,
    since: Instant,
    /// How long it takes (`switcher { slide-ms }` when it started).
    total: Duration,
}

impl Slide {
    /// Standing still at `at`.
    fn still(at: i32) -> Self {
        Self {
            from: at as f64,
            to: at,
            since: Instant::now(),
            total: Duration::ZERO,
        }
    }

    /// The offset right now.
    fn now(&self) -> f64 {
        overview::slide_at(self.from, self.to as f64, self.since.elapsed(), self.total)
    }
}

/// The search part of the overview: what was typed and what it found.
struct Search {
    query: Query,
    /// The list of apps `apps` points into, and the version it had when it was taken.
    entries: Arc<Vec<DesktopEntry>>,
    version: u64,
    /// Indices into `entries`, best match first.
    apps: Vec<usize>,
    /// Row of `apps` that is selected. While it is set the selected window is not shown as
    /// selected and Enter starts the app.
    app_selected: Option<usize>,
    /// Where the app list is drawn (shared space), for the pointer. Set when drawing.
    list_rect: Option<Rect>,
    /// The painted search box and app list, and what they were painted for.
    box_paint: Option<(u64, MemoryRenderBuffer)>,
    list_paint: Option<(u64, MemoryRenderBuffer)>,
}

impl Search {
    fn new(entries: Arc<Vec<DesktopEntry>>, version: u64) -> Self {
        Self {
            query: Query::default(),
            entries,
            version,
            apps: Vec::new(),
            app_selected: None,
            list_rect: None,
            box_paint: None,
            list_paint: None,
        }
    }

    /// Something is typed, so the overview shows results rather than every window.
    fn active(&self) -> bool {
        !self.query.is_empty()
    }

    /// The app Enter would start.
    fn selected_entry(&self) -> Option<DesktopEntry> {
        if !self.active() {
            return None;
        }
        let row = self.app_selected?;
        let index = *self.apps.get(row)?;
        self.entries.get(index).cloned()
    }
}

/// Whether `window`'s title or app id matches `query`.
fn window_matches_query(window: &Window, query: &str) -> bool {
    let (app_id, title) = window
        .toplevel()
        .map(|toplevel| toplevel_names(toplevel.wl_surface()))
        .unwrap_or_default();
    apps::window_matches(query, &title, &app_id)
}

fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// How long the app icon takes to travel between the bottom and the centre of a thumbnail.
const CARRY_ANIMATION: Duration = Duration::from_millis(150);

/// How big the icon is while carried, as a fraction of its size at rest (1.0: unchanged).
const CARRY_ICON_SCALE: f64 = 1.0;

/// Slides the icon of the dragged thumbnail to its centre, and back when it is dropped.
struct Carry {
    /// The window whose icon is moved (kept while it travels back).
    window: Option<Window>,
    from: f64,
    to: f64,
    at: Instant,
}

impl Carry {
    fn new() -> Self {
        Self {
            window: None,
            from: 0.0,
            to: 0.0,
            at: Instant::now(),
        }
    }

    /// 0 for the icon at the bottom, 1 at the centre.
    fn value(&self) -> f64 {
        let t = overview::ease_out_cubic(overview::progress(self.at.elapsed(), CARRY_ANIMATION));
        self.from + (self.to - self.from) * t
    }

    /// Follow `dragging`, the window to move the icon of and how far along it is.
    fn step(&mut self, dragging: Option<Window>) -> Option<(Window, f64)> {
        let want = if dragging.is_some() { 1.0 } else { 0.0 };
        if let Some(window) = dragging {
            if self.window.as_ref() != Some(&window) {
                self.window = Some(window);
                self.from = 0.0;
                self.to = 0.0;
            }
        }
        if self.to != want {
            self.from = self.value();
            self.to = want;
            self.at = Instant::now();
        }
        let value = self.value();
        if want == 0.0 && value <= 0.0 {
            self.window = None;
        }
        self.window.clone().map(|window| (window, value))
    }
}

/// A thumbnail being carried by the pointer.
struct Drag {
    window: Window,
    /// Where the pointer grabbed it, as fractions of width and height. Of the container's cell
    /// when `container` is set.
    anchor: (f64, f64),
    /// Set when the window is a member of a container: the whole container, every window of it,
    /// is what is being carried.
    container: Option<ContainerId>,
}

/// A tiling container as the overview shows it: one cell of the grid with its windows inside.
struct Group {
    id: ContainerId,
    /// The name above the cell: the container's name or its first window's app id.
    label: String,
    /// The container's rectangle in the real space, border not included.
    area: Rect,
    /// Its windows in tree order, each with where it really is.
    leaves: Vec<(Window, Rect)>,
    /// The window a click on the cell (and not on a window in it) selects.
    focused: Window,
}

/// A container's cell for one frame of the overview, in the shared space.
struct CellPlan {
    rect: Rect,
    /// The painted name and where it goes. `None` while the font loads, or with nothing to say.
    label: Option<(MemoryRenderBuffer, Point<i32, Logical>)>,
    /// Stable id of the cell's fill.
    id: Id,
    /// Stable id of the blur behind the cell's fill.
    blur_id: Id,
}

/// Most names remembered painted.
const MAX_CELL_LABELS: usize = 32;

/// The overview, while open or animating closed.
pub struct Overview {
    pub(crate) output: Output,
    /// Windows in a stable order (most recently used first at open time). Windows that come and
    /// go are appended or dropped, the rest never reshuffles, so raising a window on exit
    /// doesn't move thumbnails around during the closing animation.
    windows: Vec<Window>,
    /// The windows shown: all of `windows`, or with something typed the ones matching it. Same
    /// order as `windows`.
    visible: Vec<Window>,
    selected: Option<Window>,
    /// Focus before opening, restored on cancel.
    previous_focus: Option<Window>,
    /// Button pressed in the overview, not released yet.
    pressed: Option<u32>,
    /// The left button went down on the close button, so its release closes the window.
    press_on_close: bool,
    /// The close button painted at rest and under the pointer, for every side it can have, made
    /// when first needed.
    close_buttons: HashMap<i32, (MemoryRenderBuffer, MemoryRenderBuffer)>,
    /// The badge of an urgent window for every side it can have, made when first needed.
    urgent_badges: HashMap<i32, MemoryRenderBuffer>,
    /// (window, pointer pos) captured when the left button went down on a thumbnail. Moving far
    /// enough starts a drag.
    press_origin: Option<(Window, Point<f64, Logical>)>,
    /// Thumbnail being dragged. Dropped on another monitor, it moves its window there.
    drag: Option<Drag>,
    /// Buttons pressed on a layer surface and not released yet: the client still gets the
    /// release, wherever the pointer ends up.
    layer_buttons: Vec<u32>,
    /// Our pointer grab's serial, so we can recognise it when ending.
    grab_serial: Serial,
    opened: Instant,
    /// How long opening and closing take (`overview { animation-ms }` when it opened).
    animation: Duration,
    /// Set once we leave: input released, animation runs backwards.
    closing: Option<Instant>,
    /// How far open the overview was (see [`Overview::progress`]) when it started closing, so
    /// leaving halfway through opening goes back from there instead of jumping to fully open.
    closing_from: f64,
    /// Stable id of the highlight, so damage tracking sees it move rather than reappear.
    highlight: Id,
    /// Stable ids of the fills of the workspace wall's squares, one per square.
    wall_ids: Vec<Id>,
    /// Stable ids of the fills of the containers' cells, one per cell.
    cell_ids: Vec<Id>,
    /// Stable ids of the blur behind each cell's fill, one per cell.
    cell_blur_ids: Vec<Id>,
    /// The painted names of the containers' cells by text and text size, with their size.
    cell_labels: HashMap<(String, i32), (MemoryRenderBuffer, (i32, i32))>,
    /// The painted title of the selected thumbnail by text and text size, with its size.
    title_labels: HashMap<(String, i32), (MemoryRenderBuffer, (i32, i32))>,
    /// The painted number of each square of the wall: the text size it was painted at, the
    /// buffer, and its size. Painted again only when the size changes.
    wall_labels: Vec<Option<(i32, MemoryRenderBuffer, (i32, i32))>>,
    /// Set when this is the window switcher: a row of fixed-size thumbnails that slides to keep
    /// the selected one centered, ended when these modifiers are let go.
    switcher: Option<Held>,
    /// How far the switcher's row is slid sideways. Unused outside the switcher.
    slide: Slide,
    /// Typed text and its results. Stays empty in the switcher.
    search: Search,
    /// The painted app icon badge of each app id among `windows`, drawn at the bottom centre of
    /// its thumbnails. Painted when a window first shows up.
    badges: HashMap<(String, u64), MemoryRenderBuffer>,
    /// What `badges` was painted for: the version of the app list (icons come from it) and
    /// whether the font was there (the lettered fallback needs it). Another one empties it.
    badges_for: (u64, bool),
    /// A second copy of each of `badges`, with its own render id, for the icon being carried:
    /// it moves alone, while the shared copy sits still on the other thumbnails of its app.
    carry_badges: HashMap<(String, u64), MemoryRenderBuffer>,
    /// The icon of the dragged thumbnail moving to its centre.
    carry: Carry,
    /// When the slide from the previous workspace started and how far sideways it began.
    pan: Option<(Instant, f64)>,
    /// The black pixel that darkens the workspaces' wallpaper, made when first needed.
    stage_dim: Option<MemoryRenderBuffer>,
    /// Stable id of the backdrop behind the stage's frames (`backdrop-color`).
    backdrop_id: Id,
    /// A pixel of the containers' fill colour (without its alpha) and that colour, made when
    /// first needed, for cells with rounded corners.
    cell_fill_pixel: Option<([f32; 3], MemoryRenderBuffer)>,
    /// Stable ids of the blur behind each square of the shown workspace and its neighbours.
    stage_blur_ids: Vec<Id>,
    /// Stable id of the blur behind the search box.
    search_blur: Id,
}

impl Overview {
    /// Whether this overview is on `output`.
    pub fn shows(&self, output: &Output) -> bool {
        self.output == *output
    }

    /// Work out `visible` again, after the query or the windows changed.
    fn update_visible(&mut self) {
        self.visible = if self.search.active() {
            let query = self.search.query.as_str();
            self.windows
                .iter()
                .filter(|window| window_matches_query(window, query))
                .cloned()
                .collect()
        } else {
            self.windows.clone()
        };
    }

    /// 0.0 = windows at their real spot, 1.0 = windows in the grid.
    fn progress(&self) -> f64 {
        if self.switcher.is_some() {
            return 1.0;
        }
        match self.closing {
            Some(since) => {
                self.closing_from
                    * (1.0
                        - overview::ease_overview(overview::progress(
                            since.elapsed(),
                            self.closing_total(),
                        )))
            }
            None => {
                overview::ease_overview(overview::progress(self.opened.elapsed(), self.animation))
            }
        }
    }

    /// How far sideways everything is still shifted by the slide between workspaces, in pixels.
    fn pan_offset(&self) -> i32 {
        match self.pan {
            Some((since, distance)) => {
                let t = overview::ease_overview(overview::progress(since.elapsed(), PAN_DURATION));
                (distance * (1.0 - t)).round() as i32
            }
            None => 0,
        }
    }

    /// Whether the selected thumbnail carries its close button now: not in the switcher, while
    /// closing, dragging or sliding between workspaces, nor before it is fully open (the
    /// thumbnails are still on their way), and not with an app of the list selected.
    fn shows_close_button(&self) -> bool {
        self.switcher.is_none()
            && self.closing.is_none()
            && self.drag.is_none()
            && self.search.app_selected.is_none()
            && self.pan_offset() == 0
            && self.progress() > 0.99
    }

    /// How long closing takes: shorter when the overview was not fully open yet.
    fn closing_total(&self) -> Duration {
        self.animation.mul_f64(self.closing_from.clamp(0.3, 1.0))
    }
}

impl OverviewPlan {
    /// Whether the overview draws the bottom layer itself, in front of the blur it lays over the
    /// whole monitor. The renderers then leave it out of the layers behind the windows.
    pub fn draws_bottom_layer(&self) -> bool {
        self.wall
            .as_ref()
            .is_some_and(|wall| wall.behind && wall.blur)
    }
}

/// What a renderer needs for one frame of the overview.
pub struct OverviewPlan {
    /// The monitor showing the overview. Others draw as usual.
    pub output: Output,
    /// (window, where to draw it now) - animation included, in the shared space.
    pub thumbs: Vec<(Window, Thumb)>,
    /// Index into `thumbs` of the selected window.
    pub selected: Option<usize>,
    /// Index into `thumbs` of the dragged thumbnail (drawn on top).
    pub dragged: Option<usize>,
    /// Indexes into `thumbs` of the other windows of the container being dragged, drawn right
    /// behind `dragged`. Empty when a single window is.
    dragged_with: Vec<usize>,
    /// The cells of the containers, behind their windows.
    cells: Vec<CellPlan>,
    /// The cell of the container that is selected, in the switcher: it gets the outline.
    selected_cell: Option<Rect>,
    /// How far the cells are shown, 0.0 to 1.0, with the open and close animation.
    cell_alpha: f32,
    /// The fill of a cell, straight alpha (`container-background-color`).
    cell_fill: [f32; 4],
    /// A pixel of the fill's colour, for a fill with rounded corners (`None`: square corners).
    cell_fill_pixel: Option<MemoryRenderBuffer>,
    /// The border of a cell: width and straight-alpha colour (`container-border-*`).
    cell_border: (i32, [f32; 4]),
    /// The border of a workspace square: width and colour, `None` for the built-in colours
    /// (`border`, `border-color`).
    wall_border: (i32, Option<[f32; 4]>),
    /// The colour behind the stage's frames, straight alpha, and the id of its element
    /// (`backdrop-color`).
    backdrop: Option<([f32; 4], Id)>,
    /// Corner radius of a cell (`container-radius`).
    cell_radius: i32,
    /// Corner radius of windows (the `radius` setting), thumbnails get it shrunk with them.
    pub radius: i32,
    /// Which windows get blur (`blur { ignore-window / only-window }`).
    pub blur_windows: config::BlurWindows,
    /// Which layer surfaces get blur, for the panels drawn over the blurred background.
    pub blur_layers: config::BlurLayers,
    /// The search box and the app list. Not in the switcher.
    pub search: Option<SearchPlan>,
    /// How long ago the overview opened while the ripples at the monitor's top-left corner are
    /// still going. `None` in the switcher, while closing, and once they are over.
    ripple: Option<Duration>,
    /// The close button of the selected thumbnail: its index in `thumbs` and its picture, drawn
    /// in `badge_alpha`. `None` when no thumbnail shows one.
    close: Option<(usize, MemoryRenderBuffer)>,
    /// The urgent badge of each thumbnail whose window asked for attention: its index in
    /// `thumbs` and its picture, drawn in `badge_alpha`.
    urgent: Vec<(usize, MemoryRenderBuffer)>,
    /// The app icon badge of each of `thumbs` (same order), `None` for an app that has no app id.
    badges: Vec<Option<MemoryRenderBuffer>>,
    /// Fades the badges with the open and close animation.
    badge_alpha: f32,
    /// Index into `thumbs` of the thumbnail whose icon is at (or moving to or from) its centre,
    /// and how far along it is, 0 (bottom) to 1 (centre).
    badge_carry: Option<(usize, f64)>,
    /// The carried icon's own picture, see `Overview::carry_badges`.
    carry_badge: Option<MemoryRenderBuffer>,
    /// The title of the selected thumbnail, shown below its icon: its index in `thumbs`, its
    /// picture and the picture's size. `None` when the selected thumbnail shows none.
    title: Option<(usize, MemoryRenderBuffer, (i32, i32))>,
    /// The workspace wall, while a thumbnail is dragged and `workspace-wall` is on.
    pub wall: Option<WallPlan>,
    /// Width and colour (straight alpha) of the outline of the selected thumbnail, from the
    /// `overview` block of the config.
    outline: (i32, [f32; 4]),
    highlight: Id,
}

/// The squares of the workspace wall for one frame, in the shared space.
pub struct WallPlan {
    cells: Vec<Rect>,
    ids: Vec<Id>,
    /// The number of each square and where it goes (top-left, shared space). `None` while the
    /// font loads.
    labels: Vec<Option<(MemoryRenderBuffer, Point<i32, Logical>)>>,
    /// The windows of each square's workspace on the overview's monitor, shrunk into the square,
    /// topmost first. Empty lists with `thumbnails` off.
    thumbs: Vec<Vec<(Window, Thumb)>>,
    /// The wallpaper of the overview's monitor (its background layer surfaces and where each
    /// sits on the monitor), drawn shrunk behind the windows of every square.
    wallpaper: Vec<(WlSurface, Point<i32, Logical>)>,
    /// The workspace shown on the overview's monitor.
    current: usize,
    /// The square the pointer is over.
    hover: Option<usize>,
    /// Corner radius of the squares.
    radius: i32,
    /// Drawn behind everything else of the overview: the shown workspace and its neighbours
    /// instead of the squares of a drag.
    behind: bool,
    /// A black pixel, stretched over every square to darken its wallpaper.
    dim: Option<MemoryRenderBuffer>,
    /// Blur what is behind the squares instead of drawing the wallpaper in them.
    blur: bool,
    /// Stable ids of the blur behind each square.
    blur_ids: Vec<Id>,
}

/// Fill of a square of the wall (premultiplied).
const WALL_FILL: [f32; 4] = [0.05, 0.06, 0.10, 0.55];
/// Outline of the square of the workspace that is shown now (straight alpha).
const WALL_CURRENT: [f32; 4] = [0.85, 0.85, 0.90, 0.55];
/// Rim of the frames of the stage (straight alpha), drawn just outside each frame, when
/// `overview { border-color }` is not set. The neighbours' frames have it weaker.
const STAGE_BORDER: [f32; 4] = [0.92, 0.93, 0.98, 0.50];
/// How much of the rim the neighbours' frames get.
const STAGE_PEEK_STRENGTH: f32 = 0.55;
/// Colour of the numbers on the wall.
const WALL_NUMBER: [f32; 3] = [0.95, 0.95, 0.97];
/// Distance between squares, and from the bottom of the usable area.
const WALL_GAP: i32 = 12;
const WALL_BOTTOM_GAP: i32 = 24;
/// Most windows drawn in one square, the topmost ones: every one is drawn again.
const WALL_MAX_THUMBS: usize = 12;

/// The painted search box and app list, placed in the shared space (shadow margin included).
pub struct SearchPlan {
    box_buffer: MemoryRenderBuffer,
    box_pos: Point<i32, Logical>,
    list: Option<(MemoryRenderBuffer, Point<i32, Logical>)>,
    /// Blur behind the box, with the box's rectangle in the shared space (shadow margin not
    /// included). Set when the box is painted translucent.
    box_blur: Option<(Id, Rect)>,
    /// Fades in and out with the overview.
    alpha: f32,
}

/// The app id of `window`, empty when it has none.
pub(crate) fn app_id_of(window: &Window) -> String {
    window
        .toplevel()
        .map(|toplevel| toplevel_names(toplevel.wl_surface()).0)
        .unwrap_or_default()
}

/// What a window's badge is looked up by: its app id and what icon it set itself (`0` for none,
/// see `protocol::toplevel_icon`). Windows of one app share a badge until one sets its own.
fn badge_key(window: &Window) -> (String, u64) {
    let icon = window.toplevel().map_or(0, |toplevel| {
        toplevel_icon::fingerprint(toplevel.wl_surface())
    });
    (app_id_of(window), icon)
}

/// Distance from the bottom edge of the selected thumbnail to its title, from the title to the
/// icon above it, and the room kept free on each side of the title.
const TITLE_BOTTOM_GAP: i32 = 6;
const TITLE_ICON_GAP: i32 = 2;
const TITLE_SIDE_GAP: i32 = 8;
/// Space between the title's text and the edge of the pill behind it, and the pill's colour.
const TITLE_PAD_X: i32 = 8;
const TITLE_PAD_Y: i32 = 3;
const TITLE_FILL: [f32; 3] = [0.110, 0.110, 0.137];
const TITLE_FILL_ALPHA: f32 = 0.78;

impl JffState {
    /// Whether the overview takes input right now (not while it's only animating closed).
    pub fn overview_active(&self) -> bool {
        self.overview
            .as_ref()
            .is_some_and(|ov| ov.closing.is_none())
    }

    /// Serial of the pointer grab the open overview holds, to tell it from somebody else's.
    pub fn overview_grab_serial(&self) -> Option<Serial> {
        self.overview
            .as_ref()
            .filter(|ov| ov.closing.is_none())
            .map(|ov| ov.grab_serial)
    }

    /// Give the open overview its pointer grab back after a menu's grab replaced it and ended.
    /// Without it nothing selects thumbnails under the pointer any more. A grab that is still
    /// there (the menu's, a move) is left alone.
    pub fn restore_overview_grab(&mut self) {
        let Some(serial) = self.overview_grab_serial() else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return;
        }
        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: pointer.current_location(),
        };
        pointer.set_grab(self, OverviewGrab::new(start_data), serial, Focus::Clear);
    }

    /// Where the pointer may go: every monitor, also while the overview is open. The overview
    /// stays on its own monitor, other monitors keep their panels and draw as usual.
    pub fn pointer_bounds(&self) -> Vec<Rect> {
        self.output_rects()
    }

    /// The neighbours of the workspace shown: for each side, `-1` or `1`, its workspace and
    /// where its frame is, in the shared space, empty workspaces included. Empty in the
    /// switcher, while typing and while closing.
    fn overview_peeks(&self) -> Vec<(i32, usize, Rect)> {
        let Some(ov) = self
            .overview
            .as_ref()
            .filter(|ov| ov.switcher.is_none() && !ov.search.active() && ov.closing.is_none())
        else {
            return Vec::new();
        };
        let stage = stage_rect(output_rect(&ov.output));
        let name = ov.output.name();
        let current = self.desk.current_on(&name);
        let count = self.workspace_count();
        [-1, 1]
            .into_iter()
            .filter_map(|dir| {
                let target = current
                    .checked_add_signed(dir as isize)
                    .filter(|t| *t < count)?;
                Some({
                    let cell = (
                        stage.0 + dir * (stage.2 + STAGE_GAP),
                        stage.1,
                        stage.2,
                        stage.3,
                    );
                    (dir, target, cell)
                })
            })
            .collect()
    }

    /// The side (`-1` left, `1` right) of the neighbouring workspace under `p`, if any.
    fn overview_peek_at(&self, p: Point<f64, Logical>) -> Option<i32> {
        self.overview_peeks()
            .into_iter()
            .find(|(_, _, cell)| layout::contains(*cell, (p.x, p.y)))
            .map(|(dir, _, _)| dir)
    }

    /// The frames of the workspace shown and of its neighbours, shifted sideways by `pan`: the
    /// wallpaper and, in the neighbours, their windows, shrunk into the frames. The first
    /// frame is the shown workspace, whose windows are the overview's own. `None` in the
    /// switcher and while typing, and while closing the frames are gone.
    fn stage_wall(&mut self, pan: i32) -> Option<WallPlan> {
        let ov = self.overview.as_ref()?;
        if ov.switcher.is_some() || ov.search.active() {
            return None;
        }
        let output = ov.output.clone();
        let blur = self.config.overview().blur;
        let closing = ov.closing.is_some();
        let monitor = output_rect(&output);
        let peeks = self.overview_peeks();
        let shift = |cell: Rect| (cell.0 + pan, cell.1, cell.2, cell.3);
        // Closing takes every frame away at once, only the darkening fades with the windows.
        let mut cells = if closing {
            Vec::new()
        } else {
            vec![shift(stage_rect(monitor))]
        };
        cells.extend(peeks.iter().map(|(_, _, cell)| shift(*cell)));
        let mut thumbs: Vec<Vec<(Window, Thumb)>> = cells.iter().map(|_| Vec::new()).collect();
        for entry in self.desk.all_top_down() {
            let Some(i) = peeks
                .iter()
                .position(|(_, workspace, _)| *workspace == entry.workspace)
            else {
                continue;
            };
            let size = entry.window.geometry().size;
            if thumbs[i + 1].len() >= WALL_MAX_THUMBS
                || size.w <= 0
                || size.h <= 0
                || self.rect_for_window(entry.pos, (size.w, size.h)) != monitor
            {
                continue;
            }
            let thumb = overview::wall_thumb(cells[i + 1], monitor, entry.pos, (size.w, size.h));
            thumbs[i + 1].push((entry.window.clone(), thumb));
        }
        let wallpaper: Vec<(WlSurface, Point<i32, Logical>)> = {
            let map = layer_map_for_output(&output);
            log_wallpaper_layers(&map);
            map.layers_on(Layer::Background)
                .filter_map(|layer| {
                    let geo = map.layer_geometry(layer)?;
                    Some((layer.wl_surface().clone(), geo.loc))
                })
                .collect()
        };
        let ov = self.overview.as_mut()?;
        while ov.wall_ids.len() < cells.len() {
            ov.wall_ids.push(Id::new());
        }
        while ov.stage_blur_ids.len() < cells.len() {
            ov.stage_blur_ids.push(Id::new());
        }
        let blur_ids = ov.stage_blur_ids.clone();
        let dim = ov
            .stage_dim
            .get_or_insert_with(|| {
                MemoryRenderBuffer::from_slice(
                    &[0, 0, 0, 255],
                    Fourcc::Abgr8888,
                    (1, 1),
                    1,
                    Transform::Normal,
                    None,
                )
            })
            .clone();
        Some(WallPlan {
            ids: ov.wall_ids.clone(),
            labels: Vec::new(),
            thumbs,
            wallpaper,
            current: usize::MAX,
            hover: None,
            radius: STAGE_RADIUS,
            behind: true,
            dim: Some(dim),
            blur,
            blur_ids,
            cells,
        })
    }

    /// Slide the overview `steps` workspaces left (`steps < 0`) or right (`steps > 0`), empty
    /// ones included, stopping at the first and the last.
    fn overview_navigate(&mut self, steps: i32) {
        let Some(output) = self.overview.as_ref().map(|ov| ov.output.clone()) else {
            return;
        };
        let name = output.name();
        // What is left of the slide still running: the next one starts from there, so the
        // view never jumps back to the start of a slide.
        let carried = self.overview.as_ref().map_or(0, Overview::pan_offset);
        let before = self.desk.current_on(&name);
        let last = self.workspace_count().max(1) - 1;
        let target = before.saturating_add_signed(steps as isize).min(last);
        if target == before {
            return;
        }
        let distance = f64::from(stage_rect(output_rect(&output)).2 + STAGE_GAP);
        let at = self.seat.get_pointer().map(|p| p.current_location());
        self.switch_workspace_on(&name, target);
        // With the overview gone for a moment the new workspace's topmost window takes the
        // keyboard and `warp-mouse-to-focus` moves the pointer to it, clamped to the nearest
        // monitor's edge when that window is out of view. The pointer stays where it was.
        if let Some(at) = at {
            self.move_cursor((at.x, at.y));
        }
        self.open_overview_with(true, Some(output));
        if let Some(ov) = self.overview.as_mut() {
            ov.opened = Instant::now()
                .checked_sub(ov.animation)
                .unwrap_or_else(Instant::now);
            ov.pan = Some((
                Instant::now(),
                f64::from(carried) + f64::from(steps.signum()) * distance,
            ));
        }
    }

    /// The squares of the workspace wall on the overview's monitor, in the shared space. Empty
    /// when `workspace-wall` is off, in the switcher, or with a single workspace.
    fn wall_cells(&self) -> Vec<Rect> {
        let Some(ov) = self.overview.as_ref().filter(|ov| ov.switcher.is_none()) else {
            return Vec::new();
        };
        let Some(wall) = self.config.workspace_wall() else {
            return Vec::new();
        };
        let count = self.workspace_count();
        if count < 2 {
            return Vec::new();
        }
        let rect = output_rect(&ov.output);
        overview::wall(
            count,
            self.panel_free_rect(rect),
            (rect.2, rect.3),
            wall.height,
            WALL_GAP,
            WALL_BOTTOM_GAP,
        )
    }

    /// The number of every square of the wall, painted when the text size changes. Centered in
    /// its square. `None` for all of them while the font loads.
    fn wall_labels(
        &mut self,
        cells: &[Rect],
    ) -> Vec<Option<(MemoryRenderBuffer, Point<i32, Logical>)>> {
        let fonts = self.osd.fonts();
        let Some(ov) = self.overview.as_mut() else {
            return cells.iter().map(|_| None).collect();
        };
        ov.wall_labels.resize_with(cells.len(), || None);
        let Some(fonts) = fonts else {
            return cells.iter().map(|_| None).collect();
        };
        cells
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let px = (cell.3 * 2 / 5).clamp(12, 64);
                let stale = ov.wall_labels[i]
                    .as_ref()
                    .map_or(true, |(had, _, _)| *had != px);
                if stale {
                    let picture = paint_number(fonts, i + 1, px as f32);
                    let size = (picture.width, picture.height);
                    ov.wall_labels[i] = Some((px, buffer_of(&picture), size));
                }
                ov.wall_labels[i].as_ref().map(|(_, buffer, size)| {
                    (
                        buffer.clone(),
                        Point::from((
                            cell.0 + (cell.2 - size.0) / 2,
                            cell.1 + (cell.3 - size.1) / 2,
                        )),
                    )
                })
            })
            .collect()
    }

    /// The windows of every workspace shrunk into its square of the wall, topmost first, the
    /// one being dragged too (it is in its square until dropped elsewhere). Windows keep their
    /// place while their workspace is hidden, and they get no frame callbacks then, so what is
    /// drawn is the last thing they showed.
    fn wall_thumbs(&self, cells: &[Rect]) -> Vec<Vec<(Window, Thumb)>> {
        let mut out: Vec<Vec<(Window, Thumb)>> = cells.iter().map(|_| Vec::new()).collect();
        let Some(ov) = self.overview.as_ref() else {
            return out;
        };
        if !self
            .config
            .workspace_wall()
            .is_some_and(|wall| wall.thumbnails)
        {
            return out;
        }
        let monitor = output_rect(&ov.output);
        for entry in self.desk.all_top_down() {
            let Some(list) = out.get_mut(entry.workspace) else {
                continue;
            };
            let size = entry.window.geometry().size;
            if list.len() >= WALL_MAX_THUMBS
                || size.w <= 0
                || size.h <= 0
                || self.rect_for_window(entry.pos, (size.w, size.h)) != monitor
            {
                continue;
            }
            let thumb =
                overview::wall_thumb(cells[entry.workspace], monitor, entry.pos, (size.w, size.h));
            list.push((entry.window.clone(), thumb));
        }
        out
    }

    /// The tiling containers the overview shows as one cell each: those with all their windows
    /// among the windows it shows. None in the switcher, which is a row of windows, nor while
    /// something is typed, which picks windows and not containers.
    fn overview_groups(&self) -> Vec<Group> {
        let grouped = self.config.switcher().group_containers;
        let Some(ov) = self
            .overview
            .as_ref()
            .filter(|ov| (ov.switcher.is_none() || grouped) && !ov.search.active())
        else {
            return Vec::new();
        };
        let mut groups = Vec::new();
        for container in self.desk.containers() {
            let members = self.desk.members(container.id());
            let leaves: Vec<(Window, Rect)> = members
                .iter()
                .map(|entry| {
                    let size = entry.window.geometry().size;
                    (
                        entry.window.clone(),
                        (entry.pos.0, entry.pos.1, size.w, size.h),
                    )
                })
                .collect();
            if leaves.is_empty() || !leaves.iter().all(|(leaf, _)| ov.visible.contains(leaf)) {
                continue;
            }
            let rects: Vec<Rect> = leaves.iter().map(|(_, rect)| *rect).collect();
            let Some(area) = container.area().or_else(|| container_view::bounds(&rects)) else {
                continue;
            };
            let focused = self
                .desk
                .find_by_id(container.focused())
                .map(|entry| entry.window.clone())
                .unwrap_or_else(|| leaves[0].0.clone());
            groups.push(Group {
                id: container.id(),
                label: container_view::label(container.name(), &app_id_of(&leaves[0].0)),
                area,
                leaves,
                focused,
            });
        }
        groups
    }

    /// Where the cell of every container in `groups` is in the grid, without the open and close
    /// animation: the index in `groups` and the cell.
    fn overview_cells(&self, groups: &[Group]) -> Vec<(usize, Thumb)> {
        let Some(ov) = self.overview.as_ref() else {
            return Vec::new();
        };
        let cells = overview_arrangement(ov, self.config.switcher().thumbnail_height, groups).1;
        if ov.switcher.is_none() {
            return cells;
        }
        // In the switcher the row slides, and the cells with it.
        let dx = ov.slide.now().round() as i32;
        cells
            .into_iter()
            .map(|(g, cell)| (g, overview::shifted(cell, dx)))
            .collect()
    }

    /// The cells to draw, the carried container's one where `carried` puts it: the
    /// outline and fill, and the name painted at a size that fits the strip above the windows.
    /// The name is painted again only when its text or size changes.
    fn cell_plans(
        &mut self,
        groups: &[Group],
        cells: &[(usize, Thumb)],
        carried: Option<(ContainerId, Thumb)>,
    ) -> Vec<CellPlan> {
        let fonts = self.osd.fonts();
        let font_color = self.config.overview().container_font_color;
        let Some(ov) = self.overview.as_mut() else {
            return Vec::new();
        };
        while ov.cell_ids.len() < cells.len() {
            ov.cell_ids.push(Id::new());
        }
        while ov.cell_blur_ids.len() < cells.len() {
            ov.cell_blur_ids.push(Id::new());
        }
        if ov.cell_labels.len() > MAX_CELL_LABELS {
            ov.cell_labels.clear();
        }
        let mut plans = Vec::with_capacity(cells.len());
        for (n, (g, cell)) in cells.iter().enumerate() {
            let group = &groups[*g];
            let moved = carried
                .filter(|(id, _)| *id == group.id)
                .map_or(*cell, |(_, moved)| moved);
            let strip = container_view::label_rect(moved);
            let px = (strip.3 * 3 / 5).clamp(10, 28);
            let label = fonts.filter(|_| !group.label.is_empty()).and_then(|fonts| {
                let text = fit_text(fonts, &group.label, px as f32, strip.2 as f32);
                if text.is_empty() {
                    return None;
                }
                let key = (text, px);
                if !ov.cell_labels.contains_key(&key) {
                    let picture = paint_text(fonts, &key.0, px as f32, font_color);
                    let size = (picture.width, picture.height);
                    ov.cell_labels
                        .insert(key.clone(), (buffer_of(&picture), size));
                }
                ov.cell_labels.get(&key).map(|(buffer, size)| {
                    (
                        buffer.clone(),
                        Point::from((strip.0, strip.1 + (strip.3 - size.1) / 2)),
                    )
                })
            });
            plans.push(CellPlan {
                rect: moved.rect(),
                label,
                id: ov.cell_ids[n].clone(),
                blur_id: ov.cell_blur_ids[n].clone(),
            });
        }
        plans
    }

    /// Whether a thumbnail is being dragged.
    fn overview_dragging(&self) -> bool {
        self.overview.as_ref().is_some_and(|ov| ov.drag.is_some())
    }

    /// Window id of the window whose thumbnail is being dragged in the overview (for the `drag`
    /// watch event). Only set once the pointer moved past the click threshold.
    pub fn overview_dragged_view(&self) -> Option<u64> {
        let drag = self.overview.as_ref()?.drag.as_ref()?;
        drag.window
            .user_data()
            .get::<crate::ipc::WindowId>()
            .map(|id| id.0)
    }

    /// Once per frame, before drawing: finish a closed overview and follow windows that were
    /// created or destroyed while it's open.
    fn overview_tick(&mut self) {
        let Some(ov) = self.overview.as_ref() else {
            return;
        };
        if ov
            .closing
            .is_some_and(|since| since.elapsed() >= ov.closing_total())
        {
            self.overview = None;
            return;
        }
        // (A window created meanwhile can't take the keyboard: while the overview is open the
        // keyboard focus is the overview's - see `focus.rs`.)
        let Some(ov) = self.overview.as_ref() else {
            return;
        };
        let present = self.windows_on(output_rect(&ov.output));

        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        let old_index = ov
            .selected
            .as_ref()
            .and_then(|s| ov.windows.iter().position(|w| w == s));
        ov.windows.retain(|w| present.contains(w));
        for window in present {
            if !ov.windows.contains(&window) {
                ov.windows.push(window);
            }
        }
        // The selected window is gone: take the one that moved into its place.
        let selected_alive = ov.selected.as_ref().is_some_and(|s| ov.windows.contains(s));
        if !selected_alive {
            let last = ov.windows.len().saturating_sub(1);
            ov.selected = ov.windows.get(old_index.unwrap_or(0).min(last)).cloned();
        }
        ov.update_visible();
        // The switcher has nothing to show without windows. The overview stays, you may be
        // here to start an app.
        let nothing_left = ov.windows.is_empty() && ov.switcher.is_some() && ov.closing.is_none();
        if nothing_left {
            self.abort_overview();
            return;
        }
        // Follow the results: a finished scan of the apps, a title that changed.
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| ov.closing.is_none())
        {
            let version = self.app_index.version();
            let apps_changed = self
                .overview
                .as_ref()
                .is_some_and(|ov| ov.search.version != version);
            self.refresh_search(false, apps_changed);
        }
    }
}

/// The number `n` of a workspace, `px` tall, on a transparent canvas just big enough for it.
fn paint_number(fonts: &Fonts, n: usize, px: f32) -> Picture {
    let [r, g, b] = WALL_NUMBER;
    paint_text(fonts, &n.to_string(), px, [r, g, b, 1.0])
}

/// `text` shortened with an ellipsis until it is at most `max_width` px wide at `px`.
fn fit_text(fonts: &Fonts, text: &str, px: f32, max_width: f32) -> String {
    let mut text: Vec<char> = text.chars().collect();
    let shown = |chars: &[char], ellipsis: bool| {
        let mut out: String = chars.iter().collect();
        if ellipsis {
            out.push('\u{2026}');
        }
        out
    };
    if text_width(&fonts.medium, &shown(&text, false), px) <= max_width {
        return shown(&text, false);
    }
    while !text.is_empty() {
        text.pop();
        let candidate = shown(&text, true);
        if text_width(&fonts.medium, &candidate, px) <= max_width {
            return candidate;
        }
    }
    String::new()
}

/// `text`, `px` tall, on a transparent canvas just big enough for it.
fn paint_text(fonts: &Fonts, text: &str, px: f32, color: [f32; 4]) -> Picture {
    let text = text.to_owned();
    let width = text_width(&fonts.medium, &text, px).ceil() as i32 + 4;
    let height = (px * 1.4).ceil() as i32;
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (width * height) as usize];
    let baseline = height as f32 / 2.0 + half_line(&fonts.medium, px);
    draw_text(
        &mut canvas,
        width,
        height,
        &fonts.medium,
        &text,
        px,
        2.0,
        baseline,
        [color[0], color[1], color[2]],
    );
    if color[3] < 1.0 {
        for pixel in &mut canvas {
            for channel in pixel.iter_mut() {
                *channel *= color[3];
            }
        }
    }
    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for pixel in &canvas {
        for channel in pixel {
            rgba.push((channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Picture {
        rgba,
        width,
        height,
    }
}

/// `text` for a thumbnail's title, `px` tall: white on a dark translucent pill so it reads on
/// any window, light ones included.
fn paint_title(fonts: &Fonts, text: &str, px: f32) -> Picture {
    let width = text_width(&fonts.medium, text, px).ceil() as i32 + 2 * TITLE_PAD_X;
    let height = (px * 1.4).ceil() as i32 + 2 * TITLE_PAD_Y;
    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (width * height) as usize];
    let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
    for y in 0..height {
        for x in 0..width {
            let d = sd_round_box(
                x as f32 + 0.5,
                y as f32 + 0.5,
                half_w,
                half_h,
                half_w,
                half_h,
                half_h,
            );
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                over(
                    &mut canvas[(y * width + x) as usize],
                    TITLE_FILL,
                    cover * TITLE_FILL_ALPHA,
                );
            }
        }
    }
    let baseline = half_h + half_line(&fonts.medium, px);
    draw_text(
        &mut canvas,
        width,
        height,
        &fonts.medium,
        text,
        px,
        TITLE_PAD_X as f32,
        baseline,
        [1.0; 3],
    );
    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for pixel in &canvas {
        for channel in pixel {
            rgba.push((channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Picture {
        rgba,
        width,
        height,
    }
}

/// A painted picture as a buffer to draw.
fn buffer_of(picture: &crate::feature::osd::Picture) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &picture.rgba,
        Fourcc::Abgr8888,
        (picture.width, picture.height),
        1,
        Transform::Normal,
        None,
    )
}

/// Debug log of what the overview takes as wallpaper (the background layer surfaces) and what
/// it draws again in front of the blur (the bottom layer), only when that list changes.
fn log_wallpaper_layers(map: &smithay::desktop::LayerMap) {
    thread_local! {
        static LAST: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    }
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let mut line = String::new();
    for layer in [Layer::Background, Layer::Bottom] {
        for surface in map.layers_on(layer) {
            line.push_str(&format!(
                "{layer:?} {:?} at {:?}; ",
                surface.namespace(),
                map.layer_geometry(surface)
            ));
        }
    }
    LAST.with(|last| {
        if *last.borrow() != line {
            tracing::debug!("overview wallpaper and bottom layers: {line}");
            *last.borrow_mut() = line;
        }
    });
}

/// The frame of the workspace shown in the overview on the monitor at `monitor`: the monitor's
/// shape, centered, narrow enough for its neighbours to peek out on both sides.
fn stage_rect(monitor: Rect) -> Rect {
    let (mw, mh) = (i64::from(monitor.2.max(1)), i64::from(monitor.3.max(1)));
    let by_width = mw * i64::from(STAGE_PERCENT) / 100;
    let by_height = (mh - i64::from(2 * STAGE_MARGIN)).max(1) * mw / mh;
    let w = by_width.min(by_height).max(1);
    let h = (w * mh / mw).max(1);
    (
        monitor.0 + ((mw - w) / 2) as i32,
        monitor.1 + ((mh - h) / 2) as i32,
        w as i32,
        h as i32,
    )
}

/// Where the overview puts its windows, before the switcher's slide: the grid, or in the
/// switcher a row of fixed-size thumbnails that starts at the left edge. One thumbnail per
/// visible window, the windows of a container (`groups`) inside the container's cell.
fn overview_layout(ov: &Overview, thumbnail_height: Option<i32>, groups: &[Group]) -> Vec<Thumb> {
    overview_arrangement(ov, thumbnail_height, groups).0
}

/// [`overview_layout`], and the cell of every group in `groups` that is shown: its index there
/// and where its cell is.
fn overview_arrangement(
    ov: &Overview,
    thumbnail_height: Option<i32>,
    groups: &[Group],
) -> (Vec<Thumb>, Vec<(usize, Thumb)>) {
    let rect = output_rect(&ov.output);
    let mut area = if ov.switcher.is_none() && !ov.search.active() {
        layout::grow(stage_rect(rect), -STAGE_PAD)
    } else {
        layout::grow(rect, -MARGIN)
    };
    if ov.switcher.is_none() && ov.search.active() {
        // Room above the thumbnails for the search box and, under it, the app list.
        let wanted = search_layout(ov);
        let push_down = (rect.1 + wanted.top) - area.1;
        if push_down > 0 {
            area.1 += push_down;
            area.3 -= push_down;
        }
        area.3 = (area.3 - wanted.bottom).max(1);
    }
    let window_size = |w: &Window| {
        let size = w.geometry().size;
        (size.w, size.h)
    };
    // The grid, or the switcher's row, holds items: a window, or a container with all its windows in one cell.
    enum Item {
        Window(usize),
        Group(usize),
    }
    let mut items: Vec<Item> = Vec::new();
    let mut seen = vec![false; groups.len()];
    for (i, window) in ov.visible.iter().enumerate() {
        match groups
            .iter()
            .position(|group| group.leaves.iter().any(|(leaf, _)| leaf == window))
        {
            Some(g) if !seen[g] => {
                seen[g] = true;
                items.push(Item::Group(g));
            }
            Some(_) => {}
            None => items.push(Item::Window(i)),
        }
    }
    let sizes: Vec<Size> = items
        .iter()
        .map(|item| match item {
            Item::Window(i) => window_size(&ov.visible[*i]),
            Item::Group(g) => {
                let area = groups[*g].area;
                container_view::cell_size((area.2, area.3))
            }
        })
        .collect();
    let slots = if ov.switcher.is_some() {
        overview::strip(&sizes, area, GAP, thumbnail_height.unwrap_or(area.3 / 3))
    } else {
        overview::arrange(&sizes, area, GAP)
    };

    let nowhere = Thumb {
        pos: (0, 0),
        size: (1, 1),
        scale: 1.0,
    };
    let mut thumbs = vec![nowhere; ov.visible.len()];
    let mut cells = Vec::new();
    for (item, slot) in items.iter().zip(slots) {
        match item {
            Item::Window(i) => thumbs[*i] = slot,
            Item::Group(g) => {
                let group = &groups[*g];
                let rects: Vec<Rect> = group.leaves.iter().map(|(_, rect)| *rect).collect();
                let inside = container_view::leaf_thumbs(slot, group.area, &rects);
                for ((leaf, _), thumb) in group.leaves.iter().zip(inside) {
                    if let Some(i) = ov.visible.iter().position(|w| w == leaf) {
                        thumbs[i] = thumb;
                    }
                }
                cells.push((*g, slot));
            }
        }
    }
    (thumbs, cells)
}

/// What is selected now, as a position in the lists as shown.
fn current_pick(ov: &Overview) -> Option<Pick> {
    if let Some(row) = ov.search.app_selected {
        return Some(Pick::App(row));
    }
    let selected = ov.selected.as_ref()?;
    ov.visible
        .iter()
        .position(|w| w == selected)
        .map(Pick::Window)
}

/// Select `pick`. A window pick leaves the app list, an app pick leaves the window selection
/// as it is (it is just not shown).
fn apply_pick(ov: &mut Overview, pick: Option<Pick>) {
    match pick {
        Some(Pick::Window(i)) => {
            if let Some(window) = ov.visible.get(i) {
                ov.selected = Some(window.clone());
            }
            ov.search.app_selected = None;
        }
        Some(Pick::App(row)) => ov.search.app_selected = Some(row),
        None => ov.search.app_selected = None,
    }
}

/// Where the search box and the app list go, and how much room they take from the thumbnails.
struct SearchLayout {
    /// The box, in the shared space.
    box_rect: Rect,
    /// The app list, when there are apps to list.
    list_rect: Option<Rect>,
    /// How far down from the top of the monitor the thumbnails may start.
    top: i32,
    /// How much of the thumbnails' area is taken from the bottom. Nothing now that the app list
    /// is above them.
    bottom: i32,
}

fn search_layout(ov: &Overview) -> SearchLayout {
    let rect = output_rect(&ov.output);
    // Below any panel, like the toasts.
    let zone = layer_map_for_output(&ov.output).non_exclusive_zone();
    let (zone_x, zone_y, zone_w) = if zone.size.w > 0 && zone.size.h > 0 {
        (zone.loc.x, zone.loc.y, zone.size.w)
    } else {
        (0, 0, rect.2)
    };
    let width = search_ui::BOX_WIDTH.min(zone_w - 80).max(200);
    let x = rect.0 + zone_x + (zone_w - width) / 2;
    let y = rect.1 + zone_y + search_ui::TOP_OFFSET;
    let box_rect = (x, y, width, search_ui::BOX_HEIGHT);

    let count = ov.search.apps.len().min(apps::MAX_RESULTS);
    let (list_width, list_height) = search_ui::grid_size(count, zone_w - 80);
    let rows = count;
    let list_rect = (rows > 0).then(|| {
        // Always right under the box, with the thumbnails below it.
        let top = y + search_ui::BOX_HEIGHT + search_ui::GAP;
        let list_x = rect.0 + zone_x + (zone_w - list_width) / 2;
        (list_x, top, list_width, list_height)
    });
    SearchLayout {
        box_rect,
        list_rect,
        top: zone_y
            + search_ui::TOP_OFFSET
            + search_ui::BOX_HEIGHT
            + if rows > 0 {
                search_ui::GAP + list_height
            } else {
                0
            }
            + 16,
        bottom: 0,
    }
}

/// The slide that puts the switcher's selected window in the middle of the screen.
fn switcher_target(ov: &Overview, thumbnail_height: Option<i32>, groups: &[Group]) -> i32 {
    let area = layout::grow(output_rect(&ov.output), -MARGIN);
    let selected = ov
        .selected
        .as_ref()
        .and_then(|s| ov.visible.iter().position(|w| w == s))
        .unwrap_or(0);
    let (mut thumbs, cells) = overview_arrangement(ov, thumbnail_height, groups);
    // A container is one stop: its cell goes in the middle, not the window of it selected.
    let cell = ov
        .selected
        .as_ref()
        .and_then(|s| {
            groups
                .iter()
                .position(|group| group.leaves.iter().any(|(leaf, _)| leaf == s))
        })
        .and_then(|g| cells.iter().find(|(index, _)| *index == g));
    if let (Some((_, cell)), Some(thumb)) = (cell, thumbs.get_mut(selected)) {
        *thumb = *cell;
    }
    overview::strip_offset(&thumbs, selected, area)
}

/// Owns the pointer while the overview is open: windows see nothing, layer surfaces (panels,
/// widgets) still do, motion moves the selection.
pub struct OverviewGrab {
    start_data: GrabStartData<JffState>,
}

impl OverviewGrab {
    pub fn new(start_data: GrabStartData<JffState>) -> Self {
        Self { start_data }
    }
}

impl PointerGrab<JffState> for OverviewGrab {
    fn motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Panels and layer surfaces still get the pointer, windows don't.
        let focus = data.overview_layer_under(event.location);
        handle.motion(data, focus, event);
        data.overview_hover(event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        _focus: Option<(<JffState as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    // Buttons are handled in `input.rs` before they reach the pointer. Anything that gets here
    // has no focus to go to.
    fn button(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
    }

    fn axis(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut JffState, handle: &mut PointerInnerHandle<'_, JffState>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }
    fn gesture_swipe_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }
    fn gesture_swipe_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }
    fn gesture_pinch_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }
    fn gesture_pinch_update(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }
    fn gesture_pinch_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }
    fn gesture_hold_begin(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }
    fn gesture_hold_end(
        &mut self,
        data: &mut JffState,
        handle: &mut PointerInnerHandle<'_, JffState>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<JffState> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut JffState) {}
}

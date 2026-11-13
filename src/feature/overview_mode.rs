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
use crate::desk::WindowMode;
use crate::feature::osd::{
    draw_text, half_line, text_width, toplevel_names, Fonts, Picture, Pixel, SHADOW,
};
use crate::feature::outputs::output_rect;
use crate::feature::overview_search::{self as search_ui, Row};
use crate::input::{BTN_LEFT, BTN_MIDDLE};
use crate::primitives::app_search::{self as apps, DesktopEntry, Pick, Query};
use crate::primitives::geom::Size;
use crate::primitives::layout::{self, Rect};
use crate::primitives::overview_grid::{self as overview, Dir, Held, Thumb};
use crate::protocol::layer_shell::{ABOVE_WINDOWS, BELOW_WINDOWS};
use crate::protocol::toplevel_icon;
use crate::render::effects::background_effect::render_for_surface_zoomed;
use crate::render::effects::framebuffer_effect::WallpaperSnapshot;
use crate::render::effects::gamut;
use crate::render::effects::rounded::{is_fullscreen, ring_element, Rounding};
use crate::render::elements::JffElement;
use crate::state::JffState;

/// Free space around the thumbnails so docks and panels stay visible.
const MARGIN: i32 = 150;
/// Space between thumbnails.
const GAP: i32 = 24;
/// Open and close animation length.
const ANIMATION: Duration = Duration::from_millis(180);
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
    /// Where the pointer grabbed it, as fractions of width and height.
    anchor: (f64, f64),
}

/// The overview, while open or animating closed.
pub struct Overview {
    output: Output,
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
    /// Set once we leave: input released, animation runs backwards.
    closing: Option<Instant>,
    /// Stable id of the highlight, so damage tracking sees it move rather than reappear.
    highlight: Id,
    /// Stable ids of the fills of the workspace wall's squares, one per square.
    wall_ids: Vec<Id>,
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
                1.0 - overview::ease_out_cubic(overview::progress(since.elapsed(), ANIMATION))
            }
            None => overview::ease_out_cubic(overview::progress(self.opened.elapsed(), ANIMATION)),
        }
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
    /// Corner radius of windows (the `radius` setting), thumbnails get it shrunk with them.
    pub radius: i32,
    /// Which windows get blur (`blur { ignore-window / only-window }`).
    pub blur_windows: config::BlurWindows,
    /// The search box and the app list. Not in the switcher.
    pub search: Option<SearchPlan>,
    /// The app icon badge of each of `thumbs` (same order), `None` for an app that has no app id.
    badges: Vec<Option<MemoryRenderBuffer>>,
    /// Fades the badges with the open and close animation.
    badge_alpha: f32,
    /// Index into `thumbs` of the thumbnail whose icon is at (or moving to or from) its centre,
    /// and how far along it is, 0 (bottom) to 1 (centre).
    badge_carry: Option<(usize, f64)>,
    /// The carried icon's own picture, see `Overview::carry_badges`.
    carry_badge: Option<MemoryRenderBuffer>,
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
}

/// Fill of a square of the wall (premultiplied).
const WALL_FILL: [f32; 4] = [0.05, 0.06, 0.10, 0.55];
/// Outline of the square of the workspace that is shown now (straight alpha).
const WALL_CURRENT: [f32; 4] = [0.85, 0.85, 0.90, 0.55];
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

/// Push the app icon badge of thumbnail `index`: at the bottom centre of it, scaled with it. Call
/// before the thumbnail's own elements, which come after what is in front of them.
fn badge_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    index: usize,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let (Some(Some(buffer)), Some((_, thumb))) = (plan.badges.get(index), plan.thumbs.get(index))
    else {
        return;
    };
    let carried = plan.badge_carry.is_some_and(|(n, _)| n == index);
    let buffer = match (&plan.carry_badge, carried) {
        (Some(own), true) => own,
        _ => buffer,
    };
    let Some((x, y, full, _)) = overview::badge_rect(thumb.rect()) else {
        return;
    };
    // While carried the icon shrinks (to `CARRY_ICON_SCALE`) and moves to the centre.
    let mix = plan
        .badge_carry
        .filter(|(n, _)| *n == index)
        .map_or(0.0, |(_, mix)| mix);
    let side = ((f64::from(full) * (1.0 - (1.0 - CARRY_ICON_SCALE) * mix)).round() as i32).max(1);
    let x = x + (full - side) / 2;
    let centre = thumb.pos.1 + (thumb.size.1 - side) / 2;
    let y = y + (full - side) / 2;
    let y = y + (f64::from(centre - y) * mix).round() as i32;
    // The picture has a shadow margin around the badge, scaled by the same factor.
    let margin = (f64::from(search_ui::BADGE_MARGIN) * f64::from(side)
        / f64::from(search_ui::BADGE_BODY))
    .round() as i32;
    // The picture is drawn at its own size and shrunk or grown by a rescale element: a scaled
    // memory element has its damage in picture pixels, so only the top-left part got redrawn.
    let corner = Point::<i32, Physical>::from((x - margin - origin.x, y - margin - origin.y));
    let zoom = f64::from(side + 2 * margin) / f64::from(search_ui::BADGE_PICTURE);
    match MemoryRenderBufferRenderElement::from_buffer(
        &mut *renderer,
        corner.to_f64(),
        buffer,
        Some(plan.badge_alpha),
        None,
        None,
        Kind::Unspecified,
    ) {
        Ok(element) => out.push(JffElement::ScaledMemory(
            RescaleRenderElement::from_element(element, corner, zoom),
        )),
        Err(err) => tracing::warn!("error drawing an app icon in the overview: {err:?}"),
    }
}

/// Push the render elements for one thumbnail, placed relative to `origin`.
fn thumb_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    thumb: &Thumb,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    radius: i32,
    out: &mut Vec<JffElement>,
    snapshot: Option<&WallpaperSnapshot>,
) {
    let scale = Scale::from(1.0f64);
    // Draw the window at (0, 0) with its geometry corner, shrink it around that corner, then
    // move the result to its slot.
    let geo = window.geometry();
    let loc: Point<i32, Physical> =
        Point::<i32, Logical>::from((-geo.loc.x, -geo.loc.y)).to_physical_precise_round(scale);
    let slot: Point<i32, Physical> =
        Point::<i32, Logical>::from((thumb.pos.0 - origin.x, thumb.pos.1 - origin.y))
            .to_physical_precise_round(scale);
    // The thumbnail's corners shrink with it: its rectangle on screen and the radius times the
    // scale. Fullscreen windows are square, as they are live.
    let rounding = if is_fullscreen(window) {
        None
    } else {
        Rounding::new(
            renderer,
            Rectangle::new(
                (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
                (thumb.size.0, thumb.size.1).into(),
            ),
            (f64::from(radius) * thumb.scale).round() as i32,
        )
    };
    // Alpha 1.0 is the thumbnail's own: a client's fade (wp_alpha_modifier_v1) is applied per
    // surface on top, so a faded window stays faded here, as it is what the user sees live.
    // A thumbnail shows the colors of the live window: converted to the monitor's like it.
    let conversions = match window.toplevel() {
        Some(toplevel) => gamut::tree_conversions(toplevel.wl_surface()),
        None => Vec::new(),
    };
    let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        window.render_elements(&mut *renderer, loc, scale, 1.0);
    for surface in surfaces {
        let conversion = gamut::find(&conversions, surface.id());
        let shrunk = RescaleRenderElement::from_element(
            surface,
            Point::<i32, Physical>::from((0, 0)),
            thumb.scale,
        );
        let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
        out.push(
            match gamut::masked(&mut *renderer, rounding.as_ref(), conversion) {
                Some(mask) => mask.wrap_thumb(placed),
                None => JffElement::Thumb(placed),
            },
        );
    }
    // The blur goes right behind the thumbnail: after its elements in the list. The window's
    // origin sits at the slot minus its geometry offset, both shrunk with the thumbnail.
    if let (Some(blur), Some(toplevel)) = (blur, window.toplevel()) {
        let surface_loc = Point::<f64, Logical>::from((
            (thumb.pos.0 - origin.x) as f64 - geo.loc.x as f64 * thumb.scale,
            (thumb.pos.1 - origin.y) as f64 - geo.loc.y as f64 * thumb.scale,
        ));
        render_for_surface_zoomed(
            toplevel.wl_surface(),
            surface_loc,
            thumb.scale,
            blur.all_windows.then_some(geo),
            1.0,
            blur,
            rounding.as_ref(),
            &mut |element| {
                // With a snapshot the blur is of the wallpaper alone, whatever is behind.
                out.push(JffElement::from(element.with_wallpaper_source(snapshot)));
            },
        );
    }
}

/// Thumbnails and highlight, front to back. `origin` is the monitor's top-left in the shared
/// space, elements are placed relative to it.
///
/// `snapshot`: blur the wallpaper copy in it rather than the live frame (`None`: the live
/// frame, so windows behind a thumbnail are blurred too).
pub fn overview_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();

    // The search box and the app list are in front of everything.
    if let Some(search) = &plan.search {
        let panels = std::iter::once((&search.box_buffer, search.box_pos))
            .chain(search.list.as_ref().map(|(buffer, pos)| (buffer, *pos)));
        for (buffer, pos) in panels {
            let loc: Point<f64, Physical> =
                (f64::from(pos.x - origin.x), f64::from(pos.y - origin.y)).into();
            match MemoryRenderBufferRenderElement::from_buffer(
                &mut *renderer,
                loc,
                buffer,
                Some(search.alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => out.push(JffElement::Memory(element)),
                Err(err) => tracing::warn!("error drawing the overview search: {err:?}"),
            }
        }
    }

    // The dragged thumbnail sits in front of the rest, the workspace wall right behind it.
    let order = plan
        .dragged
        .into_iter()
        .chain((0..plan.thumbs.len()).filter(|i| Some(*i) != plan.dragged));
    let wall_at = usize::from(plan.dragged.is_some());
    for (n, i) in order.enumerate() {
        if n == wall_at {
            wall_elements(renderer, plan, origin, &mut out);
        }
        let (window, thumb) = &plan.thumbs[i];
        badge_elements(renderer, plan, i, origin, &mut out);
        thumb_elements(
            renderer,
            window,
            thumb,
            origin,
            plan.blur_windows.apply(blur, &app_id_of(window)),
            plan.radius,
            &mut out,
            snapshot,
        );
    }
    // Nothing came after the dragged thumbnail (or there are no thumbnails): the wall goes last.
    if plan.thumbs.len() <= wall_at {
        wall_elements(renderer, plan, origin, &mut out);
    }

    // The highlight is pushed last so it's drawn behind: only its rim shows. It is a rounded
    // outline following the thumbnail's corners (`radius` shrunk with the thumbnail, fullscreen
    // windows are square), hollow so a translucent window shows no tint behind it.
    let (outline_width, outline_color) = plan.outline;
    if let Some((window, thumb)) = plan
        .selected
        .and_then(|i| plan.thumbs.get(i))
        .filter(|_| outline_width > 0)
    {
        let radius = if is_fullscreen(window) {
            0
        } else {
            (f64::from(plan.radius) * thumb.scale).round() as i32
        };
        let thumb_rect = Rectangle::<i32, Logical>::new(
            (thumb.pos.0 - origin.x, thumb.pos.1 - origin.y).into(),
            (thumb.size.0, thumb.size.1).into(),
        );
        if let Some(ring) = ring_element(renderer, thumb_rect, outline_width, radius, outline_color)
        {
            out.push(JffElement::Ring(ring));
        } else {
            // The outline shader is unavailable: a plain rectangle behind the thumbnail.
            let geometry = Rectangle::<i32, Physical>::new(
                (
                    thumb.pos.0 - origin.x - outline_width,
                    thumb.pos.1 - origin.y - outline_width,
                )
                    .into(),
                (
                    thumb.size.0 + 2 * outline_width,
                    thumb.size.1 + 2 * outline_width,
                )
                    .into(),
            );
            let [r, g, b, a] = outline_color;
            out.push(JffElement::Solid(SolidColorRenderElement::new(
                plan.highlight.clone(),
                geometry,
                CommitCounter::default(),
                // Solid colours are premultiplied.
                [r * a, g * a, b * a, a],
                Kind::Unspecified,
            )));
        }
    }
    out
}

/// The squares of the workspace wall: the outline of the hovered one (and of the shown
/// workspace's) in front, the fills behind. Nothing without a wall in the plan.
fn wall_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    let Some(wall) = &plan.wall else {
        return;
    };
    let radius = plan.radius / 2;
    for (i, cell) in wall.cells.iter().enumerate() {
        let local = Rectangle::<i32, Logical>::new(
            (cell.0 - origin.x, cell.1 - origin.y).into(),
            (cell.2, cell.3).into(),
        );
        let outline = if wall.hover == Some(i) {
            Some(plan.outline).filter(|(width, _)| *width > 0)
        } else if i == wall.current {
            Some((2, WALL_CURRENT))
        } else {
            None
        };
        if let Some((border, color)) = outline {
            if let Some(ring) = ring_element(renderer, local, border, radius, color) {
                out.push(JffElement::Ring(ring));
            }
        }
    }
    for (i, label) in wall.labels.iter().enumerate() {
        let Some((buffer, pos)) = label else {
            continue;
        };
        // The number is for the squares with nothing in them.
        if wall.thumbs.get(i).is_some_and(|thumbs| !thumbs.is_empty()) {
            continue;
        }
        let loc: Point<f64, Physical> =
            (f64::from(pos.x - origin.x), f64::from(pos.y - origin.y)).into();
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            loc,
            buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        ) {
            Ok(element) => out.push(JffElement::Memory(element)),
            Err(err) => tracing::warn!("error drawing a workspace number: {err:?}"),
        }
    }
    for (window, thumb) in wall.thumbs.iter().flatten() {
        thumb_elements(
            renderer,
            window,
            thumb,
            origin,
            None,
            plan.radius,
            out,
            None,
        );
    }
    wallpaper_elements(renderer, plan, wall, origin, out);
    for (i, cell) in wall.cells.iter().enumerate() {
        let Some(id) = wall.ids.get(i) else {
            continue;
        };
        let geometry = Rectangle::<i32, Physical>::new(
            (cell.0 - origin.x, cell.1 - origin.y).into(),
            (cell.2, cell.3).into(),
        );
        out.push(JffElement::Solid(SolidColorRenderElement::new(
            id.clone(),
            geometry,
            CommitCounter::default(),
            WALL_FILL,
            Kind::Unspecified,
        )));
    }
}

/// The monitor's wallpaper shrunk into every square of the wall, with the square's rounded
/// corners. Behind the windows of the square and in front of its fill.
fn wallpaper_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    wall: &WallPlan,
    origin: Point<i32, Logical>,
    out: &mut Vec<JffElement>,
) {
    if wall.wallpaper.is_empty() {
        return;
    }
    let monitor = output_rect(&plan.output);
    let scale =
        f64::from(wall.cells.first().map_or(1, |cell| cell.2)) / f64::from(monitor.2.max(1));
    for cell in &wall.cells {
        let rounding = Rounding::new(
            renderer,
            Rectangle::new(
                (cell.0 - origin.x, cell.1 - origin.y).into(),
                (cell.2, cell.3).into(),
            ),
            plan.radius / 2,
        );
        // Later surfaces of the layer are in front: the list is back to front, draw it so.
        for (surface, loc) in wall.wallpaper.iter().rev() {
            let slot: Point<i32, Physical> = Point::from((
                cell.0 - origin.x + (f64::from(loc.x) * scale).round() as i32,
                cell.1 - origin.y + (f64::from(loc.y) * scale).round() as i32,
            ));
            let surfaces: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    &mut *renderer,
                    surface,
                    Point::<i32, Physical>::from((0, 0)),
                    Scale::from(1.0f64),
                    1.0,
                    Kind::Unspecified,
                );
            let conversions = gamut::tree_conversions(surface);
            for element in surfaces {
                let conversion = gamut::find(&conversions, element.id());
                let shrunk = RescaleRenderElement::from_element(
                    element,
                    Point::<i32, Physical>::from((0, 0)),
                    scale,
                );
                let placed = RelocateRenderElement::from_element(shrunk, slot, Relocate::Relative);
                out.push(
                    match gamut::masked(&mut *renderer, rounding.as_ref(), conversion) {
                        Some(mask) => mask.wrap_thumb(placed),
                        None => JffElement::Thumb(placed),
                    },
                );
            }
        }
    }
}

/// Elements for the dragged thumbnail on a monitor that isn't showing the overview, so you can
/// see the window travelling there. `origin` is the monitor's top-left at `rect`.
pub fn overview_drag_elements(
    renderer: &mut GlesRenderer,
    plan: &OverviewPlan,
    origin: Point<i32, Logical>,
    rect: Rect,
    blur: Option<config::Blur>,
    snapshot: Option<&WallpaperSnapshot>,
) -> Vec<JffElement> {
    let mut out: Vec<JffElement> = Vec::new();
    if let Some((window, thumb)) = plan.dragged.and_then(|i| plan.thumbs.get(i)) {
        if layout::intersects(thumb.rect(), rect) {
            if let Some(i) = plan.dragged {
                badge_elements(renderer, plan, i, origin, &mut out);
            }
            thumb_elements(
                renderer,
                window,
                thumb,
                origin,
                plan.blur_windows.apply(blur, &app_id_of(window)),
                plan.radius,
                &mut out,
                snapshot,
            );
        }
    }
    out
}

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

    /// Whether a thumbnail is being dragged.
    fn overview_dragging(&self) -> bool {
        self.overview.as_ref().is_some_and(|ov| ov.drag.is_some())
    }

    /// View id of the window whose thumbnail is being dragged in the overview (for the `drag`
    /// watch event). Only set once the pointer moved past the click threshold.
    pub fn overview_dragged_view(&self) -> Option<u64> {
        let drag = self.overview.as_ref()?.drag.as_ref()?;
        drag.window
            .user_data()
            .get::<crate::ipc::ViewId>()
            .map(|id| id.0)
    }

    pub fn toggle_overview(&mut self) {
        if self.overview_active() {
            self.end_overview(None);
        } else {
            self.open_overview();
        }
    }

    /// Open the overview as the window switcher, with the next window selected (the last one
    /// with `backwards`). It ends on its own when the modifiers held now are let go.
    pub fn open_switcher(&mut self, backwards: bool) {
        if self.overview_active() {
            return;
        }
        // Nothing to switch between without windows.
        self.open_overview_with(false, None);
        if !self.overview_active() {
            return;
        }
        let held = self
            .seat
            .get_keyboard()
            .map(|k| k.modifier_state())
            .map_or((false, false, false), |m| (m.alt, m.ctrl, m.logo));
        if let Some(ov) = self.overview.as_mut() {
            ov.switcher = Some(held);
        }
        // Start with the focused window centered, so the first press already slides.
        self.switcher_retarget(true);
        self.handle_overview_key(OverviewKey::Cycle { backwards });
    }

    /// Whether the switcher is open and a modifier it was opened with is no longer in `now`.
    pub fn switcher_released(&self, now: Held) -> bool {
        self.overview
            .as_ref()
            .filter(|ov| ov.closing.is_none())
            .and_then(|ov| ov.switcher)
            .is_some_and(|held| overview::modifier_released(held, now))
    }

    /// Pick the monitor: the one under the pointer, so no click is needed to choose it. If it
    /// has no windows, fall back to the monitor with the focused window, so the key still does
    /// something.
    fn overview_output(&self) -> Option<Output> {
        let under_pointer = self.output_at_pointer();
        if let Some(output) = &under_pointer {
            if !self.windows_on(output_rect(output)).is_empty() {
                return under_pointer;
            }
        }
        let focused_rect = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus())
            .and_then(|focus| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
            })
            .map(|entry| {
                let size = entry.window.geometry().size;
                self.rect_for_window(entry.pos, (size.w, size.h))
            });
        focused_rect
            .and_then(|rect| {
                self.outputs
                    .iter()
                    .find(|o| output_rect(o) == rect)
                    .cloned()
            })
            .or(under_pointer)
    }

    /// Windows whose center is on `rect`, topmost first. Windows that haven't committed a buffer
    /// yet have no size and are skipped.
    fn windows_on(&self, rect: Rect) -> Vec<Window> {
        self.desk
            .top_down()
            .filter(|entry| {
                let size = entry.window.geometry().size;
                size.w > 0
                    && size.h > 0
                    && self.rect_for_window(entry.pos, (size.w, size.h)) == rect
            })
            .map(|entry| entry.window.clone())
            .collect()
    }

    /// Open the overview. No-op while a move or resize is going on. With no windows it still
    /// opens: it is where you start an app from.
    pub fn open_overview(&mut self) {
        self.open_overview_with(true, None);
    }

    /// [`JffState::open_overview`], `allow_empty` says whether it opens without windows and
    /// `on` which monitor it opens on (`None`: the one the pointer or the focused window is on).
    fn open_overview_with(&mut self, allow_empty: bool, on: Option<Output>) {
        if self.overview_active() {
            return;
        }
        // Still animating closed: finish that first.
        self.overview = None;

        // Overview and screenshot selection never open together - the overview wins.
        self.cancel_screenshot();

        // An open menu or popover has to go first: with a grab it holds the pointer (which
        // would block the overview opening) and without one only the client knows it's open.
        // Only the menu's own grabs are released, a move or resize keeps its pointer and still
        // stops the overview.
        self.dismiss_popups();

        if self.resize.is_some() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if pointer.is_grabbed() {
            return; // a move or a resize has the pointer
        }
        let Some(output) = on.or_else(|| self.overview_output()) else {
            return;
        };
        let rect = output_rect(&output);
        let windows = self.windows_on(rect);
        if windows.is_empty() && !allow_empty {
            return;
        }

        // Read the installed apps again if they changed, and get the font ready: both take a
        // moment and are wanted by the time the first letter is typed.
        self.app_index.refresh();
        self.osd.preload_fonts();
        let (entries, version) = self.app_index.snapshot();

        let keyboard = self.seat.get_keyboard();
        let focused = keyboard
            .as_ref()
            .and_then(|k| k.current_focus())
            .and_then(|focus| {
                self.desk
                    .find_by(|w| w.toplevel().is_some_and(|t| *t.wl_surface() == focus))
                    .map(|entry| entry.window.clone())
            });
        let selected = focused
            .clone()
            .filter(|w| windows.contains(w))
            .or_else(|| windows.first().cloned());

        // Bring the pointer onto the monitor first. This also tells the client under it that
        // the pointer left.
        let at = pointer.current_location();
        let (x, y) = layout::clamp_to_rects((at.x, at.y), &[rect]);
        let at: Point<f64, Logical> = (x, y).into();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);

        let grab_serial = SERIAL_COUNTER.next_serial();
        self.overview = Some(Overview {
            output,
            visible: windows.clone(),
            windows,
            selected,
            previous_focus: focused,
            pressed: None,
            press_origin: None,
            drag: None,
            layer_buttons: Vec::new(),
            grab_serial,
            opened: Instant::now(),
            closing: None,
            highlight: Id::new(),
            wall_ids: Vec::new(),
            wall_labels: Vec::new(),
            switcher: None,
            slide: Slide::still(0),
            badges: HashMap::new(),
            badges_for: (version, self.osd.fonts().is_some()),
            carry_badges: HashMap::new(),
            carry: Carry::new(),
            search: Search::new(entries, version),
        });

        let start_data = GrabStartData {
            focus: None,
            button: 0,
            location: at,
        };
        pointer.set_grab(
            self,
            OverviewGrab::new(start_data),
            grab_serial,
            Focus::Clear,
        );

        // Nobody has the keyboard while the overview is open: the old focus gets a proper
        // leave, so keys held down at that moment end cleanly.
        self.update_keyboard_focus();
        // A client may have left its own cursor image behind.
        self.cursor.set_status(CursorImageStatus::default_named());
    }

    /// Leave the overview and start the closing animation. `select` is the window to focus and
    /// raise, `None` restores the focus from before opening.
    ///
    /// Must not be called from inside a method of [`OverviewGrab`] - the pointer is locked there.
    pub fn end_overview(&mut self, select: Option<Window>) {
        self.cancel_hot_corner_timer();
        // Menus raised from a panel while the overview was open go with it: a click on empty
        // space or a key ends the overview without the menu ever seeing it. Do this before
        // letting go of the pointer, so the menu's own grab isn't left behind.
        if self.overview_active() {
            self.dismiss_popups();
        }

        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if ov.closing.is_some() {
            return;
        }

        let target = select.or_else(|| ov.previous_focus.clone());
        ov.selected = target.clone().filter(|w| ov.windows.contains(w));
        // Every window travels back, not only the ones that matched.
        ov.search.query.clear();
        ov.search.apps.clear();
        ov.search.app_selected = None;
        ov.update_visible();

        ov.closing = Some(Instant::now());
        ov.drag = None;
        ov.press_origin = None;
        let grab_serial = ov.grab_serial;

        if let Some(pointer) = self.seat.get_pointer() {
            if pointer.has_grab(grab_serial) {
                pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), InputTime::now());
            }
        }
        // The window may be gone by now (cancel after it closed).
        let target = target.filter(|w| self.desk.find_by(|x| x == w).is_some());
        match target {
            Some(window) => self.focus_window(&window),
            // Nothing to go back to: keyboard goes to whatever is on top.
            None => self.refocus_topmost(),
        }
        self.refresh_pointer_focus();
        // The switcher has no closing animation.
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| ov.switcher.is_some())
        {
            self.overview = None;
        }
    }

    /// Drop the overview at once, no animation (monitor unplugged, VT switch).
    pub fn abort_overview(&mut self) {
        self.cancel_hot_corner_timer();
        if self.overview.is_none() {
            return;
        }
        self.end_overview(None);
        self.overview = None;
    }

    /// Tell the client under the pointer that the pointer is there. The grab ignored what was
    /// under it, and windows may have moved since.
    pub fn refresh_pointer_focus(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let at = pointer.current_location();
        let under = self.pointer_focus_under(at);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: at,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
    }

    /// Place of every window in the overview, without the open and close animation. In the
    /// switcher that includes where the row has slid to so far.
    fn overview_slots(&self) -> Vec<(Window, Thumb)> {
        let Some(ov) = self.overview.as_ref() else {
            return Vec::new();
        };
        let mut thumbs = overview_layout(ov, self.config.switcher().thumbnail_height);
        if ov.switcher.is_some() {
            let dx = ov.slide.now().round() as i32;
            thumbs = thumbs
                .into_iter()
                .map(|t| overview::shifted(t, dx))
                .collect();
        }
        ov.visible.iter().cloned().zip(thumbs).collect()
    }

    /// Aim the switcher's row at the selected window. Does nothing while it is already headed
    /// there, otherwise it slides on from wherever it is now, or jumps with `snap`.
    fn switcher_retarget(&mut self, snap: bool) {
        let Some(ov) = self.overview.as_mut().filter(|ov| ov.switcher.is_some()) else {
            return;
        };
        let settings = *self.config.switcher();
        let target = switcher_target(ov, settings.thumbnail_height);
        if snap {
            ov.slide = Slide::still(target);
        } else if ov.slide.to != target {
            ov.slide = Slide {
                from: ov.slide.now(),
                to: target,
                since: Instant::now(),
                total: Duration::from_millis(settings.slide_ms.into()),
            };
        }
    }

    /// Layer surface the pointer can reach at `p` while the overview is open: overlay and top
    /// layers are in front of the thumbnails, bottom and background are behind.
    pub fn overview_layer_under(
        &self,
        p: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.overview_dragging() {
            return None;
        }
        if let Some((_, surface, loc)) = self.layer_under(p, &ABOVE_WINDOWS) {
            return Some((surface, loc));
        }
        // A menu of a panel behind the thumbnails is in front of them.
        if let Some((_, surface, loc)) = self.layer_popup_under(p, &BELOW_WINDOWS) {
            return Some((surface, loc));
        }
        if self.overview_hit(p).is_some() {
            return None;
        }
        self.layer_under(p, &BELOW_WINDOWS)
            .map(|(_, surface, loc)| (surface, loc))
    }

    /// Whether `p` is part of the overview proper: on its monitor, clear of what panels reserve and of any panel or menu drawn over it.
    fn overview_scroll_area(&self, p: Point<f64, Logical>) -> bool {
        let Some(ov) = self.overview.as_ref() else {
            return false;
        };
        let rect = output_rect(&ov.output);
        let zone = layer_map_for_output(&ov.output).non_exclusive_zone();
        let (x, y) = (p.x - f64::from(rect.0), p.y - f64::from(rect.1));
        let inside = x >= f64::from(zone.loc.x)
            && y >= f64::from(zone.loc.y)
            && x < f64::from(zone.loc.x + zone.size.w)
            && y < f64::from(zone.loc.y + zone.size.h);
        inside
            && self.layer_under(p, &ABOVE_WINDOWS).is_none()
            && self.layer_popup_under(p, &BELOW_WINDOWS).is_none()
    }

    /// Scroll over the overview switches workspace, one step per wheel notch or touchpad gesture. True when the event was used up here, false when it belongs to whatever is under the pointer (a panel's volume control, say).
    pub fn overview_scroll(
        &mut self,
        source: AxisSource,
        amount: f64,
        v120: Option<f64>,
        stop: bool,
    ) -> bool {
        if !self.overview_active() {
            return false;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        if !self.overview_scroll_area(pointer.current_location()) {
            self.overview_scroll.reset();
            return false;
        }
        // While typing a search the thumbnails are results, not a workspace: nothing to switch.
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| !ov.search.query.is_empty())
        {
            return true;
        }
        let steps = match source {
            AxisSource::Wheel | AxisSource::WheelTilt => self
                .overview_scroll
                .wheel(v120.unwrap_or(amount * 120.0 / 15.0)),
            _ => self.overview_scroll.touch(amount, stop),
        };
        if steps == 0 {
            return true;
        }
        // The monitor that was scrolled on, not whichever the pointer or the keyboard focus is
        // on once the workspace has changed.
        let Some(output) = self.overview.as_ref().map(|ov| ov.output.clone()) else {
            return false;
        };
        let name = output.name();
        let before = self.desk.current_on(&name);
        // Workspaces without windows are skipped, there is nothing to see in their overview.
        let target = self
            .desk
            .step_occupied(&name, before, steps, self.workspace_count());
        if target == before {
            return true;
        }
        // Switching closes the overview: open it again on the new workspace.
        self.switch_workspace_on(&name, target);
        self.open_overview_with(true, Some(output));
        true
    }

    /// The window whose thumbnail is under `p`.
    fn overview_hit(&self, p: Point<f64, Logical>) -> Option<Window> {
        let slots = self.overview_slots();
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        overview::hit_test(&thumbs, (p.x, p.y)).map(|i| slots[i].0.clone())
    }

    /// Pointer moved: select what it's over. Over empty space the selection stays.
    pub fn overview_hover(&mut self, p: Point<f64, Logical>) {
        if self.overview_dragging() || self.overview_start_drag(p) {
            return;
        }
        // A panel in front of the thumbnails isn't part of the overview.
        if self.layer_under(p, &ABOVE_WINDOWS).is_some()
            || self.layer_popup_under(p, &BELOW_WINDOWS).is_some()
        {
            return;
        }
        if let Some(row) = self.overview_app_at(p) {
            if let Some(ov) = self.overview.as_mut() {
                ov.search.app_selected = Some(row);
            }
            return;
        }
        if let Some(window) = self.overview_hit(p) {
            if let Some(ov) = self.overview.as_mut() {
                ov.selected = Some(window);
                ov.search.app_selected = None;
            }
        }
    }

    /// The row of the app list under `p`.
    fn overview_app_at(&self, p: Point<f64, Logical>) -> Option<usize> {
        let ov = self.overview.as_ref()?;
        if !ov.search.active() {
            return None;
        }
        let (x, y, w, h) = ov.search.list_rect?;
        let (px, py) = (p.x.floor() as i32, p.y.floor() as i32);
        if px < x || px >= x + w || py < y || py >= y + h {
            return None;
        }
        search_ui::tile_at(px - x, py - y, w, ov.search.apps.len())
    }

    /// Start dragging the thumbnail the left button went down on, once the pointer at `p` has
    /// moved past `DRAG_THRESHOLD`. Returns whether a drag is going on now.
    fn overview_start_drag(&mut self, p: Point<f64, Logical>) -> bool {
        let Some((window, from)) = self
            .overview
            .as_ref()
            .and_then(|ov| ov.press_origin.clone())
        else {
            return false;
        };
        if !overview::moved_past((from.x, from.y), (p.x, p.y), DRAG_THRESHOLD) {
            return false;
        }
        let slots = self.overview_slots();
        let Some((_, thumb)) = slots.iter().find(|(w, _)| *w == window) else {
            return false;
        };
        let anchor = overview::anchor_in(thumb, (from.x, from.y));
        let Some(ov) = self.overview.as_mut() else {
            return false;
        };
        ov.press_origin = None;
        ov.selected = Some(window.clone());
        ov.drag = Some(Drag { window, anchor });
        true
    }

    /// The dragged thumbnail was let go. On another monitor the window moves there, keeping its
    /// real size and the point it was held by under the pointer. On the same monitor nothing
    /// changes, the thumbnail slides back to its slot.
    fn overview_drop(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some((drag, source)) = self.overview.as_mut().and_then(|ov| {
            let drag = ov.drag.take()?;
            Some((drag, output_rect(&ov.output)))
        }) else {
            return;
        };
        let at = pointer.current_location();
        // Dropped on a square of the workspace wall: the window goes to that workspace.
        let cells = self.wall_cells();
        if let Some(workspace) = overview::wall_hit(&cells, (at.x, at.y)) {
            self.send_to_workspace(&drag.window, workspace);
            return;
        }
        let rects = self.output_rects();
        let target = layout::nearest((at.x, at.y), &rects)
            .map(|i| rects[i])
            .filter(|target| *target != source);
        let moving = self
            .desk
            .find_by(|w| *w == drag.window)
            .map(|entry| (entry.window.geometry().size, entry.restore));

        if let (Some(target), Some((size, restore))) = (target, moving) {
            let window = drag.window;
            let pos = if restore.is_some() {
                // Maximized / fullscreen windows get fitted to the monitor right after, they
                // only need to end up on the right one.
                (
                    target.0 + (target.2 - size.w) / 2,
                    target.1 + (target.3 - size.h) / 2,
                )
            } else {
                overview::drop_pos(drag.anchor, (at.x, at.y), (size.w, size.h), target)
            };
            let before = self.desk.pos_of(|w| *w == window);
            self.desk.set_pos_where(|w| *w == window, pos);
            self.desk.raise_where(|w| *w == window);
            // A floating member of a zone takes its zone along.
            if let (None, Some(before)) = (restore, before) {
                self.zones_carry(&window, before);
            }
            if let Some((restore_pos, restore_size)) = restore {
                let carried = (
                    restore_pos.0 + target.0 - source.0,
                    restore_pos.1 + target.1 - source.1,
                );
                self.desk.set_restore_pos_where(
                    |w| *w == window,
                    layout::fit_into(carried, restore_size, target),
                );
                self.relayout_outputs();
            }
        }
    }

    /// Button event while the overview is open. A click counts on release, so nothing of it is
    /// left over for the window that gets focused: left selects, middle closes, any button on
    /// empty space (or on another monitor) cancels. Left-drag on a thumbnail drags it, dropped
    /// on another monitor, the window moves there.
    pub fn overview_button(&mut self, button: u32, state: ButtonState, time: InputTime) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        // Clicks on panels and layer surfaces go to their client, the overview keeps its
        // clicks for thumbnails and empty space.
        let to_layer = match state {
            ButtonState::Pressed => self
                .overview_layer_under(pointer.current_location())
                .is_some(),
            ButtonState::Released => self.overview.as_mut().is_some_and(|ov| {
                let before = ov.layer_buttons.len();
                ov.layer_buttons.retain(|b| *b != button);
                ov.layer_buttons.len() != before
            }),
        };
        if to_layer {
            if state == ButtonState::Pressed {
                if let Some(ov) = self.overview.as_mut() {
                    ov.layer_buttons.push(button);
                }
                // The pointer may not have moved since opening: make sure the surface under it
                // has the pointer before the press. The focus is given here, not left to the
                // grab: the overview's own grab works it out itself, but a menu's grab keeps
                // only the focus it is handed, and with none the press counts as a click outside
                // the menu and closes it, so a button inside a menu could never open a submenu.
                let at = pointer.current_location();
                let focus = self.overview_layer_under(at);
                pointer.motion(
                    self,
                    focus,
                    &MotionEvent {
                        location: at,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.frame(self);
            return;
        }
        match state {
            ButtonState::Pressed => {
                if self.overview_dragging() {
                    return;
                }
                let at = pointer.current_location();
                let hit = if button == BTN_LEFT {
                    self.overview_hit(at)
                } else {
                    None
                };
                if let Some(ov) = self.overview.as_mut() {
                    ov.pressed = Some(button);
                    // A press is a selection: motion may not have run since the pointer reached
                    // this thumbnail.
                    if let Some(window) = &hit {
                        ov.selected = Some(window.clone());
                        ov.search.app_selected = None;
                    }
                    ov.press_origin = hit.map(|window| (window, at));
                }
            }
            ButtonState::Released => {
                if self.overview_dragging() && button != BTN_LEFT {
                    return;
                }
                let armed = self.overview.as_mut().and_then(|ov| {
                    ov.press_origin = None;
                    ov.pressed.take()
                });
                if armed != Some(button) {
                    return;
                }
                if button == BTN_LEFT && self.overview_dragging() {
                    self.overview_drop();
                    return;
                }
                if button == BTN_LEFT {
                    if let Some(row) = self.overview_app_at(pointer.current_location()) {
                        self.set_pick(Pick::App(row));
                        self.select_result();
                        return;
                    }
                }
                match self.overview_hit(pointer.current_location()) {
                    Some(window) if button == BTN_LEFT => self.end_overview(Some(window)),
                    Some(window) if button == BTN_MIDDLE => {
                        if let Some(toplevel) = window.toplevel() {
                            toplevel.send_close();
                        }
                    }
                    Some(_) => {}
                    None => self.end_overview(None),
                }
            }
        }
    }

    /// Carry out a key pressed while the overview is open.
    pub fn handle_overview_key(&mut self, key: OverviewKey) {
        match key {
            // Escape first empties the search, then closes.
            OverviewKey::Cancel => {
                if self.overview.as_ref().is_some_and(|ov| ov.search.active()) {
                    self.edit_query(|query| {
                        query.clear();
                        true
                    });
                } else {
                    self.end_overview(None);
                }
            }
            OverviewKey::Select => self.select_result(),
            OverviewKey::Move(dir) => self.move_selection(dir),
            OverviewKey::Cycle { backwards } => self.cycle_selection(backwards),
            OverviewKey::Type(c) => self.edit_query(|query| query.push(c)),
            OverviewKey::Backspace => self.edit_query(Query::backspace),
        }
    }

    /// Enter: start the selected app, or leave and focus the selected window. With text typed
    /// and nothing found it does nothing.
    fn select_result(&mut self) {
        let app = self
            .overview
            .as_ref()
            .and_then(|ov| ov.search.selected_entry());
        if let Some(entry) = app {
            self.launch_app(&entry);
            self.end_overview(None);
            return;
        }
        if self
            .overview
            .as_ref()
            .is_some_and(|ov| ov.search.active() && ov.visible.is_empty())
        {
            return;
        }
        let selected = self.overview.as_ref().and_then(|ov| ov.selected.clone());
        self.end_overview(selected);
    }

    fn launch_app(&mut self, entry: &DesktopEntry) {
        if let Err(why) = self.app_index.launch(entry) {
            tracing::warn!("could not start {}: {why}", entry.name);
            self.show_error(&format!("Could not start {}", entry.name), &why);
        }
    }

    /// Change the search text with `edit` (which says whether it changed anything), and show
    /// the new results with the first one selected. Not in the switcher.
    fn edit_query(&mut self, edit: impl FnOnce(&mut Query) -> bool) {
        let changed = self
            .overview
            .as_mut()
            .filter(|ov| ov.switcher.is_none() && ov.closing.is_none())
            .is_some_and(|ov| edit(&mut ov.search.query));
        if changed {
            self.refresh_search(true, true);
        }
    }

    /// Bring the results up to date: the matching windows and apps, and the selection.
    ///
    /// `reset`: select the first result (the query just changed), else only fix a selection
    /// that points at something gone. `recompute_apps`: look the apps up again (the query or
    /// the app list changed).
    fn refresh_search(&mut self, reset: bool, recompute_apps: bool) {
        let (entries, version) = if recompute_apps {
            self.app_index.snapshot()
        } else {
            (Arc::new(Vec::new()), 0)
        };
        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        if recompute_apps {
            ov.search.entries = entries;
            ov.search.version = version;
            ov.search.apps = apps::search_apps(
                &ov.search.entries,
                ov.search.query.as_str(),
                self.app_index.launches(),
                apps::MAX_RESULTS,
            );
        }
        ov.update_visible();

        if !ov.search.active() {
            ov.search.app_selected = None;
            let shown = ov.selected.as_ref().is_some_and(|s| ov.visible.contains(s));
            if !shown {
                ov.selected = ov.visible.first().cloned();
            }
            return;
        }
        let (windows, apps_len) = (ov.visible.len(), ov.search.apps.len());
        let pick = if reset {
            apps::first_pick(windows, apps_len)
        } else {
            apps::valid_pick(current_pick(ov), windows, apps_len)
        };
        apply_pick(ov, pick);
    }

    /// Select `pick`: a window among the visible ones, or an app row.
    fn set_pick(&mut self, pick: Pick) {
        if let Some(ov) = self.overview.as_mut() {
            apply_pick(ov, Some(pick));
        }
    }

    /// Tab: the next result, over the windows and then the apps. Without a search, over the
    /// windows.
    fn cycle_selection(&mut self, backwards: bool) {
        let search = self
            .overview
            .as_ref()
            .filter(|ov| ov.search.active())
            .map(|ov| (current_pick(ov), ov.visible.len(), ov.search.apps.len()));
        match search {
            None => self.select_with(|thumbs, i| Some(overview::cycle(thumbs.len(), i, backwards))),
            Some((current, windows, apps_len)) => {
                if let Some(pick) = apps::step_pick(current, windows, apps_len, !backwards) {
                    self.set_pick(pick);
                }
            }
        }
    }

    /// An arrow key. Without a search it moves between thumbnails. With one it moves between
    /// thumbnails too, and down from the bottom row goes on into the app list, where up and
    /// down walk the rows.
    fn move_selection(&mut self, dir: Dir) {
        let searching = self.overview.as_ref().is_some_and(|ov| ov.search.active());
        if !searching {
            self.select_with(|thumbs, i| overview::neighbor(thumbs, i, dir));
            return;
        }
        let Some((windows, apps_len, row, before)) = self.overview.as_ref().map(|ov| {
            (
                ov.visible.len(),
                ov.search.apps.len(),
                ov.search.app_selected,
                ov.selected.clone(),
            )
        }) else {
            return;
        };
        if let Some(row) = row {
            let columns = self
                .overview
                .as_ref()
                .and_then(|ov| ov.search.list_rect)
                .map_or(1, |(_, _, w, _)| search_ui::columns_for_width(w))
                as usize;
            if let Some(pick) = apps::step_in_app_grid(row, windows, apps_len, columns, dir) {
                self.set_pick(pick);
            }
            return;
        }
        self.select_with(|thumbs, i| overview::neighbor(thumbs, i, dir));
        let stayed = self.overview.as_ref().map(|ov| ov.selected.clone()) == Some(before);
        if stayed && dir == Dir::Down && apps_len > 0 {
            self.set_pick(Pick::App(0));
        }
    }

    /// Move the selection to whatever `pick` returns, given the thumbnails and the current
    /// index.
    fn select_with(&mut self, pick: impl FnOnce(&[Thumb], usize) -> Option<usize>) {
        let slots = self.overview_slots();
        if slots.is_empty() {
            return;
        }
        let thumbs: Vec<Thumb> = slots.iter().map(|(_, t)| *t).collect();
        let current = self
            .overview
            .as_ref()
            .and_then(|ov| ov.selected.as_ref())
            .and_then(|s| slots.iter().position(|(w, _)| w == s));
        let next = match current {
            Some(i) => pick(&thumbs, i),
            None => Some(0),
        };
        if let (Some(next), Some(ov)) = (next, self.overview.as_mut()) {
            ov.selected = Some(slots[next].0.clone());
        }
    }

    /// Once per frame, before drawing: finish a closed overview and follow windows that were
    /// created or destroyed while it's open.
    fn overview_tick(&mut self) {
        let Some(ov) = self.overview.as_ref() else {
            return;
        };
        if ov.closing.is_some_and(|since| since.elapsed() >= ANIMATION) {
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

    /// What to draw for the overview this frame, if there is one. Call before borrowing the
    /// backend - the layout is recomputed every frame, there are few windows.
    pub fn overview_plan(&mut self) -> Option<OverviewPlan> {
        self.overview_tick();
        self.switcher_retarget(false);
        // The wall only exists while a thumbnail is dragged.
        let wall_cells = if self.overview_dragging() {
            self.wall_cells()
        } else {
            Vec::new()
        };
        if let Some(ov) = self.overview.as_mut() {
            while ov.wall_ids.len() < wall_cells.len() {
                ov.wall_ids.push(Id::new());
            }
        }
        let wall_labels = if wall_cells.is_empty() {
            Vec::new()
        } else {
            self.wall_labels(&wall_cells)
        };
        let wall_thumbs = self.wall_thumbs(&wall_cells);
        let wallpaper: Vec<(WlSurface, Point<i32, Logical>)> = match self.overview.as_ref() {
            Some(ov) if !wall_cells.is_empty() => {
                let map = layer_map_for_output(&ov.output);
                map.layers_on(Layer::Background)
                    .filter_map(|layer| {
                        let geo = map.layer_geometry(layer)?;
                        Some((layer.wl_surface().clone(), geo.loc))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        self.refresh_badges();
        let carry = self.overview.as_mut().and_then(|ov| {
            let dragging = ov.drag.as_ref().map(|drag| drag.window.clone());
            ov.carry.step(dragging)
        });
        let ov = self.overview.as_ref()?;
        let t = ov.progress();
        let at = self
            .seat
            .get_pointer()
            .map(|p| p.current_location())
            .unwrap_or_default();
        // With a wall the dragged thumbnail is as big as one of its squares, so it can be
        // matched with one. On another monitor it is the window's real size, as it will land.
        let overview_rect = output_rect(&ov.output);
        // (how much the dragged window is scaled, the monitor the pointer is on)
        let drag_wall = wall_cells.first().map(|cell| {
            let rects = self.output_rects();
            let here = layout::nearest((at.x, at.y), &rects)
                .map(|i| rects[i])
                .unwrap_or(overview_rect);
            let on_overview = here == overview_rect;
            let scale = if on_overview {
                f64::from(cell.2) / f64::from(overview_rect.2.max(1))
            } else {
                1.0
            };
            // Over the overview the carried thumbnail is never shorter than a wall square.
            let min_height = if on_overview { cell.3 } else { 0 };
            (scale, here, min_height)
        });

        let thumbs: Vec<(Window, Thumb)> = self
            .overview_slots()
            .into_iter()
            .map(|(window, grid)| {
                // The animation runs between where the window really is and its grid slot.
                let real = self
                    .desk
                    .find_by(|w| *w == window)
                    .map(|entry| {
                        let size = entry.window.geometry().size;
                        Thumb {
                            pos: entry.pos,
                            size: (size.w, size.h),
                            scale: 1.0,
                        }
                    })
                    .unwrap_or(grid);
                let thumb = overview::lerp_thumb(real, grid, t);
                let thumb = match ov.drag.as_ref().filter(|drag| drag.window == window) {
                    Some(drag) => {
                        let thumb = match drag_wall {
                            Some((scale, monitor, min_height)) => {
                                // The size the window has while it is carried, as when it is
                                // dragged by its title: a maximized one is half the monitor
                                // (it maximizes again when dropped), a snapped or fullscreen
                                // one the size it floats at, any other its own.
                                let carried = self.desk.find_by(|w| *w == window).map_or(
                                    real.size,
                                    |entry| match entry.mode {
                                        WindowMode::Maximized => {
                                            ((monitor.2 / 2).max(1), (monitor.3 / 2).max(1))
                                        }
                                        WindowMode::Snapped(_) | WindowMode::Fullscreen => {
                                            entry.restore.map_or(real.size, |(_, size)| size)
                                        }
                                        WindowMode::Floating => real.size,
                                    },
                                );
                                // The picture is the window's real buffer shrunk by `zoom`, so the
                                // thumbnail is the real size times it: another shape (a snapped
                                // window's restore size) would leave a strip of background
                                // inside the outline.
                                let wanted = (f64::from(carried.0) * scale).max(1.0);
                                let mut zoom = wanted / f64::from(real.size.0.max(1));
                                let height = f64::from(real.size.1.max(1)) * zoom;
                                if height < f64::from(min_height) {
                                    zoom = f64::from(min_height) / f64::from(real.size.1.max(1));
                                }
                                let size = (
                                    ((f64::from(real.size.0) * zoom).round() as i32).max(1),
                                    ((f64::from(real.size.1) * zoom).round() as i32).max(1),
                                );
                                Thumb {
                                    size,
                                    scale: zoom,
                                    ..thumb
                                }
                            }
                            None => thumb,
                        };
                        overview::dragged_thumb(thumb, drag.anchor, (at.x, at.y))
                    }
                    None => thumb,
                };
                (window, thumb)
            })
            .collect();
        // Thumbnails draw front to back in list order. While closing, the just-focused window
        // was raised in the desk, so follow the desk's real stacking: the window being selected
        // must travel in front, not behind the one that was active.
        let mut thumbs = thumbs;
        if ov.closing.is_some() {
            let stacking: Vec<Window> = self
                .desk
                .top_down()
                .map(|entry| entry.window.clone())
                .collect();
            thumbs.sort_by_key(|(window, _)| {
                stacking
                    .iter()
                    .position(|w| w == window)
                    .unwrap_or(usize::MAX)
            });
        }
        let badge_carry = carry.and_then(|(window, mix)| {
            let index = thumbs.iter().position(|(w, _)| *w == window)?;
            Some((index, mix))
        });
        let carry_badge = badge_carry
            .and_then(|(index, _)| thumbs.get(index))
            .and_then(|(window, _)| ov.carry_badges.get(&badge_key(window)).cloned());
        let dragged = ov
            .drag
            .as_ref()
            .and_then(|drag| thumbs.iter().position(|(w, _)| *w == drag.window));
        let selected = ov
            .selected
            .as_ref()
            .filter(|_| ov.search.app_selected.is_none())
            .and_then(|s| thumbs.iter().position(|(w, _)| w == s));

        let badges = thumbs
            .iter()
            .map(|(window, _)| ov.badges.get(&badge_key(window)).cloned())
            .collect();
        let output = ov.output.clone();
        let highlight = ov.highlight.clone();
        let radius = self.config.radius();
        let wall = (!wall_cells.is_empty()).then(|| WallPlan {
            ids: ov.wall_ids.clone(),
            labels: wall_labels,
            thumbs: wall_thumbs,
            wallpaper,
            current: self.desk.current_on(&output.name()),
            hover: overview::wall_hit(&wall_cells, (at.x, at.y)),
            cells: wall_cells,
        });
        Some(OverviewPlan {
            output,
            thumbs,
            selected,
            dragged,
            radius,
            blur_windows: self.config.blur_windows().clone(),
            search: self.search_plan(),
            badges,
            badge_alpha: t as f32,
            badge_carry,
            carry_badge,
            wall,
            outline: {
                let config = self.config.overview();
                (config.selection_width, config.selection_color)
            },
            highlight,
        })
    }

    /// Paint the app icon badge of every app with a window in the overview that has none yet.
    /// The icon is the one of the installed app the window's app id belongs to, or one named
    /// like the app id, or else a lettered badge.
    fn refresh_badges(&mut self) {
        let fonts = self.osd.fonts();
        let Some(ov) = self.overview.as_mut() else {
            return;
        };
        let stamp = (ov.search.version, fonts.is_some());
        if ov.badges_for != stamp {
            ov.badges.clear();
            ov.carry_badges.clear();
            ov.badges_for = stamp;
        }
        for window in &ov.windows {
            let key = badge_key(window);
            let (app_id, own_icon) = (key.0.as_str(), key.1 != 0);
            if (app_id.is_empty() && !own_icon) || ov.badges.contains_key(&key) {
                continue;
            }
            let entry = apps::entry_for_app_id(&ov.search.entries, app_id)
                .and_then(|i| ov.search.entries.get(i));
            // What the window says itself comes first, the app's desktop file is the guess. A
            // picture smaller than the badge only wins when there is nothing else: blown up
            // it looks worse than the desktop file's icon.
            let own = window
                .toplevel()
                .filter(|_| own_icon)
                .and_then(|toplevel| toplevel_icon::client_icon(toplevel.wl_surface()));
            let (own_sharp, own_blurry) = match own {
                Some((icon, true)) => (Some(icon), None),
                Some((icon, false)) => (None, Some(icon)),
                None => (None, None),
            };
            let icon = own_sharp
                .or_else(|| {
                    entry.and_then(|entry| crate::feature::app_icons::icon_for(&entry.icon))
                })
                .or_else(|| crate::feature::app_icons::icon_for(app_id))
                .or(own_blurry);
            let name = entry.map_or(app_id, |entry| entry.name.as_str());
            let picture = search_ui::paint_badge(fonts, icon.as_deref(), name);
            ov.carry_badges.insert(key.clone(), buffer_of(&picture));
            ov.badges.insert(key, buffer_of(&picture));
        }
    }

    /// The search box and the app list, painted again only when what they show changed.
    fn search_plan(&mut self) -> Option<SearchPlan> {
        let ov = self.overview.as_mut().filter(|ov| ov.switcher.is_none())?;
        let alpha = ov.progress() as f32;
        let layout = search_layout(ov);
        ov.search.list_rect = layout.list_rect;
        // `None` while the font loads: the shapes are drawn now and the text follows.
        let fonts = self.osd.fonts();
        let has_fonts = fonts.is_some();

        let (x, y, width, _) = layout.box_rect;
        let key = hash_of((ov.search.query.as_str(), width, has_fonts));
        if ov.search.box_paint.as_ref().map(|(k, _)| *k) != Some(key) {
            let picture = search_ui::paint_box(fonts, ov.search.query.as_str(), width);
            ov.search.box_paint = Some((key, buffer_of(&picture)));
        }
        let box_buffer = ov.search.box_paint.as_ref().map(|(_, b)| b.clone())?;

        let list = match layout.list_rect {
            Some((lx, ly, lw, _)) => {
                let rows: Vec<Row> = ov
                    .search
                    .apps
                    .iter()
                    .filter_map(|&i| ov.search.entries.get(i))
                    .map(|entry| Row {
                        name: entry.name.clone(),
                        detail: entry.generic_name.clone(),
                        icon: crate::feature::app_icons::icon_for(&entry.icon),
                    })
                    .collect();
                let selected = ov.search.app_selected;
                let key = hash_of((&rows, selected, lw, has_fonts));
                if ov.search.list_paint.as_ref().map(|(k, _)| *k) != Some(key) {
                    let picture = search_ui::paint_list(fonts, &rows, selected, lw);
                    ov.search.list_paint = Some((key, buffer_of(&picture)));
                }
                ov.search
                    .list_paint
                    .as_ref()
                    .map(|(_, b)| (b.clone(), Point::from((lx - SHADOW, ly - SHADOW))))
            }
            None => None,
        };
        Some(SearchPlan {
            box_buffer,
            box_pos: Point::from((x - SHADOW, y - SHADOW)),
            list,
            alpha,
        })
    }
}

/// The number `n` of a workspace, `px` tall, on a transparent canvas just big enough for it.
fn paint_number(fonts: &Fonts, n: usize, px: f32) -> Picture {
    let text = n.to_string();
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
        WALL_NUMBER,
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

/// Where the overview puts its windows, before the switcher's slide: the grid, or in the
/// switcher a row of fixed-size thumbnails that starts at the left edge.
fn overview_layout(ov: &Overview, thumbnail_height: Option<i32>) -> Vec<Thumb> {
    let rect = output_rect(&ov.output);
    let mut area = layout::grow(rect, -MARGIN);
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
    let sizes: Vec<Size> = ov
        .visible
        .iter()
        .map(|w| {
            let size = w.geometry().size;
            (size.w, size.h)
        })
        .collect();
    if ov.switcher.is_some() {
        overview::strip(&sizes, area, GAP, thumbnail_height.unwrap_or(area.3 / 3))
    } else {
        overview::arrange(&sizes, area, GAP)
    }
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
fn switcher_target(ov: &Overview, thumbnail_height: Option<i32>) -> i32 {
    let area = layout::grow(output_rect(&ov.output), -MARGIN);
    let selected = ov
        .selected
        .as_ref()
        .and_then(|s| ov.visible.iter().position(|w| w == s))
        .unwrap_or(0);
    overview::strip_offset(&overview_layout(ov, thumbnail_height), selected, area)
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

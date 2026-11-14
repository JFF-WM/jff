//! Window borders.
//!
//! A border is a hollow rounded ring just outside the window's geometry, drawn with the same
//! shader as the overview's selection outline (`rounded::make_ring`). It sits behind the
//! window and its blur, and its outer corners are `radius + width` so the curves stay
//! concentric with the window's own corners. Fullscreen windows have none, so direct scanout
//! is untouched, and with no `border { }` block nothing here runs.
//!
//! Each window keeps a small state: whether it had focus the last time it was drawn, the colour
//! and animation strength the current fade started from, and the ring it was last given. When
//! focus moves, the colour eases from the old one to the new one over `fade-ms`.
//!
//! The focused window's ring can also turn (`animate`): the shader blends the colour towards a
//! second one by a weight that rotates around the window once per `period-ms`. The turning part
//! is scaled by the same fade, so it fades in with focus and out without it. Only that window
//! is ever animated, and only while the config asks for it (see `Border::animated`), every
//! other ring is the same static, unchanging element as before.
//!
//! Nothing schedules the frames of a fade or of the animation: a frame is rendered every
//! refresh whether or not anything changed (winit redraws non-stop, the TTY backend polls on
//! the refresh grid), and an unchanged ring is the same element as before, so an idle border
//! costs nothing.
//!
//! The ring is handed out as a [`BorderElement`], which keeps one id for the life of the
//! window and reports damage for the ring's band only. The shader's rectangle also covers the
//! window, and a plain `PixelShaderElement` that changes every frame would make Smithay
//! repaint all of that, window and blur included, on every frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::desktop::Window;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};

use super::rounded::{is_fullscreen, make_ring, ring_area, RingMotion, RingStyle};
use super::surface_alpha::window_multiplier;
use crate::config::{Border, BorderAnimation, BorderLook};
use crate::primitives::alpha;
use crate::primitives::overview_grid::{ease_out_cubic, progress};

/// What a window's border looks like right now, from the config and the focus.
#[derive(Clone, Copy, Debug)]
pub struct BorderPaint {
    pub style: Border,
    /// The window has the keyboard.
    pub focused: bool,
}

impl BorderPaint {
    /// The border of `window`, or `None` when the config has no border. `focused` is the window
    /// that has the keyboard.
    pub fn of(style: Border, window: &Window, focused: Option<&Window>) -> Option<Self> {
        style.on().then(|| Self {
            style,
            focused: focused == Some(window),
        })
    }
}

/// How far a fade that started `elapsed` ago and takes `total` has got, 0.0 to 1.0, easing out.
fn eased(elapsed: Duration, total: Duration) -> f32 {
    ease_out_cubic(progress(elapsed, total)) as f32
}

/// `from` moved towards `to` by `t` (0.0 = `from`, 1.0 = `to`), channel by channel.
pub fn mix(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (i, value) in out.iter_mut().enumerate() {
        *value = from[i] + (to[i] - from[i]) * t;
    }
    out
}

/// The colour of a fade that started `elapsed` ago and takes `total`.
pub fn fade_color(from: [f32; 4], to: [f32; 4], elapsed: Duration, total: Duration) -> [f32; 4] {
    mix(from, to, eased(elapsed, total))
}

/// The strength (0.0 to 1.0) of the animation in a fade that started `elapsed` ago and takes
/// `total`, from `from` to `to`. Ends exactly on `to`, so an animation that has faded out is
/// exactly 0.0 and the ring is static again.
pub fn fade_amount(from: f32, to: f32, elapsed: Duration, total: Duration) -> f32 {
    let t = eased(elapsed, total);
    if t >= 1.0 {
        to
    } else {
        from + (to - from) * t
    }
}

/// Where the animation is in its turn (0.0 up to, not including, 1.0) `elapsed` after the
/// start, for a turn of `period_ms`. 0.0 for a period of 0.
///
/// Wraps in integer milliseconds before turning into a float, so the number handed to the GPU
/// stays small and precise however long the compositor has been running.
pub fn phase(elapsed: Duration, period_ms: u64) -> f32 {
    if period_ms == 0 {
        return 0.0;
    }
    let into_turn = elapsed.as_millis() % u128::from(period_ms);
    // below 1.0 by construction, the clamp only guards rounding
    ((into_turn as f64 / period_ms as f64) as f32).clamp(0.0, 0.999_999)
}

/// The instant every animation is measured from: the first time one is asked for. A single
/// start keeps the borders of all windows in step.
fn clock_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// The strength the animation of a window settles at.
pub fn amount_target(style: &Border, focused: bool) -> f32 {
    if focused && style.animated() {
        1.0
    } else {
        0.0
    }
}

/// Room around a paper border for its shadow, in pixels.
const PAPER_SHADOW: i32 = 16;

/// How far smoke rises off a window's border, in pixels.
const SMOKE_REACH: i32 = 56;

/// How far the bloom of a neon border spreads, in pixels.
const NEON_REACH: i32 = 18;

/// How high the curtains of an aurora border rise, in pixels.
const AURORA_REACH: i32 = 34;

/// What a ring was made from: when this is the same, the same element does.
#[derive(PartialEq)]
struct RingKey {
    area: Rectangle<i32, Logical>,
    width: i32,
    radius: i32,
    color: [f32; 4],
    motion: RingMotion,
    /// Drawn for an HDR output, whose working space needs the colors converted.
    working: bool,
}

struct State {
    /// Focus as of the last frame.
    focused: bool,
    /// Colour the running fade started from.
    from: [f32; 4],
    /// Strength of the animation the running fade started from.
    from_amount: f32,
    since: Instant,
    /// Length of the fade, as of the last frame.
    fade: Duration,
    /// When the border was last drawn. A window that is gone or hidden stops being seen, and
    /// then its entry no longer counts as an animation (see [`animating`]).
    seen: Instant,
    /// The last ring handed out. Returned again while nothing about it changed, so Smithay sees
    /// the same element (same id, same commit) and repaints nothing.
    ring: Option<(RingKey, BorderElement)>,
}

/// Most windows remembered. A closed window leaves its entry behind, the table is emptied when
/// it grows past this (every border then starts over at its final colour).
const MAX_STATES: usize = 256;

thread_local! {
    /// By the window's surface. Rendering is single-threaded, so a per-thread table is enough.
    static STATES: RefCell<HashMap<ObjectId, State>> = RefCell::new(HashMap::new());
}

/// How long after it was last drawn a border still counts as one that is being shown.
const SEEN_WITHIN: Duration = Duration::from_millis(100);

/// A border drawn lately is changing by itself: fading after a focus change, or turning
/// (`animate`). The monitor has to keep drawing for it, nothing else asks for those frames.
pub fn animating(now: Instant) -> bool {
    STATES.with(|states| {
        states.borrow().values().any(|state| {
            now.saturating_duration_since(state.seen) < SEEN_WITHIN
                && (now.saturating_duration_since(state.since) < state.fade
                    || state
                        .ring
                        .as_ref()
                        .is_some_and(|(key, _)| key.motion.amount > 0.0))
        })
    })
}

fn target(paint: &BorderPaint) -> [f32; 4] {
    if paint.focused {
        paint.style.focus_color
    } else {
        paint.style.color
    }
}

/// The ring of `window`, or `None` when it has no border: no `paint`, a fullscreen window, a
/// window that isn't a Wayland toplevel, or the shader is unavailable.
///
/// `geo` is the window geometry in frame coordinates, `radius` its corner radius. Push the
/// element after the window's own elements and their blur, so it is drawn behind them.
///
/// The ring fades with the window when its client set an opacity (wp_alpha_modifier_v1), and
/// is gone with a window that is fully transparent.
pub fn border_element(
    renderer: &mut GlesRenderer,
    window: &Window,
    geo: Rectangle<i32, Logical>,
    radius: i32,
    paint: Option<BorderPaint>,
) -> Option<BorderElement> {
    let paint = paint.filter(|p| p.style.on())?;
    if geo.size.w <= 0 || geo.size.h <= 0 || is_fullscreen(window) {
        return None;
    }
    let id = window.toplevel()?.wl_surface().id();
    let visible = window_multiplier(window);
    if alpha::is_invisible(visible) {
        return None;
    }
    let style = paint.style;
    let width = style.width;
    let fade = Duration::from_millis(u64::try_from(style.fade_ms).unwrap_or(0));
    let period = u64::try_from(style.period_ms).unwrap_or(0);
    let now = Instant::now();
    let want = target(&paint);
    let want_amount = amount_target(&style, paint.focused);

    let (key, cached, previous) = STATES.with(|states| {
        let mut states = states.borrow_mut();
        if states.len() > MAX_STATES {
            states.clear();
        }
        // A window seen for the first time shows its final look, no fade from nothing.
        let state = states.entry(id.clone()).or_insert_with(|| State {
            focused: paint.focused,
            from: want,
            from_amount: want_amount,
            since: now.checked_sub(fade).unwrap_or(now),
            fade,
            seen: now,
            ring: None,
        });
        state.fade = fade;
        state.seen = now;
        if state.focused != paint.focused {
            // Start from wherever the fade has got to, so changing focus mid-fade doesn't jump.
            let elapsed = now.duration_since(state.since);
            let was = if state.focused {
                style.focus_color
            } else {
                style.color
            };
            state.from = fade_color(state.from, was, elapsed, fade);
            let was_amount = amount_target(&style, state.focused);
            state.from_amount = fade_amount(state.from_amount, was_amount, elapsed, fade);
            state.focused = paint.focused;
            state.since = now;
        }
        let elapsed = now.duration_since(state.since);
        let color = fade_color(state.from, want, elapsed, fade);
        let amount = fade_amount(state.from_amount, want_amount, elapsed, fade);
        // At rest (no animation, or faded out) the motion is all zeros: the same key frame
        // after frame, so the ring is the same element and costs nothing.
        let motion = if style.look == BorderLook::Paper {
            // Paper stands still: the same element frame after frame. It needs room for its
            // shadow.
            RingMotion {
                paper: true,
                margin: PAPER_SHADOW,
                ..RingMotion::default()
            }
        } else if amount > 0.0 {
            RingMotion {
                color2: alpha::faded(style.focus_color_2(), visible),
                phase: phase(now.saturating_duration_since(clock_start()), period),
                amount,
                style: match style.animate {
                    BorderAnimation::Sweep => RingStyle::Sweep,
                    BorderAnimation::Smoke => RingStyle::Smoke,
                    BorderAnimation::Neon => RingStyle::Neon,
                    BorderAnimation::Aurora => RingStyle::Aurora,
                    _ => RingStyle::Spin,
                },
                margin: match style.animate {
                    BorderAnimation::Smoke => SMOKE_REACH,
                    BorderAnimation::Neon => NEON_REACH,
                    BorderAnimation::Aurora => AURORA_REACH,
                    _ => 0,
                },
                paper: false,
            }
        } else {
            RingMotion::default()
        };
        let key = RingKey {
            // Smoke, and the shadow of paper, have room around the ring.
            area: ring_area(geo, width + motion.margin),
            width,
            radius,
            // Faded only here: the fade state above keeps the colours as configured.
            color: alpha::faded(color, visible),
            motion,
            working: super::gamut::working().is_some(),
        };
        let cached = state
            .ring
            .as_ref()
            .filter(|(last, _)| *last == key)
            .map(|(_, element)| element.clone());
        let previous = state
            .ring
            .as_ref()
            .map(|(_, element)| (element.id.clone(), element.commit));
        (key, cached, previous)
    });
    if let Some(element) = cached {
        return Some(element);
    }

    let inner = make_ring(renderer, key.area, width, radius, key.color, key.motion)?;
    // The id stays, the commit moves on: Smithay then repaints the ring's band, not the whole
    // rectangle it would if the element were a new one.
    let (element_id, mut commit) =
        previous.unwrap_or_else(|| (Id::new(), CommitCounter::default()));
    commit.increment();
    let element = BorderElement {
        id: element_id,
        commit,
        inner,
        edge: width + 1 + key.motion.margin,
        reach: radius + width + 1 + key.motion.margin,
    };
    STATES.with(|states| {
        if let Some(state) = states.borrow_mut().get_mut(&id) {
            state.ring = Some((key, element.clone()));
        }
    });
    Some(element)
}

thread_local! {
    /// The last ring of each container, by container id. A container's border does not fade
    /// or turn: it changes colour at once, with the focus.
    static CONTAINER_RINGS: RefCell<HashMap<u64, (RingKey, BorderElement)>> =
        RefCell::new(HashMap::new());
}

/// Most container rings remembered. The table is emptied when it grows past this.
const MAX_CONTAINER_RINGS: usize = 64;

/// The ring around the container `id`, whose rectangle (the windows, border not included) is
/// `area`, or `None` when the shader is unavailable. `paint.focused` is whether the window that
/// has the keyboard is one of its members. Push it after the elements of the container's lowest
/// window, so it is drawn behind them all.
///
/// The same element comes back while nothing about the ring changed, so Smithay repaints
/// nothing, like [`border_element`] for a window at rest.
pub fn container_border_element(
    renderer: &mut GlesRenderer,
    id: u64,
    area: Rectangle<i32, Logical>,
    radius: i32,
    paint: BorderPaint,
) -> Option<BorderElement> {
    let style = paint.style;
    if !style.on() || area.size.w <= 0 || area.size.h <= 0 {
        return None;
    }
    let key = RingKey {
        area: ring_area(area, style.width),
        width: style.width,
        radius,
        color: target(&paint),
        motion: RingMotion::default(),
        working: super::gamut::working().is_some(),
    };
    let (cached, previous) = CONTAINER_RINGS.with(|rings| {
        let mut rings = rings.borrow_mut();
        if rings.len() > MAX_CONTAINER_RINGS {
            rings.clear();
        }
        let cached = rings
            .get(&id)
            .filter(|(last, _)| *last == key)
            .map(|(_, element)| element.clone());
        let previous = rings
            .get(&id)
            .map(|(_, element)| (element.id.clone(), element.commit));
        (cached, previous)
    });
    if let Some(element) = cached {
        return Some(element);
    }
    let inner = make_ring(renderer, key.area, key.width, radius, key.color, key.motion)?;
    let (element_id, mut commit) =
        previous.unwrap_or_else(|| (Id::new(), CommitCounter::default()));
    commit.increment();
    let element = BorderElement {
        id: element_id,
        commit,
        inner,
        edge: style.width + 1,
        reach: radius + style.width + 1,
    };
    CONTAINER_RINGS.with(|rings| {
        rings.borrow_mut().insert(id, (key, element.clone()));
    });
    Some(element)
}

/// A window's border as a render element.
///
/// Wraps the ring's `PixelShaderElement`. That one spans the window and its border, and
/// reports all of it as damaged whenever it changes, which for an animated border would be
/// every frame. This wrapper keeps one id and its own commit counter across the changes of
/// a window's ring and reports only the band the ring can occupy.
#[derive(Clone, Debug)]
pub struct BorderElement {
    id: Id,
    commit: CommitCounter,
    inner: PixelShaderElement,
    /// How far in from the outer edge the straight sides of the ring reach, in pixels.
    edge: i32,
    /// How far in the corners of the ring reach, in pixels.
    reach: i32,
}

impl Element for BorderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            return DamageSet::default();
        }
        // Every change repaints the whole band: the colours change everywhere.
        let size = self.inner.geometry(scale).size;
        let rects: Vec<Rectangle<i32, Physical>> =
            band_rects((size.w, size.h), self.edge, self.reach)
                .into_iter()
                .map(|(x, y, w, h)| Rectangle::new((x, y).into(), (w, h).into()))
                .collect();
        DamageSet::from_slice(&rects)
    }

    // The ring may be translucent, and it is mostly hole.
    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl RenderElement<GlesRenderer> for BorderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        RenderElement::<GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
    }
}

/// The rectangles `(x, y, width, height)`, relative to the ring element's top left, that hold
/// every pixel of a ring `w` by `h` pixels: a band across the top and the bottom as deep as
/// the corners reach (`reach`), and a strip down each side as wide as the ring (`edge`). The
/// middle of the window, which is most of the element, is left out. They don't overlap.
pub fn band_rects(size: (i32, i32), edge: i32, reach: i32) -> Vec<(i32, i32, i32, i32)> {
    let (w, h) = size;
    if w <= 0 || h <= 0 {
        return Vec::new();
    }
    let corner = reach.clamp(0, h / 2);
    let mut rects = Vec::with_capacity(4);
    if corner > 0 {
        rects.push((0, 0, w, corner));
        rects.push((0, h - corner, w, corner));
    }
    let middle = h - 2 * corner;
    if middle > 0 && edge > 0 {
        if edge >= w / 2 {
            // too narrow to have a hole between the two strips
            rects.push((0, corner, w, middle));
        } else {
            rects.push((0, corner, edge, middle));
            rects.push((w - edge, corner, edge, middle));
        }
    }
    rects
}

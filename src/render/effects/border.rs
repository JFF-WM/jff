//! Window borders.
//!
//! A border is a hollow rounded ring just outside the window's geometry, drawn with the same
//! shader as the overview's selection outline (`rounded::make_ring`). It sits behind the window
//! and its blur, and its outer corners are `radius + width` so the curves stay concentric with
//! the window's own corners. Fullscreen windows have none, so direct scanout is untouched, and
//! with no `border { }` block nothing here runs.
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
//! window, and a plain `PixelShaderElement` that changes every frame would make Smithay repaint
//! all of that - window and blur included - on every frame.

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

use super::rounded::{is_fullscreen, make_ring, ring_area, RingMotion};
use crate::config::{Border, BorderAnimation};
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

/// How far a fade started `elapsed` ago and lasting `total` has got (0.0 to 1.0), eased out.
fn eased(elapsed: Duration, total: Duration) -> f32 {
    ease_out_cubic(progress(elapsed, total)) as f32
}

/// `from` moved towards `to` by `t` (0.0 = `from`, 1.0 = `to`), channel by channel.
fn mix(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (i, value) in out.iter_mut().enumerate() {
        *value = from[i] + (to[i] - from[i]) * t;
    }
    out
}

/// The colour of a fade started `elapsed` ago and lasting `total`.
fn fade_color(from: [f32; 4], to: [f32; 4], elapsed: Duration, total: Duration) -> [f32; 4] {
    mix(from, to, eased(elapsed, total))
}

/// The strength (0.0 to 1.0) of the animation in a fade started `elapsed` ago and lasting
/// `total`, going from `from` to `to`. Ends exactly on `to`, so a faded-out animation is
/// exactly 0.0 and the ring is static again.
fn fade_amount(from: f32, to: f32, elapsed: Duration, total: Duration) -> f32 {
    let t = eased(elapsed, total);
    if t >= 1.0 {
        to
    } else {
        from + (to - from) * t
    }
}

/// Position within a turn (0.0 up to, not including, 1.0) `elapsed` after start, for a turn of
/// `period_ms`. 0.0 when the period is 0.
///
/// Wraps in integer milliseconds before becoming a float, so the value handed to the GPU stays
/// small and precise however long the compositor has been running.
fn phase(elapsed: Duration, period_ms: u64) -> f32 {
    if period_ms == 0 {
        return 0.0;
    }
    let into_turn = elapsed.as_millis() % u128::from(period_ms);
    // Below 1.0 by construction, the clamp only guards rounding.
    ((into_turn as f64 / period_ms as f64) as f32).clamp(0.0, 0.999_999)
}

/// Instant every animation is measured from: the first time one is asked for. A single start
/// keeps the borders of all windows in step.
fn clock_start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Strength the animation of a window settles at.
fn amount_target(style: &Border, focused: bool) -> f32 {
    if focused && style.animated() {
        1.0
    } else {
        0.0
    }
}

/// What a ring was made from: when equal, the same element does.
#[derive(PartialEq)]
struct RingKey {
    area: Rectangle<i32, Logical>,
    width: i32,
    radius: i32,
    color: [f32; 4],
    motion: RingMotion,
}

struct State {
    /// Focus as of the last frame.
    focused: bool,
    /// Colour the running fade started from.
    from: [f32; 4],
    /// Animation strength the running fade started from.
    from_amount: f32,
    since: Instant,
    /// Last ring handed out. Returned again while nothing about it changed, so Smithay sees
    /// the same element (same id, same commit) and repaints nothing.
    ring: Option<(RingKey, BorderElement)>,
}

/// Most windows remembered. A closed window leaves its entry behind, the table is emptied when
/// it grows past this (every border then starts over at its final colour).
const MAX_STATES: usize = 256;

thread_local! {
    /// Keyed by the window's surface. Rendering is single-threaded, so a per-thread table is
    /// enough.
    static STATES: RefCell<HashMap<ObjectId, State>> = RefCell::new(HashMap::new());
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
/// element after the window's own elements and their blur, so it's drawn behind them.
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
        // A window seen for the first time shows its final look - no fade from nothing.
        let state = states.entry(id.clone()).or_insert_with(|| State {
            focused: paint.focused,
            from: want,
            from_amount: want_amount,
            since: now.checked_sub(fade).unwrap_or(now),
            ring: None,
        });
        if state.focused != paint.focused {
            // Start from wherever the fade got to, so changing focus mid-fade doesn't jump.
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
        // At rest (no animation, or faded out) motion is all zeros: the same key frame after
        // frame, so the ring is the same element and costs nothing.
        let motion = if amount > 0.0 {
            RingMotion {
                color2: style.focus_color_2(),
                phase: phase(now.saturating_duration_since(clock_start()), period),
                amount,
                sweep: style.animate == BorderAnimation::Sweep,
            }
        } else {
            RingMotion::default()
        };
        let key = RingKey {
            area: ring_area(geo, width),
            width,
            radius,
            color,
            motion,
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
    // rectangle it would if the element were new.
    let (element_id, mut commit) =
        previous.unwrap_or_else(|| (Id::new(), CommitCounter::default()));
    commit.increment();
    let element = BorderElement {
        id: element_id,
        commit,
        inner,
        edge: width + 1,
        reach: radius + width + 1,
    };
    STATES.with(|states| {
        if let Some(state) = states.borrow_mut().get_mut(&id) {
            state.ring = Some((key, element.clone()));
        }
    });
    Some(element)
}

/// A window's border as a render element.
///
/// Wraps the ring's `PixelShaderElement`. That one spans the window and its border and reports
/// all of it as damaged whenever it changes - for an animated border, every frame. This
/// wrapper keeps one id and its own commit counter across the changes of a window's ring, and
/// reports only the band the ring can occupy.
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
        // Any change repaints the whole band - the colour changes everywhere.
        let size = self.inner.geometry(scale).size;
        let rects: Vec<Rectangle<i32, Physical>> =
            band_rects((size.w, size.h), self.edge, self.reach)
                .into_iter()
                .map(|(x, y, w, h)| Rectangle::new((x, y).into(), (w, h).into()))
                .collect();
        DamageSet::from_slice(&rects)
    }

    // The ring may be translucent, and it's mostly hole.
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
/// every pixel of a ring `w` by `h` pixels: a band across the top and the bottom as deep as the
/// corners reach (`reach`), and a strip down each side as wide as the ring (`edge`). The
/// middle of the window - most of the element - is left out. They don't overlap.
fn band_rects(size: (i32, i32), edge: i32, reach: i32) -> Vec<(i32, i32, i32, i32)> {
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
            // Too narrow to have a hole between the two strips.
            rects.push((0, corner, w, middle));
        } else {
            rects.push((0, corner, edge, middle));
            rects.push((w - edge, corner, edge, middle));
        }
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const B: [f32; 4] = [1.0, 0.5, 0.25, 0.0];

    #[test]
    fn mix_hits_both_ends_and_the_middle() {
        assert_eq!(mix(A, B, 0.0), A);
        assert_eq!(mix(A, B, 1.0), B);
        assert_eq!(mix(A, B, 0.5), [0.5, 0.25, 0.125, 0.5]);
    }

    #[test]
    fn fade_runs_from_the_old_colour_to_the_new_one() {
        let total = Duration::from_millis(100);
        assert_eq!(fade_color(A, B, Duration::ZERO, total), A);
        assert_eq!(fade_color(A, B, total, total), B);
        // Past the end it stays put.
        assert_eq!(fade_color(A, B, total * 3, total), B);
        // Halfway in time is further than halfway in colour: it eases out.
        let half = fade_color(A, B, total / 2, total);
        assert!(half[0] > 0.5 && half[0] < 1.0, "{half:?}");
    }

    #[test]
    fn no_fade_is_instant() {
        assert_eq!(fade_color(A, B, Duration::ZERO, Duration::ZERO), B);
    }

    #[test]
    fn animation_fades_in_and_out_and_ends_exactly() {
        let total = Duration::from_millis(100);
        // Gaining focus: nothing to everything.
        assert_eq!(fade_amount(0.0, 1.0, Duration::ZERO, total), 0.0);
        assert_eq!(fade_amount(0.0, 1.0, total, total), 1.0);
        let half = fade_amount(0.0, 1.0, total / 2, total);
        assert!(half > 0.5 && half < 1.0, "{half}");
        // Losing it ends on exactly 0.0, which is what makes the ring static again.
        assert_eq!(fade_amount(1.0, 0.0, total, total), 0.0);
        assert_eq!(fade_amount(0.37, 0.0, total * 2, total), 0.0);
        assert_eq!(fade_amount(0.37, 1.0, total * 2, total), 1.0);
        // Changing focus mid-fade starts where it got to: no jump.
        let mid = fade_amount(0.0, 1.0, total / 4, total);
        assert_eq!(fade_amount(mid, 0.0, Duration::ZERO, total), mid);
        // No fade time: at once.
        assert_eq!(fade_amount(0.0, 1.0, Duration::ZERO, Duration::ZERO), 1.0);
        assert_eq!(fade_amount(1.0, 0.0, Duration::ZERO, Duration::ZERO), 0.0);
    }

    #[test]
    fn only_the_focused_animated_border_has_a_target() {
        let mut style = Border {
            width: 2,
            animate: BorderAnimation::Spin,
            focus_color_2: Some([1.0, 0.0, 0.0, 1.0]),
            ..Border::default()
        };
        assert_eq!(amount_target(&style, true), 1.0);
        assert_eq!(amount_target(&style, false), 0.0);
        style.animate = BorderAnimation::None;
        assert_eq!(amount_target(&style, true), 0.0);
        style.animate = BorderAnimation::Spin;
        style.period_ms = 0;
        assert_eq!(amount_target(&style, true), 0.0);
        style.period_ms = 3000;
        style.focus_color_2 = None;
        assert_eq!(amount_target(&style, true), 0.0);
    }

    #[test]
    fn phase_starts_at_zero_and_wraps_at_the_period() {
        let ms = Duration::from_millis;
        assert_eq!(phase(ms(0), 3000), 0.0);
        assert_eq!(phase(ms(1500), 3000), 0.5);
        assert_eq!(phase(ms(750), 3000), 0.25);
        // A full turn is the start again, however many turns have gone by.
        assert_eq!(phase(ms(3000), 3000), 0.0);
        assert_eq!(phase(ms(6000), 3000), 0.0);
        assert_eq!(phase(ms(7500), 3000), 0.5);
    }

    #[test]
    fn phase_stays_small_and_exact_after_a_very_long_time() {
        // 400 days of uptime: the wrap happens on integers, so nothing is lost.
        let long = Duration::from_secs(400 * 24 * 3600) + Duration::from_millis(1500);
        assert_eq!(phase(long, 3000), 0.5);
        let p = phase(long + Duration::from_millis(1), 3000);
        assert!((0.0..1.0).contains(&p), "{p}");
    }

    #[test]
    fn phase_is_zero_without_a_period_and_never_nan_or_one() {
        let ms = Duration::from_millis;
        assert_eq!(phase(ms(1234), 0), 0.0);
        assert_eq!(phase(Duration::ZERO, 0), 0.0);
        for period in [1, 2, 3, 1000, 3000, 600_000] {
            for elapsed in [0, 1, period - 1, period, period + 1, 123_456_789] {
                let p = phase(ms(elapsed), period);
                assert!(
                    p.is_finite() && (0.0..1.0).contains(&p),
                    "{elapsed}/{period}: {p}"
                );
            }
        }
    }

    /// Ring coverage as the shader computes it (`ring.frag`), for pixel centre `(x, y)` of an
    /// element `w` by `h` pixels.
    fn ring_coverage(x: f32, y: f32, w: i32, h: i32, border: i32, radius: i32) -> f32 {
        fn rounded_box(p: (f32, f32), half: (f32, f32), r: f32) -> f32 {
            let q = (p.0.abs() - (half.0 - r), p.1.abs() - (half.1 - r));
            let outside = (q.0.max(0.0).powi(2) + q.1.max(0.0).powi(2)).sqrt();
            outside + q.0.max(q.1).min(0.0) - r
        }
        let (w, h, border, radius) = (w as f32, h as f32, border as f32, radius as f32);
        let p = (x - 0.5 * w, y - 0.5 * h);
        let outer_half = (0.5 * w, 0.5 * h);
        let outer_r = if radius > 0.0 {
            (radius + border).min(outer_half.0.min(outer_half.1))
        } else {
            0.0
        };
        let outer = (0.5 - rounded_box(p, outer_half, outer_r)).clamp(0.0, 1.0);
        let inner_half = (
            (outer_half.0 - border - 1.0).max(0.0),
            (outer_half.1 - border - 1.0).max(0.0),
        );
        let inner_r = (radius - 1.0).min(inner_half.0.min(inner_half.1)).max(0.0);
        let inner = (0.5 - rounded_box(p, inner_half, inner_r)).clamp(0.0, 1.0);
        outer * (1.0 - inner)
    }

    /// Whether pixel `(x, y)` lies in one of `rects`.
    fn within(rects: &[(i32, i32, i32, i32)], x: i32, y: i32) -> bool {
        rects
            .iter()
            .any(|&(rx, ry, rw, rh)| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
    }

    #[test]
    fn damage_bands_cover_every_pixel_of_the_ring() {
        for (win_w, win_h, border, radius) in [
            (100, 60, 2, 8),
            (400, 300, 3, 20),
            (400, 300, 6, 0),
            (50, 30, 4, 40),
            (7, 5, 2, 3),
            (300, 200, 1, 64),
            (1, 1, 1, 0),
            (3, 200, 2, 30),
        ] {
            let (w, h) = (win_w + 2 * border, win_h + 2 * border);
            let bands = band_rects((w, h), border + 1, radius + border + 1);
            for y in 0..h {
                for x in 0..w {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    if ring_coverage(fx, fy, w, h, border, radius) > 0.0 {
                        assert!(
                            within(&bands, x, y),
                            "pixel {x},{y} of {w}x{h} (border {border}, radius {radius})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn damage_bands_leave_the_window_alone_and_never_overlap() {
        let (w, h) = (404, 304);
        let bands = band_rects((w, h), 3, 23);
        let area: i32 = bands.iter().map(|&(_, _, bw, bh)| bw * bh).sum();
        // Far less than the whole element.
        assert!(area < (w * h) / 5, "{area}");
        for (i, a) in bands.iter().enumerate() {
            assert!(
                a.0 >= 0 && a.1 >= 0 && a.0 + a.2 <= w && a.1 + a.3 <= h,
                "{a:?}"
            );
            for b in &bands[i + 1..] {
                let apart =
                    a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // The centre of the window is not damaged.
        assert!(!within(&bands, w / 2, h / 2));
        assert!(band_rects((0, 10), 3, 5).is_empty());
    }
}

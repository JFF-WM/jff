//! The stats overlay, "statistics for nerds": what happened to the focused window on its way
//! to the screen, and why. Toggled with the `toggle-stats` action (or `jff msg toggle-stats`),
//! read as JSON with `jff msg stats`.
//!
//! The frame cost rows say what each frame cost jff (CPU time, GPU wait, share of the refresh
//! interval) and so whether a lost sync is jff's or the client's. The frame rows count the
//! flips that missed the vblank predicted when the frame was drawn, against the sync target.
//! The latency rows give the time from the window's commit to the flip that showed it (the
//! timestamp `wp_presentation` reports as `presented`) and the drift of that flip from the
//! vblank predicted for it.
//!
//! Cost when off: nothing is kept and nothing is drawn. The hooks in `commit` and `on_vblank`
//! test one bool. While on, the text is repainted at most [`REFRESH`] times a second and a
//! timer keeps the monitor redrawing at that rate, so an idle desktop does not stay idle.
//!
//! The overlay is drawn over the window, so a frame that has it is composited: it turns direct
//! scanout and tearing off while it is shown. It says so ("Direct scanout: no, this overlay is
//! drawn over the window") instead of pretending otherwise. `jff msg stats` reads the same
//! numbers without drawing anything.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{with_renderer_surface_state, CommitCounter};
use smithay::desktop::Window;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Physical, Point, Transform};

use crate::feature::osd::{
    draw_text, elide, half_line, over, sd_round_box, text_width, Fonts, Picture, Pixel,
};
use crate::primitives::stats::{
    budget_of, commit_latency, drift_ms, missed_vblank, Costs, FrameCost, Latencies, Presented,
    Section, Snapshot, Syncs, Ticks, Tone,
};
use crate::render::elements::JffElement;
use crate::state::JffState;

/// How often the text is repainted, and the monitor redrawn for it.
pub const REFRESH: Duration = Duration::from_millis(250);
/// `jff msg stats` keeps the counters running this long after its last request.
pub const COLLECT_FOR: Duration = Duration::from_secs(10);

const PX: f32 = 13.0;
const TITLE_PX: f32 = 11.0;
const HEAD_PX: f32 = 14.0;
const PAD: i32 = 12;
const LINE: i32 = 18;
const TITLE_LINE: i32 = 24;
const HEAD_LINE: i32 = 24;
const LABEL_W: f32 = 118.0;
const COL_GAP: f32 = 10.0;
const MAX_VALUE: f32 = 440.0;
const RADIUS: f32 = 10.0;
/// Distance from the corner of the monitor.
const MARGIN: i32 = 16;

const FILL: [f32; 3] = [0.060, 0.062, 0.078];
const FILL_ALPHA: f32 = 0.88;
const HEAD_COLOR: [f32; 3] = [0.96, 0.96, 0.98];
const TITLE_COLOR: [f32; 3] = [0.45, 0.62, 0.95];
const LABEL_COLOR: [f32; 3] = [0.58, 0.58, 0.66];
const GOOD: [f32; 3] = [0.45, 0.85, 0.55];
const NEUTRAL: [f32; 3] = [0.90, 0.90, 0.93];
const WARN: [f32; 3] = [0.98, 0.76, 0.30];

/// What the overlay is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off,
    /// Counters run for `jff msg stats`, nothing is drawn. Ends [`COLLECT_FOR`] after the last
    /// request.
    Collect,
    /// Counters run and the overlay is drawn.
    Overlay,
}

struct Painted {
    buffer: MemoryRenderBuffer,
    width: i32,
    sections: Vec<Section>,
    header: String,
}

/// Counters and the picture. Lives in `JffState::stats`.
pub struct StatsOverlay {
    mode: Mode,
    collect_until: Option<Instant>,
    /// The redraw timer is running.
    timer_armed: bool,
    /// The protocol id of the toplevel being followed.
    tracked: Option<u32>,
    /// The monitor it is on, whose flips are counted.
    output: String,
    /// Last commit counter seen of each surface of the window.
    commits: HashMap<u32, CommitCounter>,
    client: Ticks,
    shown: Ticks,
    flips: Ticks,
    /// What the frames queued on `output` cost.
    costs: Costs,
    /// Which flips of `output` landed on the vblank predicted for them.
    syncs: Syncs,
    /// Commit to screen and drift of the flips of `output`.
    latencies: Latencies,
    /// The vblank the frame in flight was drawn for and the refresh interval it is counted
    /// with. `None` when it can not be told (VRR, tearing, no fresh frame clock).
    predicted: Option<(Duration, Duration)>,
    /// `client.total()` when the frame in flight was drawn.
    seen_total: u64,
    /// A frame is in flight on `output`: does it carry something new of the window.
    in_flight: Option<bool>,
    /// When the newest content of the window in the frame in flight was committed.
    in_flight_commit: Option<Duration>,
    /// When the snapshot was last rebuilt.
    built: Option<Instant>,
    last: Option<Snapshot>,
    painted: Option<Painted>,
}

impl Default for StatsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsOverlay {
    pub fn new() -> Self {
        Self {
            mode: Mode::Off,
            collect_until: None,
            timer_armed: false,
            tracked: None,
            output: String::new(),
            commits: HashMap::new(),
            client: Ticks::default(),
            shown: Ticks::default(),
            flips: Ticks::default(),
            costs: Costs::default(),
            syncs: Syncs::default(),
            latencies: Latencies::default(),
            predicted: None,
            seen_total: 0,
            in_flight: None,
            in_flight_commit: None,
            built: None,
            last: None,
            painted: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the overlay is drawn.
    pub fn drawn(&self) -> bool {
        self.mode == Mode::Overlay
    }

    /// Whether anything is counted. This is the one test the hooks make.
    #[inline]
    pub fn active(&self) -> bool {
        self.mode != Mode::Off
    }

    /// Show the overlay, or hide it. Returns whether it is shown now.
    pub fn toggle(&mut self) -> bool {
        self.mode = if self.mode == Mode::Overlay {
            Mode::Off
        } else {
            Mode::Overlay
        };
        self.collect_until = None;
        if self.mode == Mode::Off {
            self.reset();
        }
        self.drawn()
    }

    /// `jff msg stats` asks for the numbers: count for a while, without drawing.
    pub fn request_collect(&mut self, now: Instant) {
        if self.mode == Mode::Off {
            self.mode = Mode::Collect;
        }
        if self.mode == Mode::Collect {
            self.collect_until = Some(now + COLLECT_FOR);
        }
    }

    /// End a `Collect` that was not asked for lately. Returns whether it ended.
    fn expire(&mut self, now: Instant) -> bool {
        if self.mode == Mode::Collect && self.collect_until.is_none_or(|until| now >= until) {
            self.mode = Mode::Off;
            self.collect_until = None;
            self.reset();
            return true;
        }
        false
    }

    fn reset(&mut self) {
        self.tracked = None;
        self.commits.clear();
        self.client.clear();
        self.shown.clear();
        self.flips.clear();
        self.costs.clear();
        self.syncs.clear();
        self.latencies.clear();
        self.predicted = None;
        self.seen_total = 0;
        self.in_flight = None;
        self.in_flight_commit = None;
        self.built = None;
        self.last = None;
        self.painted = None;
    }

    /// The window being followed is `surface` on monitor `output`. Another window starts the
    /// counters over: a rate of the old window means nothing for the new one.
    pub fn follow(&mut self, surface: u32, output: &str) {
        if self.tracked != Some(surface) {
            self.tracked = Some(surface);
            self.commits.clear();
            self.client.clear();
            self.shown.clear();
            self.flips.clear();
            self.costs.clear();
            self.syncs.clear();
            self.latencies.clear();
            self.predicted = None;
            self.seen_total = 0;
            self.in_flight = None;
            self.in_flight_commit = None;
            self.built = None;
        } else if self.output != output {
            // moved to another monitor: its flips and frame costs are another series
            self.flips.clear();
            self.costs.clear();
            self.syncs.clear();
            self.latencies.clear();
            self.predicted = None;
            self.in_flight = None;
            self.in_flight_commit = None;
        }
        if self.output != output {
            self.output = output.to_owned();
        }
    }

    /// A commit of `surface`, which belongs to the window of `root`. Only a new buffer or new
    /// damage counts: a commit that changes nothing a renderer shows is not a frame.
    pub fn note_commit(&mut self, surface: &WlSurface, root: &WlSurface, at: Duration) {
        if self.tracked != Some(root.id().protocol_id()) {
            return;
        }
        let Some(now) = with_renderer_surface_state(surface, |state| state.current_commit()) else {
            return;
        };
        if self.commits.insert(surface.id().protocol_id(), now) == Some(now) {
            return;
        }
        self.client.push(at);
    }

    /// A frame was queued on `output`. It carries something new of the window if the window
    /// committed since the last frame. `predicted` is the vblank it was drawn for and `refresh`
    /// the monitor's interval; `predicted` is `None` when there is no vblank to miss.
    pub fn note_queued(&mut self, output: &str, predicted: Option<Duration>, refresh: Duration) {
        if output != self.output {
            return;
        }
        let new = self.client.total() != self.seen_total;
        self.seen_total = self.client.total();
        self.in_flight = Some(new);
        // the newest commit is what the frame shows: older ones were replaced before it was drawn
        self.in_flight_commit = if new { self.client.last() } else { None };
        self.predicted = predicted.map(|at| (at, refresh));
    }

    /// A frame queued on `output` at `at` cost `cost`.
    pub fn note_cost(&mut self, output: &str, at: Duration, cost: FrameCost) {
        if output == self.output {
            self.costs.push(at, cost);
        }
    }

    /// `output` flipped at `at`.
    pub fn note_flip(&mut self, output: &str, at: Duration) {
        if output != self.output {
            return;
        }
        self.flips.push(at);
        let predicted = self.predicted.take();
        if let Some((predicted, refresh)) = predicted {
            self.syncs.push(at, missed_vblank(predicted, at, refresh));
        }
        let carried = self.in_flight.take() == Some(true);
        let committed = self.in_flight_commit.take();
        if carried {
            self.shown.push(at);
        }
        let latency = committed.and_then(|committed| commit_latency(committed, at));
        let drift = predicted.map(|(predicted, _)| drift_ms(predicted, at));
        if latency.is_some() || drift.is_some() {
            self.latencies.push(
                at,
                Presented {
                    latency,
                    drift_ms: drift,
                },
            );
        }
    }

    /// Time to rebuild the snapshot (and so repaint)?
    pub fn due(&self, now: Instant) -> bool {
        self.built
            .is_none_or(|at| now.duration_since(at) >= REFRESH)
    }

    /// Fill in the rates of a snapshot built by the renderer and keep it.
    pub fn store(&mut self, mut snapshot: Snapshot, now: Instant, mono: Duration) {
        snapshot.client = self.client.timing(mono);
        snapshot.shown = self.shown.timing(mono);
        snapshot.display_hz = self.flips.rate(mono);
        let budget = snapshot.mode.and_then(|(_, _, mhz)| budget_of(mhz));
        snapshot.cost = self.costs.summary(mono, budget);
        snapshot.sync = self.syncs.summary(mono);
        snapshot.latency = self.latencies.summary(mono, budget);
        self.built = Some(now);
        self.last = Some(snapshot);
    }

    /// The last snapshot as JSON, for `jff msg stats`.
    pub fn json(&self) -> String {
        match &self.last {
            Some(snapshot) => format!(
                "{{\"ok\":true,\"collecting\":true,\"stats\":{}}}\n",
                crate::primitives::stats::to_json(snapshot)
            ),
            None => "{\"ok\":true,\"collecting\":true,\"stats\":null}\n".to_owned(),
        }
    }

    /// The picture of the last snapshot, repainted when its text changed. `None` while the
    /// font loads or when there is no snapshot yet.
    fn picture(&mut self, fonts: &Fonts) -> Option<(&MemoryRenderBuffer, i32)> {
        let snapshot = self.last.as_ref()?;
        let sections = crate::primitives::stats::sections(snapshot);
        let header = if snapshot.app_id.is_empty() {
            "(window without an app id)".to_owned()
        } else {
            snapshot.app_id.clone()
        };
        let same = self
            .painted
            .as_ref()
            .is_some_and(|p| p.sections == sections && p.header == header);
        if !same {
            let picture = paint(fonts, &header, &sections);
            self.painted = Some(Painted {
                width: picture.width,
                buffer: MemoryRenderBuffer::from_slice(
                    &picture.rgba,
                    Fourcc::Abgr8888,
                    (picture.width, picture.height),
                    1,
                    Transform::Normal,
                    None,
                ),
                sections,
                header,
            });
        }
        self.painted.as_ref().map(|p| (&p.buffer, p.width))
    }

    /// The overlay as an element at the top right of a monitor `area_w` wide (`inset` is where
    /// the usable area starts: x, y, width). `None` until there is something to show.
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        fonts: Option<&Fonts>,
        inset: (i32, i32, i32),
    ) -> Option<JffElement> {
        let fonts = fonts?;
        let (zone_x, zone_y, zone_w) = inset;
        let (buffer, width) = self.picture(fonts)?;
        let loc: Point<f64, Physical> = (
            f64::from(zone_x + zone_w - width - MARGIN),
            f64::from(zone_y + MARGIN),
        )
            .into();
        match MemoryRenderBufferRenderElement::from_buffer(
            &mut *renderer,
            loc,
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ) {
            Ok(element) => Some(JffElement::Memory(element)),
            Err(err) => {
                tracing::warn!("error drawing the stats overlay: {err:?}");
                None
            }
        }
    }
}

fn tone_color(tone: Tone) -> [f32; 3] {
    match tone {
        Tone::Good => GOOD,
        Tone::Neutral => NEUTRAL,
        Tone::Warn => WARN,
    }
}

enum Line {
    Title(&'static str),
    Row(&'static str, String, Tone),
}

/// Paint the overlay: a header with the app id, then the sections.
pub fn paint(fonts: &Fonts, header: &str, sections: &[Section]) -> Picture {
    let font = &fonts.regular;
    let bold = &fonts.medium;
    let mut lines: Vec<Line> = Vec::new();
    for section in sections {
        lines.push(Line::Title(section.title));
        for row in &section.rows {
            let value = elide(&row.value, MAX_VALUE, |s| text_width(font, s, PX));
            lines.push(Line::Row(row.label, value, row.tone));
        }
    }
    let header = elide(header, LABEL_W + COL_GAP + MAX_VALUE, |s| {
        text_width(bold, s, HEAD_PX)
    });
    let value_w = lines
        .iter()
        .map(|line| match line {
            Line::Row(_, value, _) => text_width(font, value, PX),
            Line::Title(_) => 0.0,
        })
        .fold(0.0f32, f32::max);
    let inner = (LABEL_W + COL_GAP + value_w).max(text_width(bold, &header, HEAD_PX));
    let width = 2 * PAD + inner.ceil() as i32;
    let height = 2 * PAD
        + HEAD_LINE
        + lines
            .iter()
            .map(|line| match line {
                Line::Title(_) => TITLE_LINE,
                Line::Row(..) => LINE,
            })
            .sum::<i32>();

    let mut canvas: Vec<Pixel> = vec![[0.0; 4]; (width * height) as usize];
    let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
    for py in 0..height {
        for px in 0..width {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = sd_round_box(fx, fy, half_w, half_h, half_w, half_h, RADIUS);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                let pixel = &mut canvas[(py * width + px) as usize];
                over(pixel, FILL, cover * FILL_ALPHA);
                let inner_cover = (0.5 - (d + 1.0)).clamp(0.0, 1.0);
                over(pixel, [1.0; 3], (cover - inner_cover).max(0.0) * 0.10);
            }
        }
    }

    let x = PAD as f32;
    let mut y = PAD as f32;
    draw_text(
        &mut canvas,
        width,
        height,
        bold,
        &header,
        HEAD_PX,
        x,
        y + HEAD_LINE as f32 / 2.0 + half_line(bold, HEAD_PX),
        HEAD_COLOR,
    );
    y += HEAD_LINE as f32;
    for line in &lines {
        match line {
            Line::Title(title) => {
                let text = title.to_uppercase();
                draw_text(
                    &mut canvas,
                    width,
                    height,
                    bold,
                    &text,
                    TITLE_PX,
                    x,
                    y + TITLE_LINE as f32 * 0.72 + half_line(bold, TITLE_PX) * 0.5,
                    TITLE_COLOR,
                );
                y += TITLE_LINE as f32;
            }
            Line::Row(label, value, tone) => {
                let base = y + LINE as f32 / 2.0 + half_line(font, PX);
                draw_text(
                    &mut canvas,
                    width,
                    height,
                    font,
                    label,
                    PX,
                    x,
                    base,
                    LABEL_COLOR,
                );
                draw_text(
                    &mut canvas,
                    width,
                    height,
                    font,
                    value,
                    PX,
                    x + LABEL_W + COL_GAP,
                    base,
                    tone_color(*tone),
                );
                y += LINE as f32;
            }
        }
    }

    let mut rgba = Vec::with_capacity(canvas.len() * 4);
    for p in &canvas {
        for c in p {
            rgba.push((c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    Picture {
        rgba,
        width,
        height,
    }
}

/// What the window itself says, for the snapshot. Colors and the buffer are read by the
/// renderer, which has them at hand.
pub struct WindowFacts {
    pub app_id: String,
    pub fullscreen: bool,
    pub wants_tearing: bool,
    pub says_game: bool,
    pub window_size: (i32, i32),
}

pub fn window_facts(window: &Window) -> Option<(WlSurface, WindowFacts)> {
    let toplevel = window.toplevel()?;
    let surface = toplevel.wl_surface().clone();
    let (app_id, _) = crate::feature::osd::toplevel_names(&surface);
    let size = window.geometry().size;
    let facts = WindowFacts {
        app_id,
        fullscreen: toplevel
            .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Fullscreen)),
        wants_tearing: crate::protocol::tearing_control::wants_tearing(&surface),
        says_game: crate::protocol::content_type::says_game(&surface),
        window_size: (size.w, size.h),
    };
    Some((surface, facts))
}

impl JffState {
    /// The `toggle-stats` action.
    pub fn toggle_stats(&mut self) {
        let shown = self.stats.toggle();
        tracing::info!("stats overlay {}", if shown { "shown" } else { "hidden" });
        self.arm_stats_timer();
        self.queue_redraw_all();
    }

    /// Keep the monitor redrawing at [`REFRESH`] while the counters run, so the numbers move
    /// on an idle screen too. The timer stops itself when they stop.
    pub fn arm_stats_timer(&mut self) {
        if self.stats.timer_armed || !self.stats.active() {
            return;
        }
        self.stats.timer_armed = true;
        let armed = self
            .loop_handle
            .insert_source(Timer::from_duration(REFRESH), |_, _, state| {
                let ended = state.stats.expire(Instant::now());
                if state.stats.active() {
                    state.queue_redraw_all();
                    TimeoutAction::ToDuration(REFRESH)
                } else {
                    state.stats.timer_armed = false;
                    // one more frame to take the overlay off the screen
                    if ended || !state.stats.drawn() {
                        state.queue_redraw_all();
                    }
                    TimeoutAction::Drop
                }
            });
        if armed.is_err() {
            self.stats.timer_armed = false;
        }
    }

    /// Called by every commit. One bool when the overlay is off.
    #[inline]
    pub fn stats_commit(&mut self, surface: &WlSurface, root: &WlSurface) {
        if !self.stats.active() {
            return;
        }
        self.stats.note_commit(
            surface,
            root,
            crate::protocol::presentation::monotonic_now(),
        );
    }
}

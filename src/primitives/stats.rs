//! Statistics for nerds: what happened to the focused window on its way to the screen.
//!
//! Pure code. The compositor gathers a [`Snapshot`] (facts it already keeps, plus the
//! rolling [`Ticks`] it fills from `commit` and `on_vblank`), and [`sections`] turns it into
//! the lines the overlay draws and [`to_json`] into what `jff msg stats` prints.
//!
//! "The FPS" is three different numbers, and the gap between them is the interesting part:
//! - client: how often the window committed a new buffer,
//! - shown: how often a frame carrying a new buffer of the window reached the screen,
//! - display: how often the monitor flipped at all (it varies under VRR, and is 0 while idle).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::time::Duration;

use super::color::{ImageDescription, NamedPrimaries, NamedTf, TransferFunction};

/// The span rates are counted over.
pub const RATE_SPAN: Duration = Duration::from_secs(1);
/// The span frame times (average, worst, 1% low, jitter) are taken over.
pub const TIMING_SPAN: Duration = Duration::from_secs(5);
/// Two events closer than this are one (a window and its subsurface committing together).
const SAME_EVENT: Duration = Duration::from_micros(500);
/// Most events kept.
const KEEP: usize = 1024;

/// When something happened, newest last. Times are on one clock (CLOCK_MONOTONIC).
#[derive(Clone, Debug, Default)]
pub struct Ticks {
    times: VecDeque<Duration>,
    /// Everything ever pushed since the last `clear`, for "did something new arrive".
    total: u64,
}

/// What [`Ticks::timing`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    /// Events per second over [`RATE_SPAN`], 0 when nothing happened in it.
    pub fps: f64,
    /// Mean time between events, ms.
    pub avg_ms: f64,
    /// Longest time between events, ms.
    pub worst_ms: f64,
    /// The rate of the slowest 1% of the intervals ("1% low").
    pub low1_fps: f64,
    /// Standard deviation of the intervals, ms.
    pub jitter_ms: f64,
}

impl Ticks {
    pub fn push(&mut self, at: Duration) {
        match self.times.back().copied() {
            // the clock went back (a new session): start over
            Some(last) if at < last => self.times.clear(),
            Some(last) if at - last < SAME_EVENT => return,
            _ => {}
        }
        self.times.push_back(at);
        self.total = self.total.saturating_add(1);
        while self.times.len() > KEEP {
            self.times.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.times.clear();
        self.total = 0;
    }

    /// How many events there were since the last `clear`.
    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Events per second over the last [`RATE_SPAN`] before `now`.
    pub fn rate(&self, now: Duration) -> f64 {
        let from = now.saturating_sub(RATE_SPAN);
        let inside: Vec<Duration> = self.times.iter().copied().filter(|t| *t > from).collect();
        match inside.as_slice() {
            [] => 0.0,
            [_] => 1.0 / RATE_SPAN.as_secs_f64(),
            [first, .., last] => {
                let span = (*last - *first).as_secs_f64();
                if span <= 0.0 {
                    0.0
                } else {
                    (inside.len() - 1) as f64 / span
                }
            }
        }
    }

    /// Frame time figures over the last [`TIMING_SPAN`] before `now`. `None` with fewer than
    /// three events in it: there is nothing to measure yet.
    pub fn timing(&self, now: Duration) -> Option<Timing> {
        let from = now.saturating_sub(TIMING_SPAN);
        let inside: Vec<Duration> = self.times.iter().copied().filter(|t| *t > from).collect();
        if inside.len() < 3 {
            return None;
        }
        let mut gaps: Vec<f64> = inside
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).as_secs_f64() * 1000.0)
            .collect();
        let n = gaps.len() as f64;
        let avg = gaps.iter().sum::<f64>() / n;
        let variance = gaps.iter().map(|g| (g - avg).powi(2)).sum::<f64>() / n;
        gaps.sort_by(|a, b| a.total_cmp(b));
        let worst = gaps.last().copied().unwrap_or(0.0);
        // "1% low": the average of the slowest 1% of the intervals (at least one)
        let slowest = ((gaps.len() as f64 * 0.01).ceil() as usize).clamp(1, gaps.len());
        let tail = &gaps[gaps.len() - slowest..];
        let p99 = tail.iter().sum::<f64>() / tail.len() as f64;
        Some(Timing {
            fps: self.rate(now),
            avg_ms: avg,
            worst_ms: worst,
            low1_fps: if p99 > 0.0 { 1000.0 / p99 } else { 0.0 },
            jitter_ms: variance.sqrt(),
        })
    }
}

/// What the window's buffer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    /// A dmabuf, with its DRM fourcc.
    Dmabuf(u32),
    /// Shared memory: the GPU has to copy it, and the plane can not take it.
    Shm,
    /// No buffer attached yet.
    None,
}

/// Why the window is not on the primary plane as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    NotFullscreen,
    /// Fullscreen, but not exactly the monitor or not opaque.
    NotCovering,
    /// This overlay. It is drawn over the window, so the frame has to be composited.
    StatsOverlay,
    Overview,
    Locked,
    Screenshot,
    LayerOver,
    PopupOver,
    ToastOver,
    /// HDR output: the check says why (see `hdr_scanout::Decline`).
    Hdr(String),
    /// Everything allowed it and the plane still did not take the buffer.
    PlaneRefused,
}

impl Why {
    fn text(&self) -> String {
        match self {
            Self::NotFullscreen => "the window is not fullscreen".into(),
            Self::NotCovering => {
                "fullscreen, but not an opaque window exactly the monitor's size".into()
            }
            Self::StatsOverlay => "this overlay is drawn over the window".into(),
            Self::Overview => "the overview is open".into(),
            Self::Locked => "the screen is locked".into(),
            Self::Screenshot => "a screenshot is open".into(),
            Self::LayerOver => "a panel or layer surface is over it".into(),
            Self::PopupOver => "a popup is over it".into(),
            Self::ToastOver => "a toast is over it".into(),
            Self::Hdr(text) => text.clone(),
            Self::PlaneRefused => {
                "the GPU's plane refused the buffer (format, modifier or size)".into()
            }
        }
    }
}

/// How the window reached the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Path {
    /// The client's buffer is on the primary plane: no copy, no conversion.
    Direct,
    /// The compositor drew the frame itself.
    Composited(Why),
}

/// Variable refresh rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vrr {
    /// The monitor or driver can not do it.
    Unsupported,
    Off,
    On,
}

/// Everything the overlay says about the focused window, at one moment.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub app_id: String,
    pub output: String,
    /// Output mode: width, height, refresh in mHz.
    pub mode: Option<(i32, i32, i32)>,

    pub client: Option<Timing>,
    pub shown: Option<Timing>,
    pub display_hz: f64,

    // what the client asked for
    pub fullscreen: bool,
    pub wants_tearing: bool,
    pub says_game: bool,
    pub buffer: BufferKind,
    /// The buffer's size and the window's, logical pixels.
    pub buffer_size: Option<(i32, i32)>,
    pub window_size: (i32, i32),
    /// The colors the surface said it has, `None` when it said nothing.
    pub color: Option<ImageDescription>,

    // what the compositor decided
    pub allow_tearing: bool,
    /// `adaptive-sync` in the config: off, fullscreen or always.
    pub vrr_policy: &'static str,
    pub path: Path,
    /// Whether direct scanout would be possible without this overlay, when that can be told.
    pub scanout_without_overlay: Option<bool>,

    // what the hardware did
    pub vrr: Vrr,
    pub hdr_output: bool,
    /// The monitor is told the content is a game.
    pub content_game: bool,
    /// The frame was flipped asynchronously (tearing).
    pub async_flip: bool,
}

/// How a line should be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Working as hoped.
    Good,
    /// Information.
    Neutral,
    /// Works, with a cost or something worth knowing.
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// A DRM fourcc as its four letters.
pub fn fourcc(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

/// The primaries of `color` by name, or "custom primaries".
pub fn primaries_name(color: &ImageDescription) -> &'static str {
    NamedPrimaries::ALL
        .into_iter()
        .find(|p| p.chromaticities() == color.primaries)
        .map_or("custom primaries", |p| match p {
            NamedPrimaries::Srgb => "sRGB/BT.709",
            NamedPrimaries::Pal => "BT.601 PAL",
            NamedPrimaries::Ntsc => "BT.601 NTSC",
            NamedPrimaries::Bt2020 => "BT.2020",
            NamedPrimaries::DciP3 => "DCI-P3",
            NamedPrimaries::DisplayP3 => "Display P3",
            NamedPrimaries::AdobeRgb => "Adobe RGB",
        })
}

/// The transfer function of `color` by name.
pub fn tf_name(color: &ImageDescription) -> String {
    match color.tf {
        TransferFunction::Named(NamedTf::Gamma22) => "gamma 2.2".into(),
        TransferFunction::Named(NamedTf::Gamma28) => "gamma 2.8".into(),
        TransferFunction::Named(NamedTf::Srgb) => "sRGB".into(),
        TransferFunction::Named(NamedTf::St2084Pq) => "PQ".into(),
        TransferFunction::Named(NamedTf::Hlg) => "HLG".into(),
        TransferFunction::Named(NamedTf::ExtLinear) => "scRGB linear".into(),
        TransferFunction::Power(_) => "power curve".into(),
    }
}

fn is_hdr_color(color: &ImageDescription) -> bool {
    matches!(
        color.tf,
        TransferFunction::Named(NamedTf::St2084Pq | NamedTf::Hlg | NamedTf::ExtLinear)
    )
}

fn fps_text(fps: f64) -> String {
    if fps >= 100.0 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.1}")
    }
}

fn row(label: &'static str, value: impl Into<String>, tone: Tone) -> Row {
    Row {
        label,
        value: value.into(),
        tone,
    }
}

/// The lines of the overlay, in groups: the frame rates, what the client asked for, what the
/// compositor decided and what the hardware did.
pub fn sections(s: &Snapshot) -> Vec<Section> {
    let mut out = Vec::new();

    // ---- frames -------------------------------------------------------------------------
    let mut frames = Vec::new();
    match &s.client {
        Some(t) => frames.push(row(
            "Client",
            format!(
                "{} fps  {:.1} ms avg, {:.1} worst",
                fps_text(t.fps),
                t.avg_ms,
                t.worst_ms
            ),
            Tone::Neutral,
        )),
        None => frames.push(row("Client", "no frames yet", Tone::Neutral)),
    }
    match (&s.shown, &s.client) {
        (Some(shown), client) => {
            // far below what the client makes: frames are being dropped on the way
            let behind = client.is_some_and(|c| c.fps > 1.0 && shown.fps < c.fps * 0.9);
            frames.push(row(
                "Shown",
                format!(
                    "{} fps  jitter {:.2} ms",
                    fps_text(shown.fps),
                    shown.jitter_ms
                ),
                if behind { Tone::Warn } else { Tone::Good },
            ));
            frames.push(row(
                "1% low",
                format!("{} fps", fps_text(shown.low1_fps)),
                Tone::Neutral,
            ));
        }
        (None, _) => frames.push(row("Shown", "no frames yet", Tone::Neutral)),
    }
    frames.push(row(
        "Display",
        if s.display_hz < 0.5 {
            "idle: nothing changed, so nothing was flipped".to_owned()
        } else {
            format!("{} flips/s", fps_text(s.display_hz))
        },
        Tone::Neutral,
    ));
    out.push(Section {
        title: "Frames",
        rows: frames,
    });

    // ---- what the client asked for ------------------------------------------------------
    let mut asked = Vec::new();
    asked.push(row(
        "Fullscreen",
        if s.fullscreen { "yes" } else { "no" },
        Tone::Neutral,
    ));
    asked.push(row(
        "Tearing",
        if s.wants_tearing {
            "wants it"
        } else {
            "not asked"
        },
        Tone::Neutral,
    ));
    if s.says_game {
        asked.push(row("Content", "says it is a game", Tone::Neutral));
    }
    let buffer = match s.buffer {
        BufferKind::Dmabuf(code) => row("Buffer", format!("dmabuf {}", fourcc(code)), Tone::Good),
        BufferKind::Shm => row(
            "Buffer",
            "shared memory: copied by the GPU, can not go to the plane",
            Tone::Warn,
        ),
        BufferKind::None => row("Buffer", "none yet", Tone::Neutral),
    };
    asked.push(buffer);
    if let Some((bw, bh)) = s.buffer_size {
        let (ww, wh) = s.window_size;
        if (bw, bh) == (ww, wh) || ww == 0 || wh == 0 {
            asked.push(row("Size", format!("{bw}×{bh}, not scaled"), Tone::Good));
        } else {
            asked.push(row(
                "Size",
                format!("buffer {bw}×{bh} is scaled to {ww}×{wh}"),
                Tone::Warn,
            ));
        }
    }
    asked.push(match &s.color {
        None => row("Colors", "none stated, taken as sRGB", Tone::Neutral),
        Some(color) => row(
            "Colors",
            format!("{}, {}", tf_name(color), primaries_name(color)),
            Tone::Neutral,
        ),
    });
    out.push(Section {
        title: "Client asked",
        rows: asked,
    });

    // ---- what the compositor decided ----------------------------------------------------
    let mut decided = Vec::new();
    decided.push(match &s.path {
        Path::Direct => row(
            "Direct scanout",
            "yes: the buffer goes to the screen as it is",
            Tone::Good,
        ),
        Path::Composited(why) => row(
            "Direct scanout",
            format!("no: {}", why.text()),
            if matches!(why, Why::NotFullscreen) {
                Tone::Neutral
            } else {
                Tone::Warn
            },
        ),
    });
    if matches!(s.path, Path::Composited(Why::StatsOverlay)) {
        match s.scanout_without_overlay {
            Some(true) => decided.push(row(
                "Without overlay",
                "it would be scanned out: hide this to get it",
                Tone::Warn,
            )),
            Some(false) => decided.push(row(
                "Without overlay",
                "it would not be scanned out either",
                Tone::Neutral,
            )),
            None => {}
        }
    }
    decided.push(row(
        "Tearing allowed",
        if s.allow_tearing { "yes" } else { "no" },
        Tone::Neutral,
    ));
    if s.wants_tearing {
        let result = if s.async_flip {
            row("Tearing result", "async flip requested", Tone::Good)
        } else if !s.allow_tearing {
            row(
                "Tearing result",
                "not used: allow-tearing is off",
                Tone::Warn,
            )
        } else if matches!(s.path, Path::Composited(Why::StatsOverlay)) {
            row(
                "Tearing result",
                "not used: this overlay forces composition",
                Tone::Warn,
            )
        } else if matches!(s.path, Path::Composited(_)) {
            row(
                "Tearing result",
                "not used: needs direct scanout",
                Tone::Warn,
            )
        } else {
            row(
                "Tearing result",
                "not used: the flip fell back to vsync",
                Tone::Warn,
            )
        };
        decided.push(result);
    }
    decided.push(row(
        "Adaptive sync",
        format!("config: {}", s.vrr_policy),
        Tone::Neutral,
    ));
    out.push(Section {
        title: "Compositor decided",
        rows: decided,
    });

    // ---- what the hardware did ----------------------------------------------------------
    let mut did = Vec::new();
    if let Some((w, h, mhz)) = s.mode {
        did.push(row(
            "Mode",
            format!("{} {w}×{h} @ {:.2} Hz", s.output, f64::from(mhz) / 1000.0),
            Tone::Neutral,
        ));
    }
    did.push(match s.vrr {
        Vrr::Unsupported => row(
            "VRR",
            "not supported by this monitor or driver",
            Tone::Neutral,
        ),
        Vrr::Off => row("VRR", "off right now", Tone::Neutral),
        Vrr::On => row("VRR", "on: the display waits for each frame", Tone::Good),
    });
    did.push(row(
        "Frame sync",
        if s.async_flip {
            "tearing: async flip (the driver may fall back to vsync)"
        } else if s.vrr == Vrr::On {
            "adaptive: a flip when the frame is ready"
        } else {
            "vsync: a flip at the next refresh"
        },
        if s.async_flip { Tone::Warn } else { Tone::Good },
    ));
    did.push(row(
        "HDR",
        if s.hdr_output { "on" } else { "off" },
        Tone::Neutral,
    ));
    let window_hdr = s.color.as_ref().is_some_and(is_hdr_color);
    did.push(match (&s.path, s.hdr_output, window_hdr) {
        (Path::Direct, _, _) => row("Conversion", "none: passed through", Tone::Good),
        (_, true, true) => row(
            "Conversion",
            "HDR window mapped by the compositor",
            Tone::Neutral,
        ),
        (_, true, false) => row(
            "Conversion",
            "SDR window lifted to the HDR output",
            Tone::Neutral,
        ),
        (_, false, true) => row("Conversion", "HDR window tone-mapped to SDR", Tone::Warn),
        (_, false, false) => row("Conversion", "none needed", Tone::Good),
    });
    if s.content_game {
        did.push(row("Content type", "monitor told: game", Tone::Neutral));
    }
    out.push(Section {
        title: "Hardware did",
        rows: did,
    });

    out
}

fn json_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_timing(timing: &Option<Timing>) -> String {
    match timing {
        None => "null".to_owned(),
        Some(t) => format!(
            "{{\"fps\":{:.2},\"avg_ms\":{:.3},\"worst_ms\":{:.3},\"low1_fps\":{:.2},\"jitter_ms\":{:.3}}}",
            t.fps, t.avg_ms, t.worst_ms, t.low1_fps, t.jitter_ms
        ),
    }
}

/// The snapshot as one JSON object, for `jff msg stats`.
pub fn to_json(s: &Snapshot) -> String {
    let (path, reason) = match &s.path {
        Path::Direct => ("direct", "null".to_owned()),
        Path::Composited(why) => ("composited", json_str(&why.text())),
    };
    let buffer = match s.buffer {
        BufferKind::Dmabuf(code) => format!("\"dmabuf:{}\"", fourcc(code)),
        BufferKind::Shm => "\"shm\"".to_owned(),
        BufferKind::None => "null".to_owned(),
    };
    let vrr = match s.vrr {
        Vrr::Unsupported => "unsupported",
        Vrr::Off => "off",
        Vrr::On => "on",
    };
    let size =
        |size: Option<(i32, i32)>| size.map_or("null".to_owned(), |(w, h)| format!("[{w},{h}]"));
    let colors = s.color.as_ref().map_or("null".to_owned(), |c| {
        format!(
            "{{\"transfer\":{},\"primaries\":{}}}",
            json_str(&tf_name(c)),
            json_str(primaries_name(c))
        )
    });
    format!(
        "{{\"app_id\":{},\"output\":{},\"client\":{},\"shown\":{},\"display_hz\":{:.2},\
         \"client_asked\":{{\"fullscreen\":{},\"tearing\":{},\"game\":{},\"buffer\":{},\"buffer_size\":{},\"window_size\":{},\"colors\":{}}},\
         \"compositor\":{{\"path\":\"{}\",\"reason\":{},\"scanout_without_overlay\":{},\"allow_tearing\":{},\"adaptive_sync_policy\":{}}},\
         \"hardware\":{{\"vrr\":\"{}\",\"hdr\":{},\"async_flip\":{},\"content_type_game\":{}}}}}",
        json_str(&s.app_id),
        json_str(&s.output),
        json_timing(&s.client),
        json_timing(&s.shown),
        s.display_hz,
        s.fullscreen,
        s.wants_tearing,
        s.says_game,
        buffer,
        size(s.buffer_size),
        size(Some(s.window_size)),
        colors,
        path,
        reason,
        s.scanout_without_overlay
            .map_or("null".to_owned(), |b| b.to_string()),
        s.allow_tearing,
        json_str(s.vrr_policy),
        vrr,
        s.hdr_output,
        s.async_flip,
        s.content_game,
    )
}

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
//!
//! Next to them is what each frame cost jff itself ([`Costs`]): the CPU time it took to build
//! and queue the frame, the time it then waited for the GPU, and how much of the refresh interval
//! that is. A frame that took longer than the interval is one jff made late. With the client's
//! rate beside it, that says whether a lost sync comes from jff or from the client ([`Blame`]).
//!
//! The sync itself is counted too ([`Syncs`]): every frame is drawn for the vblank the frame
//! clock predicts, and a flip that comes more than half a refresh after it missed it. The share
//! of flips that did not is held against [`SYNC_TARGET`].
//!
//! Last is the latency of the content itself ([`Latencies`]): the time from the commit that
//! carried it to the flip that showed it, and how far that flip was from the vblank predicted
//! for it (the drift). The flip time is the one jff reports to the client in `wp_presentation`'s
//! `presented`, so the delay here is the one a video player or a game is told about.
//!
//! Tearing is observed, not only requested ([`flip_off_grid`], [`Tearings`]): a flip that waits
//! for the vblank lands a whole number of refreshes after the one before it, a tearing flip
//! lands wherever the game committed.

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

    /// When the newest event happened.
    pub fn last(&self) -> Option<Duration> {
        self.times.back().copied()
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

/// What one frame cost jff, from the start of the redraw to the frame being queued.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameCost {
    /// Time spent on the CPU: bookkeeping, building the elements, recording the draw calls.
    pub cpu: Duration,
    /// Time spent waiting for the GPU to finish the frame, `None` when nothing was waited for.
    pub gpu_wait: Option<Duration>,
}

impl FrameCost {
    /// From the start of the redraw until the frame was ready to be flipped.
    pub fn total(&self) -> Duration {
        self.cpu + self.gpu_wait.unwrap_or_default()
    }
}

/// What [`Costs::summary`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostSummary {
    /// Frames the figures are over.
    pub frames: usize,
    pub cpu_avg_ms: f64,
    pub cpu_worst_ms: f64,
    /// `None` when no frame in the span waited for the GPU.
    pub gpu_avg_ms: Option<f64>,
    pub gpu_worst_ms: Option<f64>,
    /// Mean and longest CPU time plus GPU wait of a frame, ms.
    pub total_avg_ms: f64,
    pub total_worst_ms: f64,
    /// The refresh interval the frames are held against, ms. `None` when the mode is unknown.
    pub budget_ms: Option<f64>,
    /// Frames that took longer than the budget: they missed their flip.
    pub late: usize,
}

impl CostSummary {
    /// The mean frame as a share of the budget, 0 to 1 and over.
    pub fn avg_share(&self) -> Option<f64> {
        self.budget_ms.map(|b| self.total_avg_ms / b)
    }

    /// The longest frame as a share of the budget.
    pub fn worst_share(&self) -> Option<f64> {
        self.budget_ms.map(|b| self.total_worst_ms / b)
    }
}

/// What frames cost, newest last. Times are on one clock (CLOCK_MONOTONIC).
#[derive(Clone, Debug, Default)]
pub struct Costs {
    frames: VecDeque<(Duration, FrameCost)>,
}

impl Costs {
    /// A frame queued at `at` cost `cost`.
    pub fn push(&mut self, at: Duration, cost: FrameCost) {
        if self.frames.back().is_some_and(|(last, _)| at < *last) {
            // the clock went back (a new session): start over
            self.frames.clear();
        }
        self.frames.push_back((at, cost));
        while self.frames.len() > KEEP {
            self.frames.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// The frames of the last [`TIMING_SPAN`] before `now` against `budget`, `None` with no frame.
    pub fn summary(&self, now: Duration, budget: Option<Duration>) -> Option<CostSummary> {
        let from = now.saturating_sub(TIMING_SPAN);
        let inside: Vec<FrameCost> = self
            .frames
            .iter()
            .filter(|(at, _)| *at > from)
            .map(|(_, cost)| *cost)
            .collect();
        if inside.is_empty() {
            return None;
        }
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let n = inside.len() as f64;
        let cpu: Vec<f64> = inside.iter().map(|c| ms(c.cpu)).collect();
        let gpu: Vec<f64> = inside.iter().filter_map(|c| c.gpu_wait).map(ms).collect();
        let total: Vec<f64> = inside.iter().map(|c| ms(c.total())).collect();
        let worst = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
        let budget_ms = budget.filter(|b| !b.is_zero()).map(ms);
        Some(CostSummary {
            frames: inside.len(),
            cpu_avg_ms: cpu.iter().sum::<f64>() / n,
            cpu_worst_ms: worst(&cpu),
            gpu_avg_ms: (!gpu.is_empty()).then(|| gpu.iter().sum::<f64>() / gpu.len() as f64),
            gpu_worst_ms: (!gpu.is_empty()).then(|| worst(&gpu)),
            total_avg_ms: total.iter().sum::<f64>() / n,
            total_worst_ms: worst(&total),
            budget_ms,
            late: budget_ms.map_or(0, |b| total.iter().filter(|t| **t > b).count()),
        })
    }
}

/// The refresh interval of a mode refresh in mHz.
pub fn budget_of(refresh_mhz: i32) -> Option<Duration> {
    (refresh_mhz > 0).then(|| Duration::from_secs_f64(1000.0 / f64::from(refresh_mhz)))
}

/// The share of flips, in percent, that should land on the vblank predicted for them.
pub const SYNC_TARGET: f64 = 99.9;

/// Whether a flip at `actual` missed the vblank `predicted` for it: it came more than half a
/// refresh late, so the frame went out with a later vblank than the one it was drawn for.
pub fn missed_vblank(predicted: Duration, actual: Duration, refresh: Duration) -> bool {
    actual > predicted + refresh / 2
}

/// The vblank a frame is counted against, `None` when it is not worth counting: with no vblank
/// seen yet there is no grid, and when the last one is more than two refreshes before the
/// prediction the monitor idled and the grid has drifted off the real one.
pub fn countable_prediction(
    last_vblank: Option<Duration>,
    predicted: Duration,
    refresh: Duration,
) -> Option<Duration> {
    let last = last_vblank?;
    (predicted.saturating_sub(last) <= refresh * 2).then_some(predicted)
}

/// How many refreshes apart two flips may be for the grid to hold between them. Further apart
/// the monitor idled, and the refresh rate is never exactly the one the mode says.
const GRID_SPAN: f64 = 8.0;

/// Whether a flip at `now` came off the vblank grid, counted from the flip before it at
/// `previous` (the kernel's stamps of both, CLOCK_MONOTONIC). A flip that waits for the vblank
/// lands a whole number of refreshes after the one before it, a tearing flip lands wherever
/// the game committed. `None` when it can not be told: the flips are too far apart, the clock
/// went back, or the refresh is unknown. Not for VRR, whose flips are off the grid by design.
///
/// This tells tearing, it does not count it: a tearing flip that lands within an eighth of a
/// refresh of the grid looks like a vsync one, which is about one in four. The share found is
/// a lower bound, and a vsync flip is not taken for a tearing one unless the stamps are off by
/// more than that.
pub fn flip_off_grid(previous: Duration, now: Duration, refresh: Duration) -> Option<bool> {
    if refresh.is_zero() {
        return None;
    }
    let gap = now.checked_sub(previous)?.as_secs_f64();
    let refresh = refresh.as_secs_f64();
    let periods = (gap / refresh).round();
    if periods > GRID_SPAN {
        return None;
    }
    // less than half a refresh after the last flip: a display can not do that in sync
    if periods < 1.0 {
        return Some(true);
    }
    Some((gap - periods * refresh).abs() > refresh / 8.0)
}

/// Whether recent flips were off the vblank grid, newest last. Times are on one clock
/// (CLOCK_MONOTONIC).
#[derive(Clone, Debug, Default)]
pub struct Tearings {
    /// (when it flipped, whether it was off the grid)
    flips: VecDeque<(Duration, bool)>,
}

impl Tearings {
    /// A flip at `at`, `off_grid` or not.
    pub fn push(&mut self, at: Duration, off_grid: bool) {
        if self.flips.back().is_some_and(|(last, _)| at < *last) {
            // the clock went back (a new session): start over
            self.flips.clear();
        }
        self.flips.push_back((at, off_grid));
        while self.flips.len() > KEEP {
            self.flips.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.flips.clear();
    }

    /// The flips of the last [`TIMING_SPAN`] before `now`, `None` when there were none.
    pub fn summary(&self, now: Duration) -> Option<TearingSummary> {
        let from = now.saturating_sub(TIMING_SPAN);
        let (flips, off_grid) = self
            .flips
            .iter()
            .filter(|(at, _)| *at > from)
            .fold((0usize, 0usize), |(n, off), (_, is_off)| {
                (n + 1, off + usize::from(*is_off))
            });
        (flips > 0).then_some(TearingSummary { flips, off_grid })
    }
}

/// What [`Tearings::summary`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TearingSummary {
    /// Flips the figures are over.
    pub flips: usize,
    /// Of those, the ones that came off the vblank grid.
    pub off_grid: usize,
}

impl TearingSummary {
    /// Flips it takes before the figures say anything.
    pub const MIN_FLIPS: usize = 8;

    /// Enough flips to judge by.
    pub fn told(&self) -> bool {
        self.flips >= Self::MIN_FLIPS
    }

    /// The display tears: one flip in twenty off the grid is far more than the stamps' noise
    /// (none in a vsync run), and well under what real tearing gives (about three in four).
    pub fn seen(&self) -> bool {
        self.told() && self.off_grid * 20 >= self.flips
    }
}

/// What [`Syncs::summary`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncSummary {
    /// Flips counted since the counters started.
    pub flips: u64,
    /// Of those, the ones that missed the vblank predicted for them.
    pub missed: u64,
    /// The same over the last [`TIMING_SPAN`].
    pub recent_flips: usize,
    pub recent_missed: usize,
}

impl SyncSummary {
    /// The share of flips that were on time, in percent. 100 with nothing counted.
    pub fn percent(&self) -> f64 {
        if self.flips == 0 {
            return 100.0;
        }
        (self.flips - self.missed.min(self.flips)) as f64 * 100.0 / self.flips as f64
    }

    /// Whether [`SYNC_TARGET`] is kept.
    pub fn on_target(&self) -> bool {
        self.percent() >= SYNC_TARGET
    }
}

/// Which flips were on the predicted vblank, newest last. Times are on one clock
/// (CLOCK_MONOTONIC). The totals count from the last `clear`: a 99.9% target is a thousand
/// flips, which is longer than the rolling window keeps.
#[derive(Clone, Debug, Default)]
pub struct Syncs {
    /// (when it flipped, whether it missed), for the recent figures.
    recent: VecDeque<(Duration, bool)>,
    flips: u64,
    missed: u64,
}

impl Syncs {
    /// A flip at `at`, `missed` or not.
    pub fn push(&mut self, at: Duration, missed: bool) {
        if self.recent.back().is_some_and(|(last, _)| at < *last) {
            // the clock went back (a new session): start over
            self.clear();
        }
        self.recent.push_back((at, missed));
        self.flips = self.flips.saturating_add(1);
        if missed {
            self.missed = self.missed.saturating_add(1);
        }
        while self.recent.len() > KEEP {
            self.recent.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.recent.clear();
        self.flips = 0;
        self.missed = 0;
    }

    /// The counts, `None` before the first flip.
    pub fn summary(&self, now: Duration) -> Option<SyncSummary> {
        if self.flips == 0 {
            return None;
        }
        let from = now.saturating_sub(TIMING_SPAN);
        let (recent_flips, recent_missed) = self
            .recent
            .iter()
            .filter(|(at, _)| *at > from)
            .fold((0usize, 0usize), |(n, m), (_, missed)| {
                (n + 1, m + usize::from(*missed))
            });
        Some(SyncSummary {
            flips: self.flips,
            missed: self.missed,
            recent_flips,
            recent_missed,
        })
    }
}

/// One flip, as far as the window's content is concerned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Presented {
    /// From the commit of the newest content the frame carried to the flip that showed it.
    /// `None` when the flip carried nothing new of the window.
    pub latency: Option<Duration>,
    /// The flip minus the vblank predicted for it, ms: positive is late. `None` when there was
    /// no vblank to miss (see [`countable_prediction`]).
    pub drift_ms: Option<f64>,
}

/// Time from a commit to the flip that showed it, `None` when the flip is stamped before the
/// commit (clocks that disagree: nothing sensible to say).
pub fn commit_latency(committed: Duration, shown: Duration) -> Option<Duration> {
    shown.checked_sub(committed)
}

/// How far a flip at `actual` was from the vblank `predicted` for it, ms. Positive is late.
pub fn drift_ms(predicted: Duration, actual: Duration) -> f64 {
    if actual >= predicted {
        (actual - predicted).as_secs_f64() * 1000.0
    } else {
        -((predicted - actual).as_secs_f64() * 1000.0)
    }
}

/// Commit to screen, over the last [`TIMING_SPAN`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyFigures {
    /// Flips of new content the figures are over.
    pub frames: usize,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub worst_ms: f64,
}

/// Flip against its predicted vblank, over the last [`TIMING_SPAN`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DriftFigures {
    pub flips: usize,
    /// The mean of the signed drift: a bias, not a size. Near 0 when the prediction is right.
    pub avg_ms: f64,
    /// The drift of largest size, with its sign.
    pub worst_ms: f64,
}

/// What [`Latencies::summary`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencySummary {
    pub latency: Option<LatencyFigures>,
    pub drift: Option<DriftFigures>,
    /// The refresh interval the figures are held against, ms. `None` when the mode is unknown.
    pub budget_ms: Option<f64>,
}

/// What each flip did for the window, newest last. Times are on one clock (CLOCK_MONOTONIC).
#[derive(Clone, Debug, Default)]
pub struct Latencies {
    flips: VecDeque<(Duration, Presented)>,
}

impl Latencies {
    /// A flip at `at`.
    pub fn push(&mut self, at: Duration, sample: Presented) {
        if self.flips.back().is_some_and(|(last, _)| at < *last) {
            // the clock went back (a new session): start over
            self.flips.clear();
        }
        self.flips.push_back((at, sample));
        while self.flips.len() > KEEP {
            self.flips.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.flips.clear();
    }

    /// The flips of the last [`TIMING_SPAN`] before `now`, `None` when there were none.
    pub fn summary(&self, now: Duration, budget: Option<Duration>) -> Option<LatencySummary> {
        let from = now.saturating_sub(TIMING_SPAN);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let inside: Vec<Presented> = self
            .flips
            .iter()
            .filter(|(at, _)| *at > from)
            .map(|(_, sample)| *sample)
            .collect();
        let latencies: Vec<f64> = inside.iter().filter_map(|p| p.latency).map(ms).collect();
        let drifts: Vec<f64> = inside.iter().filter_map(|p| p.drift_ms).collect();
        if latencies.is_empty() && drifts.is_empty() {
            return None;
        }
        Some(LatencySummary {
            latency: (!latencies.is_empty()).then(|| LatencyFigures {
                frames: latencies.len(),
                avg_ms: latencies.iter().sum::<f64>() / latencies.len() as f64,
                min_ms: latencies.iter().copied().fold(f64::INFINITY, f64::min),
                worst_ms: latencies.iter().copied().fold(0.0, f64::max),
            }),
            drift: (!drifts.is_empty()).then(|| DriftFigures {
                flips: drifts.len(),
                avg_ms: drifts.iter().sum::<f64>() / drifts.len() as f64,
                worst_ms: drifts
                    .iter()
                    .copied()
                    .fold(0.0, |w, d| if d.abs() > w.abs() { d } else { w }),
            }),
            budget_ms: budget.filter(|b| !b.is_zero()).map(ms),
        })
    }
}

/// Where a lost sync comes from, as far as the numbers say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blame {
    /// Frames were not ready by the flip.
    Compositor,
    /// jff kept its budget and the client commits fewer frames than the monitor shows.
    Client,
    /// jff kept its budget and the client keeps up with the monitor.
    Nobody,
}

/// Who is to blame for missed refreshes, `None` while there are no cost figures or client rate.
pub fn blame(s: &Snapshot) -> Option<Blame> {
    let cost = s.cost.as_ref()?;
    let budget_ms = cost.budget_ms?;
    if cost.late > 0 {
        return Some(Blame::Compositor);
    }
    if s.vrr == Vrr::On {
        return Some(Blame::Nobody);
    }
    let client = s.client.as_ref()?;
    let refresh_hz = 1000.0 / budget_ms;
    if client.fps > 1.0 && client.fps < refresh_hz * 0.95 {
        Some(Blame::Client)
    } else {
        Some(Blame::Nobody)
    }
}

/// What the window's buffer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    /// A dmabuf, with its DRM fourcc and its format modifier.
    Dmabuf(u32, u64),
    /// Shared memory: the GPU has to copy it, and the plane can not take it.
    Shm,
    /// No buffer attached yet.
    None,
}

/// Which of the frame-pacing and sync protocols the window's surface uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientSync {
    /// wp_fifo_v1: the client sets barriers so its commits are queued vsync-style.
    pub fifo: bool,
    /// wp_commit_timing_v1: the client asks for commits to show up at a given time.
    pub commit_timing: bool,
    /// wp_linux_drm_syncobj_v1: the client's commits carry acquire and release points.
    pub explicit_sync: bool,
}

impl ClientSync {
    /// The protocols in use, by name.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.explicit_sync {
            names.push("explicit sync (syncobj)");
        }
        if self.fifo {
            names.push("fifo");
        }
        if self.commit_timing {
            names.push("commit timing");
        }
        names
    }
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
    /// The monitor is on another GPU: its frame is drawn on the primary and copied across.
    OtherGpu,
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
            Self::OtherGpu => {
                "the monitor is on another GPU, its frame is drawn on the primary and copied across"
                    .into()
            }
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
    /// What the frames cost jff over [`TIMING_SPAN`].
    pub cost: Option<CostSummary>,
    /// How many flips landed on the vblank predicted for them. `None` before the first one
    /// that can be told.
    pub sync: Option<SyncSummary>,
    /// Commit to screen and the drift from the predicted vblank over [`TIMING_SPAN`].
    pub latency: Option<LatencySummary>,
    /// How many of the last flips came off the vblank grid. `None` before there are two
    /// stamped flips to tell by, and with VRR.
    pub tearing: Option<TearingSummary>,

    // what the client asked for
    pub fullscreen: bool,
    pub wants_tearing: bool,
    /// What the window says it shows: "photo", "video" or "game".
    pub says_content: Option<&'static str>,
    /// fifo, commit timing and explicit sync, as far as the window's surface uses them.
    pub client_sync: ClientSync,
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
    /// What the monitor is told the content is: "photo", "video" or "game".
    pub content_told: Option<&'static str>,
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

/// `DRM_FORMAT_MOD_INVALID`: the modifier is implicit, the driver picks the layout.
pub const MODIFIER_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// A DRM format modifier as a name: `LINEAR`, `INVALID` (implicit), or the vendor and the code.
/// The overlay's JSON carries the plain code as well.
pub fn modifier_name(modifier: u64) -> String {
    match modifier {
        0 => "LINEAR".to_owned(),
        MODIFIER_INVALID => "INVALID (implicit)".to_owned(),
        m => {
            let vendor = match (m >> 56) as u8 {
                0x01 => "Intel",
                0x02 => "AMD",
                0x03 => "NVIDIA",
                0x04 => "Samsung",
                0x05 => "Qualcomm",
                0x06 => "Vivante",
                0x07 => "Broadcom",
                0x08 => "ARM",
                0x09 => "Allwinner",
                0x0a => "Amlogic",
                _ => "unknown vendor",
            };
            format!("{vendor} 0x{m:016x}")
        }
    }
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

/// "a game", "video" or "a photo", for the overlay's "says it shows …" row.
fn content_phrase(content: &str) -> &str {
    match content {
        "game" => "a game",
        "photo" => "a photo",
        other => other,
    }
}

fn row(label: &'static str, value: impl Into<String>, tone: Tone) -> Row {
    Row {
        label,
        value: value.into(),
        tone,
    }
}

fn ms_text(ms: f64) -> String {
    if ms >= 100.0 {
        format!("{ms:.0}")
    } else {
        format!("{ms:.1}")
    }
}

/// The lines about what frames cost jff, and whether that explains a lost sync.
fn cost_rows(s: &Snapshot) -> Vec<Row> {
    let Some(c) = &s.cost else {
        return vec![row("Frame cost", "no frames drawn yet", Tone::Neutral)];
    };
    let mut rows = vec![row(
        "CPU",
        format!(
            "{} ms avg, {} worst",
            ms_text(c.cpu_avg_ms),
            ms_text(c.cpu_worst_ms)
        ),
        Tone::Neutral,
    )];
    rows.push(match (c.gpu_avg_ms, c.gpu_worst_ms) {
        (Some(avg), Some(worst)) => row(
            "GPU wait",
            format!("{} ms avg, {} worst", ms_text(avg), ms_text(worst)),
            Tone::Neutral,
        ),
        _ => row("GPU wait", "none: nothing was waited for", Tone::Neutral),
    });
    let Some(budget) = c.budget_ms else {
        rows.push(row(
            "Frame",
            format!(
                "{} ms avg, {} worst",
                ms_text(c.total_avg_ms),
                ms_text(c.total_worst_ms)
            ),
            Tone::Neutral,
        ));
        return rows;
    };
    let avg_share = c.total_avg_ms / budget;
    let worst_share = c.total_worst_ms / budget;
    rows.push(row(
        "Budget",
        format!(
            "{} of {} ms ({:.0}%) avg, {} worst ({:.0}%)",
            ms_text(c.total_avg_ms),
            ms_text(budget),
            avg_share * 100.0,
            ms_text(c.total_worst_ms),
            worst_share * 100.0
        ),
        if c.late > 0 || avg_share > 0.8 {
            Tone::Warn
        } else {
            Tone::Good
        },
    ));
    rows.push(row(
        "Late frames",
        if c.late == 0 {
            format!("none of {}", c.frames)
        } else {
            format!("{} of {} took longer than the refresh", c.late, c.frames)
        },
        if c.late == 0 { Tone::Good } else { Tone::Warn },
    ));
    match blame(s) {
        Some(Blame::Compositor) => rows.push(row(
            "Verdict",
            "jff: frames were not ready in time",
            Tone::Warn,
        )),
        Some(Blame::Client) => rows.push(row(
            "Verdict",
            "the client: it commits fewer frames than the monitor shows, jff kept its budget",
            Tone::Warn,
        )),
        Some(Blame::Nobody) => rows.push(row(
            "Verdict",
            "jff kept its budget, and the client keeps up",
            Tone::Good,
        )),
        None => {}
    }
    rows
}

/// The lines about flips that missed the vblank predicted for them, and the sync target.
fn sync_rows(s: &Snapshot) -> Vec<Row> {
    // under VRR a flip comes when the frame is ready, and a tearing one does not wait for the
    // vblank: there is no predicted vblank to miss
    if s.vrr == Vrr::On {
        return vec![row(
            "Vblank sync",
            "n/a: VRR flips when the frame is ready",
            Tone::Neutral,
        )];
    }
    if s.async_flip {
        return vec![row(
            "Vblank sync",
            "n/a: tearing flips do not wait for the vblank",
            Tone::Neutral,
        )];
    }
    let Some(sync) = &s.sync else {
        return vec![row("Vblank sync", "no flips counted yet", Tone::Neutral)];
    };
    // 100.00 would say nothing was missed
    let percent = if sync.missed > 0 {
        sync.percent().min(99.99)
    } else {
        sync.percent()
    };
    vec![
        row(
            "Vblank sync",
            format!("{percent:.2}% on time (target {SYNC_TARGET}%)"),
            if sync.on_target() {
                Tone::Good
            } else {
                Tone::Warn
            },
        ),
        row(
            "Missed vblank",
            format!(
                "{} of {} flips, {} in the last {} s",
                sync.missed,
                sync.flips,
                sync.recent_missed,
                TIMING_SPAN.as_secs()
            ),
            if sync.missed == 0 {
                Tone::Good
            } else if sync.recent_missed > 0 {
                Tone::Warn
            } else {
                Tone::Neutral
            },
        ),
    ]
}

/// The lines about the time from a commit to the screen, and the drift of the flip that
/// showed it from the vblank predicted for it.
fn latency_rows(s: &Snapshot) -> Vec<Row> {
    let Some(l) = &s.latency else {
        return vec![row(
            "Latency",
            format!("no new frame in the last {} s", TIMING_SPAN.as_secs()),
            Tone::Neutral,
        )];
    };
    let mut rows = Vec::new();
    rows.push(match &l.latency {
        // more than two refreshes: the content waited behind another frame
        Some(f) => row(
            "Latency",
            format!(
                "commit to screen {} ms avg, {} best, {} worst",
                ms_text(f.avg_ms),
                ms_text(f.min_ms),
                ms_text(f.worst_ms)
            ),
            if l.budget_ms.is_some_and(|b| f.avg_ms > b * 2.0) {
                Tone::Warn
            } else {
                Tone::Neutral
            },
        ),
        None => row("Latency", "no new frame of the window shown", Tone::Neutral),
    });
    // the same cases as the vblank sync rows: there is no predicted vblank to drift from
    if s.vrr == Vrr::On {
        rows.push(row(
            "Drift",
            "n/a: VRR flips when the frame is ready",
            Tone::Neutral,
        ));
    } else if s.async_flip {
        rows.push(row(
            "Drift",
            "n/a: tearing flips do not wait for the vblank",
            Tone::Neutral,
        ));
    } else {
        rows.push(match &l.drift {
            Some(d) => row(
                "Drift",
                format!(
                    "{:+.2} ms avg, {:+.2} worst from the predicted vblank",
                    d.avg_ms, d.worst_ms
                ),
                // half a refresh is where a flip counts as a missed vblank
                match l.budget_ms {
                    Some(b) if d.worst_ms.abs() > b / 2.0 => Tone::Warn,
                    Some(_) => Tone::Good,
                    None => Tone::Neutral,
                },
            ),
            None => row("Drift", "no flips counted yet", Tone::Neutral),
        });
    }
    rows
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
    frames.extend(sync_rows(s));
    out.push(Section {
        title: "Frames",
        rows: frames,
    });

    // ---- commit to screen ---------------------------------------------------------------
    out.push(Section {
        title: "Latency",
        rows: latency_rows(s),
    });

    // ---- what the frames cost jff -------------------------------------------------------
    out.push(Section {
        title: "Compositor cost",
        rows: cost_rows(s),
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
    if let Some(content) = s.says_content {
        asked.push(row(
            "Content",
            format!("says it shows {}", content_phrase(content)),
            Tone::Neutral,
        ));
    }
    let buffer = match s.buffer {
        BufferKind::Dmabuf(code, _) => {
            row("Buffer", format!("dmabuf {}", fourcc(code)), Tone::Good)
        }
        BufferKind::Shm => row(
            "Buffer",
            "shared memory: copied by the GPU, can not go to the plane",
            Tone::Warn,
        ),
        BufferKind::None => row("Buffer", "none yet", Tone::Neutral),
    };
    asked.push(buffer);
    // the layout of the dmabuf: the plane takes only the modifiers it lists, so a refusal
    // ("format, modifier or size") is most often this one
    if let BufferKind::Dmabuf(_, modifier) = s.buffer {
        asked.push(if modifier == MODIFIER_INVALID {
            row(
                "Modifier",
                "implicit: the driver picks the layout, the plane may refuse it",
                Tone::Warn,
            )
        } else {
            row("Modifier", modifier_name(modifier), Tone::Neutral)
        });
    }
    let used = s.client_sync.names();
    asked.push(row(
        "Sync",
        if !used.is_empty() {
            used.join(", ")
        } else if matches!(s.buffer, BufferKind::Dmabuf(..)) {
            // no explicit sync: the buffer's own fences are what the compositor waits on
            "implicit (the dmabuf's fences), no fifo, no commit timing".to_owned()
        } else {
            "none of fifo, commit timing, explicit sync".to_owned()
        },
        Tone::Neutral,
    ));
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
            match s.tearing.filter(TearingSummary::told) {
                Some(t) if t.seen() => row(
                    "Tearing result",
                    format!(
                        "seen: {} of {} flips off the vblank grid",
                        t.off_grid, t.flips
                    ),
                    Tone::Good,
                ),
                // asked for, and every flip still waited for the vblank
                Some(t) => row(
                    "Tearing result",
                    format!(
                        "requested, not seen: {} of {} flips off the vblank grid",
                        t.off_grid, t.flips
                    ),
                    Tone::Warn,
                ),
                None => row("Tearing result", "async flip requested", Tone::Good),
            }
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
    if let Some(content) = s.content_told {
        did.push(row(
            "Content type",
            format!("monitor told: {content}"),
            Tone::Neutral,
        ));
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

fn json_cost(cost: &Option<CostSummary>) -> String {
    let opt = |v: Option<f64>| v.map_or("null".to_owned(), |v| format!("{v:.3}"));
    match cost {
        None => "null".to_owned(),
        Some(c) => format!(
            "{{\"frames\":{},\"cpu_avg_ms\":{:.3},\"cpu_worst_ms\":{:.3},\"gpu_wait_avg_ms\":{},\"gpu_wait_worst_ms\":{},\"total_avg_ms\":{:.3},\"total_worst_ms\":{:.3},\"budget_ms\":{},\"budget_avg_share\":{},\"budget_worst_share\":{},\"late_frames\":{}}}",
            c.frames,
            c.cpu_avg_ms,
            c.cpu_worst_ms,
            opt(c.gpu_avg_ms),
            opt(c.gpu_worst_ms),
            c.total_avg_ms,
            c.total_worst_ms,
            opt(c.budget_ms),
            opt(c.avg_share()),
            opt(c.worst_share()),
            c.late
        ),
    }
}

fn json_sync(sync: &Option<SyncSummary>) -> String {
    match sync {
        None => "null".to_owned(),
        Some(y) => format!(
            "{{\"flips\":{},\"missed\":{},\"recent_flips\":{},\"recent_missed\":{},\"percent\":{:.3},\"target_percent\":{:.1},\"on_target\":{}}}",
            y.flips,
            y.missed,
            y.recent_flips,
            y.recent_missed,
            y.percent(),
            SYNC_TARGET,
            y.on_target()
        ),
    }
}

fn json_tearing(tearing: &Option<TearingSummary>) -> String {
    tearing.map_or("null".to_owned(), |t| {
        format!(
            "{{\"flips\":{},\"off_grid\":{},\"seen\":{}}}",
            t.flips,
            t.off_grid,
            t.seen()
        )
    })
}

fn json_latency(latency: &Option<LatencySummary>) -> String {
    let Some(l) = latency else {
        return "null".to_owned();
    };
    let commit = l.latency.map_or("null".to_owned(), |f| {
        format!(
            "{{\"frames\":{},\"avg_ms\":{:.3},\"min_ms\":{:.3},\"worst_ms\":{:.3}}}",
            f.frames, f.avg_ms, f.min_ms, f.worst_ms
        )
    });
    let drift = l.drift.map_or("null".to_owned(), |d| {
        format!(
            "{{\"flips\":{},\"avg_ms\":{:.3},\"worst_ms\":{:.3}}}",
            d.flips, d.avg_ms, d.worst_ms
        )
    });
    format!("{{\"commit_to_screen\":{commit},\"vblank_drift\":{drift}}}")
}

/// The snapshot as one JSON object, for `jff msg stats`.
pub fn to_json(s: &Snapshot) -> String {
    let (path, reason) = match &s.path {
        Path::Direct => ("direct", "null".to_owned()),
        Path::Composited(why) => ("composited", json_str(&why.text())),
    };
    let buffer = match s.buffer {
        BufferKind::Dmabuf(code, _) => format!("\"dmabuf:{}\"", fourcc(code)),
        BufferKind::Shm => "\"shm\"".to_owned(),
        BufferKind::None => "null".to_owned(),
    };
    let buffer_modifier = match s.buffer {
        BufferKind::Dmabuf(_, modifier) => format!(
            "{{\"code\":\"0x{modifier:016x}\",\"name\":{}}}",
            json_str(&modifier_name(modifier))
        ),
        _ => "null".to_owned(),
    };
    let sync_protocols = format!(
        "{{\"fifo\":{},\"commit_timing\":{},\"explicit_sync\":{}}}",
        s.client_sync.fifo, s.client_sync.commit_timing, s.client_sync.explicit_sync
    );
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
        "{{\"app_id\":{},\"output\":{},\"client\":{},\"shown\":{},\"display_hz\":{:.2},\"cost\":{},\"sync\":{},\"latency\":{},\"blame\":{},\
         \"client_asked\":{{\"fullscreen\":{},\"tearing\":{},\"game\":{},\"content\":{},\"buffer\":{},\"buffer_modifier\":{},\"sync_protocols\":{},\"buffer_size\":{},\"window_size\":{},\"colors\":{}}},\
         \"compositor\":{{\"path\":\"{}\",\"reason\":{},\"scanout_without_overlay\":{},\"allow_tearing\":{},\"adaptive_sync_policy\":{}}},\
         \"hardware\":{{\"vrr\":\"{}\",\"hdr\":{},\"async_flip\":{},\"tearing_observed\":{},\"content_type_game\":{},\"content_type\":{}}}}}",
        json_str(&s.app_id),
        json_str(&s.output),
        json_timing(&s.client),
        json_timing(&s.shown),
        s.display_hz,
        json_cost(&s.cost),
        json_sync(&s.sync),
        json_latency(&s.latency),
        match blame(s) {
            Some(Blame::Compositor) => "\"compositor\"",
            Some(Blame::Client) => "\"client\"",
            Some(Blame::Nobody) => "\"nobody\"",
            None => "null",
        },
        s.fullscreen,
        s.wants_tearing,
        s.says_content == Some("game"),
        s.says_content.map_or("null".to_owned(), json_str),
        buffer,
        buffer_modifier,
        sync_protocols,
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
        json_tearing(&s.tearing),
        s.content_told == Some("game"),
        s.content_told.map_or("null".to_owned(), json_str),
    )
}

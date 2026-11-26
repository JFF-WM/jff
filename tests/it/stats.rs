//! Tests for `jff_wm::primitives::stats`.

use std::time::Duration;

use jff_wm::primitives::color::{ImageDescription, NamedPrimaries, NamedTf, TransferFunction};
use jff_wm::primitives::stats::{
    blame, budget_of, commit_latency, countable_prediction, drift_ms, flip_off_grid, fourcc,
    missed_vblank, modifier_name, primaries_name, sections, tf_name, to_json, Blame, BufferKind,
    ClientSync, Costs, Cursor, CursorWhy, FrameCost, Latencies, Path, Presented, Row, Snapshot,
    SyncSummary, Syncs, TearingSummary, Tearings, Ticks, Timing, Tone, Vrr, Why, MODIFIER_INVALID,
    SYNC_TARGET,
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// `count` events `every_us` microseconds apart, the first at `start`.
fn steady(start: Duration, every_us: u64, count: u64) -> Ticks {
    let mut ticks = Ticks::default();
    for i in 0..count {
        ticks.push(start + Duration::from_micros(i * every_us));
    }
    ticks
}

#[test]
fn a_steady_rate_is_measured() {
    // 60 Hz for two seconds
    let ticks = steady(ms(1000), 16_667, 120);
    let now = ms(1000) + Duration::from_micros(119 * 16_667);
    let rate = ticks.rate(now);
    assert!((rate - 60.0).abs() < 1.0, "rate was {rate}");
}

#[test]
fn nothing_in_the_last_second_is_a_rate_of_zero() {
    let ticks = steady(ms(1000), 10_000, 10);
    assert_eq!(ticks.rate(ms(5000)), 0.0);
    assert_eq!(Ticks::default().rate(ms(5000)), 0.0);
}

#[test]
fn timing_needs_three_events() {
    let mut ticks = Ticks::default();
    assert!(ticks.timing(ms(100)).is_none());
    ticks.push(ms(10));
    ticks.push(ms(20));
    assert!(ticks.timing(ms(100)).is_none());
    ticks.push(ms(30));
    assert!(ticks.timing(ms(100)).is_some());
}

#[test]
fn timing_of_even_frames_has_no_jitter() {
    let ticks = steady(ms(1000), 10_000, 100);
    let now = ms(1000) + Duration::from_micros(99 * 10_000);
    let timing = ticks.timing(now).expect("enough frames");
    assert!((timing.avg_ms - 10.0).abs() < 0.01);
    assert!((timing.worst_ms - 10.0).abs() < 0.01);
    assert!(timing.jitter_ms < 0.01);
    assert!((timing.low1_fps - 100.0).abs() < 0.5);
}

#[test]
fn a_hitch_shows_in_worst_jitter_and_one_percent_low() {
    // 99 frames at 10 ms and one of 50 ms
    let mut ticks = Ticks::default();
    let mut at = ms(1000);
    for i in 0..101 {
        ticks.push(at);
        at += if i == 50 { ms(50) } else { ms(10) };
    }
    let timing = ticks.timing(at).expect("enough frames");
    assert!((timing.worst_ms - 50.0).abs() < 0.01);
    assert!(timing.jitter_ms > 3.0);
    // the slowest 1% of 100 intervals is that one frame: 1000 / 50
    assert!(
        (timing.low1_fps - 20.0).abs() < 0.5,
        "low was {}",
        timing.low1_fps
    );
    assert!(timing.avg_ms > 10.0 && timing.avg_ms < 11.0);
}

#[test]
fn events_closer_than_half_a_millisecond_are_one() {
    // a window and its subsurface committing together
    let mut ticks = Ticks::default();
    ticks.push(ms(100));
    ticks.push(ms(100) + Duration::from_micros(200));
    assert_eq!(ticks.total(), 1);
    ticks.push(ms(101));
    assert_eq!(ticks.total(), 2);
}

#[test]
fn the_clock_going_back_starts_over() {
    let mut ticks = steady(ms(10_000), 10_000, 10);
    ticks.push(ms(5));
    assert!(!ticks.is_empty());
    // only the new event is left to rate
    assert_eq!(ticks.rate(ms(6)), 1.0);
}

#[test]
fn clear_forgets_everything() {
    let mut ticks = steady(ms(1000), 10_000, 10);
    ticks.clear();
    assert!(ticks.is_empty());
    assert_eq!(ticks.total(), 0);
}

fn pq_bt2020() -> ImageDescription {
    let mut color = ImageDescription::srgb();
    color.primaries = NamedPrimaries::Bt2020.chromaticities();
    color.tf = TransferFunction::Named(NamedTf::St2084Pq);
    color
}

fn snapshot() -> Snapshot {
    Snapshot {
        app_id: "game".into(),
        output: "DP-1".into(),
        mode: Some((2560, 1440, 144_000)),
        client: None,
        shown: None,
        display_hz: 144.0,
        cost: None,
        sync: None,
        latency: None,
        tearing: None,
        fullscreen: true,
        wants_tearing: false,
        says_content: None,
        client_sync: ClientSync::default(),
        buffer: BufferKind::Dmabuf(0x3432_5258, 0),
        buffer_size: Some((2560, 1440)),
        window_size: (2560, 1440),
        color: None,
        allow_tearing: false,
        vrr_policy: "fullscreen",
        path: Path::Direct,
        scanout_without_overlay: None,
        cursor: Some(Cursor::Hardware),
        vrr: Vrr::On,
        hdr_output: false,
        content_told: None,
        async_flip: false,
    }
}

fn find(snapshot: &Snapshot, label: &str) -> Row {
    sections(snapshot)
        .into_iter()
        .flat_map(|section| section.rows)
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("no row {label:?}"))
}

#[test]
fn direct_scanout_is_good_news() {
    let row = find(&snapshot(), "Direct scanout");
    assert!(row.value.starts_with("yes"));
    assert_eq!(row.tone, Tone::Good);
    assert_eq!(
        find(&snapshot(), "Conversion").value,
        "none: passed through"
    );
}

#[test]
fn the_overlay_says_it_is_why_there_is_no_scanout() {
    let mut s = snapshot();
    s.path = Path::Composited(Why::StatsOverlay);
    s.scanout_without_overlay = Some(true);
    let row = find(&s, "Direct scanout");
    assert!(row.value.contains("this overlay"), "{}", row.value);
    assert_eq!(row.tone, Tone::Warn);
    let without = find(&s, "Without overlay");
    assert!(without.value.contains("hide this"));
}

#[test]
fn without_the_overlay_row_when_something_else_is_the_reason() {
    let mut s = snapshot();
    s.path = Path::Composited(Why::LayerOver);
    let rows: Vec<_> = sections(&s).into_iter().flat_map(|x| x.rows).collect();
    assert!(rows.iter().all(|row| row.label != "Without overlay"));
}

#[test]
fn a_window_that_is_not_fullscreen_is_not_a_warning() {
    let mut s = snapshot();
    s.fullscreen = false;
    s.path = Path::Composited(Why::NotFullscreen);
    assert_eq!(find(&s, "Direct scanout").tone, Tone::Neutral);
}

#[test]
fn tearing_that_was_asked_for_says_what_became_of_it() {
    let mut s = snapshot();
    s.wants_tearing = true;
    s.allow_tearing = false;
    s.path = Path::Composited(Why::StatsOverlay);
    assert!(find(&s, "Tearing result")
        .value
        .contains("allow-tearing is off"));

    s.allow_tearing = true;
    assert!(find(&s, "Tearing result").value.contains("this overlay"));

    s.path = Path::Composited(Why::LayerOver);
    assert!(find(&s, "Tearing result").value.contains("direct scanout"));

    s.path = Path::Direct;
    s.async_flip = true;
    assert!(find(&s, "Tearing result").value.contains("async flip"));
    assert!(find(&s, "Frame sync").value.starts_with("tearing"));
}

#[test]
fn tearing_has_no_outcome_when_the_client_did_not_ask() {
    let rows: Vec<_> = sections(&snapshot())
        .into_iter()
        .flat_map(|x| x.rows)
        .collect();
    assert!(rows.iter().all(|row| row.label != "Tearing result"));
}

#[test]
fn frame_sync_follows_vrr_and_tearing() {
    let mut s = snapshot();
    assert!(find(&s, "Frame sync").value.starts_with("adaptive"));
    s.vrr = Vrr::Off;
    assert!(find(&s, "Frame sync").value.starts_with("vsync"));
    s.vrr = Vrr::Unsupported;
    assert!(find(&s, "VRR").value.contains("not supported"));
}

#[test]
fn shm_buffers_and_scaling_are_flagged() {
    let mut s = snapshot();
    s.buffer = BufferKind::Shm;
    assert_eq!(find(&s, "Buffer").tone, Tone::Warn);
    s.buffer_size = Some((1280, 720));
    let size = find(&s, "Size");
    assert_eq!(size.tone, Tone::Warn);
    assert!(size.value.contains("1280×720") && size.value.contains("2560×1440"));
}

#[test]
fn an_hdr_window_on_an_sdr_output_is_tone_mapped() {
    let mut s = snapshot();
    s.path = Path::Composited(Why::LayerOver);
    s.color = Some(pq_bt2020());
    let row = find(&s, "Conversion");
    assert!(row.value.contains("tone-mapped"));
    assert_eq!(row.tone, Tone::Warn);
    s.hdr_output = true;
    assert!(find(&s, "Conversion").value.contains("HDR window"));
    assert_eq!(find(&s, "HDR").value, "on");
}

#[test]
fn an_idle_display_says_so() {
    let mut s = snapshot();
    s.display_hz = 0.0;
    assert!(find(&s, "Display").value.contains("idle"));
}

#[test]
fn shown_below_client_is_a_warning() {
    let timing = |fps| Timing {
        fps,
        avg_ms: 1000.0 / fps,
        worst_ms: 1000.0 / fps,
        low1_fps: fps,
        jitter_ms: 0.1,
    };
    let mut s = snapshot();
    s.client = Some(timing(144.0));
    s.shown = Some(timing(60.0));
    assert_eq!(find(&s, "Shown").tone, Tone::Warn);
    s.shown = Some(timing(143.0));
    assert_eq!(find(&s, "Shown").tone, Tone::Good);
}

#[test]
fn names_of_colors_and_formats() {
    assert_eq!(fourcc(0x3432_5258), "XR24");
    assert_eq!(primaries_name(&ImageDescription::srgb()), "sRGB/BT.709");
    assert_eq!(primaries_name(&pq_bt2020()), "BT.2020");
    assert_eq!(tf_name(&pq_bt2020()), "PQ");
    assert_eq!(tf_name(&ImageDescription::srgb()), "sRGB");
}

#[test]
fn json_has_the_three_columns_and_is_balanced() {
    let mut s = snapshot();
    s.app_id = "a\"b".into();
    s.path = Path::Composited(Why::StatsOverlay);
    let json = to_json(&s);
    for key in [
        "\"client_asked\"",
        "\"compositor\"",
        "\"hardware\"",
        "\"path\":\"composited\"",
        "\"vrr\":\"on\"",
        "\"app_id\":\"a\\\"b\"",
    ] {
        assert!(json.contains(key), "{key} missing in {json}");
    }
    let open = json.matches('{').count();
    let close = json.matches('}').count();
    assert_eq!(open, close);
    assert!(!json.contains('\n'));
}

fn cost(cpu_us: u64, gpu_us: Option<u64>) -> FrameCost {
    FrameCost {
        cpu: Duration::from_micros(cpu_us),
        gpu_wait: gpu_us.map(Duration::from_micros),
    }
}

/// `count` frames of `cost`, one every `every_us` microseconds from 1 s on.
fn frames(count: u64, every_us: u64, each: FrameCost) -> Costs {
    let mut costs = Costs::default();
    for i in 0..count {
        costs.push(ms(1000) + Duration::from_micros(i * every_us), each);
    }
    costs
}

fn timing_at(fps: f64) -> Timing {
    Timing {
        fps,
        avg_ms: 1000.0 / fps,
        worst_ms: 1000.0 / fps,
        low1_fps: fps,
        jitter_ms: 0.1,
    }
}

#[test]
fn the_budget_is_the_refresh_interval() {
    let b = budget_of(60_000).unwrap();
    assert!((b.as_secs_f64() * 1000.0 - 16.667).abs() < 0.01);
    assert_eq!(budget_of(0), None);
    assert_eq!(budget_of(-1), None);
}

#[test]
fn costs_are_averaged_and_the_worst_is_kept() {
    let mut costs = Costs::default();
    costs.push(ms(1000), cost(2000, Some(4000)));
    costs.push(ms(1017), cost(4000, Some(8000)));
    let sum = costs.summary(ms(1100), budget_of(60_000)).unwrap();
    assert_eq!(sum.frames, 2);
    assert!((sum.cpu_avg_ms - 3.0).abs() < 1e-9);
    assert!((sum.cpu_worst_ms - 4.0).abs() < 1e-9);
    assert!((sum.gpu_avg_ms.unwrap() - 6.0).abs() < 1e-9);
    assert!((sum.gpu_worst_ms.unwrap() - 8.0).abs() < 1e-9);
    assert!((sum.total_avg_ms - 9.0).abs() < 1e-9);
    assert!((sum.total_worst_ms - 12.0).abs() < 1e-9);
    assert_eq!(sum.late, 0);
    assert!((sum.avg_share().unwrap() - 9.0 / 16.667).abs() < 0.01);
}

#[test]
fn a_frame_over_the_refresh_interval_is_late() {
    let mut costs = Costs::default();
    costs.push(ms(1000), cost(6000, None));
    costs.push(ms(1017), cost(10_000, Some(9000)));
    let sum = costs.summary(ms(1100), budget_of(60_000)).unwrap();
    assert_eq!(sum.late, 1);
    assert_eq!(sum.gpu_avg_ms, Some(9.0));
}

#[test]
fn frames_without_a_gpu_wait_have_none() {
    let costs = frames(10, 6_944, cost(1500, None));
    let sum = costs.summary(ms(1100), budget_of(144_000)).unwrap();
    assert_eq!(sum.gpu_avg_ms, None);
    assert_eq!(sum.gpu_worst_ms, None);
    assert_eq!(sum.late, 0);
}

#[test]
fn old_frames_fall_out_of_the_span_and_nothing_is_none() {
    let costs = frames(10, 6_944, cost(1500, None));
    assert!(costs.summary(ms(60_000), budget_of(144_000)).is_none());
    assert!(Costs::default().summary(ms(1000), None).is_none());
}

#[test]
fn an_unknown_mode_has_no_budget_and_no_late_frames() {
    let costs = frames(10, 6_944, cost(50_000, None));
    let sum = costs.summary(ms(1100), None).unwrap();
    assert_eq!(sum.budget_ms, None);
    assert_eq!(sum.late, 0);
    assert_eq!(sum.avg_share(), None);
}

#[test]
fn the_cost_rows_show_the_share_of_the_budget() {
    let mut s = snapshot();
    s.cost = frames(100, 6_944, cost(2000, Some(4000))).summary(ms(1700), budget_of(144_000));
    let budget = find(&s, "Budget");
    assert!(budget.value.contains("6.0 of 6.9 ms"), "{}", budget.value);
    assert!(budget.value.contains("(86%)"), "{}", budget.value);
    assert_eq!(budget.tone, Tone::Warn);
    assert_eq!(find(&s, "CPU").value, "2.0 ms avg, 2.0 worst");
    assert_eq!(find(&s, "GPU wait").value, "4.0 ms avg, 4.0 worst");
    assert_eq!(find(&s, "Late frames").value, "none of 100");

    s.cost = frames(100, 6_944, cost(1000, Some(1000))).summary(ms(1700), budget_of(144_000));
    assert_eq!(find(&s, "Budget").tone, Tone::Good);
}

#[test]
fn no_cost_yet_says_so() {
    let row = find(&snapshot(), "Frame cost");
    assert!(row.value.contains("no frames"), "{}", row.value);
}

#[test]
fn late_frames_are_jffs_fault() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.client = Some(timing_at(144.0));
    let mut costs = frames(99, 6_944, cost(2000, None));
    costs.push(ms(1700), cost(9000, Some(3000)));
    s.cost = costs.summary(ms(1701), budget_of(144_000));
    assert_eq!(blame(&s), Some(Blame::Compositor));
    let verdict = find(&s, "Verdict");
    assert!(verdict.value.starts_with("jff"), "{}", verdict.value);
    assert_eq!(verdict.tone, Tone::Warn);
    assert_eq!(find(&s, "Late frames").tone, Tone::Warn);
}

#[test]
fn a_slow_client_with_jff_inside_its_budget_is_the_clients_fault() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.client = Some(timing_at(90.0));
    s.cost = frames(100, 6_944, cost(1500, Some(500))).summary(ms(1700), budget_of(144_000));
    assert_eq!(blame(&s), Some(Blame::Client));
    assert!(find(&s, "Verdict").value.starts_with("the client"));
}

#[test]
fn a_client_that_keeps_up_and_a_jff_within_budget_blame_nobody() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.client = Some(timing_at(144.0));
    s.cost = frames(100, 6_944, cost(1500, Some(500))).summary(ms(1700), budget_of(144_000));
    assert_eq!(blame(&s), Some(Blame::Nobody));
    assert_eq!(find(&s, "Verdict").tone, Tone::Good);
}

#[test]
fn a_slow_client_is_not_a_lost_sync_under_vrr() {
    let mut s = snapshot();
    s.vrr = Vrr::On;
    s.client = Some(timing_at(60.0));
    s.cost = frames(100, 6_944, cost(1500, None)).summary(ms(1700), budget_of(144_000));
    assert_eq!(blame(&s), Some(Blame::Nobody));
}

#[test]
fn there_is_no_verdict_without_costs_or_a_client_rate() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    assert_eq!(blame(&s), None);
    s.cost = frames(100, 6_944, cost(1500, None)).summary(ms(1700), budget_of(144_000));
    s.client = None;
    assert_eq!(blame(&s), None);
    let rows: Vec<_> = sections(&s).into_iter().flat_map(|x| x.rows).collect();
    assert!(rows.iter().all(|row| row.label != "Verdict"));
}

#[test]
fn json_carries_the_cost_and_the_verdict() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.client = Some(timing_at(144.0));
    let json = to_json(&s);
    assert!(json.contains("\"cost\":null"), "{json}");
    assert!(json.contains("\"blame\":null"), "{json}");

    s.cost = frames(100, 6_944, cost(1500, Some(500))).summary(ms(1700), budget_of(144_000));
    let json = to_json(&s);
    for key in [
        "\"cpu_avg_ms\":1.500",
        "\"gpu_wait_avg_ms\":0.500",
        "\"total_worst_ms\":2.000",
        "\"late_frames\":0",
        "\"blame\":\"nobody\"",
    ] {
        assert!(json.contains(key), "{key} missing in {json}");
    }
    assert_eq!(json.matches('{').count(), json.matches('}').count());
}

// ---- flips that missed the predicted vblank ----------------------------------------------

fn refresh_60() -> Duration {
    Duration::from_nanos(16_666_667)
}

#[test]
fn a_flip_on_the_predicted_vblank_is_on_time() {
    let predicted = ms(1000);
    assert!(!missed_vblank(predicted, predicted, refresh_60()));
    // a little jitter in the kernel's timestamp is not a miss
    assert!(!missed_vblank(
        predicted,
        predicted + Duration::from_micros(300),
        refresh_60()
    ));
    assert!(!missed_vblank(
        predicted,
        predicted - Duration::from_micros(300),
        refresh_60()
    ));
}

#[test]
fn a_flip_a_refresh_late_missed() {
    let predicted = ms(1000);
    assert!(missed_vblank(
        predicted,
        predicted + refresh_60(),
        refresh_60()
    ));
    assert!(missed_vblank(
        predicted,
        predicted + refresh_60() * 3,
        refresh_60()
    ));
}

#[test]
fn only_a_fresh_frame_clock_is_counted() {
    let refresh = refresh_60();
    // no vblank seen yet
    assert_eq!(countable_prediction(None, ms(1000), refresh), None);
    // the usual case: drawn a refresh after the last vblank
    let last = ms(1000);
    assert_eq!(
        countable_prediction(Some(last), last + refresh, refresh),
        Some(last + refresh)
    );
    // the monitor idled for a second: the grid has drifted
    assert_eq!(
        countable_prediction(Some(last), last + ms(1000), refresh),
        None
    );
}

#[test]
fn syncs_count_flips_and_misses() {
    let mut syncs = Syncs::default();
    assert_eq!(syncs.summary(ms(1000)), None);
    for i in 0..10u64 {
        syncs.push(ms(1000 + i * 16), i == 4);
    }
    let sum = syncs.summary(ms(1200)).expect("flips counted");
    assert_eq!((sum.flips, sum.missed), (10, 1));
    assert_eq!((sum.recent_flips, sum.recent_missed), (10, 1));
    assert!((sum.percent() - 90.0).abs() < 1e-9);
}

#[test]
fn old_misses_leave_the_recent_figure_but_not_the_total() {
    let mut syncs = Syncs::default();
    syncs.push(ms(1000), true);
    for i in 0..10u64 {
        syncs.push(ms(10_000 + i * 16), false);
    }
    let sum = syncs.summary(ms(10_200)).expect("flips counted");
    assert_eq!((sum.flips, sum.missed), (11, 1));
    assert_eq!((sum.recent_flips, sum.recent_missed), (10, 0));
}

#[test]
fn the_totals_outlast_the_rolling_window() {
    let mut syncs = Syncs::default();
    for i in 0..3000u64 {
        syncs.push(ms(i * 16), false);
    }
    let sum = syncs.summary(ms(2999 * 16)).expect("flips counted");
    assert_eq!(sum.flips, 3000);
}

#[test]
fn syncs_start_over_when_the_clock_goes_back() {
    let mut syncs = Syncs::default();
    syncs.push(ms(5000), true);
    syncs.push(ms(100), false);
    let sum = syncs.summary(ms(200)).expect("flips counted");
    assert_eq!((sum.flips, sum.missed), (1, 0));
}

#[test]
fn the_target_is_one_miss_in_a_thousand() {
    let at = |flips, missed| SyncSummary {
        flips,
        missed,
        recent_flips: 0,
        recent_missed: 0,
    };
    assert!(at(1000, 1).on_target());
    assert!(!at(1000, 2).on_target());
    assert!(at(0, 0).on_target());
    assert_eq!(SYNC_TARGET, 99.9);
}

fn with_sync(flips: u64, missed: u64, recent_missed: usize) -> Snapshot {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.sync = Some(SyncSummary {
        flips,
        missed,
        recent_flips: 300,
        recent_missed,
    });
    s
}

#[test]
fn the_overlay_shows_the_sync_against_the_target() {
    let s = with_sync(5000, 2, 0);
    let sync = find(&s, "Vblank sync");
    assert_eq!(sync.value, "99.96% on time (target 99.9%)");
    assert_eq!(sync.tone, Tone::Good);
    let missed = find(&s, "Missed vblank");
    assert_eq!(missed.value, "2 of 5000 flips, 0 in the last 5 s");
    assert_eq!(missed.tone, Tone::Neutral);
}

#[test]
fn below_the_target_is_a_warning() {
    let s = with_sync(1000, 20, 3);
    assert_eq!(find(&s, "Vblank sync").tone, Tone::Warn);
    assert_eq!(find(&s, "Missed vblank").tone, Tone::Warn);
}

#[test]
fn no_misses_is_good_news() {
    let s = with_sync(1000, 0, 0);
    assert_eq!(
        find(&s, "Vblank sync").value,
        "100.00% on time (target 99.9%)"
    );
    assert_eq!(find(&s, "Missed vblank").tone, Tone::Good);
}

#[test]
fn a_miss_never_rounds_up_to_a_hundred_percent() {
    let s = with_sync(1_000_000, 1, 0);
    assert_eq!(
        find(&s, "Vblank sync").value,
        "99.99% on time (target 99.9%)"
    );
}

#[test]
fn there_is_no_sync_to_miss_under_vrr_or_tearing() {
    let mut s = with_sync(1000, 5, 1);
    s.vrr = Vrr::On;
    assert!(find(&s, "Vblank sync").value.starts_with("n/a"));
    assert_eq!(find(&s, "Vblank sync").tone, Tone::Neutral);
    s.vrr = Vrr::Off;
    s.async_flip = true;
    assert!(find(&s, "Vblank sync").value.starts_with("n/a"));
    let rows: Vec<_> = sections(&s).into_iter().flat_map(|x| x.rows).collect();
    assert!(rows.iter().all(|row| row.label != "Missed vblank"));
}

#[test]
fn before_the_first_flip_the_overlay_says_so() {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    assert_eq!(find(&s, "Vblank sync").value, "no flips counted yet");
}

#[test]
fn json_carries_the_sync() {
    let mut s = snapshot();
    assert!(to_json(&s).contains("\"sync\":null"));
    s.sync = Some(SyncSummary {
        flips: 5000,
        missed: 2,
        recent_flips: 300,
        recent_missed: 0,
    });
    let json = to_json(&s);
    for key in [
        "\"sync\":{\"flips\":5000,\"missed\":2",
        "\"recent_missed\":0",
        "\"percent\":99.960",
        "\"target_percent\":99.9",
        "\"on_target\":true",
    ] {
        assert!(json.contains(key), "{key} missing in {json}");
    }
    assert_eq!(json.matches('{').count(), json.matches('}').count());
}

// ---- commit to screen and the drift from the vblank ---------------------------------------

fn us(n: u64) -> Duration {
    Duration::from_micros(n)
}

fn shown(latency_us: u64, drift: f64) -> Presented {
    Presented {
        latency: Some(us(latency_us)),
        drift_ms: Some(drift),
    }
}

#[test]
fn latency_is_the_time_from_commit_to_flip() {
    assert_eq!(commit_latency(ms(1000), ms(1012)), Some(ms(12)));
    assert_eq!(commit_latency(ms(1000), ms(1000)), Some(Duration::ZERO));
    // a flip stamped before the commit says nothing
    assert_eq!(commit_latency(ms(1012), ms(1000)), None);
}

#[test]
fn drift_is_signed() {
    assert!((drift_ms(ms(1000), us(1_000_300)) - 0.3).abs() < 1e-9);
    assert!((drift_ms(ms(1000), us(999_700)) + 0.3).abs() < 1e-9);
    assert_eq!(drift_ms(ms(1000), ms(1000)), 0.0);
}

#[test]
fn latencies_have_nothing_to_say_before_a_flip() {
    assert_eq!(Latencies::default().summary(ms(1000), None), None);
}

#[test]
fn latency_figures_are_mean_best_and_worst() {
    let mut l = Latencies::default();
    for (i, lat) in [8_000u64, 12_000, 20_000].into_iter().enumerate() {
        l.push(ms(1000 + i as u64 * 16), shown(lat, 0.0));
    }
    let sum = l.summary(ms(1100), budget_of(60_000)).expect("flips");
    let f = sum.latency.expect("latency");
    assert_eq!(f.frames, 3);
    assert!((f.avg_ms - 13.333).abs() < 0.01, "avg was {}", f.avg_ms);
    assert!((f.min_ms - 8.0).abs() < 1e-9);
    assert!((f.worst_ms - 20.0).abs() < 1e-9);
    assert!((sum.budget_ms.expect("budget") - 16.667).abs() < 0.01);
}

#[test]
fn the_worst_drift_keeps_its_sign_and_the_mean_is_a_bias() {
    let mut l = Latencies::default();
    for (i, d) in [0.2, -0.4, 5.0, -9.0].into_iter().enumerate() {
        l.push(ms(1000 + i as u64 * 16), shown(10_000, d));
    }
    let d = l
        .summary(ms(1100), None)
        .expect("flips")
        .drift
        .expect("drift");
    assert_eq!(d.flips, 4);
    assert_eq!(d.worst_ms, -9.0);
    assert!((d.avg_ms - (-1.05)).abs() < 1e-9, "avg was {}", d.avg_ms);
}

#[test]
fn a_flip_without_new_content_still_counts_for_the_drift() {
    let mut l = Latencies::default();
    l.push(
        ms(1000),
        Presented {
            latency: None,
            drift_ms: Some(0.5),
        },
    );
    let sum = l.summary(ms(1100), None).expect("flips");
    assert!(sum.latency.is_none());
    assert_eq!(sum.drift.expect("drift").flips, 1);
}

#[test]
fn a_flip_with_nothing_to_report_is_no_figure() {
    let mut l = Latencies::default();
    l.push(
        ms(1000),
        Presented {
            latency: None,
            drift_ms: None,
        },
    );
    assert_eq!(l.summary(ms(1100), None), None);
}

#[test]
fn old_flips_leave_the_latency_figures() {
    let mut l = Latencies::default();
    l.push(ms(1000), shown(90_000, 3.0));
    for i in 0..5u64 {
        l.push(ms(10_000 + i * 16), shown(10_000, 0.0));
    }
    let f = l
        .summary(ms(10_100), None)
        .expect("flips")
        .latency
        .expect("latency");
    assert_eq!(f.frames, 5);
    assert!((f.worst_ms - 10.0).abs() < 1e-9);
    // and nothing at all once they are all old
    assert_eq!(l.summary(ms(60_000), None), None);
}

#[test]
fn latencies_start_over_when_the_clock_goes_back() {
    let mut l = Latencies::default();
    l.push(ms(50_000), shown(10_000, 0.0));
    l.push(ms(1_000), shown(20_000, 0.0));
    let f = l
        .summary(ms(1_100), None)
        .expect("flips")
        .latency
        .expect("latency");
    assert_eq!(f.frames, 1);
    assert!((f.avg_ms - 20.0).abs() < 1e-9);
}

#[test]
fn ticks_know_their_newest_event() {
    let mut ticks = Ticks::default();
    assert_eq!(ticks.last(), None);
    ticks.push(ms(10));
    ticks.push(ms(30));
    assert_eq!(ticks.last(), Some(ms(30)));
    ticks.clear();
    assert_eq!(ticks.last(), None);
}

fn with_latency(drift: f64, latency_ms: f64) -> Snapshot {
    let mut l = Latencies::default();
    for i in 0..5u64 {
        l.push(
            ms(1000 + i * 7),
            Presented {
                latency: Some(Duration::from_secs_f64(latency_ms / 1000.0)),
                drift_ms: Some(drift),
            },
        );
    }
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.latency = l.summary(ms(1100), budget_of(60_000));
    s
}

#[test]
fn latency_rows_show_the_delay_and_the_drift() {
    let s = with_latency(0.25, 14.0);
    let latency = find(&s, "Latency");
    assert!(
        latency.value.contains("commit to screen 14.0 ms avg"),
        "{}",
        latency.value
    );
    assert_eq!(latency.tone, Tone::Neutral);
    let drift = find(&s, "Drift");
    assert!(drift.value.starts_with("+0.25 ms avg"), "{}", drift.value);
    assert_eq!(drift.tone, Tone::Good);
}

#[test]
fn a_drift_of_half_a_refresh_is_a_warning() {
    // 60 Hz: half a refresh is 8.3 ms, where a flip counts as a missed vblank
    assert_eq!(find(&with_latency(9.0, 14.0), "Drift").tone, Tone::Warn);
    assert_eq!(find(&with_latency(-9.0, 14.0), "Drift").tone, Tone::Warn);
    assert_eq!(find(&with_latency(8.0, 14.0), "Drift").tone, Tone::Good);
}

#[test]
fn content_that_waited_two_refreshes_is_a_warning() {
    assert_eq!(find(&with_latency(0.0, 40.0), "Latency").tone, Tone::Warn);
    assert_eq!(
        find(&with_latency(0.0, 30.0), "Latency").tone,
        Tone::Neutral
    );
}

#[test]
fn there_is_no_drift_to_report_under_vrr_or_tearing() {
    let mut s = with_latency(0.0, 14.0);
    s.vrr = Vrr::On;
    assert!(find(&s, "Drift").value.starts_with("n/a"));
    // the delay itself still holds
    assert!(find(&s, "Latency").value.contains("ms avg"));
    s.vrr = Vrr::Off;
    s.async_flip = true;
    assert!(find(&s, "Drift").value.starts_with("n/a"));
}

#[test]
fn latency_rows_say_when_there_is_nothing_yet() {
    let row = find(&snapshot(), "Latency");
    assert!(row.value.starts_with("no new frame"));
    assert_eq!(row.tone, Tone::Neutral);
}

#[test]
fn the_json_has_the_latency_and_stays_balanced() {
    let none = to_json(&snapshot());
    assert!(none.contains("\"latency\":null"), "{none}");
    let json = to_json(&with_latency(0.25, 14.0));
    for key in [
        "\"commit_to_screen\":{\"frames\":5,\"avg_ms\":14.000",
        "\"vblank_drift\":{\"flips\":5,\"avg_ms\":0.250,\"worst_ms\":0.250}",
    ] {
        assert!(json.contains(key), "{key} missing in {json}");
    }
    assert_eq!(json.matches('{').count(), json.matches('}').count());
}

// ---- the buffer's modifier -----------------------------------------------------------------

#[test]
fn modifiers_have_names() {
    assert_eq!(modifier_name(0), "LINEAR");
    assert_eq!(modifier_name(MODIFIER_INVALID), "INVALID (implicit)");
    assert_eq!(
        modifier_name(0x0100_0000_0000_0001),
        "Intel 0x0100000000000001"
    );
    assert_eq!(
        modifier_name(0x0200_0000_0000_0001),
        "AMD 0x0200000000000001"
    );
    assert!(modifier_name(0x7f00_0000_0000_0001).starts_with("unknown vendor"));
}

#[test]
fn a_dmabuf_shows_its_modifier() {
    let mut s = snapshot();
    s.buffer = BufferKind::Dmabuf(0x3432_5258, 0x0200_0000_0000_0001);
    let row = find(&s, "Modifier");
    assert_eq!(row.value, "AMD 0x0200000000000001");
    assert_eq!(row.tone, Tone::Neutral);
}

#[test]
fn an_implicit_modifier_is_a_warning() {
    let mut s = snapshot();
    s.buffer = BufferKind::Dmabuf(0x3432_5258, MODIFIER_INVALID);
    let row = find(&s, "Modifier");
    assert!(row.value.starts_with("implicit"), "{}", row.value);
    assert_eq!(row.tone, Tone::Warn);
}

#[test]
fn shm_and_no_buffer_have_no_modifier() {
    let mut s = snapshot();
    for buffer in [BufferKind::Shm, BufferKind::None] {
        s.buffer = buffer;
        let rows: Vec<_> = sections(&s).into_iter().flat_map(|x| x.rows).collect();
        assert!(rows.iter().all(|row| row.label != "Modifier"));
        assert!(to_json(&s).contains("\"buffer_modifier\":null"));
    }
}

#[test]
fn json_carries_the_modifier() {
    let mut s = snapshot();
    s.buffer = BufferKind::Dmabuf(0x3432_5258, 0x0200_0000_0000_0001);
    let json = to_json(&s);
    assert!(json.contains("\"buffer\":\"dmabuf:XR24\""), "{json}");
    assert!(
        json.contains("\"buffer_modifier\":{\"code\":\"0x0200000000000001\",\"name\":\"AMD 0x0200000000000001\"}"),
        "{json}"
    );
    assert_eq!(json.matches('{').count(), json.matches('}').count());
}

// ---- fifo, commit timing and explicit sync -------------------------------------------------

#[test]
fn the_protocols_in_use_have_names() {
    assert!(ClientSync::default().names().is_empty());
    let all = ClientSync {
        fifo: true,
        commit_timing: true,
        explicit_sync: true,
    };
    assert_eq!(
        all.names(),
        ["explicit sync (syncobj)", "fifo", "commit timing"]
    );
}

#[test]
fn the_overlay_lists_what_the_client_uses() {
    let mut s = snapshot();
    s.client_sync = ClientSync {
        fifo: true,
        commit_timing: false,
        explicit_sync: true,
    };
    assert_eq!(find(&s, "Sync").value, "explicit sync (syncobj), fifo");
    s.client_sync.commit_timing = true;
    assert_eq!(
        find(&s, "Sync").value,
        "explicit sync (syncobj), fifo, commit timing"
    );
}

#[test]
fn a_dmabuf_client_with_none_is_on_implicit_sync() {
    let s = snapshot();
    let row = find(&s, "Sync");
    assert!(row.value.starts_with("implicit"), "{}", row.value);
    let mut s = snapshot();
    s.buffer = BufferKind::Shm;
    assert!(find(&s, "Sync").value.starts_with("none"));
}

#[test]
fn json_carries_the_sync_protocols() {
    let mut s = snapshot();
    assert!(to_json(&s).contains(
        "\"sync_protocols\":{\"fifo\":false,\"commit_timing\":false,\"explicit_sync\":false}"
    ));
    s.client_sync = ClientSync {
        fifo: true,
        commit_timing: false,
        explicit_sync: true,
    };
    let json = to_json(&s);
    assert!(
        json.contains(
            "\"sync_protocols\":{\"fifo\":true,\"commit_timing\":false,\"explicit_sync\":true}"
        ),
        "{json}"
    );
    assert_eq!(json.matches('{').count(), json.matches('}').count());
}

// ---- tearing, seen and not only asked for -------------------------------------------------------

const R144: Duration = Duration::from_micros(6944);

#[test]
fn flips_a_whole_number_of_refreshes_apart_are_on_the_grid() {
    let prev = ms(1000);
    assert_eq!(flip_off_grid(prev, prev + R144, R144), Some(false));
    // three refreshes, a few tens of microseconds of stamp noise
    let noisy = prev + R144 * 3 + Duration::from_micros(40);
    assert_eq!(flip_off_grid(prev, noisy, R144), Some(false));
}

#[test]
fn a_flip_between_two_vblanks_is_off_the_grid() {
    let prev = ms(1000);
    assert_eq!(
        flip_off_grid(prev, prev + R144 + R144 / 2, R144),
        Some(true)
    );
    // sooner than a display can follow in sync
    assert_eq!(flip_off_grid(prev, prev + R144 / 3, R144), Some(true));
}

#[test]
fn flips_that_cannot_be_told_are_not_counted() {
    let prev = ms(1000);
    // an idle spell: the grid has drifted
    assert_eq!(flip_off_grid(prev, prev + ms(500), R144), None);
    // the clock went back
    assert_eq!(flip_off_grid(prev + ms(1), prev, R144), None);
    assert_eq!(flip_off_grid(prev, prev + R144, Duration::ZERO), None);
}

#[test]
fn the_tearing_summary_counts_the_last_span() {
    let mut t = Tearings::default();
    assert_eq!(t.summary(ms(10_000)), None);
    t.push(ms(1_000), true); // older than the span
    t.push(ms(8_000), true);
    t.push(ms(9_000), false);
    assert_eq!(
        t.summary(ms(10_000)),
        Some(TearingSummary {
            flips: 2,
            off_grid: 1
        })
    );
    t.clear();
    assert_eq!(t.summary(ms(10_000)), None);
}

#[test]
fn a_few_off_grid_flips_in_a_vsync_run_are_not_tearing() {
    assert!(!TearingSummary {
        flips: 300,
        off_grid: 0
    }
    .seen());
    assert!(!TearingSummary {
        flips: 300,
        off_grid: 14
    }
    .seen());
    assert!(TearingSummary {
        flips: 300,
        off_grid: 15
    }
    .seen());
    // too few flips to judge by
    assert!(!TearingSummary {
        flips: 4,
        off_grid: 4
    }
    .seen());
}

fn tearing_with(flips: usize, off_grid: usize) -> Snapshot {
    let mut s = snapshot();
    s.vrr = Vrr::Off;
    s.wants_tearing = true;
    s.allow_tearing = true;
    s.async_flip = true;
    s.tearing = Some(TearingSummary { flips, off_grid });
    s
}

#[test]
fn tearing_that_was_seen_says_so() {
    let row = find(&tearing_with(300, 220), "Tearing result");
    assert_eq!(row.value, "seen: 220 of 300 flips off the vblank grid");
    assert_eq!(row.tone, Tone::Good);
}

#[test]
fn tearing_asked_for_and_not_seen_warns() {
    let row = find(&tearing_with(300, 0), "Tearing result");
    assert_eq!(
        row.value,
        "requested, not seen: 0 of 300 flips off the vblank grid"
    );
    assert_eq!(row.tone, Tone::Warn);
}

#[test]
fn a_few_flips_leave_the_tearing_result_at_the_request() {
    let row = find(&tearing_with(3, 0), "Tearing result");
    assert_eq!(row.value, "async flip requested");
    assert_eq!(row.tone, Tone::Good);
}

#[test]
fn the_json_carries_the_observed_tearing() {
    assert!(to_json(&snapshot()).contains("\"tearing_observed\":null"));
    let json = to_json(&tearing_with(300, 220));
    assert!(json.contains("\"tearing_observed\":{\"flips\":300,\"off_grid\":220,\"seen\":true}"));
}

#[test]
fn a_cursor_on_the_plane_is_good_news() {
    let row = find(&snapshot(), "Cursor");
    assert!(row.value.starts_with("hardware plane"), "{}", row.value);
    assert_eq!(row.tone, Tone::Good);
}

#[test]
fn a_software_cursor_says_why() {
    let mut s = snapshot();
    for (why, needle) in [
        (CursorWhy::OtherGpu, "another GPU"),
        (CursorWhy::Hdr, "HDR"),
        (CursorWhy::Zoomed, "zoomed"),
        (CursorWhy::PlaneRefused, "refused"),
    ] {
        s.cursor = Some(Cursor::Software(why));
        let row = find(&s, "Cursor");
        assert!(row.value.starts_with("software: "), "{}", row.value);
        assert!(row.value.contains(needle), "{}", row.value);
        assert_eq!(row.tone, Tone::Warn);
    }
}

#[test]
fn a_cursor_that_is_not_drawn_is_not_a_warning() {
    let mut s = snapshot();
    s.cursor = Some(Cursor::Hidden);
    assert_eq!(find(&s, "Cursor").tone, Tone::Neutral);
    s.cursor = Some(Cursor::Elsewhere);
    let row = find(&s, "Cursor");
    assert!(row.value.contains("another monitor"), "{}", row.value);
    assert_eq!(row.tone, Tone::Neutral);
    s.cursor = None;
    let row = find(&s, "Cursor");
    assert_eq!(row.value, "no frame drawn yet");
    assert_eq!(row.tone, Tone::Neutral);
}

#[test]
fn the_json_carries_the_cursor_plane() {
    let mut s = snapshot();
    assert!(to_json(&s).contains("\"cursor\":{\"plane\":\"hardware\",\"reason\":null}"));
    s.cursor = Some(Cursor::Software(CursorWhy::PlaneRefused));
    let json = to_json(&s);
    assert!(json.contains("\"plane\":\"software\""), "{json}");
    assert!(
        json.contains("\"reason\":\"the GPU's cursor plane refused it"),
        "{json}"
    );
    s.cursor = Some(Cursor::Hidden);
    assert!(to_json(&s).contains("\"plane\":\"hidden\""));
    s.cursor = Some(Cursor::Elsewhere);
    assert!(to_json(&s).contains("\"plane\":\"elsewhere\""));
    s.cursor = None;
    assert!(to_json(&s).contains("\"cursor\":null"));
}

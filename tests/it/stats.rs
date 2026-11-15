//! Tests for `jff_wm::primitives::stats`.

use std::time::Duration;

use jff_wm::primitives::color::{ImageDescription, NamedPrimaries, NamedTf, TransferFunction};
use jff_wm::primitives::stats::{
    fourcc, primaries_name, sections, tf_name, to_json, BufferKind, Path, Row, Snapshot, Ticks,
    Tone, Vrr, Why,
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
    assert!((timing.low1_fps - 20.0).abs() < 0.5, "low was {}", timing.low1_fps);
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
        fullscreen: true,
        wants_tearing: false,
        says_game: false,
        buffer: BufferKind::Dmabuf(0x3432_5258),
        buffer_size: Some((2560, 1440)),
        window_size: (2560, 1440),
        color: None,
        allow_tearing: false,
        vrr_policy: "fullscreen",
        path: Path::Direct,
        scanout_without_overlay: None,
        vrr: Vrr::On,
        hdr_output: false,
        content_game: false,
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
    assert_eq!(find(&snapshot(), "Conversion").value, "none: passed through");
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
    assert!(find(&s, "Tearing result").value.contains("allow-tearing is off"));

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
    use jff_wm::primitives::stats::Timing;
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

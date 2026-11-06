//! Tests for the osd toasts and the `osd` config block.

use std::time::{Duration, Instant};

use jff_wm::config::Config;
use jff_wm::feature::osd::{
    alpha_at, elide, first_line, paint, subtitle_of, Icon, Notice, Osd, FADE_IN, FADE_OUT, GRACE,
    HEIGHT, MAX_TOASTS, SHADOW,
};

const TIMEOUT: Duration = Duration::from_millis(1800);

fn bell(key: Option<u64>, output: &str) -> Notice {
    Notice {
        key,
        icon: Icon::Bell,
        title: "Bell".to_owned(),
        subtitle: format!("window {}", key.unwrap_or(0)),
        output: output.to_owned(),
    }
}

#[test]
fn rings_of_one_window_share_a_toast_and_are_counted() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    assert_eq!(osd.headlines(), ["Bell"]);
    osd.show(
        bell(Some(1), "DP-1"),
        TIMEOUT,
        t0 + Duration::from_millis(100),
    );
    osd.show(
        bell(Some(1), "DP-1"),
        TIMEOUT,
        t0 + Duration::from_millis(200),
    );
    assert_eq!(osd.headlines(), ["Bell ×3"]);
}

#[test]
fn rings_of_other_windows_or_monitors_get_their_own_toast() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    osd.show(bell(Some(2), "DP-1"), TIMEOUT, t0);
    osd.show(bell(Some(1), "HDMI-A-1"), TIMEOUT, t0);
    osd.show(bell(None, "DP-1"), TIMEOUT, t0);
    assert_eq!(osd.len(), 4);
}

#[test]
fn a_ring_without_a_window_counts_too() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(None, "DP-1"), TIMEOUT, t0);
    osd.show(bell(None, "DP-1"), TIMEOUT, t0);
    assert_eq!(osd.headlines(), ["Bell ×2"]);
}

#[test]
fn a_new_ring_extends_the_toast() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    let again = t0 + Duration::from_millis(1500);
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, again);
    osd.expire(t0 + TIMEOUT + FADE_OUT + Duration::from_millis(10));
    assert_eq!(osd.len(), 1);
    osd.expire(again + TIMEOUT + FADE_OUT);
    assert!(osd.is_empty());
}

#[test]
fn a_toast_that_is_fading_out_is_not_reused() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    osd.show(
        bell(Some(1), "DP-1"),
        TIMEOUT,
        t0 + TIMEOUT + Duration::from_millis(50),
    );
    assert_eq!(osd.headlines(), ["Bell", "Bell"]);
}

#[test]
fn only_the_newest_toasts_of_a_monitor_stay() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    for key in 1..=(MAX_TOASTS as u64 + 2) {
        osd.show(bell(Some(key), "DP-1"), TIMEOUT, t0);
    }
    osd.show(bell(Some(100), "HDMI-A-1"), TIMEOUT, t0);
    let on_dp: Vec<String> = osd
        .active()
        .filter(|(n, _)| n.output == "DP-1")
        .map(|(n, _)| n.subtitle.clone())
        .collect();
    assert_eq!(on_dp.len(), MAX_TOASTS);
    assert_eq!(on_dp.first().unwrap(), "window 3");
    assert_eq!(on_dp.last().unwrap(), "window 5");
    assert_eq!(osd.len(), MAX_TOASTS + 1);
}

#[test]
fn the_fade_goes_in_holds_and_goes_out() {
    let born = Instant::now();
    let expires = born + TIMEOUT;
    assert_eq!(alpha_at(born, expires, born), 0.0);
    let mid = alpha_at(born, expires, born + FADE_IN / 2);
    assert!(mid > 0.0 && mid < 1.0, "{mid}");
    assert_eq!(alpha_at(born, expires, born + TIMEOUT / 2), 1.0);
    let out = alpha_at(born, expires, expires + FADE_OUT / 2);
    assert!(out > 0.0 && out < 1.0, "{out}");
    assert_eq!(alpha_at(born, expires, expires + FADE_OUT), 0.0);
}

#[test]
fn the_subtitle_names_what_is_known() {
    assert_eq!(
        subtitle_of("foot", "~/dev/jff ? nvim", Some(3)),
        "foot · ~/dev/jff ? nvim · workspace 3"
    );
    assert_eq!(subtitle_of("foot", "foot", None), "foot");
    assert_eq!(subtitle_of("", "", None), "");
    assert_eq!(subtitle_of("", "a title", Some(2)), "a title · workspace 2");
}

#[test]
fn long_text_is_cut_with_an_ellipsis() {
    let width = |s: &str| s.chars().count() as f32;
    assert_eq!(elide("short", 10.0, width), "short");
    let cut = elide("Configurações do sistema e outras coisas", 12.0, width);
    assert!(cut.ends_with('…'), "{cut}");
    assert!(width(&cut) <= 12.0, "{cut}");
    assert_eq!(elide("anything", 0.0, width), "…");
}

#[test]
fn a_toast_without_a_font_is_a_round_badge_with_a_shadow() {
    let picture = paint(None, "Bell", "", Icon::Bell);
    assert_eq!(picture.width, HEIGHT + 2 * SHADOW);
    assert_eq!(picture.height, HEIGHT + 2 * SHADOW);
    assert_eq!(
        picture.rgba.len(),
        (picture.width * picture.height * 4) as usize
    );
    let alpha = |x: i32, y: i32| picture.rgba[((y * picture.width + x) * 4 + 3) as usize];
    assert!(alpha(picture.width / 2, picture.height / 2) > 250);
    assert_eq!(alpha(0, 0), 0);
    assert!(alpha(picture.width / 2, picture.height - SHADOW + 3) > 0);
}

#[test]
fn the_info_icon_paints_too() {
    let picture = paint(None, "Info", "", Icon::Info);
    let alpha = |x: i32, y: i32| picture.rgba[((y * picture.width + x) * 4 + 3) as usize];
    assert!(alpha(picture.width / 2, picture.height / 2) > 250);
}

#[test]
fn toasts_are_on_by_default_and_can_be_set_up() {
    let default = Config::default();
    assert!(!default.osd().off);
    assert_eq!(default.osd().timeout_ms, 1800);

    let config = Config::parse("osd { timeout-ms 3000; font \"Inter\"; }").unwrap();
    assert_eq!(config.osd().timeout_ms, 3000);
    assert_eq!(config.osd().font, "Inter");
    assert!(!config.osd().off);

    assert!(Config::parse("osd { off; }").unwrap().osd().off);
}

#[test]
fn bad_osd_settings_are_errors() {
    assert!(Config::parse("osd { timeout-ms 5; }").is_err());
    assert!(Config::parse("osd { timeout-ms 999999; }").is_err());
    assert!(Config::parse("osd { font \"\"; }").is_err());
    assert!(Config::parse("osd { colour \"red\"; }").is_err());
}

#[test]
fn a_hovered_toast_stays_up() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    let mut now = t0 + Duration::from_millis(1700);
    for _ in 0..20 {
        osd.hold(0, now);
        osd.expire(now);
        assert_eq!(osd.len(), 1);
        now += Duration::from_millis(100);
    }
}

#[test]
fn after_the_pointer_leaves_only_the_grace_period_remains() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    let left = t0 + Duration::from_secs(10);
    osd.hold(0, left);
    assert_eq!(osd.fades_at(0), Some(left + GRACE));
    osd.expire(left + GRACE + FADE_OUT);
    assert!(osd.is_empty());
}

#[test]
fn hovering_never_shortens_the_time_left() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    osd.hold(0, t0 + Duration::from_millis(100));
    assert_eq!(osd.fades_at(0), Some(t0 + TIMEOUT));
}

#[test]
fn hovering_a_fading_toast_brings_it_back() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    let during_fade = t0 + TIMEOUT + FADE_OUT / 2;
    osd.hold(0, during_fade);
    let fades_at = osd.fades_at(0).unwrap();
    assert!(fades_at >= during_fade + GRACE);
    assert_eq!(alpha_at(t0, fades_at, during_fade), 1.0);
}

#[test]
fn holding_a_toast_that_is_not_there_does_nothing() {
    let mut osd = Osd::without_fonts();
    osd.hold(3, Instant::now());
    assert!(osd.is_empty());
}

#[test]
fn dismissing_an_icon_removes_only_those_toasts() {
    let mut osd = Osd::without_fonts();
    let t0 = Instant::now();
    osd.show(bell(Some(1), "DP-1"), TIMEOUT, t0);
    let mut error = bell(None, "DP-1");
    error.icon = Icon::Error;
    osd.show(error, TIMEOUT, t0);
    assert_eq!(osd.len(), 2);
    osd.dismiss(Icon::Error);
    assert_eq!(osd.len(), 1);
    assert_eq!(osd.active().next().unwrap().0.icon, Icon::Bell);
}

#[test]
fn an_error_message_is_cut_to_its_first_line() {
    assert_eq!(
        first_line("in osd: bad\n\n  1 | osd {\n  2 | }"),
        "in osd: bad"
    );
    assert_eq!(first_line("\n  \nsecond"), "second");
    assert_eq!(first_line(""), "");
}

#[test]
fn the_error_icon_paints_too() {
    let picture = paint(None, "Config error", "", Icon::Error);
    let alpha = |x: i32, y: i32| picture.rgba[((y * picture.width + x) * 4 + 3) as usize];
    assert!(alpha(picture.width / 2, picture.height / 2) > 250);
}

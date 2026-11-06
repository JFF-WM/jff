//! Tests for `jff_wm::feature::outputs`.

use jff_wm::config::ModeRequest;
use jff_wm::feature::outputs::{mhz_to_hz, Monitor, MonitorMode};

fn monitor() -> Monitor {
    Monitor {
        name: "DP-1".into(),
        size_mm: (600, 340),
        modes: vec![
            MonitorMode {
                width: 2560,
                height: 1440,
                refresh: 143_912,
                preferred: true,
            },
            MonitorMode {
                width: 1920,
                height: 1080,
                refresh: 60_000,
                preferred: false,
            },
        ],
    }
}

#[test]
fn has_mode_compares_size_and_whole_hz() {
    let m = monitor();
    let req = |width, height, refresh| ModeRequest {
        width,
        height,
        refresh,
    };
    assert!(m.has_mode(&req(2560, 1440, Some(144))));
    assert!(m.has_mode(&req(1920, 1080, None)));
    assert!(!m.has_mode(&req(2560, 1440, Some(60))));
    assert!(!m.has_mode(&req(1280, 720, None)));
}

#[test]
fn mhz_rounds_to_nearest_hz() {
    assert_eq!(mhz_to_hz(59_940), 60);
    assert_eq!(mhz_to_hz(143_912), 144);
}

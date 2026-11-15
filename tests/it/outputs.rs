//! Tests for `jff_wm::feature::outputs`.

use jff_wm::config::{ModeRequest, OutputTransform};
use jff_wm::feature::outputs::{mhz_to_hz, upright_point, Monitor, MonitorMode};
use smithay::utils::{Logical, Point, Size, Transform};

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

#[test]
fn transform_names_round_trip() {
    for name in OutputTransform::NAMES {
        let transform = OutputTransform::from_name(name).unwrap();
        assert_eq!(transform.name(), name);
        assert_eq!(
            OutputTransform::from_smithay(transform.to_smithay()),
            transform
        );
    }
    assert!(OutputTransform::from_name("45").is_none());
}

#[test]
fn a_touch_on_a_turned_panel_lands_where_the_frame_was_drawn() {
    // A 1920x1080 frame, turned onto the panel by each transform, and a few points in it.
    let frame: Size<f64, Logical> = (1920.0, 1080.0).into();
    for name in OutputTransform::NAMES {
        let transform = OutputTransform::from_name(name).unwrap().to_smithay();
        let native = transform.transform_size(Size::<i32, Logical>::from((1920, 1080)));
        for point in [(0.0, 0.0), (1920.0, 1080.0), (100.0, 900.0), (960.0, 540.0)] {
            let on_panel: Point<f64, Logical> =
                transform.transform_point_in(Point::from(point), &frame);
            let back = upright_point(transform, (native.w, native.h), (on_panel.x, on_panel.y));
            assert!(
                (back.0 - point.0).abs() < 1e-9 && (back.1 - point.1).abs() < 1e-9,
                "{name}: {point:?} came back as {back:?}"
            );
        }
    }
}

#[test]
fn a_quarter_turn_swaps_the_sides_of_the_frame() {
    assert_eq!(
        Transform::_90.transform_size(Size::<i32, Logical>::from((1920, 1080))),
        Size::from((1080, 1920))
    );
    assert_eq!(
        Transform::Flipped180.transform_size(Size::<i32, Logical>::from((1920, 1080))),
        Size::from((1920, 1080))
    );
}

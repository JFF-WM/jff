//! Tests for `jff_wm::feature::screenshot::area`.

use std::path::Path;
use std::path::PathBuf;

use jff_wm::feature::screenshot::area::{
    crop_rgba, expand_home, file_name, flip_rows, hit_handle, normalize_rect, physical_rect,
    resize_rect, screenshot_dir, unpremultiply, Handle, Rect, Selector,
};

const MONITOR: Rect = (1920, 0, 1280, 720);

#[test]
fn a_drag_from_any_corner_gives_the_same_positive_rectangle() {
    let (a, b) = ((2000.0, 100.0), (2300.0, 400.0));
    let expected = (2000, 100, 300, 300);
    assert_eq!(normalize_rect(a, b, MONITOR), expected);
    assert_eq!(normalize_rect(b, a, MONITOR), expected);
    assert_eq!(normalize_rect((a.0, b.1), (b.0, a.1), MONITOR), expected);
    assert_eq!(normalize_rect((b.0, a.1), (a.0, b.1), MONITOR), expected);
}

#[test]
fn a_drag_is_clamped_to_the_monitor() {
    assert_eq!(
        normalize_rect((1000.0, -50.0), (4000.0, 900.0), MONITOR),
        MONITOR
    );
    assert_eq!(
        normalize_rect((3100.0, 650.0), (5000.0, 5000.0), MONITOR),
        (3100, 650, 100, 70)
    );
    let r = normalize_rect((0.0, 0.0), (1950.0, 30.0), MONITOR);
    assert_eq!(r, (1920, 0, 30, 30));
}

#[test]
fn a_rectangle_is_never_empty() {
    for (a, b) in [
        ((2000.0, 100.0), (2000.0, 100.0)),
        ((2000.0, 100.0), (2000.0, 200.0)),
        ((2000.0, 100.0), (2100.0, 100.0)),
    ] {
        let (_, _, w, h) = normalize_rect(a, b, MONITOR);
        assert!(w >= 1 && h >= 1, "{a:?} {b:?} gave {w}x{h}");
    }
    assert_eq!(
        normalize_rect((9999.0, 9999.0), (9999.0, 9999.0), MONITOR),
        (3199, 719, 1, 1)
    );
    assert_eq!(
        normalize_rect((10.4, 10.6), (20.2, 20.2), (0, 0, 100, 100)),
        (10, 10, 11, 11)
    );
}

#[test]
fn handles_are_edges_and_corners() {
    let rect = (100, 100, 200, 100);
    let m = 8.0;
    assert_eq!(
        hit_handle(rect, (100.0, 150.0), m),
        Some(Handle { h: -1, v: 0 })
    );
    assert_eq!(
        hit_handle(rect, (300.0, 150.0), m),
        Some(Handle { h: 1, v: 0 })
    );
    assert_eq!(
        hit_handle(rect, (200.0, 100.0), m),
        Some(Handle { h: 0, v: -1 })
    );
    assert_eq!(
        hit_handle(rect, (200.0, 200.0), m),
        Some(Handle { h: 0, v: 1 })
    );
    assert_eq!(
        hit_handle(rect, (100.0, 100.0), m),
        Some(Handle { h: -1, v: -1 })
    );
    assert_eq!(
        hit_handle(rect, (300.0, 200.0), m),
        Some(Handle { h: 1, v: 1 })
    );
    assert_eq!(
        hit_handle(rect, (305.0, 95.0), m),
        Some(Handle { h: 1, v: -1 })
    );
    assert_eq!(hit_handle(rect, (200.0, 150.0), m), None);
    assert_eq!(hit_handle(rect, (50.0, 150.0), m), None);
    assert_eq!(hit_handle(rect, (200.0, 300.0), m), None);
}

#[test]
fn resizing_moves_only_the_held_edges_and_can_flip() {
    let bounds = (0, 0, 1000, 800);
    let rect = (100, 100, 200, 100);
    let right = Handle { h: 1, v: 0 };
    let corner = Handle { h: -1, v: -1 };
    assert_eq!(
        resize_rect(rect, right, (400.0, 999.0), bounds),
        (100, 100, 300, 100)
    );
    assert_eq!(
        resize_rect(rect, corner, (50.0, 20.0), bounds),
        (50, 20, 250, 180)
    );
    assert_eq!(
        resize_rect(rect, Handle { h: -1, v: 0 }, (500.0, 150.0), bounds),
        (300, 100, 200, 100)
    );
    assert_eq!(
        resize_rect(rect, right, (5000.0, 0.0), bounds),
        (100, 100, 900, 100)
    );
}

#[test]
fn the_selector_draws_resizes_and_forgets() {
    let mut s = Selector::new(MONITOR);
    assert_eq!(s.rect(), None);

    s.press((2000.0, 100.0));
    assert_eq!(s.rect(), None, "a press alone selects nothing");
    s.motion((2001.0, 101.0));
    assert_eq!(s.rect(), None, "below the threshold it is still a click");
    s.motion((2300.0, 400.0));
    assert_eq!(s.rect(), Some((2000, 100, 300, 300)));
    s.release((1000.0, 400.0));
    assert_eq!(s.rect(), Some((1920, 100, 80, 300)));

    s.motion((2500.0, 500.0));
    assert_eq!(s.rect(), Some((1920, 100, 80, 300)));

    s.press((2000.0, 200.0));
    s.motion((2400.0, 900.0));
    s.release((2400.0, 900.0));
    assert_eq!(s.rect(), Some((1920, 100, 480, 300)));

    s.press((3000.0, 600.0));
    s.release((3000.0, 600.0));
    assert_eq!(s.rect(), None);

    s.press((2100.0, 100.0));
    s.press((2900.0, 700.0));
    s.motion((2200.0, 200.0));
    assert_eq!(s.rect(), Some((2100, 100, 100, 100)));
}

#[test]
fn a_new_drag_replaces_the_old_selection() {
    let mut s = Selector::new(MONITOR);
    s.press((2000.0, 100.0));
    s.motion((2200.0, 300.0));
    s.release((2200.0, 300.0));
    s.press((3000.0, 600.0));
    s.motion((3100.0, 650.0));
    s.release((3100.0, 650.0));
    assert_eq!(s.rect(), Some((3000, 600, 100, 50)));
}

#[test]
fn logical_rectangles_become_pixels_of_their_monitor() {
    let rect = (2000, 100, 300, 200);
    assert_eq!(physical_rect(rect, (1920, 0), 1.0), (80, 100, 300, 200));
    assert_eq!(physical_rect(rect, (1920, 0), 2.0), (160, 200, 600, 400));
    let a = physical_rect((1921, 1, 3, 3), (1920, 0), 1.5);
    let b = physical_rect((1924, 1, 3, 3), (1920, 0), 1.5);
    assert_eq!(a, (2, 2, 4, 4));
    assert_eq!(b, (6, 2, 5, 4));
    assert_eq!(a.0 + a.2, b.0, "edge at 4 * 1.5 = 6 is shared");
    assert_eq!(
        physical_rect((1920, 0, 1280, 720), (1920, 0), 1.25),
        (0, 0, 1600, 900)
    );
}

fn picture(w: usize, h: usize) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 7, 255]))
        .collect()
}

#[test]
fn crop_takes_the_right_pixels_and_clips() {
    let src = picture(6, 4);
    let (out, w, h) = crop_rgba(&src, 6, 4, (2, 1, 3, 2)).unwrap();
    assert_eq!((w, h), (3, 2));
    assert_eq!(&out[..4], &[2, 1, 7, 255]);
    assert_eq!(&out[out.len() - 4..], &[4, 2, 7, 255]);
    assert_eq!(out.len(), 3 * 2 * 4);

    let (_, w, h) = crop_rgba(&src, 6, 4, (4, 3, 10, 10)).unwrap();
    assert_eq!((w, h), (2, 1));
    let (_, w, h) = crop_rgba(&src, 6, 4, (-5, -5, 7, 6)).unwrap();
    assert_eq!((w, h), (2, 1));
    assert!(crop_rgba(&src, 6, 4, (6, 0, 2, 2)).is_none());
    assert!(crop_rgba(&src, 6, 4, (1, 1, 0, 2)).is_none());
    assert!(crop_rgba(&src[..10], 6, 4, (0, 0, 2, 2)).is_none());
    // The whole picture comes back unchanged.
    assert_eq!(crop_rgba(&src, 6, 4, (0, 0, 6, 4)).unwrap().0, src);
}

#[test]
fn flipping_rows_twice_gives_the_picture_back() {
    let src = picture(3, 5);
    let mut buf = src.clone();
    flip_rows(&mut buf, 3 * 4);
    assert_eq!(&buf[..4], &[0, 4, 7, 255], "first row is the old last row");
    assert_eq!(
        &buf[2 * 3 * 4..2 * 3 * 4 + 4],
        &[0, 2, 7, 255],
        "middle row stays"
    );
    flip_rows(&mut buf, 3 * 4);
    assert_eq!(buf, src);
    let mut two = picture(1, 2);
    flip_rows(&mut two, 4);
    assert_eq!(two, vec![0, 1, 7, 255, 0, 0, 7, 255]);
    flip_rows(&mut [], 4);
    flip_rows(&mut [1, 2, 3, 4], 0);
}

#[test]
fn unpremultiply_restores_straight_alpha() {
    let mut px = [
        255, 255, 255, 255, 64, 32, 0, 128, 10, 10, 10, 0, 100, 0, 0, 100,
    ];
    unpremultiply(&mut px);
    assert_eq!(
        &px[0..4],
        &[255, 255, 255, 255],
        "opaque pixels are untouched"
    );
    assert_eq!(&px[4..8], &[128, 64, 0, 128]);
    assert_eq!(
        &px[8..12],
        &[0, 0, 0, 0],
        "transparent pixels lose their colour"
    );
    assert_eq!(&px[12..16], &[255, 0, 0, 100]);
    let mut bad = [200, 0, 0, 100];
    unpremultiply(&mut bad);
    assert_eq!(bad[0], 255);
}

#[test]
fn the_folder_follows_config_then_xdg_then_home() {
    let home = Path::new("/home/neo");
    assert_eq!(
        screenshot_dir(Some("/tmp/shots"), Some("/home/neo/Pics"), Some(home)),
        Some(PathBuf::from("/tmp/shots"))
    );
    assert_eq!(
        screenshot_dir(Some("~/Shots"), None, Some(home)),
        Some(PathBuf::from("/home/neo/Shots"))
    );
    assert_eq!(
        screenshot_dir(None, Some("/home/neo/Pics"), Some(home)),
        Some(PathBuf::from("/home/neo/Pics/Screenshots"))
    );
    assert_eq!(
        screenshot_dir(None, Some("$HOME/Imagens"), Some(home)),
        Some(PathBuf::from("/home/neo/Imagens/Screenshots"))
    );
    assert_eq!(
        screenshot_dir(None, None, Some(home)),
        Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
    );
    assert_eq!(
        screenshot_dir(Some(" "), Some(""), Some(home)),
        Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
    );
    assert_eq!(
        screenshot_dir(None, Some("Pictures"), Some(home)),
        Some(PathBuf::from("/home/neo/Pictures/Screenshots"))
    );
    assert_eq!(screenshot_dir(None, None, None), None);
    assert_eq!(
        screenshot_dir(Some("/tmp/x"), None, None),
        Some(PathBuf::from("/tmp/x"))
    );
}

#[test]
fn home_is_only_expanded_at_the_start_of_a_whole_component() {
    let home = Path::new("/home/neo");
    assert_eq!(expand_home("~", Some(home)), PathBuf::from("/home/neo"));
    assert_eq!(
        expand_home("~/a/b", Some(home)),
        PathBuf::from("/home/neo/a/b")
    );
    assert_eq!(expand_home("~root/a", Some(home)), PathBuf::from("~root/a"));
    assert_eq!(expand_home("/x/~/a", Some(home)), PathBuf::from("/x/~/a"));
    assert_eq!(expand_home("~/a", None), PathBuf::from("~/a"));
}

#[test]
fn file_names_carry_the_time_and_avoid_clashes() {
    assert_eq!(
        file_name("2026-11-04 21-30-05", 0),
        "Screenshot from 2026-11-04 21-30-05.png"
    );
    assert_eq!(
        file_name("2026-11-04 21-30-05", 2),
        "Screenshot from 2026-11-04 21-30-05 (2).png"
    );
}

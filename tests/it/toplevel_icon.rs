//! Tests for `jff_wm::protocol::toplevel_icon`.

use std::path::PathBuf;

use jff_wm::protocol::toplevel_icon::write_png;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jff-wm-icon-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn picture(shade: u8) -> Vec<u8> {
    // 2x2, straight alpha
    [
        [shade, 0, 0, 255],
        [0, shade, 0, 128],
        [0, 0, shade, 255],
        [shade; 4],
    ]
    .concat()
}

#[test]
fn a_picture_is_written_as_a_png() {
    let dir = scratch("png");
    let path = write_png(&dir, &picture(200), 2, 2).expect("written");
    assert!(path.starts_with(&dir));
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_same_picture_is_one_file_and_another_picture_is_another() {
    let dir = scratch("names");
    let first = write_png(&dir, &picture(10), 2, 2).unwrap();
    let again = write_png(&dir, &picture(10), 2, 2).unwrap();
    let other = write_png(&dir, &picture(20), 2, 2).unwrap();
    assert_eq!(first, again);
    assert_ne!(first, other);
    // two files, and no half written one left behind
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.iter().all(|n| n.ends_with(".png")), "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_whose_size_does_not_add_up_is_not_written() {
    let dir = scratch("bad");
    assert_eq!(write_png(&dir, &picture(1), 3, 3), None);
    assert_eq!(write_png(&dir, &[], 0, 0), None);
    assert!(!dir.exists());
}

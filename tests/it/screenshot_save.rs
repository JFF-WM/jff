//! Tests for `jff_wm::feature::screenshot::save`.

use std::path::PathBuf;

use jff_wm::feature::screenshot::save::{
    encode_png, save_hdr_with, save_with, timestamp, write_unique, HdrSaveJob, SaveJob,
};
use jff_wm::primitives::hdr_output::FinalPass;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("jff-shot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn gradient(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| (0..w).flat_map(move |x| [x as u8, y as u8, 99, 200]))
        .collect()
}

fn decode(png_bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    buf.truncate(info.buffer_size());
    (info, buf)
}

#[test]
fn png_round_trips() {
    let rgba = gradient(7, 5);
    let png_bytes = encode_png(&rgba, 7, 5).unwrap();
    assert_eq!(&png_bytes[1..4], b"PNG");
    let (info, pixels) = decode(&png_bytes);
    assert_eq!((info.width, info.height), (7, 5));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(pixels, rgba);
}

#[test]
fn png_rejects_wrong_sizes() {
    assert!(encode_png(&[0; 8], 2, 2).is_err());
    assert!(encode_png(&[], 0, 0).is_err());
    assert!(encode_png(&gradient(3, 3), 3, 2).is_err());
}

#[test]
fn names_never_clash_and_the_folder_is_created() {
    let dir = TempDir::new("names");
    let nested = dir.0.join("a").join("Screenshots");
    let one = write_unique(&nested, "2026-11-04 21-30-05", b"one").unwrap();
    let two = write_unique(&nested, "2026-11-04 21-30-05", b"two").unwrap();
    let other = write_unique(&nested, "2026-11-04 21-30-06", b"three").unwrap();
    assert_eq!(
        one.file_name().unwrap(),
        "Screenshot from 2026-11-04 21-30-05.png"
    );
    assert_eq!(
        two.file_name().unwrap(),
        "Screenshot from 2026-11-04 21-30-05 (1).png"
    );
    assert_ne!(one, other);
    assert_eq!(std::fs::read(&one).unwrap(), b"one");
    assert_eq!(std::fs::read(&two).unwrap(), b"two");
}

#[test]
fn the_file_is_written_even_when_the_clipboard_tool_is_missing() {
    let dir = TempDir::new("noclip");
    let job = SaveJob {
        rgba: gradient(4, 3),
        width: 4,
        height: 3,
        unpremultiply: false,
        dir: Some(dir.0.clone()),
        copy_to_clipboard: true,
    };
    let saved = save_with(job, "2026-11-04 21-30-05", "jff-no-such-clipboard-tool").unwrap();
    assert!(saved.path.is_file());
    let (info, pixels) = decode(&std::fs::read(&saved.path).unwrap());
    assert_eq!((info.width, info.height), (4, 3));
    assert_eq!(pixels, gradient(4, 3));
    match saved.clipboard {
        Some(Err(why)) => assert!(why.contains("not installed"), "{why}"),
        other => panic!("expected a clipboard error, got {other:?}"),
    }
}

#[test]
fn the_clipboard_gets_the_png_on_stdin() {
    let dir = TempDir::new("clip");
    std::fs::create_dir_all(&dir.0).unwrap();
    let tool = dir.0.join("fake-wl-copy");
    let sink = dir.0.join("clipboard.png");
    std::fs::write(
        &tool,
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = \"--type image/png\" ] || exit 3\ncat > '{}'\n",
            sink.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let job = SaveJob {
        rgba: gradient(5, 5),
        width: 5,
        height: 5,
        unpremultiply: false,
        dir: Some(dir.0.join("out")),
        copy_to_clipboard: true,
    };
    let saved = save_with(job, "t", tool.to_str().unwrap()).unwrap();
    assert!(
        matches!(saved.clipboard, Some(Ok(()))),
        "{:?}",
        saved.clipboard
    );
    assert_eq!(
        std::fs::read(&sink).unwrap(),
        std::fs::read(&saved.path).unwrap()
    );
}

#[test]
fn a_failing_clipboard_tool_is_reported_but_not_fatal() {
    let dir = TempDir::new("badclip");
    let job = SaveJob {
        rgba: gradient(2, 2),
        width: 2,
        height: 2,
        unpremultiply: false,
        dir: Some(dir.0.clone()),
        copy_to_clipboard: true,
    };
    let saved = save_with(job, "t", "false").unwrap();
    assert!(saved.path.is_file());
    assert!(matches!(saved.clipboard, Some(Err(_))));
}

#[test]
fn the_clipboard_is_left_alone_when_not_asked_for() {
    let dir = TempDir::new("noask");
    let job = SaveJob {
        rgba: gradient(2, 2),
        width: 2,
        height: 2,
        unpremultiply: false,
        dir: Some(dir.0.clone()),
        copy_to_clipboard: false,
    };
    let saved = save_with(job, "t", "false").unwrap();
    assert!(saved.clipboard.is_none());
}

#[test]
fn premultiplied_pictures_are_converted_before_encoding() {
    let dir = TempDir::new("premul");
    let job = SaveJob {
        rgba: vec![64, 32, 0, 128],
        width: 1,
        height: 1,
        unpremultiply: true,
        dir: Some(dir.0.clone()),
        copy_to_clipboard: false,
    };
    let saved = save_with(job, "t", "false").unwrap();
    let (_, pixels) = decode(&std::fs::read(&saved.path).unwrap());
    assert_eq!(pixels, vec![128, 64, 0, 128]);
}

#[test]
fn no_folder_is_an_error_not_a_panic() {
    let job = SaveJob {
        rgba: gradient(1, 1),
        width: 1,
        height: 1,
        unpremultiply: false,
        dir: None,
        copy_to_clipboard: false,
    };
    assert!(save_with(job, "t", "false").is_err());
}

#[test]
fn the_timestamp_has_the_shape_of_a_file_name() {
    let stamp = timestamp();
    assert_eq!(stamp.len(), "2026-11-04 21-30-05".len(), "{stamp}");
    assert!(!stamp.contains(':') && !stamp.contains('/'));
}

fn hdr_job(dir: &TempDir, half_rgba: Vec<u16>, width: u32, height: u32) -> HdrSaveJob {
    HdrSaveJob {
        half_rgba,
        width,
        height,
        pass: FinalPass::new(203.0, 1000.0, None),
        corner_radius: None,
        dir: Some(dir.0.clone()),
        wants_clipboard: false,
    }
}

#[test]
fn an_hdr_picture_is_saved_as_a_16_bit_png_that_says_it_is_pq() {
    let dir = TempDir::new("hdr");
    // one white pixel and one black one
    let path = save_hdr_with(
        hdr_job(
            &dir,
            vec![0x3C00, 0x3C00, 0x3C00, 0x3C00, 0, 0, 0, 0x3C00],
            2,
            1,
        ),
        "2026-11-13 00-00-00",
    )
    .unwrap();
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        "Screenshot from 2026-11-13 00-00-00.png"
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes
        .windows(8)
        .any(|w| w == [b'c', b'I', b'C', b'P', 9, 16, 0, 1]));
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .unwrap();
    assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    let first = u16::from_be_bytes([buf[0], buf[1]]);
    let last = u16::from_be_bytes([buf[6], buf[7]]);
    assert!(first > 20_000, "white should be a high code, got {first}");
    assert!(last < 10, "black should be near zero, got {last}");
}

#[test]
fn an_hdr_picture_of_the_wrong_size_is_refused() {
    let dir = TempDir::new("hdr-size");
    let err = save_hdr_with(hdr_job(&dir, vec![0; 7], 2, 1), "x").unwrap_err();
    assert!(err.contains("needs 8 values"), "{err}");
}

#[test]
fn an_hdr_picture_with_nowhere_to_go_is_refused() {
    let dir = TempDir::new("hdr-nodir");
    let mut job = hdr_job(&dir, vec![0; 4], 1, 1);
    job.dir = None;
    assert!(save_hdr_with(job, "x").is_err());
}

#[test]
fn an_hdr_window_is_saved_with_an_alpha_channel_and_cut_corners() {
    let dir = TempDir::new("hdr-window");
    // 8 by 8, all white and opaque, corners rounded by 3 pixels
    let frame: Vec<u16> = (0..64)
        .flat_map(|_| [0x3C00, 0x3C00, 0x3C00, 0x3C00])
        .collect();
    let mut job = hdr_job(&dir, frame, 8, 8);
    job.corner_radius = Some(3);
    let path = save_hdr_with(job, "2026-11-13 00-00-01").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(&bytes))
        .read_info()
        .unwrap();
    assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    assert_eq!(reader.info().bit_depth, png::BitDepth::Sixteen);
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut buf).unwrap();
    let alpha_at = |x: usize, y: usize| {
        let at = (y * 8 + x) * 8 + 6;
        u16::from_be_bytes([buf[at], buf[at + 1]])
    };
    assert!(alpha_at(0, 0) < 6_000, "the corner should be cut");
    assert_eq!(alpha_at(4, 4), 65535);
}

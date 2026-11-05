//! Turns a captured picture into a file: PNG encoding, unique filename, clipboard, notification.
//! Everything here runs on a worker thread (see `spawn_save`), so the compositor never waits on
//! the encoder or the disk. No Smithay.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::feature::screenshot::area::{file_name, unpremultiply};

/// Clipboard helper. `wl-copy` reads the PNG from its standard input.
const CLIPBOARD_PROGRAM: &str = "wl-copy";
/// How many file names to try for one second before giving up.
const MAX_NAME_TRIES: u32 = 1000;

/// One picture on its way to disk.
pub struct SaveJob {
    /// Tightly packed RGBA, `width * height * 4` bytes, top row first.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Pixels are premultiplied (a window drawn on a transparent background).
    pub unpremultiply: bool,
    /// Folder to save in. `None` means no home directory to derive one from.
    pub dir: Option<PathBuf>,
    /// Also hand the PNG to the clipboard.
    pub copy_to_clipboard: bool,
}

/// What `save_with` did.
#[derive(Debug)]
pub struct Saved {
    pub path: PathBuf,
    /// `Err(reason)` when the clipboard didn't get the picture. The file is there anyway.
    pub clipboard: Option<Result<(), String>>,
}

/// Encode straight-alpha RGBA as PNG.
pub fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 4;
    if width == 0 || height == 0 || rgba.len() != expected {
        return Err(format!(
            "a {width}x{height} picture needs {expected} bytes of pixels, got {}",
            rgba.len()
        ));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|err| format!("PNG header: {err}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|err| format!("PNG data: {err}"))?;
        writer.finish().map_err(|err| format!("PNG end: {err}"))?;
    }
    Ok(out)
}

/// Write `bytes` to a new file in `dir` (created when missing), named after `stamp`. Taken names
/// get a number, an existing screenshot is never overwritten.
pub fn write_unique(dir: &Path, stamp: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 0..MAX_NAME_TRIES {
        let path = dir.join(file_name(stamp, n));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    // Don't leave half a file behind under a screenshot's name.
                    let _ = std::fs::remove_file(&path);
                    return Err(err);
                }
                return Ok(path);
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "too many screenshots with the same name",
    ))
}

/// Hand `png` to `program --type image/png` on stdin. `wl-copy` stays in the background to serve
/// the clipboard, the call returns once it forks.
pub fn copy_to_clipboard_with(program: &str, png: &[u8]) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(["--type", "image/png"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| match err.kind() {
            ErrorKind::NotFound => format!("{program} is not installed"),
            _ => format!("could not start {program}: {err}"),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(png)
            .map_err(|err| format!("could not send the picture to {program}: {err}"))?;
    }
    let status = child
        .wait()
        .map_err(|err| format!("{program} did not finish: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed: {status}"))
    }
}

/// Encode, write, and (if asked) copy `job`. A clipboard failure never fails the save.
pub fn save_with(mut job: SaveJob, stamp: &str, clipboard_program: &str) -> Result<Saved, String> {
    let dir = job
        .dir
        .clone()
        .ok_or("no folder to save in (no home directory and no screenshot path)")?;
    if job.unpremultiply {
        unpremultiply(&mut job.rgba);
    }
    let png = encode_png(&job.rgba, job.width, job.height)?;
    let path = write_unique(&dir, stamp, &png)
        .map_err(|err| format!("could not write into {}: {err}", dir.display()))?;
    let clipboard = job
        .copy_to_clipboard
        .then(|| copy_to_clipboard_with(clipboard_program, &png));
    Ok(Saved { path, clipboard })
}

/// Local time for file names, `2026-11-04 21-30-05`.
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H-%M-%S").to_string()
}

/// Desktop notification via `notify-send`, if it's installed. A missing one is not worth a word.
pub fn notify(summary: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=jff", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Save `job` on its own thread, then report in the log and via notification.
pub fn spawn_save(job: SaveJob) {
    let spawned = std::thread::Builder::new()
        .name("jff-screenshot".to_string())
        .spawn(
            move || match save_with(job, &timestamp(), CLIPBOARD_PROGRAM) {
                Ok(saved) => {
                    let path = saved.path.display().to_string();
                    tracing::info!("screenshot saved to {path}");
                    let mut body = path;
                    match saved.clipboard {
                        Some(Ok(())) => body.push_str("\nCopied to the clipboard."),
                        Some(Err(why)) => {
                            tracing::warn!("screenshot not copied to the clipboard: {why}");
                            body.push_str(&format!("\nNot copied to the clipboard: {why}."));
                        }
                        None => {}
                    }
                    notify("Screenshot saved", &body);
                }
                Err(why) => {
                    tracing::warn!("screenshot not saved: {why}");
                    notify("Screenshot failed", &why);
                }
            },
        );
    if let Err(err) = spawned {
        tracing::warn!("could not start the screenshot thread: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut reader = png::Decoder::new(png_bytes).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
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
}

//! Turns a captured picture into a file: PNG encoding, unique filename, clipboard, notification.
//! Everything here runs on a worker thread (see `spawn_save`), so the compositor never waits on
//! the encoder or the disk. No Smithay.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::feature::screenshot::area::{file_name, unpremultiply};
use crate::primitives::hdr_capture::{pq_picture, pq_picture_rgba};
use crate::primitives::hdr_output::FinalPass;
use crate::primitives::hdr_png;

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

/// A picture of an HDR monitor on its way to disk, still in the working space it was drawn in.
/// The conversion to PQ and the encoding are slow, so they happen on the worker thread too.
pub struct HdrSaveJob {
    /// Four half floats per pixel (the bits), top row first.
    pub half_rgba: Vec<u16>,
    pub width: u32,
    pub height: u32,
    /// The final pass of the monitor the picture was taken from.
    pub pass: FinalPass,
    /// Folder to save in. `None` means no home directory to derive one from.
    pub dir: Option<PathBuf>,
    /// `Some` when the picture is a window: it has an alpha channel (premultiplied in
    /// `half_rgba`) and its corners are rounded by this many pixels, 0 for none. `None` is
    /// an opaque picture of a monitor.
    pub corner_radius: Option<u32>,
    /// The user asked for the clipboard too. An HDR picture is not put there, see [`spawn_save_hdr`].
    pub wants_clipboard: bool,
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

/// Convert and write `job` as a 16-bit PQ PNG, returning the path it went to.
pub fn save_hdr_with(job: HdrSaveJob, stamp: &str) -> Result<PathBuf, String> {
    let dir = job
        .dir
        .clone()
        .ok_or("no folder to save in (no home directory and no screenshot path)")?;
    let expected = job.width as usize * job.height as usize * 4;
    if job.width == 0 || job.height == 0 || job.half_rgba.len() != expected {
        return Err(format!(
            "a {}x{} picture needs {expected} values, got {}",
            job.width,
            job.height,
            job.half_rgba.len()
        ));
    }
    let png = match job.corner_radius {
        Some(radius) => {
            let picture = pq_picture_rgba(&job.pass, &job.half_rgba, job.width, radius);
            hdr_png::encode_rgba(
                &picture.codes,
                job.width,
                job.height,
                Some(picture.max_nits),
            )?
        }
        None => {
            let picture = pq_picture(&job.pass, &job.half_rgba);
            hdr_png::encode(
                &picture.codes,
                job.width,
                job.height,
                Some(picture.max_nits),
            )?
        }
    };
    write_unique(&dir, stamp, &png)
        .map_err(|err| format!("could not write into {}: {err}", dir.display()))
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

/// Save an HDR `job` on its own thread, then report in the log and via notification.
///
/// The file is all there is: other programs reading the clipboard mostly ignore the `cICP`
/// chunk and would show a washed-out picture, so nothing is handed to `wl-copy`. The
/// notification says so when the clipboard had been asked for.
pub fn spawn_save_hdr(job: HdrSaveJob) {
    let wants_clipboard = job.wants_clipboard;
    let spawned = std::thread::Builder::new()
        .name("jff-screenshot".to_string())
        .spawn(move || match save_hdr_with(job, &timestamp()) {
            Ok(path) => {
                let path = path.display().to_string();
                tracing::info!("HDR screenshot saved to {path}");
                let mut body = path;
                if wants_clipboard {
                    body.push_str("\nNot copied to the clipboard: it is an HDR picture.");
                }
                notify("HDR screenshot saved", &body);
            }
            Err(why) => {
                tracing::warn!("HDR screenshot not saved: {why}");
                notify("Screenshot failed", &why);
            }
        });
    if let Err(err) = spawned {
        tracing::warn!("could not start the screenshot thread: {err}");
    }
}

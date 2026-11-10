//! Logging: stderr always, plus an optional file the config turns on (`log { enable }`).
//!
//! The subscriber is built once at startup, before the config is read. The file layer is part
//! of it from the start but closed: it has no file and its filter lets nothing through. When
//! the config is loaded (and again each time it is reloaded) [`apply`] opens or closes the file
//! and sets the level, so `log { … }` takes effect without restarting jff.
//!
//! `path` is a file, or a folder (it ends with `/`, or already is one). With a folder every jff
//! session writes its own `jff-YYYY-MM-DD_HH-MM-SS.log` in it, the newest [`KEEP_SESSIONS`] are
//! kept, and `jff-latest.log` points to the current one. A config reload stays in the same
//! session file.
//!
//! The level is for jff's own code. Dependencies (smithay, calloop, …) are capped at `info`
//! so that `debug` doesn't drown the log in their per-frame chatter.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Layer, Registry};

use crate::config::{LogConfig, LogLevel};

/// How many `jff-*.log` session files a log folder keeps (the current one included).
pub const KEEP_SESSIONS: usize = 10;

/// Name of the link to the current session's file in a log folder.
const LATEST_LINK: &str = "jff-latest.log";

/// The target of every event jff's own code emits (the crate is `jff-wm`).
const OWN_TARGET: &str = "jff_wm";

/// The open log file, shared between the layer that writes to it and [`apply`]. `None` while
/// file logging is off.
#[derive(Clone, Default)]
struct SharedFile(Arc<Mutex<Option<File>>>);

impl SharedFile {
    fn lock(&self) -> MutexGuard<'_, Option<File>> {
        // A panic while logging must not stop the next event from being written.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Holds the file's lock for one event, so events from different threads don't interleave.
struct FileWriter<'a>(MutexGuard<'a, Option<File>>);

impl Write for FileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.as_mut() {
            Some(file) => file.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for SharedFile {
    type Writer = FileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        FileWriter(self.lock())
    }
}

struct FileLog {
    file: SharedFile,
    filter: reload::Handle<Targets, Registry>,
    /// What [`apply`] last set up, so a reload that changes nothing else leaves the file alone.
    active: Mutex<Option<(PathBuf, LogLevel)>>,
}

static FILE_LOG: OnceLock<FileLog> = OnceLock::new();

/// When this session started, as it is written in the name of its file.
static SESSION: OnceLock<String> = OnceLock::new();

fn session_stamp() -> &'static str {
    SESSION.get_or_init(|| chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string())
}

/// Whether `path` names a folder for session files rather than one file: it ends with `/` or
/// it already is a folder.
pub fn is_log_dir(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().ends_with(b"/") || path.is_dir()
}

/// The file to write for the configured `path`: the path itself, or for a folder the file of
/// the session that started at `stamp`.
pub fn log_file_for(path: &Path, stamp: &str) -> PathBuf {
    if is_log_dir(path) {
        path.join(format!("jff-{stamp}.log"))
    } else {
        path.to_path_buf()
    }
}

/// Whether `name` is the name of a session file (`jff-YYYY-MM-DD_HH-MM-SS.log`).
fn is_session_file(name: &str) -> bool {
    let Some(stamp) = name
        .strip_prefix("jff-")
        .and_then(|rest| rest.strip_suffix(".log"))
    else {
        return false;
    };
    stamp.len() == "YYYY-MM-DD_HH-MM-SS".len()
        && stamp
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
}

/// Delete the oldest session files in `dir` so that `keep` remain, never `current`. The stamp
/// sorts like the time, so the name says which is oldest.
pub fn prune_sessions(dir: &Path, keep: usize, current: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| is_session_file(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for old in files.iter().take(excess).filter(|old| *old != current) {
        let _ = std::fs::remove_file(old);
    }
}

/// Point `jff-latest.log` in `dir` at `file`.
fn link_latest(dir: &Path, file: &Path) {
    let Some(name) = file.file_name() else {
        return;
    };
    let link = dir.join(LATEST_LINK);
    let _ = std::fs::remove_file(&link);
    let _ = std::os::unix::fs::symlink(name, link);
}

/// The filter for a level: jff's code at `level`, everything else at `info` at most.
fn targets(level: LogLevel) -> Targets {
    let own = match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    };
    let others = match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info | LogLevel::Debug | LogLevel::Trace => LevelFilter::INFO,
    };
    Targets::new()
        .with_default(others)
        .with_target(OWN_TARGET, own)
        // why a window was refused the primary plane (direct scanout)
        .with_target(
            "smithay::backend::drm::compositor",
            if matches!(level, LogLevel::Debug | LogLevel::Trace) {
                LevelFilter::TRACE
            } else {
                own
            },
        )
}

/// Set up logging. Call once, first thing. Stderr is filtered by `RUST_LOG` (default `info`),
/// the file stays closed until [`apply`].
pub fn init() {
    let file = SharedFile::default();
    let (filter, handle) =
        reload::Layer::<Targets, Registry>::new(Targets::new().with_default(LevelFilter::OFF));

    let file_layer = fmt::layer::<Registry>()
        .with_ansi(false)
        .with_writer(file.clone())
        .with_filter(filter);
    let stderr_layer = fmt::layer()
        .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")));

    let installed = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .is_ok();
    // A subscriber somebody else installed (a test harness) means no file logging, not a crash.
    if installed {
        let _ = FILE_LOG.set(FileLog {
            file,
            filter: handle,
            active: Mutex::new(None),
        });
    }
}

/// Make the log file match the config: open it (creating the folder and the file if needed,
/// appending to what is there), close it, change the level, or move to another file.
///
/// A file that can't be opened is reported on stderr and leaves the previous setup as it was.
pub fn apply(config: &LogConfig) {
    let Some(log) = FILE_LOG.get() else {
        return;
    };
    // Asked before the file is opened, which creates a missing folder.
    let configured = config.resolved_path();
    let dir_mode = is_log_dir(&configured);
    let wanted = config
        .enable
        .then(|| (log_file_for(&configured, session_stamp()), config.level));
    let mut active = log.active.lock().unwrap_or_else(PoisonError::into_inner);
    if *active == wanted {
        return;
    }

    match &wanted {
        None => {
            // Say so while the file is still open, so the log ends with it.
            tracing::info!("log file turned off");
            let _ = log
                .filter
                .reload(Targets::new().with_default(LevelFilter::OFF));
            *log.file.lock() = None;
        }
        Some((path, level)) => {
            let same_file = active.as_ref().is_some_and(|(old, _)| old == path);
            if !same_file {
                match open(path) {
                    Ok(file) => {
                        *log.file.lock() = Some(file);
                        // In a log folder, keep it tidy and point `jff-latest.log` here.
                        if dir_mode {
                            prune_sessions(&configured, KEEP_SESSIONS, path);
                            link_latest(&configured, path);
                        }
                    }
                    Err(err) => {
                        tracing::warn!("can't open the log file {}: {err}", path.display());
                        return;
                    }
                }
            }
            if log.filter.reload(targets(*level)).is_err() {
                tracing::warn!("can't change the log level");
                return;
            }
            tracing::info!("logging to {} at level {level}", path.display());
        }
    }
    *active = wanted;
}

/// Open the log for appending. The file is private to the user (it can hold window titles at
/// `debug`), the folder is created if it isn't there.
fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

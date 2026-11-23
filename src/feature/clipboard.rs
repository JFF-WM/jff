//! Clipboard persistence: what you copied stays when the app that copied it closes.
//!
//! Wayland has no clipboard storage. The app that copies serves the data on every paste, so
//! when it exits the clipboard goes empty. Without help that means "copy in a terminal, close
//! it, paste elsewhere" fails, and tools like `wl-clip-persist` exist to work around it. jff
//! does that itself.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use smithay::reexports::calloop::channel::{channel, Event, Sender};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::LoopHandle;
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, request_data_device_client_selection, set_data_device_selection,
    SelectionRequestError,
};

use crate::primitives::clipboard::{fit_to_budget, wanted_mimes, Entry, History, Offer};
use crate::state::JffState;

/// How long an app gets to hand over one mime type. An app that is stuck loses that copy.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// A mime type no app offers. See [`JffState::clipboard_is_empty`].
const PROBE_MIME: &str = "application/x-jff-clipboard-probe";

/// Longest preview `clipboard-history` shows, in characters.
const PREVIEW_CHARS: usize = 80;

/// One mime type read, or given up on (`None`: too big, too slow, or the app went away).
pub struct Reading {
    generation: u64,
    mime: String,
    data: Option<Vec<u8>>,
}

/// A copy that is being read.
struct Pending {
    generation: u64,
    /// Mime types in the order the source offered them, to keep that order in the entry.
    order: Vec<String>,
    /// Readings still to come.
    waiting: usize,
    got: Vec<Offer>,
}

/// Everything jff keeps for the clipboard.
pub struct ClipboardKeeper {
    pub history: History,
    sender: Option<Sender<Reading>>,
    /// Counts the changes of the selection. A reading of an older one is thrown away.
    generation: u64,
    pending: Option<Pending>,
    /// True while jff sets the selection itself, so that its own change is not read back.
    offering: bool,
    /// The copy jff offers right now. `None` while an app owns the clipboard (or it is empty).
    offered: Option<Entry>,
    /// An app owns the clipboard and its copy is being (or has been) kept. Smithay empties the
    /// selection when that app goes away but does not tell [`SelectionHandler::new_selection`],
    /// so this is what [`JffState::clipboard_check_owner`] looks at to notice it.
    owned: bool,
}

impl ClipboardKeeper {
    /// Opens the channel the reading threads answer on. Without it (the loop refused the
    /// source) nothing is kept and the clipboard works as it did before.
    pub fn new(loop_handle: &LoopHandle<'static, JffState>) -> Self {
        let (sender, receiver) = channel::<Reading>();
        let inserted = loop_handle.insert_source(receiver, |event, _, state| {
            if let Event::Msg(reading) = event {
                state.clipboard_reading_done(reading);
            }
        });
        let sender = match inserted {
            Ok(_) => Some(sender),
            Err(err) => {
                tracing::warn!("clipboard persistence is off, no event source: {err}");
                None
            }
        };
        Self {
            history: History::default(),
            sender,
            generation: 0,
            pending: None,
            offering: false,
            offered: None,
            owned: false,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl JffState {
    /// The clipboard has a new owner (`Some`, with the mime types it offers) or no owner
    /// (`None`). Called from `SelectionHandler::new_selection`.
    pub fn clipboard_selection_changed(&mut self, offered: Option<Vec<String>>) {
        if self.clipboard.offering || self.config.clipboard().off {
            return;
        }
        tracing::debug!("clipboard: new_selection, offered mimes: {offered:?}");
        // Whatever was being read belongs to the owner that is gone.
        self.clipboard.generation += 1;
        self.clipboard.pending = None;
        // Set again by `clipboard_capture` once the new owner's copy is being read.
        self.clipboard.owned = false;
        let generation = self.clipboard.generation;
        // Smithay calls this before it has stored the new selection, so a read asked for now
        // finds nothing, and a selection set now is overwritten right after. Do the work on
        // the next turn of the loop, unless the selection changed again by then.
        let wanted = match offered {
            Some(offered) => {
                self.clipboard.offered = None;
                match wanted_mimes(&offered) {
                    None => {
                        tracing::debug!("clipboard: a secret, not kept");
                        return;
                    }
                    Some(wanted) if wanted.is_empty() => return,
                    Some(wanted) => Some(wanted),
                }
            }
            None => None,
        };
        let scheduled = self.loop_handle.insert_source(
            Timer::from_duration(Duration::ZERO),
            move |_, _, state| {
                tracing::debug!(
                    "clipboard: deferred work, generation {generation}, current {}",
                    state.clipboard.generation
                );
                if state.clipboard.generation == generation {
                    match wanted.clone() {
                        Some(wanted) => state.clipboard_capture(wanted),
                        None => state.clipboard_restore(),
                    }
                }
                TimeoutAction::Drop
            },
        );
        if let Err(err) = scheduled {
            tracing::warn!("clipboard: cannot schedule the work: {err}");
        }
    }

    /// Ask the owner of the clipboard for each of `wanted`. The answers come back through
    /// [`Self::clipboard_reading_done`].
    fn clipboard_capture(&mut self, wanted: Vec<String>) {
        let Some(sender) = self.clipboard.sender.clone() else {
            return;
        };
        let max_bytes = self.config.clipboard().max_size;
        let generation = self.clipboard.generation;
        let mut waiting = 0;
        for mime in &wanted {
            let Some((read_end, write_end)) = make_pipe() else {
                tracing::warn!("clipboard: cannot make a pipe, {mime} is not kept");
                continue;
            };
            // The owner gets the write end and closes it when it is done: that is the end of
            // the data. Handing it over drops jff's own copy of it.
            if let Err(err) =
                request_data_device_client_selection(&self.seat, mime.clone(), write_end)
            {
                tracing::debug!("clipboard: {mime} cannot be read: {err:?}");
                continue;
            }
            tracing::debug!("clipboard: asked the owner for {mime}");
            if spawn_reader(
                read_end,
                max_bytes,
                generation,
                mime.clone(),
                sender.clone(),
            ) {
                waiting += 1;
            }
        }
        tracing::debug!(
            "clipboard: waiting for {waiting} of {} mime types",
            wanted.len()
        );
        if waiting > 0 {
            self.clipboard.owned = true;
            self.clipboard.pending = Some(Pending {
                generation,
                order: wanted,
                waiting,
                got: Vec::new(),
            });
        }
    }

    /// A reading thread is done. The copy is kept when the last one of it is.
    pub fn clipboard_reading_done(&mut self, reading: Reading) {
        let Some(pending) = self.clipboard.pending.as_mut() else {
            return;
        };
        if pending.generation != reading.generation {
            return;
        }
        pending.waiting = pending.waiting.saturating_sub(1);
        tracing::debug!(
            "clipboard: read {}: {} (still waiting for {})",
            reading.mime,
            reading.data.as_ref().map_or_else(
                || "nothing (too big, too slow or failed)".to_string(),
                |d| { format!("{} bytes", d.len()) }
            ),
            pending.waiting
        );
        if let Some(data) = reading.data.filter(|data| !data.is_empty()) {
            pending.got.push(Offer::new(reading.mime, data));
        }
        if pending.waiting > 0 {
            return;
        }
        let Some(mut done) = self.clipboard.pending.take() else {
            return;
        };
        done.got.sort_by_key(|offer| {
            done.order
                .iter()
                .position(|mime| *mime == offer.mime)
                .unwrap_or(usize::MAX)
        });
        let (limit, max_bytes) = {
            let config = self.config.clipboard();
            (config.history, config.max_size)
        };
        let offers = fit_to_budget(done.got, max_bytes);
        self.clipboard.history.set_limit(limit);
        match self.clipboard.history.push(now_secs(), offers) {
            Some(id) => tracing::debug!("clipboard: kept copy {id}"),
            None => {
                tracing::debug!("clipboard: nothing to keep");
                // Nothing of this owner's copy is ours to offer: when it goes, the clipboard
                // stays empty rather than bringing back an older copy.
                self.clipboard.owned = false;
            }
        }
        // The owner may have gone while its copy was being read.
        self.clipboard_check_owner();
    }

    /// Called after every batch of client requests. When the app that owned the clipboard is
    /// gone, offer its copy ourselves.
    ///
    /// Smithay clears the selection when the owner's data source is destroyed (the app exited,
    /// or dropped the source) but does not call `SelectionHandler::new_selection` for it, only
    /// for a client that sets or clears the selection itself. Without this the clipboard would
    /// stay empty after the app closes.
    pub fn clipboard_check_owner(&mut self) {
        // The cheap tests first: this runs on every batch of requests.
        if !self.clipboard.owned || self.clipboard.pending.is_some() || self.config.clipboard().off
        {
            return;
        }
        if !self.clipboard_is_empty() {
            return;
        }
        tracing::debug!("clipboard: the owner is gone");
        self.clipboard.owned = false;
        self.clipboard_restore();
    }

    /// True when nothing owns the clipboard. Smithay has no getter for that, but asking for a
    /// mime type nobody offers is answered `NoSelection` for an empty clipboard and
    /// `InvalidMimetype` for any other, and nothing is sent to the owner.
    fn clipboard_is_empty(&self) -> bool {
        let Some((_read_end, write_end)) = make_pipe() else {
            return false;
        };
        matches!(
            request_data_device_client_selection(&self.seat, PROBE_MIME.to_string(), write_end),
            Err(SelectionRequestError::NoSelection)
        )
    }

    /// The clipboard went empty: offer the newest copy again.
    fn clipboard_restore(&mut self) {
        let Some(entry) = self.clipboard.history.latest().cloned() else {
            tracing::debug!("clipboard: the owner is gone and there is nothing to offer");
            return;
        };
        tracing::debug!("clipboard: the owner is gone, offering copy {}", entry.id);
        self.clipboard_offer(entry);
    }

    /// Make `entry` the clipboard, owned by jff.
    fn clipboard_offer(&mut self, entry: Entry) {
        let mime_types = entry.mime_types();
        tracing::debug!("clipboard: offering copy {} as {mime_types:?}", entry.id);
        self.clipboard.owned = false;
        self.clipboard.offering = true;
        set_data_device_selection(&self.display_handle, &self.seat, mime_types, ());
        self.clipboard.offering = false;
        self.clipboard.offered = Some(entry);
    }

    /// An app pastes while jff owns the clipboard: write the copy to its pipe. Called from
    /// `SelectionHandler::send_selection`. The pipe is closed when the write is done, or at
    /// once for a mime type the copy does not have.
    pub fn clipboard_send(&mut self, mime: &str, fd: OwnedFd) {
        let Some(data) = self
            .clipboard
            .offered
            .as_ref()
            .and_then(|entry| entry.offer(mime))
            .map(|offer| Arc::clone(&offer.data))
        else {
            tracing::debug!(
                "clipboard: paste of {mime}, nothing to send (offered copy: {:?})",
                self.clipboard
                    .offered
                    .as_ref()
                    .map(|entry| entry.mime_types())
            );
            return;
        };
        tracing::debug!("clipboard: paste of {mime}, sending {} bytes", data.len());
        // A paste can be big and its reader slow: never wait for it on the compositor's thread.
        let spawned = std::thread::Builder::new()
            .name("jff-clipboard-send".into())
            .spawn(move || {
                let mut file = File::from(fd);
                // An app that stops reading is not an error worth anything.
                match file.write_all(&data) {
                    Ok(()) => tracing::debug!("clipboard: sent {} bytes", data.len()),
                    Err(err) => tracing::debug!("clipboard: the paste stopped: {err}"),
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("clipboard: cannot start a thread to answer a paste: {err}");
        }
    }

    /// `jff msg clipboard-history`: the copies kept, newest first.
    pub fn clipboard_history_json(&self) -> String {
        use crate::primitives::json::json_str;
        let entries: Vec<String> = self
            .clipboard
            .history
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mimes: Vec<String> =
                    entry.offers.iter().map(|offer| json_str(&offer.mime)).collect();
                format!(
                    "{{\"id\":{},\"time\":{},\"size\":{},\"mimes\":[{}],\"preview\":{},\"current\":{}}}",
                    entry.id,
                    entry.stamp,
                    entry.size(),
                    mimes.join(","),
                    json_str(&entry.preview(PREVIEW_CHARS)),
                    index == 0,
                )
            })
            .collect();
        format!(
            "{{\"ok\":true,\"enabled\":{},\"limit\":{},\"entries\":[{}]}}\n",
            !self.config.clipboard().off,
            self.config.clipboard().history,
            entries.join(",")
        )
    }

    /// `jff msg clipboard-select ID`: make a kept copy the clipboard again.
    pub fn clipboard_select(&mut self, id: u64) -> Result<(), String> {
        if self.config.clipboard().off {
            return Err("the clipboard is off (`clipboard { off }`)".to_string());
        }
        let entry = self
            .clipboard
            .history
            .get(id)
            .cloned()
            .ok_or_else(|| format!("there is no clipboard entry {id}"))?;
        // Whatever was being read belongs to a clipboard that is replaced now.
        self.clipboard.generation += 1;
        self.clipboard.pending = None;
        self.clipboard.history.promote(id, now_secs());
        self.clipboard_offer(entry);
        Ok(())
    }

    /// `jff msg clipboard-clear`: forget every copy. The clipboard is emptied only when jff
    /// is the one that holds it, an app that owns it keeps it.
    pub fn clipboard_clear(&mut self) {
        self.clipboard.generation += 1;
        self.clipboard.pending = None;
        self.clipboard.history.clear();
        if self.clipboard.offered.take().is_some() {
            self.clipboard.offering = true;
            clear_data_device_selection(&self.display_handle, &self.seat);
            self.clipboard.offering = false;
        }
    }
}

/// A pipe whose ends close when the process execs. Only the write end is meant to travel to
/// another process, and it stays blocking: the file description is shared with the app, and
/// an app that wrote to a non-blocking pipe would get `EAGAIN` it does not expect.
fn make_pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `pipe2` succeeded, so both are open descriptors that nothing else owns.
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Read `read_end` on a thread of its own and answer on `sender`. False when the thread could
/// not be started, in which case no answer will come.
fn spawn_reader(
    read_end: OwnedFd,
    max_bytes: usize,
    generation: u64,
    mime: String,
    sender: Sender<Reading>,
) -> bool {
    std::thread::Builder::new()
        .name("jff-clipboard-read".into())
        .spawn(move || {
            let data = read_all(File::from(read_end), max_bytes, READ_TIMEOUT);
            // The loop is gone when jff exits: nobody to tell.
            let _ = sender.send(Reading {
                generation,
                mime,
                data,
            });
        })
        .is_ok()
}

/// Read until the writer closes its end. `None` when there is more than `max_bytes` or it takes
/// longer than `timeout` altogether.
///
/// An oversized copy is still read to the end and thrown away. Closing the pipe early would
/// hand the app an `EPIPE` (and with some toolkits a `SIGPIPE`) in the middle of its write.
fn read_all(mut file: File, max_bytes: usize, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut data = Vec::new();
    let mut too_big = false;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !wait_readable(file.as_raw_fd(), left) {
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) => return (!too_big).then_some(data),
            Ok(n) => {
                if !too_big && data.len() + n <= max_bytes {
                    data.extend_from_slice(&chunk[..n]);
                } else {
                    too_big = true;
                    data = Vec::new();
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Wait until `fd` has data or hangs up. False on a timeout or an error.
fn wait_readable(fd: RawFd, timeout: Duration) -> bool {
    let millis = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one valid `pollfd`, and a count of 1.
        let rc = unsafe { libc::poll(&mut poll_fd, 1, millis) };
        if rc < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return rc > 0;
    }
}

//! What jff keeps of the clipboard so it outlives the app that copied it, and the history of
//! earlier copies. Pure data: `feature/clipboard.rs` fills it from the selection and offers it
//! again when the app that owned the clipboard is gone.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Password managers mark a copied secret with this mime type (KeePassXC, Bitwarden, the KDE
/// clipboard). A selection that offers it is never kept.
pub const SECRET_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// `clipboard { history N }` when it is not set: only the latest copy is kept.
pub const DEFAULT_HISTORY: usize = 1;
/// Largest `clipboard { history N }`.
pub const MAX_HISTORY: usize = 500;
/// `clipboard { max-size N }` (MiB) when it is not set.
pub const DEFAULT_MAX_SIZE_MIB: usize = 8;
/// Largest `clipboard { max-size N }` (MiB).
pub const MAX_MAX_SIZE_MIB: usize = 256;

/// Mime types that name text, best first. The first one an entry has is its preview.
const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// X11 selection targets that XWayland-style clients list among the mime types. They are
/// requests for information about the selection, not data to keep.
const INTERNAL_MIMES: [&str; 10] = [
    "TARGETS",
    "TIMESTAMP",
    "MULTIPLE",
    "SAVE_TARGETS",
    "DELETE",
    "INSERT_PROPERTY",
    "INSERT_SELECTION",
    "LENGTH",
    "LIST_LENGTH",
    "PIPE",
];

/// The contents of the clipboard in one mime type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub mime: String,
    pub data: Arc<Vec<u8>>,
}

impl Offer {
    pub fn new(mime: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            mime: mime.into(),
            data: Arc::new(data),
        }
    }
}

/// One copy: the same contents in every mime type the app offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Assigned when the copy is first kept and never reused while jff runs.
    pub id: u64,
    /// Unix seconds of the last time this copy was made or picked.
    pub stamp: u64,
    pub offers: Vec<Offer>,
    key: u64,
}

impl Entry {
    /// Bytes of all the offers together.
    pub fn size(&self) -> usize {
        self.offers.iter().map(|offer| offer.data.len()).sum()
    }

    pub fn offer(&self, mime: &str) -> Option<&Offer> {
        self.offers.iter().find(|offer| offer.mime == mime)
    }

    pub fn mime_types(&self) -> Vec<String> {
        self.offers.iter().map(|offer| offer.mime.clone()).collect()
    }

    /// One line that says what this is: the start of the text with its whitespace squeezed,
    /// or `[image/png 12.3 KiB]` for anything that is not text.
    pub fn preview(&self, max_chars: usize) -> String {
        let text = TEXT_MIMES.iter().find_map(|name| {
            self.offers
                .iter()
                .find(|offer| offer.mime.eq_ignore_ascii_case(name))
        });
        if let Some(offer) = text {
            let text = String::from_utf8_lossy(&offer.data);
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut out: String = flat.chars().take(max_chars).collect();
            if flat.chars().count() > max_chars {
                out.push('…');
            }
            return out;
        }
        match self.offers.first() {
            Some(first) => format!("[{} {}]", first.mime, human_size(first.data.len())),
            None => String::new(),
        }
    }
}

/// `1.5 KiB`, `12 B`, `3.0 MiB`.
pub fn human_size(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{:.1} MiB", b / (KIB * KIB))
    }
}

/// Copies kept, newest first. Never more than `limit`, and never two with the same contents.
#[derive(Debug)]
pub struct History {
    entries: VecDeque<Entry>,
    next_id: u64,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            next_id: 1,
            limit: DEFAULT_HISTORY,
        }
    }
}

impl History {
    /// Keep at most `limit` copies (1 to [`MAX_HISTORY`]). Older ones are dropped at once.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.clamp(1, MAX_HISTORY);
        self.entries.truncate(self.limit);
    }

    /// Keep a copy. The same contents as one that is already there only move it to the front
    /// (and renew its time). Returns the entry's id, `None` for a copy with nothing in it.
    pub fn push(&mut self, stamp: u64, offers: Vec<Offer>) -> Option<u64> {
        if offers.is_empty() {
            return None;
        }
        let key = content_key(&offers);
        if let Some(pos) = self.entries.iter().position(|entry| entry.key == key) {
            let mut entry = self.entries.remove(pos)?;
            entry.stamp = stamp;
            let id = entry.id;
            self.entries.push_front(entry);
            return Some(id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_front(Entry {
            id,
            stamp,
            offers,
            key,
        });
        self.entries.truncate(self.limit);
        Some(id)
    }

    /// The newest copy.
    pub fn latest(&self) -> Option<&Entry> {
        self.entries.front()
    }

    pub fn get(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Move a copy to the front, as if it had just been made. False when there is none.
    pub fn promote(&mut self, id: u64, stamp: u64) -> bool {
        let Some(pos) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        if let Some(mut entry) = self.entries.remove(pos) {
            entry.stamp = stamp;
            self.entries.push_front(entry);
        }
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Newest first.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }
}

/// The same for the same contents, whatever the order of the offers.
fn content_key(offers: &[Offer]) -> u64 {
    let mut sorted: Vec<&Offer> = offers.iter().collect();
    sorted.sort_by(|a, b| a.mime.cmp(&b.mime));
    let mut hasher = DefaultHasher::new();
    for offer in sorted {
        offer.mime.hash(&mut hasher);
        offer.data.hash(&mut hasher);
    }
    hasher.finish()
}

/// Which of the mime types a source offers are worth reading and keeping. `None` when the
/// source says what it offers is a secret (see [`SECRET_HINT_MIME`]): nothing of it is kept.
/// The list can be empty, for a source that offers only things that are not data.
pub fn wanted_mimes(offered: &[String]) -> Option<Vec<String>> {
    if offered
        .iter()
        .any(|mime| mime.eq_ignore_ascii_case(SECRET_HINT_MIME))
    {
        return None;
    }
    let mut wanted: Vec<String> = Vec::new();
    for mime in offered {
        let internal = INTERNAL_MIMES.contains(&mime.as_str());
        if !internal && !mime.is_empty() && !wanted.contains(mime) {
            wanted.push(mime.clone());
        }
    }
    Some(wanted)
}

/// The offers, in order, that fit into `max_bytes` together. An offer that would not fit is
/// left out and the ones after it still get their chance, so a huge `image/bmp` does not take
/// the `text/plain` next to it down with it.
pub fn fit_to_budget(offers: Vec<Offer>, max_bytes: usize) -> Vec<Offer> {
    let mut used = 0usize;
    let mut kept = Vec::with_capacity(offers.len());
    for offer in offers {
        let size = offer.data.len();
        if used.saturating_add(size) <= max_bytes {
            used += size;
            kept.push(offer);
        }
    }
    kept
}

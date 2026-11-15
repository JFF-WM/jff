//! The serials of the keys, buttons and touches (presses and releases: a click acts on the
//! release) that reached a client, which is what an xdg-activation token has to carry to move
//! the focus (see `token_created`).
//!
//! The serial of a pointer or keyboard `enter` doesn't count: a plain hover is no user action.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;

/// How many serials are kept.
const KEPT: usize = 32;
/// How long after the input a token made from its serial is still good.
const MAX_AGE: Duration = Duration::from_secs(10);

struct Entry {
    serial: Serial,
    /// The client the event went to.
    client: Option<ClientId>,
    at: Instant,
}

/// Recent input serials and the client each one went to.
#[derive(Default)]
pub struct InputSerials {
    recent: VecDeque<Entry>,
}

impl InputSerials {
    /// An input event with `serial` was sent to `target`.
    pub fn note(&mut self, serial: Serial, target: Option<&WlSurface>) {
        if self.recent.len() == KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back(Entry {
            serial,
            client: target.and_then(|surface| surface.client()).map(|c| c.id()),
            at: Instant::now(),
        });
    }

    /// Whether `serial` is a recent input event that went to `client`: only the app that was
    /// acted on can ask for the focus with it.
    pub fn accepts(&self, serial: Serial, client: Option<&ClientId>) -> bool {
        let Some(client) = client else {
            return false;
        };
        self.recent.iter().any(|entry| {
            entry.serial == serial
                && entry.client.as_ref() == Some(client)
                && entry.at.elapsed() <= MAX_AGE
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_serials_and_missing_clients_are_refused() {
        let serials = InputSerials::default();
        assert!(!serials.accepts(Serial::from(1), None));
    }

    #[test]
    fn only_the_last_entries_are_kept() {
        let mut serials = InputSerials::default();
        for n in 0..(KEPT as u32 + 5) {
            serials.note(Serial::from(n), None);
        }
        assert_eq!(serials.recent.len(), KEPT);
        assert_eq!(u32::from(serials.recent[0].serial), 5);
    }
}

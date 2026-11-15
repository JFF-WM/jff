//! Fingers on a touchscreen: which ones are down, which one stands in for the mouse, and which
//! monitor a touchscreen covers.
//!
//! No Wayland in here (`S` is the device's slot id, `T` whatever a finger's events go to), so
//! the rules can be tested on their own. `input/touch.rs` is the user.

/// One finger on the glass.
#[derive(Debug, Clone, PartialEq)]
pub struct Finger<S, T> {
    pub slot: S,
    /// Where the finger's events go (a client's surface), if a client is getting them.
    pub target: Option<T>,
    /// The seat got `touch.down` for this finger, so it is owed a `touch.up` (or a cancel).
    pub forwarded: bool,
    /// The press went to the pointer side (click-to-focus, window grabs, the overview), so the
    /// release is owed there.
    pub pressed: bool,
}

/// The fingers down right now.
///
/// The *primary* finger is the one that drives the compositor's own pointer: it is the first
/// to touch down, and when it lifts the next finger to touch down takes over. The rest are
/// only passed on to clients (pinch, two-finger scroll).
#[derive(Debug, Clone, PartialEq)]
pub struct Fingers<S, T> {
    list: Vec<Finger<S, T>>,
    primary: Option<S>,
}

impl<S, T> Default for Fingers<S, T> {
    fn default() -> Self {
        Self {
            list: Vec::new(),
            primary: None,
        }
    }
}

impl<S: Copy + PartialEq, T> Fingers<S, T> {
    /// A finger touched down. True when it is the primary one.
    pub fn down(&mut self, slot: S) -> bool {
        // A slot still listed lost its release somewhere: it starts over.
        self.up(slot);
        let primary = self.primary.is_none();
        if primary {
            self.primary = Some(slot);
        }
        self.list.push(Finger {
            slot,
            target: None,
            forwarded: false,
            pressed: false,
        });
        primary
    }

    /// A finger lifted: it is gone from the list, and handed back to say what it was owed.
    pub fn up(&mut self, slot: S) -> Option<Finger<S, T>> {
        let at = self.list.iter().position(|f| f.slot == slot)?;
        if self.primary == Some(slot) {
            self.primary = None;
        }
        Some(self.list.remove(at))
    }

    pub fn get(&self, slot: S) -> Option<&Finger<S, T>> {
        self.list.iter().find(|f| f.slot == slot)
    }

    pub fn get_mut(&mut self, slot: S) -> Option<&mut Finger<S, T>> {
        self.list.iter_mut().find(|f| f.slot == slot)
    }

    pub fn is_primary(&self, slot: S) -> bool {
        self.primary == Some(slot)
    }

    /// Everything is off: the device cancelled the touch (a palm, a suspend). Hands back the
    /// fingers that were down, to settle what each was owed.
    pub fn clear(&mut self) -> Vec<Finger<S, T>> {
        self.primary = None;
        std::mem::take(&mut self.list)
    }

    /// Clients were told the touch is cancelled (a compositor grab took it): no finger is
    /// forwarded any more, the fingers themselves are still down.
    pub fn detach_all(&mut self) {
        for finger in &mut self.list {
            finger.forwarded = false;
            finger.target = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }
}

/// Whether `name` (a connector name such as `eDP-1`) is a laptop's own panel.
pub fn is_internal_panel(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["edp", "lvds", "dsi"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The monitor a touchscreen covers, as an index into `names`: a built-in panel if there is
/// one (that is where a laptop's or tablet's touchscreen is), else the first monitor.
pub fn touch_output<N: AsRef<str>>(names: &[N]) -> Option<usize> {
    if names.is_empty() {
        return None;
    }
    names
        .iter()
        .position(|name| is_internal_panel(name.as_ref()))
        .or(Some(0))
}

//! Fingers on a touchscreen: which ones are down, which one stands in for the mouse, which
//! monitor a touchscreen covers, and the three-finger swipe jff keeps for itself.
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
    /// The client under the finger never bound `wl_touch`, so the finger is passed on as
    /// `wl_pointer` events instead (pointer emulation). Never set together with `forwarded`.
    pub emulated: bool,
    /// Where the finger last was, in shared space.
    pub at: (f64, f64),
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
            emulated: false,
            at: (0.0, 0.0),
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

    /// Every finger down, with where it last was.
    pub fn positions(&self) -> Vec<(S, (f64, f64))> {
        self.list.iter().map(|f| (f.slot, f.at)).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }
}

impl<S: Copy + PartialEq, T: Clone> Fingers<S, T> {
    /// The compositor takes every finger for itself (a gesture): what each one was owed is
    /// handed back to be settled, and no finger owes anything afterwards. The fingers
    /// themselves are still down.
    pub fn settle_all(&mut self) -> Vec<Finger<S, T>> {
        let owed = self.list.clone();
        for finger in &mut self.list {
            finger.forwarded = false;
            finger.pressed = false;
            finger.emulated = false;
            finger.target = None;
        }
        owed
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

/// The monitor a touchscreen covers, as an index into `names`: the one called `wanted` (the
/// config's `map-to-output`) if it is connected, else what [`touch_output`] picks. A mapping
/// to a monitor that is unplugged does not leave the touchscreen dead.
pub fn touch_output_mapped<N: AsRef<str>>(names: &[N], wanted: Option<&str>) -> Option<usize> {
    wanted
        .and_then(|wanted| names.iter().position(|name| name.as_ref() == wanted))
        .or_else(|| touch_output(names))
}

/// Fingers of the compositor's own touchscreen swipe.
pub const GESTURE_FINGERS: usize = super::swipe::FINGERS as usize;

/// A multi-finger swipe on a touchscreen. It starts when exactly [`GESTURE_FINGERS`] fingers
/// are down, follows the middle of the fingers, and owns every finger until all have lifted.
#[derive(Debug, Clone, PartialEq)]
pub struct TouchGesture<S> {
    active: bool,
    swipe: super::swipe::SwipeTracker,
    points: Vec<(S, (f64, f64))>,
    centroid: (f64, f64),
}

impl<S> Default for TouchGesture<S> {
    fn default() -> Self {
        Self {
            active: false,
            swipe: Default::default(),
            points: Vec::new(),
            centroid: (0.0, 0.0),
        }
    }
}

fn centroid_of<S>(points: &[(S, (f64, f64))]) -> (f64, f64) {
    if points.is_empty() {
        return (0.0, 0.0);
    }
    let n = points.len() as f64;
    let (x, y) = points
        .iter()
        .fold((0.0, 0.0), |(x, y), (_, p)| (x + p.0, y + p.1));
    (x / n, y / n)
}

impl<S: Copy + PartialEq> TouchGesture<S> {
    /// The gesture is going on: no finger belongs to a client or the pointer.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// `points` are all the fingers down now. Starts the gesture when there are exactly
    /// [`GESTURE_FINGERS`] of them and `allowed` (false while something else owns the input:
    /// the lock screen, a screenshot selection). Returns whether it started.
    pub fn begin(&mut self, points: Vec<(S, (f64, f64))>, allowed: bool) -> bool {
        if self.active || !allowed || points.len() != GESTURE_FINGERS {
            return false;
        }
        self.swipe.begin(super::swipe::FINGERS, true);
        self.centroid = centroid_of(&points);
        self.points = points;
        self.active = true;
        true
    }

    /// Another finger touched down during the gesture: it counts, and nothing jumps.
    pub fn add(&mut self, slot: S, at: (f64, f64)) {
        if !self.active {
            return;
        }
        self.points.retain(|(s, _)| *s != slot);
        self.points.push((slot, at));
        self.centroid = centroid_of(&self.points);
    }

    /// A finger moved. Gives the direction once the middle of the fingers went far enough
    /// (once per gesture).
    pub fn moved(&mut self, slot: S, at: (f64, f64)) -> Option<super::swipe::Swipe> {
        if !self.active {
            return None;
        }
        self.points.iter_mut().find(|(s, _)| *s == slot)?.1 = at;
        let now = centroid_of(&self.points);
        let (dx, dy) = (now.0 - self.centroid.0, now.1 - self.centroid.1);
        self.centroid = now;
        self.swipe.update(dx, dy)
    }

    /// A finger lifted: the middle moves to the fingers left, which is not a swipe.
    pub fn lift(&mut self, slot: S) {
        self.points.retain(|(s, _)| *s != slot);
        self.centroid = centroid_of(&self.points);
    }

    /// Every finger is up, or the touch was cancelled.
    pub fn end(&mut self) {
        *self = Self::default();
    }
}

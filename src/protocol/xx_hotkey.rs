//! xx-hotkey-v1: clients bind their own global hotkeys, opt-in with `global-hotkeys`.

// jff policy, on top of what the protocol requires:
// - Off unless config.kdl has `global-hotkeys`: without it there is no global at all.
// - A trigger without ctrl, alt or super is denied (`not_permitted`) unless it is a lone
//   modifier, a keypad key, a functional key (F-keys, media, navigation) or an auxiliary mouse
//   button. Dead keys and keys that type a character are always denied that way.
// - A trigger that one of jff's own bindings uses, or that another client holds, is denied
//   (`already_bound`). First come, first served, jff's bindings always win, also after a reload.
// - Nothing fires while the session is locked, while a window has inhibited the shortcuts, or
//   while the overview or the screenshot selection is open.
// - A hotkey with a seat only fires for the main seat: input of a transient seat lands there too.
// - Events carry a time taken from the monotonic clock when jff handles the input.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smithay::backend::input::Keycode;
use smithay::input::keyboard::{xkb, Keysym, ModifiersState};
use smithay::input::Seat;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Clock, Monotonic, SERIAL_COUNTER};

use super::xx_hotkey_bindings::server::xx_hotkey_manager_v1::{self, XxHotkeyManagerV1};
use super::xx_hotkey_bindings::server::xx_hotkey_v1::{self, DenyReason, XxHotkeyV1};
use crate::input::{BTN_LEFT, BTN_RIGHT};
use crate::state::JffState;

pub const SHIFT: u32 = 1;
pub const CTRL: u32 = 2;
pub const ALT: u32 = 4;
pub const SUPER: u32 = 8;
const MODIFIER_BITS: u32 = SHIFT | CTRL | ALT | SUPER;

/// A tap (a lone modifier key) that is held longer than this does not fire.
pub const TAP_MAX_HOLD: Duration = Duration::from_millis(500);

/// What a hotkey reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// An unshifted keysym.
    Key(u32),
    /// A button code as in `wl_pointer.button`.
    Button(u32),
}

/// A trigger and the modifiers that must be held, compared exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub trigger: Trigger,
    pub mods: u32,
}

impl Spec {
    /// Whether a press of a key with first-level keysyms `syms` while `mods` are held fires this.
    pub fn matches_key(&self, syms: &[u32], mods: u32) -> bool {
        matches!(self.trigger, Trigger::Key(sym) if mods == self.mods && syms.contains(&sym))
    }

    /// Whether a press of `button` while `mods` are held fires this.
    pub fn matches_button(&self, button: u32, mods: u32) -> bool {
        self.trigger == Trigger::Button(button) && mods == self.mods
    }

    /// A key trigger on a modifier keysym fires on release.
    pub fn is_tap(&self) -> bool {
        matches!(self.trigger, Trigger::Key(sym) if is_modifier_keysym(sym))
    }
}

/// The four modifiers of a protocol bitmask that are held in `state`, lock modifiers left out.
pub fn mods_of(state: &ModifiersState) -> u32 {
    [
        (state.shift, SHIFT),
        (state.ctrl, CTRL),
        (state.alt, ALT),
        (state.logo, SUPER),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(0, |bits, (_, bit)| bits | bit)
}

/// The modifier state for a protocol bitmask.
pub fn mods_state(mods: u32) -> ModifiersState {
    ModifiersState {
        shift: mods & SHIFT != 0,
        ctrl: mods & CTRL != 0,
        alt: mods & ALT != 0,
        logo: mods & SUPER != 0,
        ..Default::default()
    }
}

/// Shift_L ..= Hyper_R.
pub fn is_modifier_keysym(sym: u32) -> bool {
    (0xffe1..=0xffee).contains(&sym)
}

fn is_keypad(sym: u32) -> bool {
    (0xff80..=0xffbd).contains(&sym)
}

fn is_dead(sym: u32) -> bool {
    (0xfe50..=0xfe8f).contains(&sym)
}

fn produces_character(sym: u32) -> bool {
    xkb::keysym_to_utf32(Keysym::new(sym)) != 0
}

/// BTN_SIDE, BTN_EXTRA, BTN_FORWARD, BTN_BACK and BTN_TASK.
pub fn is_aux_button(code: u32) -> bool {
    (0x113..=0x117).contains(&code)
}

/// jff's policy on what a client may bind, see the notes at the top of this file.
pub fn permitted(spec: &Spec) -> bool {
    if spec.mods & (CTRL | ALT | SUPER) != 0 {
        return true;
    }
    match spec.trigger {
        Trigger::Button(code) => is_aux_button(code),
        Trigger::Key(sym) => {
            is_modifier_keysym(sym) || is_keypad(sym) || !(is_dead(sym) || produces_character(sym))
        }
    }
}

fn now_ms() -> u32 {
    Duration::from(Clock::<Monotonic>::new().now()).as_millis() as u32
}

fn serial() -> u32 {
    u32::from(SERIAL_COUNTER.next_serial())
}

/// One bound hotkey.
struct Entry {
    resource: XxHotkeyV1,
    app_id: Option<String>,
    spec: Spec,
    seat: Option<Seat<JffState>>,
    description: String,
}

/// A tap that is under way: its key is down and nothing else happened yet.
struct Tap {
    key: Keycode,
    since: Instant,
    /// Another key, button or axis event happened.
    dirty: bool,
    hotkeys: Vec<XxHotkeyV1>,
}

/// The bound hotkeys and what is held.
pub struct Hotkeys {
    global: Option<GlobalId>,
    entries: Vec<Entry>,
    held_keys: Vec<(Keycode, XxHotkeyV1)>,
    held_buttons: Vec<(u32, XxHotkeyV1)>,
    tap: Option<Tap>,
    /// The configured keymap, built on first use and again after a reload.
    keymap: Option<xkb::Keymap>,
}

impl Hotkeys {
    /// Without `enabled` there is no global, so clients see no support.
    pub fn new(display: &DisplayHandle, enabled: bool) -> Self {
        Self {
            global: enabled
                .then(|| display.create_global::<JffState, XxHotkeyManagerV1, ()>(1, ())),
            entries: Vec::new(),
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
            tap: None,
            keymap: None,
        }
    }

    /// Create or remove the global after a config reload, removing it revokes every hotkey.
    fn set_enabled(&mut self, display: &DisplayHandle, enabled: bool) {
        match (enabled, self.global.is_some()) {
            (true, false) => {
                self.global = Some(display.create_global::<JffState, XxHotkeyManagerV1, ()>(1, ()));
            }
            (false, true) => {
                if let Some(global) = self.global.take() {
                    display.remove_global::<JffState>(global);
                }
                self.revoke_where(|_| true, "global hotkeys were turned off");
            }
            _ => {}
        }
    }

    /// Drop what is held or pending for `resource`.
    fn forget(&mut self, resource: &XxHotkeyV1) {
        self.held_keys.retain(|(_, held)| held != resource);
        self.held_buttons.retain(|(_, held)| held != resource);
        if let Some(tap) = &mut self.tap {
            tap.hotkeys.retain(|pending| pending != resource);
        }
    }

    fn remove(&mut self, resource: &XxHotkeyV1) {
        self.entries.retain(|entry| entry.resource != *resource);
        self.forget(resource);
    }

    /// Withdraw every binding `pred` picks, telling its client.
    fn revoke_where(&mut self, pred: impl Fn(&Entry) -> bool, message: &str) {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| pred(entry));
        self.entries = kept;
        for entry in gone {
            self.forget(&entry.resource);
            tracing::info!(
                "hotkey of {} ({:?}) revoked: {message}",
                entry.app_id.as_deref().unwrap_or("an unnamed client"),
                entry.description
            );
            if entry.resource.is_alive() {
                entry.resource.revoked(message.to_string());
            }
        }
    }

    fn wants_keys(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry.spec.trigger, Trigger::Key(_)))
    }
}

/// The identity a manager gives its hotkeys.
#[derive(Default)]
struct Identity {
    app_id: Option<String>,
    committed: bool,
}

/// Shared by a manager and the hotkeys it made, so they outlive the manager.
#[derive(Default)]
pub struct ManagerData {
    identity: Mutex<Identity>,
}

/// What `set_*` asked for, applied on commit.
#[derive(Default)]
struct Staged {
    trigger: Option<Spec>,
    seat: Option<WlSeat>,
    description: String,
}

pub struct HotkeyData {
    manager: Arc<ManagerData>,
    staged: Mutex<Staged>,
}

impl GlobalDispatch<XxHotkeyManagerV1, ()> for JffState {
    // Sandboxed apps must not see this.
    fn can_view(client: Client, _: &()) -> bool {
        !crate::state::is_sandboxed(&client)
    }

    fn bind(
        _state: &mut JffState,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XxHotkeyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, JffState>,
    ) {
        data_init.init(resource, Arc::new(ManagerData::default()));
    }
}

impl Dispatch<XxHotkeyManagerV1, Arc<ManagerData>> for JffState {
    fn request(
        _state: &mut JffState,
        _client: &Client,
        resource: &XxHotkeyManagerV1,
        request: xx_hotkey_manager_v1::Request,
        data: &Arc<ManagerData>,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, JffState>,
    ) {
        match request {
            xx_hotkey_manager_v1::Request::SetAppId { app_id } => {
                let mut identity = data.identity.lock().unwrap();
                if app_id.is_empty() || identity.app_id.is_some() || identity.committed {
                    resource.post_error(
                        xx_hotkey_manager_v1::Error::InvalidAppId,
                        "app id empty, set twice, or set after a commit",
                    );
                    return;
                }
                identity.app_id = Some(app_id);
            }
            xx_hotkey_manager_v1::Request::CreateHotkey { id } => {
                data_init.init(
                    id,
                    HotkeyData {
                        manager: data.clone(),
                        staged: Mutex::new(Staged::default()),
                    },
                );
            }
            // `destroy` leaves the hotkeys it made valid.
            _ => {}
        }
    }
}

impl Dispatch<XxHotkeyV1, HotkeyData> for JffState {
    fn request(
        state: &mut JffState,
        _client: &Client,
        resource: &XxHotkeyV1,
        request: xx_hotkey_v1::Request,
        data: &HotkeyData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, JffState>,
    ) {
        use xx_hotkey_v1::Request;
        let invalid = |what: &str| {
            resource.post_error(xx_hotkey_v1::Error::InvalidTrigger, what.to_string());
        };
        match request {
            Request::SetDescription { description } => {
                data.staged.lock().unwrap().description = description;
            }
            Request::SetSeat { seat } => data.staged.lock().unwrap().seat = seat,
            Request::SetKeyTrigger { keysym, modifiers } => {
                let mods = u32::from(modifiers);
                if keysym == 0 || mods & !MODIFIER_BITS != 0 {
                    return invalid("null keysym or unknown modifier bits");
                }
                data.staged.lock().unwrap().trigger = Some(Spec {
                    trigger: Trigger::Key(keysym),
                    mods,
                });
            }
            Request::SetButtonTrigger { button, modifiers } => {
                let mods = u32::from(modifiers);
                if mods & !MODIFIER_BITS != 0 {
                    return invalid("unknown modifier bits");
                }
                data.staged.lock().unwrap().trigger = Some(Spec {
                    trigger: Trigger::Button(button),
                    mods,
                });
            }
            Request::Commit => state.commit_hotkey(resource, data),
            // `destroy` is handled in `destroyed`.
            _ => {}
        }
    }

    // Also runs when the client goes away without destroying it.
    fn destroyed(
        state: &mut JffState,
        _client: ClientId,
        resource: &XxHotkeyV1,
        _data: &HotkeyData,
    ) {
        state.hotkeys.remove(resource);
    }
}

impl JffState {
    /// Apply the staged state of `resource`: exactly one `bound` or `denied` comes back.
    fn commit_hotkey(&mut self, resource: &XxHotkeyV1, data: &HotkeyData) {
        let (spec, seat, description) = {
            let staged = data.staged.lock().unwrap();
            (
                staged.trigger,
                staged.seat.clone(),
                staged.description.clone(),
            )
        };
        let Some(spec) = spec else {
            resource.post_error(xx_hotkey_v1::Error::NoTrigger, "commit without a trigger");
            return;
        };
        let app_id = {
            let mut identity = data.manager.identity.lock().unwrap();
            identity.committed = true;
            identity.app_id.clone()
        };
        let seat = match seat.as_ref().map(Seat::<JffState>::from_resource) {
            None => None,
            Some(Some(seat)) => Some(seat),
            Some(None) => {
                resource.denied(DenyReason::NotPermitted, "unknown seat".to_string());
                return;
            }
        };
        if let Err((reason, message)) = self.check_hotkey(resource, &spec, seat.as_ref()) {
            tracing::debug!(
                "hotkey {spec:?} of {} denied: {message}",
                app_id.as_deref().unwrap_or("an unnamed client")
            );
            resource.denied(reason, message.to_string());
            return;
        }
        tracing::info!(
            "hotkey {spec:?} bound for {} ({description:?})",
            app_id.as_deref().unwrap_or("an unnamed client")
        );
        let entry = Entry {
            resource: resource.clone(),
            app_id,
            spec,
            seat,
            description,
        };
        match self
            .hotkeys
            .entries
            .iter_mut()
            .find(|held| held.resource == *resource)
        {
            Some(slot) => *slot = entry,
            None => self.hotkeys.entries.push(entry),
        }
        // A new trigger must not inherit a press of the old one.
        self.hotkeys.forget(resource);
        resource.bound();
    }

    /// Whether `spec` may be bound by `own`: the policy, jff's bindings, then other clients.
    fn check_hotkey(
        &self,
        own: &XxHotkeyV1,
        spec: &Spec,
        seat: Option<&Seat<JffState>>,
    ) -> Result<(), (DenyReason, &'static str)> {
        if !permitted(spec) {
            return Err((
                DenyReason::NotPermitted,
                "needs ctrl, alt or super, or a functional key or extra button",
            ));
        }
        if self.conflicts_with_binds(spec) {
            return Err((DenyReason::AlreadyBound, "used by a compositor binding"));
        }
        let taken = self.hotkeys.entries.iter().any(|other| {
            other.resource != *own
                && other.resource.is_alive()
                && other.spec == *spec
                && (other.seat.is_none() || seat.is_none() || other.seat.as_ref() == seat)
        });
        if taken {
            return Err((DenyReason::AlreadyBound, "held by another application"));
        }
        Ok(())
    }

    /// Whether one of jff's own bindings (or the VT switch) uses `spec`.
    fn conflicts_with_binds(&self, spec: &Spec) -> bool {
        let mods = mods_state(spec.mods);
        match spec.trigger {
            Trigger::Key(sym) => {
                // Ctrl+Alt+F1 ..= F12 switch the VT.
                let vt = spec.mods == (CTRL | ALT) && (0xffbe..=0xffc9).contains(&sym);
                vt || self.config.action_for(&mods, Keysym::new(sym)).is_some()
            }
            Trigger::Button(code) => {
                // Modifier+drag moves and resizes windows.
                let drag =
                    matches!(code, BTN_LEFT | BTN_RIGHT) && self.config.drag_modifier_held(&mods);
                drag || self.config.button_action_for(&mods, code).is_some()
            }
        }
    }

    /// Follow `global-hotkeys` and the bindings of the config after a reload.
    pub fn apply_hotkey_config(&mut self) {
        let enabled = self.config.global_hotkeys();
        self.hotkeys.keymap = None;
        self.hotkeys.set_enabled(&self.display_handle, enabled);
        let clashing: Vec<XxHotkeyV1> = self
            .hotkeys
            .entries
            .iter()
            .filter(|entry| self.conflicts_with_binds(&entry.spec))
            .map(|entry| entry.resource.clone())
            .collect();
        self.hotkeys.revoke_where(
            |entry| clashing.contains(&entry.resource),
            "a compositor binding now uses this trigger",
        );
    }

    /// Withdraw the hotkeys bound to `seat`, which is gone.
    pub fn revoke_hotkeys_on_seat(&mut self, seat: &Seat<JffState>) {
        self.hotkeys.revoke_where(
            |entry| entry.seat.as_ref() == Some(seat),
            "the seat was removed",
        );
    }

    fn on_main_seat(&self, entry: &Entry) -> bool {
        entry.seat.as_ref().map_or(true, |seat| *seat == self.seat)
    }

    /// The first-level keysym of `key` in every layout group, `[]` when no hotkey needs it.
    pub(crate) fn hotkey_key_syms(&mut self, key: Keycode) -> Vec<u32> {
        if !self.hotkeys.wants_keys() {
            return Vec::new();
        }
        if self.hotkeys.keymap.is_none() {
            self.hotkeys.keymap = self.config.keyboard().keymap();
        }
        let Some(keymap) = &self.hotkeys.keymap else {
            return Vec::new();
        };
        let key = xkb::Keycode::new(key.raw());
        (0..keymap.num_layouts_for_key(key))
            .flat_map(|layout| keymap.key_get_syms_by_level(key, layout, 0).iter())
            .map(|sym| sym.raw())
            .collect()
    }

    /// Something other than the key `key` happened: a tap under way is no tap any more.
    pub(crate) fn hotkey_input_seen(&mut self, key: Option<Keycode>) {
        if let Some(tap) = &mut self.hotkeys.tap {
            if key != Some(tap.key) {
                tap.dirty = true;
            }
        }
    }

    /// A key went down with `mods` held before it. True when a hotkey takes the press.
    pub(crate) fn client_hotkey_pressed(&mut self, key: Keycode, mods: u32, syms: &[u32]) -> bool {
        if self.hotkeys.entries.is_empty() || self.session_locked() {
            return false;
        }
        let (mut taps, mut fired) = (Vec::new(), Vec::new());
        for entry in &self.hotkeys.entries {
            if entry.spec.matches_key(syms, mods) && self.on_main_seat(entry) {
                let list = if entry.spec.is_tap() {
                    &mut taps
                } else {
                    &mut fired
                };
                list.push(entry.resource.clone());
            }
        }
        if !taps.is_empty() {
            self.hotkeys.tap = Some(Tap {
                key,
                since: Instant::now(),
                dirty: false,
                hotkeys: taps,
            });
        }
        for resource in &fired {
            self.hotkeys.held_keys.push((key, resource.clone()));
            resource.triggered(serial(), now_ms());
        }
        !fired.is_empty()
    }

    /// A key went up: end what it held, and fire a tap that is complete.
    pub(crate) fn client_hotkey_key_released(&mut self, key: Keycode) {
        let (done, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hotkeys.held_keys)
            .into_iter()
            .partition(|(held, _)| *held == key);
        self.hotkeys.held_keys = rest;
        for (_, resource) in done {
            if resource.is_alive() {
                resource.released(serial(), now_ms());
            }
        }
        // Any key going up ends a tap, only its own key can complete it.
        let Some(tap) = self.hotkeys.tap.take() else {
            return;
        };
        let complete = tap.key == key
            && !tap.dirty
            && tap.since.elapsed() <= TAP_MAX_HOLD
            && !self.session_locked();
        if complete {
            for resource in tap.hotkeys.iter().filter(|r| r.is_alive()) {
                resource.triggered(serial(), now_ms());
                resource.released(serial(), now_ms());
            }
        }
    }

    /// A button went down with `mods` held. True when a hotkey takes the press.
    pub(crate) fn client_hotkey_button(&mut self, button: u32, mods: u32) -> bool {
        let fired: Vec<XxHotkeyV1> = self
            .hotkeys
            .entries
            .iter()
            .filter(|entry| entry.spec.matches_button(button, mods) && self.on_main_seat(entry))
            .map(|entry| entry.resource.clone())
            .collect();
        for resource in &fired {
            self.hotkeys.held_buttons.push((button, resource.clone()));
            resource.triggered(serial(), now_ms());
        }
        !fired.is_empty()
    }

    /// A button went up: end what it held.
    pub(crate) fn client_hotkey_button_released(&mut self, button: u32) {
        let (done, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.hotkeys.held_buttons)
            .into_iter()
            .partition(|(held, _)| *held == button);
        self.hotkeys.held_buttons = rest;
        for (_, resource) in done {
            if resource.is_alive() {
                resource.released(serial(), now_ms());
            }
        }
    }
}

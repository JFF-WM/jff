//! Tests for the trigger matching and the policy of `jff_wm::protocol::xx_hotkey`.

use smithay::input::keyboard::{Keysym, ModifiersState};

use jff_wm::protocol::xx_hotkey::{
    button_name, is_aux_button, modifier_names, mods_of, mods_state, permitted, HotkeyInfo, Spec,
    Trigger, ALT, CTRL, SHIFT, SUPER,
};

const BTN_LEFT: u32 = 0x110;
const BTN_SIDE: u32 = 0x113;
const BTN_TASK: u32 = 0x117;

fn key(sym: Keysym, mods: u32) -> Spec {
    Spec {
        trigger: Trigger::Key(sym.raw()),
        mods,
    }
}

fn button(code: u32, mods: u32) -> Spec {
    Spec {
        trigger: Trigger::Button(code),
        mods,
    }
}

#[test]
fn modifiers_must_match_exactly() {
    let spec = key(Keysym::b, SUPER | SHIFT);
    let syms = [Keysym::b.raw()];
    assert!(spec.matches_key(&syms, SUPER | SHIFT));
    assert!(!spec.matches_key(&syms, SUPER));
    assert!(!spec.matches_key(&syms, SUPER | SHIFT | CTRL));
    assert!(!spec.matches_key(&syms, 0));
}

#[test]
fn a_key_matches_any_layout_group() {
    let spec = key(Keysym::b, CTRL);
    // The same key types `b` in the first group and a Cyrillic letter in the second.
    assert!(spec.matches_key(&[Keysym::b.raw(), Keysym::Cyrillic_i.raw()], CTRL));
    assert!(spec.matches_key(&[Keysym::Cyrillic_i.raw(), Keysym::b.raw()], CTRL));
    assert!(!spec.matches_key(&[Keysym::Cyrillic_i.raw()], CTRL));
    // A keymap without the keysym: nothing fires.
    assert!(!spec.matches_key(&[], CTRL));
}

#[test]
fn a_button_trigger_does_not_match_keys_and_the_other_way_round() {
    assert!(button(BTN_SIDE, ALT).matches_button(BTN_SIDE, ALT));
    assert!(!button(BTN_SIDE, ALT).matches_button(BTN_SIDE, 0));
    assert!(!button(BTN_SIDE, ALT).matches_key(&[BTN_SIDE], ALT));
    assert!(!key(Keysym::b, ALT).matches_button(Keysym::b.raw(), ALT));
}

#[test]
fn lock_modifiers_are_ignored() {
    let state = ModifiersState {
        caps_lock: true,
        num_lock: true,
        logo: true,
        ..Default::default()
    };
    assert_eq!(mods_of(&state), SUPER);
    assert_eq!(mods_of(&mods_state(SHIFT | CTRL | ALT | SUPER)), 15);
    assert_eq!(mods_of(&mods_state(0)), 0);
}

#[test]
fn only_modifier_keysyms_are_taps() {
    assert!(key(Keysym::Super_L, 0).is_tap());
    assert!(key(Keysym::Shift_L, CTRL).is_tap());
    assert!(!key(Keysym::b, SUPER).is_tap());
    assert!(!button(BTN_SIDE, 0).is_tap());
}

#[test]
fn plain_character_keys_are_not_permitted() {
    for sym in [
        Keysym::a,
        Keysym::_1,
        Keysym::space,
        Keysym::Return,
        Keysym::Tab,
    ] {
        assert!(!permitted(&key(sym, 0)), "{sym:?} alone");
        assert!(!permitted(&key(sym, SHIFT)), "{sym:?} with shift");
    }
    assert!(!permitted(&key(Keysym::dead_acute, 0)));
    assert!(!permitted(&key(Keysym::dead_acute, SHIFT)));
}

#[test]
fn ctrl_alt_or_super_make_any_key_permitted() {
    for mods in [CTRL, ALT, SUPER, SUPER | SHIFT, CTRL | ALT] {
        assert!(permitted(&key(Keysym::a, mods)), "a with {mods}");
    }
}

#[test]
fn functional_keys_need_no_modifier() {
    for sym in [
        Keysym::F5,
        Keysym::Left,
        Keysym::Home,
        Keysym::Print,
        Keysym::XF86_AudioPlay,
        Keysym::XF86_AudioRaiseVolume,
        Keysym::KP_1,
        Keysym::KP_Enter,
        Keysym::Super_L,
        Keysym::Shift_L,
    ] {
        assert!(permitted(&key(sym, 0)), "{sym:?}");
    }
}

#[test]
fn buttons_need_a_modifier_unless_auxiliary() {
    assert!(!permitted(&button(BTN_LEFT, 0)));
    assert!(!permitted(&button(BTN_LEFT, SHIFT)));
    assert!(permitted(&button(BTN_LEFT, SUPER)));
    assert!(permitted(&button(BTN_SIDE, 0)));
    assert!(permitted(&button(BTN_TASK, 0)));
    assert!(is_aux_button(0x116) && !is_aux_button(0x112) && !is_aux_button(0x118));
}

#[test]
fn a_trigger_is_described_the_way_a_binding_writes_it() {
    assert_eq!(key(Keysym::b, SUPER | SHIFT).describe(), "Shift+Super+b");
    assert_eq!(key(Keysym::F5, 0).describe(), "F5");
    assert_eq!(key(Keysym::Super_L, 0).describe(), "Super_L");
    assert_eq!(
        key(Keysym::a, CTRL | ALT | SHIFT | SUPER).describe(),
        "Ctrl+Alt+Shift+Super+a"
    );
    assert_eq!(button(BTN_SIDE, 0).describe(), "BTN_SIDE");
    assert_eq!(button(BTN_LEFT, CTRL).describe(), "Ctrl+BTN_LEFT");
}

#[test]
fn modifier_and_button_names() {
    assert!(modifier_names(0).is_empty());
    assert_eq!(modifier_names(SUPER | CTRL), ["Ctrl", "Super"]);
    assert_eq!(button_name(BTN_TASK), "BTN_TASK");
    assert_eq!(button_name(0x2a0), "button 0x2a0");
}

#[test]
fn a_hotkey_is_listed_as_json() {
    let info = HotkeyInfo {
        app_id: Some("org.example.ptt".to_string()),
        description: "Push \"to\" talk".to_string(),
        spec: key(Keysym::F5, CTRL),
        seat: None,
        held: true,
    };
    assert_eq!(
        info.to_json(),
        format!(
            "{{\"app_id\":\"org.example.ptt\",\"description\":\"Push \\\"to\\\" talk\",\"trigger\":\"Ctrl+F5\",\"kind\":\"key\",\"code\":{},\"modifiers\":[\"ctrl\"],\"tap\":false,\"seat\":null,\"held\":true}}",
            Keysym::F5.raw()
        )
    );
}

#[test]
fn a_button_tap_and_an_unnamed_client_are_listed() {
    let button_info = HotkeyInfo {
        app_id: None,
        description: String::new(),
        spec: button(BTN_SIDE, 0),
        seat: Some("seat0".to_string()),
        held: false,
    };
    assert_eq!(
        button_info.to_json(),
        "{\"app_id\":null,\"description\":\"\",\"trigger\":\"BTN_SIDE\",\"kind\":\"button\",\"code\":275,\"modifiers\":[],\"tap\":false,\"seat\":\"seat0\",\"held\":false}"
    );
    let tap = HotkeyInfo {
        app_id: None,
        description: String::new(),
        spec: key(Keysym::Super_L, 0),
        seat: None,
        held: false,
    };
    assert!(tap.to_json().contains("\"tap\":true"));
}

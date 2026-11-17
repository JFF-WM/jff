//! The config nodes that decide how client-bound global hotkeys and `forward-key` fit together.

use jff_wm::config::{Action, Config};
use jff_wm::protocol::xx_hotkey::{mods_state, CTRL, SHIFT};
use smithay::input::keyboard::Keysym;

#[test]
fn global_hotkeys_are_off_unless_asked_for() {
    assert!(!Config::parse("").unwrap().global_hotkeys());
    assert!(Config::parse("global-hotkeys #true")
        .unwrap()
        .global_hotkeys());
    assert!(!Config::parse("global-hotkeys #false")
        .unwrap()
        .global_hotkeys());
}

#[test]
fn forward_key_binds_a_key_to_an_app_id() {
    let config = Config::parse(
        r#"
binds {
    Ctrl+Shift+g { forward-key "com.obsproject.Studio"; }
}
"#,
    )
    .unwrap();
    let action = config.action_for(&mods_state(CTRL | SHIFT), Keysym::g);
    assert!(
        matches!(action, Some(Action::ForwardKey(app_id)) if app_id == "com.obsproject.Studio"),
        "{action:?}"
    );
}

#[test]
fn forward_key_needs_exactly_one_app_id() {
    for line in ["forward-key", "forward-key \"\"", "forward-key \"a\" \"b\""] {
        let text = format!("binds {{ Ctrl+Shift+g {{ {line}; }} }}");
        assert!(Config::parse(&text).is_err(), "{line}");
    }
}

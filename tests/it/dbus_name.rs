//! Tests for `jff_wm::primitives::dbus_name`: the DBus names clients may annotate a window with.

use jff_wm::primitives::dbus_name::{
    valid_interface, valid_object_path, valid_service_name, MAX_NAME_LEN,
};

#[test]
fn interface_must_be_short_and_printable() {
    assert!(valid_interface("com.canonical.dbusmenu"));
    assert!(!valid_interface(""));
    assert!(!valid_interface("a\nb"));
    assert!(valid_interface(&"a".repeat(MAX_NAME_LEN)));
    assert!(!valid_interface(&"a".repeat(MAX_NAME_LEN + 1)));
}

#[test]
fn service_names() {
    assert!(valid_service_name("org.mozilla.firefox"));
    assert!(valid_service_name(":1.42"));
    assert!(valid_service_name("org.example.App-1_x"));
    assert!(!valid_service_name(""));
    assert!(!valid_service_name("single"));
    assert!(!valid_service_name("org..example"));
    assert!(!valid_service_name("org.1example"));
    assert!(!valid_service_name("org.exa mple"));
    assert!(!valid_service_name(".org.example"));
}

#[test]
fn object_paths() {
    assert!(valid_object_path("/"));
    assert!(valid_object_path("/MenuBar"));
    assert!(valid_object_path("/org/mozilla/menu_1"));
    assert!(!valid_object_path(""));
    assert!(!valid_object_path("MenuBar"));
    assert!(!valid_object_path("/MenuBar/"));
    assert!(!valid_object_path("//MenuBar"));
    assert!(!valid_object_path("/menu-bar"));
}

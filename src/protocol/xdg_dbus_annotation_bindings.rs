//! Bindings for xdg-dbus-annotation-v1, generated from `protocols/xdg-dbus-annotation-v1.xml`
//! (wayland-protocols merge request 52, as vendored by Firefox). No published wayland-protocols
//! has this protocol. Once one does, this file and the XML can go and `xdg_dbus_annotation.rs`
//! can import from `wayland_protocols`.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-dbus-annotation-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/xdg-dbus-annotation-v1.xml");
}

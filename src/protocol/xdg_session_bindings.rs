//! Bindings for xdg-session-management-v1, generated from `protocols/xdg-session-management-v1.xml`
//! (copied from wayland-protocols, `staging/xdg-session-management/`). No published
//! wayland-protocols has this protocol yet. Once one does, this file and the XML can go and
//! `session_management.rs` can import from `wayland_protocols::xdg::session_management::v1`.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        pub use smithay::reexports::wayland_protocols::xdg::shell::server::__interfaces::*;
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-session-management-v1.xml");
    }
    use self::__interfaces::*;
    // The XML names xdg_toplevel, which lives in xdg-shell.
    pub(crate) use smithay::reexports::wayland_protocols::xdg::shell::server::*;

    wayland_scanner::generate_server_code!("protocols/xdg-session-management-v1.xml");
}

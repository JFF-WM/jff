//! Bindings for xx-session-management-v1, generated from `protocols/xx-session-management-v1.xml`
//! (wayland-protocols, `experimental/xx-session-management/`). The older, experimental twin of
//! xdg-session-management-v1, still asked for by some builds, so jff offers both.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        pub use smithay::reexports::wayland_protocols::xdg::shell::server::__interfaces::*;
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xx-session-management-v1.xml");
    }
    use self::__interfaces::*;
    // The XML names xdg_toplevel, which lives in xdg-shell.
    pub(crate) use smithay::reexports::wayland_protocols::xdg::shell::server::*;

    wayland_scanner::generate_server_code!("protocols/xx-session-management-v1.xml");
}

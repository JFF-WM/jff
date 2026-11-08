//! Client bindings for xdg-session-management-v1, generated from
//! `protocols/xdg-session-management-v1.xml`.

#![allow(dead_code, non_camel_case_types, unused_imports, clippy::all)]

pub mod client {
    use wayland_client::backend as wayland_backend;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::backend as wayland_backend;
        pub use wayland_client::protocol::__interfaces::*;
        pub use wayland_protocols::xdg::shell::client::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xdg-session-management-v1.xml");
    }
    use self::__interfaces::*;
    // The XML refers to xdg_toplevel, which lives in xdg-shell.
    use wayland_protocols::xdg::shell::client::*;

    wayland_scanner::generate_client_code!("protocols/xdg-session-management-v1.xml");
}

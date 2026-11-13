//! Bindings for wp-color-management-v1 at version 3, generated from
//! `protocols/color-management-v1.xml` (wayland-protocols, `staging/color-management/`).
//!
//! The wayland-protocols crate Smithay re-exports is at version 1 of this protocol: no `ready2`,
//! no `preferred_changed2`, no reference objects. `protocol/color_management.rs` uses these.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/color-management-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/color-management-v1.xml");
}

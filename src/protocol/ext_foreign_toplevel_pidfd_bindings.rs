//! Bindings for ext-foreign-toplevel-pidfd-v1, generated from
//! `protocols/ext-foreign-toplevel-pidfd-v1.xml` (wayland-protocols MR 501, `staging/`).
//!
//! The protocol names two objects of ext-foreign-toplevel-list-v1 (Smithay's): the list it
//! extends and the toplevel handles that list creates.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        pub use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::__interfaces::*;
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/ext-foreign-toplevel-pidfd-v1.xml");
    }
    use self::__interfaces::*;
    // The XML names objects of ext-foreign-toplevel-list-v1.
    pub(crate) use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::*;

    wayland_scanner::generate_server_code!("protocols/ext-foreign-toplevel-pidfd-v1.xml");
}

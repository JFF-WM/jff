//! Bindings for ext-transient-output-v1, generated from `protocols/ext-transient-output-v1.xml`
//! (not in a published wayland-protocols yet).

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/ext-transient-output-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/ext-transient-output-v1.xml");
}

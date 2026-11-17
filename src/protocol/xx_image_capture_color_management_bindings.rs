//! Bindings for xx-image-capture-color-management-v1, generated from
//! `protocols/xx-image-capture-color-management-v1.xml` (wayland-protocols,
//! `experimental/xx-image-capture-color-management/`).
//!
//! The protocol names objects of three other protocols: the capture source and the frame of
//! ext-image-capture-source-v1 / ext-image-copy-capture-v1 (Smithay's), and the image
//! description objects of wp-color-management-v1, which are the version 3 ones of
//! `protocol/color_management_bindings.rs`, not the crate's (it has no reference objects).

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        pub use crate::protocol::color_management_bindings::server::__interfaces::*;
        pub use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::__interfaces::*;
        pub use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::__interfaces::*;
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/xx-image-capture-color-management-v1.xml");
    }
    use self::__interfaces::*;
    // The XML names objects of the protocols above.
    pub(crate) use crate::protocol::color_management_bindings::server::*;
    pub(crate) use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::*;
    pub(crate) use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::*;

    wayland_scanner::generate_server_code!("protocols/xx-image-capture-color-management-v1.xml");
}

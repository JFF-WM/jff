//! Bindings for KDE's appmenu protocol (`org_kde_kwin_appmenu_manager`), generated from
//! `protocols/kde-appmenu.xml`. It is not part of wayland-protocols.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/kde-appmenu.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/kde-appmenu.xml");
}

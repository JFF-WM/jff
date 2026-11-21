//! Bindings for GTK's private Wayland extension (`gtk_shell1`, `gtk_surface1`), generated from
//! `protocols/gtk-shell.xml`. It is not part of wayland-protocols: copy the file from GTK
//! (`gdk/wayland/protocol/gtk-shell.xml`) and keep its header.

#![allow(clippy::all, non_camel_case_types, dead_code, unused_imports)]

pub mod server {
    use smithay::reexports::wayland_server;
    use wayland_server::backend as wayland_backend;
    pub(crate) use wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        pub use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/gtk-shell.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/gtk-shell.xml");
}

//! One module per Wayland protocol family (handlers and their wiring).

pub mod content_type;
pub mod ext_foreign_toplevel;
pub mod foreign_toplevel;
pub mod idle;
pub mod image_copy;
pub mod layer_shell;
pub mod output_management;
pub mod presentation;
pub mod session_lock;
pub mod shortcuts_inhibit;
pub mod tearing_control;
pub mod text_input;
pub mod workspace;

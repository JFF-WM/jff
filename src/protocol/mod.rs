//! One module per Wayland protocol family (handlers and their wiring).

pub mod background_effect;
pub mod content_type;
pub mod drm_lease;
pub mod ext_foreign_toplevel;
pub mod foreign_toplevel;
pub mod idle;
pub mod image_copy;
pub mod layer_shell;
pub mod output_management;
pub mod output_power;
pub mod pointer_warp;
pub mod presentation;
pub mod session_lock;
pub mod session_management;
pub mod shortcuts_inhibit;
pub mod tearing_control;
pub mod text_input;
pub mod toplevel_icon;
pub mod toplevel_tag;
pub mod workspace;
pub mod xdg_session_bindings;

//! Checks for the DBus names a client hands the compositor (`protocol::xdg_dbus_annotation`).
//!
//! The strings come from clients and end up on the control socket and in other apps' hands, so
//! a malformed one is dropped before it is stored.

/// Longest DBus interface or service name, in bytes (the DBus specification's limit).
pub const MAX_NAME_LEN: usize = 255;

/// Is `name` usable as the interface name an annotation is filed under?
///
/// Only the length and the absence of control characters are checked: the interface is an
/// opaque key chosen by the app, which other apps compare as a whole.
pub fn valid_interface(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME_LEN && !name.chars().any(char::is_control)
}

/// Is `name` a plausible DBus bus name, either unique (`:1.42`) or well-known
/// (`org.example.App`)?
///
/// Checks the length, the character set and that a well-known name has at least two
/// non-empty elements. It does not check that the name exists on the bus.
pub fn valid_service_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    let (unique, body) = match name.strip_prefix(':') {
        Some(rest) => (true, rest),
        None => (false, name),
    };
    let mut elements = 0usize;
    for element in body.split('.') {
        // Elements of a unique name may start with a digit, those of a well-known name may not.
        let starts_ok = element
            .bytes()
            .next()
            .is_some_and(|b| unique || !b.is_ascii_digit());
        let chars_ok = element
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !starts_ok || !chars_ok {
            return false;
        }
        elements += 1;
    }
    elements >= 2
}

/// Is `path` a valid DBus object path (`/` or `/org/example/Object`)?
///
/// Elements are non-empty runs of `[A-Za-z0-9_]` and there is no trailing slash.
pub fn valid_object_path(path: &str) -> bool {
    if path == "/" {
        return true;
    }
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    path.len() <= MAX_NAME_LEN * 16
        && rest
            .split('/')
            .all(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

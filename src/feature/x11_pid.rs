//! Which process owns the X11 window that has the keyboard.
//!
//! To jff every X11 window belongs to xwayland-satellite (its one Wayland client), so the
//! Wayland side cannot tell which app is behind one. The X server can: the `X-Resource`
//! extension (`ResQueryClientIds`, `LOCAL_CLIENT_PID`) gives the pid the server read from the
//! socket credentials of the app's X connection, which the app cannot fake (unlike
//! `_NET_WM_PID`). The satellite hands the keyboard focus of the Wayland side to the X side with
//! `SetInputFocus`, so `GetInputFocus` names the window the keyboard is on.
//!
//! This is a very small X11 client, just the few requests it needs, over the X socket in
//! `/tmp/.X11-unix`, with a short timeout on every read and write: the compositor waits for it.
//! No authorization is sent. The satellite starts Xwayland for the user alone, and a server that
//! wants a cookie makes this fail with an error, never with a wrong answer.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// A hung X server must not hang the compositor for long.
const IO_TIMEOUT: Duration = Duration::from_millis(250);
/// `ResClientIdSpec` mask: ask for the pid the server read from the socket of the client.
const LOCAL_CLIENT_PID: u32 = 2;
/// Predefined atoms of the core protocol.
const ATOM_WM_NAME: u32 = 39;

/// The X11 window with the keyboard and the process behind it.
#[derive(Debug, Clone)]
pub struct FocusedX11 {
    /// The top-level window (the focus can be on a child window of it).
    pub window: u32,
    /// The pid of the app that owns it, as the X server knows it.
    pub pid: u32,
    /// Its title, with control characters turned into spaces, like jff's own.
    pub title: String,
}

/// Ask the X server of `display` (`:1`, `:1.0`) for the focused window and its process.
pub fn focused_window(display: &str) -> Result<FocusedX11, String> {
    let mut x = Conn::open(display)?;

    let focus = x.get_input_focus()?;
    // 0 is `None`, 1 is `PointerRoot`: no window has the keyboard.
    if focus <= 1 {
        return Err("no X11 window has the keyboard on the X server's side".to_string());
    }
    let window = x.toplevel_of(focus)?;

    let xres = x
        .query_extension("X-Resource")?
        .ok_or("the X server has no X-Resource extension")?;
    let pid = x
        .client_pid(xres, window)?
        .ok_or("the X server could not say which process owns the window")?;
    let title = x.title_of(window)?;

    Ok(FocusedX11 { window, pid, title })
}

struct Conn {
    stream: UnixStream,
    root: u32,
}

fn io_err(what: &str, err: std::io::Error) -> String {
    format!("X11 {what}: {err}")
}

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

impl Conn {
    fn open(display: &str) -> Result<Self, String> {
        // ":1" and ":1.0" are local displays, anything with a host in front is not ours.
        let number = display
            .strip_prefix(':')
            .and_then(|rest| rest.split('.').next())
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| format!("DISPLAY={display:?} is not a local X display"))?;
        let mut stream = UnixStream::connect(format!("/tmp/.X11-unix/X{number}"))
            .map_err(|e| io_err("connect", e))?;
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
            .map_err(|e| io_err("socket", e))?;

        // Little endian, protocol 11.0, no authorization name or data.
        stream
            .write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .map_err(|e| io_err("setup", e))?;
        let mut head = [0u8; 8];
        stream
            .read_exact(&mut head)
            .map_err(|e| io_err("setup reply", e))?;
        let extra = usize::from(u16::from_le_bytes([head[6], head[7]])) * 4;
        let mut data = vec![0u8; extra];
        stream
            .read_exact(&mut data)
            .map_err(|e| io_err("setup reply", e))?;
        if head[0] != 1 {
            return Err(
                "the X server did not accept the connection (it wants authorization?)".to_string(),
            );
        }

        // After the fixed part: the vendor string, the pixmap formats, then the first screen,
        // which starts with its root window.
        let vendor_len = usize::from(u16::from_le_bytes([
            *data.get(16).ok_or("short X11 setup reply")?,
            *data.get(17).ok_or("short X11 setup reply")?,
        ]));
        let formats = usize::from(*data.get(21).ok_or("short X11 setup reply")?);
        let screen = 32 + pad4(vendor_len) + 8 * formats;
        let root = u32_at(&data, screen).ok_or("short X11 setup reply")?;
        Ok(Self { stream, root })
    }

    /// Read one reply: the 32 byte header and what follows it. Errors become `Err`, events
    /// (nobody asked for any) are skipped.
    fn reply(&mut self) -> Result<([u8; 32], Vec<u8>), String> {
        loop {
            let mut head = [0u8; 32];
            self.stream
                .read_exact(&mut head)
                .map_err(|e| io_err("reply", e))?;
            match head[0] {
                1 => {
                    let extra = u32_at(&head, 4).unwrap_or(0) as usize * 4;
                    if extra > 1 << 20 {
                        return Err("X11 reply is too long".to_string());
                    }
                    let mut data = vec![0u8; extra];
                    self.stream
                        .read_exact(&mut data)
                        .map_err(|e| io_err("reply", e))?;
                    return Ok((head, data));
                }
                0 => return Err(format!("X11 error {} on request {}", head[1], head[10])),
                _ => continue,
            }
        }
    }

    fn call(&mut self, request: &[u8]) -> Result<([u8; 32], Vec<u8>), String> {
        self.stream
            .write_all(request)
            .map_err(|e| io_err("request", e))?;
        self.reply()
    }

    /// `GetInputFocus`: the window with the keyboard (0 `None`, 1 `PointerRoot`).
    fn get_input_focus(&mut self) -> Result<u32, String> {
        let (head, _) = self.call(&[43, 0, 1, 0])?;
        u32_at(&head, 8).ok_or_else(|| "short X11 reply".to_string())
    }

    /// `QueryTree`: the parent of `window`, 0 for the root.
    fn parent_of(&mut self, window: u32) -> Result<u32, String> {
        let mut request = vec![15, 0, 2, 0];
        request.extend_from_slice(&window.to_le_bytes());
        let (head, _) = self.call(&request)?;
        u32_at(&head, 12).ok_or_else(|| "short X11 reply".to_string())
    }

    /// The top-level ancestor of `window`: the one just under the root.
    fn toplevel_of(&mut self, mut window: u32) -> Result<u32, String> {
        for _ in 0..32 {
            let parent = self.parent_of(window)?;
            if parent == 0 || parent == self.root {
                return Ok(window);
            }
            window = parent;
        }
        Err("the X11 window tree is too deep".to_string())
    }

    /// `QueryExtension`: the major opcode of a extension, `None` when the server has none.
    fn query_extension(&mut self, name: &str) -> Result<Option<u8>, String> {
        let mut request = vec![98, 0];
        let words = 2 + pad4(name.len()) / 4;
        request.extend_from_slice(&(words as u16).to_le_bytes());
        request.extend_from_slice(&(name.len() as u16).to_le_bytes());
        request.extend_from_slice(&[0, 0]);
        request.extend_from_slice(name.as_bytes());
        request.resize(8 + pad4(name.len()), 0);
        let (head, _) = self.call(&request)?;
        Ok((head[8] != 0).then_some(head[9]))
    }

    /// `InternAtom` without creating it: 0 when the server does not know the name.
    fn atom(&mut self, name: &str) -> Result<u32, String> {
        let mut request = vec![16, 1];
        let words = 2 + pad4(name.len()) / 4;
        request.extend_from_slice(&(words as u16).to_le_bytes());
        request.extend_from_slice(&(name.len() as u16).to_le_bytes());
        request.extend_from_slice(&[0, 0]);
        request.extend_from_slice(name.as_bytes());
        request.resize(8 + pad4(name.len()), 0);
        let (head, _) = self.call(&request)?;
        u32_at(&head, 8).ok_or_else(|| "short X11 reply".to_string())
    }

    /// `GetProperty` of any type, the first 1024 bytes. Empty when the property is not set.
    fn property(&mut self, window: u32, property: u32) -> Result<Vec<u8>, String> {
        let mut request = vec![20, 0, 6, 0];
        for value in [window, property, 0, 0, 256] {
            request.extend_from_slice(&value.to_le_bytes());
        }
        let (head, data) = self.call(&request)?;
        let format = usize::from(head[1]);
        let items = u32_at(&head, 16).unwrap_or(0) as usize;
        let bytes = items.saturating_mul(format) / 8;
        Ok(data.get(..bytes).unwrap_or(&data).to_vec())
    }

    /// The title the satellite shows: `_NET_WM_NAME`, else `WM_NAME`.
    fn title_of(&mut self, window: u32) -> Result<String, String> {
        let net = self.atom("_NET_WM_NAME")?;
        let mut raw = if net == 0 {
            Vec::new()
        } else {
            self.property(window, net)?
        };
        if raw.is_empty() {
            raw = self.property(window, ATOM_WM_NAME)?;
        }
        Ok(String::from_utf8_lossy(&raw)
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_owned())
    }

    /// X-Resource `ResQueryClientIds`: the pid of the client that owns `window`.
    fn client_pid(&mut self, xres: u8, window: u32) -> Result<Option<u32>, String> {
        // One spec: the window (any resource of the client will do) and the pid mask.
        let mut request = vec![xres, 4, 4, 0];
        for value in [1u32, window, LOCAL_CLIENT_PID] {
            request.extend_from_slice(&value.to_le_bytes());
        }
        let (head, data) = self.call(&request)?;
        let count = u32_at(&head, 8).unwrap_or(0);
        let mut at = 0;
        for _ in 0..count {
            let (Some(mask), Some(len)) = (u32_at(&data, at + 4), u32_at(&data, at + 8)) else {
                break;
            };
            let len = len as usize;
            let Some(value) = data.get(at + 12..at + 12 + len) else {
                break;
            };
            if mask == LOCAL_CLIENT_PID && len >= 4 {
                return Ok(u32_at(value, 0).filter(|pid| *pid > 0));
            }
            at += 12 + len;
        }
        Ok(None)
    }
}

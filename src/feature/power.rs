//! The PC's power button.
//!
//! logind powers the machine off when the button is pressed, unless a program holds a
//! `handle-power-key` inhibitor lock. jff takes that lock and runs what `power-key` says instead
//! (suspend by default). The lock is held by a small child process, `systemd-inhibit ... sleep
//! infinity`, which the kernel kills when jff goes away, so the system is never left without
//! someone to handle the button.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::config::PowerKey;

/// XF86PowerOff, what xkb makes of KEY_POWER.
pub const XF86_POWER_OFF: u32 = 0x1008_FF2A;

/// The process holding the lock, if jff has the button.
static LOCK: Mutex<Option<Child>> = Mutex::new(None);

/// Take or give back the lock to match `mode`. Call at startup and after a config reload.
/// `session` is false for the nested backend, where the host owns the button.
pub fn sync(mode: PowerKey, session: bool) {
    let want = session && mode != PowerKey::System;
    let Ok(mut lock) = LOCK.lock() else { return };
    // A holder that died is gone.
    if let Some(child) = lock.as_mut() {
        if !matches!(child.try_wait(), Ok(None)) {
            *lock = None;
        }
    }
    match (want, lock.is_some()) {
        (true, false) => {
            let mut command = Command::new("systemd-inhibit");
            command
                .args([
                    "--what=handle-power-key",
                    "--who=jff",
                    "--why=jff handles the power button",
                    "--mode=block",
                    "sleep",
                    "infinity",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: only prctl, which is async-signal-safe, runs between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            match command.spawn() {
                Ok(child) => {
                    tracing::info!("handling the power button ({mode:?})");
                    *lock = Some(child);
                }
                Err(err) => tracing::warn!(
                    "could not take the power button from logind ({err}); it keeps its default"
                ),
            }
        }
        (false, true) => {
            if let Some(mut child) = lock.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        _ => {}
    }
}

/// `systemctl <verb>`, detached.
pub fn systemctl(verb: &str) {
    crate::input::spawn_command("systemctl", &[verb.to_string()]);
}

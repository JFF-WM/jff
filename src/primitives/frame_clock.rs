//! When the next frame reaches the screen, counted on from the kernel's last flip time.
//!
//! All times are CLOCK_MONOTONIC, as a `Duration` since its start.

use std::time::Duration;

/// Shortest wait [`wait_until`] hands out, so a timer is never set for "now".
pub const MIN_WAIT: Duration = Duration::from_micros(500);

/// The refresh interval to count with when the monitor reports none (60 Hz).
pub const FALLBACK_REFRESH: Duration = Duration::from_nanos(16_666_667);

/// The first vblank after `now` on the grid `last + n × refresh`, the time a frame drawn now
/// reaches the screen. With no vblank seen yet there is no grid: one refresh from `now`.
pub fn next_presentation_time(
    last: Option<Duration>,
    refresh: Duration,
    now: Duration,
) -> Duration {
    let Some(last) = last else {
        return now + refresh;
    };
    let refresh_ns = refresh.as_nanos().max(1);
    let periods = now.saturating_sub(last).as_nanos() / refresh_ns + 1;
    last + Duration::from_nanos((periods * refresh_ns) as u64)
}

/// How long to wait from `now` for `target`, never less than [`MIN_WAIT`].
pub fn wait_until(target: Duration, now: Duration) -> Duration {
    target.saturating_sub(now).max(MIN_WAIT)
}

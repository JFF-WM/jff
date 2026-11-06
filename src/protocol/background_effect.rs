//! `ext_background_effect_v1`: a client declares which part of its surface should have a
//! blurred background drawn behind it.
//!
//! The blur region is double-buffered like the rest of a surface's state: `set_blur_region`
//! and `unset_blur_region` apply on the next `wl_surface.commit`. Smithay keeps that state,
//! this module turns the current `wl_region` into renderer-friendly rectangles, but only when
//! it changed:
//!
//! ```text
//!  set/unset request  -->  pending
//!  commit             -->  (post-commit hook) pending becomes stale, blur damaged
//!  next frame         -->  get_cached_blur_region() sees stale and recomputes the rectangles
//! ```
//!
//! Recomputing at render time costs one conversion however often a client changes its region
//! between frames, and none for a surface that is never drawn.

use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::background_effect::{
    self, BackgroundEffectSurfaceCachedState, ExtBackgroundEffectHandler,
};
use smithay::wayland::compositor::{
    add_post_commit_hook, with_states, RegionAttributes, SurfaceData,
};

use crate::primitives::region::region_to_non_overlapping_rects;
use crate::render::effects::background_effect::damage_surface;
use crate::state::JffState;

/// Blur region of a surface as rectangles, kept in its user data.
#[derive(Default)]
struct BlurRegionCache(Mutex<BlurRegionState>);

#[derive(Default)]
struct BlurRegionState {
    /// A request is waiting for the next commit to apply it.
    pending: bool,
    /// A commit applied a change, so `rects` is out of date.
    stale: bool,
    /// Commit hook is installed (once per surface).
    hook_installed: bool,
    /// Non-overlapping rectangles in surface-local coordinates. `None`: no blur region.
    /// `Some(empty)`: the client set an empty region, meaning "no blur". The `Arc` lets the
    /// renderer keep the rectangles for a frame while a commit replaces them.
    rects: Option<Arc<Vec<Rectangle<i32, Logical>>>>,
}

/// Blur region of the surface owning `states`: `None` if the client has none, otherwise its
/// rectangles (possibly empty, meaning "no blur"). Recomputed first if it changed.
pub fn get_cached_blur_region(states: &SurfaceData) -> Option<Arc<Vec<Rectangle<i32, Logical>>>> {
    let cache = states
        .data_map
        .get_or_insert_threadsafe(BlurRegionCache::default);
    let mut state = cache.0.lock().unwrap();

    if state.stale {
        state.stale = false;
        state.rects = current_blur_rects(states);
    }
    state.rects.clone()
}

/// Flatten the region the last commit made current.
fn current_blur_rects(states: &SurfaceData) -> Option<Arc<Vec<Rectangle<i32, Logical>>>> {
    // A surface that never used the protocol has no such state, don't create it.
    if !states
        .cached_state
        .has::<BackgroundEffectSurfaceCachedState>()
    {
        return None;
    }
    let mut cached = states
        .cached_state
        .get::<BackgroundEffectSurfaceCachedState>();
    let region = cached.current().blur_region.as_ref()?;

    let mut rects = Vec::new();
    region_to_non_overlapping_rects(region, &mut rects);
    Some(Arc::new(rects))
}

/// The client asked for a change, its next commit applies it.
fn mark_pending(surface: &WlSurface) {
    let install_hook = with_states(surface, |states| {
        let cache = states
            .data_map
            .get_or_insert_threadsafe(BlurRegionCache::default);
        let mut state = cache.0.lock().unwrap();
        state.pending = true;
        !std::mem::replace(&mut state.hook_installed, true)
    });
    if !install_hook {
        return;
    }

    // Once per surface. Smithay has already applied the commit by the time this runs, which
    // is why the request handlers can't do it: they only see the pending state.
    add_post_commit_hook::<JffState, _>(surface, |_state, _display, surface| {
        with_states(surface, |states| {
            let Some(cache) = states.data_map.get::<BlurRegionCache>() else {
                return;
            };
            let changed = {
                let mut state = cache.0.lock().unwrap();
                let changed = std::mem::take(&mut state.pending);
                state.stale |= changed;
                changed
            };
            if changed {
                damage_surface(states);
            }
        });
    });
}

impl ExtBackgroundEffectHandler for JffState {
    fn capabilities(&self) -> background_effect::Capability {
        background_effect::Capability::Blur
    }

    fn set_blur_region(&mut self, surface: WlSurface, _region: RegionAttributes) {
        // The region is read from surface state after the commit.
        mark_pending(&surface);
    }

    fn unset_blur_region(&mut self, surface: WlSurface) {
        mark_pending(&surface);
    }
}

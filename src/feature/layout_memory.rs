//! `remember-container-layouts`: windows tiled together again get the arrangement they had.
//!
//! The pure part (what is remembered, and which window goes in which slot) is in
//! `primitives::layout_memory`. This is the Smithay side: it reads a container's windows and
//! app ids, takes a snapshot whenever the user changes a container or takes it apart, and lays
//! a new container out from a snapshot when its windows were arranged before.
//!
//! The arrangements live in memory, for the length of the session.

use crate::desk::{ContainerId, WindowId};
use crate::feature::osd::toplevel_names;
use crate::primitives::layout_template::Template;
use crate::state::JffState;

impl JffState {
    /// Every window of container `id` with its app id, in tree order. `None` when the
    /// container is gone or one of its windows is not a toplevel.
    fn container_app_ids(&self, id: ContainerId) -> Option<Vec<(WindowId, String)>> {
        let container = self.desk.container(id)?;
        container
            .tree()
            .leaves()
            .into_iter()
            .map(|leaf| {
                let entry = self.desk.find_by_id(leaf)?;
                let surface = entry.window.toplevel()?.wl_surface();
                Some((leaf, toplevel_names(surface).0))
            })
            .collect()
    }

    /// Remember how container `id` is arranged now, for windows with these app ids. Called
    /// after the user changed the container and before it is taken apart or loses a window.
    /// Does nothing with `remember-container-layouts` off, for a container of one window, and
    /// when a window has no app id.
    pub(crate) fn remember_container_layout(&mut self, id: ContainerId) {
        if !self.config.remember_container_layouts() {
            return;
        }
        let Some(leaves) = self.container_app_ids(id) else {
            return;
        };
        let Some(template) = self
            .desk
            .container(id)
            .and_then(|container| Template::from_tree(container.tree(), |_| None))
        else {
            return;
        };
        if self.layout_memory.remember(template, leaves) {
            tracing::debug!("container {}: arrangement remembered", id.get());
        }
    }

    /// Lay the new container `id` out as windows with the same app ids were arranged before,
    /// when that was remembered. Returns true when it did. The caller tiles the container
    /// after. Does nothing with `remember-container-layouts` off.
    pub(crate) fn restore_container_layout(&mut self, id: ContainerId) -> bool {
        if !self.config.remember_container_layouts() {
            return false;
        }
        let Some(windows) = self.container_app_ids(id) else {
            return false;
        };
        let Some(recalled) = self.layout_memory.recall(&windows) else {
            return false;
        };
        let template = recalled.template.clone();
        let order = recalled.order;
        let gap = self.config.snap_gap();
        match self
            .desk
            .apply_container_layout_ordered(id, &template, |_| None, &order, gap)
        {
            Ok(applied) => {
                tracing::info!(
                    "container {} arranged as it was before: {} slots, {} empty, {} extra windows",
                    id.get(),
                    applied.slots,
                    applied.empty,
                    applied.extra
                );
                true
            }
            Err(err) => {
                tracing::warn!(
                    "container {}: remembered arrangement not used: {err:?}",
                    id.get()
                );
                false
            }
        }
    }
}
